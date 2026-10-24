use super::*;
use crate::chunk::{layout::ChunkLayout, slice::SliceDesc, store::InMemoryBlockStore};
use crate::meta::client::MetaClientOptions;
use crate::meta::config::{CompactConfig, MetaClientConfig};
use crate::meta::factory::create_meta_store_from_url;
use crate::meta::store::{MetaError, OpenFlags, RenameOutcome};
use crate::vfs::cache::config::CacheConfig;
use s3s::S3ErrorCode;

/// Real SQLite metadata with independently injectable xattr read failures.
struct XattrFailureStore {
    inner: Arc<dyn MetaStore>,
    fail_name: std::sync::Mutex<Option<&'static str>>,
}

#[async_trait]
impl MetaStore for XattrFailureStore {
    async fn get_xattr(&self, inode: i64, name: &str) -> Result<Option<Vec<u8>>, MetaError> {
        if *self.fail_name.lock().unwrap() == Some(name) {
            return Err(MetaError::Internal("injected xattr read failure".into()));
        }
        self.inner.get_xattr(inode, name).await
    }

    async fn set_xattr(
        &self,
        inode: i64,
        name: &str,
        value: &[u8],
        flags: u32,
    ) -> Result<(), MetaError> {
        self.inner.set_xattr(inode, name, value, flags).await
    }

    async fn remove_xattr(&self, inode: i64, name: &str) -> Result<(), MetaError> {
        self.inner.remove_xattr(inode, name).await
    }

    async fn stat(&self, ino: i64) -> Result<Option<FileAttr>, MetaError> {
        self.inner.stat(ino).await
    }

    async fn lookup(&self, parent: i64, name: &str) -> Result<Option<i64>, MetaError> {
        self.inner.lookup(parent, name).await
    }

    async fn lookup_path(&self, path: &str) -> Result<Option<(i64, FileType)>, MetaError> {
        self.inner.lookup_path(path).await
    }

    async fn readdir(&self, ino: i64) -> Result<Vec<DirEntry>, MetaError> {
        self.inner.readdir(ino).await
    }

    async fn mkdir(&self, parent: i64, name: String) -> Result<i64, MetaError> {
        self.inner.mkdir(parent, name).await
    }

    async fn rmdir(&self, parent: i64, name: &str) -> Result<(), MetaError> {
        self.inner.rmdir(parent, name).await
    }

    async fn create_file(&self, parent: i64, name: String) -> Result<i64, MetaError> {
        self.inner.create_file(parent, name).await
    }

    async fn unlink(&self, parent: i64, name: &str) -> Result<(), MetaError> {
        self.inner.unlink(parent, name).await
    }

    async fn rename_with_mode(
        &self,
        old_parent: i64,
        old_name: &str,
        new_parent: i64,
        new_name: String,
        noreplace: bool,
    ) -> Result<RenameOutcome, MetaError> {
        self.inner
            .rename_with_mode(old_parent, old_name, new_parent, new_name, noreplace)
            .await
    }

    async fn rename_exchange(
        &self,
        old_parent: i64,
        old_name: &str,
        new_parent: i64,
        new_name: &str,
    ) -> Result<(), MetaError> {
        self.inner
            .rename_exchange(old_parent, old_name, new_parent, new_name)
            .await
    }

    async fn set_file_size(&self, ino: i64, size: u64) -> Result<(), MetaError> {
        self.inner.set_file_size(ino, size).await
    }

    async fn get_names(&self, ino: i64) -> Result<Vec<(Option<i64>, String)>, MetaError> {
        self.inner.get_names(ino).await
    }

    async fn get_paths(&self, ino: i64) -> Result<Vec<String>, MetaError> {
        self.inner.get_paths(ino).await
    }

    fn root_ino(&self) -> i64 {
        self.inner.root_ino()
    }

    async fn initialize(&self) -> Result<(), MetaError> {
        self.inner.initialize().await
    }

    async fn get_deleted_files(&self) -> Result<Vec<i64>, MetaError> {
        self.inner.get_deleted_files().await
    }

    async fn remove_file_metadata(&self, ino: i64) -> Result<(), MetaError> {
        self.inner.remove_file_metadata(ino).await
    }

    async fn get_slices(&self, chunk_id: u64) -> Result<Vec<SliceDesc>, MetaError> {
        self.inner.get_slices(chunk_id).await
    }

    async fn append_slice(&self, chunk_id: u64, slice: SliceDesc) -> Result<(), MetaError> {
        self.inner.append_slice(chunk_id, slice).await
    }

    async fn write(
        &self,
        ino: i64,
        chunk_id: u64,
        slice: SliceDesc,
        new_size: u64,
    ) -> Result<(), MetaError> {
        self.inner.write(ino, chunk_id, slice, new_size).await
    }

    async fn next_id(&self, key: &str) -> Result<i64, MetaError> {
        self.inner.next_id(key).await
    }

    async fn open(&self, ino: i64, flags: OpenFlags) -> Result<FileAttr, MetaError> {
        self.inner.open(ino, flags).await
    }

    async fn close(&self, ino: i64) -> Result<(), MetaError> {
        self.inner.close(ino).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn request<T>(input: T) -> S3Request<T> {
    S3Request {
        input,
        method: Default::default(),
        uri: Default::default(),
        headers: Default::default(),
        extensions: Default::default(),
        credentials: None,
        region: None,
        service: None,
        trailing_headers: None,
    }
}

async fn test_s3() -> (BrewFsS3<InMemoryBlockStore>, Arc<XattrFailureStore>) {
    let handle = create_meta_store_from_url("sqlite::memory:").await.unwrap();
    let store = Arc::new(XattrFailureStore {
        inner: handle.store(),
        fail_name: std::sync::Mutex::new(None),
    });
    let config = MetaClientConfig::default();
    let client = MetaClient::with_options(
        store.clone() as Arc<dyn MetaStore>,
        config.capacity.clone(),
        config.effective_ttl(),
        MetaClientOptions::default(),
    );
    let vfs = VFS::with_meta_layer_with_cache_config(
        ChunkLayout::default(),
        Arc::new(InMemoryBlockStore::new()),
        client,
        CompactConfig::default(),
        CacheConfig::default(),
    )
    .unwrap();
    vfs.mkdir_p(&multipart::tmp_dir()).await.unwrap();
    let s3 = BrewFsS3::new(
        vfs,
        S3Options {
            bucket_mode: BucketMode::Single {
                bucket: "bucket".into(),
            },
            hide_dir_objects: false,
        },
    );
    for key in ["source", "directory/", "existing"] {
        s3.put_object(request(PutObjectInput {
            bucket: "bucket".into(),
            key: key.into(),
            body: Some(StreamingBlob::from_bytes(bytes::Bytes::new())),
            content_type: Some("text/plain".into()),
            metadata: Some(HashMap::from([(
                "author".into(),
                MetadataValue::from("test"),
            )])),
            ..Default::default()
        }))
        .await
        .unwrap();
    }
    (s3, store)
}

fn copy_input(source: &str, destination: &str) -> CopyObjectInput {
    CopyObjectInput::builder()
        .bucket("bucket".into())
        .key(destination.into())
        .copy_source(CopySource::Bucket {
            bucket: "bucket".into(),
            key: source.into(),
            version_id: None,
        })
        .build()
        .unwrap()
}

const FAILURE_CASES: [(&str, &str); 3] = [
    (XATTR_META, "source"),
    (XATTR_ETAG, "source"),
    (XATTR_DIROBJ, "directory/"),
];

#[tokio::test]
async fn get_rejects_xattr_read_errors() {
    let (s3, store) = test_s3().await;
    for (name, key) in FAILURE_CASES {
        *store.fail_name.lock().unwrap() = Some(name);
        let result = s3
            .get_object(request(GetObjectInput {
                bucket: "bucket".into(),
                key: key.into(),
                ..Default::default()
            }))
            .await;
        assert_eq!(
            result.unwrap_err().code(),
            &S3ErrorCode::InternalError,
            "{name}"
        );
    }
}

#[tokio::test]
async fn head_rejects_xattr_read_errors() {
    let (s3, store) = test_s3().await;
    for (name, key) in FAILURE_CASES {
        *store.fail_name.lock().unwrap() = Some(name);
        let result = s3
            .head_object(request(HeadObjectInput {
                bucket: "bucket".into(),
                key: key.into(),
                ..Default::default()
            }))
            .await;
        assert_eq!(
            result.unwrap_err().code(),
            &S3ErrorCode::InternalError,
            "{name}"
        );
    }
}

#[tokio::test]
async fn copy_rejects_xattr_read_errors_without_publishing() {
    let (s3, store) = test_s3().await;
    let existing_ino = s3.vfs.stat("/existing").await.unwrap().ino;
    for (name, key) in FAILURE_CASES {
        *store.fail_name.lock().unwrap() = Some(name);
        for destination in ["new", "new-directory/", "existing"] {
            let result = s3.copy_object(request(copy_input(key, destination))).await;
            assert!(
                matches!(
                    s3.vfs.stat("/new").await,
                    Err(crate::vfs::error::VfsError::NotFound { .. })
                ),
                "{name}: COPY published a file"
            );
            assert!(
                matches!(
                    s3.vfs.stat("/new-directory").await,
                    Err(crate::vfs::error::VfsError::NotFound { .. })
                ),
                "{name}: COPY published a directory"
            );
            assert_eq!(
                s3.vfs.stat("/existing").await.unwrap().ino,
                existing_ino,
                "{name}: COPY replaced an existing destination"
            );
            assert_eq!(
                result.unwrap_err().code(),
                &S3ErrorCode::InternalError,
                "{name}"
            );
        }
    }
}

#[tokio::test]
async fn listing_and_delete_reject_directory_marker_read_errors() {
    let (s3, store) = test_s3().await;
    *store.fail_name.lock().unwrap() = Some(XATTR_DIROBJ);
    let result = s3
        .list_objects_v2(request(ListObjectsV2Input {
            bucket: "bucket".into(),
            ..Default::default()
        }))
        .await;
    assert_eq!(result.unwrap_err().code(), &S3ErrorCode::InternalError);
    let result = s3
        .delete_object(request(DeleteObjectInput {
            bucket: "bucket".into(),
            key: "directory/".into(),
            ..Default::default()
        }))
        .await;
    assert_eq!(result.unwrap_err().code(), &S3ErrorCode::InternalError);
    assert!(s3.vfs.stat("/directory").await.is_ok());
}

#[tokio::test]
async fn parent_pruning_preserves_directory_when_marker_read_fails() {
    let (s3, store) = test_s3().await;
    s3.vfs.create_file("/directory/child").await.unwrap();
    *store.fail_name.lock().unwrap() = Some(XATTR_DIROBJ);
    s3.delete_object(request(DeleteObjectInput {
        bucket: "bucket".into(),
        key: "directory/child".into(),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert!(s3.vfs.stat("/directory").await.is_ok());
}

#[tokio::test]
async fn malformed_xattrs_are_not_treated_as_absent() {
    for (name, key, value) in [
        (XATTR_ETAG, "source", &[0xff][..]),
        (XATTR_META, "source", &[0xff][..]),
        (XATTR_META, "source", &b"not json"[..]),
        (XATTR_DIROBJ, "directory/", &[0xff][..]),
    ] {
        let (s3, _) = test_s3().await;
        let attr = s3.vfs.stat(&format!("/{key}")).await.unwrap();
        s3.vfs
            .set_xattr_ino(attr.ino, name, value, 0)
            .await
            .unwrap();
        let result = s3
            .head_object(request(HeadObjectInput {
                bucket: "bucket".into(),
                key: key.into(),
                ..Default::default()
            }))
            .await;
        assert_eq!(
            result.unwrap_err().code(),
            &S3ErrorCode::InternalError,
            "{name}"
        );
        let result = s3.copy_object(request(copy_input(key, "copy"))).await;
        assert_eq!(
            result.unwrap_err().code(),
            &S3ErrorCode::InternalError,
            "{name}"
        );
        assert!(matches!(
            s3.vfs.stat("/copy").await,
            Err(crate::vfs::error::VfsError::NotFound { .. })
        ));
    }
}

#[tokio::test]
async fn absent_xattrs_keep_external_file_defaults() {
    let (s3, _) = test_s3().await;
    s3.vfs.create_file("/external").await.unwrap();
    let attr = s3.vfs.stat("/external").await.unwrap();
    let expected_etag = BrewFsS3::<InMemoryBlockStore>::fallback_etag(&attr);
    let head = s3
        .head_object(request(HeadObjectInput {
            bucket: "bucket".into(),
            key: "external".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .output;
    assert_eq!(head.e_tag.unwrap().value(), expected_etag);
    assert_eq!(head.content_type.unwrap().as_str(), DEFAULT_CONTENT_TYPE);
    assert!(head.metadata.is_none());
    let get = s3
        .get_object(request(GetObjectInput {
            bucket: "bucket".into(),
            key: "external".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .output;
    assert_eq!(get.e_tag.unwrap().value(), expected_etag);
    assert_eq!(get.content_type.unwrap().as_str(), DEFAULT_CONTENT_TYPE);
    assert!(get.metadata.is_none());
    s3.copy_object(request(copy_input("external", "copy")))
        .await
        .unwrap();
    s3.vfs.mkdir_p("/implicit").await.unwrap();
    let error = s3
        .head_object(request(HeadObjectInput {
            bucket: "bucket".into(),
            key: "implicit/".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), &S3ErrorCode::NoSuchKey);
}
