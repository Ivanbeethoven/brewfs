#![cfg(feature = "workspace-overlay")]

use brewfs::chunk::cache::{ChunksCache, ChunksCacheConfig};
use brewfs::chunk::cache_integrity;
use brewfs::workspace_overlay::cache_scope::{read_cache_root, writeback_root};
use brewfs::workspace_overlay::ids::WorkspaceId;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn legacy_and_other_workspace_blocks_cannot_satisfy_a_scoped_read() {
    // This tests the disk cache itself; no mock object store is involved.
    let root = tempfile::tempdir().unwrap();
    let volume = Uuid::from_u128(1);
    let workspace = WorkspaceId::from_uuid(Uuid::from_u128(2));
    let sibling = WorkspaceId::from_uuid(Uuid::from_u128(3));
    let other_volume = Uuid::from_u128(4);
    let key = "chunks/2/0".to_string();
    let filename = hex::encode(Sha256::digest(key.as_bytes()));
    let wrong = cache_integrity::encode(b"hello-fr");
    let poisoned_roots = [
        root.path().to_path_buf(),
        read_cache_root(root.path(), volume, sibling),
        read_cache_root(root.path(), other_volume, workspace),
    ];
    for poisoned_root in &poisoned_roots {
        let chunks = poisoned_root.join("chunks");
        std::fs::create_dir_all(&chunks).unwrap();
        std::fs::write(chunks.join(&filename), &wrong).unwrap();
        let cache = ChunksCache::new_with_config(ChunksCacheConfig::with_budgets(
            1024,
            1024 * 1024,
            chunks,
        ))
        .await
        .unwrap();
        assert_eq!(&cache.get(&key).await.unwrap()[..], b"hello-fr");
    }
    let selected = read_cache_root(root.path(), volume, workspace).join("chunks");
    let cache = ChunksCache::new_with_config(ChunksCacheConfig::with_budgets(
        1024,
        1024 * 1024,
        selected.clone(),
    ))
    .await
    .unwrap();
    assert!(cache.get(&key).await.is_none());
    drop(cache);

    // A subsequent mount can reuse this workspace's immutable clean bytes.
    std::fs::write(
        selected.join(&filename),
        cache_integrity::encode(b"agent-2\n"),
    )
    .unwrap();
    let reopened =
        ChunksCache::new_with_config(ChunksCacheConfig::with_budgets(1024, 1024 * 1024, selected))
            .await
            .unwrap();
    assert_eq!(&reopened.get(&key).await.unwrap()[..], b"agent-2\n");
    for poisoned_root in poisoned_roots {
        assert_eq!(
            std::fs::read(poisoned_root.join("chunks").join(&filename)).unwrap(),
            wrong
        );
    }
}

#[test]
fn read_scope_is_stable_without_moving_epoch_scoped_dirty_staging() {
    let root = tempfile::tempdir().unwrap();
    let volume = Uuid::from_u128(1);
    let workspace = WorkspaceId::from_uuid(Uuid::from_u128(2));
    let clean = read_cache_root(root.path(), volume, workspace);
    assert!(clean.ends_with(format!("workspace-read-v1/{volume}/{workspace}")));
    for epoch in [1, 2] {
        let dirty = writeback_root(root.path(), volume, workspace, epoch).unwrap();
        assert!(dirty.ends_with(format!("workspace-v1/{volume}/{workspace}/{epoch}")));
        assert!(!dirty.starts_with(&clean));
    }
    assert_eq!(clean, read_cache_root(root.path(), volume, workspace));
}
