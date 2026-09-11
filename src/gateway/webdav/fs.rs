use std::fmt;
use std::io::SeekFrom;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use bytes::{Buf, Bytes};
use dashmap::DashSet;
use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, DavProp, FsError, FsFuture, FsResult,
    FsStream, OpenOptions, ReadDirMeta,
};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::chunk::store::BlockStore;
use crate::meta::MetaStore;
use crate::meta::client::MetaClient;
use crate::meta::store::{FileAttr, FileType, SetAttrFlags, SetAttrRequest};
use crate::vfs::error::VfsError;
use crate::vfs::fs::{FileGuard, VFS};

use super::path::{ensure_mutable, to_vfs_path};
use super::props::{self, XATTR_DEAD_PROPS};

const IO_CHUNK: u64 = 4 * 1024 * 1024;
const LOCK_SHARDS: usize = 256;
const STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct BrewFsDavFs<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    vfs: VFS<S, MetaClient<dyn MetaStore>>,
    atomic_put: bool,
    props_supported: bool,
    locks: Arc<[Arc<Mutex<()>>; LOCK_SHARDS]>,
    active_staging: Arc<DashSet<String>>,
}

struct ResolvedPath {
    parent_ino: i64,
    name: String,
    target: Option<(i64, FileAttr)>,
}

impl<S> Clone for BrewFsDavFs<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            vfs: self.vfs.clone(),
            atomic_put: self.atomic_put,
            props_supported: self.props_supported,
            locks: self.locks.clone(),
            active_staging: self.active_staging.clone(),
        }
    }
}

impl<S> BrewFsDavFs<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    pub fn new(
        vfs: VFS<S, MetaClient<dyn MetaStore>>,
        atomic_put: bool,
        props_supported: bool,
    ) -> Self {
        Self {
            vfs,
            atomic_put,
            props_supported,
            locks: Arc::new(std::array::from_fn(|_| Arc::new(Mutex::new(())))),
            active_staging: Arc::new(DashSet::new()),
        }
    }

    pub async fn initialize(&self) -> anyhow::Result<()> {
        self.vfs
            .mkdir_p(&staging_dir())
            .await
            .map_err(|error| anyhow::anyhow!("create WebDAV staging directory: {error}"))?;
        Ok(())
    }

    pub async fn cleanup_stale_staging(&self) -> anyhow::Result<()> {
        let dir = staging_dir();
        let (dir_ino, dir_attr) = self
            .resolve_existing(&dir)
            .await
            .map_err(|error| anyhow::anyhow!("read WebDAV staging directory {dir}: {error:?}"))?;
        if dir_attr.kind != FileType::Dir {
            return Err(anyhow::anyhow!(
                "WebDAV staging path is not a directory: {dir}"
            ));
        }
        let entries = self
            .read_children_ino(dir_ino)
            .await
            .map_err(|error| anyhow::anyhow!("read WebDAV staging directory {dir}: {error:?}"))?;
        let cutoff = SystemTime::now()
            .checked_sub(STAGING_MAX_AGE)
            .unwrap_or(UNIX_EPOCH);
        for entry in entries {
            let path = format!("{dir}/{}", entry.name);
            if self.active_staging.contains(&path) {
                continue;
            }
            if let Some(attr) = self.vfs.stat_ino(entry.ino).await
                && system_time(attr.mtime).is_ok_and(|mtime| mtime < cutoff)
            {
                let _ = self.vfs.unlink_at(dir_ino, &entry.name).await;
            }
        }
        Ok(())
    }

    async fn resolve_path(
        &self,
        path: &str,
        require_existing: bool,
    ) -> Result<ResolvedPath, FsError> {
        if path == "/" {
            return Err(FsError::Forbidden);
        }
        let components: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut parent_ino = self.vfs.root_ino();
        for (index, component) in components.iter().enumerate() {
            let is_leaf = index + 1 == components.len();
            let entry = self
                .vfs
                .child_attr_of(parent_ino, component)
                .await
                .map_err(map_vfs_error)?;
            let Some((ino, attr)) = entry else {
                if is_leaf && !require_existing {
                    return Ok(ResolvedPath {
                        parent_ino,
                        name: (*component).to_string(),
                        target: None,
                    });
                }
                return Err(FsError::NotFound);
            };
            if attr.kind == FileType::Symlink {
                return Err(FsError::Forbidden);
            }
            if is_leaf {
                return Ok(ResolvedPath {
                    parent_ino,
                    name: (*component).to_string(),
                    target: Some((ino, attr)),
                });
            }
            if attr.kind != FileType::Dir {
                return Err(FsError::Forbidden);
            }
            parent_ino = ino;
        }
        Err(FsError::Forbidden)
    }

    async fn resolve_existing(&self, path: &str) -> Result<(i64, FileAttr), FsError> {
        if path == "/" {
            return self
                .vfs
                .stat_ino(self.vfs.root_ino())
                .await
                .map(|attr| (self.vfs.root_ino(), attr))
                .ok_or(FsError::NotFound);
        }
        self.resolve_path(path, true)
            .await?
            .target
            .ok_or(FsError::NotFound)
    }

    async fn resolve_parent(&self, path: &str) -> Result<ResolvedPath, FsError> {
        self.resolve_path(path, false).await
    }

    async fn read_children_ino(
        &self,
        ino: i64,
    ) -> Result<Vec<crate::meta::store::DirEntry>, FsError> {
        let handle = self.vfs.opendir(ino).await.map_err(map_vfs_error)?;
        let result = (|| {
            let mut entries = Vec::new();
            let mut offset = 0;
            loop {
                let page = self
                    .vfs
                    .readdir(handle, offset)
                    .ok_or(FsError::GeneralFailure)?;
                if page.is_empty() {
                    break;
                }
                offset += page.len() as u64;
                entries.extend(page);
            }
            Ok(entries)
        })();
        let close_result = self.vfs.closedir(handle).map_err(map_vfs_error);
        match (result, close_result) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(entries), Ok(())) => Ok(entries),
        }
    }

    fn lock_for(&self, path: &str) -> Arc<Mutex<()>> {
        self.locks[lock_shard(path)].clone()
    }
    async fn lock_paths(&self, first: &str, second: &str) -> Vec<OwnedMutexGuard<()>> {
        let first_shard = lock_shard(first);
        let second_shard = lock_shard(second);
        if first_shard == second_shard {
            return vec![self.locks[first_shard].clone().lock_owned().await];
        }
        let (low, high) = if first_shard < second_shard {
            (first_shard, second_shard)
        } else {
            (second_shard, first_shard)
        };
        vec![
            self.locks[low].clone().lock_owned().await,
            self.locks[high].clone().lock_owned().await,
        ]
    }

    fn reserve_staging(&self) -> String {
        let path = format!("{}/{}", staging_dir(), uuid::Uuid::now_v7());
        self.active_staging.insert(path.clone());
        path
    }

    async fn discard_staging(&self, path: &str) {
        if let Ok(resolved) = self.resolve_parent(path).await {
            let _ = self
                .vfs
                .unlink_at(resolved.parent_ino, &resolved.name)
                .await;
        }
        self.active_staging.remove(path);
    }

    async fn copy_exact(&self, source: &FileAttr, destination_ino: i64) -> Result<(), FsError> {
        let mut offset = 0;
        while offset < source.size {
            let length = (source.size - offset).min(IO_CHUNK);
            let copied = self
                .vfs
                .copy_file_range_inodes(source.ino, offset, destination_ino, offset, length)
                .await
                .map_err(map_vfs_error)?;
            if copied as u64 != length {
                tracing::error!(copied, expected = length, "short WebDAV file copy");
                return Err(FsError::GeneralFailure);
            }
            offset += length;
        }
        Ok(())
    }

    async fn copy_dead_props(&self, source_ino: i64, destination_ino: i64) -> Result<(), FsError> {
        if !self.props_supported {
            return Ok(());
        }
        if let Some(raw) = self
            .vfs
            .get_xattr_ino(source_ino, XATTR_DEAD_PROPS)
            .await
            .map_err(map_vfs_error)?
        {
            self.vfs
                .set_xattr_ino(destination_ino, XATTR_DEAD_PROPS, &raw, 0)
                .await
                .map_err(map_vfs_error)?;
        }
        Ok(())
    }

    async fn open_inner(
        &self,
        path: String,
        options: OpenOptions,
    ) -> Result<Box<dyn DavFile>, FsError> {
        if path == "/" {
            return Err(FsError::Forbidden);
        }
        if options.write || options.create || options.create_new || options.truncate {
            ensure_mutable(&path)?;
        }
        let resolved = self.resolve_parent(&path).await?;
        let mutation_guard = if options.write || options.create || options.create_new {
            Some(self.lock_for(&path).lock_owned().await)
        } else {
            None
        };

        let existing = if mutation_guard.is_some() {
            self.vfs
                .child_attr_of(resolved.parent_ino, &resolved.name)
                .await
                .map_err(map_vfs_error)?
        } else {
            resolved.target
        };
        if let Some(expected) = super::current_request_if_match()
            && options.write
            && !if_match_satisfied(&expected, existing.as_ref().map(|(_, attr)| attr))
        {
            super::mark_precondition_failed();
            return Err(FsError::GeneralFailure);
        }
        if options.create_new && existing.is_some() {
            return Err(FsError::Exists);
        }
        if existing
            .as_ref()
            .is_some_and(|(_, attr)| attr.kind != FileType::File)
        {
            return Err(FsError::Forbidden);
        }
        if existing.is_none() && !options.create {
            return Err(FsError::NotFound);
        }

        let atomic = self.atomic_put && options.write && !super::current_request_is_lock();
        let (storage_path, storage_ino, staging_path, staging_parent_ino, staging_name, attr) =
            if atomic {
                let staging = self.reserve_staging();
                let staging_resolved = self.resolve_parent(&staging).await?;
                let staging_ino = match self
                    .vfs
                    .create_file_at(staging_resolved.parent_ino, &staging_resolved.name, true)
                    .await
                {
                    Ok(ino) => ino,
                    Err(error) => {
                        self.active_staging.remove(&staging);
                        return Err(map_vfs_error(error));
                    }
                };
                let mut staging_attr = match self.vfs.stat_ino(staging_ino).await {
                    Some(attr) => attr,
                    None => {
                        self.discard_staging(&staging).await;
                        return Err(FsError::NotFound);
                    }
                };
                if let Some((source_ino, source_attr)) = existing.as_ref() {
                    if !options.truncate
                        && let Err(error) = self.copy_exact(source_attr, staging_ino).await
                    {
                        self.discard_staging(&staging).await;
                        return Err(error);
                    }
                    if let Err(error) = self.copy_dead_props(*source_ino, staging_ino).await {
                        self.discard_staging(&staging).await;
                        return Err(error);
                    }
                    staging_attr = match self.vfs.stat_ino(staging_ino).await {
                        Some(attr) => attr,
                        None => {
                            self.discard_staging(&staging).await;
                            return Err(FsError::NotFound);
                        }
                    };
                }
                (
                    staging.clone(),
                    staging_ino,
                    Some(staging),
                    staging_resolved.parent_ino,
                    staging_resolved.name,
                    staging_attr,
                )
            } else {
                let (ino, _attr) = match existing {
                    Some(existing) => existing,
                    None => {
                        let ino = self
                            .vfs
                            .create_file_at(resolved.parent_ino, &resolved.name, options.create_new)
                            .await
                            .map_err(map_vfs_error)?;
                        let attr = self.vfs.stat_ino(ino).await.ok_or(FsError::NotFound)?;
                        (ino, attr)
                    }
                };
                if options.truncate {
                    self.vfs
                        .truncate_inode(ino, 0)
                        .await
                        .map_err(map_vfs_error)?;
                }
                (
                    path.clone(),
                    ino,
                    None,
                    0,
                    String::new(),
                    self.vfs.stat_ino(ino).await.ok_or(FsError::NotFound)?,
                )
            };
        let position = if options.append { attr.size } else { 0 };
        let guard = match self
            .vfs
            .open_guard(attr.ino, attr.clone(), options.read, options.write)
            .await
        {
            Ok(guard) => guard,
            Err(error) => {
                if let Some(staging) = staging_path.as_deref() {
                    self.discard_staging(staging).await;
                }
                return Err(map_vfs_error(error));
            }
        };
        Ok(Box::new(BrewFsDavFile {
            vfs: self.vfs.clone(),
            target_path: path,
            target_parent_ino: resolved.parent_ino,
            target_name: resolved.name,
            storage_path,
            storage_ino,
            staging_path,
            staging_parent_ino,
            staging_name,
            active_staging: self.active_staging.clone(),
            guard: Some(guard),
            mutation_guard,
            position,
            logical_size: attr.size,
            append: options.append,
            create_new: options.create_new,
            if_match: super::current_request_if_match(),
            expected_body_size: options.size,
            bytes_written: 0,
        }))
    }

    async fn read_dead_props(&self, inode: i64) -> Result<Option<Vec<u8>>, FsError> {
        if !self.props_supported {
            return Ok(None);
        }
        self.vfs
            .get_xattr_ino(inode, XATTR_DEAD_PROPS)
            .await
            .map_err(map_vfs_error)
    }
}

impl<S> DavFileSystem for BrewFsDavFs<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        async move {
            let path = to_vfs_path(path)?;
            self.open_inner(path, options).await
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        async move {
            let path = to_vfs_path(path)?;
            let (directory_ino, directory_attr) = self.resolve_existing(&path).await?;
            if directory_attr.kind != FileType::Dir {
                return Err(FsError::Forbidden);
            }
            let entries = self.read_children_ino(directory_ino).await?;
            let results = futures_util::stream::iter(entries.into_iter().filter_map(|entry| {
                if path == "/" && entry.name == ".brewfs.sys" {
                    return None;
                }
                Some(async move {
                    match self.vfs.stat_ino(entry.ino).await {
                        Some(attr) => Ok(Box::new(BrewFsDavDirEntry {
                            name: entry.name.into_bytes(),
                            attr,
                        }) as Box<dyn DavDirEntry>),
                        None => Err(FsError::NotFound),
                    }
                })
            }))
            .buffered(32)
            .collect::<Vec<_>>()
            .await;
            Ok(Box::pin(futures_util::stream::iter(results)) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        async move {
            let path = to_vfs_path(path)?;
            let (_, attr) = self.resolve_existing(&path).await?;
            Ok(Box::new(BrewFsDavMetaData(attr)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            if resolved.target.is_some() {
                return Err(FsError::Exists);
            }
            let _guard = self.lock_for(&path).lock_owned().await;
            self.vfs
                .mkdir_at_new(resolved.parent_ino, &resolved.name)
                .await
                .map_err(map_vfs_error)?;
            Ok(())
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            let Some((_, attr)) = resolved.target else {
                return Err(FsError::NotFound);
            };
            if attr.kind != FileType::Dir {
                return Err(FsError::Forbidden);
            }
            let _guard = self.lock_for(&path).lock_owned().await;
            let result = self.vfs.rmdir_at(resolved.parent_ino, &resolved.name).await;
            if matches!(result, Err(VfsError::DirectoryNotEmpty { .. })) {
                super::mark_directory_not_empty();
            }
            result.map_err(map_vfs_error)
        }
        .boxed()
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            let Some((ino, attr)) = resolved.target else {
                return Err(FsError::NotFound);
            };
            if attr.kind == FileType::Dir {
                return Err(FsError::Forbidden);
            }
            let _guard = self.lock_for(&path).lock_owned().await;
            self.vfs
                .unlink_at_with_known_attr(resolved.parent_ino, &resolved.name, ino, attr)
                .await
                .map_err(map_vfs_error)
        }
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let from = to_vfs_path(from)?;
            let to = to_vfs_path(to)?;
            ensure_mutable(&from)?;
            ensure_mutable(&to)?;
            let source = self.resolve_parent(&from).await?;
            if source.target.is_none() {
                return Err(FsError::NotFound);
            }
            let destination = self.resolve_parent(&to).await?;
            let _guards = self.lock_paths(&from, &to).await;
            self.vfs
                .rename_at_noreplace(
                    source.parent_ino,
                    &source.name,
                    destination.parent_ino,
                    &destination.name,
                )
                .await
                .map_err(map_vfs_error)
        }
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let from = to_vfs_path(from)?;
            let to = to_vfs_path(to)?;
            ensure_mutable(&to)?;
            let source = self.resolve_existing(&from).await?;
            if source.1.kind != FileType::File {
                return Err(FsError::Forbidden);
            }
            let destination = self.resolve_parent(&to).await?;
            let _guards = self.lock_paths(&from, &to).await;
            let staging = self.reserve_staging();
            let staging_parent = self.resolve_parent(&staging).await?;
            let result = async {
                let destination_ino = self
                    .vfs
                    .create_file_at(staging_parent.parent_ino, &staging_parent.name, true)
                    .await
                    .map_err(map_vfs_error)?;
                self.copy_exact(&source.1, destination_ino).await?;
                self.copy_dead_props(source.0, destination_ino).await?;
                self.vfs
                    .rename_at_noreplace(
                        staging_parent.parent_ino,
                        &staging_parent.name,
                        destination.parent_ino,
                        &destination.name,
                    )
                    .await
                    .map_err(map_vfs_error)
            }
            .await;
            if result.is_err() {
                let _ = self
                    .vfs
                    .unlink_at(staging_parent.parent_ino, &staging_parent.name)
                    .await;
            }
            self.active_staging.remove(&staging);
            result
        }
        .boxed()
    }

    fn set_accessed<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            let Some((ino, _)) = resolved.target else {
                return Err(FsError::NotFound);
            };
            let _guard = self.lock_for(&path).lock_owned().await;
            self.vfs
                .set_attr(
                    ino,
                    &SetAttrRequest {
                        atime: Some(system_time_nanos(tm)?),
                        ..Default::default()
                    },
                    SetAttrFlags::empty(),
                )
                .await
                .map_err(map_vfs_error)?;
            Ok(())
        }
        .boxed()
    }

    fn set_modified<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            let Some((ino, _)) = resolved.target else {
                return Err(FsError::NotFound);
            };
            let _guard = self.lock_for(&path).lock_owned().await;
            self.vfs
                .set_attr(
                    ino,
                    &SetAttrRequest {
                        mtime: Some(system_time_nanos(tm)?),
                        ..Default::default()
                    },
                    SetAttrFlags::empty(),
                )
                .await
                .map_err(map_vfs_error)?;
            Ok(())
        }
        .boxed()
    }

    fn have_props<'a>(
        &'a self,
        _path: &'a DavPath,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(std::future::ready(self.props_supported))
    }

    fn patch_props<'a>(
        &'a self,
        path: &'a DavPath,
        patch: Vec<(bool, DavProp)>,
    ) -> FsFuture<'a, Vec<(StatusCode, DavProp)>> {
        async move {
            if !self.props_supported {
                return Err(FsError::InsufficientStorage);
            }
            let path = to_vfs_path(path)?;
            ensure_mutable(&path)?;
            let resolved = self.resolve_parent(&path).await?;
            let Some((ino, _)) = resolved.target else {
                return Err(FsError::NotFound);
            };
            let _guard = self.lock_for(&path).lock_owned().await;
            let raw = self.read_dead_props(ino).await?;
            let (encoded, statuses) = props::apply(raw.as_deref(), patch)?;
            if let Some(encoded) = encoded {
                self.vfs
                    .set_xattr_ino(ino, XATTR_DEAD_PROPS, &encoded, 0)
                    .await
                    .map_err(map_vfs_error)?;
            }
            Ok(statuses)
        }
        .boxed()
    }

    fn get_props<'a>(&'a self, path: &'a DavPath, do_content: bool) -> FsFuture<'a, Vec<DavProp>> {
        async move {
            let path = to_vfs_path(path)?;
            let (_, attr) = self.resolve_existing(&path).await?;
            let raw = self.read_dead_props(attr.ino).await?;
            props::list(raw.as_deref(), do_content)
        }
        .boxed()
    }

    fn get_prop<'a>(&'a self, path: &'a DavPath, prop: DavProp) -> FsFuture<'a, Vec<u8>> {
        async move {
            let path = to_vfs_path(path)?;
            let (_, attr) = self.resolve_existing(&path).await?;
            let raw = self.read_dead_props(attr.ino).await?;
            props::get(raw.as_deref(), &prop)
        }
        .boxed()
    }
}

struct BrewFsDavFile<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    vfs: VFS<S, MetaClient<dyn MetaStore>>,
    target_path: String,
    target_parent_ino: i64,
    target_name: String,
    storage_path: String,
    storage_ino: i64,
    staging_path: Option<String>,
    staging_parent_ino: i64,
    staging_name: String,
    active_staging: Arc<DashSet<String>>,
    guard: Option<FileGuard<S, MetaClient<dyn MetaStore>>>,
    mutation_guard: Option<OwnedMutexGuard<()>>,
    position: u64,
    logical_size: u64,
    append: bool,
    create_new: bool,
    if_match: Option<String>,
    expected_body_size: Option<u64>,
    bytes_written: u64,
}

impl<S> fmt::Debug for BrewFsDavFile<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrewFsDavFile")
            .field("path", &self.target_path)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

impl<S> BrewFsDavFile<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    async fn write_all(&mut self, mut bytes: Bytes) -> FsResult<()> {
        if self.append {
            self.position = self.logical_size;
        }
        while !bytes.is_empty() {
            let guard = self.guard.as_ref().ok_or(FsError::GeneralFailure)?;
            let written = guard
                .write(self.position, &bytes)
                .await
                .map_err(map_vfs_error)?;
            if written == 0 {
                return Err(FsError::GeneralFailure);
            }
            self.position = self
                .position
                .checked_add(written as u64)
                .ok_or(FsError::TooLarge)?;
            self.bytes_written = self
                .bytes_written
                .checked_add(written as u64)
                .ok_or(FsError::TooLarge)?;
            self.logical_size = self.logical_size.max(self.position);
            bytes.advance(written);
        }
        Ok(())
    }

    async fn cleanup_staging(&mut self) {
        if let Some(path) = self.staging_path.take() {
            let _ = self
                .vfs
                .unlink_at(self.staging_parent_ino, &self.staging_name)
                .await;
            self.active_staging.remove(&path);
        }
    }
}

impl<S> DavFile for BrewFsDavFile<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        async move {
            let mut attr = self
                .vfs
                .stat_ino(self.storage_ino)
                .await
                .ok_or(FsError::NotFound)?;
            attr.size = self.logical_size;
            Ok(Box::new(BrewFsDavMetaData(attr)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn write_buf(&'_ mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        async move {
            while buf.has_remaining() {
                let length = buf.remaining().min(IO_CHUNK as usize);
                self.write_all(buf.copy_to_bytes(length)).await?;
            }
            Ok(())
        }
        .boxed()
    }

    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        async move { self.write_all(buf).await }.boxed()
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, Bytes> {
        async move {
            let guard = self.guard.as_ref().ok_or(FsError::GeneralFailure)?;
            let bytes = guard
                .read(self.position, count)
                .await
                .map_err(map_vfs_error)?;
            self.position = self
                .position
                .checked_add(bytes.len() as u64)
                .ok_or(FsError::TooLarge)?;
            Ok(Bytes::from(bytes))
        }
        .boxed()
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        async move {
            let next = match pos {
                SeekFrom::Start(position) => position,
                SeekFrom::Current(delta) => checked_seek(self.position, delta)?,
                SeekFrom::End(delta) => checked_seek(self.logical_size, delta)?,
            };
            self.position = next;
            Ok(next)
        }
        .boxed()
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        async move {
            if self.staging_path.is_some()
                && let Some(expected) = self.expected_body_size
                && self.bytes_written != expected
            {
                if let Some(guard) = self.guard.take() {
                    let _ = guard.close().await;
                }
                self.cleanup_staging().await;
                return Err(FsError::GeneralFailure);
            }
            if let Some(guard) = self.guard.take()
                && let Err(error) = guard.close().await
            {
                self.cleanup_staging().await;
                return Err(map_vfs_error(error));
            }
            if let Some(staging) = self.staging_path.clone() {
                if let Some(if_match) = &self.if_match {
                    let current = self
                        .vfs
                        .child_attr_of(self.target_parent_ino, &self.target_name)
                        .await
                        .map_err(map_vfs_error)?;
                    if !if_match_satisfied(if_match, current.as_ref().map(|(_, attr)| attr)) {
                        self.cleanup_staging().await;
                        super::mark_precondition_failed();
                        return Err(FsError::GeneralFailure);
                    }
                }
                let result = if self.create_new {
                    self.vfs
                        .rename_at_noreplace(
                            self.staging_parent_ino,
                            &self.staging_name,
                            self.target_parent_ino,
                            &self.target_name,
                        )
                        .await
                } else {
                    self.vfs
                        .rename_at(
                            self.staging_parent_ino,
                            &self.staging_name,
                            self.target_parent_ino,
                            &self.target_name,
                        )
                        .await
                };
                if let Err(error) = result {
                    self.cleanup_staging().await;
                    return Err(map_vfs_error(error));
                }
                self.staging_path = None;
                self.storage_path = self.target_path.clone();
                self.active_staging.remove(&staging);
            }
            self.mutation_guard.take();
            Ok(())
        }
        .boxed()
    }
}

impl<S> Drop for BrewFsDavFile<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let guard = self.guard.take();
        let staging = self.staging_path.take();
        let staging_parent_ino = self.staging_parent_ino;
        let staging_name = self.staging_name.clone();
        let mutation_guard = self.mutation_guard.take();
        let vfs = self.vfs.clone();
        let active = self.active_staging.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _mutation_guard = mutation_guard;
                if let Some(guard) = guard {
                    let _ = guard.close().await;
                }
                if let Some(path) = staging {
                    if !staging_name.is_empty() {
                        let _ = vfs.unlink_at(staging_parent_ino, &staging_name).await;
                    } else {
                        let _ = vfs.unlink(&path).await;
                    }
                    active.remove(&path);
                }
            });
        }
    }
}

#[derive(Clone, Debug)]
struct BrewFsDavDirEntry {
    name: Vec<u8>,
    attr: FileAttr,
}

impl DavDirEntry for BrewFsDavDirEntry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let metadata = BrewFsDavMetaData(self.attr.clone());
        async move { Ok(Box::new(metadata) as Box<dyn DavMetaData>) }.boxed()
    }

    fn is_dir(&'_ self) -> FsFuture<'_, bool> {
        let is_dir = self.attr.kind == FileType::Dir;
        async move { Ok(is_dir) }.boxed()
    }

    fn is_file(&'_ self) -> FsFuture<'_, bool> {
        let is_file = self.attr.kind == FileType::File;
        async move { Ok(is_file) }.boxed()
    }

    fn is_symlink(&'_ self) -> FsFuture<'_, bool> {
        let is_symlink = self.attr.kind == FileType::Symlink;
        async move { Ok(is_symlink) }.boxed()
    }
}

#[derive(Clone, Debug)]
struct BrewFsDavMetaData(FileAttr);

impl DavMetaData for BrewFsDavMetaData {
    fn len(&self) -> u64 {
        self.0.size
    }

    fn modified(&self) -> FsResult<SystemTime> {
        system_time(self.0.mtime)
    }

    fn is_dir(&self) -> bool {
        self.0.kind == FileType::Dir
    }

    fn is_symlink(&self) -> bool {
        self.0.kind == FileType::Symlink
    }

    fn etag(&self) -> Option<String> {
        Some(etag_value(&self.0))
    }

    fn accessed(&self) -> FsResult<SystemTime> {
        system_time(self.0.atime)
    }

    fn status_changed(&self) -> FsResult<SystemTime> {
        system_time(self.0.ctime)
    }

    fn executable(&self) -> FsResult<bool> {
        Ok(self.0.mode & 0o111 != 0)
    }
}

fn etag_value(attr: &FileAttr) -> String {
    format!(
        "{:x}-{:x}-{:x}-{:x}",
        attr.ino, attr.mtime, attr.ctime, attr.size
    )
}

fn if_match_satisfied(value: &str, attr: Option<&FileAttr>) -> bool {
    let Some(attr) = attr else {
        return false;
    };
    let expected = format!("\"{}\"", etag_value(attr));
    value.trim() == "*" || value.split(',').any(|tag| tag.trim() == expected)
}

fn staging_dir() -> String {
    format!("{}/tmp", crate::gateway::webdav_sys_dir())
}

fn lock_shard(path: &str) -> usize {
    use std::hash::{Hash, Hasher};

    let key = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() as usize % LOCK_SHARDS
}

fn checked_seek(base: u64, delta: i64) -> FsResult<u64> {
    if delta >= 0 {
        base.checked_add(delta as u64).ok_or(FsError::TooLarge)
    } else {
        base.checked_sub(delta.unsigned_abs())
            .ok_or(FsError::Forbidden)
    }
}

fn system_time(nanos: i64) -> FsResult<SystemTime> {
    if nanos >= 0 {
        UNIX_EPOCH
            .checked_add(Duration::from_nanos(nanos as u64))
            .ok_or(FsError::GeneralFailure)
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_nanos(nanos.unsigned_abs()))
            .ok_or(FsError::GeneralFailure)
    }
}

fn system_time_nanos(time: SystemTime) -> FsResult<i64> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FsError::Forbidden)?;
    i64::try_from(duration.as_nanos()).map_err(|_| FsError::TooLarge)
}

fn map_vfs_error(error: VfsError) -> FsError {
    match error {
        VfsError::NotFound { .. } => FsError::NotFound,
        VfsError::AlreadyExists { .. } | VfsError::DirectoryNotEmpty { .. } => FsError::Exists,
        VfsError::PermissionDenied { .. }
        | VfsError::ReadOnlyFilesystem { .. }
        | VfsError::NotADirectory { .. }
        | VfsError::IsADirectory { .. }
        | VfsError::InvalidInput
        | VfsError::InvalidData
        | VfsError::InvalidFilename
        | VfsError::InvalidRenameTarget { .. }
        | VfsError::ResourceBusy
        | VfsError::ExecutableFileBusy => FsError::Forbidden,
        VfsError::StorageFull | VfsError::QuotaExceeded | VfsError::OutOfMemory => {
            FsError::InsufficientStorage
        }
        VfsError::FileTooLarge => FsError::TooLarge,
        VfsError::CrossesDevices => FsError::IsRemote,
        VfsError::FilenameTooLong { .. } | VfsError::ArgumentListTooLong => FsError::PathTooLong,
        VfsError::CircularRename { .. } | VfsError::TooManyLinks => FsError::LoopDetected,
        VfsError::Unsupported => FsError::NotImplemented,
        other => {
            tracing::error!(error = %other, "WebDAV VFS operation failed");
            FsError::GeneralFailure
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanosecond_time_round_trip() {
        let nanos = 1_700_000_000_123_456_789;
        assert_eq!(
            system_time_nanos(system_time(nanos).unwrap()).unwrap(),
            nanos
        );
    }

    #[test]
    fn seek_rejects_before_start() {
        assert_eq!(checked_seek(3, -4), Err(FsError::Forbidden));
        assert_eq!(checked_seek(3, -3), Ok(0));
    }

    #[test]
    fn if_match_uses_metadata_version() {
        let attr = FileAttr {
            ino: 7,
            size: 11,
            blocks: 1,
            kind: FileType::File,
            mode: 0,
            rdev: 0,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 2,
            ctime: 3,
            nlink: 1,
        };
        let tag = format!("\"{}\"", etag_value(&attr));
        assert!(if_match_satisfied(&tag, Some(&attr)));
        assert!(if_match_satisfied("*", Some(&attr)));
        assert!(!if_match_satisfied("\"stale\"", Some(&attr)));
        assert!(!if_match_satisfied("*", None));
    }

    #[test]
    fn lock_shards_group_descendants() {
        assert_eq!(lock_shard("/docs"), lock_shard("/docs/a/b"));
    }
}
