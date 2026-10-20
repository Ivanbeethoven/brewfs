//! Write-through file I/O for the experimental current-entity namespace.
//!
//! A file operation retains the namespace's inode lock through transport and
//! publication. Other inodes remain independent. Uploads precede visibility;
//! each chunk publishes its inode/map/memberships and authenticated object
//! descriptor atomically. Mount admission remains a separate qualification.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::cadapter::client::ObjectBackend;

use super::backend::{
    CommitOutcome, EntityBackend, EntityPlan, MAX_TRANSACTION_BYTES, MAX_TRANSACTION_ROWS,
    RowCheck, RowWrite,
};
use super::codec;
use super::lifecycle::MaintenanceToken;
use super::model::{ChunkMap, ChunkSource, EntityInode, EntityKind, UpperInode};
use super::namespace::{EntityHandle, EntityNamespace};
use super::ownership::{Ownership, SliceOwnership, plan_chunk_membership};
use super::recovery::{DiscardProgress, Recovery, absent_intent_check, prepare_resize};
use super::resolver;
use super::{EntityError, Result};

pub use super::resolver::ImmutableLowerData;

pub const MAX_IO_BYTES: usize = 16 * 1024 * 1024;
const MAX_IO_CHUNKS: u64 = 64;
const MAX_READ_PIECES: usize = 4096;
const MAX_RETRIES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteOutcome {
    /// The selected starting offset, including the atomic append decision.
    pub offset: u64,
    /// Only the prefix whose metadata publications were acknowledged.
    pub written: usize,
    pub size: u64,
}

/// The existing VFS handle path can dispatch through this interface without
/// manufacturing legacy slices or a workspace-wide read generation.
#[async_trait]
pub trait EntityFileIo: Send + Sync {
    fn ino(&self) -> i64;
    async fn attr(&self) -> Result<EntityInode>;
    async fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>>;
    async fn write(&self, offset: u64, data: Bytes, append: bool) -> Result<WriteOutcome>;
    async fn truncate(&self, size: u64) -> Result<()>;
    async fn fsync(&self) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DataConfig {
    chunk_size: u64,
}

/// Immutable metadata committed with the first visible reference. The object
/// key is derived from the volume/namespace/slice identity, never arbitrary input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectDescriptor {
    pub namespace_id: Uuid,
    pub slice_id: u64,
    pub size: u64,
    pub sha256: [u8; 32],
}

pub struct EntityData<B, L, O> {
    namespace: Arc<EntityNamespace<B, L>>,
    objects: Arc<O>,
    chunk_size: u64,
    config_key: Vec<u8>,
    config_bytes: Vec<u8>,
}

struct BoundFile<B, L, O> {
    data: Arc<EntityData<B, L, O>>,
    handle: Arc<EntityHandle>,
}

#[derive(Default)]
struct ReadSet {
    rows: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    bytes: usize,
    now_ns: i64,
}

impl ReadSet {
    fn add(&mut self, check: RowCheck) -> Result<()> {
        if let Some(existing) = self.rows.get(&check.key) {
            return if existing == &check.expected {
                Ok(())
            } else {
                Err(EntityError::Conflict)
            };
        }
        let bytes = check.key.len() + check.expected.as_ref().map_or(0, Vec::len);
        if self.rows.len() >= MAX_TRANSACTION_ROWS
            || self
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > MAX_TRANSACTION_BYTES)
        {
            return Err(EntityError::Invalid("file read set exceeds bound".into()));
        }
        self.bytes += bytes;
        self.rows.insert(check.key, check.expected);
        Ok(())
    }

    async fn raw<B: EntityBackend>(
        &mut self,
        backend: &B,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>> {
        if let Some(value) = self.rows.get(&key) {
            return Ok(value.clone());
        }
        let snapshot = backend.read(std::slice::from_ref(&key)).await?;
        self.now_ns = snapshot.now_ns;
        let value =
            snapshot.rows.into_iter().next().ok_or_else(|| {
                EntityError::Corrupt("short entity file metadata response".into())
            })?;
        self.add(RowCheck {
            key,
            expected: value.clone(),
        })?;
        Ok(value)
    }

    fn plan(&self, writes: Vec<RowWrite>) -> EntityPlan {
        EntityPlan {
            checks: self
                .rows
                .iter()
                .map(|(key, expected)| RowCheck {
                    key: key.clone(),
                    expected: expected.clone(),
                })
                .collect(),
            writes,
        }
    }
}

impl<B, L, O> EntityData<B, L, O>
where
    B: EntityBackend + 'static,
    L: ImmutableLowerData + 'static,
    O: ObjectBackend + 'static,
{
    /// Establish immutable namespace geometry before exposing file I/O. This
    /// row is checked, never incremented, by ordinary file transactions.
    pub async fn new(
        namespace: Arc<EntityNamespace<B, L>>,
        objects: Arc<O>,
        chunk_size: u64,
    ) -> Result<Arc<Self>> {
        if chunk_size == 0 || chunk_size > MAX_IO_BYTES as u64 {
            return Err(EntityError::Invalid("invalid entity chunk size".into()));
        }
        let _admission = namespace.session.admit()?;
        let config_key = [namespace.keys.prefix(), b"data-config"].concat();
        let config_bytes = codec::encode(&DataConfig { chunk_size })?;
        for _ in 0..MAX_RETRIES {
            let authority = namespace.authority().await?;
            let snapshot = namespace
                .backend
                .read(std::slice::from_ref(&config_key))
                .await?;
            let current = snapshot
                .rows
                .into_iter()
                .next()
                .ok_or_else(|| EntityError::Corrupt("missing data geometry response".into()))?;
            if current.as_ref().is_some_and(|bytes| bytes != &config_bytes) {
                return Err(EntityError::Invalid(
                    "namespace chunk geometry differs".into(),
                ));
            }
            let writes = if current.is_none() {
                vec![RowWrite::Put {
                    key: config_key.clone(),
                    value: config_bytes.clone(),
                }]
            } else {
                Vec::new()
            };
            if namespace
                .backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: vec![RowCheck {
                            key: config_key.clone(),
                            expected: current,
                        }],
                        writes,
                    },
                )
                .await?
                == CommitOutcome::Committed
            {
                return Ok(Arc::new(Self {
                    namespace,
                    objects,
                    chunk_size,
                    config_key,
                    config_bytes,
                }));
            }
        }
        Err(EntityError::Busy)
    }

    pub fn bind(self: &Arc<Self>, handle: Arc<EntityHandle>) -> Result<Arc<dyn EntityFileIo>> {
        self.namespace.validate_handle(&handle, 0)?;
        Ok(Arc::new(BoundFile {
            data: self.clone(),
            handle,
        }))
    }

    fn read_set(&self) -> Result<ReadSet> {
        let mut rows = ReadSet::default();
        rows.add(RowCheck {
            key: self.config_key.clone(),
            expected: Some(self.config_bytes.clone()),
        })?;
        Ok(rows)
    }

    fn remember_failure(&self, ino: i64, error: &EntityError) {
        // Known rejections (including contention) publish no uncertain bytes.
        // Expiry remains typed fencing; it cannot become a clean live session.
        // Keep uncertain transport/commit outcomes and corruption sticky.
        if matches!(error, EntityError::Backend(_) | EntityError::Corrupt(_)) {
            self.namespace
                .session
                .remember_io_failure(ino, error.to_string());
        }
    }

    fn check_failure(&self, ino: i64) -> Result<()> {
        if let Some(error) = self.namespace.session.io_failure(ino) {
            return Err(EntityError::Backend(format!(
                "earlier file I/O failed: {error}"
            )));
        }
        Ok(())
    }

    fn check_range(&self, offset: u64, length: usize) -> Result<u64> {
        let end = offset
            .checked_add(length as u64)
            .ok_or(EntityError::Overflow)?;
        if length > MAX_IO_BYTES
            || end > i64::MAX as u64
            || (length != 0
                && (offset % self.chunk_size + length as u64).div_ceil(self.chunk_size)
                    > MAX_IO_CHUNKS)
        {
            return Err(EntityError::Invalid(
                "entity I/O range exceeds bound".into(),
            ));
        }
        Ok(end)
    }

    async fn inode(&self, ino: i64, rows: &mut ReadSet) -> Result<EntityInode> {
        let _diagnostic = super::diagnostics::Span::start("data.resolve_inode");
        let bytes = rows
            .raw(
                self.namespace.backend.as_ref(),
                self.namespace.keys.inode(ino),
            )
            .await?;
        let upper: Option<UpperInode> = bytes.as_deref().map(codec::decode).transpose()?;
        let inode = resolver::resolve_inode(
            self.namespace.lower.as_ref(),
            self.namespace.lower.binding(),
            ino,
            upper.as_ref(),
        )
        .await?
        .ok_or(EntityError::NotFound)?;
        if inode.kind != EntityKind::File {
            return Err(EntityError::Unsupported(
                "entity file I/O requires regular inode",
            ));
        }
        // nlink == 0 is intentionally valid for an admitted, retained open handle.
        Ok(inode)
    }

    async fn chunk(&self, inode: &EntityInode, index: u64, rows: &mut ReadSet) -> Result<ChunkMap> {
        let _diagnostic = super::diagnostics::Span::start("data.resolve_chunk");
        let bytes = rows
            .raw(
                self.namespace.backend.as_ref(),
                self.namespace.keys.chunk(inode.ino, index),
            )
            .await?;
        let upper: Option<ChunkMap> = bytes.as_deref().map(codec::decode).transpose()?;
        resolver::resolve_chunk(
            self.namespace.lower.as_ref(),
            self.namespace.lower.binding(),
            inode,
            index,
            self.chunk_size,
            upper.as_ref(),
        )
        .await
    }

    async fn recover_locked(&self, ino: i64) -> Result<()> {
        let _diagnostic = super::diagnostics::Span::start("data.recover");
        let recovery = Recovery::new(self.namespace.backend.clone(), self.namespace.keys.clone());
        // The reference scanner admits at most 4096 chunk rows. One additional
        // step clears the intent after the final boundary/tail row.
        for _ in 0..4098 {
            let mut completed = None;
            for _ in 0..MAX_RETRIES {
                let authority = self.namespace.authority().await?;
                match recovery.truncate_step(&authority, ino).await {
                    Err(EntityError::Conflict) => continue,
                    result => {
                        completed = Some(result?);
                        break;
                    }
                }
            }
            match completed {
                Some(true) => return Ok(()),
                Some(false) => {}
                None => return Err(EntityError::Busy),
            }
        }
        Err(EntityError::Busy)
    }

    pub async fn attr(self: &Arc<Self>, handle: Arc<EntityHandle>) -> Result<EntityInode> {
        let ino = self.namespace.validate_handle(&handle, 0)?;
        let _admission = self.namespace.session.admit()?;
        let _guard = self.namespace.lock_inode(ino).await;
        for _ in 0..MAX_RETRIES {
            let authority = self.namespace.authority().await?;
            let mut rows = self.read_set()?;
            let inode = self.inode(ino, &mut rows).await?;
            if self
                .namespace
                .backend
                .commit(&authority, &rows.plan(Vec::new()))
                .await?
                == CommitOutcome::Committed
            {
                return Ok(inode);
            }
        }
        Err(EntityError::Busy)
    }

    pub async fn write(
        self: &Arc<Self>,
        handle: Arc<EntityHandle>,
        offset: u64,
        data: Bytes,
        append: bool,
    ) -> Result<WriteOutcome> {
        let ino = self.namespace.validate_handle(&handle, 2)?;
        if data.len() > MAX_IO_BYTES {
            return Err(EntityError::Invalid("entity write exceeds bound".into()));
        }
        let admission = self.namespace.session.admit()?;
        // Enqueue on the shared FIFO lock before spawning; a following fsync
        // cannot overtake an already admitted write waiting on this inode.
        let guard = self.namespace.lock_inode(ino).await;
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let (_handle, _admission, _guard) = (handle, admission, guard);
            this.write_locked(ino, offset, data, append).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned file write failed: {e}")))?
    }

    async fn write_locked(
        &self,
        ino: i64,
        offset: u64,
        data: Bytes,
        append: bool,
    ) -> Result<WriteOutcome> {
        let _diagnostic = super::diagnostics::Span::start("data.write");
        self.namespace.authority().await?;
        self.check_failure(ino)?;
        self.recover_locked(ino).await?;
        let mut initial = self.read_set()?;
        let inode = self.inode(ino, &mut initial).await?;
        let offset = if append { inode.size } else { offset };
        self.check_range(offset, data.len())?;
        let mut result = WriteOutcome {
            offset,
            written: 0,
            size: inode.size,
        };
        while result.written < data.len() {
            let position = offset + result.written as u64;
            let length = (self.chunk_size - position % self.chunk_size)
                .min((data.len() - result.written) as u64) as usize;
            let bytes = data.slice(result.written..result.written + length);
            match self.write_chunk_locked(ino, position, bytes).await {
                Ok(size) => {
                    result.written += length;
                    result.size = size;
                }
                Err(error) => {
                    self.remember_failure(ino, &error);
                    return if result.written == 0 {
                        Err(error)
                    } else {
                        Ok(result)
                    };
                }
            }
        }
        // Empty writes still validate live authority and the observed inode.
        if data.is_empty() {
            let authority = self.namespace.authority().await?;
            if self
                .namespace
                .backend
                .commit(&authority, &initial.plan(Vec::new()))
                .await?
                != CommitOutcome::Committed
            {
                return Err(EntityError::Conflict);
            }
        }
        Ok(result)
    }

    async fn write_chunk_locked(&self, ino: i64, offset: u64, data: Bytes) -> Result<u64> {
        let _diagnostic = super::diagnostics::Span::start("data.write_chunk");
        let ownership = Ownership::new(self.namespace.backend.clone(), self.namespace.keys.clone());
        let mut reserved = None;
        for _ in 0..MAX_RETRIES {
            match ownership
                .allocate_upload(&self.namespace.session, data.len() as u64)
                .await
            {
                Ok(token) => {
                    reserved = Some(token);
                    break;
                }
                // Renewal may change exact authority after INCR but before the
                // owner-row transaction. Gaps are safe; no PUT has started.
                Err(EntityError::Conflict) => continue,
                Err(error) => return Err(error),
            }
        }
        let token = reserved.ok_or(EntityError::Busy)?;
        let descriptor = ObjectDescriptor {
            namespace_id: self.namespace.keys.namespace_id(),
            slice_id: token.slice_id(),
            size: data.len() as u64,
            sha256: Sha256::digest(&data).into(),
        };
        {
            let _diagnostic = super::diagnostics::Span::start("object.put");
            self.objects
                .put_object_create_only_durable(
                    &self.namespace.keys.object(token.slice_id()),
                    &data,
                )
                .await
                .map_err(|e| EntityError::Backend(format!("entity object PUT failed: {e}")))?;
        }
        let mut uploaded = false;
        for _ in 0..MAX_RETRIES {
            match ownership.uploaded(&self.namespace.session, &token).await {
                Ok(()) => {
                    uploaded = true;
                    break;
                }
                Err(EntityError::Conflict) => continue,
                Err(error) => return Err(error),
            }
        }
        if !uploaded {
            return Err(EntityError::Busy);
        }
        let index = offset / self.chunk_size;
        for _ in 0..MAX_RETRIES {
            let authority = self.namespace.authority().await?;
            let mut rows = self.read_set()?;
            rows.add(absent_intent_check(&self.namespace.keys, ino))?;
            let mut inode = self.inode(ino, &mut rows).await?;
            let old = self.chunk(&inode, index, &mut rows).await?;
            let new = old.replace(
                offset % self.chunk_size,
                data.len() as u64,
                ChunkSource::Data {
                    slice_id: token.slice_id(),
                    slice_offset: 0,
                },
            )?;
            let membership = plan_chunk_membership(
                self.namespace.backend.as_ref(),
                &self.namespace.keys,
                ino,
                index,
                &old,
                &new,
            )
            .await?;
            for check in membership.checks {
                rows.add(check)?;
            }
            rows.add(RowCheck {
                key: self.namespace.keys.object_descriptor(token.slice_id()),
                expected: None,
            })?;
            inode.size = inode.size.max(offset + data.len() as u64);
            inode.data_version = inode
                .data_version
                .checked_add(1)
                .ok_or(EntityError::Overflow)?;
            inode.mtime_ns = rows.now_ns;
            inode.ctime_ns = rows.now_ns;
            inode.mode &= !0o6000;
            inode.validate()?;
            let size = inode.size;
            let mut writes = membership.writes;
            writes.extend([
                RowWrite::Put {
                    key: self.namespace.keys.inode(ino),
                    value: codec::encode(&UpperInode::Present(inode))?,
                },
                RowWrite::Put {
                    key: self.namespace.keys.chunk(ino, index),
                    value: codec::encode(&new)?,
                },
                RowWrite::Put {
                    key: self.namespace.keys.object_descriptor(token.slice_id()),
                    value: codec::encode(&descriptor)?,
                },
            ]);
            if self
                .namespace
                .backend
                .commit(&authority, &rows.plan(writes))
                .await?
                == CommitOutcome::Committed
            {
                return Ok(size);
            }
        }
        Err(EntityError::Busy)
    }

    pub async fn read(
        self: &Arc<Self>,
        handle: Arc<EntityHandle>,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        let ino = self.namespace.validate_handle(&handle, 4)?;
        self.check_range(offset, length)?;
        let admission = self.namespace.session.admit()?;
        let guard = self.namespace.lock_inode(ino).await;
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let (_handle, _admission, _guard) = (handle, admission, guard);
            this.read_locked(ino, offset, length).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned file read failed: {e}")))?
    }

    async fn read_locked(&self, ino: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        let _diagnostic = super::diagnostics::Span::start("data.read");
        for _ in 0..MAX_RETRIES {
            let authority = self.namespace.authority().await?;
            let mut rows = self.read_set()?;
            let inode = self.inode(ino, &mut rows).await?;
            let length = inode.size.saturating_sub(offset).min(length as u64) as usize;
            let mut output = vec![0; length];
            let mut position = offset;
            let mut pieces = Vec::new();
            let mut descriptors = BTreeMap::new();
            while position < offset + length as u64 {
                let index = position / self.chunk_size;
                let count = (self.chunk_size - position % self.chunk_size)
                    .min(offset + length as u64 - position);
                let map = self.chunk(&inode, index, &mut rows).await?;
                let membership = plan_chunk_membership(
                    self.namespace.backend.as_ref(),
                    &self.namespace.keys,
                    ino,
                    index,
                    &map,
                    &map,
                )
                .await?;
                for check in membership.checks {
                    rows.add(check)?;
                }
                for span in map.clip(position % self.chunk_size, count)? {
                    if pieces.len() >= MAX_READ_PIECES {
                        return Err(EntityError::Invalid(
                            "entity read fragmentation exceeds bound".into(),
                        ));
                    }
                    if let ChunkSource::Data { slice_id, .. } = span.source
                        && !descriptors.contains_key(&slice_id)
                    {
                        let bytes = rows
                            .raw(
                                self.namespace.backend.as_ref(),
                                self.namespace.keys.object_descriptor(slice_id),
                            )
                            .await?
                            .ok_or_else(|| {
                                EntityError::Corrupt(
                                    "visible slice lacks object authentication".into(),
                                )
                            })?;
                        let descriptor: ObjectDescriptor = codec::decode(&bytes)?;
                        let ownership: SliceOwnership = codec::decode(
                            rows.rows
                                .get(&self.namespace.keys.slice(slice_id))
                                .and_then(Option::as_deref)
                                .ok_or_else(|| {
                                    EntityError::Corrupt("missing validated slice owner".into())
                                })?,
                        )?;
                        if descriptor.namespace_id != self.namespace.keys.namespace_id()
                            || descriptor.slice_id != slice_id
                            || descriptor.size != ownership.size
                            || descriptor.size == 0
                            || descriptor.size > MAX_IO_BYTES as u64
                        {
                            return Err(EntityError::Corrupt(
                                "invalid entity object descriptor".into(),
                            ));
                        }
                        descriptors.insert(slice_id, descriptor);
                    }
                    pieces.push((index * self.chunk_size + span.offset - offset, span));
                }
                position += count;
            }
            let plan = rows.plan(Vec::new());
            if self.namespace.backend.commit(&authority, &plan).await? == CommitOutcome::Conflict {
                continue;
            }
            for (destination, span) in pieces {
                let destination =
                    usize::try_from(destination).map_err(|_| EntityError::Overflow)?;
                let count = usize::try_from(span.length).map_err(|_| EntityError::Overflow)?;
                let target = &mut output[destination..destination + count];
                match span.source {
                    ChunkSource::Hole => {}
                    ChunkSource::LowerRange {
                        binding_digest,
                        ino,
                        file_offset,
                    } => {
                        if binding_digest != self.namespace.lower.binding().manifest_digest {
                            return Err(EntityError::Fenced);
                        }
                        let bytes = self
                            .namespace
                            .lower
                            .read_range(ino, file_offset, count)
                            .await?;
                        if bytes.len() != count {
                            return Err(EntityError::Corrupt(
                                "short authenticated lower read".into(),
                            ));
                        }
                        target.copy_from_slice(&bytes);
                    }
                    ChunkSource::Data {
                        slice_id,
                        slice_offset,
                    } => {
                        let descriptor = descriptors
                            .get(&slice_id)
                            .expect("validated object descriptor");
                        let bytes = self.read_object(descriptor).await?;
                        let start =
                            usize::try_from(slice_offset).map_err(|_| EntityError::Overflow)?;
                        let source = bytes
                            .get(start..start.checked_add(count).ok_or(EntityError::Overflow)?)
                            .ok_or_else(|| {
                                EntityError::Corrupt(
                                    "slice read exceeds authenticated object".into(),
                                )
                            })?;
                        target.copy_from_slice(source);
                    }
                }
            }
            // Revalidate after transport too. Old objects remain protected by
            // the retained handle/admission even if authority expired meanwhile.
            if self.namespace.backend.commit(&authority, &plan).await? == CommitOutcome::Committed {
                return Ok(output);
            }
        }
        Err(EntityError::Busy)
    }

    async fn read_object(&self, descriptor: &ObjectDescriptor) -> Result<Vec<u8>> {
        let _diagnostic = super::diagnostics::Span::start("object.read");
        let key = self.namespace.keys.object(descriptor.slice_id);
        let head_diagnostic = super::diagnostics::Span::start("object.head");
        let size = self
            .objects
            .get_object_size_bounded(&key)
            .await
            .map_err(|e| EntityError::Backend(format!("entity object HEAD failed: {e}")))?;
        drop(head_diagnostic);
        if size != Some(descriptor.size) {
            return Err(EntityError::Corrupt(
                "entity object size or presence mismatch".into(),
            ));
        }
        let get_diagnostic = super::diagnostics::Span::start("object.get");
        let mut stream = self
            .objects
            .get_object_range_stream(&key, 0, descriptor.size)
            .await
            .map_err(|e| EntityError::Backend(format!("entity object GET failed: {e}")))?;
        let mut bytes = Vec::with_capacity(descriptor.size as usize);
        while let Some(part) = stream.next().await {
            let part =
                part.map_err(|e| EntityError::Backend(format!("entity object body failed: {e}")))?;
            if bytes
                .len()
                .checked_add(part.len())
                .is_none_or(|n| n > descriptor.size as usize)
            {
                return Err(EntityError::Corrupt(
                    "oversized entity object response".into(),
                ));
            }
            bytes.extend_from_slice(&part);
        }
        drop(get_diagnostic);
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if bytes.len() != descriptor.size as usize || digest != descriptor.sha256 {
            return Err(EntityError::Corrupt(
                "entity object authentication failed".into(),
            ));
        }
        Ok(bytes)
    }

    pub async fn truncate(self: &Arc<Self>, handle: Arc<EntityHandle>, size: u64) -> Result<()> {
        let ino = self.namespace.validate_handle(&handle, 2)?;
        if size > i64::MAX as u64 {
            return Err(EntityError::Invalid(
                "entity size exceeds signed offset range".into(),
            ));
        }
        let admission = self.namespace.session.admit()?;
        let guard = self.namespace.lock_inode(ino).await;
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let (_handle, _admission, _guard) = (handle, admission, guard);
            this.truncate_locked(ino, size).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned file truncate failed: {e}")))?
    }

    async fn truncate_locked(&self, ino: i64, size: u64) -> Result<()> {
        self.namespace.authority().await?;
        self.check_failure(ino)?;
        self.recover_locked(ino).await?;
        for _ in 0..MAX_RETRIES {
            let authority = self.namespace.authority().await?;
            let mut rows = self.read_set()?;
            let inode = self.inode(ino, &mut rows).await?;
            let old = rows
                .rows
                .get(&self.namespace.keys.inode(ino))
                .cloned()
                .flatten();
            let mut plan = prepare_resize(
                &self.namespace.keys,
                ino,
                old,
                &inode,
                size,
                self.chunk_size,
                rows.now_ns,
            )?;
            for check in plan.checks.drain(..) {
                rows.add(check)?;
            }
            match self
                .namespace
                .backend
                .commit(&authority, &rows.plan(plan.writes))
                .await
            {
                Ok(CommitOutcome::Committed) => return self.recover_locked(ino).await,
                Ok(CommitOutcome::Conflict) => {}
                Err(error) => {
                    if matches!(error, EntityError::Backend(_)) {
                        self.remember_failure(ino, &error);
                    }
                    return Err(error);
                }
            }
        }
        Err(EntityError::Busy)
    }

    /// Every acknowledged write already completed upload and publication.
    /// This FIFO file barrier waits for preceding operations, finishes a visible
    /// shrink, reports retained failures, and validates the configured metadata
    /// durability boundary. Redis persistence policy remains deployment policy.
    pub async fn fsync(self: &Arc<Self>, handle: Arc<EntityHandle>) -> Result<()> {
        let ino = self.namespace.validate_handle(&handle, 0)?;
        let admission = self.namespace.session.admit()?;
        let guard = self.namespace.lock_inode(ino).await;
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let (_handle, _admission, _guard) = (handle, admission, guard);
            this.namespace.authority().await?;
            this.check_failure(ino)?;
            this.recover_locked(ino).await?;
            for _ in 0..MAX_RETRIES {
                let authority = this.namespace.authority().await?;
                let mut rows = this.read_set()?;
                this.inode(ino, &mut rows).await?;
                rows.add(absent_intent_check(&this.namespace.keys, ino))?;
                if this
                    .namespace
                    .backend
                    .commit(&authority, &rows.plan(Vec::new()))
                    .await?
                    == CommitOutcome::Committed
                {
                    return Ok(());
                }
            }
            Err(EntityError::Busy)
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned file fsync failed: {e}")))?
    }

    /// One bounded administrative cleanup step. The recovery transaction stores
    /// deletion exclusion before this owned task submits transport DELETE. A
    /// lost reply is returned without replay; a later explicit step reconciles
    /// the same permanently reserved object identity.
    pub async fn discard_step(self: &Arc<Self>, maintenance: &MaintenanceToken) -> Result<bool> {
        let this = self.clone();
        let maintenance = maintenance.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let recovery =
                Recovery::new(this.namespace.backend.clone(), this.namespace.keys.clone());
            match recovery.discard_step(&maintenance).await? {
                DiscardProgress::Pending => Ok(false),
                DiscardProgress::Complete => Ok(true),
                DiscardProgress::ObjectDeletion(token) => {
                    if !this.objects.forbids_mutation_replay() {
                        return Err(EntityError::Unsupported(
                            "entity deletion requires nonreplaying transport",
                        ));
                    }
                    let ownership =
                        Ownership::new(this.namespace.backend.clone(), this.namespace.keys.clone());
                    ownership.validate_delete(&maintenance, &token).await?;
                    this.objects
                        .delete_object(&this.namespace.keys.object(token.slice_id()))
                        .await
                        .map_err(|e| {
                            EntityError::Backend(format!("entity object DELETE failed: {e}"))
                        })?;
                    ownership.complete_delete(&maintenance, &token).await?;
                    Ok(false)
                }
            }
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned entity discard failed: {e}")))?
    }
}

#[async_trait]
impl<B, L, O> EntityFileIo for BoundFile<B, L, O>
where
    B: EntityBackend + 'static,
    L: ImmutableLowerData + 'static,
    O: ObjectBackend + 'static,
{
    fn ino(&self) -> i64 {
        self.handle.ino()
    }
    async fn attr(&self) -> Result<EntityInode> {
        self.data.attr(self.handle.clone()).await
    }
    async fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.data.read(self.handle.clone(), offset, length).await
    }
    async fn write(&self, offset: u64, data: Bytes, append: bool) -> Result<WriteOutcome> {
        self.data
            .write(self.handle.clone(), offset, data, append)
            .await
    }
    async fn truncate(&self, size: u64) -> Result<()> {
        self.data.truncate(self.handle.clone(), size).await
    }
    async fn fsync(&self) -> Result<()> {
        self.data.fsync(self.handle.clone()).await
    }
}

#[cfg(test)]
#[async_trait]
impl ImmutableLowerData for super::resolver::test_support::MemoryLower {
    async fn read_range(&self, ino: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        let inode = self.inodes.get(&ino).ok_or(EntityError::NotFound)?;
        let end = offset
            .checked_add(length as u64)
            .ok_or(EntityError::Overflow)?;
        if inode.kind != EntityKind::File || length > MAX_IO_BYTES || end > inode.size {
            return Err(EntityError::Invalid("test lower read outside file".into()));
        }
        Ok(test_lower_bytes(ino, offset, length))
    }
}

#[cfg(test)]
pub(crate) fn test_lower_bytes(ino: i64, offset: u64, length: usize) -> Vec<u8> {
    (offset..offset + length as u64)
        .map(|position| ((ino as u64 * 17 + position) % 251) as u8)
        .collect()
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
