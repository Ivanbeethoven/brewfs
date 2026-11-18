use super::*;

type Member = Arc<ParkingMutex<SliceState>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Open,
    Ready,
    Uploading,
}

pub(super) fn seal(groups: Vec<Vec<Member>>) {
    for members in groups {
        for member in members {
            let mut state = member.lock();
            state.native_group = Some(Stage::Ready);
            state.notify.notify_waiters();
        }
    }
}

enum Publication {
    Published,
    Stale,
    Reordered,
}

impl<B, M> FileWriter<B, M>
where
    B: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    pub(super) async fn commit_native_front(
        shared: &Arc<Shared<B, M>>,
        chunk_id: u64,
        front: &Member,
    ) {
        let (notify, started) = {
            let s = front.lock();
            (s.notify.clone(), s.started)
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let mut limit = started + COMMIT_UPLOAD_MAX_WAIT;
        {
            let mut inner = shared.inner.lock().await;
            let groups = inner.native_groups.as_mut().expect("native writer");
            let now = Instant::now();
            seal(groups.seal_due(now));
            let state = front.lock().native_group;
            let Some(Stage::Ready) = state else {
                if now >= limit {
                    seal(groups.seal_all());
                    return;
                }
                let until = groups.next_deadline().unwrap_or(limit).min(limit);
                drop(inner);
                let _ = tokio::time::timeout_at(until.into(), notified).await;
                return;
            };
        }

        let epoch = front.lock().frozen_epoch;
        if epoch != 0 && shared.inode.data_epoch() != epoch {
            let handle = SliceHandle {
                shared,
                slice: front,
            };
            handle.with_mut(|s| {
                s.native_group = None;
                s.upload_task_active = false;
            });
            handle.mark_committed();
            return;
        }

        // Keep ready members in the chunk queue while uploads occupy the
        // existing file permits. Select the object only when a permit is free.
        let observation = shared.request_observation();
        let permit_wait = observation.as_ref().and_then(|observer| {
            observer.unscoped_segment_timer(RequestSegment::NativeUploadPermitWait)
        });
        let permit =
            tokio::time::timeout_at(limit.into(), shared.upload_limit.clone().acquire_owned())
                .await
                .context("native upload exceeded the existing slice deadline")
                .and_then(|permit| permit.context("native upload semaphore closed"));
        drop(permit_wait);
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => {
                let handle = SliceHandle {
                    shared,
                    slice: front,
                };
                handle.with_mut(|s| s.native_group = None);
                handle.mark_failed(error);
                return;
            }
        };
        let members = {
            let mut inner = shared.inner.lock().await;
            seal(
                inner
                    .native_groups
                    .as_mut()
                    .expect("native writer")
                    .seal_due(Instant::now()),
            );
            let Some(chunk) = inner.chunks.get(&chunk_id) else {
                return;
            };
            if !chunk.slices.front().is_some_and(|s| Arc::ptr_eq(s, front)) {
                return;
            }
            let epoch = front.lock().frozen_epoch;
            let capacity =
                (shared.config.layout.block_size as usize / 4096).min(native_groups::MAX_MEMBERS);
            let mut picked = Vec::new();
            let mut offsets = Vec::new();
            for slice in chunk.slices.iter().take(capacity) {
                let s = slice.lock();
                if s.native_group != Some(Stage::Ready)
                    || s.frozen_epoch != epoch
                    || offsets.contains(&s.offset)
                {
                    break;
                }
                limit = limit.min(s.started + COMMIT_UPLOAD_MAX_WAIT);
                offsets.push(s.offset);
                picked.push(slice.clone());
            }
            for member in &picked {
                member.lock().native_group = Some(Stage::Uploading);
            }
            picked
        };
        if members.is_empty() {
            return;
        }
        let mut object = None;
        let result =
            Self::upload_native_group(shared, chunk_id, &members, limit, &mut object, permit).await;
        if !matches!(result, Ok(Publication::Published))
            && let Some(slice_id) = object
        {
            let orphan = SliceDesc {
                chunk_id,
                slice_id,
                offset: 0,
                length: members.len() as u64 * 4096,
            };
            record_uploaded_orphan(shared, orphan).await;
        }
        match result {
            Ok(Publication::Reordered) => Self::resume_individual_uploads(shared, &members),
            Ok(Publication::Published | Publication::Stale) => {
                for member in members {
                    let handle = SliceHandle {
                        shared,
                        slice: &member,
                    };
                    handle.with_mut(|s| {
                        s.native_group = None;
                        s.upload_task_active = false;
                    });
                    handle.mark_committed();
                }
            }
            Err(error) => {
                for member in members {
                    let handle = SliceHandle {
                        shared,
                        slice: &member,
                    };
                    handle.with_mut(|s| s.native_group = None);
                    handle.mark_failed(anyhow::anyhow!("native group: {error:#}"));
                }
            }
        }
    }

    fn resume_individual_uploads(shared: &Arc<Shared<B, M>>, members: &[Member]) {
        for member in members {
            {
                let mut s = member.lock();
                s.native_group = None;
                s.upload_task_active = false;
                s.slice_id = None;
                s.dispatched_end = 0;
                s.in_flight = 0;
                s.block_done = 0;
                s.uploaded = 0;
                s.state = SliceStatus::Readonly;
            }
            Self::spawn_flush_slice(shared.clone(), member.clone());
        }
    }

    async fn upload_native_group(
        shared: &Arc<Shared<B, M>>,
        chunk_id: u64,
        members: &[Member],
        limit: Instant,
        object: &mut Option<u64>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> anyhow::Result<Publication> {
        let (ino, index) = extract_ino_and_chunk_index(chunk_id);
        let epoch = members[0].lock().frozen_epoch;
        if epoch != 0 && shared.inode.data_epoch() != epoch {
            return Ok(Publication::Stale);
        }
        let meta = shared.backend.meta();
        let offsets: Vec<_> = members.iter().map(|s| s.lock().offset).collect();
        let allocate_and_upload = async {
            let id = u64::try_from(meta.next_id(SLICE_ID_KEY).await?)?;
            let mut parts = Vec::with_capacity(members.len());
            for member in members {
                let handle = SliceHandle {
                    shared,
                    slice: member,
                };
                handle.set_slice_id(id);
                let plan = handle.prepare_upload()?.context("missing native page")?;
                let before = parts.len();
                for (index, pages) in plan.data {
                    anyhow::ensure!(index == 0, "native page spans multiple blocks");
                    parts.extend(pages);
                }
                anyhow::ensure!(
                    parts[before..].iter().map(Bytes::len).sum::<usize>() == 4096,
                    "native member must cover exactly one page"
                );
            }
            let uploader = DataUploader::new(shared.config.layout, &shared.backend);
            let _upload = shared
                .recent_pending_upload
                .track_remote_upload_inflight(members.len() as u64 * 4096);
            *object = Some(id);
            backoff(UPLOAD_MAX_RETRIES, || async {
                uploader
                    .write_at_vectored_with_priority_and_limit(
                        id,
                        0u64.into(),
                        &parts,
                        UploadPriority::Foreground,
                        None,
                    )
                    .await
                    .map_err(|_| MetaError::ContinueRetry(RetryReason::VersionConflict))
            })
            .await?;
            Ok::<_, anyhow::Error>(id)
        };
        let id = tokio::time::timeout_at(limit.into(), allocate_and_upload)
            .await
            .context("native upload exceeded the existing slice deadline")??;
        drop(permit);
        for member in members {
            SliceHandle {
                shared,
                slice: member,
            }
            .advance_upload_range(0, 1, 4096);
        }
        let mut attempts = 0;
        loop {
            // Only the chunk's commit task publishes its immutable prefix.
            // Writers register every span in order while holding this lock.
            let inner = shared.inner.lock().await;
            if epoch != 0 && shared.inode.data_epoch() != epoch {
                return Ok(Publication::Stale);
            }
            let ordered = inner.chunks.get(&chunk_id).is_some_and(|chunk| {
                chunk.slices.len() >= members.len()
                    && chunk
                        .slices
                        .iter()
                        .zip(members)
                        .all(|(a, b)| Arc::ptr_eq(a, b))
            });
            if !ordered {
                return Ok(Publication::Reordered);
            }
            drop(inner);
            let _commit = shared.recent_pending_upload.track_metadata_commit();
            let observation = shared.request_observation();
            let publish = observation.as_ref().and_then(|observer| {
                observer.unscoped_segment_timer(RequestSegment::NativeMetadataPublish)
            });
            let status = meta.write_native_group(ino, chunk_id, id, &offsets).await;
            drop(publish);
            match status {
                Ok(()) => break,
                Err(err) => {
                    attempts += 1;
                    if !should_retry_meta_write(&err) || attempts >= COMMIT_META_MAX_RETRIES {
                        return Err(err.into());
                    }
                    tokio::time::sleep(commit_retry_backoff(attempts)).await;
                }
            }
        }
        meta.flush_metadata().await?;
        for offset in offsets {
            let absolute = index * shared.config.layout.chunk_size + offset;
            shared.inode.extend_committed_size(absolute + 4096);
            shared.inode.add_estimated_allocated_bytes(4096);
            shared.reader.invalidate(ino as u64, absolute, 4096).await?;
        }
        Ok(Publication::Published)
    }
}
