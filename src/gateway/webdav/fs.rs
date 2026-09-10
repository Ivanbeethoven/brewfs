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
use futures_util::FutureExt;
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
        let entries = self
            .read_children(&dir)
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
            if let Ok(attr) = self.vfs.stat(&path).await
                && system_time(attr.mtime).is_ok_and(|mtime| mtime < cutoff)
            {
                let _ = self.vfs.unlink(&path).await;
            }
        }
        Ok(())
    }

    async fn read_children(
        &self,
        path: &str,
    ) -> Result<Vec<crate::meta::store::DirEntry>, FsError> {
        let attr = self.vfs.stat(path).await.map_err(map_vfs_error)?;
        if attr.kind != FileType::Dir {
            return Err(FsError::Forbidden);
        }
        self.read_children_ino(attr.ino).await
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

    async fn validate_path(&self, path: &str) -> Result<(), FsError> {
        if path == "/" {
            return Ok(());
        }

        let components: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let root = self.vfs.stat("/").await.map_err(map_vfs_error)?;
        let mut parent_ino = root.ino;
        for (index, component) in components.iter().enumerate() {
            let Some(entry) = self
                .read_children_ino(parent_ino)
                .await?
                .into_iter()
                .find(|entry| entry.name == *component)
            else {
                return Ok(());
            };
            if entry.kind == FileType::Symlink {
                return Err(FsError::Forbidden);
            }
            if index + 1 < components.len() {
                if entry.kind != FileType::Dir {
                    return Ok(());
                }
                parent_ino = entry.ino;
            }
        }
        Ok(())
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
        let _ = self.vfs.unlink(path).await;
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
        self.validate_path(&path).await?;
        if options.write || options.create || options.create_new || options.truncate {
            ensure_mutable(&path)?;
        }
        let mutation_guard = if options.write || options.create || options.create_new {
            Some(self.lock_for(&path).lock_owned().await)
        } else {
            None
        };

        let existing = match self.vfs.stat(&path).await {
            Ok(attr) => Some(attr),
            Err(VfsError::NotFound { .. }) => None,
            Err(error) => return Err(map_vfs_error(error)),
        };
        if options.create_new && existing.is_some() {
            return Err(FsError::Exists);
        }
        if existing
            .as_ref()
            .is_some_and(|attr| attr.kind != FileType::File)
        {
            return Err(FsError::Forbidden);
        }
        if existing.is_none() && !options.create {
            return Err(FsError::NotFound);
        }

        let atomic = self.atomic_put && options.write && !super::current_request_is_lock();
        let (storage_path, staging_path) = if atomic {
            let staging = self.reserve_staging();
            if let Err(error) = self
                .vfs
                .create_file_in_existing_dir_err(&staging, true)
                .await
            {
                self.active_staging.remove(&staging);
                return Err(map_vfs_error(error));
            }
            let staging_attr = match self.vfs.stat(&staging).await {
                Ok(attr) => attr,
                Err(error) => {
                    self.discard_staging(&staging).await;
                    return Err(map_vfs_error(error));
                }
            };
            if let Some(source) = existing.as_ref() {
                if !options.truncate
                    && let Err(error) = self.copy_exact(source, staging_attr.ino).await
                {
                    self.discard_staging(&staging).await;
                    return Err(error);
                }
                if let Err(error) = self.copy_dead_props(source.ino, staging_attr.ino).await {
                    self.discard_staging(&staging).await;
                    return Err(error);
                }
            }
            (staging.clone(), Some(staging))
        } else {
            if existing.is_none() {
                self.vfs
                    .create_file_in_existing_dir_err(&path, options.create_new)
                    .await
                    .map_err(map_vfs_error)?;
            }
            if options.truncate {
                self.vfs.truncate(&path, 0).await.map_err(map_vfs_error)?;
            }
            (path.clone(), None)
        };

        let attr = match self.vfs.stat(&storage_path).await {
            Ok(attr) => attr,
            Err(error) => {
                if let Some(staging) = staging_path.as_deref() {
                    self.discard_staging(staging).await;
                }
                return Err(map_vfs_error(error));
            }
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
            storage_path,
            staging_path,
            active_staging: self.active_staging.clone(),
            guard: Some(guard),
            mutation_guard,
            position,
            logical_size: attr.size,
            append: options.append,
            create_new: options.create_new,
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
            self.validate_path(&path).await?;
            let entries = self.read_children(&path).await?;
            let mut result: Vec<FsResult<Box<dyn DavDirEntry>>> = Vec::new();
            for entry in entries {
                if path == "/" && entry.name == ".brewfs.sys" {
                    continue;
                }
                let child = if path == "/" {
                    format!("/{}", entry.name)
                } else {
                    format!("{path}/{}", entry.name)
                };
                match self.vfs.stat(&child).await {
                    Ok(attr) => result.push(Ok(Box::new(BrewFsDavDirEntry {
                        name: entry.name.into_bytes(),
                        attr,
                    }))),
                    Err(error) => result.push(Err(map_vfs_error(error))),
                }
            }
            Ok(Box::pin(futures_util::stream::iter(result)) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        async move {
            let path = to_vfs_path(path)?;
            self.validate_path(&path).await?;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
            Ok(Box::new(BrewFsDavMetaData(attr)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            if self.vfs.stat(&path).await.is_ok() {
                return Err(FsError::Exists);
            }
            self.vfs.mkdir_err(&path).await.map_err(map_vfs_error)?;
            Ok(())
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            let result = self.vfs.rmdir(&path).await;
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
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            self.vfs.unlink(&path).await.map_err(map_vfs_error)
        }
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let from = to_vfs_path(from)?;
            let to = to_vfs_path(to)?;
            self.validate_path(&from).await?;
            self.validate_path(&to).await?;
            ensure_mutable(&from)?;
            ensure_mutable(&to)?;
            let _guards = self.lock_paths(&from, &to).await;
            self.vfs
                .rename_noreplace(&from, &to)
                .await
                .map_err(map_vfs_error)
        }
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        async move {
            let from = to_vfs_path(from)?;
            let to = to_vfs_path(to)?;
            self.validate_path(&from).await?;
            self.validate_path(&to).await?;
            ensure_mutable(&to)?;
            let _guards = self.lock_paths(&from, &to).await;
            let source = self.vfs.stat(&from).await.map_err(map_vfs_error)?;
            if source.kind != FileType::File {
                return Err(FsError::Forbidden);
            }
            let staging = self.reserve_staging();
            let result = async {
                let destination_ino = self
                    .vfs
                    .create_file_in_existing_dir_err(&staging, true)
                    .await
                    .map_err(map_vfs_error)?;
                self.copy_exact(&source, destination_ino).await?;
                self.copy_dead_props(source.ino, destination_ino).await?;
                self.vfs
                    .rename_noreplace(&staging, &to)
                    .await
                    .map_err(map_vfs_error)
            }
            .await;
            if result.is_err() {
                let _ = self.vfs.unlink(&staging).await;
            }
            self.active_staging.remove(&staging);
            result
        }
        .boxed()
    }

    fn set_accessed<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
        async move {
            let path = to_vfs_path(path)?;
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
            self.vfs
                .set_attr(
                    attr.ino,
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
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
            self.vfs
                .set_attr(
                    attr.ino,
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
            self.validate_path(&path).await?;
            ensure_mutable(&path)?;
            let _guard = self.lock_for(&path).lock_owned().await;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
            let raw = self.read_dead_props(attr.ino).await?;
            let (encoded, statuses) = props::apply(raw.as_deref(), patch)?;
            if let Some(encoded) = encoded {
                self.vfs
                    .set_xattr_ino(attr.ino, XATTR_DEAD_PROPS, &encoded, 0)
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
            self.validate_path(&path).await?;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
            let raw = self.read_dead_props(attr.ino).await?;
            props::list(raw.as_deref(), do_content)
        }
        .boxed()
    }

    fn get_prop<'a>(&'a self, path: &'a DavPath, prop: DavProp) -> FsFuture<'a, Vec<u8>> {
        async move {
            let path = to_vfs_path(path)?;
            self.validate_path(&path).await?;
            let attr = self.vfs.stat(&path).await.map_err(map_vfs_error)?;
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
    storage_path: String,
    staging_path: Option<String>,
    active_staging: Arc<DashSet<String>>,
    guard: Option<FileGuard<S, MetaClient<dyn MetaStore>>>,
    mutation_guard: Option<OwnedMutexGuard<()>>,
    position: u64,
    logical_size: u64,
    append: bool,
    create_new: bool,
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
            let _ = self.vfs.unlink(&path).await;
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
            let path = if self.staging_path.is_some() {
                &self.storage_path
            } else {
                &self.target_path
            };
            let mut attr = self.vfs.stat(path).await.map_err(map_vfs_error)?;
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
                let result = if self.create_new {
                    self.vfs.rename_noreplace(&staging, &self.target_path).await
                } else {
                    self.vfs.rename(&staging, &self.target_path).await
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
                    let _ = vfs.unlink(&path).await;
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
        Some(format!(
            "{:x}-{:x}-{:x}",
            self.0.ino, self.0.mtime, self.0.size
        ))
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
    fn lock_shards_group_descendants() {
        assert_eq!(lock_shard("/docs"), lock_shard("/docs/a/b"));
    }
}
