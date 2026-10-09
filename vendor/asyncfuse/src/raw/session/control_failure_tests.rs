//! Exercise the production session waits with a non-publishing control channel.
use super::*;
use crate::raw::reply::ReplyInit;
use std::time::Duration;

#[derive(Debug)]
struct TestFilesystem;
impl Filesystem for TestFilesystem {
    async fn init(&self, _: Request) -> crate::Result<ReplyInit> {
        Ok(ReplyInit::default())
    }
    async fn destroy(&self, _: Request) {}
    #[cfg(feature = "file-lock")]
    async fn getlk(
        &self,
        _: Request,
        _: crate::Inode,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u32,
        _: u32,
    ) -> crate::Result<crate::raw::reply::ReplyLock> {
        Err(libc::ENOSYS.into())
    }
    #[cfg(feature = "file-lock")]
    async fn setlk(
        &self,
        _: Request,
        _: crate::Inode,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u32,
        _: u32,
        _: bool,
    ) -> crate::Result<()> {
        Err(libc::ENOSYS.into())
    }
}

#[tokio::test]
async fn idle_reply_task_returns_shared_ring_failure_without_channel_close() {
    let (connection, control) = FuseConnection::test_control_channel();
    let (sender, receiver) = unbounded();
    let task = tokio::spawn(Session::<TestFilesystem>::reply_fuse(
        Arc::new(connection),
        receiver,
        None,
    ));
    tokio::task::yield_now().await;
    assert!(!sender.is_closed());
    control.fail(libc::EIO);
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("idle reply task retained its wait after ring failure")
        .unwrap()
        .unwrap_err();
    assert_eq!(result.raw_os_error(), Some(libc::EIO));
    assert!(sender.is_closed());
    drop(sender);
}

#[tokio::test]
async fn full_backpressure_wait_returns_persistent_ring_failure() {
    let (connection, control) = FuseConnection::test_control_channel();
    let mut session = Session::<TestFilesystem>::new(MountOptions::default());
    session.max_background = 1;
    session.inflight.store(1, Ordering::Release);
    let task = tokio::spawn(async move { session.wait_for_request_capacity(&connection).await });
    tokio::task::yield_now().await;
    control.fail(libc::EIO);
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("backpressure waiter ignored shared reply-ring failure")
        .unwrap()
        .unwrap_err();
    assert_eq!(result.raw_os_error(), Some(libc::EIO));
}

#[tokio::test]
async fn failed_dispatch_read_is_fatal_and_retains_the_errno() {
    let (connection, control) = FuseConnection::test_control_channel();
    control.fail(libc::EIO);
    let mut session = Session::<TestFilesystem>::new(MountOptions::default());
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        session.read_fuse_request(
            &connection,
            vec![0; FUSE_IN_HEADER_SIZE],
            AlignedBuffer::try_new(FUSE_MIN_READ_BUFFER_SIZE).unwrap(),
        ),
    )
    .await
    .expect("failed session read did not return");
    match result {
        ReadResult::Fatal(error) => assert_eq!(error.raw_os_error(), Some(libc::EIO)),
        other => panic!("ring failure became an ordinary read/destroy result: {other:?}"),
    }
}

#[tokio::test]
async fn init_read_failure_remains_eio_instead_of_successful_destroy() {
    let (connection, control) = FuseConnection::test_control_channel();
    control.fail(libc::EIO);
    let mut session = Session::<TestFilesystem>::new(MountOptions::default());
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        session.init_filesystem(&TestFilesystem, &connection),
    )
    .await
    .expect("INIT failure retained its wait")
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EIO));
}
