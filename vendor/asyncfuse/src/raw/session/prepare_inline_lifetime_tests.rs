//! Actual child/holder allocator boundary proof for inline preparation. UNRUN.
use super::*;
use crate::raw::reply::ReplyInit;
use crate::raw::session::owned_open_queue::tests::allocator;
use crate::raw::Request;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug)]
struct InlineCharge {
    used: Arc<AtomicUsize>,
    observed: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for InlineCharge {
    fn drop(&mut self) {
        self.observed.store(
            allocator::DEALLOCATED.load(Ordering::Acquire),
            Ordering::Release,
        );
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[derive(Default)]
struct PrepareFs {
    used: Arc<AtomicUsize>,
    observed: Arc<AtomicUsize>,
}
impl Filesystem for PrepareFs {
    async fn init(&self, _: Request) -> crate::Result<ReplyInit> {
        Ok(ReplyInit::default())
    }
    async fn destroy(&self, _: Request) {}
    fn reserve_inline_prepare_memory(&self, bytes: u64) -> crate::Result<Option<InlineRootPermit>> {
        let bytes = usize::try_from(bytes).map_err(|_| crate::Errno::from(libc::ENOMEM))?;
        self.used.fetch_add(bytes, Ordering::AcqRel);
        Ok(Some(
            InlineRootPermit::try_new(InlineCharge {
                used: self.used.clone(),
                observed: self.observed.clone(),
                bytes,
            })
            .unwrap(),
        ))
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
struct Child {
    bytes: [u8; 37],
    dropped: Arc<AtomicUsize>,
    panic_on_drop: bool,
}
impl Future for Child {
    type Output = io::Result<()>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        assert_eq!(self.bytes[0], 7);
        Poll::Ready(Ok(()))
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
        if self.panic_on_drop {
            panic!("controlled child destructor panic");
        }
    }
}
fn actual_boxes_before_refund(panic_on_drop: bool) {
    let _serial = allocator::SERIAL.lock().unwrap();
    let fs = PrepareFs::default();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut owned = OwnedPrepare::new(
        &fs,
        Child {
            bytes: [7; 37],
            dropped: dropped.clone(),
            panic_on_drop,
        },
    )
    .unwrap();
    let allocation = owned.allocation.as_ref().unwrap();
    let holder = &**allocation as *const PrepareAllocation<Child> as usize;
    let child = allocation.future.as_ref().unwrap().as_ref().get_ref() as *const Child as usize;
    let exact = std::mem::size_of::<Child>() + std::mem::size_of::<PrepareAllocation<Child>>();
    assert_eq!(
        std::mem::size_of::<OwnedPrepare<Child>>(),
        8,
        "fixed outer owns pointer only"
    );
    assert_eq!(
        fs.used.load(Ordering::Acquire),
        exact,
        "no added heap guard allocation"
    );
    allocator::watch([holder, child, 0, 0]);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(Pin::new(&mut owned).poll(&mut cx).is_ready());
    assert_eq!(
        dropped.load(Ordering::Acquire),
        0,
        "Ready is not Box allocation retirement"
    );
    assert_eq!(fs.used.load(Ordering::Acquire), exact);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(owned)));
    assert_eq!(outcome.is_err(), panic_on_drop);
    assert_eq!(dropped.load(Ordering::Acquire), 1);
    assert_eq!(
        fs.observed.load(Ordering::Acquire),
        0b11,
        "actual child Box AND enclosing holder Box dealloc must precede refund"
    );
    assert_eq!(fs.used.load(Ordering::Acquire), 0);
    allocator::clear();
}
#[test]
fn actual_prepare_child_and_holder_deallocate_before_inline_grant_refund() {
    actual_boxes_before_refund(false);
}
#[test]
fn actual_prepare_holder_deallocation_precedes_refund_even_if_child_drop_unwinds() {
    actual_boxes_before_refund(true);
}
