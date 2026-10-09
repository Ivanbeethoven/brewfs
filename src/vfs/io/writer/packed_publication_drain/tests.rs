use super::*;
use crate::chunk::store::{BlockKey, InMemoryBlockStore};
use crate::meta::factory::create_meta_store_from_url;
use crate::vfs::config::ReadConfig;

struct HeldUploadStore {
    inner: InMemoryBlockStore,
    blocked: AtomicBool,
    fail: AtomicBool,
    notify: Notify,
}
impl HeldUploadStore {
    fn release(&self) {
        self.blocked.store(false, Ordering::Release);
        self.notify.notify_waiters();
    }
}
#[async_trait::async_trait]
impl BlockStore for HeldUploadStore {
    async fn write_fresh_range(
        &self,
        key: BlockKey,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<u64> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.blocked.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        if self.fail.load(Ordering::Acquire) {
            anyhow::bail!("injected terminal object upload failure");
        }
        self.inner.write_fresh_range(key, offset, data).await
    }
    async fn read_range(&self, key: BlockKey, offset: u64, out: &mut [u8]) -> anyhow::Result<()> {
        self.inner.read_range(key, offset, out).await
    }
    async fn delete_range(&self, key: BlockKey, count: u64) -> anyhow::Result<()> {
        self.inner.delete_range(key, count).await
    }
}

async fn fixture() -> (
    Arc<DataWriter<HeldUploadStore, impl MetaLayer>>,
    Arc<HeldUploadStore>,
    Arc<Inode>,
) {
    let layout = crate::chunk::ChunkLayout {
        chunk_size: 8192,
        block_size: 4096,
    };
    let store = Arc::new(HeldUploadStore {
        inner: InMemoryBlockStore::new(),
        blocked: AtomicBool::new(true),
        fail: AtomicBool::new(false),
        notify: Notify::new(),
    });
    let meta = create_meta_store_from_url("sqlite::memory:")
        .await
        .unwrap()
        .layer();
    let ino = meta.create_file(1, "publication.bin".into()).await.unwrap();
    let backend = Arc::new(Backend::new(store.clone(), meta));
    let reader = Arc::new(DataReader::new(
        Arc::new(ReadConfig::new(layout)),
        backend.clone(),
    ));
    let mut config = WriteConfig::new(layout)
        .page_size(4096)
        .freeze_min_bytes(8192)
        .auto_flush_max_age(Duration::from_secs(3600))
        .writeback_mode(WriteBackMode::CommitBeforeUpload);
    config.writeback_require_stage_before_commit = false;
    let writer = Arc::new(
        DataWriter::new(Arc::new(config), backend, reader, None)
            .with_packed_publication_tracking(true),
    );
    (writer, store, Inode::new(ino, 0))
}

#[tokio::test]
async fn packed_publication_waits_real_remote_upload_after_metadata_flush_returned() {
    let (writer, store, inode) = fixture().await;
    let file = writer.ensure_file(inode.clone());
    let data = vec![0x57; 2048];
    file.write_at(0, &data).await.unwrap();
    timeout(Duration::from_secs(2), file.flush())
        .await
        .unwrap()
        .unwrap();
    assert!(!file.has_pending().await);
    assert!(file.has_overlay_state().await);
    let mut drain = Box::pin(writer.drain_for_packed_publication(256));
    assert!(
        timeout(Duration::from_millis(20), drain.as_mut())
            .await
            .is_err()
    );
    store.release();
    let proof = timeout(Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proof.inodes(), &[inode.ino()]);
    let reader = writer.reader.open_for_handle(inode, 9);
    assert_eq!(reader.read(0, data.len()).await.unwrap(), data);
    assert_eq!(writer.recent_pending_upload_bytes(), 0);
}

#[tokio::test]
async fn packed_publication_waits_upload_owner_from_writer_removed_before_snapshot() {
    let (writer, store, inode) = fixture().await;
    let file = writer.ensure_file(inode.clone());
    file.write_at(0, &[0x37; 2048]).await.unwrap();
    timeout(Duration::from_secs(2), file.flush())
        .await
        .unwrap()
        .unwrap();
    writer.discard(inode.ino() as u64).await;
    drop(file);
    assert!(!writer.has_file(inode.ino() as u64));
    let mut drain = Box::pin(writer.drain_for_packed_publication(256));
    assert!(
        timeout(Duration::from_millis(20), drain.as_mut())
            .await
            .is_err()
    );
    store.release();
    timeout(Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(writer.recent_pending_upload_bytes(), 0);
}

#[tokio::test]
async fn packed_publication_rejects_terminal_upload_failure_from_removed_writer() {
    let (writer, store, inode) = fixture().await;
    let file = writer.ensure_file(inode.clone());
    file.write_at(0, &[0x61; 2048]).await.unwrap();
    timeout(Duration::from_secs(2), file.flush())
        .await
        .unwrap()
        .unwrap();
    assert!(!file.has_pending().await);
    assert!(file.has_overlay_state().await);
    writer.discard(inode.ino() as u64).await;
    assert!(!writer.has_file(inode.ino() as u64));
    store.fail.store(true, Ordering::Release);
    store.release();
    // Observe the real uploader exhaust its retries; a lost map entry must
    // not turn that terminal error into a completed publication boundary.
    timeout(Duration::from_secs(2), async {
        loop {
            if file.shared.writeback_error().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(file);
    assert!(
        timeout(
            Duration::from_secs(2),
            writer.drain_for_packed_publication(256)
        )
        .await
        .unwrap()
        .is_err(),
        "terminal upload failure vanished with its removed writer"
    );
}

#[tokio::test]
async fn packed_writer_owner_is_registered_before_queued_task_runs() {
    let tasks = PackedWriterTasks::new();
    tasks.enabled.store(true, Ordering::Release);
    let (release, wait) = tokio::sync::oneshot::channel();
    spawn_writer_owned(&tasks, async move {
        let _ = wait.await;
    });
    assert_eq!(tasks.active.load(Ordering::Acquire), 1);
    assert!(tasks.wait_idle(Duration::from_millis(20)).await.is_err());
    release.send(()).unwrap();
    tasks.wait_idle(Duration::from_secs(2)).await.unwrap();
}
