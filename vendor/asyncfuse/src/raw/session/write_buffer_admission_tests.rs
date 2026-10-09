//! Controlled input reservation denial, no physical mount or kernel IO.
use super::*;
use crate::raw::reply::{ReplyInit, ReplyMemoryGuard};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug)]
struct Charge {
    used: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct ProbeFs {
    deny_input: AtomicBool,
    input_attempts: AtomicUsize,
    roots: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}
impl Filesystem for Arc<ProbeFs> {
    async fn init(&self, _: Request) -> crate::Result<ReplyInit> {
        Ok(ReplyInit::default())
    }
    async fn destroy(&self, _: Request) {}
    fn reserve_input_buffer_memory(&self, bytes: u64) -> crate::Result<Option<ReplyMemoryGuard>> {
        self.input_attempts.fetch_add(1, Ordering::AcqRel);
        if self.deny_input.load(Ordering::Acquire) {
            return Err(libc::ENOMEM.into());
        }
        let bytes = usize::try_from(bytes).map_err(|_| crate::Errno::from(libc::ENOMEM))?;
        self.roots.fetch_add(bytes, Ordering::AcqRel);
        Ok(Some(Arc::new(Charge {
            used: self.roots.clone(),
            bytes,
        })))
    }
    fn reserve_request_memory(&self, bytes: u64) -> crate::Result<Option<ReplyMemoryGuard>> {
        let bytes = usize::try_from(bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(8192))
            .ok_or_else(|| crate::Errno::from(libc::ENOMEM))?;
        self.requests.fetch_add(bytes, Ordering::AcqRel);
        Ok(Some(Arc::new(Charge {
            used: self.requests.clone(),
            bytes,
        })))
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

fn request(unique: u64) -> Request {
    Request {
        unique,
        uid: 0,
        gid: 0,
        pid: 0,
    }
}

fn pool(fs: &Arc<ProbeFs>) -> Arc<BufferPool> {
    let fs = fs.clone();
    let allocator: crate::raw::buffer_pool::InputMemoryAllocator = Arc::new(move |bytes| {
        fs.reserve_input_buffer_memory(bytes + 512)
            .map_err(IoError::from)
    });
    Arc::new(BufferPool::with_capacity_owned(
        128,
        16,
        Some(allocator),
        None,
    ))
}

async fn assert_error_reply(receiver: &mut UnboundedReceiver<FuseData>, unique: u64, errno: i32) {
    let packet = tokio::time::timeout(Duration::from_millis(250), receiver.next())
        .await
        .expect("WRITE allocation refusal must enqueue an errno reply")
        .expect("session response channel must remain open");
    let (header, error, actual_unique) = match &packet {
        Either::Left(data) => reply_tracker::wire_header(data, None).unwrap(),
        Either::Right((data, body)) => reply_tracker::wire_header(data, Some(body)).unwrap(),
    };
    assert_eq!(header, 16);
    assert_eq!(error, -errno);
    assert_eq!(actual_unique, unique);
}

#[tokio::test(flavor = "current_thread")]
async fn pooled_write_roots_denial_copies_only_admitted_bytes_and_retains_owner() {
    let fs = Arc::new(ProbeFs::default());
    let mut session = Session::<Arc<ProbeFs>>::new(MountOptions::default());
    let mut replies = session.response_receivers[0].take().unwrap();
    let pool = pool(&fs);
    let original_owner = fs.reserve_input_buffer_memory(128).unwrap();
    let mut input = AlignedBuffer::try_new_owned(128, original_owner).unwrap();
    input[..97].fill(0x73);
    let request_owner = fs.reserve_request_memory(97).unwrap();
    fs.deny_input.store(true, Ordering::Release);
    let body = session
        .acquire_pooled_write_body(request(41), &fs, &pool, &mut input, 97, &request_owner)
        .await
        .unwrap()
        .expect("admitted tiny WRITE must survive full-size Roots denial");
    assert_eq!(fs.input_attempts.load(Ordering::Acquire), 2);
    assert_eq!(body.len(), 97);
    assert_eq!(body.as_ref(), &[0x73; 97]);
    assert_eq!(fs.roots.load(Ordering::Acquire), 128);
    assert_eq!(fs.requests.load(Ordering::Acquire), 97 + 8192);

    // Model the actual handler's payload slice surviving WorkItem/reply owners.
    let payload = body.slice(40..);
    drop((body, request_owner));
    input[..97].fill(0x99);
    assert_eq!(payload.as_ref(), &[0x73; 57]);
    assert_eq!(fs.requests.load(Ordering::Acquire), 97 + 8192);
    drop(payload);
    assert_eq!(fs.requests.load(Ordering::Acquire), 0);

    // The response channel/session remains available after fallback admission.
    session
        .reject_ordinary_request(request(42), libc::EINVAL.into(), &fs)
        .await
        .unwrap();
    assert_error_reply(&mut replies, 42, libc::EINVAL).await;
    drop((input, pool));
    assert_eq!(fs.roots.load(Ordering::Acquire), 0);
}
