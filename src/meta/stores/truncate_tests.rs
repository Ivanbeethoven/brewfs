use super::{DatabaseMetaStore, EtcdMetaStore, RedisMetaStore};
use crate::chunk::SliceDesc;
use crate::chunk::compact::{BlockGcConfig, BlockStoreGC};
use crate::chunk::store::{BlockKey, BlockStore, InMemoryBlockStore};
use crate::meta::config::{Config, DatabaseConfig, DatabaseType};
use crate::meta::store::MetaStore;
use crate::vfs::chunk_id_for;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const BLOCK_SIZE: u64 = 4 * 1024 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct RetryBlockStore {
    inner: InMemoryBlockStore,
    fail_deletes: AtomicBool,
}

#[async_trait::async_trait]
impl BlockStore for RetryBlockStore {
    async fn write_fresh_range(
        &self,
        key: BlockKey,
        offset: u64,
        data: &[u8],
    ) -> anyhow::Result<u64> {
        self.inner.write_fresh_range(key, offset, data).await
    }

    async fn read_range(&self, key: BlockKey, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.inner.read_range(key, offset, buf).await
    }

    async fn delete_range(&self, key: BlockKey, count: u64) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.fail_deletes.load(Ordering::SeqCst),
            "injected deletion failure"
        );
        self.inner.delete_range(key, count).await
    }
}

async fn assert_truncate_gc<M: MetaStore + Send + Sync + 'static>(store: M, partial: bool) {
    let store = Arc::new(store);
    let blocks = Arc::new(RetryBlockStore::default());
    let gc = BlockStoreGC::new(store.clone(), blocks.clone());
    let config = BlockGcConfig {
        min_age_secs: -1,
        block_size: BLOCK_SIZE,
        ..Default::default()
    };

    // Exercise both compaction APIs and a subsequent truncate of a shortened slice.
    for mode in 0..3 {
        let ino = store
            .create_file(
                store.root_ino(),
                format!("truncate-gc-{partial}-{mode}-{}", uuid::Uuid::new_v4()),
            )
            .await
            .unwrap();
        let chunk_id = chunk_id_for(ino, 0).unwrap();
        let slice = SliceDesc {
            slice_id: store.next_id(crate::meta::SLICE_ID_KEY).await.unwrap() as u64,
            chunk_id,
            offset: 0,
            length: 2 * BLOCK_SIZE,
        };
        for index in 0..2 {
            blocks
                .write_fresh_range((slice.slice_id, index), 0, &[42])
                .await
                .unwrap();
        }
        store.append_slice(chunk_id, slice).await.unwrap();
        let past_eof = SliceDesc {
            slice_id: store.next_id(crate::meta::SLICE_ID_KEY).await.unwrap() as u64,
            offset: 3 * BLOCK_SIZE,
            length: 1,
            ..slice
        };
        store.append_slice(chunk_id, past_eof).await.unwrap();
        blocks
            .write_fresh_range((past_eof.slice_id, 0), 0, &[44])
            .await
            .unwrap();
        let tail = SliceDesc {
            slice_id: store.next_id(crate::meta::SLICE_ID_KEY).await.unwrap() as u64,
            chunk_id: chunk_id_for(ino, 1).unwrap(),
            ..slice
        };
        for index in 0..2 {
            blocks
                .write_fresh_range((tail.slice_id, index), 0, &[43])
                .await
                .unwrap();
        }
        store.append_slice(tail.chunk_id, tail).await.unwrap();
        store
            .extend_file_size(ino, CHUNK_SIZE + tail.length)
            .await
            .unwrap();
        // No uncommitted record can rescue a forgotten committed slice.
        store.confirm_slice_committed(slice.slice_id).await.unwrap();

        if partial {
            // Repeated shortening must preserve the first physical length.
            store.truncate(ino, BLOCK_SIZE, CHUNK_SIZE).await.unwrap();
            store.truncate(ino, 1, CHUNK_SIZE).await.unwrap();
            gc.run_gc_cycle(&config).await.unwrap();
            for index in 0..2 {
                let mut byte = [0];
                blocks
                    .read_range((slice.slice_id, index), 0, &mut byte)
                    .await
                    .unwrap();
                assert_eq!(byte, [42], "GC deleted a still-owned slice");
            }
            let surviving = store.get_slices(chunk_id).await.unwrap();
            assert_eq!(surviving.len(), 1);
            assert_eq!(surviving[0].length, 1);
            if mode < 2 {
                let delayed = SliceDesc::encode_delayed_data(&surviving, &[slice.slice_id]);
                if mode == 0 {
                    store
                        .replace_slices_for_compact(chunk_id, &[], &delayed)
                        .await
                        .unwrap();
                } else {
                    store
                        .replace_slices_for_compact_with_version(
                            chunk_id,
                            &[],
                            &delayed,
                            &surviving,
                        )
                        .await
                        .unwrap();
                }
            } else {
                store.truncate(ino, 0, CHUNK_SIZE).await.unwrap();
            }
        } else {
            store.truncate(ino, 0, CHUNK_SIZE).await.unwrap();
        }

        // Fresh deletion records still honor the reader grace period.
        gc.run_gc_cycle(&BlockGcConfig {
            min_age_secs: 3600,
            ..config.clone()
        })
        .await
        .unwrap();
        let mut byte = [0];
        blocks
            .read_range((slice.slice_id, 0), 0, &mut byte)
            .await
            .unwrap();
        blocks.fail_deletes.store(true, Ordering::SeqCst);
        gc.run_gc_cycle(&config).await.unwrap();
        let pending = store.process_delayed_slices(100, -1).await.unwrap();
        assert!(
            pending
                .iter()
                .any(|&(id, _, size, _)| id == slice.slice_id && size == slice.length),
            "failed deletion lost the original physical extent"
        );
        blocks
            .read_range((slice.slice_id, 1), 0, &mut byte)
            .await
            .unwrap();
        blocks.fail_deletes.store(false, Ordering::SeqCst);
        gc.run_gc_cycle(&config).await.unwrap();
        for index in 0..2 {
            assert!(
                blocks
                    .read_range((slice.slice_id, index), 0, &mut byte)
                    .await
                    .is_err(),
                "truncate leaked committed slice {} block {index}",
                slice.slice_id
            );
            assert!(
                blocks
                    .read_range((tail.slice_id, index), 0, &mut byte)
                    .await
                    .is_err(),
                "truncate leaked a discarded tail chunk"
            );
        }
        assert!(store.get_slices(chunk_id).await.unwrap().is_empty());
        assert!(
            blocks
                .read_range((past_eof.slice_id, 0), 0, &mut byte)
                .await
                .is_err(),
            "truncate leaked a discarded slice in the partial chunk"
        );
        assert!(
            store
                .process_delayed_slices(100, -1)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

fn config(db_config: DatabaseType) -> Config {
    Config {
        database: DatabaseConfig { db_config },
        cache: Default::default(),
        client: Default::default(),
        compact: Default::default(),
    }
}

#[tokio::test]
async fn truncate_to_zero_gc_sqlite() {
    let store = DatabaseMetaStore::from_config(config(DatabaseType::Sqlite {
        url: "sqlite::memory:".into(),
    }))
    .await
    .unwrap();
    assert_truncate_gc(store, false).await;
}

#[tokio::test]
async fn partial_truncate_gc_sqlite() {
    let store = DatabaseMetaStore::from_config(config(DatabaseType::Sqlite {
        url: "sqlite::memory:".into(),
    }))
    .await
    .unwrap();
    assert_truncate_gc(store, true).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via BREWFS_POSTGRES_TEST_URL"]
#[serial_test::serial]
async fn truncate_to_zero_gc_postgres() {
    let url = std::env::var("BREWFS_POSTGRES_TEST_URL").unwrap();
    let store = DatabaseMetaStore::from_config(config(DatabaseType::Postgres { url }))
        .await
        .unwrap();
    assert_truncate_gc(store, false).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via BREWFS_POSTGRES_TEST_URL"]
#[serial_test::serial]
async fn partial_truncate_gc_postgres() {
    let url = std::env::var("BREWFS_POSTGRES_TEST_URL").unwrap();
    let store = DatabaseMetaStore::from_config(config(DatabaseType::Postgres { url }))
        .await
        .unwrap();
    assert_truncate_gc(store, true).await;
}

#[tokio::test]
#[ignore = "requires isolated Redis via BREWFS_REDIS_TEST_URL"]
#[serial_test::serial]
async fn truncate_to_zero_gc_redis() {
    let url = std::env::var("BREWFS_REDIS_TEST_URL").unwrap();
    let store = RedisMetaStore::from_config(config(DatabaseType::Redis { url }))
        .await
        .unwrap();
    assert_truncate_gc(store, false).await;
}

#[tokio::test]
#[ignore = "requires isolated Redis via BREWFS_REDIS_TEST_URL"]
#[serial_test::serial]
async fn partial_truncate_gc_redis() {
    let url = std::env::var("BREWFS_REDIS_TEST_URL").unwrap();
    let store = RedisMetaStore::from_config(config(DatabaseType::Redis { url }))
        .await
        .unwrap();
    assert_truncate_gc(store, true).await;
}

#[tokio::test]
#[ignore = "requires isolated etcd via BREWFS_ETCD_TEST_URL"]
#[serial_test::serial]
async fn truncate_to_zero_gc_etcd() {
    let url = std::env::var("BREWFS_ETCD_TEST_URL").unwrap();
    let store = EtcdMetaStore::from_config(config(DatabaseType::Etcd { urls: vec![url] }))
        .await
        .unwrap();
    assert_truncate_gc(store, false).await;
}

#[tokio::test]
#[ignore = "requires isolated etcd via BREWFS_ETCD_TEST_URL"]
#[serial_test::serial]
async fn partial_truncate_gc_etcd() {
    let url = std::env::var("BREWFS_ETCD_TEST_URL").unwrap();
    let store = EtcdMetaStore::from_config(config(DatabaseType::Etcd { urls: vec![url] }))
        .await
        .unwrap();
    assert_truncate_gc(store, true).await;
}

#[tokio::test]
#[ignore = "requires isolated etcd via BREWFS_ETCD_TEST_URL"]
#[serial_test::serial]
async fn truncate_gc_etcd_batches_many_chunks_and_slices() {
    let url = std::env::var("BREWFS_ETCD_TEST_URL").unwrap();
    let store = Arc::new(
        EtcdMetaStore::from_config(config(DatabaseType::Etcd { urls: vec![url] }))
            .await
            .unwrap(),
    );
    let blocks = Arc::new(InMemoryBlockStore::new());
    let ino = store
        .create_file(
            store.root_ino(),
            format!("truncate-batch-{}", uuid::Uuid::new_v4()),
        )
        .await
        .unwrap();
    let mut keys = Vec::new();
    for index in 0..60 {
        let chunk_id = chunk_id_for(ino, index).unwrap();
        // Both the file's chunk count and its first chunk's slice count exceed
        // the etcd transaction write limit.
        for _ in 0..if index == 0 { 60 } else { 1 } {
            let slice_id = store.next_id(crate::meta::SLICE_ID_KEY).await.unwrap() as u64;
            store
                .append_slice(
                    chunk_id,
                    SliceDesc {
                        slice_id,
                        chunk_id,
                        offset: 0,
                        length: 1,
                    },
                )
                .await
                .unwrap();
            blocks
                .write_fresh_range((slice_id, 0), 0, &[42])
                .await
                .unwrap();
            keys.push((slice_id, 0));
        }
    }
    store.extend_file_size(ino, 60 * CHUNK_SIZE).await.unwrap();
    store.truncate(ino, 0, CHUNK_SIZE).await.unwrap();
    for index in 0..60 {
        assert!(
            store
                .get_slices(chunk_id_for(ino, index).unwrap())
                .await
                .unwrap()
                .is_empty()
        );
    }
    let gc = BlockStoreGC::new(store.clone(), blocks.clone());
    let config = BlockGcConfig {
        min_age_secs: -1,
        batch_size: 100,
        ..Default::default()
    };
    for _ in 0..keys.len().div_ceil(config.batch_size) {
        gc.run_gc_cycle(&config).await.unwrap();
    }
    for key in keys {
        assert!(
            blocks.read_range(key, 0, &mut [0]).await.is_err(),
            "batched truncate leaked {key:?}"
        );
    }
    assert!(
        store
            .process_delayed_slices(100, -1)
            .await
            .unwrap()
            .is_empty()
    );
}
