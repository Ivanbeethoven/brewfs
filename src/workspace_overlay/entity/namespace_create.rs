//! Create reads its permission inputs together and allocates inside publication.
//! Topology hints choose a read set, never authorize an operation. A cold or
//! stale hint can require extra reads; all observed rows remain exact predicates.

use super::*;
use crate::workspace_overlay::entity::backend::{CreateAllocation, RowSnapshot};

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> EntityNamespace<B, L> {
    async fn create_snapshot(
        &self,
        request: &CreateRequest,
    ) -> Result<(TransactionAuthority, Prepared<'_, B, L>)> {
        if self.session.namespace_id() != self.keys.namespace_id()
            || self.session.lower_binding() != self.lower.binding()
        {
            return Err(EntityError::Fenced);
        }
        let authority_keys = self.lifecycle.authority_keys(&self.session)?;
        let mut keys = BTreeSet::from([
            self.keys.dentry(request.parent, &request.name),
            self.keys.xattr(request.parent, DEFAULT_XATTR),
            self.keys.truncate(request.parent),
        ]);
        let mut current = request.parent;
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
                // Immutable prefetch/memo supplies lower ancestry locally after
                // warmup. Fresh upper rows below still override this hint.
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
        let mut read_keys = authority_keys.clone();
        read_keys.extend(keys);
        let mut snapshot = self.backend.read(&read_keys).await?;
        if snapshot.rows.len() != read_keys.len() {
            return Err(EntityError::Corrupt("short create snapshot".into()));
        }
        let rows = snapshot.rows.split_off(authority_keys.len());
        let authority = self.lifecycle.authority_from_snapshot(
            &self.session,
            RowSnapshot {
                now_ns: snapshot.now_ns,
                rows: snapshot.rows,
            },
        )?;
        Ok((
            authority,
            Prepared {
                ns: self,
                rows: read_keys
                    .into_iter()
                    .skip(authority_keys.len())
                    .zip(rows)
                    .collect(),
                writes: BTreeMap::new(),
                now: snapshot.now_ns,
            },
        ))
    }

    /// The caller retains admission and the affected parent lock across retries
    /// and (for create-open) until it installs the returned open capability.
    pub(super) async fn create_locked(
        &self,
        request: &CreateRequest,
        credentials: &Credentials,
    ) -> Result<EntityInode> {
        validate_name(&request.name)?;
        for _ in 0..MAX_RETRIES {
            let (authority, mut p) = self.create_snapshot(request).await?;
            let create = match p.prepare_create(request, credentials).await {
                Err(EntityError::Conflict) => continue,
                Err(error @ EntityError::Backend(_)) => return Err(error),
                Err(error) => {
                    if self.backend.commit(&authority, &p.read_plan()).await?
                        == CommitOutcome::Conflict
                    {
                        continue;
                    }
                    return Err(error);
                }
                Ok(create) => create,
            };
            if let Some(ino) = self
                .backend
                .commit_with_inode_allocation(&authority, &p.plan(), &create)
                .await?
            {
                let mut inode = create.inode;
                inode.ino = ino;
                if inode.kind == EntityKind::Directory {
                    self.directory_hints.insert(ino, request.parent);
                }
                return Ok(inode);
            }
        }
        Err(EntityError::Busy)
    }
}

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> Prepared<'_, B, L> {
    async fn prepare_create(
        &mut self,
        r: &CreateRequest,
        c: &Credentials,
    ) -> Result<CreateAllocation> {
        let _diagnostic =
            crate::workspace_overlay::entity::diagnostics::Span::start("namespace.prepare_create");
        if r.mode & !0o7777 != 0 {
            return Err(EntityError::Invalid("invalid create mode".into()));
        }
        if matches!(r.kind, EntityKind::CharDevice | EntityKind::BlockDevice) && c.uid != 0 {
            return Err(EntityError::Permission);
        }
        let parent = self.parent(r.parent, c, 3).await?;
        if self.dentry(r.parent, &r.name).await?.is_some() {
            return Err(EntityError::Exists);
        }
        let default = self
            .xattr(r.parent, DEFAULT_XATTR)
            .await?
            .map(|v| PosixAcl::decode(&v).map_err(|e| EntityError::Corrupt(e.into())))
            .transpose()?;
        let inherited = if r.kind == EntityKind::Symlink {
            None
        } else {
            default.as_ref().map(|a| a.inherited_access(r.mode))
        };
        let mut mode = inherited
            .as_ref()
            .map_or(r.mode & !c.umask, |a| (r.mode & 0o7000) | a.mode_bits());
        if r.kind == EntityKind::Symlink {
            mode = 0o777;
        }
        if parent.mode & 0o2000 != 0 && r.kind == EntityKind::Directory {
            mode |= 0o2000;
        }
        let gid = if parent.mode & 0o2000 != 0 {
            parent.gid
        } else {
            c.gid
        };
        if c.uid != 0 && gid != c.gid && !c.groups.contains(&gid) && r.kind != EntityKind::Directory
        {
            mode &= !0o2000;
        }
        let inode = EntityInode {
            // A validated template; the transaction injects the exact allocated
            // integer without routing IDs through Lua floating-point numbers.
            ino: 1,
            kind: r.kind,
            size: r.symlink_target.as_ref().map_or(0, |v| v.len() as u64),
            mode,
            uid: c.uid,
            gid,
            rdev: r.rdev,
            nlink: if r.kind == EntityKind::Directory {
                2
            } else {
                1
            },
            atime_ns: self.now,
            mtime_ns: self.now,
            ctime_ns: self.now,
            symlink_target: r.symlink_target.clone(),
            parent_hint: Some(r.parent),
            lower_visible_size: 0,
            data_version: 0,
        };
        inode.validate()?;
        let mut xattrs = Vec::new();
        if let Some(acl) = inherited.filter(PosixAcl::is_extended) {
            xattrs.push((ACCESS_XATTR.to_vec(), acl.encode()));
        }
        if r.kind == EntityKind::Directory
            && let Some(acl) = default
        {
            xattrs.push((DEFAULT_XATTR.to_vec(), acl.encode()));
        }
        self.touch_parent(r.parent, i32::from(r.kind == EntityKind::Directory))
            .await?;
        Ok(CreateAllocation {
            keys: self.ns.keys.clone(),
            inode,
            name: r.name.clone(),
            xattrs,
            lower_max_ino: self.ns.lower.binding().max_ino,
        })
    }
}
