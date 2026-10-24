use std::time::Duration;

use brewfs::chunk::cache::{ChunksCache, ChunksCacheConfig};
use brewfs::chunk::cache_integrity::CacheIntegrityMode;
use bytes::Bytes;

async fn refill_after_eviction(range_read: bool) {
    let dir = tempfile::tempdir().unwrap();
    let cache = ChunksCache::new_with_config(
        ChunksCacheConfig::with_budgets(0, 4096, dir.path().to_path_buf())
            .with_integrity_mode(CacheIntegrityMode::None),
    )
    .await
    .unwrap();
    let key = "evicted-block".to_owned();
    let data = Bytes::from(vec![42; 4096]);
    cache.store_to_disk(&key, data.clone()).await.unwrap();
    assert!(cache.is_disk_cached(&key).await);

    // The budget holds one block. Publishing another block must evict A.
    cache
        .store_to_disk("replacement-block", Bytes::from(vec![7; 4096]))
        .await
        .unwrap();
    let mut buf = [0; 32];
    if range_read {
        assert_eq!(cache.get_range_into(&key, 16, &mut buf).await, None);
    } else {
        assert_eq!(cache.get(&key).await, None);
    }

    // Model the public cache-aside read path after fetching the missing block.
    cache.insert_opportunistic(key.clone(), data.clone()).await;
    let refilled = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if range_read {
                if cache.get_range_into(&key, 16, &mut buf).await == Some(buf.len()) {
                    assert_eq!(&buf, &data[16..48]);
                    break;
                }
            } else if let Some(cached) = cache.get(&key).await {
                assert_eq!(cached, data);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        refilled.is_ok(),
        "evicted block was not republished after a cache miss and opportunistic refill"
    );
    assert!(cache.is_disk_cached(&key).await);
}

#[tokio::test]
async fn whole_block_miss_allows_refill_after_disk_eviction() {
    refill_after_eviction(false).await;
}

#[tokio::test]
async fn range_miss_allows_refill_after_disk_eviction() {
    refill_after_eviction(true).await;
}
