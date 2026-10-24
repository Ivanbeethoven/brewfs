use crate::chunk::compact::{BlockGcConfig, BlockStoreGC, CompactResult, Compactor};
use crate::chunk::{BlockStore, ChunkLayout};
use crate::meta::config::{CompactConfig, LockTtlConfig};
use crate::meta::store::{LockName, MetaError, MetaStore};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

type CompactionHook = Arc<dyn Fn(u64) + Send + Sync>;

pub struct ChunkLockGuard<M: MetaStore + ?Sized + 'static> {
    chunk_id: u64,
    locked_chunks: Arc<RwLock<HashSet<u64>>>,
    meta_store: Arc<M>,
    unlocked: bool,
}

impl<M: MetaStore + ?Sized + 'static> ChunkLockGuard<M> {
    fn new(chunk_id: u64, locked_chunks: Arc<RwLock<HashSet<u64>>>, meta_store: Arc<M>) -> Self {
        Self {
            chunk_id,
            locked_chunks,
            meta_store,
            unlocked: false,
        }
    }

    pub async fn unlock(&mut self) {
        if self.unlocked {
            return;
        }
        self.unlocked = true;

        // Best-effort explicit release on the normal path. Crash paths still
        // rely on TTL expiry for self-healing.
        let released = self
            .meta_store
            .release_global_lock(LockName::ChunkCompactLock(self.chunk_id))
            .await;
        if !released {
            warn!(
                chunk_id = self.chunk_id,
                "Failed to release global compact lock explicitly, TTL fallback applies"
            );
        }

        let mut locked = self.locked_chunks.write().await;
        locked.remove(&self.chunk_id);
    }
}

impl<M: MetaStore + ?Sized + 'static> Drop for ChunkLockGuard<M> {
    fn drop(&mut self) {
        if !self.unlocked {
            let chunk_id = self.chunk_id;
            let locked_chunks = Arc::clone(&self.locked_chunks);
            let meta_store = Arc::clone(&self.meta_store);
            tokio::spawn(async move {
                // Best-effort release of global lock on unexpected drop (panic, early return).
                // TTL expiry is still the ultimate fallback for crash scenarios.
                let _ = meta_store
                    .release_global_lock(LockName::ChunkCompactLock(chunk_id))
                    .await;
                let mut locked = locked_chunks.write().await;
                locked.remove(&chunk_id);
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompactionWorkerConfig {
    pub scan_interval: Duration,
    pub max_chunks_per_run: usize,
    pub enabled: bool,
}

impl Default for CompactionWorkerConfig {
    fn default() -> Self {
        Self {
            // Scan every 10 minutes instead of every hour so fragmentation is
            // caught sooner.  Scanning is cheap — it only queries metadata.
            scan_interval: Duration::from_secs(600),
            max_chunks_per_run: 100,
            enabled: true,
        }
    }
}

/// Chunk compaction lock manager with both local and global lock support.
///
/// Uses local in-memory locks for fast checking within the same process,
/// and global locks (via MetaStore) for cross-process synchronization.
/// Supports dynamic TTL calculation based on compaction type and slice count.
#[derive(Debug)]
pub struct CompactLockManager<M: MetaStore + ?Sized> {
    locked_chunks: Arc<RwLock<HashSet<u64>>>,
    meta_store: Arc<M>,
    ttl_config: LockTtlConfig,
}

impl<M: MetaStore + ?Sized + 'static> CompactLockManager<M> {
    #[allow(dead_code)]
    pub fn new(meta_store: Arc<M>) -> Self {
        Self {
            locked_chunks: Arc::new(RwLock::new(HashSet::new())),
            meta_store,
            ttl_config: LockTtlConfig::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_ttl_config(meta_store: Arc<M>, ttl_config: LockTtlConfig) -> Self {
        Self {
            locked_chunks: Arc::new(RwLock::new(HashSet::new())),
            meta_store,
            ttl_config,
        }
    }

    /// Try to acquire lock for chunk compaction with dynamic TTL.
    ///
    /// # Arguments
    /// * `chunk_id` - The chunk ID to lock
    /// * `slice_count` - Number of slices in the chunk (used for TTL calculation)
    /// * `is_sync` - Whether this is a sync compaction (needs longer TTL)
    pub async fn try_lock(
        &self,
        chunk_id: u64,
        slice_count: usize,
        is_sync: bool,
    ) -> Option<ChunkLockGuard<M>> {
        // Calculate dynamic TTL based on compaction type and slice count
        let ttl_secs = self.ttl_config.calculate_ttl(is_sync, slice_count);

        {
            let mut locked = self.locked_chunks.write().await;
            if locked.contains(&chunk_id) {
                return None;
            }
            locked.insert(chunk_id);
        }

        let global_acquired = self
            .meta_store
            .get_global_lock(LockName::ChunkCompactLock(chunk_id), ttl_secs)
            .await;

        if !global_acquired {
            let mut locked = self.locked_chunks.write().await;
            locked.remove(&chunk_id);
            return None;
        }

        info!(
            chunk_id,
            ttl_secs, slice_count, is_sync, "Acquired compact lock with dynamic TTL"
        );

        Some(ChunkLockGuard::new(
            chunk_id,
            Arc::clone(&self.locked_chunks),
            Arc::clone(&self.meta_store),
        ))
    }

    #[allow(dead_code)]
    pub async fn is_locally_locked(&self, chunk_id: u64) -> bool {
        let locked = self.locked_chunks.read().await;
        locked.contains(&chunk_id)
    }
}

pub struct CompactionWorker<M, B>
where
    M: MetaStore + ?Sized,
{
    meta_store: Arc<M>,
    compactor: Arc<Compactor<B, M>>,
    lock_manager: Arc<CompactLockManager<M>>,
    compaction_hook: Option<CompactionHook>,
}

impl<M: ?Sized, B> CompactionWorker<M, B>
where
    M: MetaStore + Send + Sync + 'static,
    B: BlockStore + Send + Sync + 'static,
{
    #[allow(dead_code)]
    pub fn new(meta_store: Arc<M>, block_store: Arc<B>) -> Self {
        Self::with_config(
            meta_store,
            block_store,
            ChunkLayout::default(),
            CompactConfig::default(),
            LockTtlConfig::default(),
        )
    }

    pub fn with_config(
        meta_store: Arc<M>,
        block_store: Arc<B>,
        layout: ChunkLayout,
        compact_config: CompactConfig,
        lock_ttl_config: LockTtlConfig,
    ) -> Self {
        let compactor: Arc<Compactor<B, M>> = Arc::new(Compactor::with_config(
            Arc::clone(&meta_store),
            block_store,
            layout,
            compact_config,
        ));
        let lock_manager = Arc::new(CompactLockManager::with_ttl_config(
            Arc::clone(&meta_store),
            lock_ttl_config,
        ));
        Self {
            meta_store,
            compactor,
            lock_manager,
            compaction_hook: None,
        }
    }

    pub fn with_compaction_hook(mut self, hook: CompactionHook) -> Self {
        self.compaction_hook = Some(hook);
        self
    }

    pub fn start(
        self,
        worker_config: CompactionWorkerConfig,
        gc_config: BlockGcConfig,
    ) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
        let compactor = self.compactor.clone();
        let meta_store = self.meta_store.clone();
        let lock_manager = self.lock_manager.clone();
        let compaction_hook = self.compaction_hook.clone();
        let compaction_handle = tokio::spawn(async move {
            if !worker_config.enabled {
                return;
            }

            let mut ticker = interval(worker_config.scan_interval);
            loop {
                ticker.tick().await;

                if let Err(e) = run_compaction_cycle(
                    &meta_store,
                    &compactor,
                    &lock_manager,
                    &worker_config,
                    compaction_hook.as_ref(),
                )
                .await
                {
                    error!(error = %e, "Compaction cycle failed");
                }
            }
        });
        let gc_handle = BlockStoreGC::new(self.meta_store, self.compactor.block_store().clone())
            .start(gc_config);

        (compaction_handle, gc_handle)
    }

    #[allow(dead_code)]
    pub async fn is_chunk_locally_locked(&self, chunk_id: u64) -> bool {
        self.lock_manager.is_locally_locked(chunk_id).await
    }
}

async fn run_compaction_cycle<M, B>(
    meta_store: &Arc<M>,
    compactor: &Arc<Compactor<B, M>>,
    lock_manager: &Arc<CompactLockManager<M>>,
    config: &CompactionWorkerConfig,
    compaction_hook: Option<&CompactionHook>,
) -> anyhow::Result<()>
where
    M: MetaStore + Send + Sync + ?Sized + 'static,
    B: BlockStore + Send + Sync + 'static,
{
    let chunk_ids = match meta_store.list_chunk_ids(config.max_chunks_per_run).await {
        Ok(ids) => ids,
        Err(MetaError::NotImplemented) => {
            debug!(
                "list_chunk_ids not implemented for this MetaStore backend, skipping compaction cycle"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to list chunk IDs: {}", e)),
    };

    for chunk_id in chunk_ids {
        match compactor.snapshot(chunk_id).await {
            Ok(before) => {
                let (needed, is_sync) = compactor.should_compact_snapshot(&before);
                if !needed {
                    continue;
                }
                let slice_count = before.count();
                drop(before);

                // Both sync and async compaction require the distributed lock
                // to prevent multiple nodes from compacting the same chunk.
                let mut lock_guard = lock_manager.try_lock(chunk_id, slice_count, is_sync).await;

                if lock_guard.is_none() {
                    debug!(
                        chunk_id,
                        is_sync, "Could not acquire compact lock (local or global), skipping"
                    );
                    continue;
                }

                // Re-read once after locking: another node may have compacted
                // the chunk. Reuse this snapshot through light/heavy planning;
                // versioned publication still rejects concurrent writes.
                let snapshot = match compactor.snapshot(chunk_id).await {
                    Ok(snapshot) => snapshot,
                    Err(e) => {
                        warn!(chunk_id, error = %e, "Failed to re-analyze chunk after lock");
                        if let Some(ref mut guard) = lock_guard {
                            guard.unlock().await;
                        }
                        continue;
                    }
                };

                if !compactor.should_compact_snapshot(&snapshot).0 {
                    debug!(chunk_id, "Compaction no longer needed after re-check");
                    if let Some(ref mut guard) = lock_guard {
                        guard.unlock().await;
                    }
                    continue;
                }

                let slice_count = snapshot.count();
                if is_sync {
                    info!(
                        chunk_id,
                        slice_count, "Acquired sync compact lock, blocking writes"
                    );
                } else {
                    debug!(chunk_id, slice_count, "Acquired async compact lock");
                }

                let result = compactor.compact_snapshot(chunk_id, snapshot).await;
                if let Some(ref mut guard) = lock_guard {
                    guard.unlock().await;
                }

                match result {
                    Ok(CompactResult::Skipped) => {
                        debug!(chunk_id, "Compaction skipped");
                    }
                    Ok(CompactResult::Light { removed }) => {
                        info!(chunk_id, removed, is_sync, "Light compaction completed");
                        if let Some(hook) = compaction_hook {
                            hook(chunk_id);
                        }
                    }
                    Ok(CompactResult::Heavy { .. }) => {
                        info!(chunk_id, is_sync, "Heavy compaction completed");
                        if let Some(hook) = compaction_hook {
                            hook(chunk_id);
                        }
                    }
                    Err(e) => {
                        warn!(chunk_id, error = %e, is_sync, "Compaction failed");
                    }
                }
            }
            Err(e) => {
                warn!(chunk_id, error = %e, "Error checking compaction status");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::chunk::{
        SliceDesc,
        store::{BlockKey, InMemoryBlockStore},
    };
    use crate::meta::SLICE_ID_KEY;
    use crate::{Config, DatabaseConfig, DatabaseType, RedisMetaStore};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct WriterDuringUpload {
        blocks: InMemoryBlockStore,
        meta: Arc<RedisMetaStore>,
        inject: AtomicBool,
        chunk: u64,
    }

    #[async_trait::async_trait]
    impl BlockStore for WriterDuringUpload {
        async fn write_fresh_range(
            &self,
            key: BlockKey,
            offset: u64,
            data: &[u8],
        ) -> anyhow::Result<u64> {
            if self.inject.swap(false, Ordering::SeqCst) {
                self.blocks
                    .write_fresh_range((999999, 0), 0, b"writer")
                    .await?;
                self.meta
                    .append_slice(
                        self.chunk,
                        SliceDesc {
                            slice_id: 999999,
                            chunk_id: self.chunk,
                            offset: 0,
                            length: 6,
                        },
                    )
                    .await?;
            }
            self.blocks.write_fresh_range(key, offset, data).await
        }
        async fn read_range(
            &self,
            key: BlockKey,
            offset: u64,
            buf: &mut [u8],
        ) -> anyhow::Result<()> {
            self.blocks.read_range(key, offset, buf).await
        }
        async fn delete_range(&self, key: BlockKey, count: u64) -> anyhow::Result<()> {
            self.blocks.delete_range(key, count).await
        }
    }

    async fn lrange_calls(conn: &mut redis::aio::MultiplexedConnection) -> u64 {
        let info: String = redis::cmd("INFO")
            .arg("commandstats")
            .query_async(conn)
            .await
            .unwrap();
        info.lines()
            .find_map(|line| line.strip_prefix("cmdstat_lrange:calls="))
            .map(|stats| stats.split(',').next().unwrap().parse().unwrap())
            .unwrap_or(0)
    }

    async fn exercise_cycle(light_removes: bool, concurrent_write: bool, sync: bool) {
        let url = std::env::var("BREWFS_REDIS_TEST_URL").expect("isolated Redis test URL required");
        let client = redis::Client::open(url.clone()).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        redis::cmd("FLUSHDB")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        let meta = Arc::new(
            RedisMetaStore::from_config(Config {
                database: DatabaseConfig {
                    db_config: DatabaseType::Redis { url },
                },
                cache: Default::default(),
                client: Default::default(),
                compact: Default::default(),
            })
            .await
            .unwrap(),
        );
        let chunk = crate::vfs::chunk_id_for(42, 0).unwrap();
        let blocks = Arc::new(WriterDuringUpload {
            blocks: InMemoryBlockStore::new(),
            meta: meta.clone(),
            inject: AtomicBool::new(false),
            chunk,
        });
        let offsets: &[u64] = if light_removes {
            &[0, 0, 2048, 4096]
        } else {
            &[0, 2048, 4096]
        };
        for &offset in offsets {
            let slice_id = meta.next_id(SLICE_ID_KEY).await.unwrap() as u64;
            blocks
                .blocks
                .write_fresh_range((slice_id, 0), 0, &[7; 4096])
                .await
                .unwrap();
            meta.append_slice(
                chunk,
                SliceDesc {
                    slice_id,
                    chunk_id: chunk,
                    offset,
                    length: 4096,
                },
            )
            .await
            .unwrap();
        }
        let compact_config = CompactConfig {
            sync_threshold: if sync { 3 } else { 200 },
            ..Default::default()
        };
        let compactor = Arc::new(Compactor::with_config(
            meta.clone(),
            blocks.clone(),
            ChunkLayout {
                chunk_size: 8192,
                block_size: 4096,
            },
            compact_config,
        ));
        let locks = Arc::new(CompactLockManager::new(meta.clone()));
        blocks.inject.store(concurrent_write, Ordering::SeqCst);
        let before = lrange_calls(&mut conn).await;
        run_compaction_cycle(
            &meta,
            &compactor,
            &locks,
            &CompactionWorkerConfig::default(),
            None,
        )
        .await
        .unwrap();
        let reads = lrange_calls(&mut conn).await - before;
        // Include the publication CAS read(s), which must remain in place.
        let expected = if light_removes { 5 } else { 3 };
        assert_eq!(
            reads, expected,
            "one pre-lock and one post-lock snapshot, plus reload after light publication"
        );
        assert!(!locks.is_locally_locked(chunk).await);
        let slices = meta.get_slices(chunk).await.unwrap();
        if concurrent_write {
            assert_eq!(slices.len(), 4);
            assert!(slices.iter().any(|slice| slice.slice_id == 999999));
        } else {
            assert_eq!(slices.len(), 1);
            let mut data = vec![0; 8192];
            blocks
                .read_range((slices[0].slice_id, 0), 0, &mut data[..4096])
                .await
                .unwrap();
            blocks
                .read_range((slices[0].slice_id, 1), 0, &mut data[4096..])
                .await
                .unwrap();
            assert_eq!(data, vec![7; 8192]);
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires isolated Redis via BREWFS_REDIS_TEST_URL"]
    async fn no_op_light_uses_two_snapshots() {
        exercise_cycle(false, false, false).await;
        exercise_cycle(false, false, true).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires isolated Redis via BREWFS_REDIS_TEST_URL"]
    async fn successful_light_reloads_once_before_heavy() {
        exercise_cycle(true, false, false).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    #[ignore = "requires isolated Redis via BREWFS_REDIS_TEST_URL"]
    async fn concurrent_mutation_still_rejects_stale_snapshot() {
        exercise_cycle(false, true, false).await;
    }
}
