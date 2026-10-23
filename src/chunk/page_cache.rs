//! ReadPageCache: page-granularity (64KB) read cache for small range reads.
//!
//! ChunksCache only stores full 4MB blocks, so small range reads (<= 1MB) that
//! use `get_object_range` currently discard the result — every repeated small
//! read hits object storage.  ReadPageCache fills this gap by caching 64KB pages
//! so that subsequent small reads within the same page avoid the network round-trip.
//!
//! Blocks are COW-immutable once committed, so there is no cache-invalidation
//! problem: a page keyed by `(slice_id, block_index, page_index)` is valid
//! forever.  Pages from replaced/compacted slices naturally expire via TTL.

use bytes::Bytes;
use moka::future::Cache;
use std::time::Duration;

/// Default page size: 64 KiB, matching the write-cache `DEFAULT_PAGE_SIZE`.
#[allow(dead_code)]
pub const DEFAULT_PAGE_SIZE: usize = 64 * 1024;

/// Default capacity in number of pages: 32768 pages × 64 KiB = 2 GiB.
#[allow(dead_code)]
pub const DEFAULT_PAGE_CAPACITY: usize = 32768;

/// Uniquely identifies a 64KB page within a committed block.
///
/// `(slice_id, block_index, page_index)` where `page_index` is the page number
/// within the block (block_offset / page_size).
pub type PageKey = (
    u64, /*slice_id*/
    u32, /*block_index*/
    u32, /*page_index*/
);

/// A lightweight, process-wide cache for block pages.
///
/// Sits between the per-handle FileReader slice cache and the disk-backed
/// ChunksCache.  Intercepts small range reads that would otherwise be
/// discard-after-read and keeps them in memory for future accesses.
pub struct ReadPageCache {
    cache: Cache<PageKey, Bytes>,
    page_size: usize,
}

impl ReadPageCache {
    /// Create a new page cache.
    ///
    /// `capacity_pages` is the maximum number of cached pages (e.g. 4096 →
    /// 256 MiB with 64 KiB pages).  `page_size` is typically 64 KiB.
    pub fn new(capacity_pages: usize, page_size: usize) -> Self {
        let cache = Cache::builder()
            .max_capacity(capacity_pages as u64)
            .time_to_live(Duration::from_secs(120))
            .time_to_idle(Duration::from_secs(30))
            .build();
        Self { cache, page_size }
    }

    /// Look up a cached page.
    #[inline]
    pub async fn get(&self, key: &PageKey) -> Option<Bytes> {
        self.cache.get(key).await
    }

    /// Insert a page with its own allocation so eviction releases page memory.
    #[inline]
    pub async fn insert(&self, key: PageKey, data: Bytes) {
        self.cache.insert(key, Bytes::copy_from_slice(&data)).await;
    }

    /// Page size in bytes (the granularity at which data is cached).
    #[inline]
    pub fn page_size(&self) -> usize {
        self.page_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn evicted_range_pages_release_the_response_allocation() {
        struct Response(Arc<Vec<u8>>);

        impl AsRef<[u8]> for Response {
            fn as_ref(&self) -> &[u8] {
                self.0.as_slice()
            }
        }

        let cache = ReadPageCache::new(1, DEFAULT_PAGE_SIZE);
        let allocation = Arc::new(vec![37; 16 * DEFAULT_PAGE_SIZE]);
        let retained_response = Arc::downgrade(&allocation);
        let response = Bytes::from_owner(Response(allocation));
        for page in 0..16 {
            let start = page * DEFAULT_PAGE_SIZE;
            cache
                .insert(
                    (1, 0, page as u32),
                    response.slice(start..start + DEFAULT_PAGE_SIZE),
                )
                .await;
        }
        drop(response);
        cache.cache.run_pending_tasks().await;

        assert_eq!(cache.cache.entry_count(), 1);
        let mut surviving_pages = 0;
        for page in 0..16 {
            if let Some(bytes) = cache.get(&(1, 0, page)).await {
                assert_eq!(bytes.as_ref(), vec![37; DEFAULT_PAGE_SIZE]);
                surviving_pages += 1;
            }
        }
        assert_eq!(surviving_pages, 1);
        assert!(
            retained_response.upgrade().is_none(),
            "one cached page must not retain the entire 1 MiB response allocation"
        );
    }
}
