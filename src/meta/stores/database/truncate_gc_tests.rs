use super::*;
use crate::cadapter::client::ObjectClient;
use crate::cadapter::localfs::LocalFsBackend;
use crate::chunk::cache::ChunksCacheConfig;
use crate::chunk::compact::gc::{BlockGcConfig, BlockStoreGC};
use crate::chunk::compress::Compression;
use crate::chunk::store::{BlockStore, BlockStoreConfig, ObjectBlockStore};

struct Fixture {
    _temp: tempfile::TempDir,
    meta: Arc<DatabaseMetaStore>,
    blocks: Arc<ObjectBlockStore<LocalFsBackend>>,
    client: ObjectClient<LocalFsBackend>,
    ino: i64,
    slice: SliceDesc,
}

impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let meta = Arc::new(new_test_store().await);
        let ino = meta
            .create_file(meta.root_ino(), "truncate-gc".into())
            .await
            .unwrap();
        let client = ObjectClient::new(LocalFsBackend::new(temp.path().join("objects")));
        let blocks = Arc::new(
            ObjectBlockStore::new_with_configs_async(
                client.clone(),
                ChunksCacheConfig::with_budgets(0, 0, temp.path().join("cache")),
                BlockStoreConfig {
                    block_size: 4096,
                    page_size: 4096,
                    page_cache_capacity: 0,
                    compression: Compression::None,
                    range_background_prefetch: false,
                    populate_write_cache_after_upload: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let slice = SliceDesc {
            slice_id: 17,
            chunk_id: chunk_id_for(ino, 0).unwrap(),
            offset: 0,
            length: 8192,
        };
        for index in 0..2 {
            blocks
                .write_fresh_range((slice.slice_id, index), 0, &[7; 4096])
                .await
                .unwrap();
        }
        meta.truncate(ino, slice.length, 64 << 20).await.unwrap();
        meta.append_slice(slice.chunk_id, slice).await.unwrap();
        Self {
            _temp: temp,
            meta,
            blocks,
            client,
            ino,
            slice,
        }
    }
    async fn gc(&self) {
        BlockStoreGC::new(self.meta.clone(), self.blocks.clone())
            .run_gc_cycle(&BlockGcConfig {
                min_age_secs: 0,
                block_size: 4096,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    async fn object_exists(&self, block: u32) -> bool {
        self.client
            .get_object(&format!("chunks-v2/{}/{block}", self.slice.slice_id))
            .await
            .unwrap()
            .is_some()
    }
}

#[tokio::test]
async fn truncate_to_zero_eventually_deletes_all_committed_slice_objects() {
    let fixture = Fixture::new().await;
    fixture
        .meta
        .truncate(fixture.ino, 0, 64 << 20)
        .await
        .unwrap();
    assert!(
        fixture
            .meta
            .get_slices(fixture.slice.chunk_id)
            .await
            .unwrap()
            .is_empty()
    );
    fixture.gc().await;
    assert!(
        !fixture.object_exists(0).await,
        "truncate must schedule durable object reclamation"
    );
    assert!(!fixture.object_exists(1).await);
    fixture.gc().await;
}

#[tokio::test]
async fn partial_truncate_then_replace_reclaims_original_physical_tail() {
    for versioned in [false, true] {
        let fixture = Fixture::new().await;
        fixture
            .meta
            .truncate(fixture.ino, 4096, 64 << 20)
            .await
            .unwrap();
        fixture
            .meta
            .truncate(fixture.ino, 1, 64 << 20)
            .await
            .unwrap();
        fixture.gc().await;
        assert!(
            fixture.object_exists(0).await,
            "the surviving prefix must remain readable"
        );
        let visible = fixture
            .meta
            .get_slices(fixture.slice.chunk_id)
            .await
            .unwrap();
        assert_eq!(visible[0].length, 1);
        let delayed = SliceDesc::encode_delayed_data(&visible, &[fixture.slice.slice_id]);
        if versioned {
            fixture
                .meta
                .replace_slices_for_compact_with_version(
                    fixture.slice.chunk_id,
                    &[],
                    &delayed,
                    &visible,
                )
                .await
                .unwrap();
        } else {
            fixture
                .meta
                .replace_slices_for_compact(fixture.slice.chunk_id, &[], &delayed)
                .await
                .unwrap();
        }
        fixture.gc().await;
        assert!(!fixture.object_exists(0).await);
        assert!(
            !fixture.object_exists(1).await,
            "visible length must not erase physical tail ownership"
        );
    }
}

#[tokio::test]
async fn retained_truncate_prefix_does_not_block_other_gc_work() {
    let fixture = Fixture::new().await;
    fixture
        .meta
        .truncate(fixture.ino, 1, 64 << 20)
        .await
        .unwrap();
    let other = SliceDesc {
        slice_id: 18,
        ..fixture.slice
    };
    fixture
        .meta
        .append_slice(other.chunk_id, other)
        .await
        .unwrap();
    fixture
        .meta
        .replace_slices_for_compact(
            other.chunk_id,
            &[],
            &SliceDesc::encode_delayed_data(&[other], &[other.slice_id]),
        )
        .await
        .unwrap();
    let ready = fixture.meta.process_delayed_slices(1, 0).await.unwrap();
    assert_eq!(
        ready.len(),
        1,
        "retained prefixes must not fill the pending GC batch"
    );
    assert_eq!(ready[0].0, other.slice_id);
    assert!(fixture.object_exists(0).await);
}

#[tokio::test]
async fn retained_truncate_tail_starts_grace_period_when_unmapped() {
    let fixture = Fixture::new().await;
    fixture
        .meta
        .truncate(fixture.ino, 1, 64 << 20)
        .await
        .unwrap();
    // Age the ownership record without sleeping. The prefix is still live.
    DelayedSlice::update_many()
        .col_expr(delayed_slice::Column::CreatedAt, sea_query::Expr::value(0))
        .exec(&fixture.meta.db)
        .await
        .unwrap();
    fixture
        .meta
        .truncate(fixture.ino, 0, 64 << 20)
        .await
        .unwrap();
    assert!(
        fixture
            .meta
            .process_delayed_slices(10, 60)
            .await
            .unwrap()
            .is_empty(),
        "old physical ownership must not bypass reader grace after unmapping"
    );
    fixture.gc().await;
    assert!(!fixture.object_exists(0).await);
    assert!(!fixture.object_exists(1).await);
}
