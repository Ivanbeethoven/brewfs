//! Explicit experimental handle dispatch. CLI mount admission stays disabled.
//!
//! Entity handles use their own authenticated data path and inode locks. They
//! never translate current maps to legacy slice history or use the legacy cache.

use super::*;
use crate::workspace_overlay::entity::{EntityError, data::EntityFileIo, model::EntityKind};

pub(super) fn error(error: EntityError) -> VfsError {
    match error {
        EntityError::Permission => VfsError::PermissionDenied {
            path: PathHint::none(),
        },
        EntityError::NotFound => VfsError::NotFound {
            path: PathHint::none(),
        },
        EntityError::Exists => VfsError::AlreadyExists {
            path: PathHint::none(),
        },
        EntityError::IsDirectory => VfsError::IsADirectory {
            path: PathHint::none(),
        },
        EntityError::NotDirectory => VfsError::NotADirectory {
            path: PathHint::none(),
        },
        EntityError::NotEmpty => VfsError::DirectoryNotEmpty {
            path: PathHint::none(),
        },
        EntityError::Fenced => VfsError::StaleNetworkFileHandle,
        EntityError::Busy | EntityError::Conflict => VfsError::ResourceBusy,
        EntityError::Overflow | EntityError::Exhausted => VfsError::FileTooLarge,
        EntityError::Invalid(_) => VfsError::InvalidInput,
        EntityError::Unsupported(_) => VfsError::Unsupported,
        EntityError::Corrupt(_) => VfsError::InvalidData,
        error => VfsError::Anyhow(anyhow::Error::new(error)),
    }
}

impl<S, M> VFS<S, M>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    pub(crate) fn is_entity_handle(&self, fh: u64) -> bool {
        self.file_handle(fh)
            .is_some_and(|handle| handle.entity_io.get().is_some())
    }

    pub(super) fn entity_file(&self, fh: u64) -> Option<Arc<dyn EntityFileIo>> {
        self.file_handle(fh)?.entity_io.get().cloned()
    }

    /// Bind an already permission-checked entity open to the VFS file APIs.
    /// This is an experimental SDK seam, not a mount factory. Path operations
    /// remain on EntityNamespace; the CLI deliberately rejects this format.
    pub async fn attach_entity_file(
        &self,
        format: crate::config::VolumeFormat,
        io: Arc<dyn EntityFileIo>,
        read: bool,
        write: bool,
        append: bool,
    ) -> Result<u64, VfsError> {
        if format != crate::config::VolumeFormat::WorkspaceEntityV1
            || self.core.backend.workspace_read_plan().is_some()
        {
            return Err(VfsError::InvalidInput);
        }
        #[cfg(feature = "native-packed-base")]
        if self.native_runtime().is_some() {
            return Err(VfsError::InvalidInput);
        }
        let inode = io.attr().await.map_err(error)?;
        if inode.kind != EntityKind::File || inode.ino != io.ino() || (!read && !write) {
            return Err(VfsError::InvalidInput);
        }
        // The data adapter enforces the original open access regardless of
        // these flags; an SDK caller cannot upgrade an O_RDONLY descriptor.
        let attr = FileAttr {
            ino: inode.ino,
            size: inode.size,
            blocks: inode.size.div_ceil(512),
            kind: FileType::File,
            mode: inode.mode,
            rdev: inode.rdev,
            uid: inode.uid,
            gid: inode.gid,
            atime: inode.atime_ns,
            mtime: inode.mtime_ns,
            ctime: inode.ctime_ns,
            nlink: inode.nlink,
        };
        let fh = self.state.handles.next_handle()?;
        let handle = Arc::new(FileHandle::new_owned(
            fh,
            inode.ino,
            attr,
            HandleFlags::new(read, write, append),
            None,
        ));
        // Fresh handle, not yet returned to a caller.
        if handle.entity_io.set(io).is_err() {
            return Err(VfsError::InvalidInput);
        }
        // Entity inode IDs belong to a separate namespace. Index only by this
        // unique VFS handle; never feed them into legacy inode caches/locks.
        self.state.handles.handles.insert(fh, handle);
        Ok(fh)
    }

    /// O_TRUNC/ftruncate for an explicitly attached experimental entity handle.
    pub async fn truncate_entity_file(&self, fh: u64, size: u64) -> Result<(), VfsError> {
        if !self.file_handle_required(fh)?.flags.write {
            return Err(VfsError::PermissionDenied {
                path: PathHint::none(),
            });
        }
        let io = self.entity_file(fh).ok_or(VfsError::InvalidInput)?;
        io.truncate(size).await.map_err(error)?;
        if let Some(handle) = self.file_handle(fh) {
            let inode = io.attr().await.map_err(error)?;
            let mut attr = handle.attr();
            attr.size = inode.size;
            attr.blocks = inode.size.div_ceil(512);
            attr.mtime = inode.mtime_ns;
            attr.ctime = inode.ctime_ns;
            handle.update_attr(&attr);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::store::InMemoryBlockStore;
    use crate::meta::factory::create_meta_store_from_url;
    use crate::workspace_overlay::entity::{
        backend::EntityBackend,
        data::{EntityData, test_lower_bytes},
        keys::EntityKeys,
        lifecycle::Lifecycle,
        memory::MemoryEntityBackend,
        namespace::{Credentials, EntityNamespace},
        redis::RedisEntityBackend,
        resolver::{ImmutableLower, test_support::MemoryLower},
    };
    use asyncfuse::raw::{Filesystem, Request, flags::FUSE_WRITE_CACHE};
    use asyncfuse::{Errno, SetAttr};
    use uuid::Uuid;

    async fn exercise<B: EntityBackend + 'static>(backend: Arc<B>, volume: Uuid) {
        let mut lower = MemoryLower::empty();
        lower.add_file(1, b"base", 2, 20);
        let lower = Arc::new(lower);
        let keys = EntityKeys::new(volume, Uuid::new_v4());
        let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
        lifecycle.provision(lower.binding()).await.unwrap();
        let session = lifecycle
            .acquire(Uuid::new_v4(), 300_000_000_000)
            .await
            .unwrap();
        let namespace = EntityNamespace::new(backend, lower, keys, session);
        let credentials = Credentials {
            uid: 0,
            gid: 0,
            groups: vec![0],
            umask: 0o022,
        };
        let handle = Arc::new(namespace.open(1, b"base", 6, &credentials).await.unwrap());
        let data = EntityData::new(
            namespace.clone(),
            Arc::new(crate::workspace_overlay::entity::test_objects::backend().await),
            8,
        )
        .await
        .unwrap();
        let io = data.bind(handle.clone()).unwrap();

        // Empty unrelated native metadata makes accidental legacy fallthrough
        // fail. All bytes, append offsets and durability use the entity handle.
        let native = create_meta_store_from_url("sqlite::memory:").await.unwrap();
        let vfs = VFS::new(
            ChunkLayout::default(),
            InMemoryBlockStore::new(),
            native.store(),
        )
        .await
        .unwrap();
        assert!(
            vfs.attach_entity_file(
                crate::config::VolumeFormat::WorkspaceV1,
                io.clone(),
                true,
                true,
                false
            )
            .await
            .is_err()
        );
        let fh = vfs
            .attach_entity_file(
                crate::config::VolumeFormat::WorkspaceEntityV1,
                io.clone(),
                true,
                true,
                false,
            )
            .await
            .unwrap();
        // An unsupported handle operation must never fall through to another
        // metadata namespace just because its numeric inode happens to match.
        let collision = vfs.create_file("/native-collision").await.unwrap();
        assert_eq!(collision, 2);
        let mismatched = vfs.create_file("/native-other").await.unwrap();
        // The public FUSE trait must also reject an SDK entity handle before
        // inode-only writeback, truncation or privilege-bit updates. Exercise
        // both a colliding inode number and a different supplied inode.
        for native_ino in [collision, mismatched] {
            let before = vfs.chmod(native_ino, 0o6777).await.unwrap();
            let request = Request {
                uid: 65534,
                gid: 65534,
                ..Request::default()
            };
            for flags in [0, FUSE_WRITE_CACHE] {
                assert_eq!(
                    Filesystem::write(
                        &vfs,
                        request,
                        native_ino as u64,
                        fh,
                        0,
                        b"foreign",
                        flags,
                        0,
                    )
                    .await
                    .err(),
                    Some(Errno::from(libc::EOPNOTSUPP))
                );
            }
            assert_eq!(
                Filesystem::setattr(
                    &vfs,
                    request,
                    native_ino as u64,
                    Some(fh),
                    SetAttr {
                        size: Some(4096),
                        ..SetAttr::default()
                    },
                )
                .await
                .err(),
                Some(Errno::from(libc::EOPNOTSUPP))
            );
            assert_eq!(
                Filesystem::getattr(&vfs, request, native_ino as u64, Some(fh), 0)
                    .await
                    .err(),
                Some(Errno::from(libc::EOPNOTSUPP))
            );
            assert_eq!(
                Filesystem::read(&vfs, request, native_ino as u64, fh, 0, 8)
                    .await
                    .err(),
                Some(Errno::from(libc::EOPNOTSUPP))
            );
            assert_eq!(
                Filesystem::flush(&vfs, request, native_ino as u64, fh, 0).await,
                Err(Errno::from(libc::EOPNOTSUPP))
            );
            assert_eq!(
                Filesystem::release(&vfs, request, native_ino as u64, fh, 0, 0, true).await,
                Err(Errno::from(libc::EOPNOTSUPP))
            );
            let after = vfs
                .meta_stat_required(native_ino, PathHint::none())
                .await
                .unwrap();
            let fingerprint = |attr: &FileAttr| {
                (
                    attr.size,
                    attr.blocks,
                    attr.mode,
                    attr.uid,
                    attr.gid,
                    attr.atime,
                    attr.mtime,
                    attr.ctime,
                    attr.nlink,
                )
            };
            assert_eq!(fingerprint(&after), fingerprint(&before));
        }
        assert!(vfs.file_handles_for_inode(collision).is_empty());
        assert!(matches!(
            vfs.fallocate_handle(fh, 2, 0, 4096).await,
            Err(VfsError::Unsupported)
        ));
        assert!(matches!(
            vfs.copy_file_range(fh, 0, fh, 1, 4).await,
            Err(VfsError::Unsupported)
        ));
        assert_eq!(
            vfs.meta_stat_required(collision, PathHint::none())
                .await
                .unwrap()
                .size,
            0
        );
        assert_eq!(
            vfs.read(fh, 0, 32).await.unwrap(),
            test_lower_bytes(2, 0, 20)
        );
        assert_eq!(vfs.write(fh, 6, b"replacement").await.unwrap(), 11);
        let mut expected = test_lower_bytes(2, 0, 20);
        expected[6..17].copy_from_slice(b"replacement");
        assert_eq!(vfs.read(fh, 0, 32).await.unwrap(), expected);
        vfs.fsync(fh, false).await.unwrap();
        vfs.truncate_entity_file(fh, 3).await.unwrap();
        vfs.truncate_entity_file(fh, 20).await.unwrap();
        expected[3..].fill(0);
        assert_eq!(vfs.read(fh, 0, 32).await.unwrap(), expected);
        let append = vfs
            .attach_entity_file(
                crate::config::VolumeFormat::WorkspaceEntityV1,
                io,
                true,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(vfs.write(append, 0, b"tail").await.unwrap(), 4);
        expected.extend_from_slice(b"tail");
        assert_eq!(vfs.read(fh, 0, 32).await.unwrap(), expected);
        vfs.fsync_snapshot(append, false).await.unwrap();
        vfs.flush_dirty_handle_snapshot(fh).await.unwrap();
        namespace
            .unlink(1, b"base".to_vec(), false, credentials)
            .await
            .unwrap();
        assert_eq!(vfs.read(fh, 0, 32).await.unwrap(), expected);
        vfs.close(append).await.unwrap();
        vfs.close(fh).await.unwrap();
        assert!(vfs.read(fh, 0, 1).await.is_err());
        drop(handle);
    }

    #[tokio::test]
    #[ignore = "requires isolated RustFS through BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET"]
    async fn entity_vfs_memory_write_append_truncate_fsync_and_open_unlink() {
        exercise(Arc::new(MemoryEntityBackend::new(1_000)), Uuid::new_v4()).await;
    }

    #[tokio::test]
    #[ignore = "requires isolated RustFS and BREWFS_TEST_REDIS_URL"]
    async fn entity_vfs_redis_write_append_truncate_fsync_and_open_unlink() {
        let url = std::env::var("BREWFS_TEST_REDIS_URL").expect("isolated Redis required");
        let volume = Uuid::new_v4();
        exercise(
            Arc::new(RedisEntityBackend::connect(&url, volume).await.unwrap()),
            volume,
        )
        .await;
    }
}
