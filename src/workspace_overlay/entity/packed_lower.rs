//! Authenticated packed-v3 metadata/data adapter for the current-entity upper.
//!
//! A durable collector-visible owner retains the entire published lower graph,
//! including its borrowed native roots. This module grants no mount admission.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::model::{
    ChunkMap, EntityDentry, EntityInode, EntityKind, LowerBinding, validate_name,
    validate_xattr_name,
};
use super::resolver::{ImmutableLower, ImmutableLowerData, MAX_DIRECTORY_ENTRIES};
use super::{EntityError, Result};
use crate::cadapter::client::{ObjectBackend, ObjectClient};
use crate::workspace_overlay::error::WorkspaceError;
use crate::workspace_overlay::packed_v3::PackedWireError;
use crate::workspace_overlay::packed_v3::directory_key;
use crate::workspace_overlay::packed_v3::wire005::{
    AuthenticatedV3Snapshot, V3IndexReader, V3MountBudget,
};
use crate::workspace_overlay::publish::binding::VerifiedPackedLower;
use crate::workspace_overlay::stores::kv_backend::WorkspaceKvBackend;
use crate::workspace_overlay::stores::kv_store::KvWorkspaceStore;
use crate::workspace_overlay::stores::kv_store::packed_reader_pins::{
    PackedEntityLowerOwner, PackedReaderPin,
};

const ALLOCATION_LIMIT: usize = 32 << 20;
const DIRECTORY_PAGE: usize = 256;

pub struct PackedLower<O: ObjectBackend + Clone, K> {
    store: Arc<KvWorkspaceStore<K>>,
    owner: PackedEntityLowerOwner,
    binding: LowerBinding,
    client: ObjectClient<O>,
    snapshot: AuthenticatedV3Snapshot,
    reader: V3IndexReader<O>,
}

fn packed_error(error: PackedWireError) -> EntityError {
    match error {
        PackedWireError::Backend(message) => EntityError::Backend(message),
        other => EntityError::Corrupt(other.to_string()),
    }
}

fn store_error(error: WorkspaceError) -> EntityError {
    match error {
        WorkspaceError::Fenced => EntityError::Fenced,
        WorkspaceError::Busy => EntityError::Conflict,
        WorkspaceError::CorruptMetadata(message) => EntityError::Corrupt(message),
        other => EntityError::Backend(other.to_string()),
    }
}

fn kind(value: u8) -> Result<EntityKind> {
    Ok(match value {
        1 => EntityKind::File,
        2 => EntityKind::Directory,
        3 => EntityKind::Symlink,
        4 => EntityKind::Fifo,
        5 => EntityKind::Socket,
        6 => EntityKind::CharDevice,
        7 => EntityKind::BlockDevice,
        _ => return Err(EntityError::Corrupt("packed inode kind".into())),
    })
}

impl<O: ObjectBackend + Clone + 'static, K: WorkspaceKvBackend> PackedLower<O, K> {
    /// Authenticate the descriptor against the manifest and its exact maximum
    /// inode, then establish durable ownership before exposing any lower data.
    pub async fn open(
        store: Arc<KvWorkspaceStore<K>>,
        client: ObjectClient<O>,
        volume_id: Uuid,
        namespace_id: Uuid,
        source: &PackedReaderPin,
        binding: LowerBinding,
    ) -> Result<Self> {
        binding.validate()?;
        let reference = &source.binding.binding.manifest;
        if reference.key.as_bytes() != binding.manifest_key
            || reference.digest != binding.manifest_digest
        {
            return Err(EntityError::Fenced);
        }
        store
            .validate_packed_reader_pin(source)
            .await
            .map_err(store_error)?;
        let budget = store.resolve_packed_reader_pin_budget(V3MountBudget::defaults());
        let reader = V3IndexReader::with_budget(client.clone(), 4 << 20, budget);
        let snapshot = AuthenticatedV3Snapshot::open(&client, reference)
            .await
            .map_err(packed_error)?;
        let verified = VerifiedPackedLower::from_authenticated_snapshot(&snapshot, &reader)
            .await
            .map_err(store_error)?;
        if snapshot.manifest().root_inode != binding.root_ino as u64
            || verified.highest_inode() != binding.max_ino
            || source.binding.highest_inode != binding.max_ino
        {
            return Err(EntityError::Corrupt(
                "entity lower namespace boundary disagrees with manifest".into(),
            ));
        }
        let owner = store
            .retain_packed_entity_lower(volume_id, namespace_id, source)
            .await
            .map_err(store_error)?;
        Ok(Self {
            store,
            owner,
            binding,
            client,
            snapshot,
            reader,
        })
    }

    pub fn owner(&self) -> &PackedEntityLowerOwner {
        &self.owner
    }

    /// Reopen an existing durable owner without reviving its expired source
    /// reader lease. Namespace writer admission remains the caller's duty.
    pub async fn reopen(
        store: Arc<KvWorkspaceStore<K>>,
        client: ObjectClient<O>,
        volume_id: Uuid,
        namespace_id: Uuid,
        binding: LowerBinding,
    ) -> Result<Self> {
        binding.validate()?;
        // A fresh store after process restart has no in-memory pin budget yet.
        // Admit owner reads through the same budget the packed reader will use.
        let budget = store.resolve_packed_reader_pin_budget(V3MountBudget::defaults());
        let owner = store
            .load_packed_entity_lower(volume_id, namespace_id)
            .await
            .map_err(store_error)?;
        let reference = &owner.binding().binding.manifest;
        if reference.key.as_bytes() != binding.manifest_key
            || reference.digest != binding.manifest_digest
        {
            return Err(EntityError::Fenced);
        }
        let reader = V3IndexReader::with_budget(client.clone(), 4 << 20, budget);
        let snapshot = AuthenticatedV3Snapshot::open(&client, reference)
            .await
            .map_err(packed_error)?;
        let verified = VerifiedPackedLower::from_authenticated_snapshot(&snapshot, &reader)
            .await
            .map_err(store_error)?;
        if snapshot.manifest().root_inode != binding.root_ino as u64
            || verified.highest_inode() != binding.max_ino
            || owner.binding().highest_inode != binding.max_ino
        {
            return Err(EntityError::Corrupt(
                "entity lower namespace boundary disagrees with manifest".into(),
            ));
        }
        store
            .validate_packed_entity_lower(&owner)
            .await
            .map_err(store_error)?;
        Ok(Self {
            store,
            owner,
            binding,
            client,
            snapshot,
            reader,
        })
    }

    async fn validate_owner(&self) -> Result<()> {
        self.store
            .validate_packed_entity_lower(&self.owner)
            .await
            .map_err(store_error)
    }

    fn checked_ino(&self, ino: i64) -> Result<bool> {
        if ino <= 0 {
            return Err(EntityError::Invalid("invalid lower inode".into()));
        }
        Ok(ino <= self.binding.max_ino)
    }

    fn directory_key(&self, ino: i64) -> [u8; 32] {
        if ino == self.binding.root_ino {
            self.snapshot.manifest().root_dir_key
        } else {
            directory_key(self.snapshot.manifest().snapshot_id, ino as u64)
        }
    }

    async fn inode_inner(&self, ino: i64) -> Result<Option<EntityInode>> {
        if !self.checked_ino(ino)? {
            return Ok(None);
        }
        if ino == self.binding.root_ino {
            // PM07's synthetic root is a versioned reader contract. PM08+
            // authenticates the original root attributes in the manifest.
            let source = self
                .snapshot
                .manifest()
                .source
                .as_ref()
                .map(|source| &source.root);
            let row = EntityInode {
                ino,
                kind: EntityKind::Directory,
                size: source.map_or(0, |root| root.size),
                mode: source.map_or(0o755, |root| root.mode & 0o7777),
                uid: source.map_or(0, |root| root.uid),
                gid: source.map_or(0, |root| root.gid),
                rdev: 0,
                nlink: source.map_or(2, |root| root.nlink),
                atime_ns: source.map_or(0, |root| root.atime_ns),
                mtime_ns: source.map_or(0, |root| root.mtime_ns),
                ctime_ns: source.map_or(0, |root| root.ctime_ns),
                symlink_target: None,
                parent_hint: Some(ino),
                lower_visible_size: 0,
                data_version: 0,
            };
            row.validate()?;
            return Ok(Some(row));
        }
        let Some(location) = self
            .snapshot
            .lookup_inode(&self.reader, ino as u64)
            .await
            .map_err(packed_error)?
        else {
            return Ok(None);
        };
        let hot = location.hot;
        let kind = kind(hot.kind)?;
        let symlink_target = if kind == EntityKind::Symlink {
            Some(
                self.snapshot
                    .cold_attributes(&self.client, &self.reader, ino as u64)
                    .await
                    .map_err(packed_error)?
                    .and_then(|cold| cold.symlink_target)
                    .ok_or_else(|| EntityError::Corrupt("packed symlink has no target".into()))?,
            )
        } else {
            None
        };
        let row = EntityInode {
            ino,
            kind,
            size: hot.size,
            mode: hot.mode & 0o7777,
            uid: hot.uid,
            gid: hot.gid,
            rdev: u32::try_from(hot.rdev)
                .map_err(|_| EntityError::Corrupt("packed rdev exceeds entity format".into()))?,
            nlink: hot.nlink,
            atime_ns: hot.atime_ns,
            mtime_ns: hot.mtime_ns,
            ctime_ns: hot.ctime_ns,
            symlink_target,
            parent_hint: Some(
                i64::try_from(hot.parent_inode)
                    .map_err(|_| EntityError::Corrupt("packed parent inode overflow".into()))?,
            ),
            lower_visible_size: if kind == EntityKind::File {
                hot.size
            } else {
                0
            },
            data_version: 0,
        };
        row.validate()?;
        Ok(Some(row))
    }

    async fn require_directory(&self, ino: i64) -> Result<()> {
        let row = self.inode_inner(ino).await?.ok_or(EntityError::NotFound)?;
        if row.kind != EntityKind::Directory {
            return Err(EntityError::NotDirectory);
        }
        Ok(())
    }

    fn dentry_value(&self, ino: u64, packed_kind: u8) -> Result<EntityDentry> {
        let ino = i64::try_from(ino)
            .map_err(|_| EntityError::Corrupt("packed dentry inode overflow".into()))?;
        if !self.checked_ino(ino)? {
            return Err(EntityError::Corrupt(
                "packed dentry exceeds namespace".into(),
            ));
        }
        Ok(EntityDentry::Present {
            ino,
            kind: kind(packed_kind)?,
        })
    }
}

#[async_trait]
impl<O: ObjectBackend + Clone + 'static, K: WorkspaceKvBackend> ImmutableLower
    for PackedLower<O, K>
{
    fn binding(&self) -> &LowerBinding {
        &self.binding
    }

    async fn inode(&self, ino: i64) -> Result<Option<EntityInode>> {
        self.validate_owner().await?;
        let result = self.inode_inner(ino).await?;
        self.validate_owner().await?;
        Ok(result)
    }

    async fn dentry(&self, parent: i64, name: &[u8]) -> Result<Option<EntityDentry>> {
        validate_name(name)?;
        self.validate_owner().await?;
        self.require_directory(parent).await?;
        let entry = self
            .snapshot
            .lookup_dentry(
                &self.client,
                &self.reader,
                self.directory_key(parent),
                name,
                ALLOCATION_LIMIT,
            )
            .await
            .map_err(packed_error)?;
        let result = entry
            .map(|entry| self.dentry_value(entry.inode, entry.kind))
            .transpose()?;
        self.validate_owner().await?;
        Ok(result)
    }

    async fn xattr(&self, ino: i64, name: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_xattr_name(name)?;
        self.validate_owner().await?;
        self.inode_inner(ino).await?.ok_or(EntityError::NotFound)?;
        let cold = self
            .snapshot
            .cold_attributes(&self.client, &self.reader, ino as u64)
            .await
            .map_err(packed_error)?;
        let result = cold.and_then(|cold| {
            cold.xattrs
                .into_iter()
                .find(|attr| attr.name == name)
                .map(|attr| attr.value)
        });
        self.validate_owner().await?;
        Ok(result)
    }

    async fn chunk(&self, ino: i64, chunk_index: u64, chunk_size: u64) -> Result<Option<ChunkMap>> {
        self.validate_owner().await?;
        let row = self.inode_inner(ino).await?.ok_or(EntityError::NotFound)?;
        if row.kind != EntityKind::File {
            return Err(EntityError::IsDirectory);
        }
        let start = chunk_index
            .checked_mul(chunk_size)
            .ok_or(EntityError::Overflow)?;
        let map = ChunkMap::inherited(chunk_size, &self.binding, ino, start, row.size)?;
        self.validate_owner().await?;
        Ok(Some(map))
    }

    async fn readdir(&self, ino: i64) -> Result<Vec<(Vec<u8>, EntityDentry)>> {
        self.validate_owner().await?;
        self.require_directory(ino).await?;
        let mut result = Vec::new();
        let mut names = BTreeSet::new();
        loop {
            let limit = DIRECTORY_PAGE.min(MAX_DIRECTORY_ENTRIES + 1 - result.len());
            let entries = self
                .snapshot
                .readdir_page(
                    &self.client,
                    &self.reader,
                    self.directory_key(ino),
                    result.len() as u64,
                    limit,
                    ALLOCATION_LIMIT,
                )
                .await
                .map_err(packed_error)?;
            if entries.is_empty() {
                break;
            }
            if result.len() + entries.len() > MAX_DIRECTORY_ENTRIES {
                return Err(EntityError::Unsupported(
                    "packed directory exceeds entity entry bound",
                ));
            }
            for entry in entries {
                validate_name(&entry.name)?;
                if !names.insert(entry.name.clone()) {
                    return Err(EntityError::Corrupt(
                        "duplicate packed directory name".into(),
                    ));
                }
                let value = self.dentry_value(entry.inode, entry.kind)?;
                result.push((entry.name, value));
            }
        }
        self.validate_owner().await?;
        Ok(result)
    }
}

#[async_trait]
impl<O: ObjectBackend + Clone + 'static, K: WorkspaceKvBackend> ImmutableLowerData
    for PackedLower<O, K>
{
    async fn read_range(&self, ino: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        if length > ALLOCATION_LIMIT {
            return Err(EntityError::Invalid(
                "packed lower read exceeds allocation bound".into(),
            ));
        }
        self.validate_owner().await?;
        let row = self.inode_inner(ino).await?.ok_or(EntityError::NotFound)?;
        if row.kind != EntityKind::File {
            return Err(EntityError::IsDirectory);
        }
        let end = offset
            .checked_add(length as u64)
            .ok_or(EntityError::Overflow)?;
        if end > row.size {
            return Err(EntityError::Invalid(
                "packed lower read exceeds immutable EOF".into(),
            ));
        }
        let mut output = vec![0; length];
        self.snapshot
            .read_inode_range(
                &self.client,
                &self.reader,
                ino as u64,
                offset,
                &mut output,
                ALLOCATION_LIMIT,
            )
            .await
            .map_err(packed_error)?;
        self.validate_owner().await?;
        Ok(output)
    }
}
