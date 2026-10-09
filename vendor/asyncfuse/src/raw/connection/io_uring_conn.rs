//! io_uring-based FUSE connection for Linux.
//!
//! Uses a dedicated io_uring ring thread to perform readv/writev on `/dev/fuse`
//! without blocking thread pool overhead. Communicates with the async tokio world
//! via channels.
//!
//! Performance advantage over the tokio `spawn_blocking` path:
//! - No thread context switches for each FUSE read/write
//! - Kernel processes readv/writev via the submission queue directly
//! - Can batch multiple response writes in a single io_uring submit

use std::any::Any;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::DerefMut;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::io::FromRawFd;
use std::pin::pin;
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_notify::Notify;
use futures_util::{select, FutureExt};
use io_uring::{opcode, types, IoUring};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::debug;

use super::CompleteIoResult;
use crate::raw::reply::ReplyBytes as Bytes;

/// Number of submission queue entries in the io_uring ring.
const RING_SIZE: u32 = 64;

/// Check the actual ring before a sender or any request buffer can escape.
fn validate_ring_capabilities(
    ext_arg: bool,
    nodrop: bool,
    submit_stable: bool,
    sqpoll: bool,
    iopoll: bool,
) -> io::Result<()> {
    if !ext_arg {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requires bounded EXT_ARG waits (Linux >= 5.11)",
        ));
    }
    if !nodrop {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring NODROP completion retention is required",
        ));
    }
    if !submit_stable || sqpoll || iopoll {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "quiescence proof requires private non-polling issuer and SUBMIT_STABLE",
        ));
    }
    Ok(())
}

/// Opt-in, bounded-volume observation. No borrowed buffer or connection owner
/// is retained by the monitor. This does not cancel IO or fix teardown.
#[derive(Default)]
struct RingProgress {
    phase: AtomicU8,
    published_reads: AtomicU64,
    completed_reads: AtomicU64,
    published_writes: AtomicU64,
    completed_writes: AtomicU64,
    last_tag: AtomicU64,
    last_result: AtomicI32,
}

impl RingProgress {
    fn start(fd: i32) -> Option<Arc<Self>> {
        if std::env::var("BREWFS_FUSE_TEARDOWN_DIAG").as_deref() != Ok("1") {
            return None;
        }
        let progress = Arc::new(Self::default());
        let monitor = progress.clone();
        if let Err(error) = thread::Builder::new()
            .name("fuse-ring-diag".into())
            .spawn(move || loop {
                let phase = monitor.phase.load(Ordering::Acquire);
                let phase_name = match phase {
                    0 => "starting",
                    1 => "waiting_channel",
                    2 => "preparing_requests",
                    3 => "waiting_cqe",
                    4 => "processing_cqe",
                    5 => "recovering",
                    6 => "stopped",
                    7 => "failed",
                    _ => "unknown",
                };
                tracing::info!(target: "asyncfuse::teardown", event="ring_snapshot", fd,
                    phase=phase_name,
                    published_reads=monitor.published_reads.load(Ordering::Relaxed),
                    completed_reads=monitor.completed_reads.load(Ordering::Relaxed),
                    published_writes=monitor.published_writes.load(Ordering::Relaxed),
                    completed_writes=monitor.completed_writes.load(Ordering::Relaxed),
                    last_tag=monitor.last_tag.load(Ordering::Relaxed),
                    last_result=monitor.last_result.load(Ordering::Relaxed));
                if matches!(phase, 6 | 7) {
                    break;
                }
                thread::sleep(Duration::from_secs(1));
            })
        {
            tracing::warn!(target: "asyncfuse::teardown", %error, "ring diagnostic monitor unavailable");
        }
        Some(progress)
    }
}

fn phase(progress: Option<&RingProgress>, value: u8) {
    if let Some(progress) = progress {
        progress.phase.store(value, Ordering::Release);
    }
}

/// A FUSE connection powered by io_uring for zero-overhead kernel I/O.
#[derive(Debug)]
pub struct FuseConnection {
    unmount_notify: Arc<Notify>,
    inner: IoUringConnection,
}

/// Represents a pending read request sent to the ring thread.
struct ReadRequest {
    header_buf: Vec<u8>,
    data_buf: Box<dyn OwnedReadBuffer>,
    reply: oneshot::Sender<CompleteIoResult<(Vec<u8>, Box<dyn OwnedReadBuffer>), usize>>,
}

/// Represents a pending write request sent to the ring thread.
struct WriteRequest {
    data: Bytes,
    body_extend: Option<Bytes>,
    reply: oneshot::Sender<CompleteIoResult<(Bytes, Option<Bytes>), usize>>,
}

/// Combined request type for the single ring thread channel.
enum RingRequest {
    Read(ReadRequest),
    Write(WriteRequest),
}

/// The ring owns the allocation, including inline buffers such as `[u8; N]`.
/// Neither cancellation nor moving the caller's future can invalidate it.
trait OwnedReadBuffer: Send {
    fn bytes_mut(&mut self) -> &mut [u8];
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

impl<T: DerefMut<Target = [u8]> + Send + 'static> OwnedReadBuffer for T {
    fn bytes_mut(&mut self) -> &mut [u8] {
        self.deref_mut()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

fn recover_read_buffer<T: Send + 'static>(buffer: Box<dyn OwnedReadBuffer>) -> T {
    // Only this call's T was sent with its private oneshot sender.
    *buffer
        .into_any()
        .downcast::<T>()
        .unwrap_or_else(|_| panic!("io_uring returned a different owned read buffer type"))
}

#[derive(Debug)]
struct IoUringConnection {
    tx: mpsc::Sender<RingRequest>,
    fd: i32,
    #[allow(dead_code)]
    file: Arc<File>, // kept alive for the fd
    status: Arc<RingStatus>,
}

#[derive(Debug)]
struct RingStatus {
    failure: Mutex<Option<(Option<i32>, io::ErrorKind, String)>>,
    failure_notify: watch::Sender<bool>,
}

impl Default for RingStatus {
    fn default() -> Self {
        Self {
            failure: Mutex::new(None),
            failure_notify: watch::channel(false).0,
        }
    }
}

impl RingStatus {
    fn fail(&self, error: &io::Error) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if failure.is_none() {
            *failure = Some((error.raw_os_error(), error.kind(), error.to_string()));
            // Persistent broadcast: every current and future waiter observes
            // failure, including when the driver must keep draining memory.
            self.failure_notify.send_replace(true);
        }
    }

    fn error(&self) -> Option<io::Error> {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|(errno, kind, text)| {
                errno
                    .map(io::Error::from_raw_os_error)
                    .unwrap_or_else(|| io::Error::new(*kind, text.clone()))
            })
    }

    async fn failed(&self) -> io::Error {
        let mut receiver = self.failure_notify.subscribe();
        loop {
            if let Some(error) = self.error() {
                return error;
            }
            // The status owns the sender, so it cannot close while borrowed.
            receiver
                .changed()
                .await
                .expect("ring failure sender remains alive");
        }
    }
}

impl AsFd for FuseConnection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.file.as_fd()
    }
}

impl AsRawFd for FuseConnection {
    fn as_raw_fd(&self) -> i32 {
        self.inner.fd
    }
}

impl FuseConnection {
    // Only a test fd source changes; capability validation and the ring driver
    // are the same production path used for /dev/fuse.
    #[cfg(test)]
    pub(crate) fn from_test_fd(fd: std::os::fd::OwnedFd) -> io::Result<Self> {
        let file = Arc::new(File::from(fd));
        let (tx, status) = IoUringConnection::start_ring_thread(file.clone())?;
        Ok(Self {
            unmount_notify: Arc::new(Notify::new()),
            inner: IoUringConnection {
                fd: file.as_raw_fd(),
                file,
                tx,
                status,
            },
        })
    }
    #[cfg(test)]
    pub(crate) fn test_control_channel() -> (Self, TestRingControl) {
        let (tx, rx) = mpsc::channel(128);
        let status = Arc::new(RingStatus::default());
        let file = Arc::new(File::open("/dev/null").unwrap());
        (
            Self {
                unmount_notify: Arc::new(Notify::new()),
                inner: IoUringConnection {
                    fd: file.as_raw_fd(),
                    file,
                    tx,
                    status: status.clone(),
                },
            },
            TestRingControl {
                _requests: rx,
                status,
            },
        )
    }

    /// Opens `/dev/fuse` and starts the io_uring ring thread.
    pub fn new(unmount_notify: Arc<Notify>) -> io::Result<Self> {
        const DEV_FUSE: &str = "/dev/fuse";

        let file = OpenOptions::new().write(true).read(true).open(DEV_FUSE)?;
        let fd = file.as_raw_fd();
        debug!(fd, "io_uring: opened /dev/fuse");
        let file = Arc::new(file);

        let (tx, status) = IoUringConnection::start_ring_thread(file.clone())?;

        Ok(Self {
            unmount_notify,
            inner: IoUringConnection {
                tx,
                fd,
                file,
                status,
            },
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        let new_fd = unsafe { libc::dup(self.inner.fd) };
        if new_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = Arc::new(unsafe { File::from_raw_fd(new_fd) });
        let status = self.inner.status.clone();
        let tx = IoUringConnection::start_ring_thread_with_status(file.clone(), status.clone())?;

        Ok(Self {
            unmount_notify: self.unmount_notify.clone(),
            inner: IoUringConnection {
                tx,
                fd: new_fd,
                file,
                status,
            },
        })
    }

    /// Mount with unprivileged fusermount3, then start the io_uring ring thread.
    #[cfg(all(target_os = "linux", feature = "unprivileged"))]
    pub async fn new_with_unprivileged(
        mount_options: crate::MountOptions,
        mount_path: impl AsRef<std::path::Path>,
        unmount_notify: Arc<Notify>,
    ) -> io::Result<Self> {
        use nix::sys::socket::{
            self, AddressFamily, ControlMessageOwned, MsgFlags, SockFlag, SockType,
        };
        use std::ffi::OsString;
        use std::os::fd::AsRawFd as _;
        use std::os::fd::FromRawFd as _;
        use tokio::process::Command;

        let (sock0, sock1) = socket::socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::empty(),
        )
        .map_err(io::Error::from)?;

        let binary_path = crate::find_fusermount3()?;
        let options = mount_options.build_with_unprivileged();
        let mount_path = mount_path.as_ref().as_os_str().to_os_string();

        const ENV: &str = "_FUSE_COMMFD";
        let fd0 = sock0.as_raw_fd();
        let mut child = Command::new(binary_path)
            .env(ENV, fd0.to_string())
            .args(vec![OsString::from("-o"), options, mount_path])
            .spawn()?;

        if !child.wait().await?.success() {
            return Err(io::Error::other("fusermount run failed"));
        }

        let fd1 = sock1.as_raw_fd();
        let fuse_fd = tokio::task::spawn_blocking(move || {
            let mut buf = vec![];
            let mut cmsg_buf = nix::cmsg_space!([std::os::unix::io::RawFd; 1]);
            let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
            let msg =
                socket::recvmsg::<()>(fd1, &mut bufs[..], Some(&mut cmsg_buf), MsgFlags::empty())
                    .map_err(io::Error::from)?;
            if let Some(ControlMessageOwned::ScmRights(fds)) =
                msg.cmsgs().ok().and_then(|mut c| c.next())
            {
                if fds.is_empty() {
                    return Err(io::Error::other("no fuse fd"));
                }
                Ok(fds[0])
            } else {
                Err(io::Error::other("get fuse fd failed"))
            }
        })
        .await
        .unwrap()?;

        let file = Arc::new(unsafe { File::from_raw_fd(fuse_fd) });
        let (tx, status) = IoUringConnection::start_ring_thread(file.clone())?;

        Ok(Self {
            unmount_notify,
            inner: IoUringConnection {
                tx,
                fd: fuse_fd,
                file,
                status,
            },
        })
    }

    /// Read a FUSE request from `/dev/fuse` using io_uring readv.
    /// Returns None if unmount was signaled or the ring stopped.
    /// The ring retains accepted buffers until the read CQE, even when this
    /// future is dropped. The ring observes receiver closure during its bounded
    /// control wait and cancels IO, then drains the original target CQE.
    pub async fn read_vectored<T: DerefMut<Target = [u8]> + Send + 'static>(
        &self,
        header_buf: Vec<u8>,
        data_buf: T,
    ) -> Option<CompleteIoResult<(Vec<u8>, T), usize>> {
        let (tx, rx) = oneshot::channel();

        let req = RingRequest::Read(ReadRequest {
            header_buf,
            data_buf: Box::new(data_buf),
            reply: tx,
        });

        if let Err(error) = self.inner.tx.send(req).await {
            self.inner.status.fail(&io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ring thread gone",
            ));
            let RingRequest::Read(req) = error.0 else {
                unreachable!("this call only sends a read request")
            };
            return Some((
                (req.header_buf, recover_read_buffer(req.data_buf)),
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "ring thread gone",
                )),
            ));
        }

        let mut unmount_fut = pin!(self.unmount_notify.notified().fuse());
        let mut failure_fut = pin!(self.inner.status.failed().fuse());
        let mut read_fut = pin!(rx.fuse());

        select! {
            _ = failure_fut => None,
            _ = unmount_fut => {
                debug!("io_uring read_vectored: unmount signaled");
                None
            },
            result = read_fut => {
                match result {
                    Ok(((header, buffer), res)) =>
                        Some(((header, recover_read_buffer(buffer)), res)),
                    Err(_) => {
                        self.inner.status.fail(&io::Error::other("ring read reply channel closed"));
                        None
                    },
                }
            }
        }
    }

    /// Distinguish ring failure from successful unmount when read has no buffer
    /// to return because the owning driver is still draining its target CQE.
    pub(crate) fn ring_failure(&self) -> Option<io::Error> {
        self.inner.status.error()
    }

    pub(crate) async fn wait_ring_failure(&self) -> io::Error {
        self.inner.status.failed().await
    }

    /// Write a FUSE response to `/dev/fuse` using io_uring writev.
    pub async fn write_vectored(
        &self,
        data: Bytes,
        body_extend_data: Option<Bytes>,
    ) -> CompleteIoResult<(Bytes, Option<Bytes>), usize> {
        let (tx, rx) = oneshot::channel();

        // Pass Bytes directly to the ring thread — zero-copy (Arc bump
        // only).  The ring thread reads .as_ptr() at writev time.
        let req = RingRequest::Write(WriteRequest {
            data: data.clone(),
            body_extend: body_extend_data.clone(),
            reply: tx,
        });

        if self.inner.tx.send(req).await.is_err() {
            self.inner.status.fail(&io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ring thread gone",
            ));
            return (
                (data, body_extend_data),
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "ring thread gone",
                )),
            );
        }

        let mut completion = pin!(rx.fuse());
        let mut failed = pin!(self.inner.status.failed().fuse());
        select! {
            error = failed => ((data, body_extend_data), Err(error)),
            result = completion => match result {
                Ok((_buffers, result)) => ((data, body_extend_data), result),
                Err(_) => ((data, body_extend_data), Err(io::Error::new(
                    io::ErrorKind::BrokenPipe, "ring reply channel closed"))),
            },
        }
    }
}

impl IoUringConnection {
    /// Start the io_uring ring thread. Returns a sender for ring requests.
    fn start_ring_thread(
        file: Arc<File>,
    ) -> io::Result<(mpsc::Sender<RingRequest>, Arc<RingStatus>)> {
        let status = Arc::new(RingStatus::default());
        let tx = Self::start_ring_thread_with_status(file, status.clone())?;
        Ok((tx, status))
    }

    fn start_ring_thread_with_status(
        file: Arc<File>,
        status: Arc<RingStatus>,
    ) -> io::Result<mpsc::Sender<RingRequest>> {
        let (tx, rx) = mpsc::channel::<RingRequest>(RING_SIZE as usize);
        let fd = file.as_raw_fd();
        // Fail before accepting any buffers when bounded control wakeups or
        // cancellation are unsupported; no SQ owns user memory at this point.
        let ring = IoUring::builder().build(RING_SIZE)?;
        // This default ring moves into exactly one RingDriver. Its submitter
        // never escapes that owner; kernel polling issuers are rejected too.
        validate_ring_capabilities(
            ring.params().is_feature_ext_arg(),
            ring.params().is_feature_nodrop(),
            ring.params().is_feature_submit_stable(),
            ring.params().is_setup_sqpoll(),
            ring.params().is_setup_iopoll(),
        )?;
        match ring.submitter().register_sync_cancel(
            Some(types::Timespec::from(CONTROL_POLL)),
            types::CancelBuilder::user_data(0),
        ) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            result => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "bounded exact-tag io_uring synchronous cancellation required: {result:?}"
                    ),
                ));
            }
        }
        let mut probe = io_uring::register::Probe::new();
        ring.submitter().register_probe(&mut probe)?;
        if !probe.is_supported(opcode::AsyncCancel::CODE) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring AsyncCancel is required",
            ));
        }
        let thread_status = status.clone();

        thread::Builder::new()
            .name("fuse-io-uring".into())
            .spawn(move || {
                if let Err(e) = ring_thread_main_with_ring(ring, file, rx, thread_status.clone()) {
                    thread_status.fail(&e);
                    tracing::error!(fd, error = %e, "io_uring ring thread exited with error");
                }
            })?;

        Ok(tx)
    }
}

/// Cancel completion tags are distinct from monotonically unique IO tags.
/// A late cancel CQE can never be confused with a later read/write operation.
const CANCEL_TAG: u64 = 1 << 63;
const CONTROL_POLL: Duration = Duration::from_millis(50);

#[cfg(test)]
pub(crate) struct TestRingControl {
    // This endpoint never publishes pointers or touches caller storage.
    _requests: mpsc::Receiver<RingRequest>,
    status: Arc<RingStatus>,
}
#[cfg(test)]
impl TestRingControl {
    pub(crate) fn fail(&self, errno: i32) {
        self.status.fail(&io::Error::from_raw_os_error(errno));
    }
}

struct InflightRead {
    req: ReadRequest,
    _iovecs: Box<[libc::iovec; 2]>,
}

struct InflightWrite {
    req: WriteRequest,
    _iovecs: Box<[libc::iovec; 2]>,
}

enum PendingIo {
    Read(InflightRead),
    Write(InflightWrite),
}

impl PendingIo {
    fn receiver_closed(&self) -> bool {
        match self {
            Self::Read(value) => value.req.reply.is_closed(),
            Self::Write(value) => value.req.reply.is_closed(),
        }
    }

    fn complete(self, result: io::Result<usize>) {
        match self {
            Self::Read(value) => {
                let _ = value
                    .req
                    .reply
                    .send(((value.req.header_buf, value.req.data_buf), result));
            }
            Self::Write(value) => {
                let _ = value
                    .req
                    .reply
                    .send(((value.req.data, value.req.body_extend), result));
            }
        }
    }
}

/// Owns every buffer/device until its target CQE or exact synchronous
/// quiescence proof. Cancel CQEs and ring-close alone never release owners.
struct RingDriver {
    ring: IoUring,
    file: Arc<File>,
    pending: std::collections::BTreeMap<u64, PendingIo>,
    cancel_pending: BTreeSet<u64>,
    cancel_requested: BTreeSet<u64>,
    next_tag: u64,
    status: Arc<RingStatus>,
    failure: Option<io::Error>,
    progress: Option<Arc<RingProgress>>,
    #[cfg(test)]
    enter_faults: std::collections::VecDeque<i32>,
}

impl RingDriver {
    fn new(ring: IoUring, file: Arc<File>, status: Arc<RingStatus>) -> Self {
        let progress = RingProgress::start(file.as_raw_fd());
        Self {
            ring,
            file,
            pending: Default::default(),
            cancel_pending: Default::default(),
            cancel_requested: Default::default(),
            next_tag: 1,
            status,
            failure: None,
            progress,
            #[cfg(test)]
            enter_faults: Default::default(),
        }
    }

    fn fail(&mut self, error: io::Error) {
        self.status.fail(&error);
        if self.failure.is_none() {
            self.failure = Some(error);
        }
    }

    fn io_error(&self) -> io::Error {
        self.status
            .error()
            .unwrap_or_else(|| io::Error::other("ring driver failed"))
    }

    fn all_completed(&self) -> bool {
        self.pending.is_empty() && self.cancel_pending.is_empty()
    }

    fn has_sq_capacity(&mut self) -> bool {
        !self.ring.submission().is_full()
    }

    fn accept(&mut self, request: RingRequest) -> io::Result<()> {
        phase(self.progress.as_deref(), 2);
        let fd = self.file.as_raw_fd();
        let tag = self.next_tag;
        if tag == CANCEL_TAG {
            return Err(io::Error::other("io_uring tag space exhausted"));
        }
        self.next_tag += 1;
        let (pending, entry) = match request {
            RingRequest::Read(mut req) => {
                if req.reply.is_closed() {
                    return Ok(());
                }
                if self
                    .pending
                    .values()
                    .any(|value| matches!(value, PendingIo::Read(_)))
                {
                    let _ = req.reply.send((
                        (req.header_buf, req.data_buf),
                        Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "read already inflight",
                        )),
                    ));
                    return Ok(());
                }
                let data = req.data_buf.bytes_mut();
                let data_ptr = data.as_mut_ptr();
                let data_len = data.len();
                let iovecs = Box::new([
                    libc::iovec {
                        iov_base: req.header_buf.as_mut_ptr().cast(),
                        iov_len: req.header_buf.len(),
                    },
                    libc::iovec {
                        iov_base: data_ptr.cast(),
                        iov_len: data_len,
                    },
                ]);
                let entry = opcode::Readv::new(types::Fd(fd), iovecs.as_ptr(), 2)
                    .build()
                    .user_data(tag);
                (
                    PendingIo::Read(InflightRead {
                        req,
                        _iovecs: iovecs,
                    }),
                    entry,
                )
            }
            RingRequest::Write(req) => {
                if req.reply.is_closed() {
                    return Ok(());
                }
                let (body_ptr, body_len, count) = req
                    .body_extend
                    .as_ref()
                    .map_or((std::ptr::null_mut(), 0, 1), |body| {
                        (body.as_ptr() as *mut libc::c_void, body.len(), 2)
                    });
                let iovecs = Box::new([
                    libc::iovec {
                        iov_base: req.data.as_ptr() as *mut libc::c_void,
                        iov_len: req.data.len(),
                    },
                    libc::iovec {
                        iov_base: body_ptr,
                        iov_len: body_len,
                    },
                ]);
                let entry = opcode::Writev::new(types::Fd(fd), iovecs.as_ptr(), count)
                    .build()
                    .user_data(tag);
                (
                    PendingIo::Write(InflightWrite {
                        req,
                        _iovecs: iovecs,
                    }),
                    entry,
                )
            }
        };
        // Allocate tracking BEFORE publishing the SQE; moving heap owners does
        // not move their bytes or the Box containing inline caller storage.
        let is_read = matches!(&pending, PendingIo::Read(_));
        self.pending.insert(tag, pending);
        // SAFETY: owners are in pending before SQ publication, and release is
        // permitted only by the matching target CQE or exact sync-quiescence proof.
        if unsafe { self.ring.submission().push(&entry) }.is_err() {
            self.pending
                .remove(&tag)
                .unwrap()
                .complete(Err(io::Error::other("SQ full")));
            return Err(io::Error::other("SQ full"));
        }
        if let Some(progress) = &self.progress {
            let counter = if is_read {
                &progress.published_reads
            } else {
                &progress.published_writes
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn schedule_cancels(&mut self, force: bool) -> io::Result<()> {
        let tags = self
            .pending
            .iter()
            .filter_map(|(tag, value)| (force || value.receiver_closed()).then_some(*tag))
            .collect::<Vec<_>>();
        for tag in tags {
            if self.cancel_requested.contains(&tag) {
                continue;
            }
            if !self.has_sq_capacity() {
                break;
            }
            let entry = opcode::AsyncCancel::new(tag)
                .build()
                .user_data(CANCEL_TAG | tag);
            // Allocate bookkeeping first for the same unwind safety as IO.
            self.cancel_pending.insert(tag);
            self.cancel_requested.insert(tag);
            // SAFETY: AsyncCancel contains only an integer target tag.
            if unsafe { self.ring.submission().push(&entry) }.is_err() {
                self.cancel_pending.remove(&tag);
                self.cancel_requested.remove(&tag);
                return Err(io::Error::other("cancel SQ full"));
            }
        }
        Ok(())
    }

    fn complete_cqes(&mut self) -> io::Result<()> {
        phase(self.progress.as_deref(), 4);
        // End the CQ borrow before releasing owners or updating maps.
        let completions = self
            .ring
            .completion()
            .map(|cqe| (cqe.user_data(), cqe.result()))
            .collect::<Vec<_>>();
        let mut first_error = None;
        for (tag, result) in completions {
            if let Err(error) = self.complete_one(tag, result) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn complete_one(&mut self, tag: u64, result: i32) -> io::Result<()> {
        if let Some(progress) = &self.progress {
            progress.last_tag.store(tag, Ordering::Relaxed);
            progress.last_result.store(result, Ordering::Relaxed);
        }
        if tag & CANCEL_TAG != 0 {
            let target = tag & !CANCEL_TAG;
            if !self.cancel_pending.remove(&target) {
                return Err(io::Error::other("unknown io_uring cancel CQE"));
            }
            // Cancel result does NOT free the target. EALREADY/ENOENT races
            // still need that original IO's CQE. Retry transient cancel errors.
            if !self.pending.contains_key(&target) {
                self.cancel_requested.remove(&target);
            } else if matches!(
                -result,
                libc::EINTR | libc::EAGAIN | libc::EBUSY | libc::ENOMEM
            ) {
                self.cancel_requested.remove(&target);
            } else if result < 0 && !matches!(-result, libc::ENOENT | libc::EALREADY) {
                self.cancel_requested.remove(&target);
                return Err(io::Error::from_raw_os_error(-result));
            }
            return Ok(());
        }
        let pending = self
            .pending
            .remove(&tag)
            .ok_or_else(|| io::Error::other("unknown io_uring target CQE"))?;
        if let Some(progress) = &self.progress {
            let counter = if matches!(&pending, PendingIo::Read(_)) {
                &progress.completed_reads
            } else {
                &progress.completed_writes
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        if !self.cancel_pending.contains(&tag) {
            self.cancel_requested.remove(&tag);
        }
        pending.complete(if result < 0 {
            Err(io::Error::from_raw_os_error(-result))
        } else {
            Ok(result as usize)
        });
        Ok(())
    }

    fn wait_once(&mut self) -> io::Result<()> {
        phase(
            self.progress.as_deref(),
            if self.failure.is_some() { 5 } else { 3 },
        );
        #[cfg(test)]
        if let Some(errno) = self.enter_faults.pop_front() {
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        let timeout = types::Timespec::from(CONTROL_POLL);
        match self
            .ring
            .submitter()
            .submit_with_args(1, &types::SubmitArgs::new().timespec(&timeout))
        {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ETIME) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Recover missing CQEs only after a separate kernel-quiescence proof.
    /// Exact Readv/Writev tags are unique, SQPOLL/IOPOLL and linked/multishot
    /// requests are absent, and the SQ must have been consumed before this API.
    /// A successful exact-tag sync cancellation has completed the target; an
    /// absent valid tag cannot still be queued for submission by this driver.
    /// See the saved Linux cancel.c/io-wq.c and liburing cancellation contract.
    /// Any timeout/error retains every pending owner. This is a bounded proof
    /// attempt, not a claim that an arbitrarily wedged kernel can be recovered.
    fn reclaim_after_sync_quiescence(&mut self, deadline: std::time::Instant) -> io::Result<()> {
        validate_ring_capabilities(
            self.ring.params().is_feature_ext_arg(),
            self.ring.params().is_feature_nodrop(),
            self.ring.params().is_feature_submit_stable(),
            self.ring.params().is_setup_sqpoll(),
            self.ring.params().is_setup_iopoll(),
        )?;
        if !self.ring.submission().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "unsubmitted SQ entries invalidate absent-target proof",
            ));
        }
        // No owners are released until *every* remaining target is proved.
        // ALL/ANY cancellation counts and cancel CQEs are not such a proof.
        for tag in self.pending.keys().copied() {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::from_raw_os_error(libc::ETIME));
            }
            match self.ring.submitter().register_sync_cancel(
                Some(types::Timespec::from(remaining)),
                types::CancelBuilder::user_data(tag),
            ) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                Err(error) => return Err(error),
            }
        }
        // Prefer actual completions and retain their real result. Remaining
        // owners have a separate quiescence proof but no trustworthy IO result.
        // Report the mount failure, never manufacture successful delivery.
        if let Err(error) = self.complete_cqes() {
            self.fail(error);
        }
        let missing = self.pending.len();
        if missing != 0 {
            self.fail(io::Error::other(
                "target CQE missing after exact kernel quiescence",
            ));
        }
        let failure = self.io_error();
        for (_, pending) in std::mem::take(&mut self.pending) {
            pending.complete(Err(io::Error::new(
                failure.kind(),
                "target result unavailable after kernel quiescence",
            )));
        }
        // AsyncCancel entries contain integer tags only and cannot touch a
        // reclaimed target. The failed driver never submits another user IO.
        self.cancel_pending.clear();
        self.cancel_requested.clear();
        Ok(())
    }

    /// Actual CQE loss is recovered by an exact-tag synchronous proof, not a
    /// timer, cancellation CQE or ring-close. A permanently wedged kernel that
    /// cannot produce either a target CQE or this proof remains unaccepted;
    /// preserving memory safety there is not successful finite cleanup.
    fn recover(&mut self) {
        let mut last_report = std::time::Instant::now();
        let mut sync_cancel_error = None;
        // Original read/write CQEs guard user memory and the device. Once
        // those drain, any late AsyncCancel SQE/CQE contains integer tags
        // only. This ring has no SQPOLL thread or other submitting owner.
        while !self.pending.is_empty() {
            if let Err(error) = self.complete_cqes() {
                self.fail(error);
            }
            if self.pending.is_empty() {
                break;
            }
            if let Err(error) = self.schedule_cancels(true) {
                self.fail(error);
            }
            match self.wait_once() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    sync_cancel_error = Some(error.raw_os_error());
                    self.fail(error);
                    thread::sleep(Duration::from_millis(10));
                }
            }
            // A CQE may have been permanently dropped under kernel memory
            // pressure even with NODROP. The private SQ has no new writer here.
            // This attempt is bounded and cannot release on ETIME/EINTR/etc.
            if self.ring.submission().is_empty() {
                let deadline = std::time::Instant::now() + CONTROL_POLL;
                match self.reclaim_after_sync_quiescence(deadline) {
                    Ok(()) => break,
                    Err(error) => sync_cancel_error = Some(error.raw_os_error()),
                }
            }
            if last_report.elapsed() >= Duration::from_secs(1) {
                tracing::error!(
                    targets = self.pending.len(),
                    cancels = self.cancel_pending.len(),
                    ?sync_cancel_error,
                    "io_uring failure recovery is still waiting for target CQEs"
                );
                last_report = std::time::Instant::now();
            }
        }
    }

    fn run(&mut self, rx: &mut mpsc::Receiver<RingRequest>) -> io::Result<()> {
        loop {
            // A failed reply ring also fails the dispatch ring. Outstanding IO
            // observes this during bounded waits; idle rings wake when the
            // failed session drops all senders, and own no kernel buffers.
            if self.failure.is_none() {
                if let Some(error) = self.status.error() {
                    self.fail(error);
                }
            }
            if let Err(error) = self.complete_cqes() {
                self.fail(error);
            }
            if self.failure.is_some() {
                rx.close();
                // Not-yet-published requests have no kernel access.
                while let Ok(request) = rx.try_recv() {
                    match request {
                        RingRequest::Read(req) => {
                            let _ = req
                                .reply
                                .send(((req.header_buf, req.data_buf), Err(self.io_error())));
                        }
                        RingRequest::Write(req) => {
                            let _ = req
                                .reply
                                .send(((req.data, req.body_extend), Err(self.io_error())));
                        }
                    }
                }
                self.recover();
                return Err(self.failure.take().unwrap());
            }
            let closing = rx.is_closed() && rx.is_empty();
            if let Err(error) = self.schedule_cancels(closing) {
                self.fail(error);
                continue;
            }
            if closing && self.all_completed() {
                return Ok(());
            }

            // Idle channel wait cannot retain any kernel IO: no unmount cancel
            // wakeup is needed until a read/write becomes outstanding.
            if self.all_completed() && !closing {
                phase(self.progress.as_deref(), 1);
                if let Some(request) = rx.blocking_recv() {
                    if let Err(error) = self.accept(request) {
                        self.fail(error);
                    }
                }
            }
            while self.has_sq_capacity() && self.pending.len() < RING_SIZE as usize {
                let Ok(request) = rx.try_recv() else {
                    break;
                };
                if let Err(error) = self.accept(request) {
                    self.fail(error);
                    break;
                }
            }
            if self.all_completed() {
                continue;
            }
            match self.wait_once() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => self.fail(error),
            }
        }
    }
}

impl Drop for RingDriver {
    fn drop(&mut self) {
        if !self.all_completed() {
            if self.failure.is_none() {
                self.fail(io::Error::other("ring driver dropped with outstanding IO"));
            }
            self.recover();
        }
        phase(
            self.progress.as_deref(),
            if self.status.error().is_some() { 7 } else { 6 },
        );
    }
}

fn ring_thread_main_with_ring(
    ring: IoUring,
    file: Arc<File>,
    mut rx: mpsc::Receiver<RingRequest>,
    status: Arc<RingStatus>,
) -> io::Result<()> {
    let mut driver = RingDriver::new(ring, file, status);
    driver.run(&mut rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Deref;
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug)]
    struct InlineBuffer {
        bytes: [u8; 64],
        drops: Arc<AtomicUsize>,
    }

    impl Deref for InlineBuffer {
        type Target = [u8];
        fn deref(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl DerefMut for InlineBuffer {
        fn deref_mut(&mut self) -> &mut [u8] {
            &mut self.bytes
        }
    }

    impl Drop for InlineBuffer {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn buffer(drops: &Arc<AtomicUsize>) -> InlineBuffer {
        InlineBuffer {
            bytes: [0; 64],
            drops: drops.clone(),
        }
    }

    fn fake_connection() -> (Arc<FuseConnection>, mpsc::Receiver<RingRequest>) {
        let (tx, rx) = mpsc::channel(128);
        let file = Arc::new(File::open("/dev/null").unwrap());
        (
            Arc::new(FuseConnection {
                unmount_notify: Arc::new(Notify::new()),
                inner: IoUringConnection {
                    fd: file.as_raw_fd(),
                    file,
                    tx,
                    status: Arc::new(RingStatus::default()),
                },
            }),
            rx,
        )
    }

    async fn request(rx: &mut mpsc::Receiver<RingRequest>) -> ReadRequest {
        let value = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("request deadline")
            .expect("request channel open");
        let RingRequest::Read(value) = value else {
            panic!("expected read")
        };
        value
    }

    #[tokio::test]
    async fn notify_retains_inline_buffer_until_delayed_completion() {
        let (connection, mut rx) = fake_connection();
        let drops = Arc::new(AtomicUsize::new(0));
        let data = buffer(&drops);
        let caller = connection.clone();
        let task = tokio::spawn(async move { caller.read_vectored(vec![0; 40], data).await });
        let mut req = request(&mut rx).await;
        connection.unmount_notify.notify();
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        req.header_buf.fill(0x11);
        req.data_buf.bytes_mut().fill(0x22);
        let _ = req.reply.send(((req.header_buf, req.data_buf), Ok(104)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn abort_retains_inline_buffer_until_delayed_completion() {
        let (connection, mut rx) = fake_connection();
        let drops = Arc::new(AtomicUsize::new(0));
        let data = buffer(&drops);
        let task = tokio::spawn(async move { connection.read_vectored(vec![0; 40], data).await });
        let mut req = request(&mut rx).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        req.header_buf.fill(0x33);
        req.data_buf.bytes_mut().fill(0x44);
        let _ = req.reply.send(((req.header_buf, req.data_buf), Ok(104)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn normal_completion_returns_original_type_and_both_buffers() {
        let (connection, mut rx) = fake_connection();
        let drops = Arc::new(AtomicUsize::new(0));
        let data = buffer(&drops);
        let task = tokio::spawn(async move { connection.read_vectored(vec![0; 40], data).await });
        let mut req = request(&mut rx).await;
        req.header_buf.fill(0x55);
        req.data_buf.bytes_mut().fill(0x66);
        assert!(req
            .reply
            .send(((req.header_buf, req.data_buf), Ok(104)))
            .is_ok());
        let ((header, data), result) = task.await.unwrap().unwrap();
        assert_eq!(result.unwrap(), 104);
        assert_eq!(header, vec![0x55; 40]);
        assert_eq!(&*data, &[0x66; 64]);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(data);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_send_returns_buffers_and_exposes_failure() {
        let (connection, rx) = fake_connection();
        drop(rx);
        let drops = Arc::new(AtomicUsize::new(0));
        let ((header, data), result) = connection
            .read_vectored(vec![7; 40], buffer(&drops))
            .await
            .unwrap();
        assert_eq!(header, vec![7; 40]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(
            connection.ring_failure().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(data);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reply_disconnect_is_failure_and_reclaims_unpublished_owner() {
        let (connection, mut rx) = fake_connection();
        let drops = Arc::new(AtomicUsize::new(0));
        let data = buffer(&drops);
        let caller = connection.clone();
        let task = tokio::spawn(async move { caller.read_vectored(vec![0; 40], data).await });
        drop(request(&mut rx).await);
        assert!(task.await.unwrap().is_none());
        assert!(connection.ring_failure().is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_wakes_every_waiter_and_is_visible_to_late_waiters() {
        let status = Arc::new(RingStatus::default());
        let mut tasks = Vec::new();
        for _ in 0..64 {
            let status = status.clone();
            tasks.push(tokio::spawn(
                async move { status.failed().await.raw_os_error() },
            ));
        }
        tokio::task::yield_now().await;
        status.fail(&io::Error::from_raw_os_error(libc::EIO));
        status.fail(&io::Error::from_raw_os_error(libc::ENOMEM));
        for task in tasks {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap(),
                Some(libc::EIO)
            );
        }
        assert_eq!(status.failed().await.raw_os_error(), Some(libc::EIO));
    }

    #[tokio::test]
    async fn failure_does_not_release_the_pending_read_owner() {
        let (connection, mut rx) = fake_connection();
        let drops = Arc::new(AtomicUsize::new(0));
        let data = buffer(&drops);
        let caller = connection.clone();
        let task = tokio::spawn(async move { caller.read_vectored(vec![0; 40], data).await });
        let req = request(&mut rx).await;
        connection
            .inner
            .status
            .fail(&io::Error::from_raw_os_error(libc::EIO));
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_none());
        assert_eq!(
            connection.ring_failure().unwrap().raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let _ = req.reply.send((
            (req.header_buf, req.data_buf),
            Err(io::Error::from_raw_os_error(libc::ECANCELED)),
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_wakes_all_writes_while_ring_owns_their_payloads() {
        let (connection, mut rx) = fake_connection();
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let caller = connection.clone();
            tasks.push(tokio::spawn(async move {
                caller
                    .write_vectored(
                        Bytes::from_static(b"header"),
                        Some(Bytes::from_static(b"body")),
                    )
                    .await
            }));
        }
        let mut pending = Vec::new();
        for _ in 0..32 {
            pending.push(rx.recv().await.unwrap());
        }
        connection
            .inner
            .status
            .fail(&io::Error::from_raw_os_error(libc::EIO));
        for task in tasks {
            let ((data, body), result) = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&data[..], b"header");
            assert_eq!(&body.unwrap()[..], b"body");
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
        }
        // This fake queue is unpublished and can safely drop its copies.
        drop(pending);
    }

    fn kernel_ring() -> IoUring {
        let ring = IoUring::new(RING_SIZE).expect("kernel io_uring required");
        validate_ring_capabilities(
            ring.params().is_feature_ext_arg(),
            ring.params().is_feature_nodrop(),
            ring.params().is_feature_submit_stable(),
            ring.params().is_setup_sqpoll(),
            ring.params().is_setup_iopoll(),
        )
        .expect("real kernel must support the production quiescence prerequisites");
        let mut probe = io_uring::register::Probe::new();
        ring.submitter().register_probe(&mut probe).unwrap();
        assert!(probe.is_supported(opcode::AsyncCancel::CODE));
        ring
    }

    #[test]
    fn startup_rejects_missing_stable_submission_before_admission() {
        let error = validate_ring_capabilities(true, true, false, false, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn startup_rejects_kernel_polling_issuers_before_admission() {
        for (sqpoll, iopoll) in [(true, false), (false, true), (true, true)] {
            let error = validate_ring_capabilities(true, true, true, sqpoll, iopoll).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
        validate_ring_capabilities(true, true, true, false, false).unwrap();
    }

    #[test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    fn kernel_startup_accepts_supported_ring_and_closes_without_owners() {
        let (file, _peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let (sender, status) = IoUringConnection::start_ring_thread(file).unwrap();
        drop(sender);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while weak_file.upgrade().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "startup ring retained its owner"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(status.error().is_none());
    }

    fn socket_file() -> (Arc<File>, std::os::unix::net::UnixStream) {
        let (socket, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd: std::os::fd::OwnedFd = socket.into();
        (Arc::new(File::from(fd)), peer)
    }

    fn read_request(
        drops: &Arc<AtomicUsize>,
    ) -> (
        ReadRequest,
        oneshot::Receiver<CompleteIoResult<(Vec<u8>, Box<dyn OwnedReadBuffer>), usize>>,
    ) {
        let (reply, receiver) = oneshot::channel();
        (
            ReadRequest {
                header_buf: vec![0; 40],
                data_buf: Box::new(buffer(drops)),
                reply,
            },
            receiver,
        )
    }

    fn drain_kernel(driver: &mut RingDriver, force: bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !driver.all_completed() {
            driver.complete_cqes().unwrap();
            if driver.all_completed() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "kernel CQE drain exceeded deadline"
            );
            driver.schedule_cancels(force).unwrap();
            driver.wait_once().unwrap();
        }
        assert!(driver.pending.is_empty());
        assert!(driver.cancel_pending.is_empty());
        assert!(driver.cancel_requested.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_missing_target_cqe_reclaims_only_after_exact_sync_quiescence() {
        use std::io::Write;
        let (file, mut peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        driver.ring.submit().unwrap();
        drop(response);
        peer.write_all(&[0x71; 104]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut lost = false;
        while !lost {
            // Reap a real kernel CQE while simulating the consumer/overflow
            // loss of its original result. No synthetic CQE is a release proof.
            lost = driver.ring.completion().any(|cqe| cqe.user_data() == 1);
            if !lost {
                assert!(
                    std::time::Instant::now() < deadline,
                    "real target never completed"
                );
                driver.wait_once().unwrap();
            }
        }
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(driver.pending.len(), 1);
        driver.reclaim_after_sync_quiescence(deadline).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(driver.pending.is_empty());
        assert!(
            driver.status.error().is_some(),
            "lost result must fail the mount"
        );
        drop(driver);
        assert!(weak_file.upgrade().is_none());
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_unsubmitted_target_cannot_use_absent_tag_as_owner_release_proof() {
        let (file, _peer) = socket_file();
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        drop(response);
        let error = driver
            .reclaim_after_sync_quiescence(std::time::Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(driver.pending.len(), 1);
        driver.ring.submit().unwrap();
        drain_kernel(&mut driver, true);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_exact_sync_quiescence_cancels_live_read_and_releases_owner() {
        let (file, _peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        driver.ring.submit().unwrap();
        drop(response);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        driver
            .reclaim_after_sync_quiescence(std::time::Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(driver.pending.is_empty());
        drop(driver);
        assert!(weak_file.upgrade().is_none());
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_expired_quiescence_attempt_retains_published_owner() {
        let (file, _peer) = socket_file();
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        driver.ring.submit().unwrap();
        drop(response);
        let error = driver
            .reclaim_after_sync_quiescence(std::time::Instant::now())
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ETIME));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(driver.pending.len(), 1);
        drain_kernel(&mut driver, true);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "requires Linux kernel io_uring; baseline red run requires an external timeout"]
    fn kernel_lost_target_cqe_production_recovery_finishes_and_reclaims_owner() {
        use std::io::Write;
        let (file, mut peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        driver.ring.submit().unwrap();
        drop(response);
        peer.write_all(&[0x73; 104]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if driver.ring.completion().any(|cqe| cqe.user_data() == 1) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real target never completed"
            );
            driver.wait_once().unwrap();
        }
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(driver.pending.len(), 1);
        driver.fail(io::Error::from_raw_os_error(libc::EIO));
        let recovery_started = std::time::Instant::now();
        driver.recover();
        assert!(recovery_started.elapsed() < Duration::from_secs(2));
        assert!(driver.pending.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(driver);
        assert!(weak_file.upgrade().is_none());
    }

    /// Real target CQEs prove reclamation; no peer bytes are supplied.
    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_notify_abort_cancel_and_reclaim_header_data_file() {
        for abort in [false, true] {
            let (connection, mut rx) = fake_connection();
            let drops = Arc::new(AtomicUsize::new(0));
            let data = buffer(&drops);
            let caller = connection.clone();
            let task = tokio::spawn(async move { caller.read_vectored(vec![0; 40], data).await });
            let req = request(&mut rx).await;
            let (file, _peer) = socket_file();
            let weak_file = Arc::downgrade(&file);
            let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
            driver.accept(RingRequest::Read(req)).unwrap();
            driver.ring.submit().unwrap();
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                connection.unmount_notify.notify();
                assert!(task.await.unwrap().is_none());
            }
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert!(weak_file.upgrade().is_some());
            drain_kernel(&mut driver, false);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            drop(driver);
            assert!(weak_file.upgrade().is_none());
        }
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_normal_read_returns_valid_header_and_inline_data() {
        use std::io::Write;
        let (file, mut peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.accept(RingRequest::Read(req)).unwrap();
        driver.ring.submit().unwrap();
        let mut wire = vec![0x71; 40];
        wire.extend_from_slice(&[0x72; 64]);
        peer.write_all(&wire).unwrap();
        drain_kernel(&mut driver, false);
        let ((header, data), result) = response.await.unwrap();
        assert_eq!(result.unwrap(), 104);
        assert_eq!(header, vec![0x71; 40]);
        let data = recover_read_buffer::<InlineBuffer>(data);
        assert_eq!(&*data, &[0x72; 64]);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(data);
        drop(driver);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(weak_file.upgrade().is_none());
    }

    #[test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    fn kernel_production_loop_disconnect_drains_and_thread_exits() {
        let (file, _peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let (tx, mut rx) = mpsc::channel(2);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        assert!(tx.try_send(RingRequest::Read(req)).is_ok());
        let thread = thread::spawn(move || {
            let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
            driver.accept(rx.blocking_recv().unwrap()).unwrap();
            driver.wait_once().unwrap();
            ready_tx.send(()).unwrap();
            let result = driver.run(&mut rx);
            drop(driver);
            done_tx.send(result).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(response);
        drop(tx);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(weak_file.upgrade().is_none());
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_injected_eintr_retries_eio_fails_and_recovers_every_owner() {
        let (file, _peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let (tx, mut rx) = mpsc::channel(64);
        let status = Arc::new(RingStatus::default());
        let connection = Arc::new(FuseConnection {
            unmount_notify: Arc::new(Notify::new()),
            inner: IoUringConnection {
                tx,
                fd: file.as_raw_fd(),
                file: file.clone(),
                status: status.clone(),
            },
        });
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let mut driver = RingDriver::new(kernel_ring(), file, status);
            // EINTR before entry; real submission and bounded wait; unexpected
            // EIO while the original socket read is still outstanding.
            driver.enter_faults.extend([libc::EINTR, 0, libc::EIO]);
            let result = driver.run(&mut rx);
            drop(driver);
            done_tx.send(result).unwrap();
        });
        let drops = Arc::new(AtomicUsize::new(0));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connection.read_vectored(vec![0; 40], buffer(&drops)),
        )
        .await
        .unwrap();
        assert!(result.is_none());
        assert_eq!(
            connection.ring_failure().unwrap().raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
        thread.join().unwrap();
        drop(connection);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(weak_file.upgrade().is_none());
    }

    #[tokio::test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    async fn kernel_write_burst_above_sq_capacity_completes_and_reclaims_thread() {
        use std::io::Read;
        const REQUESTS: usize = 128;
        let (file, mut peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let (tx, mut rx) = mpsc::channel(REQUESTS);
        let mut replies = Vec::new();
        for _ in 0..REQUESTS {
            let (reply, response) = oneshot::channel();
            assert!(tx
                .try_send(RingRequest::Write(WriteRequest {
                    data: Bytes::from(vec![0x81; 32]),
                    body_extend: Some(Bytes::from(vec![0x82; 32])),
                    reply,
                }))
                .is_ok());
            replies.push(response);
        }
        let reader = thread::spawn(move || {
            let mut bytes = vec![0; REQUESTS * 64];
            peer.read_exact(&mut bytes).unwrap();
            bytes
        });
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
            let result = driver.run(&mut rx);
            drop(driver);
            done_tx.send(result).unwrap();
        });
        for response in replies {
            let (_, result) = tokio::time::timeout(Duration::from_secs(2), response)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap(), 64);
        }
        drop(tx);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        let bytes = reader.join().unwrap();
        for chunk in bytes.chunks_exact(64) {
            assert_eq!(&chunk[..32], &[0x81; 32]);
            assert_eq!(&chunk[32..], &[0x82; 32]);
        }
        assert!(weak_file.upgrade().is_none());
    }

    /// This tests state transitions only. No synthetic CQE frees a published
    /// pointer: the fake pending request is never added to the kernel SQ.
    #[test]
    #[ignore = "requires io_uring construction; state machine only, not kernel lifetime proof"]
    fn cancel_cqe_orders_races_transient_errors_keep_target_owner_until_target() {
        for cancel_first in [false, true] {
            for cancel_result in [
                0,
                -libc::ENOENT,
                -libc::EALREADY,
                -libc::EAGAIN,
                -libc::ENOMEM,
                -libc::EINVAL,
            ] {
                let (file, _peer) = socket_file();
                let drops = Arc::new(AtomicUsize::new(0));
                let (req, response) = read_request(&drops);
                drop(response);
                let mut driver =
                    RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
                driver.pending.insert(
                    1,
                    PendingIo::Read(InflightRead {
                        req,
                        _iovecs: Box::new(
                            [libc::iovec {
                                iov_base: std::ptr::null_mut(),
                                iov_len: 0,
                            }; 2],
                        ),
                    }),
                );
                driver.cancel_pending.insert(1);
                driver.cancel_requested.insert(1);
                if cancel_first {
                    let result = driver.complete_one(CANCEL_TAG | 1, cancel_result);
                    assert_eq!(result.is_err(), cancel_result == -libc::EINVAL);
                    assert_eq!(drops.load(Ordering::SeqCst), 0);
                    driver.complete_one(1, -libc::ECANCELED).unwrap();
                } else {
                    driver.complete_one(1, -libc::ECANCELED).unwrap();
                    driver.complete_one(CANCEL_TAG | 1, cancel_result).unwrap();
                }
                assert_eq!(drops.load(Ordering::SeqCst), 1);
                assert!(driver.all_completed());
                assert!(driver.cancel_requested.is_empty());
            }
        }
    }

    #[test]
    #[ignore = "requires Linux kernel io_uring; run explicitly before acceptance"]
    fn kernel_unwind_drop_cancels_drains_and_reclaims_original_owner() {
        let (file, _peer) = socket_file();
        let weak_file = Arc::downgrade(&file);
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
            driver.accept(RingRequest::Read(req)).unwrap();
            driver.ring.submit().unwrap();
            drop(response);
            panic!("injected ring unwind after publication");
        }));
        assert!(unwind.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(weak_file.upgrade().is_none());
    }

    #[tokio::test]
    #[ignore = "requires io_uring construction; no published pointers in this test"]
    async fn failed_driver_reclaims_queued_unpublished_requests() {
        let (file, _peer) = socket_file();
        let (tx, mut rx) = mpsc::channel(128);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut responses = Vec::new();
        for _ in 0..128 {
            let (req, response) = read_request(&drops);
            assert!(tx.try_send(RingRequest::Read(req)).is_ok());
            responses.push(response);
        }
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        driver.fail(io::Error::from_raw_os_error(libc::EIO));
        assert_eq!(
            driver.run(&mut rx).unwrap_err().raw_os_error(),
            Some(libc::EIO)
        );
        for response in responses {
            let ((_, data), result) = response.await.unwrap();
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
            drop(data);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 128);
        assert!(driver.all_completed());
    }

    #[tokio::test]
    #[ignore = "requires io_uring construction; only harmless NOP SQEs published"]
    async fn sq_full_rejects_unpublished_owner_without_dangling_pointer() {
        let (file, _peer) = socket_file();
        let mut driver = RingDriver::new(kernel_ring(), file, Arc::new(RingStatus::default()));
        for _ in 0..RING_SIZE {
            // SAFETY: NOP contains no pointers or allocations.
            unsafe { driver.ring.submission().push(&opcode::Nop::new().build()) }.unwrap();
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let (req, response) = read_request(&drops);
        assert!(driver.accept(RingRequest::Read(req)).is_err());
        assert!(driver.pending.is_empty());
        let ((_, data), result) = response.await.unwrap();
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(data);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[cfg(all(test, any(feature = "tokio-runtime", feature = "io-uring-runtime")))]
#[path = "native_packet_lifetime_tests.rs"]
mod packet_lifetime_tests;
