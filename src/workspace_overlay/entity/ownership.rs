//! Durable upper-object ownership, independent of inode link counts.
//!
//! Chunk membership is counted once per (namespace, inode, chunk, slice).
//! Removing a membership creates a retirement hold even when other current
//! memberships remain. Only a drained namespace may clear that reader hold.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::backend::{EntityBackend, EntityPlan, RowCheck, RowWrite};
use super::codec::decode;
use super::keys::EntityKeys;
use super::lifecycle::{
    LeaseIdentity, Lifecycle, MaintenanceToken, OperationGuard, WriterSession, checked_rows,
    committed, put,
};
use super::model::{ChunkMap, EntityDentry, UpperInode};
use super::{EntityError, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SliceState {
    Pending {
        upload_id: Uuid,
        lease: LeaseIdentity,
    },
    Available,
    Deleting {
        reservation: Uuid,
    },
    Deleted {
        reservation: Uuid,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SliceOwnership {
    pub namespace_id: Uuid,
    pub slice_id: u64,
    pub size: u64,
    pub state: SliceState,
    pub current_refs: u64,
    /// Upload or unpublished object protection, consumed at first publication.
    pub staging_hold: bool,
    /// Persistent protection of previously returned read plans.
    pub retirement_hold: bool,
    /// Future immutable-publication owners; no API releases them implicitly.
    pub publication_refs: u64,
}

impl SliceOwnership {
    fn validate(&self, keys: &EntityKeys, slice_id: u64) -> Result<()> {
        if self.namespace_id != keys.namespace_id()
            || self.slice_id != slice_id
            || slice_id == 0
            || slice_id > i64::MAX as u64
            || self.size == 0
            || (matches!(self.state, SliceState::Pending { .. })
                && (!self.staging_hold || self.current_refs != 0))
            || (matches!(
                self.state,
                SliceState::Deleting { .. } | SliceState::Deleted { .. }
            ) && (self.current_refs != 0
                || self.staging_hold
                || self.retirement_hold
                || self.publication_refs != 0))
        {
            return Err(EntityError::Corrupt(
                "invalid or foreign slice ownership".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SliceMembership {
    pub namespace_id: Uuid,
    pub slice_id: u64,
    pub ino: i64,
    pub chunk_index: u64,
}

#[derive(Clone, Debug)]
pub struct UploadToken {
    namespace_id: Uuid,
    namespace_prefix: Vec<u8>,
    slice_id: u64,
    upload_id: Uuid,
    lease: LeaseIdentity,
    _admission: Arc<OperationGuard>,
}

#[derive(Clone, Debug)]
pub struct DeletionToken {
    namespace_id: Uuid,
    namespace_prefix: Vec<u8>,
    slice_id: u64,
    reservation: Uuid,
}

impl UploadToken {
    pub fn slice_id(&self) -> u64 {
        self.slice_id
    }
}
impl DeletionToken {
    pub fn slice_id(&self) -> u64 {
        self.slice_id
    }
}

pub struct Ownership<B> {
    backend: Arc<B>,
    keys: EntityKeys,
    lifecycle: Lifecycle<B>,
}

impl<B: EntityBackend> Ownership<B> {
    pub fn new(backend: Arc<B>, keys: EntityKeys) -> Self {
        Self {
            lifecycle: Lifecycle::new(backend.clone(), keys.clone()),
            backend,
            keys,
        }
    }

    /// Uses the volume's plain unique-ID allocator; allocation gaps are safe.
    pub async fn allocate_upload(&self, session: &WriterSession, size: u64) -> Result<UploadToken> {
        let _guard = session.admit()?;
        let authority = self.lifecycle.authority(session).await?;
        let slice_id = self
            .backend
            .allocate(&authority, &self.keys.counter("slice"))
            .await?;
        self.register_upload(session, slice_id, size).await
    }

    /// Register before PUT. Retain this token until upload and any attempted
    /// publication finish; it owns an admission guard, including across clones.
    pub async fn register_upload(
        &self,
        session: &WriterSession,
        slice_id: u64,
        size: u64,
    ) -> Result<UploadToken> {
        let admission = Arc::new(session.admit()?);
        if slice_id == 0 || slice_id > i64::MAX as u64 || size == 0 {
            return Err(EntityError::Invalid("invalid upload identity".into()));
        }
        let authority = self.lifecycle.authority(session).await?;
        let token = UploadToken {
            namespace_id: self.keys.namespace_id(),
            namespace_prefix: self.keys.prefix().to_vec(),
            slice_id,
            upload_id: Uuid::new_v4(),
            lease: session.identity(),
            _admission: admission,
        };
        let row = SliceOwnership {
            namespace_id: token.namespace_id,
            slice_id,
            size,
            state: SliceState::Pending {
                upload_id: token.upload_id,
                lease: token.lease,
            },
            current_refs: 0,
            staging_hold: true,
            retirement_hold: false,
            publication_refs: 0,
        };
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: vec![RowCheck {
                            key: self.keys.slice(slice_id),
                            expected: None,
                        }],
                        writes: vec![put(self.keys.slice(slice_id), &row)?],
                    },
                )
                .await?,
        )?;
        Ok(token)
    }

    /// The storage adapter calls this only after its durable PUT acknowledgment.
    /// It is not itself evidence that any transport or VFS fsync has completed.
    pub async fn uploaded(&self, session: &WriterSession, token: &UploadToken) -> Result<()> {
        let _guard = session.admit()?;
        if token.namespace_id != self.keys.namespace_id()
            || token.namespace_prefix != self.keys.prefix()
            || token.lease != session.identity()
        {
            return Err(EntityError::Fenced);
        }
        let authority = self.lifecycle.authority(session).await?;
        let keys = vec![self.keys.slice(token.slice_id)];
        let snapshot = self.backend.read(&keys).await?;
        let mut row: SliceOwnership =
            decode(snapshot.rows[0].as_deref().ok_or(EntityError::NotFound)?)?;
        row.validate(&self.keys, token.slice_id)?;
        match row.state {
            SliceState::Pending { upload_id, lease }
                if upload_id == token.upload_id && lease == token.lease => {}
            SliceState::Available => return Ok(()),
            _ => return Err(EntityError::Fenced),
        }
        row.state = SliceState::Available;
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: checked_rows(&keys, snapshot.rows),
                        writes: vec![put(keys[0].clone(), &row)?],
                    },
                )
                .await?,
        )
    }

    /// An upload can be abandoned only after all its local publishers drained.
    /// Its immutable object identity remains reserved, including after deletion.
    pub async fn abandon_upload(
        &self,
        maintenance: &MaintenanceToken,
        slice_id: u64,
    ) -> Result<()> {
        let authority = self.lifecycle.maintenance_authority(maintenance).await?;
        let keys = vec![self.keys.slice(slice_id)];
        let snapshot = self.backend.read(&keys).await?;
        let mut row: SliceOwnership =
            decode(snapshot.rows[0].as_deref().ok_or(EntityError::NotFound)?)?;
        row.validate(&self.keys, slice_id)?;
        if row.current_refs != 0 || row.publication_refs != 0 {
            return Err(EntityError::Busy);
        }
        if !matches!(
            row.state,
            SliceState::Pending { .. } | SliceState::Available
        ) {
            return Err(EntityError::Busy);
        }
        row.state = SliceState::Available;
        row.staging_hold = false;
        row.retirement_hold = true;
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: checked_rows(&keys, snapshot.rows),
                        writes: vec![put(keys[0].clone(), &row)?],
                    },
                )
                .await?,
        )
    }

    /// Persists deletion exclusion before the caller sends an object DELETE.
    /// Pending and unpublished uploads, current maps and publication owners all
    /// prevent reservation. Quiescence alone never removes those ownerships.
    pub async fn reserve_delete(
        &self,
        maintenance: &MaintenanceToken,
        slice_id: u64,
    ) -> Result<DeletionToken> {
        let authority = self.lifecycle.maintenance_authority(maintenance).await?;
        let keys = vec![self.keys.slice(slice_id)];
        let snapshot = self.backend.read(&keys).await?;
        let mut row: SliceOwnership =
            decode(snapshot.rows[0].as_deref().ok_or(EntityError::NotFound)?)?;
        row.validate(&self.keys, slice_id)?;
        if row.current_refs != 0 || row.staging_hold || row.publication_refs != 0 {
            return Err(EntityError::Busy);
        }
        if let SliceState::Deleting { reservation } | SliceState::Deleted { reservation } =
            row.state
        {
            return Ok(DeletionToken {
                namespace_id: self.keys.namespace_id(),
                namespace_prefix: self.keys.prefix().to_vec(),
                slice_id,
                reservation,
            });
        }
        if row.state != SliceState::Available {
            return Err(EntityError::Busy);
        }
        let reservation = Uuid::new_v4();
        row.retirement_hold = false;
        row.state = SliceState::Deleting { reservation };
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: checked_rows(&keys, snapshot.rows),
                        writes: vec![put(keys[0].clone(), &row)?],
                    },
                )
                .await?,
        )?;
        Ok(DeletionToken {
            namespace_id: self.keys.namespace_id(),
            namespace_prefix: self.keys.prefix().to_vec(),
            slice_id,
            reservation,
        })
    }

    /// Call after idempotent object DELETE succeeds. Preserve a tombstone so an
    /// object ID cannot be registered again after stale allocation/restore.
    pub async fn complete_delete(
        &self,
        maintenance: &MaintenanceToken,
        token: &DeletionToken,
    ) -> Result<()> {
        if token.namespace_id != self.keys.namespace_id()
            || token.namespace_prefix != self.keys.prefix()
        {
            return Err(EntityError::Fenced);
        }
        let authority = self.lifecycle.maintenance_authority(maintenance).await?;
        let keys = vec![self.keys.slice(token.slice_id)];
        let snapshot = self.backend.read(&keys).await?;
        let mut row: SliceOwnership =
            decode(snapshot.rows[0].as_deref().ok_or(EntityError::NotFound)?)?;
        row.validate(&self.keys, token.slice_id)?;
        match row.state {
            SliceState::Deleted { reservation } if reservation == token.reservation => {
                return Ok(());
            }
            SliceState::Deleting { reservation } if reservation == token.reservation => {}
            _ => return Err(EntityError::Fenced),
        }
        row.state = SliceState::Deleted {
            reservation: token.reservation,
        };
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: checked_rows(&keys, snapshot.rows),
                        writes: vec![put(keys[0].clone(), &row)?],
                    },
                )
                .await?,
        )
    }

    /// One recoverable, atomic cleanup step after session quiescence. This
    /// bounded reference walker supports at most 4096 rows per scanned prefix;
    /// larger namespaces fail closed until a paginated walker is qualified.
    /// Returns true once the orphan is gone. Removing each chunk and advancing
    /// its cursor share the membership/refcount transaction, preventing replay
    /// from releasing an owner twice. Lower data is never treated as upper-owned.
    pub async fn recover_orphan_step(
        &self,
        maintenance: &MaintenanceToken,
        ino: i64,
    ) -> Result<bool> {
        if ino <= 0 {
            return Err(EntityError::Invalid("invalid orphan inode".into()));
        }
        let authority = self.lifecycle.maintenance_authority(maintenance).await?;
        let keys = vec![self.keys.orphan(ino), self.keys.inode(ino)];
        let snapshot = self.backend.read(&keys).await?;
        let Some(bytes) = snapshot.rows[0].as_deref() else {
            return Ok(true);
        };
        let mut orphan: OrphanRecord = decode(bytes)?;
        if orphan.namespace_id != self.keys.namespace_id() || orphan.ino != ino {
            return Err(EntityError::Corrupt("invalid orphan identity".into()));
        }
        let inode: UpperInode = decode(
            snapshot.rows[1]
                .as_deref()
                .ok_or_else(|| EntityError::Corrupt("orphan missing inode".into()))?,
        )?;
        let UpperInode::Present(inode) = inode else {
            return Err(EntityError::Corrupt(
                "orphan inode already tombstoned".into(),
            ));
        };
        inode.validate()?;
        if inode.ino != ino || inode.nlink != 0 {
            return Err(EntityError::Corrupt("orphan still linked".into()));
        }
        let chunk_prefix = [self.keys.prefix(), format!("chunk/{ino}/").as_bytes()].concat();
        let chunks = self.backend.scan_prefix(&chunk_prefix, 4096).await?;
        let mut ordered_chunks = Vec::with_capacity(chunks.len());
        for (key, value) in chunks {
            let suffix = std::str::from_utf8(&key[chunk_prefix.len()..])
                .map_err(|_| EntityError::Corrupt("invalid chunk index".into()))?;
            let index: u64 = suffix
                .parse()
                .map_err(|_| EntityError::Corrupt("invalid chunk index".into()))?;
            if key != self.keys.chunk(ino, index) || index < orphan.cleanup_chunk {
                return Err(EntityError::Corrupt(
                    "orphan cleanup cursor skipped a chunk".into(),
                ));
            }
            ordered_chunks.push((index, key, value));
        }
        ordered_chunks.sort_by_key(|(index, _, _)| *index);
        let mut plan = EntityPlan {
            checks: checked_rows(&keys, snapshot.rows),
            writes: Vec::new(),
        };
        if let Some((index, key, bytes)) = ordered_chunks.into_iter().next() {
            let old: ChunkMap = decode(&bytes)?;
            let empty = ChunkMap::holes(old.chunk_size)?;
            let membership =
                plan_chunk_membership(self.backend.as_ref(), &self.keys, ino, index, &old, &empty)
                    .await?;
            plan.checks.extend(membership.checks);
            plan.writes.extend(membership.writes);
            plan.checks.push(RowCheck {
                key: key.clone(),
                expected: Some(bytes),
            });
            plan.writes.push(RowWrite::Delete { key });
            orphan.cleanup_chunk = index.checked_add(1).ok_or(EntityError::Overflow)?;
            plan.writes.push(put(self.keys.orphan(ino), &orphan)?);
            committed(self.backend.commit(&authority, &plan).await?)?;
            return Ok(false);
        }
        let xattr_prefix = [self.keys.prefix(), format!("xattr/{ino}/").as_bytes()].concat();
        for (key, value) in self.backend.scan_prefix(&xattr_prefix, 4096).await? {
            plan.checks.push(RowCheck {
                key: key.clone(),
                expected: Some(value),
            });
            plan.writes.push(RowWrite::Delete { key });
        }
        for (key, value) in self
            .backend
            .scan_prefix(&self.keys.dentry_prefix(ino), 4096)
            .await?
        {
            if decode::<EntityDentry>(&value)? != EntityDentry::Whiteout {
                return Err(EntityError::NotEmpty);
            }
            plan.checks.push(RowCheck {
                key: key.clone(),
                expected: Some(value),
            });
            plan.writes.push(RowWrite::Delete { key });
        }
        plan.writes
            .push(put(self.keys.inode(ino), &UpperInode::Deleted)?);
        plan.writes.push(RowWrite::Delete {
            key: self.keys.orphan(ino),
        });
        committed(self.backend.commit(&authority, &plan).await?)?;
        Ok(true)
    }
}

/// Append this plan to the same transaction as the exact old/new chunk row.
/// The caller must hold its inode/chunk coordination and runtime authority.
/// Unchanged memberships are also validated; corrupted counts fail closed.
pub async fn plan_chunk_membership<B: EntityBackend>(
    backend: &B,
    keys: &EntityKeys,
    ino: i64,
    index: u64,
    old: &ChunkMap,
    new: &ChunkMap,
) -> Result<EntityPlan> {
    old.validate()?;
    new.validate()?;
    if ino <= 0 || old.chunk_size != new.chunk_size {
        return Err(EntityError::Invalid(
            "invalid chunk membership replacement".into(),
        ));
    }
    let old_slices = old.upper_slices()?;
    let new_slices = new.upper_slices()?;
    let ids = old_slices.union(&new_slices).copied().collect::<Vec<_>>();
    let row_keys = ids
        .iter()
        .flat_map(|id| [keys.slice(*id), keys.owner(*id, ino, index)])
        .collect::<Vec<_>>();
    if row_keys.is_empty() {
        return Ok(EntityPlan::default());
    }
    let snapshot = backend.read(&row_keys).await?;
    let mut writes = Vec::new();
    for (position, id) in ids.iter().copied().enumerate() {
        let mut row: SliceOwnership =
            decode(snapshot.rows[position * 2].as_deref().ok_or_else(|| {
                EntityError::Corrupt("chunk references an unregistered slice".into())
            })?)?;
        row.validate(keys, id)?;
        if row.state != SliceState::Available {
            return Err(EntityError::Busy);
        }
        let membership = SliceMembership {
            namespace_id: keys.namespace_id(),
            slice_id: id,
            ino,
            chunk_index: index,
        };
        let was_present = old_slices.contains(&id);
        let is_present = new_slices.contains(&id);
        match &snapshot.rows[position * 2 + 1] {
            Some(bytes) => {
                let actual: SliceMembership = decode(bytes)?;
                if !was_present || actual != membership || row.current_refs == 0 {
                    return Err(EntityError::Corrupt("inconsistent slice membership".into()));
                }
            }
            None if was_present => {
                return Err(EntityError::Corrupt("missing slice membership".into()));
            }
            None => {}
        }
        // Both maps must be valid: recovery must not hide an existing corrupt
        // object range merely because the replacement removes that range.
        for span in old.spans.iter().chain(&new.spans) {
            if let super::model::ChunkSource::Data {
                slice_id,
                slice_offset,
            } = span.source
                && slice_id == id
                && slice_offset
                    .checked_add(span.length)
                    .is_none_or(|end| end > row.size)
            {
                return Err(EntityError::Corrupt(
                    "chunk extends beyond immutable slice".into(),
                ));
            }
        }
        if was_present == is_present {
            continue;
        }
        if is_present {
            row.current_refs = row
                .current_refs
                .checked_add(1)
                .ok_or(EntityError::Overflow)?;
            row.staging_hold = false;
            writes.push(put(keys.owner(id, ino, index), &membership)?);
        } else {
            row.current_refs = row
                .current_refs
                .checked_sub(1)
                .ok_or_else(|| EntityError::Corrupt("slice refcount underflow".into()))?;
            row.retirement_hold = true;
            writes.push(RowWrite::Delete {
                key: keys.owner(id, ino, index),
            });
        }
        writes.push(put(keys.slice(id), &row)?);
    }
    Ok(EntityPlan {
        checks: checked_rows(&row_keys, snapshot.rows),
        writes,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrphanRecord {
    pub namespace_id: Uuid,
    pub ino: i64,
    pub lease: LeaseIdentity,
    pub cleanup_chunk: u64,
}

/// Combine with the last-unlink inode/dentry transaction. The edge exists even
/// when no local handle is open, so cancellation cannot lose cleanup work.
pub fn plan_orphan(
    keys: &EntityKeys,
    session: &WriterSession,
    ino: i64,
    current: Option<Vec<u8>>,
) -> Result<EntityPlan> {
    if ino <= 0 || !session.matches_keys(keys) {
        return Err(EntityError::Fenced);
    }
    let row = OrphanRecord {
        namespace_id: keys.namespace_id(),
        ino,
        lease: session.identity(),
        cleanup_chunk: 0,
    };
    let writes = match current.as_deref() {
        Some(bytes) => {
            let existing: OrphanRecord = decode(bytes)?;
            if existing.namespace_id != row.namespace_id
                || existing.ino != ino
                || existing.lease != row.lease
            {
                return Err(EntityError::Corrupt("foreign orphan record".into()));
            }
            Vec::new()
        }
        None => vec![put(keys.orphan(ino), &row)?],
    };
    Ok(EntityPlan {
        checks: vec![RowCheck {
            key: keys.orphan(ino),
            expected: current,
        }],
        writes,
    })
}

#[cfg(test)]
#[path = "ownership_tests.rs"]
mod tests;
