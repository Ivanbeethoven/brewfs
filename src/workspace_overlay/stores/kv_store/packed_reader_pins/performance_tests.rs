//! Operation-count and scheduling regressions against the real packed catalog.

use super::*;
use crate::meta::layer::{NamespaceActor, scope_namespace_actor};

#[tokio::test]
async fn packed_create_reuses_fresh_parent_permission_snapshot() {
    let f = fixture(false).await;
    f.meta.chmod(1, 0o777).await.unwrap();
    f.backend.consistent_reads.store(0, Ordering::SeqCst);
    scope_namespace_actor(
        Some(NamespaceActor {
            uid: 1000,
            gid: 1000,
            groups: vec![1000],
        }),
        f.meta.create_file(1, "actor-create".into()),
    )
    .await
    .unwrap();
    let reads = f.backend.consistent_reads.load(Ordering::SeqCst);
    f.reader.shutdown().await.unwrap();
    assert_eq!(reads, 1, "one fresh parent snapshot per create attempt");
}

#[tokio::test]
async fn packed_file_churn_preserves_unrelated_path_cache() {
    use crate::workspace_overlay::lifecycle::LeaseValidity;
    let lease = Arc::new(LeaseValidity::new(
        std::time::Instant::now(),
        std::time::Duration::from_secs(60),
    ));
    let f = fixture_with_lease(false, packed().await, Some(lease)).await;
    let stable = f.meta.create_file(1, "stable".into()).await.unwrap();
    let changed = f.meta.create_file(1, "changed".into()).await.unwrap();
    for operation in 0..3 {
        assert_eq!(
            f.meta.get_paths_bytes_owned(stable).await.unwrap().paths,
            vec![b"/stable".to_vec()]
        );
        // Also warm the affected inode so correctness of invalidation is tested.
        drop(f.meta.get_paths_bytes_owned(changed).await.unwrap());
        match operation {
            0 => {
                f.meta.link(changed, 1, "alias").await.unwrap();
            }
            1 => {
                f.meta
                    .rename(1, "alias", 1, "renamed".into())
                    .await
                    .unwrap();
            }
            _ => {
                f.meta.unlink(1, "renamed").await.unwrap();
            }
        }
        f.backend.page_prefixes.lock().await.clear();
        assert_eq!(
            f.meta.get_paths_bytes_owned(stable).await.unwrap().paths,
            vec![b"/stable".to_vec()]
        );
        assert!(
            f.backend.page_prefixes.lock().await.is_empty(),
            "unrelated path needed a reverse-index walk after operation {operation}"
        );
        let mut paths = f.meta.get_paths_bytes_owned(changed).await.unwrap().paths;
        paths.sort();
        let mut expected = vec![b"/changed".to_vec()];
        if operation < 2 {
            expected.push(if operation == 0 {
                b"/alias".to_vec()
            } else {
                b"/renamed".to_vec()
            });
        }
        expected.sort();
        assert_eq!(paths, expected);
    }
    // Replacement must invalidate the overwritten file as well as the source.
    let victim = f.meta.create_file(1, "victim".into()).await.unwrap();
    drop(f.meta.get_paths_bytes_owned(victim).await.unwrap());
    f.meta
        .rename(1, "changed", 1, "victim".into())
        .await
        .unwrap();
    assert!(
        f.meta
            .get_paths_bytes_owned(victim)
            .await
            .unwrap()
            .paths
            .is_empty()
    );
    assert_eq!(
        f.meta.get_paths_bytes_owned(changed).await.unwrap().paths,
        vec![b"/victim".to_vec()]
    );
    // A directory move still invalidates paths of its descendants.
    f.vfs.mkdir_err("/parent").await.unwrap();
    let child = f.vfs.create_file("/parent/child").await.unwrap();
    drop(f.meta.get_paths_bytes_owned(child).await.unwrap());
    f.meta.rename(1, "parent", 1, "moved".into()).await.unwrap();
    assert_eq!(
        f.meta.get_paths_bytes_owned(child).await.unwrap().paths,
        vec![b"/moved/child".to_vec()]
    );
    f.reader.shutdown().await.unwrap();
}

#[tokio::test]
async fn packed_directory_slow_page_does_not_block_other_offsets() {
    let f = fixture(false).await;
    for index in 0..73 {
        f.vfs
            .create_file(&format!("/native-{index:03}"))
            .await
            .unwrap();
    }
    let directory = Arc::new(f.meta.opendir(1).await.unwrap());
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *f.backend.page_barrier.lock().await = Some((entered.clone(), resume.clone()));
    let slow_directory = directory.clone();
    let slow = tokio::spawn(async move { slow_directory.get_entries_page_raw_owned(64, 1).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let fast = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        directory.get_entries_page_raw_owned(0, 1),
    )
    .await;
    resume.notify_one();
    let slow = slow.await.unwrap().unwrap();
    assert_eq!(slow.entries[0].name, b"native-064");
    drop(slow);
    drop(directory);
    // Release any retained output owner before shutting down the reader.
    let fast = fast.map(|page| page.unwrap().entries);
    f.reader.shutdown().await.unwrap();
    let fast = fast.expect("offset zero waited for another offset's remote page");
    assert_eq!(fast[0].name, b"native-000");
}
