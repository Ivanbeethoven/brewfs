//! File-map oracles use deterministic MemoryLower bytes and real RustFS for
//! every physical upper PUT/GET/DELETE. The packed-lower integration suite
//! separately exercises authenticated packed objects through EntityData.
//! Metadata-only checks use a capability stub that panics on any object I/O.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::Notify;

use super::*;
use crate::cadapter::client::ObjectByteStream;
use crate::cadapter::s3::S3Backend;
use crate::workspace_overlay::entity::backend::{RowSnapshot, TransactionAuthority};
use crate::workspace_overlay::entity::keys::EntityKeys;
use crate::workspace_overlay::entity::lifecycle::{Lifecycle, WriterLease, WriterSession};
use crate::workspace_overlay::entity::memory::MemoryEntityBackend;
use crate::workspace_overlay::entity::namespace::{CreateRequest, Credentials};
use crate::workspace_overlay::entity::redis::RedisEntityBackend;
use crate::workspace_overlay::entity::resolver::ImmutableLower;
use crate::workspace_overlay::entity::resolver::test_support::MemoryLower;
use crate::workspace_overlay::entity::test_objects;

struct Objects {
    inner: S3Backend,
    puts: AtomicUsize,
    fail_put: AtomicUsize,
    pause_put: AtomicUsize,
    deletes: AtomicUsize,
    fail_delete: AtomicBool,
    entered: Notify,
    resume: Notify,
}

impl Objects {
    fn new(inner: S3Backend) -> Self {
        Self {
            inner,
            puts: AtomicUsize::new(0),
            fail_put: AtomicUsize::new(0),
            pause_put: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            fail_delete: AtomicBool::new(false),
            entered: Notify::new(),
            resume: Notify::new(),
        }
    }

    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .expect("upload did not reach pause");
    }
}

#[async_trait]
impl ObjectBackend for Objects {
    fn forbids_mutation_replay(&self) -> bool {
        self.inner.forbids_mutation_replay()
    }
    async fn put_object(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.inner.put_object(key, bytes).await
    }
    async fn put_object_create_only_durable(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let number = self.puts.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner
            .put_object_create_only_durable(key, bytes)
            .await?;
        if self.pause_put.load(Ordering::SeqCst) == number {
            let resumed = self.resume.notified();
            tokio::pin!(resumed);
            resumed.as_mut().enable();
            self.entered.notify_one();
            resumed.await;
        }
        if self.fail_put.load(Ordering::SeqCst) == number {
            anyhow::bail!("injected ambiguous durable PUT reply");
        }
        Ok(())
    }
    async fn get_object(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.inner.get_object(key).await
    }
    async fn get_object_range(
        &self,
        key: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> anyhow::Result<usize> {
        self.inner.get_object_range(key, offset, buf).await
    }
    async fn get_object_range_stream(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> anyhow::Result<ObjectByteStream> {
        self.inner
            .get_object_range_stream(key, offset, length)
            .await
    }
    async fn get_object_size_bounded(&self, key: &str) -> anyhow::Result<Option<u64>> {
        self.inner.get_object_size_bounded(key).await
    }
    async fn get_etag(&self, key: &str) -> anyhow::Result<String> {
        self.inner.get_etag(key).await
    }
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_object(key).await?;
        if self.fail_delete.swap(false, Ordering::SeqCst) {
            anyhow::bail!("injected ambiguous DELETE reply");
        }
        Ok(())
    }
}

struct NoObjectIo;

#[async_trait]
impl ObjectBackend for NoObjectIo {
    async fn put_object(&self, _: &str, _: &[u8]) -> anyhow::Result<()> {
        panic!("metadata-only test attempted object PUT")
    }
    async fn put_object_create_only_durable(&self, _: &str, _: &[u8]) -> anyhow::Result<()> {
        panic!("metadata-only test attempted durable object PUT")
    }
    async fn get_object(&self, _: &str) -> anyhow::Result<Option<Vec<u8>>> {
        panic!("metadata-only test attempted object GET")
    }
    async fn get_object_range(&self, _: &str, _: u64, _: &mut [u8]) -> anyhow::Result<usize> {
        panic!("metadata-only test attempted range GET")
    }
    async fn get_object_size_bounded(&self, _: &str) -> anyhow::Result<Option<u64>> {
        panic!("metadata-only test attempted object HEAD")
    }
    async fn get_etag(&self, _: &str) -> anyhow::Result<String> {
        panic!("metadata-only test attempted object ETag")
    }
    async fn delete_object(&self, _: &str) -> anyhow::Result<()> {
        panic!("metadata-only test attempted object DELETE")
    }
}

struct Fixture<B, O = Objects> {
    backend: Arc<B>,
    keys: EntityKeys,
    session: WriterSession,
    namespace: Arc<EntityNamespace<B, MemoryLower>>,
    data: Arc<EntityData<B, MemoryLower, O>>,
    objects: Arc<O>,
}

fn root() -> Credentials {
    Credentials {
        uid: 0,
        gid: 0,
        groups: vec![0],
        umask: 0o022,
    }
}

async fn attach<B: EntityBackend + 'static>(
    backend: Arc<B>,
    keys: EntityKeys,
    ttl_ns: i64,
) -> Fixture<B> {
    attach_objects(
        backend,
        keys,
        ttl_ns,
        Arc::new(Objects::new(test_objects::backend().await)),
    )
    .await
}

async fn attach_objects<B: EntityBackend + 'static, O: ObjectBackend + 'static>(
    backend: Arc<B>,
    keys: EntityKeys,
    ttl_ns: i64,
    objects: Arc<O>,
) -> Fixture<B, O> {
    let mut lower = MemoryLower::empty();
    lower.add_file(1, b"base", 2, 24);
    let lower = Arc::new(lower);
    let lifecycle = Lifecycle::new(backend.clone(), keys.clone());
    lifecycle.provision(lower.binding()).await.unwrap();
    let session = lifecycle.acquire(Uuid::new_v4(), ttl_ns).await.unwrap();
    let namespace = EntityNamespace::new(backend.clone(), lower, keys.clone(), session.clone());
    let data = EntityData::new(namespace.clone(), objects.clone(), 8)
        .await
        .unwrap();
    Fixture {
        backend,
        keys,
        session,
        namespace,
        data,
        objects,
    }
}

async fn fixture() -> Fixture<MemoryEntityBackend> {
    attach(
        Arc::new(MemoryEntityBackend::new(1000)),
        EntityKeys::new(Uuid::new_v4(), Uuid::new_v4()),
        1_000_000,
    )
    .await
}

async fn new_file<B: EntityBackend + 'static, O: ObjectBackend + 'static>(
    fixture: &Fixture<B, O>,
    name: &[u8],
) -> Arc<EntityHandle> {
    fixture
        .namespace
        .create(
            CreateRequest {
                parent: 1,
                name: name.to_vec(),
                kind: EntityKind::File,
                mode: 0o666,
                rdev: 0,
                symlink_target: None,
            },
            root(),
        )
        .await
        .unwrap();
    Arc::new(fixture.namespace.open(1, name, 6, &root()).await.unwrap())
}

async fn exercise_copy_up<B: EntityBackend + 'static>(fixture: &Fixture<B>) {
    let handle = Arc::new(
        fixture
            .namespace
            .open(1, b"base", 6, &root())
            .await
            .unwrap(),
    );
    let file = fixture.data.bind(handle).unwrap();
    let mut expected = test_lower_bytes(2, 0, 24);
    assert_eq!(file.read(0, 40).await.unwrap(), expected);
    let written = file
        .write(6, Bytes::from_static(b"WXYZ"), false)
        .await
        .unwrap();
    assert_eq!(
        written,
        WriteOutcome {
            offset: 6,
            written: 4,
            size: 24
        }
    );
    expected[6..10].copy_from_slice(b"WXYZ");
    assert_eq!(file.read(0, 40).await.unwrap(), expected);
    file.write(30, Bytes::from_static(b"tail"), false)
        .await
        .unwrap();
    expected.resize(30, 0);
    expected.extend_from_slice(b"tail");
    assert_eq!(file.read(0, 40).await.unwrap(), expected);
    // The boundary chunk is a mixture of LowerRange and Data. Shrink/extend
    // must remove both kinds beyond the cut, including already present maps.
    file.truncate(7).await.unwrap();
    file.truncate(34).await.unwrap();
    expected.truncate(7);
    expected.resize(34, 0);
    assert_eq!(file.read(0, 40).await.unwrap(), expected);
    assert_eq!(file.attr().await.unwrap().lower_visible_size, 7);
    file.write(9, Bytes::from_static(b"new"), false)
        .await
        .unwrap();
    expected[9..12].copy_from_slice(b"new");
    assert_eq!(file.read(0, 40).await.unwrap(), expected);
    file.fsync().await.unwrap();
    assert!(
        fixture
            .backend
            .read(&[fixture.keys.truncate(2)])
            .await
            .unwrap()
            .rows[0]
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn durable_create_only_put_accepts_identical_retry_and_rejects_replacement() {
    let objects = test_objects::backend().await;
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let key = keys.object(1);
    objects
        .put_object_create_only_durable(&key, b"original")
        .await
        .unwrap();
    objects
        .put_object_create_only_durable(&key, b"original")
        .await
        .unwrap();
    assert!(
        objects
            .put_object_create_only_durable(&key, b"replaced")
            .await
            .is_err()
    );
    assert_eq!(
        objects.get_object(&key).await.unwrap().unwrap(),
        b"original"
    );
    objects.delete_object(&key).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn copy_up_overwrite_sparse_extend_and_shrink_never_resurrect_data() {
    exercise_copy_up(&fixture().await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn concurrent_append_assigns_disjoint_offsets_and_preserves_every_record() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"append").await;
    let mut tasks = Vec::new();
    for byte in 0..32_u8 {
        let data = fixture.data.clone();
        let handle = handle.clone();
        tasks.push(tokio::spawn(async move {
            (
                byte,
                data.write(handle, 0, Bytes::from(vec![byte; 4]), true)
                    .await
                    .unwrap(),
            )
        }));
    }
    let mut expected = vec![0; 128];
    let mut offsets = std::collections::BTreeSet::new();
    for task in tasks {
        let (byte, result) = task.await.unwrap();
        assert_eq!(result.written, 4);
        assert!(offsets.insert(result.offset));
        expected[result.offset as usize..result.offset as usize + 4].fill(byte);
    }
    assert_eq!(
        offsets.into_iter().collect::<Vec<_>>(),
        (0..32).map(|n| n * 4).collect::<Vec<_>>()
    );
    fixture.data.fsync(handle.clone()).await.unwrap();
    assert_eq!(fixture.data.read(handle, 0, 128).await.unwrap(), expected);
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn paused_upload_is_invisible_and_cancellation_cannot_release_file_barrier() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"waiting").await;
    let other = new_file(&fixture, b"independent").await;
    fixture.objects.pause_put.store(1, Ordering::SeqCst);
    let data = fixture.data.clone();
    let writer_handle = handle.clone();
    let caller = tokio::spawn(async move {
        data.write(writer_handle, 0, Bytes::from_static(b"waiting"), false)
            .await
    });
    fixture.objects.entered().await;
    let snapshot = fixture
        .backend
        .read(&[
            fixture.keys.inode(handle.ino()),
            fixture.keys.chunk(handle.ino(), 0),
        ])
        .await
        .unwrap();
    let UpperInode::Present(inode) = codec::decode(snapshot.rows[0].as_deref().unwrap()).unwrap()
    else {
        panic!("missing inode")
    };
    assert_eq!(inode.size, 0);
    assert!(snapshot.rows[1].is_none());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let data = fixture.data.clone();
    let barrier_handle = handle.clone();
    let barrier = tokio::spawn(async move { data.fsync(barrier_handle).await });
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .data
            .write(other, 0, Bytes::from_static(b"other"), false),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !barrier.is_finished(),
        "fsync overtook a still-running accepted write"
    );
    fixture.objects.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(5), barrier)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fixture.data.read(handle, 0, 8).await.unwrap(), b"waiting");
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn short_write_reports_confirmed_prefix_and_later_fsync_reports_upload_failure() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"short").await;
    fixture.objects.fail_put.store(2, Ordering::SeqCst);
    let result = fixture
        .data
        .write(
            handle.clone(),
            0,
            Bytes::from_static(b"abcdefghijkl"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        WriteOutcome {
            offset: 0,
            written: 8,
            size: 8
        }
    );
    assert_eq!(
        fixture.data.read(handle.clone(), 0, 16).await.unwrap(),
        b"abcdefgh"
    );
    assert!(matches!(
        fixture.data.fsync(handle.clone()).await,
        Err(EntityError::Backend(_))
    ));
    // A second adapter must not turn a short/uncertain write into clean fsync.
    let second = EntityData::new(fixture.namespace.clone(), fixture.objects.clone(), 8)
        .await
        .unwrap();
    assert!(matches!(
        second.fsync(handle).await,
        Err(EntityError::Backend(_))
    ));
    assert_eq!(fixture.objects.puts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn canceled_failed_write_keeps_error_for_fsync() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"canceled").await;
    fixture.objects.pause_put.store(1, Ordering::SeqCst);
    fixture.objects.fail_put.store(1, Ordering::SeqCst);
    let data = fixture.data.clone();
    let writer_handle = handle.clone();
    let caller = tokio::spawn(async move {
        data.write(writer_handle, 0, Bytes::from_static(b"failed"), false)
            .await
    });
    fixture.objects.entered().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    fixture.objects.resume.notify_one();
    assert!(matches!(
        fixture.data.fsync(handle.clone()).await,
        Err(EntityError::Backend(_))
    ));
    assert!(fixture.data.read(handle, 0, 8).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn upper_reads_reject_missing_corrupted_and_oversized_objects() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"authenticated").await;
    fixture
        .data
        .write(handle.clone(), 0, Bytes::from_static(b"payload"), false)
        .await
        .unwrap();
    let snapshot = fixture
        .backend
        .read(&[fixture.keys.chunk(handle.ino(), 0)])
        .await
        .unwrap();
    let map: ChunkMap = codec::decode(snapshot.rows[0].as_deref().unwrap()).unwrap();
    let slice = *map.upper_slices().unwrap().first().unwrap();
    let key = fixture.keys.object(slice);
    assert!(key.starts_with("workspace-entity-v1/"));
    assert!(!key.starts_with("chunks/"));
    for damaged in [b"PAYLOAD".as_slice(), b"payload-extra"] {
        fixture
            .objects
            .inner
            .put_object(&key, damaged)
            .await
            .unwrap();
        assert!(matches!(
            fixture.data.read(handle.clone(), 0, 7).await,
            Err(EntityError::Corrupt(_))
        ));
    }
    fixture.objects.inner.delete_object(&key).await.unwrap();
    assert!(matches!(
        fixture.data.read(handle, 0, 7).await,
        Err(EntityError::Corrupt(_))
    ));
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn unlinked_open_file_retains_data_and_readonly_handles_cannot_mutate() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"live").await;
    let readonly = Arc::new(
        fixture
            .namespace
            .open(1, b"live", 4, &root())
            .await
            .unwrap(),
    );
    assert_eq!(
        fixture
            .data
            .write(readonly.clone(), 0, Bytes::from_static(b"no"), false)
            .await
            .unwrap_err(),
        EntityError::Permission
    );
    assert_eq!(
        fixture.data.truncate(readonly, 0).await.unwrap_err(),
        EntityError::Permission
    );
    fixture
        .namespace
        .unlink(1, b"live".to_vec(), false, root())
        .await
        .unwrap();
    fixture
        .data
        .write(handle.clone(), 0, Bytes::from_static(b"alive"), false)
        .await
        .unwrap();
    fixture.data.fsync(handle.clone()).await.unwrap();
    assert_eq!(
        fixture.data.read(handle.clone(), 0, 8).await.unwrap(),
        b"alive"
    );
    assert_eq!(fixture.data.attr(handle.clone()).await.unwrap().nlink, 0);
    assert!(
        fixture
            .backend
            .read(&[fixture.keys.orphan(handle.ino())])
            .await
            .unwrap()
            .rows[0]
            .is_some()
    );
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn read_only_fsync_and_reads_are_noatime_and_invalid_ranges_do_not_poison_file() {
    let fixture = fixture().await;
    let readonly = Arc::new(
        fixture
            .namespace
            .open(1, b"base", 4, &root())
            .await
            .unwrap(),
    );
    let before = fixture
        .backend
        .scan_prefix(fixture.keys.volume_prefix(), 4096)
        .await
        .unwrap();
    let attr = fixture.data.attr(readonly.clone()).await.unwrap();
    assert_eq!(
        fixture.data.read(readonly.clone(), 0, 24).await.unwrap(),
        test_lower_bytes(2, 0, 24)
    );
    fixture.data.fsync(readonly.clone()).await.unwrap();
    assert_eq!(fixture.data.attr(readonly).await.unwrap(), attr);
    assert_eq!(
        fixture
            .backend
            .scan_prefix(fixture.keys.volume_prefix(), 4096)
            .await
            .unwrap(),
        before
    );
    let writable = Arc::new(
        fixture
            .namespace
            .open(1, b"base", 6, &root())
            .await
            .unwrap(),
    );
    assert!(matches!(
        fixture
            .data
            .write(
                writable.clone(),
                u64::MAX,
                Bytes::from_static(b"invalid"),
                false
            )
            .await,
        Err(EntityError::Overflow)
    ));
    fixture.data.fsync(writable.clone()).await.unwrap();
    fixture
        .data
        .write(writable.clone(), 0, Bytes::from_static(b"ok"), false)
        .await
        .unwrap();
    fixture.data.fsync(writable).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn discard_reserves_before_delete_and_reconciles_an_ambiguous_reply() {
    let fixture = fixture().await;
    let handle = new_file(&fixture, b"discarded").await;
    fixture
        .data
        .write(
            handle.clone(),
            0,
            Bytes::from_static(b"two objects!"),
            false,
        )
        .await
        .unwrap();
    fixture.data.fsync(handle.clone()).await.unwrap();
    drop(handle);
    let maintenance = Lifecycle::new(fixture.backend.clone(), fixture.keys.clone())
        .enter_maintenance(&fixture.session)
        .await
        .unwrap();
    fixture.objects.fail_delete.store(true, Ordering::SeqCst);
    let mut saw_error = false;
    for _ in 0..32 {
        match fixture.data.discard_step(&maintenance).await {
            Ok(false) => {}
            Ok(true) => panic!("discard completed before injected DELETE failure"),
            Err(EntityError::Backend(_)) => {
                saw_error = true;
                break;
            }
            Err(error) => panic!("unexpected discard error: {error}"),
        }
    }
    assert!(saw_error);
    assert_eq!(fixture.objects.deletes.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .backend
            .read(&[fixture.keys.lower_pin()])
            .await
            .unwrap()
            .rows[0]
            .is_some()
    );
    let mut complete = false;
    for _ in 0..32 {
        if fixture.data.discard_step(&maintenance).await.unwrap() {
            complete = true;
            break;
        }
    }
    assert!(complete);
    assert!(fixture.data.discard_step(&maintenance).await.unwrap());
    assert_eq!(fixture.objects.deletes.load(Ordering::SeqCst), 3);
    assert!(
        fixture
            .backend
            .read(&[fixture.keys.lower_pin()])
            .await
            .unwrap()
            .rows[0]
            .is_none()
    );
    for slice in [1, 2] {
        assert!(
            fixture
                .objects
                .get_object_size_bounded(&fixture.keys.object(slice))
                .await
                .unwrap()
                .is_none()
        );
        let row = fixture
            .backend
            .read(&[fixture.keys.slice(slice)])
            .await
            .unwrap();
        let owner: SliceOwnership = codec::decode(row.rows[0].as_deref().unwrap()).unwrap();
        assert!(matches!(
            owner.state,
            super::super::ownership::SliceState::Deleted { .. }
        ));
    }
}

#[tokio::test]
async fn chunk_geometry_is_fixed_and_expired_empty_io_is_fenced() {
    let fixture = attach_objects(
        Arc::new(MemoryEntityBackend::new(1000)),
        EntityKeys::new(Uuid::new_v4(), Uuid::new_v4()),
        1_000_000,
        Arc::new(NoObjectIo),
    )
    .await;
    assert!(matches!(
        EntityData::new(fixture.namespace.clone(), fixture.objects.clone(), 16).await,
        Err(EntityError::Invalid(_))
    ));
    let handle = new_file(&fixture, b"empty").await;
    fixture.backend.set_time(2_000_000).unwrap();
    assert_eq!(
        fixture.data.read(handle.clone(), 0, 0).await.unwrap_err(),
        EntityError::Fenced
    );
    assert_eq!(
        fixture
            .data
            .write(handle.clone(), 0, Bytes::new(), false)
            .await
            .unwrap_err(),
        EntityError::Fenced
    );
    assert!(fixture.data.fsync(handle).await.is_err());
}

/// Force a real authority-row conflict after allocation reads its lease. The
/// underlying allocator, rather than this wrapper, rejects the stale authority.
struct RenewDuringAllocationBackend {
    inner: MemoryEntityBackend,
    keys: EntityKeys,
    renew_allocations: AtomicUsize,
    allocations: AtomicUsize,
}

impl RenewDuringAllocationBackend {
    fn new(keys: EntityKeys) -> Self {
        Self {
            inner: MemoryEntityBackend::new(1000),
            keys,
            renew_allocations: AtomicUsize::new(0),
            allocations: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl EntityBackend for RenewDuringAllocationBackend {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot> {
        self.inner.read(keys).await
    }

    async fn allocate(&self, authority: &TransactionAuthority, key: &[u8]) -> Result<u64> {
        if key == self.keys.counter("slice") {
            self.allocations.fetch_add(1, Ordering::SeqCst);
            if self
                .renew_allocations
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                let lease_key = self.keys.lease();
                let snapshot = self.inner.read(std::slice::from_ref(&lease_key)).await?;
                let previous = snapshot.rows[0].clone().unwrap();
                let mut lease: WriterLease = codec::decode(&previous)?;
                lease.expires_ns += 1_000_000;
                assert_eq!(
                    self.inner
                        .commit(
                            authority,
                            &EntityPlan {
                                checks: vec![RowCheck {
                                    key: lease_key.clone(),
                                    expected: Some(previous),
                                }],
                                writes: vec![RowWrite::Put {
                                    key: lease_key,
                                    value: codec::encode(&lease)?,
                                }],
                            },
                        )
                        .await?,
                    CommitOutcome::Committed
                );
            }
        }
        self.inner.allocate(authority, key).await
    }

    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix, limit).await
    }

    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome> {
        self.inner.commit(authority, plan).await
    }
}

#[tokio::test]
async fn reservation_contention_is_bounded_and_does_not_poison_fsync() {
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let backend = Arc::new(RenewDuringAllocationBackend::new(keys.clone()));
    let fixture = attach_objects(backend.clone(), keys, 1_000_000, Arc::new(NoObjectIo)).await;
    let handle = new_file(&fixture, b"contention").await;
    backend
        .renew_allocations
        .store(MAX_RETRIES, Ordering::SeqCst);
    assert_eq!(
        fixture
            .data
            .write(handle.clone(), 0, Bytes::from_static(b"once"), false)
            .await
            .unwrap_err(),
        EntityError::Busy
    );
    assert_eq!(backend.allocations.load(Ordering::SeqCst), MAX_RETRIES);
    assert_eq!(fixture.data.attr(handle.clone()).await.unwrap().size, 0);
    // NoObjectIo would panic if a failed reservation reached any object call.
    fixture.data.fsync(handle.clone()).await.unwrap();
    backend.inner.set_time(100_000_000).unwrap();
    assert_eq!(
        fixture.data.fsync(handle).await.unwrap_err(),
        EntityError::Fenced
    );
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn reservation_renews_authority_before_retrying_known_allocation_conflict() {
    let keys = EntityKeys::new(Uuid::new_v4(), Uuid::new_v4());
    let backend = Arc::new(RenewDuringAllocationBackend::new(keys.clone()));
    let fixture = attach(backend.clone(), keys, 1_000_000).await;
    let handle = new_file(&fixture, b"renewed").await;
    backend.renew_allocations.store(1, Ordering::SeqCst);
    let written = fixture
        .data
        .write(handle.clone(), 0, Bytes::from_static(b"once"), false)
        .await
        .unwrap();
    assert_eq!(written.written, 4);
    assert_eq!(backend.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.objects.puts.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.data.read(handle.clone(), 0, 8).await.unwrap(),
        b"once"
    );
    fixture.data.fsync(handle).await.unwrap();
}

struct LostReplyBackend {
    inner: MemoryEntityBackend,
    lose: AtomicBool,
    publications: AtomicUsize,
}

#[async_trait]
impl EntityBackend for LostReplyBackend {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot> {
        self.inner.read(keys).await
    }
    async fn allocate(&self, authority: &TransactionAuthority, key: &[u8]) -> Result<u64> {
        self.inner.allocate(authority, key).await
    }
    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix, limit).await
    }
    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome> {
        let result = self.inner.commit(authority, plan).await?;
        if result == CommitOutcome::Committed
            && plan
                .writes
                .iter()
                .any(|write| write.key().windows(7).any(|part| part == b"/chunk/"))
        {
            self.publications.fetch_add(1, Ordering::SeqCst);
            if self.lose.swap(false, Ordering::SeqCst) {
                return Err(EntityError::Backend("injected lost committed reply".into()));
            }
        }
        Ok(result)
    }
}

#[tokio::test]
#[ignore = "requires isolated RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn uncertain_file_publication_is_not_replayed_or_hidden_by_fsync() {
    let backend = Arc::new(LostReplyBackend {
        inner: MemoryEntityBackend::new(1000),
        lose: AtomicBool::new(false),
        publications: AtomicUsize::new(0),
    });
    let fixture = attach(
        backend.clone(),
        EntityKeys::new(Uuid::new_v4(), Uuid::new_v4()),
        1_000_000,
    )
    .await;
    let handle = new_file(&fixture, b"uncertain").await;
    backend.lose.store(true, Ordering::SeqCst);
    assert!(matches!(
        fixture
            .data
            .write(handle.clone(), 0, Bytes::from_static(b"once"), false)
            .await,
        Err(EntityError::Backend(_))
    ));
    assert_eq!(backend.publications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.objects.puts.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.data.read(handle.clone(), 0, 8).await.unwrap(),
        b"once"
    );
    assert!(matches!(
        fixture.data.fsync(handle).await,
        Err(EntityError::Backend(_))
    ));
}

#[tokio::test]
#[ignore = "requires isolated Redis via BREWFS_TEST_REDIS_URL and RustFS via BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT/BUCKET and AWS credentials"]
async fn redis_entity_data_copy_up_durability_and_truncate_contract() {
    let url = std::env::var("BREWFS_TEST_REDIS_URL").expect("BREWFS_TEST_REDIS_URL");
    let volume = Uuid::new_v4();
    let keys = EntityKeys::new(volume, Uuid::new_v4());
    let backend = Arc::new(RedisEntityBackend::connect(&url, volume).await.unwrap());
    let outcome = AssertUnwindSafe(async {
        let fixture = attach(backend.clone(), keys.clone(), 60_000_000_000).await;
        exercise_copy_up(&fixture).await;
        let handle = new_file(&fixture, b"upper").await;
        fixture
            .data
            .write(handle.clone(), 0, Bytes::from_static(b"durable"), false)
            .await
            .unwrap();
        fixture.data.fsync(handle.clone()).await.unwrap();
        let reattached = EntityData::new(fixture.namespace.clone(), fixture.objects.clone(), 8)
            .await
            .unwrap();
        assert_eq!(reattached.read(handle, 0, 8).await.unwrap(), b"durable");
        assert!(!fixture.session.is_closed());
    })
    .catch_unwind()
    .await;
    let rows = backend
        .scan_prefix(keys.volume_prefix(), 4096)
        .await
        .unwrap();
    if let Some((key, value)) = rows.first() {
        let authority = TransactionAuthority::from_validated(
            vec![RowCheck {
                key: key.clone(),
                expected: Some(value.clone()),
            }],
            None,
            None,
        )
        .unwrap();
        let plan = EntityPlan {
            checks: rows
                .iter()
                .map(|(key, value)| RowCheck {
                    key: key.clone(),
                    expected: Some(value.clone()),
                })
                .collect(),
            writes: rows
                .into_iter()
                .map(|(key, _)| RowWrite::Delete { key })
                .collect(),
        };
        assert_eq!(
            backend.commit(&authority, &plan).await.unwrap(),
            CommitOutcome::Committed
        );
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
