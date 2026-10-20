//! Inode operations used by the explicit experimental FUSE adapter.
//!
//! Kernel lookup performs ancestor search checks. Inode opens still check the
//! current file ACL; existing open handles retain their granted access.

use super::*;

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> EntityNamespace<B, L> {
    pub async fn getattr(&self, ino: i64, _credentials: &Credentials) -> Result<EntityInode> {
        let _admission = self.session.admit()?;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let result = p.inode(ino).await;
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                return result;
            }
        }
        Err(EntityError::Busy)
    }

    pub async fn access(&self, ino: i64, mask: u32, credentials: &Credentials) -> Result<()> {
        if mask & !7 != 0 {
            return Err(EntityError::Invalid("invalid access mask".into()));
        }
        let _admission = self.session.admit()?;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let result = async {
                let inode = p.inode(ino).await?;
                p.access(&inode, credentials, mask).await
            }
            .await;
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                return result;
            }
        }
        Err(EntityError::Busy)
    }

    /// An inode open follows a kernel-authorized lookup. No atime is written.
    pub async fn open_inode(
        &self,
        ino: i64,
        access: u32,
        credentials: &Credentials,
    ) -> Result<EntityHandle> {
        if access == 0 || access & !7 != 0 {
            return Err(EntityError::Invalid("invalid open access mask".into()));
        }
        let admission = self.session.admit()?;
        let state = self.inode_lock(ino);
        let _guard = state.mutex.lock().await;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let result = async {
                let inode = p.inode(ino).await?;
                if inode.kind != EntityKind::File {
                    return Err(EntityError::Unsupported("open requires a regular file"));
                }
                if inode.nlink == 0 {
                    return Err(EntityError::NotFound);
                }
                p.access(&inode, credentials, access).await
            }
            .await;
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                result?;
                state.handles.fetch_add(1, Ordering::AcqRel);
                return Ok(EntityHandle {
                    ino,
                    namespace: self.keys.prefix().to_vec(),
                    access,
                    state: state.clone(),
                    _admission: admission,
                });
            }
        }
        Err(EntityError::Busy)
    }

    /// Commit O_EXCL visibility with a retained open capability. Creating a
    /// mode-000 file may grant its creator the requested descriptor access; a
    /// subsequent ordinary open must pass the new inode's permissions.
    pub async fn create_open(
        self: &Arc<Self>,
        request: CreateRequest,
        credentials: Credentials,
        access: u32,
    ) -> Result<(EntityInode, EntityHandle)> {
        if request.kind != EntityKind::File || access == 0 || access & !6 != 0 {
            return Err(EntityError::Invalid("invalid create-open request".into()));
        }
        let this = self.clone();
        tokio::spawn(async move {
            let admission = this.session.admit()?;
            let ids = BTreeSet::from([request.parent]);
            let (_states, _guards) = this.lock_all(&ids).await;
            let operation = Operation::Create(request);
            for _ in 0..MAX_RETRIES {
                let authority = this.authority().await?;
                let mut p = Prepared::new(&this);
                let inode = match p.prepare(&operation, &credentials, &ids, &authority).await {
                    Err(EntityError::Conflict) => continue,
                    Err(error @ EntityError::Backend(_)) => return Err(error),
                    Err(error) => {
                        if this.backend.commit(&authority, &p.read_plan()).await?
                            == CommitOutcome::Conflict
                        {
                            continue;
                        }
                        return Err(error);
                    }
                    Ok(Some(inode)) => inode,
                    Ok(None) => return Err(EntityError::Corrupt("create lacks inode".into())),
                };
                // This new inode has not been published yet. Retain its registry
                // state before committing, while the parent lock excludes unlink.
                let state = this.inode_lock(inode.ino);
                if this.backend.commit(&authority, &p.plan()).await? == CommitOutcome::Committed {
                    state.handles.fetch_add(1, Ordering::AcqRel);
                    let handle = EntityHandle {
                        ino: inode.ino,
                        namespace: this.keys.prefix().to_vec(),
                        access,
                        state,
                        _admission: admission,
                    };
                    return Ok((inode, handle));
                }
            }
            Err(EntityError::Busy)
        })
        .await
        .map_err(|e| EntityError::Backend(format!("owned create-open task failed: {e}")))?
    }

    /// Stable bounded directory snapshot. Large directory streaming is separate
    /// from the paginated maintenance walkers; oversized replies fail explicitly.
    pub async fn readdir(
        &self,
        ino: i64,
        credentials: &Credentials,
    ) -> Result<Vec<(Vec<u8>, EntityInode)>> {
        let _admission = self.session.admit()?;
        let _guard = self.lock_inode(ino).await;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let result = async {
                p.parent(ino, credentials, 5).await?;
                let prefix = self.keys.dentry_prefix(ino);
                let rows = self
                    .backend
                    .scan_prefix(&prefix, MAX_DIRECTORY_ROWS)
                    .await?;
                let mut upper = Vec::with_capacity(rows.len());
                for (key, value) in rows {
                    let name = hex::decode(&key[prefix.len()..])
                        .map_err(|_| EntityError::Corrupt("invalid dentry key".into()))?;
                    validate_name(&name)?;
                    if key != self.keys.dentry(ino, &name) {
                        return Err(EntityError::Corrupt("noncanonical dentry key".into()));
                    }
                    upper.push((name, codec::decode(&value)?));
                    p.rows.insert(key, Some(value));
                }
                let entries = resolver::merge_directory(
                    self.lower.as_ref(),
                    self.lower.binding(),
                    ino,
                    &upper,
                )
                .await?;
                let mut output = Vec::with_capacity(entries.len());
                for (name, entry) in entries {
                    let EntityDentry::Present { ino, kind } = entry else {
                        return Err(EntityError::Corrupt(
                            "merged directory contains tombstone".into(),
                        ));
                    };
                    let inode = p.inode(ino).await?;
                    if inode.kind != kind || inode.nlink == 0 {
                        return Err(EntityError::Corrupt(
                            "directory entry disagrees with inode".into(),
                        ));
                    }
                    output.push((name, inode));
                }
                Ok(output)
            }
            .await;
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                return result;
            }
        }
        Err(EntityError::Busy)
    }
}

#[cfg(test)]
#[path = "namespace_mount_tests.rs"]
mod tests;
