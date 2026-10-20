//! Current-entity namespace transactions. Local locks cover only affected inodes;
//! exact read predicates cover mutable permissions and ancestor topology.
//!
//! Submitted operations run in owned tasks: cancelling a caller cannot release
//! its locks while a backend transaction is still executing.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::meta::posix_acl::{ACCESS_XATTR, DEFAULT_XATTR, PosixAcl};

use super::{
    EntityError, Result,
    backend::{CommitOutcome, EntityBackend, EntityPlan, RowCheck, RowWrite, TransactionAuthority},
    codec,
    keys::EntityKeys,
    lifecycle::{Lifecycle, OperationGuard, WriterSession},
    model::*,
    resolver::{self, ImmutableLower},
};

const MAX_RETRIES: usize = 16;
const MAX_ANCESTORS: usize = 256;
const MAX_DIRECTORY_ROWS: usize = 4096;

#[path = "namespace_create.rs"]
mod create_ops;
#[path = "namespace_mount.rs"]
mod mount_ops;

#[derive(Clone, Debug)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
    pub umask: u32,
}

#[derive(Clone, Debug)]
pub struct CreateRequest {
    pub parent: i64,
    pub name: Vec<u8>,
    pub kind: EntityKind,
    pub mode: u32,
    pub rdev: u32,
    pub symlink_target: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub(super) struct InodeLock {
    mutex: Arc<AsyncMutex<()>>,
    handles: AtomicUsize,
}

pub struct EntityHandle {
    ino: i64,
    namespace: Vec<u8>,
    access: u32,
    state: Arc<InodeLock>,
    _admission: OperationGuard,
}

impl EntityHandle {
    pub fn ino(&self) -> i64 {
        self.ino
    }
}

impl Drop for EntityHandle {
    fn drop(&mut self) {
        self.state.handles.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct EntityNamespace<B, L> {
    pub(super) backend: Arc<B>,
    pub(super) lower: Arc<L>,
    pub(super) keys: EntityKeys,
    pub(super) lifecycle: Lifecycle<B>,
    pub(super) session: WriterSession,
    locks: Arc<Mutex<BTreeMap<i64, Weak<InodeLock>>>>,
    // Advisory topology only. Every inode/ACL used for a decision is freshly
    // read and checked; stale hints can only cause additional reads/retries.
    directory_hints: moka::sync::Cache<i64, i64>,
}

/// Retain the registry entry as well as its mutex. Keeping only the mutex guard
/// would allow the weak registry to create a second lock for the same inode.
pub(super) struct InodeGuard {
    _state: Arc<InodeLock>,
    _guard: OwnedMutexGuard<()>,
}

#[derive(Clone)]
enum Operation {
    Link {
        ino: i64,
        parent: i64,
        name: Vec<u8>,
    },
    Unlink {
        parent: i64,
        name: Vec<u8>,
        directory: bool,
    },
    Rename {
        parent: i64,
        name: Vec<u8>,
        destination: i64,
        new_name: Vec<u8>,
        no_replace: bool,
    },
    Chmod {
        ino: i64,
        mode: u32,
    },
    Xattr {
        ino: i64,
        name: Vec<u8>,
        value: Option<Vec<u8>>,
    },
}

struct Prepared<'a, B, L> {
    ns: &'a EntityNamespace<B, L>,
    rows: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    now: i64,
}

impl<B: EntityBackend + 'static, L: ImmutableLower + 'static> EntityNamespace<B, L> {
    /// The lifecycle must already own its lower and writer lease. Production
    /// attach additionally authenticates and retains the packed lower graph.
    pub fn new(
        backend: Arc<B>,
        lower: Arc<L>,
        keys: EntityKeys,
        session: WriterSession,
    ) -> Arc<Self> {
        let locks = session.inode_locks.clone();
        Arc::new(Self {
            lifecycle: Lifecycle::new(backend.clone(), keys.clone()),
            backend,
            lower,
            keys,
            session,
            locks,
            directory_hints: moka::sync::Cache::new(4096),
        })
    }

    pub(super) async fn authority(&self) -> Result<TransactionAuthority> {
        let _diagnostic = super::diagnostics::Span::start("namespace.authority");
        if self.session.namespace_id() != self.keys.namespace_id() {
            return Err(EntityError::Fenced);
        }
        if self.session.lower_binding() != self.lower.binding() {
            return Err(EntityError::Fenced);
        }
        self.lifecycle.authority(&self.session).await
    }

    fn inode_lock(&self, ino: i64) -> Arc<InodeLock> {
        let mut locks = self.locks.lock().expect("entity lock registry poisoned");
        if let Some(lock) = locks.get(&ino).and_then(Weak::upgrade) {
            return lock;
        }
        // Only dead registry entries are pruned; live handles retain their lock.
        if locks.len() > 4096 {
            locks.retain(|_, v| v.strong_count() > 0);
        }
        let lock = Arc::new(InodeLock::default());
        locks.insert(ino, Arc::downgrade(&lock));
        lock
    }

    pub(super) async fn lock_inode(&self, ino: i64) -> InodeGuard {
        let _diagnostic = super::diagnostics::Span::start("data.lock_wait");
        let state = self.inode_lock(ino);
        let guard = state.mutex.clone().lock_owned().await;
        InodeGuard {
            _state: state,
            _guard: guard,
        }
    }

    pub(super) fn validate_handle(&self, handle: &EntityHandle, access: u32) -> Result<i64> {
        if handle.namespace != self.keys.prefix() {
            return Err(EntityError::Invalid("foreign handle".into()));
        }
        if access & !7 != 0 || handle.access & access != access {
            return Err(EntityError::Permission);
        }
        Ok(handle.ino)
    }

    async fn lock_all(
        &self,
        ids: &BTreeSet<i64>,
    ) -> (Vec<Arc<InodeLock>>, Vec<OwnedMutexGuard<()>>) {
        let _diagnostic = super::diagnostics::Span::start("namespace.lock_wait");
        let states: Vec<_> = ids.iter().map(|ino| self.inode_lock(*ino)).collect();
        let mut guards = Vec::with_capacity(states.len());
        for state in &states {
            guards.push(state.mutex.clone().lock_owned().await);
        }
        (states, guards)
    }

    async fn run(
        self: &Arc<Self>,
        operation: Operation,
        credentials: Credentials,
    ) -> Result<Option<EntityInode>> {
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            this.execute(operation, credentials).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned namespace task failed: {e}")))?
    }

    pub async fn create(
        self: &Arc<Self>,
        request: CreateRequest,
        credentials: Credentials,
    ) -> Result<EntityInode> {
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            let _admission = this.session.admit()?;
            let _guard = this.lock_inode(request.parent).await;
            this.create_locked(&request, &credentials).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned create task failed: {e}")))?
    }
    pub async fn link(
        self: &Arc<Self>,
        ino: i64,
        parent: i64,
        name: Vec<u8>,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(Operation::Link { ino, parent, name }, credentials)
            .await
            .map(|_| ())
    }
    pub async fn unlink(
        self: &Arc<Self>,
        parent: i64,
        name: Vec<u8>,
        directory: bool,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(
            Operation::Unlink {
                parent,
                name,
                directory,
            },
            credentials,
        )
        .await
        .map(|_| ())
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn rename(
        self: &Arc<Self>,
        parent: i64,
        name: Vec<u8>,
        destination: i64,
        new_name: Vec<u8>,
        no_replace: bool,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(
            Operation::Rename {
                parent,
                name,
                destination,
                new_name,
                no_replace,
            },
            credentials,
        )
        .await
        .map(|_| ())
    }
    pub async fn chmod(
        self: &Arc<Self>,
        ino: i64,
        mode: u32,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(Operation::Chmod { ino, mode }, credentials)
            .await
            .map(|_| ())
    }
    pub async fn set_xattr(
        self: &Arc<Self>,
        ino: i64,
        name: Vec<u8>,
        value: Option<Vec<u8>>,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(Operation::Xattr { ino, name, value }, credentials)
            .await
            .map(|_| ())
    }

    pub async fn lookup(
        &self,
        parent: i64,
        name: &[u8],
        credentials: &Credentials,
    ) -> Result<EntityInode> {
        let _admission = self.session.admit()?;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let result = async {
                p.parent(parent, credentials, 1).await?;
                let (ino, _) = p.dentry(parent, name).await?.ok_or(EntityError::NotFound)?;
                p.inode(ino).await
            }
            .await;
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                return result;
            }
        }
        Err(EntityError::Busy)
    }

    /// Explicit noatime. The final read predicate validation has no writes.
    /// O_TRUNC is a separate EntityData truncate operation after authorized open.
    pub async fn open(
        &self,
        parent: i64,
        name: &[u8],
        access: u32,
        credentials: &Credentials,
    ) -> Result<EntityHandle> {
        if access == 0 || access & !7 != 0 {
            return Err(EntityError::Invalid("invalid open access mask".into()));
        }
        let admission = self.session.admit()?;
        for _ in 0..MAX_RETRIES {
            let authority = self.authority().await?;
            let mut discovery = Prepared::new(self);
            let discovered = async {
                discovery.parent(parent, credentials, 1).await?;
                discovery
                    .dentry_row(parent, name)
                    .await?
                    .ok_or(EntityError::NotFound)
            }
            .await;
            let discovered = match discovered {
                Ok((ino, _)) => ino,
                Err(error) => {
                    if self
                        .backend
                        .commit(&authority, &discovery.read_plan())
                        .await?
                        == CommitOutcome::Conflict
                    {
                        continue;
                    }
                    return Err(error);
                }
            };
            let state = self.inode_lock(discovered);
            let _guard = state.mutex.lock().await;
            let mut p = Prepared::new(self);
            let result = async {
                p.parent(parent, credentials, 1).await?;
                let (ino, kind) = p.dentry(parent, name).await?.ok_or(EntityError::NotFound)?;
                if ino != discovered {
                    return Err(EntityError::Conflict);
                }
                if kind != EntityKind::File {
                    return Err(EntityError::Unsupported(
                        "reference open supports regular files",
                    ));
                }
                let inode = p.inode(ino).await?;
                p.access(&inode, credentials, access).await?;
                Ok(ino)
            }
            .await;
            if matches!(result, Err(EntityError::Conflict)) {
                continue;
            }
            if self.backend.commit(&authority, &p.read_plan()).await? == CommitOutcome::Committed {
                let ino = result?;
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

    /// Linked close is local bookkeeping. Unlinked inodes remain durable orphans
    /// for the ownership recovery walker; close never silently drops their data.
    pub fn close(&self, handle: EntityHandle) -> Result<()> {
        if handle.namespace != self.keys.prefix() {
            return Err(EntityError::Invalid("foreign handle".into()));
        }
        drop(handle);
        Ok(())
    }

    async fn execute(
        &self,
        op: Operation,
        credentials: Credentials,
    ) -> Result<Option<EntityInode>> {
        let _admission = self.session.admit()?;
        for _ in 0..MAX_RETRIES {
            let mut discovery = Prepared::new(self);
            let mut ids = BTreeSet::new();
            match &op {
                Operation::Link { ino, parent, .. } => {
                    ids.extend([*ino, *parent]);
                }
                Operation::Unlink { parent, name, .. } => {
                    ids.insert(*parent);
                    if let Some((ino, _)) = discovery.dentry_row(*parent, name).await? {
                        ids.insert(ino);
                    }
                }
                Operation::Rename {
                    parent,
                    name,
                    destination,
                    new_name,
                    ..
                } => {
                    ids.extend([*parent, *destination]);
                    for (p, n) in [(*parent, name), (*destination, new_name)] {
                        if let Some((ino, _)) = discovery.dentry_row(p, n).await? {
                            ids.insert(ino);
                        }
                    }
                }
                Operation::Chmod { ino, .. } | Operation::Xattr { ino, .. } => {
                    ids.insert(*ino);
                }
            }
            let (_states, _guards) = self.lock_all(&ids).await;
            let authority = self.authority().await?;
            let mut p = Prepared::new(self);
            let outcome = p.prepare(&op, &credentials, &ids).await;
            let outcome = match outcome {
                Err(EntityError::Conflict) => continue,
                Err(error @ EntityError::Backend(_)) => return Err(error),
                Err(error) => {
                    // A denial/corruption decision prepared from several reads
                    // must itself be stable; conflicting inputs are recalculated.
                    if self.backend.commit(&authority, &p.read_plan()).await?
                        == CommitOutcome::Conflict
                    {
                        continue;
                    }
                    return Err(error);
                }
                Ok(outcome) => outcome,
            };
            match self.backend.commit(&authority, &p.plan()).await? {
                CommitOutcome::Committed => return Ok(outcome),
                CommitOutcome::Conflict => continue,
            }
        }
        Err(EntityError::Busy)
    }
}

impl<'a, B: EntityBackend + 'static, L: ImmutableLower + 'static> Prepared<'a, B, L> {
    fn new(ns: &'a EntityNamespace<B, L>) -> Self {
        Self {
            ns,
            rows: BTreeMap::new(),
            writes: BTreeMap::new(),
            now: 0,
        }
    }
    async fn raw(&mut self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        if let Some(value) = self.writes.get(&key) {
            return Ok(value.clone());
        }
        if let Some(value) = self.rows.get(&key) {
            return Ok(value.clone());
        }
        let snapshot = self.ns.backend.read(std::slice::from_ref(&key)).await?;
        self.now = snapshot.now_ns;
        let value = snapshot
            .rows
            .into_iter()
            .next()
            .ok_or_else(|| EntityError::Corrupt("short backend snapshot".into()))?;
        self.rows.insert(key, value.clone());
        Ok(value)
    }
    async fn put<T: serde::Serialize>(&mut self, key: Vec<u8>, value: &T) -> Result<()> {
        self.raw(key.clone()).await?;
        self.writes.insert(key, Some(codec::encode(value)?));
        Ok(())
    }
    fn plan(&self) -> EntityPlan {
        EntityPlan {
            checks: self
                .rows
                .iter()
                .map(|(key, expected)| RowCheck {
                    key: key.clone(),
                    expected: expected.clone(),
                })
                .collect(),
            writes: self
                .writes
                .iter()
                .map(|(key, value)| match value {
                    Some(value) => RowWrite::Put {
                        key: key.clone(),
                        value: value.clone(),
                    },
                    None => RowWrite::Delete { key: key.clone() },
                })
                .collect(),
        }
    }
    fn read_plan(&self) -> EntityPlan {
        EntityPlan {
            checks: self
                .rows
                .iter()
                .map(|(key, expected)| RowCheck {
                    key: key.clone(),
                    expected: expected.clone(),
                })
                .collect(),
            writes: Vec::new(),
        }
    }
    fn append_plan(&mut self, plan: EntityPlan) -> Result<()> {
        for check in plan.checks {
            if let Some(previous) = self.rows.insert(check.key, check.expected.clone())
                && previous != check.expected
            {
                return Err(EntityError::Conflict);
            }
        }
        for write in plan.writes {
            let (key, value) = match write {
                RowWrite::Put { key, value } => (key, Some(value)),
                RowWrite::Delete { key } => (key, None),
            };
            if self.writes.insert(key, value).is_some() {
                return Err(EntityError::Invalid("duplicate prepared write".into()));
            }
        }
        Ok(())
    }
    async fn inode(&mut self, ino: i64) -> Result<EntityInode> {
        let _diagnostic = super::diagnostics::Span::start("namespace.resolve_inode");
        let row = self.raw(self.ns.keys.inode(ino)).await?;
        let upper: Option<UpperInode> = row.as_deref().map(codec::decode).transpose()?;
        let inode = resolver::resolve_inode(
            self.ns.lower.as_ref(),
            self.ns.lower.binding(),
            ino,
            upper.as_ref(),
        )
        .await?
        .ok_or(EntityError::NotFound)?;
        if inode.kind == EntityKind::Directory
            && let Some(parent) = inode.parent_hint
        {
            self.ns.directory_hints.insert(ino, parent);
        }
        Ok(inode)
    }
    async fn dentry_row(&mut self, parent: i64, name: &[u8]) -> Result<Option<(i64, EntityKind)>> {
        let _diagnostic = super::diagnostics::Span::start("namespace.resolve_dentry");
        validate_name(name)?;
        let row = self.raw(self.ns.keys.dentry(parent, name)).await?;
        let upper: Option<EntityDentry> = row.as_deref().map(codec::decode).transpose()?;
        Ok(
            match resolver::resolve_dentry(
                self.ns.lower.as_ref(),
                self.ns.lower.binding(),
                parent,
                name,
                upper.as_ref(),
            )
            .await?
            {
                Some(EntityDentry::Present { ino, kind }) => Some((ino, kind)),
                _ => None,
            },
        )
    }
    async fn dentry(&mut self, parent: i64, name: &[u8]) -> Result<Option<(i64, EntityKind)>> {
        match self.dentry_row(parent, name).await? {
            Some((ino, kind)) => {
                let inode = self.inode(ino).await.map_err(|e| {
                    if e == EntityError::NotFound {
                        EntityError::Corrupt("live dentry lacks inode".into())
                    } else {
                        e
                    }
                })?;
                if inode.kind != kind || inode.nlink == 0 {
                    return Err(EntityError::Corrupt("live dentry/inode mismatch".into()));
                }
                Ok(Some((ino, kind)))
            }
            _ => Ok(None),
        }
    }
    async fn xattr(&mut self, ino: i64, name: &[u8]) -> Result<Option<Vec<u8>>> {
        let _diagnostic = super::diagnostics::Span::start("namespace.resolve_xattr");
        let row = self.raw(self.ns.keys.xattr(ino, name)).await?;
        let upper: Option<EntityValue> = row.as_deref().map(codec::decode).transpose()?;
        resolver::resolve_xattr(
            self.ns.lower.as_ref(),
            self.ns.lower.binding(),
            ino,
            name,
            upper.as_ref(),
        )
        .await
    }
    async fn access(&mut self, inode: &EntityInode, c: &Credentials, requested: u32) -> Result<()> {
        let acl = self
            .xattr(inode.ino, ACCESS_XATTR)
            .await?
            .map(|v| PosixAcl::decode(&v).map_err(|e| EntityError::Corrupt(e.into())))
            .transpose()?;
        if acl
            .as_ref()
            .is_some_and(|a| a.mode_bits() != inode.mode & 0o777)
        {
            return Err(EntityError::Corrupt("mode/ACL mismatch".into()));
        }
        if c.uid == 0 {
            if requested & 1 == 0 || inode.kind == EntityKind::Directory || inode.mode & 0o111 != 0
            {
                return Ok(());
            }
            return Err(EntityError::Permission);
        }
        let mut groups = c.groups.clone();
        groups.push(c.gid);
        let allowed = if let Some(acl) = acl {
            acl.allows_access(inode.uid, inode.gid, c.uid, &groups, requested)
        } else {
            let shift = if c.uid == inode.uid {
                6
            } else if groups.contains(&inode.gid) {
                3
            } else {
                0
            };
            (inode.mode >> shift) & requested == requested
        };
        if allowed {
            Ok(())
        } else {
            Err(EntityError::Permission)
        }
    }
    async fn parent(&mut self, ino: i64, c: &Credentials, requested: u32) -> Result<EntityInode> {
        let parent = self.inode(ino).await?;
        if parent.kind != EntityKind::Directory {
            return Err(EntityError::NotDirectory);
        }
        if parent.nlink == 0 {
            return Err(EntityError::NotFound);
        }
        self.access(&parent, c, requested).await?;
        let mut current = parent.clone();
        let mut seen = BTreeSet::new();
        while current.ino != self.ns.lower.binding().root_ino {
            if !seen.insert(current.ino) || seen.len() > MAX_ANCESTORS {
                return Err(EntityError::Corrupt(
                    "invalid or excessive ancestor chain".into(),
                ));
            }
            current = self
                .inode(
                    current
                        .parent_hint
                        .ok_or_else(|| EntityError::Corrupt("directory lacks parent".into()))?,
                )
                .await?;
            if current.kind != EntityKind::Directory || current.nlink == 0 {
                return Err(EntityError::NotDirectory);
            }
            self.access(&current, c, 1).await?;
        }
        Ok(parent)
    }
    async fn store_inode(&mut self, mut inode: EntityInode) -> Result<()> {
        // The durable cut owns the inode until all stale maps are retired.
        // Namespace mutations must not change its cleanup fence in between
        // bounded recovery steps, even after an interrupted truncate caller.
        if self.raw(self.ns.keys.truncate(inode.ino)).await?.is_some() {
            return Err(EntityError::Busy);
        }
        inode.data_version = inode
            .data_version
            .checked_add(1)
            .ok_or(EntityError::Overflow)?;
        inode.ctime_ns = self.now;
        inode.validate()?;
        self.put(self.ns.keys.inode(inode.ino), &UpperInode::Present(inode))
            .await
    }
    async fn touch_parent(&mut self, ino: i64, link_delta: i32) -> Result<()> {
        let mut parent = self.inode(ino).await?;
        parent.nlink = parent
            .nlink
            .checked_add_signed(link_delta)
            .ok_or(EntityError::Overflow)?;
        parent.mtime_ns = self.now;
        self.store_inode(parent).await
    }
    fn sticky(parent: &EntityInode, target: &EntityInode, c: &Credentials) -> Result<()> {
        if parent.mode & 0o1000 != 0 && c.uid != 0 && c.uid != parent.uid && c.uid != target.uid {
            Err(EntityError::Permission)
        } else {
            Ok(())
        }
    }
    fn require_locked(ids: &BTreeSet<i64>, ino: i64) -> Result<()> {
        if ids.contains(&ino) {
            Ok(())
        } else {
            Err(EntityError::Conflict)
        }
    }
    async fn empty_directory(&mut self, ino: i64) -> Result<()> {
        // The directory inode predicate fences the scan: every namespace mutation
        // changes that parent, including deletion/recreation of an existing name.
        self.inode(ino).await?;
        let prefix = self.ns.keys.dentry_prefix(ino);
        let rows = self
            .ns
            .backend
            .scan_prefix(&prefix, MAX_DIRECTORY_ROWS)
            .await?;
        let mut upper = Vec::with_capacity(rows.len());
        for (key, value) in rows {
            let name = hex::decode(&key[prefix.len()..])
                .map_err(|_| EntityError::Corrupt("invalid dentry key".into()))?;
            validate_name(&name)?;
            if key != self.ns.keys.dentry(ino, &name) {
                return Err(EntityError::Corrupt("noncanonical dentry key".into()));
            }
            upper.push((name, codec::decode(&value)?));
            self.rows.insert(key, Some(value));
        }
        if resolver::merge_directory(self.ns.lower.as_ref(), self.ns.lower.binding(), ino, &upper)
            .await?
            .is_empty()
        {
            Ok(())
        } else {
            Err(EntityError::NotEmpty)
        }
    }
    async fn retire_link(&mut self, mut inode: EntityInode) -> Result<()> {
        inode.nlink = if inode.kind == EntityKind::Directory {
            0
        } else {
            inode
                .nlink
                .checked_sub(1)
                .ok_or_else(|| EntityError::Corrupt("link count underflow".into()))?
        };
        if inode.nlink == 0 {
            // A durable orphan is required even without a local open handle:
            // ownership cleanup can span transactions and outlive this request.
            let current = self.raw(self.ns.keys.orphan(inode.ino)).await?;
            self.append_plan(super::ownership::plan_orphan(
                &self.ns.keys,
                &self.ns.session,
                inode.ino,
                current,
            )?)?;
        }
        self.store_inode(inode).await
    }
    async fn prepare(
        &mut self,
        op: &Operation,
        c: &Credentials,
        ids: &BTreeSet<i64>,
    ) -> Result<Option<EntityInode>> {
        let _diagnostic = super::diagnostics::Span::start("namespace.prepare");
        match op {
            Operation::Link { ino, parent, name } => {
                self.parent(*parent, c, 3).await?;
                if self.dentry(*parent, name).await?.is_some() {
                    return Err(EntityError::Exists);
                }
                let mut inode = self.inode(*ino).await?;
                if inode.kind == EntityKind::Directory {
                    return Err(EntityError::IsDirectory);
                }
                if inode.nlink == 0 {
                    return Err(EntityError::NotFound);
                }
                inode.nlink = inode.nlink.checked_add(1).ok_or(EntityError::Overflow)?;
                self.put(
                    self.ns.keys.dentry(*parent, name),
                    &EntityDentry::Present {
                        ino: *ino,
                        kind: inode.kind,
                    },
                )
                .await?;
                self.store_inode(inode).await?;
                self.touch_parent(*parent, 0).await?;
            }
            Operation::Unlink {
                parent,
                name,
                directory,
            } => {
                let parent_inode = self.parent(*parent, c, 3).await?;
                let (ino, kind) = self
                    .dentry(*parent, name)
                    .await?
                    .ok_or(EntityError::NotFound)?;
                Self::require_locked(ids, ino)?;
                let inode = self.inode(ino).await?;
                Self::sticky(&parent_inode, &inode, c)?;
                if *directory != (kind == EntityKind::Directory) {
                    return Err(if *directory {
                        EntityError::NotDirectory
                    } else {
                        EntityError::IsDirectory
                    });
                }
                if *directory {
                    self.empty_directory(ino).await?;
                }
                self.put(self.ns.keys.dentry(*parent, name), &EntityDentry::Whiteout)
                    .await?;
                self.retire_link(inode).await?;
                self.touch_parent(*parent, if *directory { -1 } else { 0 })
                    .await?;
            }
            Operation::Rename {
                parent,
                name,
                destination,
                new_name,
                no_replace,
            } => {
                let from = self.parent(*parent, c, 3).await?;
                let to = self.parent(*destination, c, 3).await?;
                let (ino, kind) = self
                    .dentry(*parent, name)
                    .await?
                    .ok_or(EntityError::NotFound)?;
                Self::require_locked(ids, ino)?;
                let mut inode = self.inode(ino).await?;
                Self::sticky(&from, &inode, c)?;
                let target = self.dentry(*destination, new_name).await?;
                if *no_replace && target.is_some() {
                    return Err(EntityError::Exists);
                }
                if target.is_some_and(|(target, _)| target == ino) {
                    return Ok(None);
                }
                if kind == EntityKind::Directory {
                    let mut ancestor = *destination;
                    let mut seen = BTreeSet::new();
                    loop {
                        if ancestor == ino {
                            return Err(EntityError::Invalid(
                                "rename would create a directory cycle".into(),
                            ));
                        }
                        if ancestor == self.ns.lower.binding().root_ino {
                            break;
                        }
                        if !seen.insert(ancestor) || seen.len() > MAX_ANCESTORS {
                            return Err(EntityError::Corrupt("directory cycle".into()));
                        }
                        ancestor =
                            self.inode(ancestor).await?.parent_hint.ok_or_else(|| {
                                EntityError::Corrupt("directory lacks parent".into())
                            })?;
                    }
                }
                let mut replaced_directory = false;
                if let Some((target, target_kind)) = target {
                    Self::require_locked(ids, target)?;
                    if (kind == EntityKind::Directory) != (target_kind == EntityKind::Directory) {
                        return Err(if kind == EntityKind::Directory {
                            EntityError::NotDirectory
                        } else {
                            EntityError::IsDirectory
                        });
                    }
                    let target = self.inode(target).await?;
                    Self::sticky(&to, &target, c)?;
                    if target_kind == EntityKind::Directory {
                        self.empty_directory(target.ino).await?;
                        replaced_directory = true;
                    }
                    self.retire_link(target).await?;
                }
                self.put(self.ns.keys.dentry(*parent, name), &EntityDentry::Whiteout)
                    .await?;
                self.put(
                    self.ns.keys.dentry(*destination, new_name),
                    &EntityDentry::Present { ino, kind },
                )
                .await?;
                if kind == EntityKind::Directory {
                    inode.parent_hint = Some(*destination);
                }
                self.store_inode(inode).await?;
                self.touch_parent(*parent, if kind == EntityKind::Directory { -1 } else { 0 })
                    .await?;
                self.touch_parent(
                    *destination,
                    i32::from(kind == EntityKind::Directory) - i32::from(replaced_directory),
                )
                .await?;
            }
            Operation::Chmod { ino, mode } => {
                if mode & !0o7777 != 0 {
                    return Err(EntityError::Invalid("invalid chmod mode".into()));
                }
                let mut inode = self.inode(*ino).await?;
                if c.uid != 0 && c.uid != inode.uid {
                    return Err(EntityError::Permission);
                }
                if inode.kind == EntityKind::Symlink {
                    return Err(EntityError::Unsupported("chmod symlink"));
                }
                inode.mode = *mode;
                if c.uid != 0 && inode.gid != c.gid && !c.groups.contains(&inode.gid) {
                    inode.mode &= !0o2000;
                }
                if let Some(acl) = self.xattr(*ino, ACCESS_XATTR).await? {
                    let acl = PosixAcl::decode(&acl)
                        .map_err(|e| EntityError::Corrupt(e.into()))?
                        .chmod(inode.mode);
                    self.put(
                        self.ns.keys.xattr(*ino, ACCESS_XATTR),
                        &EntityValue::Present(acl.encode()),
                    )
                    .await?;
                }
                self.store_inode(inode).await?;
            }
            Operation::Xattr { ino, name, value } => {
                validate_xattr_name(name)?;
                let mut inode = self.inode(*ino).await?;
                if !name.starts_with(b"user.") && name != ACCESS_XATTR && name != DEFAULT_XATTR {
                    return Err(EntityError::Unsupported("xattr namespace"));
                }
                if name.starts_with(b"user.") {
                    if !matches!(inode.kind, EntityKind::File | EntityKind::Directory) {
                        return Err(EntityError::Unsupported("user xattr on special inode"));
                    }
                    if inode.kind == EntityKind::Directory {
                        Self::sticky(&inode, &inode, c)?;
                    }
                    self.access(&inode, c, 2).await?;
                } else {
                    if inode.kind == EntityKind::Symlink {
                        return Err(EntityError::Unsupported("ACL on symlink"));
                    }
                    if c.uid != 0 && c.uid != inode.uid {
                        return Err(EntityError::Permission);
                    }
                }
                if value.as_ref().is_some_and(|v| v.len() > MAX_XATTR_BYTES) {
                    return Err(EntityError::Invalid("xattr exceeds bound".into()));
                }
                if name == DEFAULT_XATTR && inode.kind != EntityKind::Directory {
                    return Err(EntityError::NotDirectory);
                }
                if (name == ACCESS_XATTR || name == DEFAULT_XATTR)
                    && let Some(value) = value
                {
                    let acl =
                        PosixAcl::decode(value).map_err(|e| EntityError::Invalid(e.into()))?;
                    if name == ACCESS_XATTR {
                        inode.mode = (inode.mode & 0o7000) | acl.mode_bits();
                    }
                }
                if value.is_none() && self.xattr(*ino, name).await?.is_none() {
                    return Err(EntityError::NotFound);
                }
                self.put(
                    self.ns.keys.xattr(*ino, name),
                    &value
                        .clone()
                        .map_or(EntityValue::Whiteout, EntityValue::Present),
                )
                .await?;
                self.store_inode(inode).await?;
            }
        }
        Ok(None)
    }
}
