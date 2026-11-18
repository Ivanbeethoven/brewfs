#![cfg(feature = "workspace")]

use super::super::reader::DataReader;
use super::FileWriter;
use crate::chunk::ChunkLayout;
use crate::chunk::read_plan::{ReadPlanSegment, WorkspaceReadPlanProvider};
use crate::chunk::store::{BlockKey, BlockStore};
use crate::meta::MetaLayer;
use crate::vfs::Inode;
use crate::vfs::backend::Backend;
use crate::vfs::cache::config::WriteBackMode;
use crate::vfs::config::{ReadConfig, WriteConfig};
use crate::workspace::catalog::{AcquireLease, CreateVolumeRoot, WorkspaceStore};
use crate::workspace::ids::{LayerId, LeaseId, WorkspaceId};
use crate::workspace::meta_layer::WorkspaceMetaLayer;
use crate::workspace::model::{ViewContext, WORKSPACE_SCHEMA_VERSION};
use crate::workspace::stores::database::SqliteWorkspaceStore;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::time::timeout;
use uuid::Uuid;

const PAGE: usize = 4096;
const OUTPUT_LEN: usize = PAGE * 3;
const CHUNK_SIZE: u64 = 1024 * 1024;
const BLOCK_SIZE: u32 = 64 * 1024;

type TestMeta = WorkspaceMetaLayer<SqliteWorkspaceStore>;
type TestBackend = Backend<RecordingStore, TestMeta>;
type TestWriter = FileWriter<RecordingStore, TestMeta>;

#[derive(Clone, Debug, PartialEq)]
struct PutCall {
    key: BlockKey,
    offset: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct ReadCall {
    key: BlockKey,
    offset: u64,
    len: usize,
}

#[derive(Default)]
struct StoreState {
    blocks: HashMap<BlockKey, Vec<u8>>,
    puts: Vec<PutCall>,
    reads: Vec<ReadCall>,
}

struct RecordingStore {
    state: Mutex<StoreState>,
    hold_put: AtomicBool,
    fail_put: AtomicBool,
    fail_next_put: AtomicBool,
    put_started: Notify,
    release_put: Semaphore,
}

impl RecordingStore {
    fn new(hold_put: bool, fail_put: bool) -> Self {
        Self {
            state: Mutex::new(StoreState::default()),
            hold_put: AtomicBool::new(hold_put),
            fail_put: AtomicBool::new(fail_put),
            fail_next_put: AtomicBool::new(false),
            put_started: Notify::new(),
            release_put: Semaphore::new(0),
        }
    }

    async fn puts(&self) -> Vec<PutCall> {
        self.state.lock().await.puts.clone()
    }

    async fn reads(&self) -> Vec<ReadCall> {
        self.state.lock().await.reads.clone()
    }

    async fn stored_block_count(&self) -> usize {
        self.state.lock().await.blocks.len()
    }

    async fn wait_for_put_start(&self) {
        self.put_started.notified().await;
    }

    fn release_puts(&self) {
        self.hold_put.store(false, Ordering::Release);
        self.release_put.add_permits(32);
    }

    fn release_one_put(&self) {
        self.release_put.add_permits(1);
    }
}

#[async_trait]
impl BlockStore for RecordingStore {
    async fn write_fresh_range(
        &self,
        key: BlockKey,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<u64> {
        self.state.lock().await.puts.push(PutCall {
            key,
            offset,
            bytes: data.to_vec(),
        });
        self.put_started.notify_one();
        if self.hold_put.load(Ordering::Acquire) {
            self.release_put.acquire().await?.forget();
        }
        if self.fail_put.load(Ordering::Acquire) || self.fail_next_put.swap(false, Ordering::AcqRel)
        {
            anyhow::bail!("injected object PUT failure");
        }

        let start = usize::try_from(offset)?;
        let end = start
            .checked_add(data.len())
            .ok_or_else(|| anyhow::anyhow!("object range overflow"))?;
        let mut state = self.state.lock().await;
        let block = state.blocks.entry(key).or_default();
        if block.len() < end {
            block.resize(end, 0);
        }
        block[start..end].copy_from_slice(data);
        Ok(data.len() as u64)
    }

    async fn read_range(&self, key: BlockKey, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let start = usize::try_from(offset)?;
        let mut state = self.state.lock().await;
        state.reads.push(ReadCall {
            key,
            offset,
            len: buf.len(),
        });
        let block = state
            .blocks
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("missing object block {key:?}"))?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| anyhow::anyhow!("object range overflow"))?;
        anyhow::ensure!(end <= block.len(), "short object block read");
        buf.copy_from_slice(&block[start..end]);
        Ok(())
    }

    async fn delete_range(&self, key: BlockKey, block_count: u64) -> anyhow::Result<()> {
        let (chunk_id, first) = key;
        let end = first.saturating_add(block_count as u32);
        let mut state = self.state.lock().await;
        for index in first..end {
            state.blocks.remove(&(chunk_id, index));
        }
        Ok(())
    }
}

struct Fixture {
    meta: Arc<TestMeta>,
    store: Arc<RecordingStore>,
    backend: Arc<TestBackend>,
    writer: Arc<TestWriter>,
    inode: Arc<Inode>,
    layout: ChunkLayout,
}

async fn fixture(window: Duration, hold_put: bool, fail_put: bool) -> Fixture {
    let store = Arc::new(RecordingStore::new(hold_put, fail_put));
    let metadata_store = Arc::new(
        SqliteWorkspaceStore::connect("sqlite::memory:")
            .await
            .unwrap(),
    );
    metadata_store.initialize_workspace_schema().await.unwrap();
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(0x7771));
    let workspace = metadata_store
        .create_volume_root(CreateVolumeRoot {
            upper_bucket: None,
            volume_format: "workspace".into(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            volume_id: Uuid::from_u128(0x7772),
            workspace_id,
            root_layer_id: LayerId::from_uuid(Uuid::from_u128(0x7773)),
            writable_layer_id: LayerId::from_uuid(Uuid::from_u128(0x7774)),
            owner_id: Some("native-writer-test".into()),
        })
        .await
        .unwrap();
    let lease = metadata_store
        .acquire_lease(AcquireLease {
            workspace_id,
            lease_id: LeaseId::from_uuid(Uuid::from_u128(0x7775)),
            holder_generation: 1,
            ttl_ns: 60_000_000_000,
        })
        .await
        .unwrap();
    let meta = Arc::new(WorkspaceMetaLayer::with_chunk_size(
        metadata_store,
        ViewContext {
            workspace_id,
            head_layer_id: workspace.head_layer_id,
            head_epoch: workspace.head_epoch,
            lease_id: lease.lease_id,
            holder_generation: lease.holder_generation,
        },
        CHUNK_SIZE,
    ));
    let provider: Arc<dyn WorkspaceReadPlanProvider> = meta.clone();
    let backend = Arc::new(Backend::new_workspace(
        store.clone(),
        meta.clone(),
        provider,
    ));
    let layout = ChunkLayout {
        chunk_size: CHUNK_SIZE,
        block_size: BLOCK_SIZE,
    };
    let ino = meta
        .create_file(meta.root_ino(), "native-writer-test".into())
        .await
        .unwrap();
    let inode = Inode::new(ino, 0);
    let reader = Arc::new(DataReader::new(
        Arc::new(ReadConfig::new(layout)),
        backend.clone(),
    ));
    let config = Arc::new(
        WriteConfig::new(layout)
            .page_size(PAGE as u32)
            .freeze_min_bytes(BLOCK_SIZE as u64)
            .auto_flush_max_age(Duration::from_secs(60))
            .native_write_coalesce_window(window)
            .writeback_mode(WriteBackMode::UploadBeforeCommit),
    );
    let writer = Arc::new(FileWriter::new(
        inode.clone(),
        config,
        backend.clone(),
        reader,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        None,
    ));

    Fixture {
        meta,
        store,
        backend,
        writer,
        inode,
        layout,
    }
}

async fn cold_read(f: &Fixture, len: usize) -> Vec<u8> {
    let reader = DataReader::new(Arc::new(ReadConfig::new(f.layout)), f.backend.clone());
    reader
        .open_for_handle(f.inode.clone(), 0xabc)
        .read(0, len)
        .await
        .unwrap()
}

async fn hold_all_upload_permits(f: &Fixture) -> tokio::sync::OwnedSemaphorePermit {
    let permits = f.writer.shared.upload_limit.available_permits();
    assert!(permits > 0, "fixture should have upload permits available");
    f.writer
        .shared
        .upload_limit
        .clone()
        .acquire_many_owned(u32::try_from(permits).expect("permit count fits u32"))
        .await
        .expect("upload semaphore should remain open")
}

async fn seal_native_groups(f: &Fixture) {
    let mut inner = f.writer.shared.inner.lock().await;
    let groups = inner
        .native_groups
        .as_mut()
        .expect("positive window enables native groups")
        .seal_all();
    super::native_upload::seal(groups);
}

async fn wait_for_native_metadata(f: &Fixture) {
    timeout(Duration::from_secs(5), async {
        while f
            .writer
            .shared
            .recent_pending_upload
            .metadata_inflight
            .load(Ordering::Acquire)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the native PUT should finish and enter metadata publication");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_native_metadata_allows_other_blocks_and_preserves_overwrites() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held = f.meta.hold_mutations().await;
    f.writer.write_at(0, &[0x11; PAGE]).await.unwrap();
    seal_native_groups(&f).await;
    wait_for_native_metadata(&f).await;
    let admitted = timeout(Duration::from_secs(2), async {
        f.writer.write_at(CHUNK_SIZE, &[0x22; PAGE]).await.unwrap();
        f.writer
            .write_at(u64::from(BLOCK_SIZE), &[0x33; PAGE])
            .await
            .unwrap();
        f.writer.write_at(0, &[0x44; PAGE]).await.unwrap();
    })
    .await;
    drop(held);
    admitted.expect("writes to other blocks must finish while native metadata is delayed");
    f.writer.flush().await.unwrap();
    let mut expected = vec![0; CHUNK_SIZE as usize + PAGE];
    expected[..PAGE].fill(0x44);
    expected[BLOCK_SIZE as usize..BLOCK_SIZE as usize + PAGE].fill(0x33);
    expected[CHUNK_SIZE as usize..].fill(0x22);
    assert_eq!(cold_read(&f, expected.len()).await, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_native_metadata_is_drained_before_truncate() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held = f.meta.hold_mutations().await;
    f.writer.write_at(0, &[0x41; PAGE]).await.unwrap();
    f.writer.write_at(CHUNK_SIZE, &[0x52; PAGE]).await.unwrap();
    seal_native_groups(&f).await;
    wait_for_native_metadata(&f).await;
    let ino = f.inode.ino();
    let manager = super::DataWriter::new(
        f.writer.shared.config.clone(),
        f.backend.clone(),
        f.writer.shared.reader.clone(),
        None,
    );
    manager.files.insert(ino as u64, f.writer.clone());
    let drain = manager.flush_required_for_truncate(ino as u64);
    tokio::pin!(drain);
    assert!(
        timeout(Duration::from_millis(100), &mut drain)
            .await
            .is_err()
    );
    drop(held);
    timeout(Duration::from_secs(5), &mut drain)
        .await
        .unwrap()
        .unwrap();
    f.meta.truncate(ino, PAGE as u64, CHUNK_SIZE).await.unwrap();
    manager.clear(ino as u64).await;
    f.inode.set_size(PAGE as u64);
    f.inode.bump_data_epoch();
    assert_eq!(cold_read(&f, PAGE).await, vec![0x41; PAGE]);
    f.meta
        .truncate(ino, CHUNK_SIZE + PAGE as u64, CHUNK_SIZE)
        .await
        .unwrap();
    f.inode.set_size(CHUNK_SIZE + PAGE as u64);
    let mut expected = vec![0; CHUNK_SIZE as usize + PAGE];
    expected[..PAGE].fill(0x41);
    assert_eq!(cold_read(&f, expected.len()).await, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_window_uses_the_original_single_slice_upload_path() {
    let f = fixture(Duration::ZERO, false, false).await;
    assert!(f.writer.shared.inner.lock().await.native_groups.is_none());

    let page = vec![0xa7; PAGE];
    f.writer.write_at(PAGE as u64, &page).await.unwrap();
    timeout(Duration::from_secs(2), f.writer.flush())
        .await
        .expect("zero-window flush should finish without a coalescing timer")
        .unwrap();

    let puts = f.store.puts().await;
    assert_eq!(puts.len(), 1, "the normal slice should produce one PUT");
    assert_eq!(puts[0].offset, 0);
    assert_eq!(puts[0].bytes, page);

    let actual = cold_read(&f, PAGE * 2).await;
    let mut expected = vec![0; PAGE * 2];
    expected[PAGE..].copy_from_slice(&page);
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positive_window_flush_forces_one_put_waits_for_it_and_cold_reads_packed_offsets() {
    // This exercises FileWriter::flush, the writer layer used by FUSE fsync,
    // without mounting the FUSE callback. The metadata receipt hook pauses the
    // test-only WorkspaceMetaLayer response after its store flush completes.
    let window = Duration::from_secs(30);
    let f = fixture(window, true, false).await;
    assert!(f.writer.shared.inner.lock().await.native_groups.is_some());

    let first = vec![0x31; PAGE];
    let second = vec![0xd4; PAGE];
    f.writer.write_at(0, &first).await.unwrap();
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    assert!(
        f.store.puts().await.is_empty(),
        "the positive window should hold both writes"
    );
    let (metadata_flush_reached, release_metadata_flush) = f.meta.hold_next_flush_receipt();

    let writer = f.writer.clone();
    let flush_task = tokio::spawn(async move { writer.flush().await });
    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("flush should seal the group without waiting for the 30-second deadline");
    tokio::task::yield_now().await;
    assert!(
        !flush_task.is_finished(),
        "flush returned before the gated object PUT was acknowledged"
    );
    f.store.release_puts();
    timeout(Duration::from_secs(2), metadata_flush_reached)
        .await
        .expect("writer should reach the metadata flush receipt after the PUT")
        .expect("metadata flush receipt sender should remain alive");
    assert!(
        !flush_task.is_finished(),
        "flush returned before the metadata flush receipt was released"
    );
    release_metadata_flush
        .send(())
        .expect("metadata flush task should still be waiting for its receipt");
    timeout(Duration::from_secs(5), flush_task)
        .await
        .expect("flush should finish after the object PUT is released")
        .unwrap()
        .unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        1,
        "two 4 KiB members should share one object PUT"
    );
    assert_eq!(puts[0].offset, 0);
    assert_eq!(puts[0].bytes.len(), PAGE * 2);
    let mut packed = first.clone();
    packed.extend_from_slice(&second);
    assert_eq!(puts[0].bytes, packed);

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&first);
    expected[PAGE * 2..].copy_from_slice(&second);
    assert_eq!(
        actual, expected,
        "cold read should preserve the unwritten file hole"
    );
    let reads = f.store.reads().await;
    assert!(
        reads
            .iter()
            .any(|read| { read.key == puts[0].key && read.offset == 0 && read.len == PAGE })
    );
    assert!(
        reads.iter().any(|read| {
            read.key == puts[0].key && read.offset == PAGE as u64 && read.len == PAGE
        }),
        "the second file page must cold-read from its packed object offset"
    );

    // Ensure the metadata extent is visible when the writer-level flush returns.
    let plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    assert!(matches!(
        plan.segments.as_slice(),
        [
            ReadPlanSegment::Data {
                logical_offset: 0,
                length: 4096,
                slice_offset: 0,
                ..
            },
            ReadPlanSegment::Zero {
                logical_offset: 4096,
                length: 4096,
            },
            ReadPlanSegment::Data {
                logical_offset: 8192,
                length: 4096,
                slice_offset: 4096,
                ..
            },
        ]
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_positive_window_put_returns_error_without_publishing_object_extents() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let original = vec![0x79; OUTPUT_LEN];
    f.writer.write_at(0, &original).await.unwrap();
    f.writer.flush().await.unwrap();
    let original_plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    let initial_puts = f.store.puts().await.len();
    let initial_blocks = f.store.stored_block_count().await;
    f.store.fail_put.store(true, Ordering::Release);
    let first = vec![0x52; PAGE];
    let second = vec![0xa9; PAGE];
    f.writer.write_at(0, &first).await.unwrap();
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    assert_eq!(f.store.puts().await.len(), initial_puts);

    let result = timeout(Duration::from_secs(5), f.writer.flush())
        .await
        .expect("failed PUT retries should complete within the bounded retry backoff");
    assert!(
        result.is_err(),
        "flush must return the failed native upload"
    );
    assert!(
        f.store.puts().await.len() > initial_puts,
        "the native PUT path should be attempted"
    );
    assert_eq!(f.store.stored_block_count().await, initial_blocks);

    let plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    assert_eq!(plan, original_plan);
    assert_eq!(cold_read(&f, OUTPUT_LEN).await, original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_groups_queued_for_a_permit_combine_and_flush_waits_for_metadata() {
    let f = fixture(Duration::from_secs(30), true, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    let first = vec![0x28; PAGE];
    let second = vec![0xb6; PAGE];

    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    seal_native_groups(&f).await;
    assert!(f.store.puts().await.is_empty());

    let (metadata_flush_reached, release_metadata_flush) = f.meta.hold_next_flush_receipt();
    let writer = f.writer.clone();
    let flush_task = tokio::spawn(async move { writer.flush().await });
    tokio::task::yield_now().await;
    assert!(f.store.puts().await.is_empty());
    drop(held_permits);

    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("a queued native group should start after an upload permit is released");
    assert_eq!(f.store.puts().await.len(), 1);
    assert!(
        !flush_task.is_finished(),
        "flush returned before the combined object PUT completed"
    );

    f.store.release_puts();
    timeout(Duration::from_secs(2), metadata_flush_reached)
        .await
        .expect("flush should wait for metadata after the object PUT")
        .expect("metadata receipt sender should remain alive");
    assert!(
        !flush_task.is_finished(),
        "flush returned before the metadata receipt was released"
    );
    release_metadata_flush
        .send(())
        .expect("metadata flush should still be waiting");
    timeout(Duration::from_secs(5), flush_task)
        .await
        .expect("flush should finish after the metadata receipt")
        .unwrap()
        .unwrap();

    let puts = f.store.puts().await;
    assert_eq!(puts.len(), 1, "two sealed groups should share one PUT");
    let mut packed = first.clone();
    packed.extend_from_slice(&second);
    assert_eq!(puts[0].offset, 0);
    assert_eq!(puts[0].bytes, packed);

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&first);
    expected[PAGE * 2..].copy_from_slice(&second);
    assert_eq!(
        actual, expected,
        "cold read should preserve the sparse hole"
    );
    let reads = f.store.reads().await;
    assert!(
        reads
            .iter()
            .any(|read| { read.key == puts[0].key && read.offset == 0 && read.len == PAGE })
    );
    assert!(
        reads.iter().any(|read| {
            read.key == puts[0].key && read.offset == PAGE as u64 && read.len == PAGE
        })
    );

    let plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    assert!(matches!(
        plan.segments.as_slice(),
        [
            ReadPlanSegment::Data {
                logical_offset: 0,
                length: 4096,
                slice_offset: 0,
                ..
            },
            ReadPlanSegment::Zero {
                logical_offset: 4096,
                length: 4096,
            },
            ReadPlanSegment::Data {
                logical_offset: 8192,
                length: 4096,
                slice_offset: 4096,
                ..
            },
        ]
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn started_put_members_stay_unchanged_while_later_sealed_groups_combine() {
    let f = fixture(Duration::from_secs(30), true, false).await;
    let first = vec![0x19; PAGE];
    let second = vec![0x47; PAGE];
    let third = vec![0xe2; PAGE];

    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("the first sealed group should start its PUT");
    let first_put = f.store.puts().await[0].clone();
    assert_eq!(first_put.bytes, first);

    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at((PAGE * 4) as u64, &third).await.unwrap();
    seal_native_groups(&f).await;
    assert_eq!(
        f.store.puts().await.as_slice(),
        std::slice::from_ref(&first_put),
        "the active PUT should retain its original bytes while later groups queue"
    );

    let writer = f.writer.clone();
    let flush_task = tokio::spawn(async move { writer.flush().await });
    f.store.release_puts();
    timeout(Duration::from_secs(5), flush_task)
        .await
        .expect("flush should finish after both native objects complete")
        .unwrap()
        .unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        2,
        "the first PUT is fixed and the next two groups combine"
    );
    assert_eq!(puts[0].key, first_put.key);
    assert_eq!(puts[0].offset, first_put.offset);
    assert_eq!(puts[0].bytes, first_put.bytes);
    let mut packed_tail = second.clone();
    packed_tail.extend_from_slice(&third);
    assert_eq!(puts[1].offset, 0);
    assert_eq!(puts[1].bytes, packed_tail);

    let actual = cold_read(&f, PAGE * 5).await;
    let mut expected = vec![0; PAGE * 5];
    expected[..PAGE].copy_from_slice(&first);
    expected[PAGE * 2..PAGE * 3].copy_from_slice(&second);
    expected[PAGE * 4..PAGE * 5].copy_from_slice(&third);
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_offset_stops_a_prefix_and_preserves_newer_write_order() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    let first = vec![0x32; PAGE];
    let overwrite = vec![0xa1; PAGE];
    let next = vec![0xc8; PAGE];

    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at(0, &overwrite).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at(PAGE as u64, &next).await.unwrap();
    seal_native_groups(&f).await;
    drop(held_permits);
    f.writer.flush().await.unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        2,
        "the duplicate offset should split the prefix"
    );
    assert_eq!(puts[0].bytes, first);
    let mut packed = overwrite.clone();
    packed.extend_from_slice(&next);
    assert_eq!(puts[1].offset, 0);
    assert_eq!(puts[1].bytes, packed);

    let actual = cold_read(&f, PAGE * 2).await;
    let mut expected = vec![0; PAGE * 2];
    expected[..PAGE].copy_from_slice(&overwrite);
    expected[PAGE..].copy_from_slice(&next);
    assert_eq!(
        actual, expected,
        "the later write should replace the older extent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_groups_from_distinct_epochs_stay_in_separate_objects() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    let first = vec![0x36; PAGE];
    let second = vec![0xc2; PAGE];

    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    f.inode.bump_data_epoch();
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    seal_native_groups(&f).await;
    drop(held_permits);
    f.writer.flush().await.unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        2,
        "groups from different epochs must use separate PUTs"
    );
    assert_ne!(puts[0].key, puts[1].key);
    assert_eq!(puts[0].bytes, first);
    assert_eq!(puts[1].bytes, second);
    assert_eq!(puts[0].bytes.len(), PAGE);
    assert_eq!(puts[1].bytes.len(), PAGE);

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&first);
    expected[PAGE * 2..].copy_from_slice(&second);
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_combined_put_keeps_old_extents_and_file_data() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let original = vec![0x79; OUTPUT_LEN];
    f.writer.write_at(0, &original).await.unwrap();
    f.writer.flush().await.unwrap();
    let original_plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    let initial_puts = f.store.puts().await.len();
    let initial_blocks = f.store.stored_block_count().await;
    let held_permits = hold_all_upload_permits(&f).await;
    f.store.fail_put.store(true, Ordering::Release);
    let first = vec![0x52; PAGE];
    let second = vec![0xa9; PAGE];
    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    seal_native_groups(&f).await;
    drop(held_permits);

    let result = timeout(Duration::from_secs(5), f.writer.flush())
        .await
        .expect("failed combined PUT retries should finish within the retry budget");
    assert!(
        result.is_err(),
        "flush should return the combined PUT failure"
    );

    let puts = f.store.puts().await;
    let attempts = &puts[initial_puts..];
    assert!(
        !attempts.is_empty(),
        "the combined object should be attempted"
    );
    let expected_key = attempts[0].key;
    let mut expected_put = first;
    expected_put.extend_from_slice(&second);
    assert!(
        attempts
            .iter()
            .all(|put| put.key == expected_key && put.offset == 0 && put.bytes == expected_put),
        "every retry should preserve the same combined object bytes"
    );
    assert_eq!(f.store.stored_block_count().await, initial_blocks);

    let plan = f
        .meta
        .read_plan(f.inode.ino() as i64, 0, 0, OUTPUT_LEN as u64)
        .await
        .unwrap();
    assert_eq!(plan, original_plan);
    assert_eq!(cold_read(&f, OUTPUT_LEN).await, original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_after_one_put_failure_reuses_the_same_combined_object_bytes() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    f.store.fail_next_put.store(true, Ordering::Release);
    let first = vec![0x5d; PAGE];
    let second = vec![0x8b; PAGE];
    f.writer.write_at(0, &first).await.unwrap();
    seal_native_groups(&f).await;
    f.writer.write_at((PAGE * 2) as u64, &second).await.unwrap();
    seal_native_groups(&f).await;
    drop(held_permits);
    f.writer.flush().await.unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        2,
        "one injected failure should cause exactly one retry"
    );
    let expected_key = puts[0].key;
    let mut expected_put = first.clone();
    expected_put.extend_from_slice(&second);
    assert!(
        puts.iter()
            .all(|put| put.key == expected_key && put.offset == 0 && put.bytes == expected_put)
    );

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&first);
    expected[PAGE * 2..].copy_from_slice(&second);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn snapshot_flush_returns_while_cached_write_remains_pending() {
    let f = fixture(Duration::from_secs(30), true, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    let first = vec![0x44; PAGE];
    let second = vec![0xd1; PAGE];
    f.writer.write_at((PAGE * 2) as u64, &first).await.unwrap();
    seal_native_groups(&f).await;

    let mut flush_future = Box::pin(f.writer.flush_snapshot());
    assert!(matches!(
        futures::poll!(flush_future.as_mut()),
        std::task::Poll::Pending
    ));
    f.writer.write_at_cached(0, &second, 0).await.unwrap();
    drop(held_permits);
    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("initial native PUT should begin");
    f.store.release_one_put();
    timeout(Duration::from_secs(5), flush_future)
        .await
        .expect("snapshot flush should complete its original snapshot")
        .unwrap();
    assert!(
        f.writer.has_pending().await,
        "the cached write added during flush should remain pending after the snapshot flush"
    );

    let writer = f.writer.clone();
    let second_flush =
        tokio::spawn(async move { writer.flush_with_deadline(Duration::from_secs(5)).await });
    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("the second flush should freeze and upload the cached write");

    f.store.release_puts();
    timeout(Duration::from_secs(5), second_flush)
        .await
        .expect("a follow-up flush should commit the cached write")
        .unwrap()
        .unwrap();

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&second);
    expected[PAGE * 2..].copy_from_slice(&first);
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_with_deadline_chases_a_cached_write_added_during_metadata_wait() {
    let f = fixture(Duration::from_secs(30), true, false).await;
    let first = vec![0x44; PAGE];
    let second = vec![0xd1; PAGE];
    f.writer.write_at((PAGE * 2) as u64, &first).await.unwrap();
    seal_native_groups(&f).await;
    let (first_flush_reached, release_first_flush) = f.meta.hold_next_flush_receipt();

    let writer = f.writer.clone();
    let flush_task =
        tokio::spawn(async move { writer.flush_with_deadline(Duration::from_secs(5)).await });
    timeout(Duration::from_secs(2), f.store.wait_for_put_start())
        .await
        .expect("initial native PUT should begin");
    f.store.release_puts();
    timeout(Duration::from_secs(2), first_flush_reached)
        .await
        .expect("initial flush should reach its metadata receipt")
        .expect("initial metadata receipt sender should remain alive");

    let (second_flush_reached, release_second_flush) = f.meta.hold_next_flush_receipt();
    f.writer.write_at_cached(0, &second, 0).await.unwrap();
    release_first_flush
        .send(())
        .expect("first metadata receipt should still be waiting");

    timeout(Duration::from_secs(2), second_flush_reached)
        .await
        .expect("flush should capture and commit the write appended during the first receipt")
        .expect("second metadata receipt sender should remain alive");
    assert!(
        !flush_task.is_finished(),
        "flush should wait for the metadata receipt covering the appended write"
    );
    release_second_flush
        .send(())
        .expect("second metadata receipt should still be waiting");
    timeout(Duration::from_secs(5), flush_task)
        .await
        .expect("flush should finish after the appended write is published")
        .unwrap()
        .unwrap();

    let actual = cold_read(&f, OUTPUT_LEN).await;
    let mut expected = vec![0; OUTPUT_LEN];
    expected[..PAGE].copy_from_slice(&second);
    expected[PAGE * 2..].copy_from_slice(&first);
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coalesced_prefix_respects_the_block_capacity_of_sixteen_pages() {
    let f = fixture(Duration::from_secs(30), false, false).await;
    let held_permits = hold_all_upload_permits(&f).await;
    let pages: Vec<Vec<u8>> = (0..17)
        .map(|index| vec![u8::try_from(index + 1).unwrap(); PAGE])
        .collect();

    for (index, page) in pages.iter().enumerate() {
        f.writer
            .write_at((index * PAGE) as u64, page)
            .await
            .unwrap();
        seal_native_groups(&f).await;
    }
    drop(held_permits);
    f.writer.flush().await.unwrap();

    let puts = f.store.puts().await;
    assert_eq!(
        puts.len(),
        2,
        "a 16-page block should split 17 pages into two PUTs"
    );
    let expected_first: Vec<u8> = pages[..16]
        .iter()
        .flat_map(|page| page.iter().copied())
        .collect();
    assert_eq!(puts[0].bytes.len(), BLOCK_SIZE as usize);
    assert_eq!(puts[0].bytes, expected_first);
    assert_eq!(puts[1].offset, 0);
    assert_eq!(puts[1].bytes, pages[16]);

    let actual = cold_read(&f, PAGE * pages.len()).await;
    assert_eq!(actual, pages.into_iter().flatten().collect::<Vec<_>>());
}
