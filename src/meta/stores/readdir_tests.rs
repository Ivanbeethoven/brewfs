use crate::meta::MetaStore;
use crate::meta::store::{FileType, MetaError};

async fn assert_pages(store: &impl MetaStore, dir: i64) {
    let mut expected = store.readdir(dir).await.unwrap();
    expected.sort_by(|a, b| a.name.cmp(&b.name));
    let expected: Vec<_> = expected
        .into_iter()
        .map(|e| (e.name, e.ino, e.kind))
        .collect();
    for limit in [1, 2, 3, 100] {
        let mut found = Vec::new();
        let mut start = String::new();
        loop {
            let page = store.readdir_page(dir, &start, limit).await.unwrap();
            assert!(page.len() <= limit);
            if page.is_empty() {
                break;
            }
            assert!(page[0].name >= start);
            start = format!("{}\u{1}", page.last().unwrap().name);
            found.extend(page.into_iter().map(|e| (e.name, e.ino, e.kind)));
            assert!(found.len() <= expected.len());
        }
        assert_eq!(found, expected);
    }
}

/// Exercise every namespace operation which can affect an ordered name index.
pub(super) async fn directory_pages_follow_namespace_mutations(store: &impl MetaStore) {
    let dir = store.mkdir(1, "paged".to_string()).await.unwrap();
    assert!(store.readdir_page(dir, "", 0).await.unwrap().is_empty());
    assert_pages(store, dir).await;
    for name in ["雪", "é", "z", "a!", "a\u{1}", "a", "A"] {
        store.create_file(dir, name.to_string()).await.unwrap();
    }
    let child = store.mkdir(dir, "d".to_string()).await.unwrap();
    assert_pages(store, dir).await;
    let file = store.lookup(dir, "A").await.unwrap().unwrap();
    store.link(file, dir, "linked").await.unwrap();
    assert_pages(store, dir).await;
    store.rename(dir, "A", dir, "z".to_string()).await.unwrap();
    assert_pages(store, dir).await;
    store.rename_exchange(dir, "d", dir, "a").await.unwrap();
    assert_eq!(
        store.readdir_page(dir, "a", 1).await.unwrap()[0].kind,
        FileType::Dir
    );
    assert_pages(store, dir).await;
    store.unlink(dir, "linked").await.unwrap();
    store.rmdir(dir, "a").await.unwrap();
    assert_pages(store, dir).await;
    assert!(matches!(
        store.readdir_page(child, "", 1).await,
        Err(MetaError::NotFound(_))
    ));

    let other = store.mkdir(1, "other".to_string()).await.unwrap();
    assert_pages(store, other).await; // prime the empty destination's index
    store
        .rename(dir, "d", other, "moved".to_string())
        .await
        .unwrap();
    assert_pages(store, dir).await;
    assert_pages(store, other).await;
    let old = store.mkdir(dir, "old".to_string()).await.unwrap();
    let replaced = store.mkdir(other, "replaced".to_string()).await.unwrap();
    assert_pages(store, old).await;
    assert_pages(store, replaced).await;
    store
        .rename(dir, "old", other, "replaced".to_string())
        .await
        .unwrap();
    assert_pages(store, dir).await;
    assert_pages(store, other).await;

    let first = store.readdir_page(dir, "", 1).await.unwrap().remove(0);
    store.unlink(dir, &first.name).await.unwrap();
    let next = store
        .readdir_page(dir, &format!("{}\u{1}", first.name), 1)
        .await
        .unwrap();
    assert!(next[0].name > first.name); // deleted cursors still resume
    assert!(matches!(
        store.readdir_page(file, "", 1).await,
        Err(MetaError::NotDirectory(_))
    ));
}
