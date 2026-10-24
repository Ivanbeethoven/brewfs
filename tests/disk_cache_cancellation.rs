use std::sync::Arc;
use std::time::Duration;

use brewfs::chunk::cache::{ChunksCache, ChunksCacheConfig};
use bytes::Bytes;

fn cache_files(dir: &std::path::Path) -> Vec<std::fs::DirEntry> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(cache_files(&entry.path()));
        } else {
            files.push(entry);
        }
    }
    files
}

fn available_permits(cache: &ChunksCache) -> usize {
    let mut permits = Vec::new();
    while let Some(permit) = cache.try_disk_store_permit("permit-probe") {
        permits.push(permit);
    }
    permits.len()
}

struct CancellationResult {
    total_permits: usize,
    permits_after_abort: usize,
    reserved: u64,
    published: u64,
    temporary_files: usize,
    settled: bool,
}

fn cancel_store_during_blocking_write() -> CancellationResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(
            ChunksCache::new_with_config(ChunksCacheConfig::with_budgets(
                0,
                16 * 1024 * 1024,
                dir.path().to_path_buf(),
            ))
            .await
            .unwrap(),
        );
        let total_permits = available_permits(&cache);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            ready_tx.send(()).unwrap();
            // Dropping the sender also releases this worker if the test panics.
            let _ = release_rx.recv();
        });
        ready_rx.await.unwrap();

        let store_cache = cache.clone();
        let store = tokio::spawn(async move {
            store_cache
                .store_to_disk("cancelled-store", Bytes::from(vec![42; 4096]))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while cache.stats().disk_bytes == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("store must reserve bytes before its blocking write");
        assert!(!store.is_finished());
        store.abort();
        assert!(store.await.unwrap_err().is_cancelled());
        let permits_after_abort = available_permits(&cache);

        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        let mut reserved = 0;
        let mut published = 0;
        let mut temporary_files = 0;
        let settled = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                published = 0;
                temporary_files = 0;
                for entry in cache_files(dir.path()) {
                    if entry.file_name().to_string_lossy().ends_with(".tmp") {
                        temporary_files += 1;
                    } else {
                        published += entry.metadata().unwrap().len();
                    }
                }
                reserved = cache.stats().disk_bytes;
                if temporary_files == 0
                    && reserved == published
                    && available_permits(&cache) == total_permits
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        CancellationResult {
            total_permits,
            permits_after_abort,
            reserved,
            published,
            temporary_files,
            settled,
        }
    })
}

#[test]
fn cancelled_store_retains_permit_until_write_finishes() {
    let result = cancel_store_during_blocking_write();
    assert_eq!(
        result.permits_after_abort,
        result.total_permits - 1,
        "cancelling the caller must not release the pending write's permit"
    );
}

#[test]
fn cancelled_store_reconciles_bytes_and_temporary_files() {
    let result = cancel_store_during_blocking_write();
    assert!(
        result.settled,
        "cancelled store leaked: reserved={}, published={}, temporary_files={}",
        result.reserved, result.published, result.temporary_files
    );
}
