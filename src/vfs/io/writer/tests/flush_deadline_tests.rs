use super::*;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_with_deadline_expires_while_flush_notify_repeats() {
    let layout = ChunkLayout {
        chunk_size: 8 * 1024,
        block_size: 4 * 1024,
    };
    let store = Arc::new(BlockingStore::new(true));
    let meta_handle = create_meta_store_from_url("sqlite::memory:").await.unwrap();
    let meta = meta_handle.layer();
    let backend = Arc::new(Backend::new(store.clone(), meta.clone()));
    let ino = meta
        .create_file(1, "flush_deadline_repeated_notify.txt".to_string())
        .await
        .unwrap();
    let inode = Inode::new(ino, 0);
    let reader = Arc::new(DataReader::new(
        Arc::new(ReadConfig::new(layout)),
        backend.clone(),
    ));
    let file_writer = FileWriter::new(
        inode,
        test_config_without_auto_flush(layout),
        backend,
        reader,
        Arc::new(AtomicU64::new(0)),
        None,
    );
    file_writer
        .write_at(0, &vec![0x5a; layout.block_size as usize / 2])
        .await
        .unwrap();

    let chunk_id = chunk_id_for(ino, 0).unwrap();
    let slice = file_writer.shared.inner.lock().await.chunks[&chunk_id]
        .slices
        .front()
        .unwrap()
        .clone();

    let shared = file_writer.shared.clone();
    let notifier = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        loop {
            ticker.tick().await;
            shared.flush_notify.notify_waiters();
        }
    });

    let deadline = Duration::from_millis(60);
    let started = Instant::now();
    let first_flush = tokio::time::timeout(
        Duration::from_secs(1),
        file_writer.flush_with_deadline(deadline),
    )
    .await;
    let elapsed = started.elapsed();
    notifier.abort();
    let _ = notifier.await;

    let remained_pending = {
        let state = slice.lock();
        !matches!(state.state, SliceStatus::Committed)
            && state.upload_task_active
            && !state.upload_complete()
    };

    store.unblock();
    store.notify.notify_one();
    tokio::time::timeout(
        Duration::from_secs(1),
        file_writer.flush_with_deadline(Duration::from_secs(1)),
    )
    .await
    .expect("flush should succeed after the held upload is released")
    .unwrap();

    assert!(
        remained_pending,
        "the slice should still have an active, incomplete upload when the deadline expires"
    );
    assert!(matches!(slice.lock().state, SliceStatus::Committed));

    let error = first_flush
        .expect("flush_with_deadline should return before the one-second outer timeout")
        .expect_err("flush_with_deadline should return an error while the upload remains blocked");
    assert!(
        error.to_string().contains("flush timeout"),
        "unexpected flush error: {error:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(40),
        "flush should wait near its configured deadline, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "flush should honor its deadline, elapsed {elapsed:?}"
    );
}
