//! S3 object layer on top of the BrewFS VFS.
//!
//! Implements the `s3s::S3` trait (see `doc/protocols/s3-gateway.md` §4 for
//! the mapping table). All object data lives in the volume as regular files;
//! protocol metadata (etag, content-type, user metadata, directory-object
//! markers) lives in xattrs; multipart state lives under
//! `/.brewfs.sys/s3/`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use s3s::dto::*;
use s3s::{S3, S3Error, S3Request, S3Response, S3Result, s3_error};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::chunk::store::BlockStore;
use crate::gateway::s3::list::{ListCollector, ListQuery};
use crate::gateway::s3::multipart::{
    self, UploadMeta, multipart_etag, new_upload_id, parse_part_name,
};
use crate::gateway::s3::path::{self, BucketMode, PathError};
use crate::meta::MetaStore;
use crate::meta::client::MetaClient;
use crate::meta::store::{DirEntry, FileAttr, FileType};
use crate::vfs::fs::VFS;

/// Default content type when none is stored or provided.
const DEFAULT_CONTENT_TYPE: &str = "binary/octet-stream";

/// xattr carrying the object ETag.
pub const XATTR_ETAG: &str = "brewfs.s3.etag";
/// xattr carrying content-type + user metadata as JSON.
pub const XATTR_META: &str = "brewfs.s3.meta";
/// xattr marking an explicit S3 directory object.
pub const XATTR_DIROBJ: &str = "brewfs.s3.dirobj";

/// Read chunk size for streaming copies and GET bodies.
const STREAM_CHUNK: u64 = 4 * 1024 * 1024;

/// Maximum directory recursion depth during listings (defensive bound).
const MAX_WALK_DEPTH: usize = 64;

/// Name of the hidden system directory at the volume root.
const SYS_DIR_NAME: &str = ".brewfs.sys";

/// JSON payload stored in the `brewfs.s3.meta` xattr.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ObjectMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<HashMap<String, String>>,
}

/// Behavior options of the S3 gateway.
#[derive(Debug, Clone)]
pub struct S3Options {
    pub bucket_mode: BucketMode,
    pub hide_dir_objects: bool,
}

/// The S3 object layer over one BrewFS volume.
pub struct BrewFsS3<S: BlockStore + Send + Sync + 'static> {
    vfs: VFS<S, MetaClient<dyn MetaStore>>,
    opts: S3Options,
    /// Per-(bucket,key) write serialization within this gateway instance.
    locks: DashMap<String, Arc<Mutex<()>>>,
}

impl<S> BrewFsS3<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    pub fn new(vfs: VFS<S, MetaClient<dyn MetaStore>>, opts: S3Options) -> Self {
        Self {
            vfs,
            opts,
            locks: DashMap::new(),
        }
    }

    /// Grants access to the underlying VFS (used by the cleanup task).
    pub fn vfs(&self) -> &VFS<S, MetaClient<dyn MetaStore>> {
        &self.vfs
    }

    fn lock_for(&self, bucket: &str, key: &str) -> Arc<Mutex<()>> {
        let id = format!("{bucket}/{key}");
        Arc::clone(self.locks.entry(id).or_default().value())
    }

    // ---- error mapping ----------------------------------------------------

    fn err(e: crate::vfs::error::VfsError) -> S3Error {
        use crate::vfs::error::VfsError;
        match e {
            VfsError::NotFound { .. } => s3_error!(NoSuchKey, "object not found"),
            VfsError::AlreadyExists { .. } => {
                s3_error!(BucketAlreadyExists, "target already exists")
            }
            VfsError::NotADirectory { .. } => {
                s3_error!(InvalidArgument, "a path component is not a directory")
            }
            VfsError::IsADirectory { .. } => s3_error!(NoSuchKey, "is a directory"),
            VfsError::DirectoryNotEmpty { .. } => s3_error!(BucketNotEmpty, "not empty"),
            VfsError::PermissionDenied { .. } => s3_error!(AccessDenied, "permission denied"),
            VfsError::InvalidInput => s3_error!(InvalidArgument, "invalid input"),
            VfsError::ReadOnlyFilesystem { .. } => s3_error!(AccessDenied, "read-only"),
            other => s3_error!(InternalError, "vfs error: {other}"),
        }
    }

    fn path_err(e: PathError) -> S3Error {
        match e {
            PathError::InvalidBucket(b) => s3_error!(NoSuchBucket, "bucket not found: {b}"),
            PathError::InvalidKey(k) => s3_error!(InvalidArgument, "invalid object key: {k}"),
        }
    }

    // ---- helpers ------------------------------------------------------------

    async fn stat_bucket_root(&self, bucket: &str) -> S3Result<FileAttr> {
        let root = path::bucket_root(&self.opts.bucket_mode, bucket)
            .map_err(Self::path_err)?;
        let attr = self.vfs.stat(&root).await.map_err(Self::err)?;
        if attr.kind != FileType::Dir {
            return Err(s3_error!(NoSuchBucket, "bucket not found: {bucket}"));
        }
        Ok(attr)
    }

    async fn read_dir_entries(&self, dir_path: &str) -> S3Result<Vec<DirEntry>> {
        let attr = match self.vfs.stat(dir_path).await {
            Ok(a) => a,
            Err(crate::vfs::error::VfsError::NotFound { .. }) => return Ok(Vec::new()),
            Err(e) => return Err(Self::err(e)),
        };
        if attr.kind != FileType::Dir {
            return Ok(Vec::new());
        }
        let fh = self.vfs.opendir(attr.ino).await.map_err(Self::err)?;
        let entries = self.vfs.readdir(fh, 0).unwrap_or_default();
        self.vfs.closedir(fh).map_err(Self::err)?;
        Ok(entries)
    }

    async fn get_xattr(&self, ino: i64, name: &str) -> Option<String> {
        match self.vfs.get_xattr_ino(ino, name).await {
            Ok(Some(bytes)) => String::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    async fn set_xattr(&self, ino: i64, name: &str, value: &str) -> S3Result<()> {
        self.vfs
            .set_xattr_ino(ino, name, value.as_bytes(), 0)
            .await
            .map_err(Self::err)
    }

    async fn read_etag(&self, ino: i64) -> Option<String> {
        self.get_xattr(ino, XATTR_ETAG).await
    }

    async fn read_object_meta(&self, ino: i64) -> ObjectMeta {
        self.get_xattr(ino, XATTR_META)
            .await
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    async fn is_dir_object(&self, ino: i64) -> bool {
        self.get_xattr(ino, XATTR_DIROBJ).await.is_some()
    }

    /// ETag reported for objects written outside the gateway (no stored etag).
    fn fallback_etag(attr: &FileAttr) -> String {
        format!("{:x}-{:x}", attr.ino, attr.mtime)
    }

    fn mtime_timestamp(attr: &FileAttr) -> Option<Timestamp> {
        // VFS file attrs carry mtime in nanoseconds.
        Some(Timestamp::from(UNIX_EPOCH + Duration::from_nanos(
            attr.mtime.max(0) as u64,
        )))
    }

    /// Streams a blob into `dst` (created/truncated), returning size and MD5.
    async fn write_blob(
        &self,
        dst: &str,
        body: &mut StreamingBlob,
    ) -> S3Result<(u64, String)> {
        if self.vfs.exists(dst).await {
            self.vfs.unlink(dst).await.map_err(Self::err)?;
        }
        let ino = self.vfs.create_file(dst).await.map_err(Self::err)?;
        let attr = self.vfs.stat(dst).await.map_err(Self::err)?;
        let guard = self
            .vfs
            .open_guard(ino, attr, false, true)
            .await
            .map_err(Self::err)?;

        let mut offset: u64 = 0;
        let mut md = md5::Context::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| s3_error!(InternalError, "body read: {e}"))?;
            if !chunk.is_empty() {
                guard
                    .write(offset, &chunk)
                    .await
                    .map_err(Self::err)?;
                md.consume(&chunk);
                offset += chunk.len() as u64;
            }
        }
        guard.close().await.map_err(Self::err)?;
        let etag = format!("{:x}", md.compute());
        Ok((offset, etag))
    }

    /// Reads a byte range of a file as a streaming blob. Chunks are produced
    /// by a spawned task and forwarded through an mpsc channel (the channel
    /// receiver is `Send + Sync`, as required by `StreamingBlob::wrap`).
    fn read_stream(
        &self,
        attr: FileAttr,
        offset: u64,
        length: u64,
    ) -> S3Result<StreamingBlob> {
        let ino = attr.ino;
        let vfs = self.vfs.clone();
        let (mut tx, rx) = futures::channel::mpsc::channel::<
            Result<bytes::Bytes, std::io::Error>,
        >(4);
        tokio::spawn(async move {
            let mut offset = offset;
            let mut remaining = length;
            while remaining > 0 {
                let len = remaining.min(STREAM_CHUNK) as usize;
                // Open a short-lived read guard per chunk; the VFS read path
                // caches open handles, so this stays cheap.
                let guard = match vfs.stat_ino(ino).await {
                    Some(a) => vfs.open_guard(ino, a, true, false).await,
                    None => {
                        let _ = tx
                            .try_send(Err(std::io::Error::other(
                                "object deleted during read",
                            )));
                        break;
                    }
                };
                let guard = match guard {
                    Ok(g) => g,
                    Err(e) => {
                        let _ = tx
                            .try_send(Err(std::io::Error::other(format!("open: {e}"))));
                        break;
                    }
                };
                match guard.read(offset, len).await {
                    Ok(data) => {
                        let n = data.len() as u64;
                        drop(guard);
                        if tx
                            .try_send(Ok(bytes::Bytes::from(data)))
                            .is_err()
                        {
                            break;
                        }
                        offset += n;
                        remaining -= n;
                    }
                    Err(e) => {
                        drop(guard);
                        let _ = tx
                            .try_send(Err(std::io::Error::other(format!("read: {e}"))));
                        break;
                    }
                }
            }
        });
        Ok(StreamingBlob::wrap(rx))
    }

    /// Copies `src` into a fresh tmp file and renames it to `dst` (atomic publish).
    async fn copy_object_data(&self, src: &str, dst: &str) -> S3Result<(u64, FileAttr)> {
        let src_attr = self.vfs.stat(src).await.map_err(Self::err)?;
        let tmp = format!("{}/{}", multipart::tmp_dir(), new_upload_id());
        if self.vfs.exists(&tmp).await {
            self.vfs.unlink(&tmp).await.map_err(Self::err)?;
        }
        let tmp_ino = self.vfs.create_file(&tmp).await.map_err(Self::err)?;
        let tmp_attr = self.vfs.stat(&tmp).await.map_err(Self::err)?;
        let dst_guard = self
            .vfs
            .open_guard(tmp_ino, tmp_attr, false, true)
            .await
            .map_err(Self::err)?;
        let src_guard = self
            .vfs
            .open_guard(src_attr.ino, src_attr.clone(), true, false)
            .await
            .map_err(Self::err)?;

        let mut offset: u64 = 0;
        while offset < src_attr.size {
            let len = (src_attr.size - offset).min(STREAM_CHUNK) as usize;
            let chunk = src_guard.read(offset, len).await.map_err(Self::err)?;
            if chunk.is_empty() {
                break;
            }
            dst_guard.write(offset, &chunk).await.map_err(Self::err)?;
            offset += chunk.len() as u64;
        }
        src_guard.close().await.map_err(Self::err)?;
        dst_guard.close().await.map_err(Self::err)?;

        if let Some(parent) = std::path::Path::new(dst).parent() {
            let parent = parent.to_string_lossy().to_string();
            if !parent.is_empty() {
                self.vfs.mkdir_p(&parent).await.map_err(Self::err)?;
            }
        }
        if self.vfs.exists(dst).await {
            let attr = self.vfs.stat(dst).await.map_err(Self::err)?;
            if attr.kind == FileType::Dir {
                let _ = self.vfs.unlink(&tmp).await;
                return Err(s3_error!(InvalidArgument, "target is a directory"));
            }
        }
        self.vfs.rename(&tmp, dst).await.map_err(Self::err)?;
        let final_attr = self.vfs.stat(dst).await.map_err(Self::err)?;
        Ok((final_attr.size, final_attr))
    }

    /// Removes empty implicit parent directories left behind after a delete,
    /// stopping at the bucket root, at the volume root, or at the first
    /// directory object / non-empty directory.
    async fn prune_empty_parents(&self, key_path: &str, bucket_root: &str) {
        let mut current = key_path.to_string();
        while let Some(parent) = std::path::Path::new(&current)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
        {
            if parent.is_empty() || parent == "/" || parent == bucket_root {
                return;
            }
            if parent.ends_with(SYS_DIR_NAME) || parent.contains(SYS_DIR_NAME) {
                return;
            }
            let Ok(attr) = self.vfs.stat(&parent).await else {
                return;
            };
            if attr.kind != FileType::Dir {
                return;
            }
            if self.is_dir_object(attr.ino).await {
                return;
            }
            let Ok(entries) = self.read_dir_entries(&parent).await else {
                return;
            };
            if !entries.is_empty() {
                return;
            }
            if self.vfs.rmdir(&parent).await.is_err() {
                return;
            }
            current = parent;
        }
    }

    /// Deletes one object; shared by DeleteObject and DeleteObjects.
    async fn delete_object_internal(&self, bucket: &str, key: &str) -> S3Result<()> {
        let mode = &self.opts.bucket_mode;
        let bucket_root = path::bucket_root(mode, bucket).map_err(Self::path_err)?;
        let target = path::object_path(mode, bucket, key).map_err(Self::path_err)?;

        if target == bucket_root || target == "/" {
            // Deleting the bucket root via the object API is not allowed.
            return Err(s3_error!(InvalidArgument, "cannot delete bucket root"));
        }

        let attr = match self.vfs.stat(&target).await {
            Ok(a) => a,
            Err(crate::vfs::error::VfsError::NotFound { .. }) => return Ok(()),
            Err(e) => return Err(Self::err(e)),
        };

        if attr.kind == FileType::Dir {
            // Directory object delete: drop the marker; remove the directory
            // itself only if it is empty.
            let _ = self.vfs.remove_xattr_ino(attr.ino, XATTR_DIROBJ).await;
            let entries = self.read_dir_entries(&target).await?;
            if entries.is_empty() {
                self.vfs.rmdir(&target).await.map_err(Self::err)?;
            }
            return Ok(());
        }

        self.vfs.unlink(&target).await.map_err(Self::err)?;
        self.prune_empty_parents(&target, &bucket_root).await;
        Ok(())
    }

    /// Resolves an object and decides whether it is visible as an object
    /// (file, or directory with the dir-object marker when `allow_dir_obj`).
    async fn resolve_object(
        &self,
        mode: &BucketMode,
        bucket: &str,
        key: &str,
    ) -> S3Result<(String, FileAttr, bool)> {
        self.stat_bucket_root(bucket).await?;
        let target = path::object_path(mode, bucket, key).map_err(Self::path_err)?;
        if target == "/" {
            return Err(s3_error!(NoSuchKey, "no such object"));
        }
        let attr = match self.vfs.stat(&target).await {
            Ok(a) => a,
            Err(e) => return Err(Self::err(e)),
        };
        if attr.kind == FileType::Dir {
            let dirobj = key.ends_with('/') && self.is_dir_object(attr.ino).await;
            if !dirobj {
                return Err(s3_error!(NoSuchKey, "no such object"));
            }
            return Ok((target, attr, true));
        }
        Ok((target, attr, false))
    }

    // ---- listing ------------------------------------------------------------

    #[async_recursion::async_recursion]
    async fn walk_dir(
        &self,
        dir_path: &str,
        parent_key: &str,
        at_root: bool,
        collector: &mut ListCollector,
        depth: usize,
    ) -> S3Result<()> {
        if depth > MAX_WALK_DEPTH {
            return Ok(());
        }
        let entries = self.read_dir_entries(dir_path).await?;
        for entry in entries {
            if at_root && entry.name == SYS_DIR_NAME {
                continue;
            }
            let child_path = format!("{dir_path}/{}", entry.name);
            let is_dir = entry.kind == FileType::Dir;
            let mut dirobj = false;
            let mut etag = None;
            let (size, mtime) = match self.vfs.stat_ino(entry.ino).await {
                Some(a) => (a.size, a.mtime),
                None => (0, 0),
            };
            if is_dir {
                dirobj = self.is_dir_object(entry.ino).await;
            } else {
                etag = self.read_etag(entry.ino).await;
            }
            collector.push_entry(parent_key, &entry.name, is_dir, size, mtime, dirobj, etag);
            if is_dir && collector.should_descend(parent_key, &entry.name) {
                let child_key = format!("{parent_key}{}/", entry.name);
                self.walk_dir(&child_path, &child_key, false, collector, depth + 1)
                    .await?;
            }
        }
        Ok(())
    }

    async fn list_keys(&self, bucket: &str, query: ListQuery) -> S3Result<crate::gateway::s3::list::ListResult> {
        let mode = &self.opts.bucket_mode;
        let root = path::bucket_root(mode, bucket).map_err(Self::path_err)?;
        self.stat_bucket_root(bucket).await?;
        let mut collector = ListCollector::new(query);
        self.walk_dir(&root, "", true, &mut collector, 0).await?;
        Ok(collector.finish())
    }

    // ---- multipart ------------------------------------------------------------

    async fn read_upload_meta(&self, upload_id: &str) -> S3Result<UploadMeta> {
        let target = UploadMeta::target_path(upload_id);
        match self.vfs.stat(&target).await {
            Ok(attr) => {
                let size = attr.size;
                let guard = self
                    .vfs
                    .open_guard(attr.ino, attr, true, false)
                    .await
                    .map_err(Self::err)?;
                let data = guard.read(0, size as usize).await.map_err(Self::err)?;
                guard.close().await.map_err(Self::err)?;
                serde_json::from_slice(&data)
                    .map_err(|e| s3_error!(InternalError, "corrupt upload meta: {e}"))
            }
            Err(crate::vfs::error::VfsError::NotFound { .. }) => {
                Err(s3_error!(NoSuchUpload, "no such upload"))
            }
            Err(e) => Err(Self::err(e)),
        }
    }
}

#[async_trait]
impl<S> S3 for BrewFsS3<S>
where
    S: BlockStore + Send + Sync + 'static,
{
    async fn create_bucket(&self, req: S3Request<CreateBucketInput>) -> S3Result<S3Response<CreateBucketOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        match &self.opts.bucket_mode {
            BucketMode::Single { bucket: expected } => {
                if bucket == *expected {
                    return Err(s3_error!(BucketAlreadyOwnedByYou, "bucket already exists"));
                }
                Err(s3_error!(
                    InvalidBucketName,
                    "this gateway serves a single bucket"
                ))
            }
            BucketMode::Multi => {
                if !path::is_valid_bucket_name(&bucket) {
                    return Err(s3_error!(InvalidBucketName, "invalid bucket name"));
                }
                let dir = format!("/{bucket}");
                match self.vfs.mkdir_err(&dir).await {
                    Ok(_) => Ok(S3Response::new(CreateBucketOutput::default())),
                    Err(crate::vfs::error::VfsError::AlreadyExists { .. }) => Err(s3_error!(
                        BucketAlreadyOwnedByYou,
                        "bucket already exists"
                    )),
                    Err(e) => Err(Self::err(e)),
                }
            }
        }
    }

    async fn delete_bucket(&self, req: S3Request<DeleteBucketInput>) -> S3Result<S3Response<DeleteBucketOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        match &self.opts.bucket_mode {
            BucketMode::Single { .. } => Err(s3_error!(
                MethodNotAllowed,
                "cannot delete the volume bucket in single-bucket mode"
            )),
            BucketMode::Multi => {
                let dir = format!("/{bucket}");
                match self.vfs.rmdir(&dir).await {
                    Ok(_) => Ok(S3Response::new(DeleteBucketOutput::default())),
                    Err(crate::vfs::error::VfsError::NotFound { .. }) => {
                        Err(s3_error!(NoSuchBucket, "no such bucket"))
                    }
                    Err(crate::vfs::error::VfsError::DirectoryNotEmpty { .. }) => {
                        Err(s3_error!(BucketNotEmpty, "bucket is not empty"))
                    }
                    Err(e) => Err(Self::err(e)),
                }
            }
        }
    }

    async fn head_bucket(&self, req: S3Request<HeadBucketInput>) -> S3Result<S3Response<HeadBucketOutput>> {
        self.stat_bucket_root(req.input.bucket.as_str()).await?;
        Ok(S3Response::new(HeadBucketOutput::default()))
    }

    async fn list_buckets(&self, _req: S3Request<ListBucketsInput>) -> S3Result<S3Response<ListBucketsOutput>> {
        let owner = Owner::default();
        let buckets = match &self.opts.bucket_mode {
            BucketMode::Single { bucket } => vec![Bucket {
                name: Some(bucket.clone()),
                creation_date: None,
                bucket_region: None,
            }],
            BucketMode::Multi => {
                let mut out = Vec::new();
                for entry in self.read_dir_entries("/").await? {
                    if entry.name == SYS_DIR_NAME || entry.kind != FileType::Dir {
                        continue;
                    }
                    if path::is_valid_bucket_name(&entry.name) {
                        out.push(Bucket {
                            name: Some(entry.name.clone()),
                            creation_date: None,
                            bucket_region: None,
                        });
                    }
                }
                out
            }
        };
        Ok(S3Response::new(ListBucketsOutput {
            buckets: Some(buckets),
            owner: Some(owner),
            ..Default::default()
        }))
    }

    async fn get_bucket_location(
        &self,
        _req: S3Request<GetBucketLocationInput>,
    ) -> S3Result<S3Response<GetBucketLocationOutput>> {
        Ok(S3Response::new(GetBucketLocationOutput::default()))
    }

    async fn put_object(&self, mut req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
        let input = &mut req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        let mode = &self.opts.bucket_mode;
        self.stat_bucket_root(&bucket).await?;
        let target = path::object_path(mode, &bucket, &key).map_err(Self::path_err)?;
        if target == "/" {
            return Err(s3_error!(InvalidArgument, "empty key"));
        }

        let content_type = input
            .content_type
            .as_ref()
            .map(|ct| ct.as_str().to_string());
        let metadata: HashMap<String, String> = input
            .metadata
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let lock = self.lock_for(&bucket, &key);
        let _guard = lock.lock().await;

        // Explicit directory object: PUT with a key ending in '/'.
        if key.ends_with('/') {
            self.vfs.mkdir_p(&target).await.map_err(Self::err)?;
            let attr = self.vfs.stat(&target).await.map_err(Self::err)?;
            self.set_xattr(attr.ino, XATTR_DIROBJ, "").await?;
            let meta = ObjectMeta {
                content_type,
                metadata: if metadata.is_empty() {
                    None
                } else {
                    Some(metadata)
                },
            };
            self.set_xattr(attr.ino, XATTR_META, &serde_json::to_string(&meta).unwrap())
                .await?;
            let etag = format!("{:x}", md5::Context::new().compute());
            self.set_xattr(attr.ino, XATTR_ETAG, &etag).await?;
            return Ok(S3Response::new(PutObjectOutput {
                e_tag: Some(ETag::Strong(etag)),
                ..Default::default()
            }));
        }

        let body = input
            .body
            .take()
            .ok_or_else(|| s3_error!(InvalidArgument, "missing body"))?;
        let mut body = body;

        let tmp = format!("{}/{}", multipart::tmp_dir(), new_upload_id());
        let (size, etag) = self.write_blob(&tmp, &mut body).await?;

        if let Some(parent) = std::path::Path::new(&target).parent() {
            let parent = parent.to_string_lossy().to_string();
            if !parent.is_empty() {
                self.vfs.mkdir_p(&parent).await.map_err(Self::err)?;
            }
        }
        if self.vfs.exists(&target).await {
            let attr = self.vfs.stat(&target).await.map_err(Self::err)?;
            if attr.kind == FileType::Dir {
                let _ = self.vfs.unlink(&tmp).await;
                return Err(s3_error!(InvalidArgument, "key maps to a directory"));
            }
        }
        self.vfs.rename(&tmp, &target).await.map_err(Self::err)?;
        let attr = self.vfs.stat(&target).await.map_err(Self::err)?;
        self.set_xattr(attr.ino, XATTR_ETAG, &etag).await?;
        let meta = ObjectMeta {
            content_type,
            metadata: if metadata.is_empty() {
                None
            } else {
                Some(metadata)
            },
        };
        self.set_xattr(attr.ino, XATTR_META, &serde_json::to_string(&meta).unwrap())
            .await?;

        tracing::debug!(bucket = %bucket, key = %key, size, "s3 put_object");
        Ok(S3Response::new(PutObjectOutput {
            e_tag: Some(ETag::Strong(etag)),
            ..Default::default()
        }))
    }

    async fn get_object(&self, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        let mode = &self.opts.bucket_mode;
        let (_target, attr, is_dir_obj) = self.resolve_object(mode, &bucket, &key).await?;

        let meta = self.read_object_meta(attr.ino).await;
        let etag = self
            .read_etag(attr.ino)
            .await
            .unwrap_or_else(|| Self::fallback_etag(&attr));

        // Directory objects are always empty.
        if is_dir_obj {
            return Ok(S3Response::new(GetObjectOutput {
                body: Some(StreamingBlob::from_bytes(Default::default())),
                content_length: Some(0),
                e_tag: Some(ETag::Strong(etag)),
                content_type: Some(meta.content_type.clone().map(ContentType::from).unwrap_or_else(|| ContentType::from(DEFAULT_CONTENT_TYPE))),
                last_modified: Self::mtime_timestamp(&attr),
                accept_ranges: Some(AcceptRanges::from("bytes")),
                ..Default::default()
            }));
        }

        // Range handling.
        let (offset, length) = match input.range.as_ref() {
            Some(range) => {
                let checked = range
                    .check(attr.size)
                    .map_err(|_| s3_error!(InvalidRange, "range not satisfiable"))?;
                (checked.start, checked.end - checked.start)
            }
            None => (0, attr.size),
        };

        let body = self.read_stream(attr.clone(), offset, length)?;
        Ok(S3Response::new(GetObjectOutput {
            body: Some(body),
            content_length: Some(length as i64),
            e_tag: Some(ETag::Strong(etag)),
            content_type: Some(meta.content_type.map(ContentType::from).unwrap_or_else(|| ContentType::from(DEFAULT_CONTENT_TYPE))),
            last_modified: Self::mtime_timestamp(&attr),
            accept_ranges: Some(AcceptRanges::from("bytes")),
            ..Default::default()
        }))
    }

    async fn head_object(&self, req: S3Request<HeadObjectInput>) -> S3Result<S3Response<HeadObjectOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        let mode = &self.opts.bucket_mode;
        let (_target, attr, _is_dir_obj) = self.resolve_object(mode, &bucket, &key).await?;

        let meta = self.read_object_meta(attr.ino).await;
        let etag = self
            .read_etag(attr.ino)
            .await
            .unwrap_or_else(|| Self::fallback_etag(&attr));
        let metadata = meta.metadata.map(|m| {
            m.into_iter()
                .map(|(k, v)| (k, MetadataValue::from(v)))
                .collect::<HashMap<String, MetadataValue>>()
        });

        Ok(S3Response::new(HeadObjectOutput {
            content_length: Some(attr.size as i64),
            e_tag: Some(ETag::Strong(etag)),
            content_type: Some(meta.content_type.map(ContentType::from).unwrap_or_else(|| ContentType::from(DEFAULT_CONTENT_TYPE))),
            last_modified: Self::mtime_timestamp(&attr),
            metadata,
            accept_ranges: Some(AcceptRanges::from("bytes")),
            ..Default::default()
        }))
    }

    async fn delete_object(&self, req: S3Request<DeleteObjectInput>) -> S3Result<S3Response<DeleteObjectOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        let lock = self.lock_for(&bucket, &key);
        let _guard = lock.lock().await;
        self.delete_object_internal(&bucket, &key).await?;
        Ok(S3Response::with_status(
            DeleteObjectOutput::default(),
            axum::http::StatusCode::NO_CONTENT,
        ))
    }

    async fn delete_objects(&self, req: S3Request<DeleteObjectsInput>) -> S3Result<S3Response<DeleteObjectsOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let objects = input.delete.objects;

        let mut deleted = Vec::new();
        let mut errors = Vec::new();
        for obj in objects {
            let key = obj.key.as_str().to_string();
            let lock = self.lock_for(&bucket, &key);
        let _guard = lock.lock().await;
            match self.delete_object_internal(&bucket, &key).await {
                Ok(()) => deleted.push(DeletedObject {
                    key: Some(obj.key),
                    ..Default::default()
                }),
                Err(e) => errors.push(s3s::dto::Error {
                    code: Some(e.code().as_str().to_string()),
                    key: Some(obj.key),
                    message: e.message().map(|m| m.to_string()),
                    ..Default::default()
                }),
            }
        }
        Ok(S3Response::new(DeleteObjectsOutput {
            deleted: if deleted.is_empty() { None } else { Some(deleted) },
            errors: if errors.is_empty() { None } else { Some(errors) },
            ..Default::default()
        }))
    }

    async fn copy_object(&self, req: S3Request<CopyObjectInput>) -> S3Result<S3Response<CopyObjectOutput>> {
        let input = req.input;
        let dst_bucket = input.bucket.as_str().to_string();
        let dst_key = input.key.as_str().to_string();
        let mode = &self.opts.bucket_mode;

        // Parse the copy source (bucket/key form; ARN forms unsupported).
        let (src_bucket, src_key) = match input.copy_source {
            CopySource::Bucket { bucket, key, .. } => (bucket.to_string(), key.to_string()),
            _ => {
                return Err(s3_error!(
                    NotImplemented,
                    "only bucket/key copy sources are supported"
                ))
            }
        };

        self.stat_bucket_root(&dst_bucket).await?;
        let src_path = path::object_path(mode, &src_bucket, &src_key).map_err(Self::path_err)?;
        let dst_path = path::object_path(mode, &dst_bucket, &dst_key).map_err(Self::path_err)?;
        if dst_path == "/" {
            return Err(s3_error!(InvalidArgument, "empty destination key"));
        }

        let src_attr = match self.vfs.stat(&src_path).await {
            Ok(a) => a,
            Err(_) => return Err(s3_error!(NoSuchKey, "source object not found")),
        };

        let lock = self.lock_for(&dst_bucket, &dst_key);
        let _guard = lock.lock().await;
        let (size, final_attr) = self.copy_object_data(&src_path, &dst_path).await?;

        // Copy or replace metadata according to the directive (default COPY).
        let replace = matches!(
            input.metadata_directive.as_ref().map(|d| d.as_str()),
            Some("REPLACE")
        );
        let meta = if replace {
            ObjectMeta {
                content_type: input.content_type.as_ref().map(|ct| ct.as_str().to_string()),
                metadata: input.metadata.as_ref().map(|m| {
                    m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
                }),
            }
        } else {
            self.read_object_meta(src_attr.ino).await
        };
        let etag = if replace {
            None
        } else {
            self.read_etag(src_attr.ino).await
        };
        let etag = etag.unwrap_or_else(|| Self::fallback_etag(&final_attr));

        self.set_xattr(final_attr.ino, XATTR_ETAG, &etag).await?;
        self.set_xattr(final_attr.ino, XATTR_META, &serde_json::to_string(&meta).unwrap())
            .await?;

        let last_modified = Self::mtime_timestamp(&final_attr);
        tracing::debug!(bucket = %dst_bucket, key = %dst_key, size, "s3 copy_object");
        Ok(S3Response::new(CopyObjectOutput {
            copy_object_result: Some(CopyObjectResult {
                e_tag: Some(ETag::Strong(etag)),
                last_modified,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    async fn list_objects(&self, req: S3Request<ListObjectsInput>) -> S3Result<S3Response<ListObjectsOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let query = ListQuery {
            prefix: input.prefix.as_ref().map(|p| p.as_str().to_string()).unwrap_or_default(),
            delimiter: input.delimiter.as_ref().map(|d| d.as_str().to_string()),
            start_after: input.marker.as_ref().map(|m| m.as_str().to_string()).unwrap_or_default(),
            max_keys: input
                .max_keys
                .map(|m| usize::try_from(m).unwrap_or(1000))
                .unwrap_or(1000),
        };
        let result = self.list_keys(&bucket, query).await?;

        let contents: Vec<Object> = result
            .objects
            .iter()
            .filter(|o| !o.is_dir || o.is_dir_object)
            .filter(|o| !(o.is_dir_object && self.opts.hide_dir_objects))
            .map(|o| Object {
                key: Some(o.key.clone()),
                size: Some(if o.is_dir_object { 0 } else { o.size as i64 }),
                last_modified: Some(Timestamp::from(
                    UNIX_EPOCH + Duration::from_nanos(o.mtime.max(0) as u64),
                )),
                e_tag: o.etag.clone().map(ETag::Strong),
                ..Default::default()
            })
            .collect();

        Ok(S3Response::new(ListObjectsOutput {
            name: Some(input.bucket),
            prefix: input.prefix,
            marker: input.marker,
            max_keys: input.max_keys,
            is_truncated: Some(result.is_truncated),
            next_marker: if result.is_truncated {
                Some(s3s::dto::NextMarker::from(result.next_marker))
            } else {
                None
            },
            contents: if contents.is_empty() { None } else { Some(contents) },
            common_prefixes: prefixes_to_dto(&result.common_prefixes),
            ..Default::default()
        }))
    }

    async fn list_objects_v2(&self, req: S3Request<ListObjectsV2Input>) -> S3Result<S3Response<ListObjectsV2Output>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let start_after = input
            .start_after
            .as_ref()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        let continuation = input
            .continuation_token
            .as_ref()
            .map(|t| t.as_str().to_string())
            .unwrap_or_default();
        let query = ListQuery {
            prefix: input.prefix.as_ref().map(|p| p.as_str().to_string()).unwrap_or_default(),
            delimiter: input.delimiter.as_ref().map(|d| d.as_str().to_string()),
            start_after: if continuation.is_empty() {
                start_after
            } else {
                continuation
            },
            max_keys: input
                .max_keys
                .map(|m| usize::try_from(m).unwrap_or(1000))
                .unwrap_or(1000),
        };
        let result = self.list_keys(&bucket, query).await?;

        let contents: Vec<Object> = result
            .objects
            .iter()
            .filter(|o| !o.is_dir || o.is_dir_object)
            .filter(|o| !(o.is_dir_object && self.opts.hide_dir_objects))
            .map(|o| Object {
                key: Some(o.key.clone()),
                size: Some(if o.is_dir_object { 0 } else { o.size as i64 }),
                last_modified: Some(Timestamp::from(
                    UNIX_EPOCH + Duration::from_nanos(o.mtime.max(0) as u64),
                )),
                e_tag: o.etag.clone().map(ETag::Strong),
                ..Default::default()
            })
            .collect();
        let count = contents.len() + result.common_prefixes.len();

        Ok(S3Response::new(ListObjectsV2Output {
            name: Some(input.bucket),
            prefix: input.prefix,
            max_keys: input.max_keys,
            key_count: Some(s3s::dto::KeyCount::from(count as i32)),
            continuation_token: input.continuation_token,
            start_after: input.start_after,
            is_truncated: Some(result.is_truncated),
            next_continuation_token: if result.is_truncated {
                Some(NextToken::from(result.next_marker))
            } else {
                None
            },
            contents: if contents.is_empty() { None } else { Some(contents) },
            common_prefixes: prefixes_to_dto(&result.common_prefixes),
            ..Default::default()
        }))
    }

    async fn create_multipart_upload(
        &self,
        req: S3Request<CreateMultipartUploadInput>,
    ) -> S3Result<S3Response<CreateMultipartUploadOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        if key.ends_with('/') {
            return Err(s3_error!(InvalidArgument, "multipart key must not end with '/'"));
        }
        self.stat_bucket_root(&bucket).await?;
        path::object_path(&self.opts.bucket_mode, &bucket, &key).map_err(Self::path_err)?;

        let upload_id = new_upload_id();
        let dir = multipart::upload_dir(&upload_id);
        self.vfs.mkdir_p(&dir).await.map_err(Self::err)?;

        let meta = UploadMeta {
            bucket: bucket.clone(),
            key: key.clone(),
            content_type: input
                .content_type
                .as_ref()
                .map(|ct| ct.as_str().to_string()),
            metadata: input.metadata.as_ref().map(|m| {
                m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
            }),
            initiated: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        };
        let target_path = UploadMeta::target_path(&upload_id);
        let ino = self.vfs.create_file(&target_path).await.map_err(Self::err)?;
        let attr = self.vfs.stat(&target_path).await.map_err(Self::err)?;
        let guard = self
            .vfs
            .open_guard(ino, attr, false, true)
            .await
            .map_err(Self::err)?;
        guard
            .write(0, serde_json::to_vec(&meta).unwrap().as_slice())
            .await
            .map_err(Self::err)?;
        guard.close().await.map_err(Self::err)?;

        Ok(S3Response::new(CreateMultipartUploadOutput {
            bucket: Some(input.bucket),
            key: Some(input.key),
            upload_id: Some(MultipartUploadId::from(upload_id)),
            ..Default::default()
        }))
    }

    async fn upload_part(&self, mut req: S3Request<UploadPartInput>) -> S3Result<S3Response<UploadPartOutput>> {
        let upload_id = req.input.upload_id.clone();
        let part_number = req.input.part_number;

        // Validate the upload exists and belongs to this bucket/key.
        self.read_upload_meta(&upload_id).await?;

        let dst = multipart::part_path(&upload_id, i64::from(part_number));
        let body = req
            .input
            .body
            .take()
            .ok_or_else(|| s3_error!(InvalidArgument, "missing body"))?;
        let mut body = body;
        let (_size, etag) = self.write_blob(&dst, &mut body).await?;

        let attr = self.vfs.stat(&dst).await.map_err(Self::err)?;
        self.set_xattr(attr.ino, XATTR_ETAG, &etag).await?;

        Ok(S3Response::new(UploadPartOutput {
            e_tag: Some(ETag::Strong(etag)),
            ..Default::default()
        }))
    }

    async fn list_parts(&self, req: S3Request<ListPartsInput>) -> S3Result<S3Response<ListPartsOutput>> {
        let input = req.input;
        let upload_id = input.upload_id.clone();
        let meta = self.read_upload_meta(&upload_id).await?;

        let dir = multipart::upload_dir(&upload_id);
        let mut parts: Vec<(i64, u64, i64, Option<String>)> = Vec::new();
        for entry in self.read_dir_entries(&dir).await? {
            if let Some(n) = parse_part_name(&entry.name) {
                if let Some(attr) = self.vfs.stat_ino(entry.ino).await {
                    let etag = self.read_etag(entry.ino).await;
                    parts.push((n, attr.size, attr.mtime, etag));
                }
            }
        }
        parts.sort_by_key(|p| p.0);

        Ok(S3Response::new(ListPartsOutput {
            bucket: Some(input.bucket),
            key: Some(ObjectKey::from(meta.key)),
            upload_id: Some(MultipartUploadId::from(upload_id)),
            parts: Some(
                parts
                    .into_iter()
                    .map(|(n, size, mtime, etag)| Part {
                        part_number: Some(n as i32),
                        size: Some(size as i64),
                        last_modified: Some(Timestamp::from(
                            UNIX_EPOCH + Duration::from_nanos(mtime.max(0) as u64),
                        )),
                        e_tag: etag.map(ETag::Strong),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }))
    }

    async fn complete_multipart_upload(
        &self,
        req: S3Request<CompleteMultipartUploadInput>,
    ) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let key = input.key.as_str().to_string();
        let upload_id = input.upload_id.clone();

        let meta = self.read_upload_meta(&upload_id).await?;
        if meta.bucket != bucket || meta.key != key {
            return Err(s3_error!(NoSuchUpload, "upload does not match bucket/key"));
        }

        let completed = input
            .multipart_upload
            .and_then(|m| m.parts)
            .ok_or_else(|| s3_error!(InvalidArgument, "missing parts list"))?;

        let lock = self.lock_for(&bucket, &key);
        let _guard = lock.lock().await;

        // Validate part ordering and etags.
        let mut expected = 1i32;
        let mut part_etags: Vec<String> = Vec::with_capacity(completed.len());
        let mut part_files: Vec<(i64, String)> = Vec::with_capacity(completed.len());
        for part in &completed {
            let n = part.part_number.unwrap_or(0);
            if n != expected {
                return Err(s3_error!(
                    InvalidPartOrder,
                    "parts must be ascending starting at 1 (got {n}, expected {expected})"
                ));
            }
            expected += 1;
            let src = multipart::part_path(&upload_id, i64::from(n));
            let attr = match self.vfs.stat(&src).await {
                Ok(a) => a,
                Err(_) => return Err(s3_error!(InvalidPart, "part {n} not uploaded")),
            };
            // Every part except the last must be at least 5 MiB.
            let is_last = std::ptr::eq(part, completed.last().unwrap());
            if !is_last && attr.size < 5 * 1024 * 1024 {
                return Err(s3_error!(EntityTooSmall, "part {n} is smaller than 5 MiB"));
            }
            let stored = self.read_etag(attr.ino).await;
            if let (Some(want), Some(have)) = (part.e_tag.as_ref(), stored.as_ref()) {
                if want.value() != have {
                    return Err(s3_error!(InvalidPart, "etag mismatch on part {n}"));
                }
            }
            part_etags.push(stored.unwrap_or_default());
            part_files.push((i64::from(n), src));
        }

        // Concatenate parts into a staging file, then publish atomically.
        let tmp = format!("{}/{}", multipart::tmp_dir(), new_upload_id());
        if self.vfs.exists(&tmp).await {
            self.vfs.unlink(&tmp).await.map_err(Self::err)?;
        }
        let tmp_ino = self.vfs.create_file(&tmp).await.map_err(Self::err)?;
        let tmp_attr = self.vfs.stat(&tmp).await.map_err(Self::err)?;
        let out_guard = self
            .vfs
            .open_guard(tmp_ino, tmp_attr, false, true)
            .await
            .map_err(Self::err)?;

        let mut offset: u64 = 0;
        for (_, src) in &part_files {
            let attr = self.vfs.stat(src).await.map_err(Self::err)?;
            let size = attr.size;
            let in_guard = self
                .vfs
                .open_guard(attr.ino, attr, true, false)
                .await
                .map_err(Self::err)?;
            let mut pos: u64 = 0;
            while pos < size {
                let len = (size - pos).min(STREAM_CHUNK) as usize;
                let chunk = in_guard.read(pos, len).await.map_err(Self::err)?;
                if chunk.is_empty() {
                    break;
                }
                out_guard.write(offset, &chunk).await.map_err(Self::err)?;
                offset += chunk.len() as u64;
                pos += chunk.len() as u64;
            }
            in_guard.close().await.map_err(Self::err)?;
        }
        out_guard.close().await.map_err(Self::err)?;

        let target = path::object_path(&self.opts.bucket_mode, &bucket, &key)
            .map_err(Self::path_err)?;
        if let Some(parent) = std::path::Path::new(&target).parent() {
            let parent = parent.to_string_lossy().to_string();
            if !parent.is_empty() {
                self.vfs.mkdir_p(&parent).await.map_err(Self::err)?;
            }
        }
        self.vfs.rename(&tmp, &target).await.map_err(Self::err)?;
        let final_attr = self.vfs.stat(&target).await.map_err(Self::err)?;

        let etag = multipart_etag(&part_etags);
        let object_meta = ObjectMeta {
            content_type: meta.content_type.clone(),
            metadata: meta.metadata.clone(),
        };
        self.set_xattr(final_attr.ino, XATTR_ETAG, &etag).await?;
        self.set_xattr(
            final_attr.ino,
            XATTR_META,
            &serde_json::to_string(&object_meta).unwrap(),
        )
        .await?;

        // Drop the upload state.
        let upload_dir = multipart::upload_dir(&upload_id);
        let _ = super::remove_dir_all_rec(&self.vfs, &upload_dir).await;

        let location = format!("/{bucket}/{key}");
        Ok(S3Response::new(CompleteMultipartUploadOutput {
            bucket: Some(BucketName::from(bucket)),
            key: Some(ObjectKey::from(key)),
            e_tag: Some(ETag::Strong(etag)),
            location: Some(Location::from(location)),
            ..Default::default()
        }))
    }

    async fn abort_multipart_upload(
        &self,
        req: S3Request<AbortMultipartUploadInput>,
    ) -> S3Result<S3Response<AbortMultipartUploadOutput>> {
        let input = req.input;
        let upload_id = input.upload_id.clone();
        self.read_upload_meta(&upload_id).await?;
        let dir = multipart::upload_dir(&upload_id);
        super::remove_dir_all_rec(&self.vfs, &dir)
            .await
            .map_err(Self::err)?;
        Ok(S3Response::new(AbortMultipartUploadOutput::default()))
    }

    async fn list_multipart_uploads(
        &self,
        req: S3Request<ListMultipartUploadsInput>,
    ) -> S3Result<S3Response<ListMultipartUploadsOutput>> {
        let input = req.input;
        let bucket = input.bucket.as_str().to_string();
        let prefix = input
            .prefix
            .as_ref()
            .map(|p| p.as_str().to_string())
            .unwrap_or_default();

        let mut uploads = Vec::new();
        for hh in self.read_dir_entries(&multipart::uploads_dir()).await? {
            let hh_dir = format!("{}/{}", multipart::uploads_dir(), hh.name);
            for upload in self.read_dir_entries(&hh_dir).await? {
                let target = format!("{}/{}", hh_dir, upload.name);
                // Reconstruct the upload id from the directory name.
                let upload_id = upload.name.clone();
                if let Ok(meta) = self.read_upload_meta(&upload_id).await {
                    if meta.bucket == bucket
                        && (prefix.is_empty() || meta.key.starts_with(&prefix))
                    {
                        uploads.push(MultipartUpload {
                            key: Some(ObjectKey::from(meta.key)),
                            upload_id: Some(MultipartUploadId::from(upload_id)),
                            initiated: Some(Timestamp::from(
                                UNIX_EPOCH + Duration::from_secs(meta.initiated.max(0) as u64),
                            )),
                            storage_class: Some(StorageClass::from_static(
                                StorageClass::STANDARD,
                            )),
                            ..Default::default()
                        });
                    }
                }
                let _ = target;
            }
        }
        uploads.sort_by(|a, b| a.key.cmp(&b.key));

        Ok(S3Response::new(ListMultipartUploadsOutput {
            bucket: Some(input.bucket),
            uploads: if uploads.is_empty() { None } else { Some(uploads) },
            ..Default::default()
        }))
    }
}

fn prefixes_to_dto(prefixes: &std::collections::BTreeSet<String>) -> Option<Vec<CommonPrefix>> {
    if prefixes.is_empty() {
        return None;
    }
    Some(
        prefixes
            .iter()
            .map(|p| CommonPrefix {
                prefix: Some(s3s::dto::Prefix::from(p.as_str())),
            })
            .collect(),
    )
}
