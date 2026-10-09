//! Candidate only: the real readonly worker and reply channel are exercised.
//! The existing physical-unmount test seam is the only substituted operation.
//! Packet retirement here is not evidence of a successful /dev/fuse write.
use super::*;
use crate::raw::reply::{ReplyData, ReplyInit};
use futures_util::FutureExt;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

const READ_UNIQUE: u64 = 4241;
const DEADLINE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct ReplyLifetimeFs {
    reads: Mutex<BTreeMap<u64, oneshot::Sender<()>>>,
    entered: AtomicUsize,
    live: AtomicUsize,
    dropped: AtomicUsize,
    prepared: AtomicUsize,
    request_owners: Arc<AtomicUsize>,
    destroy_calls: AtomicUsize,
    ordinary_calls: AtomicUsize,
    ordinary_saw_live: AtomicUsize,
    ordinary_saw_owners: AtomicUsize,
    events: Mutex<Vec<&'static str>>,
}

struct PendingReadLifetime<'a>(&'a ReplyLifetimeFs);
impl Drop for PendingReadLifetime<'_> {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
        self.0.dropped.fetch_add(1, Ordering::AcqRel);
        self.0.events.lock().unwrap().push("read_dropped");
    }
}

#[derive(Debug)]
struct RequestOwner(Arc<AtomicUsize>);
impl Drop for RequestOwner {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ReplyLifetimeFs {
    async fn drain_reads(&self) {
        let reads = std::mem::take(&mut *self.reads.lock().unwrap());
        for (_, sender) in reads {
            let _ = sender.send(());
        }
        while self.live.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    }

    fn request_owner(&self) -> crate::raw::reply::ReplyMemoryGuard {
        self.request_owners.fetch_add(1, Ordering::AcqRel);
        Arc::new(RequestOwner(self.request_owners.clone()))
    }
}

impl Filesystem for ReplyLifetimeFs {
    async fn init(&self, _: Request) -> crate::Result<ReplyInit> {
        Ok(ReplyInit::default())
    }

    async fn destroy(&self, _: Request) {
        self.destroy_calls.fetch_add(1, Ordering::AcqRel);
        self.events.lock().unwrap().push("destroy_entered");
        self.drain_reads().await;
        self.events.lock().unwrap().push("destroy_drained");
    }

    fn supports_read_cancellation(&self) -> bool {
        true
    }

    async fn prepare_unmount(&self) -> crate::Result<()> {
        self.drain_reads().await;
        self.prepared.fetch_add(1, Ordering::AcqRel);
        self.events.lock().unwrap().push("prepare_drained");
        Ok(())
    }

    async fn read(
        &self,
        req: Request,
        _: crate::Inode,
        _: u64,
        _: u64,
        _: u32,
    ) -> crate::Result<ReplyData> {
        let (sender, receiver) = oneshot::channel();
        assert!(self
            .reads
            .lock()
            .unwrap()
            .insert(req.unique, sender)
            .is_none());
        self.live.fetch_add(1, Ordering::AcqRel);
        let _lifetime = PendingReadLifetime(self);
        self.entered.fetch_add(1, Ordering::Release);
        let _ = receiver.await;
        Err(libc::EINTR.into())
    }

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

fn owned_read_item(session: &Session<ReplyLifetimeFs>, fs: &ReplyLifetimeFs) -> WorkItem {
    let mut body = vec![0; std::mem::size_of::<fuse_read_in>()];
    body[..8].copy_from_slice(&1u64.to_le_bytes());
    body[16..20].copy_from_slice(&4096u32.to_le_bytes());
    WorkItem {
        unique: READ_UNIQUE,
        opcode: fuse_opcode::FUSE_READ as u32,
        in_header: InHeaderLite {
            nodeid: 1,
            uid: 0,
            gid: 0,
            pid: 0,
        },
        data: Bytes::from(body),
        _inflight_guard: Some(InflightGuard::new(
            session.inflight.clone(),
            session.inflight_notify.clone(),
        )),
        _memory_guard: Some(fs.request_owner()),
        _response_registration: None,
    }
}

async fn wait_read_entered(fs: &ReplyLifetimeFs) {
    tokio::time::timeout(DEADLINE, async {
        while fs.entered.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the real worker did not enter the pending read");
}

async fn cancelled_reply(receiver: &mut UnboundedReceiver<FuseData>) -> FuseData {
    let packet = tokio::time::timeout(DEADLINE, receiver.next())
        .await
        .expect("the real worker did not enqueue the cancellation reply")
        .expect("reply channel closed before the cancellation reply");
    let header = match &packet {
        Either::Left(data) => data.as_slice(),
        Either::Right((header, body)) if header.is_empty() => body.as_ref(),
        Either::Right((header, _)) => header.as_slice(),
    };
    assert_eq!(header.len(), FUSE_OUT_HEADER_SIZE);
    assert_eq!(
        i32::from_le_bytes(header[4..8].try_into().unwrap()),
        -libc::EINTR
    );
    assert_eq!(
        u64::from_le_bytes(header[8..16].try_into().unwrap()),
        READ_UNIQUE
    );
    packet
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_read_reply_keeps_inflight_until_last_packet_reference_retires() {
    let fs = Arc::new(ReplyLifetimeFs::default());
    let mut session = Session::new(MountOptions::default()).with_workers(1, 2);
    let mut replies = session.response_receivers[0].take().unwrap();
    session.ensure_workers(fs.clone()).unwrap();
    session
        .workers
        .as_ref()
        .unwrap()
        .submit(owned_read_item(&session, &fs));
    wait_read_entered(&fs).await;
    assert_eq!(session.inflight.load(Ordering::Acquire), 1);

    fs.prepare_unmount()
        .await
        .expect("fixture preparation failed");
    let packet = cancelled_reply(&mut replies).await;
    // The reply already exists, then joining the owning worker rules out any
    // remaining WorkItem as the source of the retained inflight guard.
    session.workers.as_mut().unwrap().shutdown().await;
    let after_worker_join = session.inflight.load(Ordering::Acquire);
    let owners_after_worker_join = fs.request_owners.load(Ordering::Acquire);
    let last_reference = packet.clone();
    drop(packet);
    let after_original_drop = session.inflight.load(Ordering::Acquire);
    let owners_after_original_drop = fs.request_owners.load(Ordering::Acquire);
    drop(last_reference);

    // Assert only after deterministic cleanup, so a RED run does not leave
    // the worker or a synthetic packet owner behind.
    assert_eq!(fs.live.load(Ordering::Acquire), 0);
    assert_eq!(fs.dropped.load(Ordering::Acquire), 1);
    assert_eq!(owners_after_worker_join, 1);
    assert_eq!(owners_after_original_drop, 1);
    assert_eq!(fs.request_owners.load(Ordering::Acquire), 0);
    assert_eq!(session.inflight.load(Ordering::Acquire), 0);
    assert_eq!(
        after_worker_join, 1,
        "queued reply lost the request inflight lifetime"
    );
    assert_eq!(
        after_original_drop, 1,
        "another reply reference still owns the packet"
    );
}

struct ReplyTestMount {
    handle: MountHandle,
    notify: Arc<async_notify::Notify>,
    finished: oneshot::Receiver<()>,
    inflight: Arc<AtomicUsize>,
    replies: UnboundedReceiver<FuseData>,
    tracker: Option<Arc<ReplyTracker>>,
}

fn reply_test_mount(fs: Arc<ReplyLifetimeFs>) -> IoResult<ReplyTestMount> {
    let mut session = Session::new(MountOptions::default()).with_workers(1, 2);
    let replies = session.response_receivers[0].take().unwrap();
    session.ensure_workers(fs.clone())?;
    session
        .workers
        .as_ref()
        .unwrap()
        .submit(owned_read_item(&session, &fs));
    let inflight = session.inflight.clone();
    let (pre_unmount, pre_unmount_memory) = session.prepare_pre_unmount(&fs)?;
    let tracker = session.readonly_reply_tracker.clone();
    let notify = Arc::new(async_notify::Notify::new());
    let task_notify = notify.clone();
    let task_fs = fs.clone();
    let (finished_sender, finished) = oneshot::channel();
    let task = task::spawn(async move {
        task_notify.notified().await;
        task_fs
            .destroy(Request {
                unique: 0,
                uid: 0,
                gid: 0,
                pid: 0,
            })
            .await;
        if let Some(mut workers) = session.workers.take() {
            workers.shutdown().await;
        }
        task_fs.events.lock().unwrap().push("session_joined");
        let _ = finished_sender.send(());
        Ok(())
    });
    let operation = OrdinaryUnmountForTest {
        future: Box::pin(async move {
            fs.ordinary_calls.fetch_add(1, Ordering::AcqRel);
            fs.events.lock().unwrap().push("ordinary_unmount");
            let live = fs.live.load(Ordering::Acquire);
            let owners = fs.request_owners.load(Ordering::Acquire);
            fs.ordinary_saw_live.store(live, Ordering::Release);
            fs.ordinary_saw_owners.store(owners, Ordering::Release);
            // Test oracle for premature entry, not a model of kernel EBUSY.
            if live != 0 || owners != 0 {
                return Err(IoError::from_raw_os_error(libc::EBUSY));
            }
            Ok(())
        }),
    };
    Ok(ReplyTestMount {
        handle: MountHandle {
            inner: Some(MountHandleInner {
                task,
                mount_path: PathBuf::from("/test-transport-no-real-mount"),
                destroy_notify: notify.clone(),
                pre_unmount,
                pre_unmount_memory,
                #[cfg(feature = "unprivileged")]
                unprivileged: false,
                ordinary_unmount_for_test: Some(operation),
            }),
        },
        notify,
        finished,
        inflight,
        replies,
        tracker,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn public_unmount_waits_for_cancelled_read_reply_owner_before_physical_unmount() {
    let fs = Arc::new(ReplyLifetimeFs::default());
    let ReplyTestMount {
        handle,
        notify,
        finished,
        inflight,
        mut replies,
        tracker,
    } = reply_test_mount(fs.clone()).unwrap();
    wait_read_entered(&fs).await;
    let mut unmount = Box::pin(handle.unmount());
    // The read is known to be pending. Polling the public API starts real
    // preparation, which sends the cancellation but cannot yet drain it.
    assert!(unmount.as_mut().now_or_never().is_none());
    let packet = cancelled_reply(&mut replies).await;
    let last_reference = packet.clone();
    drop(packet);
    assert_eq!(fs.live.load(Ordering::Acquire), 0);
    assert_eq!(fs.dropped.load(Ordering::Acquire), 1);
    assert_eq!(fs.request_owners.load(Ordering::Acquire), 1);

    // Receiving a packet must not be mistaken for completing a write. A last
    // physical body reference is deliberately held across this observation.
    // A second direct poll resumes preparation after the actual worker has
    // produced the response. No sleep or scheduling interval is the oracle.
    let observation = unmount.as_mut().now_or_never();
    let stayed_pending = observation.is_none();
    let premature_errno = match observation {
        Some(result) => result.err().and_then(|error| error.raw_os_error()),
        None => None,
    };
    assert_eq!(fs.prepared.load(Ordering::Acquire), 1);
    let calls_while_owned = fs.ordinary_calls.load(Ordering::Acquire);
    let inflight_while_owned = inflight.load(Ordering::Acquire);
    // Complete this final packet through the real production socket pump.
    // Manual drop alone now correctly records a terminal failure.
    super::unmount_order_tests::write_test_packet::<ReplyLifetimeFs>(last_reference, tracker)
        .await
        .unwrap();
    if stayed_pending {
        tokio::time::timeout(DEADLINE, &mut unmount)
            .await
            .expect("public unmount did not resume after final reply retirement")
            .expect("ordinary test unmount failed after reply retirement");
    } else {
        // On the old code, the seam reports EBUSY and no production destroy
        // notification is sent. Clean up explicitly before recording RED.
        notify.notify();
    }
    tokio::time::timeout(DEADLINE, finished)
        .await
        .expect("test session cleanup stalled")
        .expect("test session cleanup sender dropped");
    assert_eq!(fs.live.load(Ordering::Acquire), 0);
    assert_eq!(fs.request_owners.load(Ordering::Acquire), 0);
    assert_eq!(inflight.load(Ordering::Acquire), 0);
    assert_eq!(fs.destroy_calls.load(Ordering::Acquire), 1);
    assert!(
        stayed_pending,
        "physical unmount ran before packet retirement: {premature_errno:?}"
    );
    assert_eq!(calls_while_owned, 0);
    assert_eq!(inflight_while_owned, 1);
    assert_eq!(fs.ordinary_calls.load(Ordering::Acquire), 1);
    assert_eq!(fs.ordinary_saw_live.load(Ordering::Acquire), 0);
    assert_eq!(fs.ordinary_saw_owners.load(Ordering::Acquire), 0);
    assert_eq!(
        fs.events.lock().unwrap().as_slice(),
        [
            "read_dropped",
            "prepare_drained",
            "ordinary_unmount",
            "destroy_entered",
            "destroy_drained",
            "session_joined",
        ]
    );
}
