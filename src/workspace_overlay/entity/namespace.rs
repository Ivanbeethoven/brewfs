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
    backend::{
        CommitOutcome, CreateAllocation, EntityBackend, EntityPlan, RowCheck, RowWrite,
        TransactionAuthority,
    },
    codec,
    keys::EntityKeys,
    lifecycle::{Lifecycle, OperationGuard, WriterSession},
    model::*,
    resolver::{self, ImmutableLower},
};

const MAX_RETRIES: usize = 16;
const MAX_ANCESTORS: usize = 256;
const MAX_DIRECTORY_ROWS: usize = 4096;

#[path = "namespace_snapshot.rs"]
mod snapshot;

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
    dentry_hints: moka::sync::Cache<(i64, Vec<u8>), i64>,
}

/// Retain the registry entry as well as its mutex. Keeping only the mutex guard
/// would allow the weak registry to create a second lock for the same inode.
pub(super) struct InodeGuard {
    _state: Arc<InodeLock>,
    _guard: OwnedMutexGuard<()>,
}

enum Target {
    Inode(i64),
    Name(i64, Vec<u8>),
}

enum Request {
    Create(CreateRequest, Option<u32>),
    Stat(Target),
    Access {
        ino: i64,
        mask: u32,
    },
    Open(Target, u32),
    Readdir {
        ino: i64,
    },
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

enum Reply {
    Done,
    Inode(EntityInode),
    Open(EntityInode, EntityHandle),
    Directory(Vec<(Vec<u8>, EntityInode)>),
    Create(CreateAllocation),
}

impl Reply {
    fn inode(self) -> EntityInode {
        let Self::Inode(inode) = self else {
            unreachable!("inode request reply")
        };
        inode
    }
    fn open(self) -> (EntityInode, EntityHandle) {
        let Self::Open(inode, handle) = self else {
            unreachable!("open request reply")
        };
        (inode, handle)
    }
    fn directory(self) -> Vec<(Vec<u8>, EntityInode)> {
        let Self::Directory(entries) = self else {
            unreachable!("directory request reply")
        };
        entries
    }
}

impl Request {
    fn ids(&self) -> BTreeSet<i64> {
        match self {
            Self::Create(request, _) => BTreeSet::from([request.parent]),
            Self::Stat(Target::Inode(ino))
            | Self::Access { ino, .. }
            | Self::Open(Target::Inode(ino), _)
            | Self::Readdir { ino }
            | Self::Chmod { ino, .. }
            | Self::Xattr { ino, .. } => BTreeSet::from([*ino]),
            Self::Stat(Target::Name(parent, _))
            | Self::Open(Target::Name(parent, _), _)
            | Self::Unlink { parent, .. } => BTreeSet::from([*parent]),
            Self::Link { ino, parent, .. } => BTreeSet::from([*ino, *parent]),
            Self::Rename {
                parent,
                destination,
                ..
            } => BTreeSet::from([*parent, *destination]),
        }
    }

    fn paths(&self) -> Vec<(i64, &[u8])> {
        match self {
            Self::Create(request, _) => vec![(request.parent, &request.name)],
            Self::Stat(Target::Name(parent, name))
            | Self::Open(Target::Name(parent, name), _)
            | Self::Link { parent, name, .. }
            | Self::Unlink { parent, name, .. } => vec![(*parent, name)],
            Self::Rename {
                parent,
                name,
                destination,
                new_name,
                ..
            } => vec![(*parent, name), (*destination, new_name)],
            _ => Vec::new(),
        }
    }

    fn open_access(&self) -> Option<u32> {
        match self {
            Self::Create(_, access) => *access,
            Self::Open(_, access) => Some(*access),
            _ => None,
        }
    }

    fn mutation(&self) -> bool {
        matches!(
            self,
            Self::Create(..)
                | Self::Link { .. }
                | Self::Unlink { .. }
                | Self::Rename { .. }
                | Self::Chmod { .. }
                | Self::Xattr { .. }
        )
    }
}

pub(super) struct Prepared<'a, B, L> {
    ns: &'a EntityNamespace<B, L>,
    rows: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    now: i64,
    bytes: usize,
    inodes: BTreeMap<i64, EntityInode>,
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
            dentry_hints: moka::sync::Cache::new(4096),
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

    async fn lock_all(&self, ids: &BTreeSet<i64>) -> Vec<InodeGuard> {
        let _diagnostic = super::diagnostics::Span::start("namespace.lock_wait");
        let mut guards = Vec::with_capacity(ids.len());
        for ino in ids {
            guards.push(self.lock_inode(*ino).await);
        }
        guards
    }

    async fn run(self: &Arc<Self>, request: Request, credentials: Credentials) -> Result<Reply> {
        let this = self.clone();
        tokio::spawn(super::diagnostics::inherit(async move {
            this.execute(request, credentials).await
        }))
        .await
        .map_err(|e| EntityError::Backend(format!("owned namespace task failed: {e}")))?
    }

    pub async fn create(
        self: &Arc<Self>,
        request: CreateRequest,
        credentials: Credentials,
    ) -> Result<EntityInode> {
        self.run(Request::Create(request, None), credentials)
            .await
            .map(Reply::inode)
    }

    /// Retain the parent lock until the newly committed inode's handle exists.
    /// Creation grants the requested access even when its mode denies later opens.
    pub async fn create_open(
        self: &Arc<Self>,
        request: CreateRequest,
        credentials: Credentials,
        access: u32,
    ) -> Result<(EntityInode, EntityHandle)> {
        if request.kind != EntityKind::File || access == 0 || access & !6 != 0 {
            return Err(EntityError::Invalid("invalid create-open request".into()));
        }
        self.run(Request::Create(request, Some(access)), credentials)
            .await
            .map(Reply::open)
    }

    pub async fn link(
        self: &Arc<Self>,
        ino: i64,
        parent: i64,
        name: Vec<u8>,
        credentials: Credentials,
    ) -> Result<()> {
        self.run(Request::Link { ino, parent, name }, credentials)
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
            Request::Unlink {
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
            Request::Rename {
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
        self.run(Request::Chmod { ino, mode }, credentials)
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
        self.run(Request::Xattr { ino, name, value }, credentials)
            .await
            .map(|_| ())
    }

    pub async fn lookup(
        &self,
        parent: i64,
        name: &[u8],
        credentials: &Credentials,
    ) -> Result<EntityInode> {
        self.execute(
            Request::Stat(Target::Name(parent, name.to_vec())),
            credentials.clone(),
        )
        .await
        .map(Reply::inode)
    }

    /// Noatime open; path and inode opens use the same preparation and commit.
    pub async fn open(
        &self,
        parent: i64,
        name: &[u8],
        access: u32,
        credentials: &Credentials,
    ) -> Result<EntityHandle> {
        self.open_with_attr(parent, name, access, credentials)
            .await
            .map(|(_, handle)| handle)
    }

    pub async fn open_with_attr(
        &self,
        parent: i64,
        name: &[u8],
        access: u32,
        credentials: &Credentials,
    ) -> Result<(EntityInode, EntityHandle)> {
        self.execute(
            Request::Open(Target::Name(parent, name.to_vec()), access),
            credentials.clone(),
        )
        .await
        .map(Reply::open)
    }

    /// Kernel lookup already checked ancestor search; check current inode access.
    pub async fn open_inode(
        &self,
        ino: i64,
        access: u32,
        credentials: &Credentials,
    ) -> Result<EntityHandle> {
        self.execute(
            Request::Open(Target::Inode(ino), access),
            credentials.clone(),
        )
        .await
        .map(|reply| reply.open().1)
    }

    pub async fn getattr(&self, ino: i64, credentials: &Credentials) -> Result<EntityInode> {
        self.execute(Request::Stat(Target::Inode(ino)), credentials.clone())
            .await
            .map(Reply::inode)
    }

    pub async fn access(&self, ino: i64, mask: u32, credentials: &Credentials) -> Result<()> {
        if mask & !7 != 0 {
            return Err(EntityError::Invalid("invalid access mask".into()));
        }
        self.execute(Request::Access { ino, mask }, credentials.clone())
            .await
            .map(|_| ())
    }

    pub async fn readdir(
        &self,
        ino: i64,
        credentials: &Credentials,
    ) -> Result<Vec<(Vec<u8>, EntityInode)>> {
        self.execute(Request::Readdir { ino }, credentials.clone())
            .await
            .map(Reply::directory)
    }

    /// Linked close is local. Unlinked data remains a durable recovery obligation.
    pub fn close(&self, handle: EntityHandle) -> Result<()> {
        if handle.namespace != self.keys.prefix() {
            return Err(EntityError::Invalid("foreign handle".into()));
        }
        drop(handle);
        Ok(())
    }

    fn execute(
        &self,
        request: Request,
        credentials: Credentials,
    ) -> impl std::future::Future<Output = Result<Reply>> + Send + '_ {
        Box::pin(async move {
            if request
                .open_access()
                .is_some_and(|access| access == 0 || access & !7 != 0)
            {
                return Err(EntityError::Invalid("invalid open access mask".into()));
            }
            let admission = self.session.admit()?;
            let named = matches!(
                request,
                Request::Open(Target::Name(..), _)
                    | Request::Unlink { .. }
                    | Request::Rename { .. }
            );
            for _ in 0..MAX_RETRIES {
                let mut ids = request.ids();
                let mut _guards =
                    if named || matches!(request, Request::Stat(_) | Request::Access { .. }) {
                        Vec::new()
                    } else {
                        self.lock_all(&ids).await
                    };
                let keys = self.operation_keys(&request, &ids).await?;
                let (authority, mut p) = self.snapshot(keys).await?;
                let outcome = async {
                    if named {
                        // Names select actual lock identities from this same view.
                        // Preserve those observations through lock acquisition.
                        if let Request::Open(Target::Name(parent, _), _) = &request {
                            p.parent(*parent, &credentials, 1).await?;
                            ids.clear();
                        }
                        for (parent, name) in request.paths() {
                            if let Some((ino, _)) = p.dentry_row(parent, name).await? {
                                ids.insert(ino);
                            }
                        }
                        _guards = self.lock_all(&ids).await;
                        p.prefetch(self.operation_keys(&request, &ids).await?)
                            .await?;
                    }
                    p.prepare(&request, &credentials, &ids).await
                }
                .await;
                let reply = match outcome {
                    Err(EntityError::Conflict) => continue,
                    Err(error @ EntityError::Backend(_)) => return Err(error),
                    Err(error) => {
                        // Denials also need a stable view. Never publish partially
                        // prepared writes when validation found an error.
                        if self.backend.commit(&authority, &p.read_plan()).await?
                            == CommitOutcome::Conflict
                        {
                            continue;
                        }
                        return Err(error);
                    }
                    Ok(reply) => reply,
                };
                let reply = match reply {
                    Reply::Create(create) => {
                        let Some(ino) = self
                            .backend
                            .commit_with_inode_allocation(&authority, &p.plan(), &create)
                            .await?
                        else {
                            continue;
                        };
                        let mut inode = create.inode;
                        inode.ino = ino;
                        if inode.kind == EntityKind::Directory {
                            self.directory_hints
                                .insert(ino, inode.parent_hint.expect("created parent"));
                        }
                        let Request::Create(request, _) = &request else {
                            unreachable!("create request")
                        };
                        self.dentry_hints
                            .insert((request.parent, request.name.clone()), ino);
                        Reply::Inode(inode)
                    }
                    reply => {
                        if self.backend.commit(&authority, &p.plan()).await?
                            == CommitOutcome::Conflict
                        {
                            continue;
                        }
                        reply
                    }
                };
                if let Some(access) = request.open_access() {
                    let inode = reply.inode();
                    let state = self.inode_lock(inode.ino);
                    state.handles.fetch_add(1, Ordering::AcqRel);
                    let handle = EntityHandle {
                        ino: inode.ino,
                        namespace: self.keys.prefix().to_vec(),
                        access,
                        state,
                        _admission: admission,
                    };
                    return Ok(Reply::Open(inode, handle));
                }
                return Ok(reply);
            }
            Err(EntityError::Busy)
        })
    }
}
impl<'a, B: EntityBackend + 'static, L: ImmutableLower + 'static> Prepared<'a, B, L> {
    fn new(ns: &'a EntityNamespace<B, L>) -> Self {
        Self {
            ns,
            rows: BTreeMap::new(),
            writes: BTreeMap::new(),
            now: 0,
            bytes: 0,
            inodes: BTreeMap::new(),
        }
    }
    pub(super) async fn raw(&mut self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        if let Some(value) = self.writes.get(&key) {
            return Ok(value.clone());
        }
        if let Some(value) = self.rows.get(&key) {
            return Ok(value.clone());
        }
        self.prefetch(vec![key.clone()]).await?;
        Ok(self.rows[&key].clone())
    }
    async fn put<T: serde::Serialize>(&mut self, key: Vec<u8>, value: &T) -> Result<()> {
        self.raw(key.clone()).await?;
        self.writes.insert(key, Some(codec::encode(value)?));
        self.inodes.clear();
        Ok(())
    }
    pub(super) fn plan(&self) -> EntityPlan {
        let mut plan = self.read_plan();
        plan.writes = self
            .writes
            .iter()
            .map(|(key, value)| match value {
                Some(value) => RowWrite::Put {
                    key: key.clone(),
                    value: value.clone(),
                },
                None => RowWrite::Delete { key: key.clone() },
            })
            .collect();
        plan
    }
    pub(super) fn read_plan(&self) -> EntityPlan {
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
            self.add(check)?;
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
        self.inodes.clear();
        Ok(())
    }
    pub(super) async fn inode(&mut self, ino: i64) -> Result<EntityInode> {
        let _diagnostic = super::diagnostics::Span::start("namespace.resolve_inode");
        if let Some(inode) = self.inodes.get(&ino) {
            return Ok(inode.clone());
        }
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
        self.inodes.insert(ino, inode.clone());
        Ok(inode)
    }
    async fn dentry_row(&mut self, parent: i64, name: &[u8]) -> Result<Option<(i64, EntityKind)>> {
        let _diagnostic = super::diagnostics::Span::start("namespace.resolve_dentry");
        validate_name(name)?;
        let row = self.raw(self.ns.keys.dentry(parent, name)).await?;
        let upper: Option<EntityDentry> = row.as_deref().map(codec::decode).transpose()?;
        let entry = match resolver::resolve_dentry(
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
        };
        let key = (parent, name.to_vec());
        if let Some((ino, _)) = entry {
            self.ns.dentry_hints.insert(key, ino);
        } else {
            self.ns.dentry_hints.invalidate(&key);
        }
        Ok(entry)
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
    async fn authorize_open(
        &mut self,
        inode: &EntityInode,
        c: &Credentials,
        access: u32,
    ) -> Result<()> {
        if inode.kind != EntityKind::File {
            return Err(EntityError::Unsupported("open requires a regular file"));
        }
        if inode.nlink == 0 {
            return Err(EntityError::NotFound);
        }
        self.access(inode, c, access).await
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
    async fn directory_entries(&mut self, ino: i64) -> Result<Vec<(Vec<u8>, EntityDentry)>> {
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
            self.add(RowCheck {
                key,
                expected: Some(value),
            })?;
        }
        resolver::merge_directory(self.ns.lower.as_ref(), self.ns.lower.binding(), ino, &upper)
            .await
    }
    async fn empty_directory(&mut self, ino: i64) -> Result<()> {
        if self.directory_entries(ino).await?.is_empty() {
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

    async fn prepare(
        &mut self,
        op: &Request,
        c: &Credentials,
        ids: &BTreeSet<i64>,
    ) -> Result<Reply> {
        let _diagnostic = super::diagnostics::Span::start("namespace.prepare");
        match op {
            Request::Create(request, _) => {
                return self.prepare_create(request, c).await.map(Reply::Create);
            }
            Request::Access { ino, mask } => {
                let inode = self.inode(*ino).await?;
                self.access(&inode, c, *mask).await?;
            }
            Request::Stat(target) | Request::Open(target, _) => {
                let ino = match target {
                    Target::Inode(ino) => *ino,
                    Target::Name(parent, name) => {
                        self.parent(*parent, c, 1).await?;
                        self.dentry(*parent, name)
                            .await?
                            .ok_or(EntityError::NotFound)?
                            .0
                    }
                };
                let inode = self.inode(ino).await?;
                if let Some(access) = op.open_access() {
                    Self::require_locked(ids, ino)?;
                    self.authorize_open(&inode, c, access).await?;
                }
                return Ok(Reply::Inode(inode));
            }
            Request::Readdir { ino } => {
                self.parent(*ino, c, 5).await?;
                let entries = self.directory_entries(*ino).await?;
                self.prefetch(
                    entries
                        .iter()
                        .map(|(_, entry)| match entry {
                            EntityDentry::Present { ino, .. } => self.ns.keys.inode(*ino),
                            EntityDentry::Whiteout => unreachable!("merged directory tombstone"),
                        })
                        .collect(),
                )
                .await?;
                let mut output = Vec::with_capacity(entries.len());
                for (name, entry) in entries {
                    let EntityDentry::Present { ino, kind } = entry else {
                        unreachable!("merged directory tombstone")
                    };
                    let inode = self.inode(ino).await?;
                    if inode.kind != kind || inode.nlink == 0 {
                        return Err(EntityError::Corrupt(
                            "directory entry disagrees with inode".into(),
                        ));
                    }
                    output.push((name, inode));
                }
                return Ok(Reply::Directory(output));
            }
            Request::Link { ino, parent, name } => {
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
            Request::Unlink {
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
            Request::Rename {
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
                    return Ok(Reply::Done);
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
            Request::Chmod { ino, mode } => {
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
            Request::Xattr { ino, name, value } => {
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
        Ok(Reply::Done)
    }
}
