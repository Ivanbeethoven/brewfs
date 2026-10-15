//! Short-lived cache of writable-layer (upper) rows for read-only paths.
//!
//! Opt-in per mount (`WorkspaceMetaLayer::with_upper_row_cache`). The
//! workspace lease is exclusive, so while it is held the mounted layer is
//! the only writer of upper rows; every local commit and every view change
//! invalidates the whole cache, and a generation counter keeps a read that
//! raced a commit from inserting pre-commit rows. The TTL bounds staleness
//! for anything outside this mount, matching the native metadata client's
//! inode/path cache. Mutation paths never read through this cache.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moka::sync::Cache;

use crate::workspace_overlay::catalog::PermissionSnapshot;
use crate::workspace_overlay::model::{DentryDelta, InodeDelta, XattrDelta};

const CAPACITY: u64 = 65_536;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum RowKey {
    Inode(i64),
    Dentry(i64, Vec<u8>),
    Xattr(i64, Vec<u8>),
    Permission(i64),
}

#[derive(Clone)]
pub(super) enum RowValue {
    Inode(Arc<Vec<InodeDelta>>),
    Dentry(Arc<Vec<DentryDelta>>),
    Xattr(Arc<Vec<XattrDelta>>),
    Permission(Arc<PermissionSnapshot>),
}

pub(super) struct UpperRowCache {
    rows: Cache<RowKey, RowValue>,
    generation: AtomicU64,
}

/// Captured before a backend read; the insert is dropped if any
/// invalidation happened in between.
#[derive(Clone, Copy)]
pub(super) struct RowTicket(u64);

impl UpperRowCache {
    pub(super) fn new(ttl: Duration) -> Self {
        Self {
            rows: Cache::builder()
                .max_capacity(CAPACITY)
                .time_to_live(ttl)
                .build(),
            generation: AtomicU64::new(0),
        }
    }

    pub(super) fn ticket(&self) -> RowTicket {
        RowTicket(self.generation.load(Ordering::Acquire))
    }

    pub(super) fn get(&self, key: &RowKey) -> Option<RowValue> {
        self.rows.get(key)
    }

    pub(super) fn insert(&self, ticket: RowTicket, key: RowKey, value: RowValue) {
        if self.generation.load(Ordering::Acquire) != ticket.0 {
            return;
        }
        self.rows.insert(key, value);
        // An invalidation that ran concurrently with this insert must win.
        if self.generation.load(Ordering::Acquire) != ticket.0 {
            self.rows.invalidate_all();
        }
    }

    pub(super) fn invalidate_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.rows.invalidate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn racing_invalidation_drops_stale_insert() {
        let cache = UpperRowCache::new(Duration::from_secs(60));
        let ticket = cache.ticket();
        cache.invalidate_all();
        cache.insert(
            ticket,
            RowKey::Inode(5),
            RowValue::Inode(Arc::new(Vec::new())),
        );
        assert!(cache.get(&RowKey::Inode(5)).is_none());
        let fresh = cache.ticket();
        cache.insert(
            fresh,
            RowKey::Inode(5),
            RowValue::Inode(Arc::new(Vec::new())),
        );
        assert!(cache.get(&RowKey::Inode(5)).is_some());
        cache.invalidate_all();
        assert!(cache.get(&RowKey::Inode(5)).is_none());
    }
}
