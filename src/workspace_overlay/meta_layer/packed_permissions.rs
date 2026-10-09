//! Merge only native absence with the actual authenticated immutable lower.

use super::*;
use crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner;
use crate::workspace_overlay::packed_v3::wire005::{V3BudgetPool, V3OwnedPermit};

const OPERATION_BYTES: u64 = 16 << 20;

/// Keep copy-up templates and captured reader generation owned through the
/// caller's policy calculation and its actual conditional backend response.
pub(super) struct MutationVersion {
    layers: [LayerRecord; 2],
    _permit: Option<V3OwnedPermit>,
    _reader: Option<PackedReaderRequestOwner>,
}
impl std::ops::Deref for MutationVersion {
    type Target = [LayerRecord; 2];
    fn deref(&self) -> &Self::Target {
        &self.layers
    }
}

impl<W: WorkspaceStore + 'static> WorkspaceMetaLayer<W> {
    pub(super) async fn retain_mutation_version(&self) -> Result<MutationVersion, MetaError> {
        let (permit, reader) = if let Some(lower) = self.packed_lower() {
            if !self.store.supports_packed_permissions() {
                return Err(MetaError::NotSupported(
                    "conditional packed permission store".into(),
                ));
            }
            let permit = lower
                .budget
                .admit(&[(V3BudgetPool::Metadata, OPERATION_BYTES)])
                .map_err(packed_lower::budget_to_meta)?;
            let reader = lower
                .authority
                .retain_reader_request()
                .map_err(workspace_to_meta)?;
            (Some(permit), reader)
        } else {
            (None, None)
        };
        let layers = self
            .permission_snapshot(vec![self.root_ino()], None)
            .await?
            .layers;
        Ok(MutationVersion {
            layers,
            _permit: permit,
            _reader: reader,
        })
    }

    pub(super) async fn merged_packed_permission_snapshot(
        &self,
        lower: &WorkspacePackedLower,
        inodes: Vec<i64>,
        dentry: Option<(i64, Vec<u8>)>,
    ) -> Result<PermissionSnapshot, MetaError> {
        let _permit = lower
            .budget
            .admit(&[(V3BudgetPool::Metadata, OPERATION_BYTES)])
            .map_err(packed_lower::budget_to_meta)?;
        let _reader = lower
            .authority
            .retain_reader_request()
            .map_err(workspace_to_meta)?;
        let guard = self.guard().await;
        let chain = self.chain().await?;
        validate_fixed_layer_pair(&chain).map_err(workspace_to_meta)?;
        let query = PermissionSnapshotQuery {
            layer_ids: [guard.expected_head_layer_id, lower.binding.base_layer_id],
            inodes: inodes.clone(),
            dentry,
        };
        if [chain[0].layer_id, chain[1].layer_id] != query.layer_ids {
            return Err(workspace_to_meta(WorkspaceError::Fenced));
        }
        if let Some(reader) = lower.authority.reader_session() {
            if reader.binding() != &lower.binding {
                return Err(workspace_to_meta(WorkspaceError::Fenced));
            }
            reader.validate().await.map_err(workspace_to_meta)?;
        }
        let before = self
            .store
            .read_packed_permission_snapshot(guard.clone(), lower.binding.clone(), query.clone())
            .await
            .map_err(workspace_to_meta)?;
        let mut merged = before.clone();
        for ino in inodes {
            let state = resolve_inode_state(&before.layers, &before.inodes, ino)
                .map_err(workspace_to_meta)?;
            if matches!(state, Resolution::Masked) {
                continue;
            }
            let needs_inode = matches!(state, Resolution::Absent);
            let hot = if needs_inode {
                lower.metadata.frozen_inode_metadata_owned(ino).await?
            } else {
                None
            };
            if needs_inode && hot.is_none() {
                continue;
            }
            let mut needs_cold = needs_inode;
            for name in [ACCESS_XATTR, DEFAULT_XATTR, b"system.brewfs.acl".as_slice()] {
                needs_cold |= matches!(
                    resolve_xattr_state(&before.layers, &before.xattrs, ino, name)
                        .map_err(workspace_to_meta)?,
                    Resolution::Absent
                );
            }
            let cold = if needs_cold {
                lower.metadata.frozen_cold_attributes_owned(ino).await?
            } else {
                None
            };
            if let Some(hot) = hot {
                let attr = &hot.attr;
                if attr.ino != ino {
                    return Err(MetaError::Internal("packed copy-up inode identity".into()));
                }
                let target = cold.as_ref().and_then(|value| value.symlink_target.clone());
                if attr.kind == FileType::Symlink
                    && target
                        .as_ref()
                        .is_none_or(|value| value.len() as u64 != attr.size)
                {
                    return Err(MetaError::Internal(
                        "packed copy-up symlink target/size".into(),
                    ));
                }
                merged.inodes.push(InodeDelta {
                    layer_id: lower.binding.base_layer_id,
                    ino,
                    state: InodeState::Present,
                    kind: file_type_code(attr.kind),
                    size: attr.size,
                    mode: attr.mode,
                    uid: attr.uid,
                    gid: attr.gid,
                    rdev: attr.rdev,
                    nlink: attr.nlink,
                    atime_ns: attr.atime,
                    mtime_ns: attr.mtime,
                    ctime_ns: attr.ctime,
                    symlink_target: target,
                    parent_hint: hot.parent_hint,
                    // The immutable packed format has no native data_version.
                    // Zero is the initial overlay version; current binding and
                    // head sequence independently fence every read generation.
                    data_version: 0,
                    sequence: 0,
                });
            }
            if let Some(cold) = cold {
                for name in [ACCESS_XATTR, DEFAULT_XATTR, b"system.brewfs.acl".as_slice()] {
                    match resolve_xattr_state(&before.layers, &before.xattrs, ino, name)
                        .map_err(workspace_to_meta)?
                    {
                        Resolution::Present(_) | Resolution::Masked => {}
                        Resolution::Absent => {
                            if let Some(value) = cold.xattrs.iter().find(|value| value.name == name)
                            {
                                merged.xattrs.push(XattrDelta {
                                    layer_id: lower.binding.base_layer_id,
                                    ino,
                                    name: name.to_vec(),
                                    op: ValueOp::Put,
                                    value: Some(value.value.clone()),
                                    sequence: 0,
                                });
                            }
                        }
                    }
                }
            }
        }
        // Re-read the same native rows/PWB/guard after actual authenticated
        // lower I/O. Every later mutation checks this unchanged layer version
        // and current PWB again in its actual timed CAS.
        let after = self
            .store
            .read_packed_permission_snapshot(guard, lower.binding.clone(), query)
            .await
            .map_err(workspace_to_meta)?;
        if after != before {
            return Err(workspace_to_meta(WorkspaceError::Busy));
        }
        if let Some(reader) = lower.authority.reader_session() {
            reader.validate().await.map_err(workspace_to_meta)?;
        }
        Ok(merged)
    }

    pub(super) async fn commit_versioned_mutation(
        &self,
        request: VersionedMutation,
    ) -> Result<super::super::catalog::MutationResult, WorkspaceError> {
        if let Some(lower) = self.packed_lower() {
            self.store
                .apply_packed_versioned_mutation(request, lower.binding.clone())
                .await
        } else {
            self.store.apply_versioned_mutation(request).await
        }
    }
}
