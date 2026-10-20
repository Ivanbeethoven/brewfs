//! Durable upper-object ownership, independent of inode link counts.
//!
//! Chunk membership is counted once per (namespace, inode, chunk, slice).
//! Removing a membership creates a retirement hold even when other current
//! memberships remain. Only a drained namespace may clear that reader hold.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::backend::{
    EntityBackend, EntityPlan, RowCheck, RowWrite, SliceAllocation, TransactionAuthority,
};
use super::codec::{decode, encode};
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
    pub(super) fn validate(&self, keys: &EntityKeys, slice_id: u64) -> Result<()> {
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
    pending_key: Vec<u8>,
    pending_bytes: Vec<u8>,
    _admission: Arc<OperationGuard>,
}

/// Local evidence that the storage adapter acknowledged the durable PUT. The
/// durable row stays Pending until its first map publication consumes this token.
pub struct DurableUpload {
    token: UploadToken,
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
    /// Call only after a successful durable create-only object PUT.
    pub(super) fn confirm_durable_put(self) -> DurableUpload {
        DurableUpload { token: self }
    }
}
impl DurableUpload {
    pub fn slice_id(&self) -> u64 {
        self.token.slice_id()
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

    /// Reuse the operation's authority; this performs exactly one backend call
    /// and never starts transport after an ambiguous reservation response.
    pub(super) async fn reserve_uploads(
        &self,
        session: &WriterSession,
        authority: &TransactionAuthority,
        sizes: &[u64],
    ) -> Result<Vec<UploadToken>> {
        let _diagnostic = super::diagnostics::Span::start("ownership.reserve_uploads");
        let admission = Arc::new(session.admit()?);
        if !session.matches_keys(&self.keys) {
            return Err(EntityError::Fenced);
        }
        // Supplied authority must be the same session's complete lower/lease
        // snapshot. This is a local structural check, never another read.
        let authority_keys = self.lifecycle.authority_keys(session)?;
        let authority_rows = authority_keys
            .iter()
            .map(|key| {
                authority
                    .checks
                    .iter()
                    .find(|check| &check.key == key)
                    .map(|check| check.expected.clone())
                    .ok_or(EntityError::Fenced)
            })
            .collect::<Result<Vec<_>>>()?;
        let checked = self.lifecycle.authority_from_snapshot(
            session,
            super::backend::RowSnapshot {
                now_ns: 0,
                rows: authority_rows,
            },
        )?;
        if checked.expires_before_ns != authority.expires_before_ns {
            return Err(EntityError::Fenced);
        }
        let allocation = SliceAllocation {
            keys: self.keys.clone(),
            sizes: sizes.to_vec(),
            upload_id: Uuid::new_v4(),
            lease: session.identity(),
        };
        let ids = self
            .backend
            .reserve_slices(authority, &allocation)
            .await?
            .ok_or(EntityError::Conflict)?;
        if ids.len() != sizes.len() {
            return Err(EntityError::Corrupt("short upload reservation".into()));
        }
        ids.into_iter()
            .enumerate()
            .map(|(position, slice_id)| {
                let row = allocation.pending(slice_id, position);
                row.validate(&self.keys, slice_id)?;
                Ok(UploadToken {
                    namespace_id: self.keys.namespace_id(),
                    namespace_prefix: self.keys.prefix().to_vec(),
                    slice_id,
                    upload_id: allocation.upload_id,
                    lease: allocation.lease,
                    pending_key: self.keys.slice(slice_id),
                    pending_bytes: encode(&row)?,
                    _admission: admission.clone(),
                })
            })
            .collect()
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

    /// Validate the exact namespace and durable reservation before transport
    /// receives an object DELETE. Validation after DELETE would be too late to
    /// reject a token from another volume/namespace. Deleted IDs stay reserved,
    /// making an uncertain physical DELETE safely repeatable.
    pub async fn validate_delete(
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
        let row: SliceOwnership =
            decode(snapshot.rows[0].as_deref().ok_or(EntityError::NotFound)?)?;
        row.validate(&self.keys, token.slice_id)?;
        if !matches!(row.state, SliceState::Deleting { reservation } | SliceState::Deleted { reservation }
            if reservation == token.reservation)
        {
            return Err(EntityError::Fenced);
        }
        committed(
            self.backend
                .commit(
                    &authority,
                    &EntityPlan {
                        checks: checked_rows(&keys, snapshot.rows),
                        writes: Vec::new(),
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
        let keys = vec![
            self.keys.orphan(ino),
            self.keys.inode(ino),
            self.keys.truncate(ino),
        ];
        let snapshot = self.backend.read(&keys).await?;
        if snapshot.rows[2].is_some() {
            return Err(EntityError::Busy);
        }
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
    let row_keys = chunk_membership_keys(keys, ino, index, old, new)?;
    if row_keys.is_empty() {
        return Ok(EntityPlan::default());
    }
    let snapshot = backend.read(&row_keys).await?;
    if snapshot.rows.len() != row_keys.len() {
        return Err(EntityError::Corrupt("short ownership snapshot".into()));
    }
    let rows = row_keys.into_iter().zip(snapshot.rows).collect();
    plan_chunk_membership_from_rows(keys, ino, index, old, new, &rows, None)
}

pub fn chunk_membership_keys(
    keys: &EntityKeys,
    ino: i64,
    index: u64,
    old: &ChunkMap,
    new: &ChunkMap,
) -> Result<Vec<Vec<u8>>> {
    old.validate()?;
    new.validate()?;
    if ino <= 0 || old.chunk_size != new.chunk_size {
        return Err(EntityError::Invalid(
            "invalid chunk membership replacement".into(),
        ));
    }
    let old_slices = old.upper_slices()?;
    let new_slices = new.upper_slices()?;
    Ok(old_slices
        .union(&new_slices)
        .flat_map(|id| [keys.slice(*id), keys.owner(*id, ino, index)])
        .collect())
}

/// Pure planning over an operation's exact observations. A durable PUT receipt
/// supplies the already-known Pending bytes and absent new membership predicate;
/// the final transaction checks both before making the object visible.
pub fn plan_chunk_membership_from_rows(
    keys: &EntityKeys,
    ino: i64,
    index: u64,
    old: &ChunkMap,
    new: &ChunkMap,
    rows: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    upload: Option<&DurableUpload>,
) -> Result<EntityPlan> {
    plan_chunk_memberships_from_rows(
        keys,
        ino,
        &[(index, old.clone(), new.clone())],
        rows,
        upload.map_or(&[], std::slice::from_ref),
    )
}

/// Aggregate ownership once per slice across all replaced chunks. This avoids
/// overwriting independent refcount deltas when one object spans several maps.
pub fn plan_chunk_memberships_from_rows(
    keys: &EntityKeys,
    ino: i64,
    replacements: &[(u64, ChunkMap, ChunkMap)],
    rows: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    uploads: &[DurableUpload],
) -> Result<EntityPlan> {
    if ino <= 0 || replacements.len() > 64 || uploads.len() > 64 {
        return Err(EntityError::Invalid("invalid membership batch".into()));
    }
    let mut receipts = BTreeMap::new();
    for upload in uploads {
        let token = &upload.token;
        if token.namespace_id != keys.namespace_id()
            || token.namespace_prefix != keys.prefix()
            || token.pending_key != keys.slice(token.slice_id)
            || receipts.insert(token.slice_id, token).is_some()
        {
            return Err(EntityError::Invalid(
                "foreign or duplicate upload receipt".into(),
            ));
        }
        let pending: SliceOwnership = decode(&token.pending_bytes)?;
        pending.validate(keys, token.slice_id)?;
        if pending.state
            != (SliceState::Pending {
                upload_id: token.upload_id,
                lease: token.lease,
            })
            || pending.retirement_hold
            || pending.publication_refs != 0
        {
            return Err(EntityError::Corrupt(
                "invalid durable upload receipt".into(),
            ));
        }
    }
    struct Delta {
        row: SliceOwnership,
        old_memberships: u64,
        additions: u64,
        removals: u64,
        promote: bool,
    }
    let mut slices = BTreeMap::<u64, Delta>::new();
    let mut indices = BTreeSet::new();
    let mut checks = BTreeMap::new();
    let mut writes = Vec::new();
    for (index, old, new) in replacements {
        if !indices.insert(*index) {
            return Err(EntityError::Invalid("duplicate chunk replacement".into()));
        }
        chunk_membership_keys(keys, ino, *index, old, new)?;
        let old_slices = old.upper_slices()?;
        let new_slices = new.upper_slices()?;
        for id in old_slices.union(&new_slices).copied() {
            let slice_key = keys.slice(id);
            if let std::collections::btree_map::Entry::Vacant(entry) = slices.entry(id) {
                let expected = if let Some(token) = receipts.get(&id) {
                    let expected = Some(token.pending_bytes.clone());
                    if rows
                        .get(&slice_key)
                        .is_some_and(|actual| actual != &expected)
                    {
                        return Err(EntityError::Conflict);
                    }
                    expected
                } else {
                    rows.get(&slice_key)
                        .cloned()
                        .ok_or_else(|| EntityError::Corrupt("missing slice observation".into()))?
                };
                let row: SliceOwnership = decode(expected.as_deref().ok_or_else(|| {
                    EntityError::Corrupt("chunk references an unregistered slice".into())
                })?)?;
                row.validate(keys, id)?;
                let promote = receipts.contains_key(&id);
                if !promote && row.state != SliceState::Available {
                    return Err(EntityError::Busy);
                }
                checks.insert(slice_key, expected);
                entry.insert(Delta {
                    row,
                    old_memberships: 0,
                    additions: 0,
                    removals: 0,
                    promote,
                });
            }
            let delta = slices.get_mut(&id).expect("inserted slice delta");
            let membership = SliceMembership {
                namespace_id: keys.namespace_id(),
                slice_id: id,
                ino,
                chunk_index: *index,
            };
            let was_present = old_slices.contains(&id);
            let is_present = new_slices.contains(&id);
            let owner_key = keys.owner(id, ino, *index);
            let owner = if delta.promote {
                if was_present {
                    return Err(EntityError::Corrupt("visible Pending slice".into()));
                }
                if rows.get(&owner_key).is_some_and(Option::is_some) {
                    return Err(EntityError::Conflict);
                }
                None
            } else {
                rows.get(&owner_key)
                    .cloned()
                    .ok_or_else(|| EntityError::Corrupt("missing membership observation".into()))?
            };
            match owner.as_deref() {
                Some(bytes) => {
                    let actual: SliceMembership = decode(bytes)?;
                    if !was_present || actual != membership || delta.row.current_refs == 0 {
                        return Err(EntityError::Corrupt("inconsistent slice membership".into()));
                    }
                }
                None if was_present => {
                    return Err(EntityError::Corrupt("missing slice membership".into()));
                }
                None => {}
            }
            checks.insert(owner_key.clone(), owner);
            // Validate removed spans too: replacing corrupt bytes cannot hide an
            // invalid object range from recovery or normal publication.
            for span in old.spans.iter().chain(&new.spans) {
                if let super::model::ChunkSource::Data {
                    slice_id,
                    slice_offset,
                } = span.source
                    && slice_id == id
                    && slice_offset
                        .checked_add(span.length)
                        .is_none_or(|end| end > delta.row.size)
                {
                    return Err(EntityError::Corrupt(
                        "chunk extends beyond immutable slice".into(),
                    ));
                }
            }
            delta.old_memberships += u64::from(was_present);
            if was_present == is_present {
                continue;
            }
            if is_present {
                delta.additions += 1;
                writes.push(put(owner_key, &membership)?);
            } else {
                delta.removals += 1;
                writes.push(RowWrite::Delete { key: owner_key });
            }
        }
    }
    if receipts.keys().any(|id| !slices.contains_key(id)) {
        return Err(EntityError::Invalid("unused upload receipt".into()));
    }
    for (id, delta) in slices {
        let mut row = delta.row;
        if row.current_refs < delta.old_memberships {
            return Err(EntityError::Corrupt(
                "slice refcount undercounts memberships".into(),
            ));
        }
        row.current_refs = row
            .current_refs
            .checked_sub(delta.removals)
            .ok_or_else(|| EntityError::Corrupt("slice refcount underflow".into()))?
            .checked_add(delta.additions)
            .ok_or(EntityError::Overflow)?;
        if delta.additions > 0 {
            row.staging_hold = false;
        }
        if delta.removals > 0 {
            row.retirement_hold = true;
        }
        if delta.promote {
            row.state = SliceState::Available;
        }
        if delta.promote || delta.additions > 0 || delta.removals > 0 {
            writes.push(put(keys.slice(id), &row)?);
        }
    }
    if checks.len() + writes.len() > super::backend::MAX_TRANSACTION_ROWS {
        return Err(EntityError::Invalid(
            "membership batch exceeds row bound".into(),
        ));
    }
    let mut bytes = 0;
    for (key, value) in &checks {
        super::backend::add_bytes(&mut bytes, key, value.as_deref())?;
    }
    for write in &writes {
        let value = match write {
            RowWrite::Put { value, .. } => Some(value.as_slice()),
            RowWrite::Delete { .. } => None,
        };
        super::backend::add_bytes(&mut bytes, write.key(), value)?;
    }
    Ok(EntityPlan {
        checks: checks
            .into_iter()
            .map(|(key, expected)| RowCheck { key, expected })
            .collect(),
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
