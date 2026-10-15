//! Merge only native absence with the actual authenticated immutable lower.

use super::*;
use crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner;
use crate::workspace_overlay::packed_v3::wire005::{V3BudgetPool, V3OwnedPermit};

const OPERATION_BYTES: u64 = 16 << 20;
// A frozen-base snapshot owns at most two inodes x two layers of rows plus
// three permission xattrs each (bounded at 96 KiB/value by the store) and
// the merged copies. Lower hot/cold reads admit their own charges.
const FROZEN_SNAPSHOT_BYTES: u64 = 2 << 20;

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
            if !lower.strict() {
                // Frozen base: the mutation's own CAS re-authenticates the
                // binding, head and lease against these exact layers, so only
                // the current layer pair (fresh head, immutable base) is needed.
                // The commit admits its own bounded row/packet budget.
                let permit = lower
                    .budget
                    .admit(&[(V3BudgetPool::Metadata, FROZEN_SNAPSHOT_BYTES)])
                    .map_err(packed_lower::budget_to_meta)?;
                let layers = self.frozen_layer_pair(lower).await?;
                return Ok(MutationVersion {
                    layers,
                    _permit: Some(permit),
                    _reader: None,
                });
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

    /// Fresh writable head plus the mount's immutable sealed base.
    pub(super) async fn frozen_layer_pair(
        &self,
        lower: &WorkspacePackedLower,
    ) -> Result<[LayerRecord; 2], MetaError> {
        let guard = self.guard().await;
        let chain = self.chain().await?;
        // The head this mount just committed is the current head unless
        // something else wrote since, in which case the mutation CAS fails
        // with Busy and the caller's retry takes the fresh-read path.
        let head = match self.store.take_committed_head(guard.expected_head_layer_id) {
            Some(head) => head,
            None => self
                .store
                .load_layer(guard.expected_head_layer_id)
                .await
                .map_err(workspace_to_meta)?,
        };
        let layers = [head, chain[1].clone()];
        if layers[1].layer_id != lower.binding.base_layer_id {
            return Err(workspace_to_meta(WorkspaceError::Fenced));
        }
        validate_fixed_layer_pair(&layers).map_err(workspace_to_meta)?;
        Ok(layers)
    }

    pub(super) async fn merged_packed_permission_snapshot(
        &self,
        lower: &WorkspacePackedLower,
        inodes: Vec<i64>,
        dentry: Option<(i64, Vec<u8>)>,
    ) -> Result<PermissionSnapshot, MetaError> {
        let strict = lower.strict();
        let _permit = lower
            .budget
            .admit(&[(
                V3BudgetPool::Metadata,
                if strict {
                    OPERATION_BYTES
                } else {
                    FROZEN_SNAPSHOT_BYTES
                },
            )])
            .map_err(packed_lower::budget_to_meta)?;
        let _reader = if strict {
            lower
                .authority
                .retain_reader_request()
                .map_err(workspace_to_meta)?
        } else {
            None
        };
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
            if strict {
                reader.validate().await.map_err(workspace_to_meta)?;
            }
        }
        let before = if strict {
            self.store
                .read_packed_permission_snapshot(
                    guard.clone(),
                    lower.binding.clone(),
                    query.clone(),
                )
                .await
                .map_err(workspace_to_meta)?
        } else {
            // Frozen base: one atomic native row read. Every mutation built on
            // these layers re-authenticates binding, head and lease in its CAS.
            self.store
                .read_permission_snapshot(query.clone())
                .await
                .map_err(workspace_to_meta)?
        };
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
        if !strict {
            return Ok(merged);
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
        // Before and after: an uncertain/failed commit may still have
        // applied, and no concurrent read may cache pre-commit rows. Only
        // the keys this commit writes are dropped.
        let written = super::RowsWritten::of(&request);
        self.invalidate_rows_written(&written);
        let result = if let Some(lower) = self.packed_lower() {
            self.store
                .apply_packed_versioned_mutation(request, lower.binding.clone())
                .await
        } else {
            self.store.apply_versioned_mutation(request).await
        };
        self.invalidate_rows_written(&written);
        result
    }
}
