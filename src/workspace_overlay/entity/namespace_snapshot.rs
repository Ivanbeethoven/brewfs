//! Per-operation observed rows. Advisory topology chooses a bounded read set;
//! exact observations and lifecycle authority still authorize the final commit.

use super::*;
use crate::workspace_overlay::entity::backend::{
    MAX_TRANSACTION_ROWS, RowSnapshot, add_bytes, validate_keys,
};

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> EntityNamespace<B, L> {
    pub(in crate::workspace_overlay::entity) async fn snapshot(
        &self,
        keys: Vec<Vec<u8>>,
    ) -> Result<(TransactionAuthority, Prepared<'_, B, L>)> {
        let _diagnostic = super::super::diagnostics::Span::start("namespace.snapshot");
        if !self.session.matches_keys(&self.keys)
            || self.session.lower_binding() != self.lower.binding()
        {
            return Err(EntityError::Fenced);
        }
        let authority_keys = self.lifecycle.authority_keys(&self.session)?;
        let mut read_keys = authority_keys.clone();
        read_keys.extend(
            keys.into_iter()
                .filter(|key| !authority_keys.contains(key))
                .collect::<BTreeSet<_>>(),
        );
        let mut snapshot = self.backend.read(&read_keys).await?;
        if snapshot.rows.len() != read_keys.len() {
            return Err(EntityError::Corrupt("short operation snapshot".into()));
        }
        let rows = snapshot.rows.split_off(authority_keys.len());
        let now = snapshot.now_ns;
        let authority = self.lifecycle.authority_from_snapshot(
            &self.session,
            RowSnapshot {
                now_ns: now,
                rows: snapshot.rows,
            },
        )?;
        let mut prepared = Prepared::new(self);
        prepared.now = now;
        for (key, expected) in read_keys.into_iter().skip(authority_keys.len()).zip(rows) {
            prepared.add(RowCheck { key, expected })?;
        }
        Ok((authority, prepared))
    }

    /// Hints only nominate rows to fetch. The actual parent walk resolves the
    /// observed rows and fetches any newly discovered ancestors before commit.
    pub(super) async fn parent_keys(
        &self,
        parent: i64,
        keys: &mut BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        let mut current = parent;
        let mut seen = BTreeSet::new();
        while current > 0 && seen.insert(current) && seen.len() <= MAX_ANCESTORS {
            keys.insert(self.keys.inode(current));
            keys.insert(self.keys.xattr(current, ACCESS_XATTR));
            if current == self.lower.binding().root_ino {
                break;
            }
            if let Some(parent) = self.directory_hints.get(&current) {
                current = parent;
            } else if current <= self.lower.binding().max_ino {
                match self.lower.inode(current).await? {
                    Some(inode) if inode.kind == EntityKind::Directory => {
                        if let Some(parent) = inode.parent_hint {
                            self.directory_hints.insert(current, parent);
                            current = parent;
                        } else {
                            break;
                        }
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    pub(super) async fn path_keys(&self, parent: i64, name: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut keys = BTreeSet::from([self.keys.dentry(parent, name)]);
        if let Some(ino) = self.dentry_hints.get(&(parent, name.to_vec())) {
            keys.insert(self.keys.inode(ino));
            keys.insert(self.keys.xattr(ino, ACCESS_XATTR));
        }
        self.parent_keys(parent, &mut keys).await?;
        Ok(keys.into_iter().collect())
    }

    pub(super) async fn operation_keys(
        &self,
        operation: &Operation,
        ids: &BTreeSet<i64>,
    ) -> Result<Vec<Vec<u8>>> {
        let mut keys = BTreeSet::new();
        for ino in ids {
            keys.extend([
                self.keys.inode(*ino),
                self.keys.xattr(*ino, ACCESS_XATTR),
                self.keys.truncate(*ino),
            ]);
            if matches!(
                operation,
                Operation::Unlink { .. } | Operation::Rename { .. }
            ) {
                keys.insert(self.keys.orphan(*ino));
            }
        }
        match operation {
            Operation::Link { parent, name, .. } => {
                self.parent_keys(*parent, &mut keys).await?;
                keys.insert(self.keys.dentry(*parent, name));
            }
            Operation::Unlink { parent, .. } => {
                self.parent_keys(*parent, &mut keys).await?;
            }
            Operation::Rename {
                parent,
                destination,
                ..
            } => {
                self.parent_keys(*parent, &mut keys).await?;
                self.parent_keys(*destination, &mut keys).await?;
            }
            Operation::Xattr { ino, name, .. } => {
                keys.insert(self.keys.xattr(*ino, name));
            }
            Operation::Chmod { .. } => {}
        }
        Ok(keys.into_iter().collect())
    }
}

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> Prepared<'_, B, L> {
    pub(in crate::workspace_overlay::entity) fn now_ns(&self) -> i64 {
        self.now
    }

    pub(in crate::workspace_overlay::entity) fn observed(
        &self,
        key: &[u8],
    ) -> Option<&Option<Vec<u8>>> {
        self.rows.get(key)
    }

    pub(in crate::workspace_overlay::entity) fn observations(
        &self,
    ) -> &BTreeMap<Vec<u8>, Option<Vec<u8>>> {
        &self.rows
    }

    /// Never replace an observation: a plan assembled from incompatible views
    /// is a conflict, including when one of those observations was absence.
    pub(in crate::workspace_overlay::entity) fn add(&mut self, check: RowCheck) -> Result<()> {
        if let Some(previous) = self.rows.get(&check.key) {
            return if previous == &check.expected {
                Ok(())
            } else {
                Err(EntityError::Conflict)
            };
        }
        if self.rows.len() >= MAX_TRANSACTION_ROWS {
            return Err(EntityError::Invalid(
                "operation read set exceeds row bound".into(),
            ));
        }
        let mut bytes = self.bytes;
        add_bytes(&mut bytes, &check.key, check.expected.as_deref())?;
        self.bytes = bytes;
        self.rows.insert(check.key, check.expected);
        Ok(())
    }

    pub(in crate::workspace_overlay::entity) async fn prefetch(
        &mut self,
        keys: Vec<Vec<u8>>,
    ) -> Result<()> {
        let keys: Vec<_> = keys
            .into_iter()
            .filter(|key| !self.rows.contains_key(key) && !self.writes.contains_key(key))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        validate_keys(&keys)?;
        if self.rows.len().saturating_add(keys.len()) > MAX_TRANSACTION_ROWS {
            return Err(EntityError::Invalid(
                "operation read set exceeds row bound".into(),
            ));
        }
        let snapshot = self.ns.backend.read(&keys).await?;
        if snapshot.rows.len() != keys.len() || snapshot.now_ns < 0 {
            return Err(EntityError::Corrupt("invalid operation snapshot".into()));
        }
        self.now = snapshot.now_ns;
        for (key, expected) in keys.into_iter().zip(snapshot.rows) {
            self.add(RowCheck { key, expected })?;
        }
        Ok(())
    }

    pub(in crate::workspace_overlay::entity) fn plan_with_writes(
        &self,
        writes: Vec<RowWrite>,
    ) -> EntityPlan {
        let mut plan = self.plan();
        plan.writes.extend(writes);
        plan
    }

    pub(in crate::workspace_overlay::entity) async fn chunk(
        &mut self,
        inode: &EntityInode,
        index: u64,
        chunk_size: u64,
    ) -> Result<ChunkMap> {
        let row = self.raw(self.ns.keys.chunk(inode.ino, index)).await?;
        let upper: Option<ChunkMap> = row.as_deref().map(codec::decode).transpose()?;
        resolver::resolve_chunk(
            self.ns.lower.as_ref(),
            self.ns.lower.binding(),
            inode,
            index,
            chunk_size,
            upper.as_ref(),
        )
        .await
    }
}
