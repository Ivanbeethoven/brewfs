use super::*;
use futures_util::future::{Abortable, FutureExt};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Owner(Arc<AtomicUsize>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn unknown_then_registered_retry_cancels_before_first_poll_and_drops_owner() {
    let registry = ReadRegistry::default();
    assert_eq!(
        registry.interrupt(9).unwrap_err(),
        Errno::from(libc::EAGAIN)
    );
    let dropped = Arc::new(AtomicUsize::new(0));
    let owner: MetadataMemoryGuard = Arc::new(Owner(dropped.clone()));
    let (registered, abort) = registry.register(9, Some(owner)).unwrap();
    registry.interrupt(9).unwrap();
    assert!(
        Abortable::new(std::future::pending::<()>(), abort)
            .now_or_never()
            .unwrap()
            .is_err()
    );
    registered.finish();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry.interrupt(9).unwrap_err(),
        Errno::from(libc::EAGAIN)
    );
}

#[test]
fn unique_isolation_duplicate_rejection_and_completion_removal() {
    let registry = ReadRegistry::default();
    let (first, first_abort) = registry.register(1, None).unwrap();
    let (second, second_abort) = registry.register(2, None).unwrap();
    assert!(matches!(registry.register(1, None), Err(error) if error == Errno::from(libc::EBUSY)));
    registry.interrupt(1).unwrap();
    assert!(
        Abortable::new(std::future::pending::<()>(), first_abort)
            .now_or_never()
            .unwrap()
            .is_err()
    );
    assert!(
        Abortable::new(std::future::ready(7), second_abort)
            .now_or_never()
            .unwrap()
            .is_ok()
    );
    first.finish();
    second.finish();
    assert_eq!(
        registry.interrupt(2).unwrap_err(),
        Errno::from(libc::EAGAIN)
    );
    let (reused, _) = registry.register(1, None).unwrap();
    drop(reused);
    assert!(registry.requests.lock().unwrap().requests.is_empty());
}

#[test]
fn dropping_registration_removes_unique_and_releases_admitted_state() {
    let registry = ReadRegistry::default();
    let dropped = Arc::new(AtomicUsize::new(0));
    let owner: MetadataMemoryGuard = Arc::new(Owner(dropped.clone()));
    let (registered, abort) = registry.register(3, Some(owner)).unwrap();
    drop(abort);
    drop(registered);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry.interrupt(3).unwrap_err(),
        Errno::from(libc::EAGAIN)
    );
}

#[tokio::test]
async fn shutdown_aborts_all_uniques_waits_for_owner_drop_and_closes_registration() {
    let registry = Arc::new(ReadRegistry::default());
    let dropped = Arc::new(AtomicUsize::new(0));
    let (first, first_abort) = registry
        .register(11, Some(Arc::new(Owner(dropped.clone()))))
        .unwrap();
    let (second, second_abort) = registry
        .register(12, Some(Arc::new(Owner(dropped.clone()))))
        .unwrap();
    let shutdown_registry = registry.clone();
    let shutdown = tokio::spawn(async move { shutdown_registry.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    assert!(
        Abortable::new(std::future::pending::<()>(), first_abort)
            .now_or_never()
            .unwrap()
            .is_err()
    );
    assert!(
        Abortable::new(std::future::pending::<()>(), second_abort)
            .now_or_never()
            .unwrap()
            .is_err()
    );
    first.finish();
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    second.finish();
    shutdown.await.unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert!(matches!(registry.register(13, None), Err(error) if error == Errno::from(libc::EIO)));
    registry.shutdown().await;
}

#[tokio::test]
async fn shutdown_waits_for_actual_owner_drop_after_unique_map_is_empty() {
    #[derive(Debug)]
    struct BlockedOwner {
        entered: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Drop for BlockedOwner {
        fn drop(&mut self) {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
    }
    let registry = Arc::new(ReadRegistry::default());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let owner: MetadataMemoryGuard = Arc::new(BlockedOwner {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let (registered, abort) = registry.register(21, Some(owner)).unwrap();
    drop(abort);
    let shutdown_registry = registry.clone();
    let shutdown = tokio::spawn(async move { shutdown_registry.shutdown().await });
    tokio::task::yield_now().await;
    let retiring = tokio::task::spawn_blocking(move || drop(registered));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match entered_rx.try_recv() {
                Ok(()) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("owner Drop did not reach release gate: {error}"),
            }
        }
    })
    .await
    .expect("owner Drop did not start");
    let map_empty = registry.requests.lock().unwrap().requests.is_empty();
    tokio::task::yield_now().await;
    let completed_before_owner_release = shutdown.is_finished();
    // Release before any assertions so even a failing candidate can join.
    release_tx.send(()).unwrap();
    retiring.await.unwrap();
    shutdown.await.unwrap();
    assert!(map_empty);
    assert!(!completed_before_owner_release);
    assert_eq!(registry.requests.lock().unwrap().live_registrations, 0);
}
