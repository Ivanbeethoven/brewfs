//! Unit tests for Compactor module
//!
//! These tests focus on the core compaction logic without requiring
//! a full filesystem setup.

#[path = "common/compaction_reply.rs"]
mod compaction_reply;

mod tests {
    use brewfs::DatabaseMetaStore;
    use brewfs::chunk::store::BlockStore;
    use brewfs::chunk::{
        ChunkLayout, CompactResult, Compactor, slice::SliceDesc, store::InMemoryBlockStore,
    };
    use brewfs::meta::store::MetaStore;
    use brewfs::{Config, DatabaseConfig, DatabaseType};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn heavy_compaction_keeps_published_objects_after_a_lost_commit_reply() {
        let (_tmp, meta, blocks) = setup_test_env().await;
        let ino = meta
            .create_file(meta.root_ino(), "lost-reply".into())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(ino, 0).unwrap();
        let old = SliceDesc {
            slice_id: 700,
            chunk_id,
            offset: 0,
            length: 4096,
        };
        blocks
            .write_fresh_range((old.slice_id, 0), 0, &[b'x'; 4096])
            .await
            .unwrap();
        meta.write(ino, chunk_id, old, 4096).await.unwrap();
        let compactor = Compactor::with_layout(
            Arc::new(super::compaction_reply::LostCompactionReply(meta.clone())),
            blocks.clone(),
            ChunkLayout {
                chunk_size: 8192,
                block_size: 4096,
            },
        );
        let error = compactor.compact_heavy(chunk_id).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected lost compaction commit reply"),
            "wrong injected failure: {error}"
        );
        let published = meta.get_slices(chunk_id).await.unwrap();
        assert_eq!(published.len(), 1);
        assert_ne!(
            published[0].slice_id, old.slice_id,
            "the underlying commit must actually have succeeded"
        );
        let mut actual = [0; 4096];
        let read = blocks
            .read_range((published[0].slice_id, 0), 0, &mut actual)
            .await;
        assert!(
            read.is_ok(),
            "lost reply cleanup deleted an already-published object: {read:?}"
        );
        assert_eq!(actual, [b'x'; 4096]);
    }

    #[tokio::test]
    async fn light_compaction_preserves_publication_order_with_nonmonotonic_ids() {
        let (_tmp, meta, blocks) = setup_test_env().await;
        let ino = meta
            .create_file(meta.root_ino(), "light-order".into())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(ino, 0).unwrap();
        for slice_id in [2001, 1001] {
            let byte = if slice_id == 2001 { b'a' } else { b'b' };
            blocks
                .write_fresh_range((slice_id, 0), 0, &[byte; 4096])
                .await
                .unwrap();
            meta.write(
                ino,
                chunk_id,
                SliceDesc {
                    slice_id,
                    chunk_id,
                    offset: 0,
                    length: 4096,
                },
                4096,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            meta.get_slices(chunk_id)
                .await
                .unwrap()
                .iter()
                .map(|s| s.slice_id)
                .collect::<Vec<_>>(),
            [2001, 1001]
        );
        assert_eq!(
            Compactor::new(meta.clone(), blocks.clone())
                .compact_light(chunk_id)
                .await
                .unwrap(),
            Some(1)
        );
        let kept = meta.get_slices(chunk_id).await.unwrap();
        assert_eq!(kept.len(), 1);
        let mut actual = [0; 4096];
        blocks
            .read_range((kept[0].slice_id, 0), 0, &mut actual)
            .await
            .unwrap();
        assert!(
            actual.iter().all(|b| *b == b'b'),
            "light compaction resurrected the older, higher-ID data"
        );
        assert_eq!(
            kept[0].slice_id, 1001,
            "light compaction discarded the last-published data"
        );
    }

    #[tokio::test]
    async fn heavy_compaction_preserves_publication_order_with_nonmonotonic_ids() {
        let (_tmp, meta, blocks) = setup_test_env().await;
        let ino = meta
            .create_file(meta.root_ino(), "heavy-order".into())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(ino, 0).unwrap();
        for (slice_id, offset, length, byte) in [(2001, 0, 8192, b'a'), (1001, 2048, 4096, b'b')] {
            blocks
                .write_fresh_range((slice_id, 0), 0, &vec![byte; length as usize])
                .await
                .unwrap();
            meta.write(
                ino,
                chunk_id,
                SliceDesc {
                    slice_id,
                    chunk_id,
                    offset,
                    length,
                },
                8192,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            meta.get_slices(chunk_id)
                .await
                .unwrap()
                .iter()
                .map(|s| s.slice_id)
                .collect::<Vec<_>>(),
            [2001, 1001]
        );
        let layout = ChunkLayout {
            chunk_size: 16384,
            block_size: 8192,
        };
        let new_id = Compactor::with_layout(meta, blocks.clone(), layout)
            .compact_heavy(chunk_id)
            .await
            .unwrap();
        let mut actual = vec![0; 8192];
        blocks
            .read_range((new_id, 0), 0, &mut actual)
            .await
            .unwrap();
        assert!(actual[..2048].iter().all(|b| *b == b'a'));
        assert!(actual[6144..].iter().all(|b| *b == b'a'));
        assert!(
            actual[2048..6144].iter().all(|b| *b == b'b'),
            "heavy compaction resurrected the older, higher-ID data"
        );
    }

    async fn setup_test_env() -> (TempDir, Arc<DatabaseMetaStore>, Arc<InMemoryBlockStore>) {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");

        let config = Config {
            database: DatabaseConfig {
                db_config: DatabaseType::Sqlite {
                    url: format!("sqlite://{}?mode=rwc", db_path.display()),
                },
            },
            cache: Default::default(),
            client: Default::default(),
            compact: Default::default(),
        };

        let meta_store = Arc::new(DatabaseMetaStore::from_config(config).await.unwrap());
        let block_store = Arc::new(InMemoryBlockStore::new());

        meta_store.initialize().await.unwrap();

        (tmp_dir, meta_store, block_store)
    }

    #[tokio::test]
    async fn test_should_compact_empty_chunk() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store, block_store);

        // Create a chunk with no slices
        let (should_compact, is_sync) = compactor.should_compact(9999).await.unwrap();

        assert!(!should_compact, "Empty chunk should not trigger compaction");
        assert!(!is_sync, "Empty chunk should not require sync");
    }

    #[tokio::test]
    async fn test_should_compact_single_slice() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);

        // Create a file with single slice
        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "single.txt".to_string())
            .await
            .unwrap();

        // Write single slice
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();
        let slice = SliceDesc {
            slice_id: 1,
            chunk_id,
            offset: 0,
            length: 4096,
        };
        meta_store.write(file, chunk_id, slice, 4096).await.unwrap();

        let (should_compact, is_sync) = compactor.should_compact(chunk_id).await.unwrap();

        assert!(
            !should_compact,
            "Single slice chunk should not trigger compaction"
        );
        assert!(!is_sync);
    }

    #[tokio::test]
    async fn test_analyze_chunk_fragmentation() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);

        // Create a file with overlapping slices
        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "frag.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();

        // Create overlapping slices to generate fragmentation
        for i in 0..5u64 {
            let slice = SliceDesc {
                slice_id: i + 1,
                chunk_id,
                offset: i * 512, // Overlapping: 0-1024, 512-1536, etc.
                length: 1024,
            };
            meta_store
                .write(file, chunk_id, slice, (i + 1) * 512 + 1024)
                .await
                .unwrap();
        }

        let (count, _total, frag) = compactor.analyze_chunk(chunk_id).await.unwrap();

        assert_eq!(count, 5, "Should have 5 slices");
        assert!(
            frag > 0.0,
            "Fragmentation should be > 0 for overlapping slices"
        );
        println!("Fragmentation: {:.2}", frag);
    }

    #[tokio::test]
    async fn test_compact_light_removes_fully_covered() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);

        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "light.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();

        // Create fully covered scenario:
        // Slice 1: [0, 1000) - will be fully covered
        // Slice 2: [0, 2000) - covers slice 1 completely
        let slice1 = SliceDesc {
            slice_id: 1,
            chunk_id,
            offset: 0,
            length: 1000,
        };
        meta_store
            .write(file, chunk_id, slice1, 1000)
            .await
            .unwrap();

        let slice2 = SliceDesc {
            slice_id: 2,
            chunk_id,
            offset: 0,
            length: 2000,
        };
        meta_store
            .write(file, chunk_id, slice2, 2000)
            .await
            .unwrap();

        let result = compactor.compact_light(chunk_id).await.unwrap();

        assert!(result.is_some(), "Should have removed slices");
        assert_eq!(result.unwrap(), 1, "Should remove 1 fully covered slice");

        // Verify slice count after compaction
        let slices = meta_store.get_slices(chunk_id).await.unwrap();
        assert_eq!(slices.len(), 1, "Should have 1 slice remaining");
        assert_eq!(slices[0].slice_id, 2, "Remaining slice should be #2");
    }

    #[tokio::test]
    async fn test_compact_light_no_change_when_no_full_coverage() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);

        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "no_cover.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();

        // Create partially overlapping slices (no full coverage)
        // Slice 1: [0, 1000)
        // Slice 2: [500, 1500) - overlaps but doesn't fully cover slice 1
        let slice1 = SliceDesc {
            slice_id: 1,
            chunk_id,
            offset: 0,
            length: 1000,
        };
        meta_store
            .write(file, chunk_id, slice1, 1000)
            .await
            .unwrap();

        let slice2 = SliceDesc {
            slice_id: 2,
            chunk_id,
            offset: 500,
            length: 1000,
        };
        meta_store
            .write(file, chunk_id, slice2, 1500)
            .await
            .unwrap();

        let result = compactor.compact_light(chunk_id).await.unwrap();

        assert!(result.is_none(), "Should not remove any slices");

        let slices = meta_store.get_slices(chunk_id).await.unwrap();
        assert_eq!(slices.len(), 2, "Should still have 2 slices");
    }

    #[tokio::test]
    async fn test_compact_chunk_skips_when_no_fragmentation() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);

        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "no_frag.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();

        // Create non-overlapping slices (no fragmentation)
        for i in 0..3u64 {
            let slice = SliceDesc {
                slice_id: i + 1,
                chunk_id,
                offset: i * 2048, // No overlap
                length: 1024,
            };
            meta_store
                .write(file, chunk_id, slice, i * 2048 + 1024)
                .await
                .unwrap();
        }

        let result = compactor.compact_chunk(chunk_id).await.unwrap();

        assert_eq!(
            result,
            CompactResult::Skipped,
            "Should skip when no fragmentation"
        );
    }

    #[tokio::test]
    async fn test_config_thresholds() {
        use brewfs::meta::config::CompactConfig;
        use std::time::Duration;

        let (_tmp, meta_store, block_store) = setup_test_env().await;

        // Custom config with very low thresholds
        let config = CompactConfig {
            min_slice_count: 2,       // Very low
            min_fragment_ratio: 0.01, // Very low
            async_threshold: 5,
            sync_threshold: 10,
            interval: Duration::from_secs(60),
            max_chunks_per_run: 100,
            max_concurrent_tasks: 2,
            lock_ttl: Default::default(),
            ..Default::default()
        };

        let compactor = Compactor::with_config(
            meta_store.clone(),
            block_store,
            ChunkLayout::default(),
            config,
        );

        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "threshold.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();

        // Create overlapping slices
        for i in 0..3u64 {
            let slice = SliceDesc {
                slice_id: i + 1,
                chunk_id,
                offset: i * 100,
                length: 200,
            };
            meta_store.write(file, chunk_id, slice, 500).await.unwrap();
        }

        let (should_compact, _) = compactor.should_compact(chunk_id).await.unwrap();
        assert!(should_compact, "Should trigger with low thresholds");
    }

    #[tokio::test]
    async fn heavy_compaction_preserves_metadata_when_a_referenced_block_is_missing() {
        let (_tmp, meta_store, block_store) = setup_test_env().await;
        let compactor = Compactor::new(meta_store.clone(), block_store);
        let root = meta_store.root_ino();
        let file = meta_store
            .create_file(root, "missing-block.txt".to_string())
            .await
            .unwrap();
        let chunk_id = brewfs::vfs::chunk_id_for(file, 0).unwrap();
        let original = SliceDesc {
            slice_id: 700,
            chunk_id,
            offset: 0,
            length: 4096,
        };
        meta_store
            .write(file, chunk_id, original, original.length)
            .await
            .unwrap();

        let error = compactor.compact_heavy(chunk_id).await.unwrap_err();
        assert!(
            matches!(error, brewfs::chunk::CompactorError::BlockStoreError(_)),
            "missing data must abort heavy compaction: {error}"
        );
        assert_eq!(
            meta_store.get_slices(chunk_id).await.unwrap(),
            vec![original],
            "failed compaction must not replace the authoritative slice metadata"
        );
    }
}
