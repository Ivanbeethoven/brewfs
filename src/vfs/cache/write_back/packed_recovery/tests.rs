use super::*;

fn key(sequence: u64) -> DirtySliceKey {
    DirtySliceKey {
        ino: 10,
        chunk_id: 22,
        local_seq: sequence,
        epoch: 7,
    }
}

#[tokio::test]
async fn packed_recovery_strict_inventory_roundtrip_tracks_remaining_real_rows() {
    let temp = tempfile::tempdir().unwrap();
    let cache = FsWriteBackCache::new_with_sync(temp.path().to_path_buf(), false);
    cache
        .persist_slice_data(key(1), vec![Bytes::from_static(b"payload")], 0)
        .await
        .unwrap();
    cache.seal_slice_record(key(1), 0, 7).await.unwrap();
    let recovered = FsWriteBackCache::new_with_sync(temp.path().to_path_buf(), false);
    assert!(!recovered.has_recoverable_records());
    let rows = recovered.recover_packed_publication().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, key(1));
    assert!(recovered.has_recoverable_records());
    recovered.remove(&key(1)).await.unwrap();
    assert!(!recovered.has_recoverable_records());
}

#[tokio::test]
async fn packed_recovery_rejects_oversized_slice_description_before_reading_data() {
    let temp = tempfile::tempdir().unwrap();
    let cache = FsWriteBackCache::new_with_sync(temp.path().to_path_buf(), false);
    let path = key(1).sealed_slice_path(temp.path(), 0, MAX_PACKED_RECOVERY_SLICE_BYTES + 1, None);
    fs::create_dir_all(path.parent().unwrap()).await.unwrap();
    fs::write(&path, b"one byte is enough to reject declared size")
        .await
        .unwrap();
    let error = cache.recover_packed_publication().await.unwrap_err();
    assert!(error.to_string().contains("slice byte cap"));
    assert!(!cache.has_recoverable_records());
}

#[tokio::test]
async fn packed_recovery_rejects_oversized_json_and_row_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let cache = FsWriteBackCache::new_with_sync(temp.path().to_path_buf(), false);
    let path = key(0).meta_path(temp.path());
    fs::create_dir_all(path.parent().unwrap()).await.unwrap();
    fs::write(&path, vec![b' '; MAX_RECOVERY_META_BYTES + 1])
        .await
        .unwrap();
    assert!(
        cache
            .recover_packed_publication()
            .await
            .unwrap_err()
            .to_string()
            .contains("metadata byte cap")
    );
    fs::remove_file(path).await.unwrap();
    for sequence in 0..=MAX_RECOVERY_ROWS {
        let path = key(sequence as u64).sealed_slice_path(temp.path(), 0, 1, None);
        fs::write(path, b"x").await.unwrap();
    }
    assert!(
        cache
            .recover_packed_publication()
            .await
            .unwrap_err()
            .to_string()
            .contains("row cap")
    );
    assert!(!cache.has_recoverable_records());
}
