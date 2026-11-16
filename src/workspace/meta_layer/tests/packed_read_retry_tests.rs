use super::super::kernel_cache::retry_busy_metadata_read;
use crate::meta::store::MetaError;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn io_meta_error(errno: Option<i32>) -> MetaError {
    let error = match errno {
        Some(errno) => std::io::Error::from_raw_os_error(errno),
        None => std::io::Error::new(std::io::ErrorKind::Other, "no OS error"),
    };
    MetaError::Io(error)
}

#[tokio::test]
async fn packed_read_retry_transient_busy_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();

    let value = retry_busy_metadata_read(move || {
        let observed = observed.clone();
        async move {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                Err(io_meta_error(Some(libc::EBUSY)))
            } else {
                Ok("ready")
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(value, "ready");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn packed_read_retry_persistent_busy_stops_after_eight_attempts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();

    let error = retry_busy_metadata_read(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(io_meta_error(Some(libc::EBUSY)))
        }
    })
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        MetaError::Io(ref error) if error.raw_os_error() == Some(libc::EBUSY)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn packed_read_retry_does_not_retry_other_errors() {
    for errno in [
        Some(libc::EIO),
        Some(libc::ESTALE),
        Some(libc::EACCES),
        None,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();

        let result = retry_busy_metadata_read(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(io_meta_error(errno))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "errno {errno:?} must not be retried"
        );
    }
}

#[tokio::test]
async fn packed_read_retry_uses_twenty_millisecond_busy_backoff() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let started = tokio::time::Instant::now();

    retry_busy_metadata_read(move || {
        let observed = observed.clone();
        async move {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                Err::<(), _>(io_meta_error(Some(libc::EBUSY)))
            } else {
                Ok(())
            }
        }
    })
    .await
    .unwrap();

    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "the first retry must wait for the configured backoff"
    );
}
