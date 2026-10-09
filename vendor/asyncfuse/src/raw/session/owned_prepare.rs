//! Dynamic child/holder admission without a newly allocated grant Arc.
use crate::raw::filesystem::Filesystem;
use crate::raw::reply::InlineRootPermit;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

struct PrepareAllocation<F: Future> {
    future: Option<Pin<Box<F>>>,
    memory: InlineRootPermit,
}

// Field order is also correct during unwinding: the actual holder Box and its
// child Box finish their Drop/deallocation glue before the stack-only permit.
struct RetiringPrepare<F: Future> {
    _allocation: Box<PrepareAllocation<F>>,
    _roots: InlineRootPermit,
}

pub(super) struct OwnedPrepare<F: Future> {
    allocation: Option<Box<PrepareAllocation<F>>>,
}
impl<F: Future> OwnedPrepare<F> {
    pub(super) fn new<FS: Filesystem>(fs: &FS, future: F) -> io::Result<Self> {
        let child = std::alloc::Layout::for_value(&future);
        let holder = std::alloc::Layout::new::<PrepareAllocation<F>>();
        // The outer handle lives in the independently admitted fixed512 outer
        // preparation Box; dynamic admission covers exactly these two heaps.
        let bytes = child
            .size()
            .checked_add(holder.size())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOMEM))?;
        tracing::debug!(
            event = "readonly_prepare_child_layout",
            child_bytes = child.size(),
            child_align = child.align(),
            holder_bytes = holder.size(),
            holder_align = holder.align(),
            outer_handle_bytes = std::mem::size_of::<Self>(),
            bytes
        );
        let memory = fs
            .reserve_inline_prepare_memory(bytes)
            .map_err(io::Error::from)?
            .unwrap_or_else(InlineRootPermit::empty);
        Ok(Self {
            allocation: Some(Box::new(PrepareAllocation {
                future: Some(Box::pin(future)),
                memory,
            })),
        })
    }
}
impl<F: Future> Future for OwnedPrepare<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .allocation
            .as_mut()
            .expect("live preparation allocation")
            .future
            .as_mut()
            .expect("live preparation child")
            .as_mut()
            .poll(cx)
    }
}
impl<F: Future> Drop for OwnedPrepare<F> {
    fn drop(&mut self) {
        let Some(mut allocation) = self.allocation.take() else {
            return;
        };
        let roots = std::mem::replace(&mut allocation.memory, InlineRootPermit::empty());
        // No F is moved out of its Pin<Box>; only the unpinned permit moves.
        drop(RetiringPrepare {
            _allocation: allocation,
            _roots: roots,
        });
    }
}

/// The complete FS/failure race is the one child boxed by production.
pub(super) fn preparation_child<FS: Filesystem + Send + Sync + 'static>(
    fs: std::sync::Arc<FS>,
    tracker: std::sync::Arc<super::reply_tracker::ReplyTracker>,
) -> impl Future<Output = io::Result<()>> + Send + 'static {
    async move {
        // Owning this async FS child delays even constructing FS preparation
        // until the admitted complete race is first polled.
        let child = async move { fs.prepare_unmount().await.map_err(io::Error::from) };
        let mut child = std::pin::pin!(child);
        let mut failure = std::pin::pin!(tracker.wait_failure());
        // Register the actual failure waiter before polling FS. A
        // later reply error wakes this same stored preparation future.
        futures_util::future::poll_fn(|cx| {
            if let Poll::Ready(error) = failure.as_mut().poll(cx) {
                return Poll::Ready(Err(error));
            }
            child.as_mut().poll(cx)
        })
        .await
    }
}

/// This is the concrete outer preparation boxed/stored by the production factory.
pub(super) fn preparation_future<F>(
    child: OwnedPrepare<F>,
    tracker: std::sync::Arc<super::reply_tracker::ReplyTracker>,
) -> impl Future<Output = io::Result<()>> + Send + 'static
where
    F: Future<Output = io::Result<()>> + Send + 'static,
{
    async move {
        child.await?;
        tracker.drain().await
    }
}

/// Type inference measures those exact production factories, without values.
/// Tuple order: complete race child, holder, outer; each is (size, alignment).
pub(super) fn preparation_future_layout<FS: Filesystem + Send + Sync + 'static>(
) -> ((usize, usize), (usize, usize), (usize, usize)) {
    fn inferred<FS, F, O>(
        _: fn(std::sync::Arc<FS>, std::sync::Arc<super::reply_tracker::ReplyTracker>) -> F,
        _: fn(OwnedPrepare<F>, std::sync::Arc<super::reply_tracker::ReplyTracker>) -> O,
    ) -> ((usize, usize), (usize, usize), (usize, usize))
    where
        FS: Filesystem + Send + Sync + 'static,
        F: Future<Output = io::Result<()>> + Send + 'static,
        O: Future<Output = io::Result<()>> + Send + 'static,
    {
        (
            (std::mem::size_of::<F>(), std::mem::align_of::<F>()),
            (
                std::mem::size_of::<PrepareAllocation<F>>(),
                std::mem::align_of::<PrepareAllocation<F>>(),
            ),
            (std::mem::size_of::<O>(), std::mem::align_of::<O>()),
        )
    }
    // Neither function pointer is invoked: no FS object, tracker, Box, or poll.
    inferred(preparation_child::<FS>, preparation_future)
}

#[cfg(all(test, any(feature = "tokio-runtime", feature = "io-uring-runtime")))]
#[path = "prepare_inline_lifetime_tests.rs"]
mod allocation_tests;
