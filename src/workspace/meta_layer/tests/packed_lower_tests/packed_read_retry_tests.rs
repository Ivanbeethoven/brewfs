use super::*;
use crate::meta::MetaLayer;
use crate::meta::store::MetaError;
use crate::workspace::stores::database::SqliteWorkspaceStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};

fn retry_store(
    meta: &WorkspaceMetaLayer<SqliteWorkspaceStore>,
    busy_layer_chain_reads: usize,
    busy_permission_snapshot_reads: usize,
) -> Arc<BindingWithoutPins<SqliteWorkspaceStore>> {
    Arc::new(BindingWithoutPins {
        inner: meta.store().clone(),
        binding: None,
        probes: AtomicU64::new(0),
        metadata_reads_enabled: true,
        busy_layer_chain_reads: AtomicUsize::new(busy_layer_chain_reads),
        layer_chain_reads: AtomicUsize::new(0),
        busy_permission_snapshot_reads: AtomicUsize::new(busy_permission_snapshot_reads),
        permission_snapshot_reads: AtomicUsize::new(0),
    })
}

#[tokio::test]
async fn packed_read_retry_kernel_chain_busy_keeps_original_ttl() {
    let base = super::super::super::test_meta().await;
    let store = retry_store(&base, 2, 0);
    let meta = WorkspaceMetaLayer::new(store.clone(), base.view_context().await);
    let ttl = Duration::from_secs(1);
    let started = Instant::now();
    meta.kernel_cache.lock().await.now = Some(started);

    let deadline = meta
        .kernel_metadata_cache_deadline(ttl)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(store.layer_chain_reads.load(Ordering::SeqCst), 3);
    assert_eq!(store.busy_layer_chain_reads.load(Ordering::SeqCst), 0);
    assert_eq!(deadline, started + ttl);
    assert_eq!(
        meta.kernel_metadata_cache_deadline(ttl).await.unwrap(),
        Some(deadline)
    );
    assert_eq!(store.layer_chain_reads.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn packed_read_retry_kernel_chain_failure_is_not_cached() {
    let base = super::super::super::test_meta().await;
    let store = retry_store(&base, 8, 0);
    let meta = WorkspaceMetaLayer::new(store.clone(), base.view_context().await);
    let ttl = Duration::from_secs(1);

    let error = meta.kernel_metadata_cache_deadline(ttl).await.unwrap_err();
    assert!(matches!(
        error,
        MetaError::Io(ref error) if error.raw_os_error() == Some(libc::EBUSY)
    ));
    assert_eq!(store.layer_chain_reads.load(Ordering::SeqCst), 8);

    store.busy_layer_chain_reads.store(0, Ordering::SeqCst);
    assert!(
        meta.kernel_metadata_cache_deadline(ttl)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(store.layer_chain_reads.load(Ordering::SeqCst), 9);
}

#[tokio::test]
async fn packed_read_retry_permission_snapshot_retries_busy() {
    let base = super::super::super::test_meta().await;
    let store = retry_store(&base, 0, 2);
    let meta = WorkspaceMetaLayer::new(store.clone(), base.view_context().await);

    meta.permission_snapshot(vec![meta.root_ino()], None)
        .await
        .unwrap();

    assert_eq!(store.permission_snapshot_reads.load(Ordering::SeqCst), 3);
    assert_eq!(
        store.busy_permission_snapshot_reads.load(Ordering::SeqCst),
        0
    );
}
