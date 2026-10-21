//! Opt-in request timing shared by all filesystem implementations.
//!
//! `BREWFS_ENTITY_DIAGNOSTICS=1` enables a bounded, drainable event buffer.
//! Times use CLOCK_MONOTONIC so a workload process can correlate syscall
//! intervals. Handler CPU is measured around individual future polls, never
//! across an await. Nested intervals and CPU are separate accounting views.
//! Kernel queueing before a completed /dev/fuse read is not measured here.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::{Mutex, OnceLock};

const MAX_EVENTS: usize = 524_288;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub unique: u64,
    pub nodeid: u64,
    pub opcode: u32,
    /// FUSE header pid, normally the workload thread ID on Linux.
    pub pid: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// None for mount-wide admission waits before the request is read.
    pub context: Option<Context>,
    pub stage: &'static str,
    pub start_ns: u64,
    pub end_ns: u64,
    /// CPU consumed by polling this handler, not elapsed callback time.
    pub cpu_ns: u64,
    pub worker: Option<usize>,
}

#[derive(Default)]
struct Buffer {
    events: Vec<Event>,
    replies: BTreeMap<u64, (Context, u64)>,
    dropped: u64,
}

static ENABLED: OnceLock<bool> = OnceLock::new();
static BUFFER: OnceLock<Mutex<Buffer>> = OnceLock::new();
thread_local! {
    static CONTEXT: Cell<Option<Context>> = const { Cell::new(None) };
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var("BREWFS_ENTITY_DIAGNOSTICS").as_deref() == Ok("1"))
}

fn buffer() -> &'static Mutex<Buffer> {
    BUFFER.get_or_init(|| Mutex::new(Buffer::default()))
}

fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Both clocks are supported by the Unix platforms supported by asyncfuse.
    if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
        return 0;
    }
    (value.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(value.tv_nsec as u64)
}

pub(crate) fn timestamp() -> Option<u64> {
    enabled().then(|| clock_ns(libc::CLOCK_MONOTONIC))
}

pub fn current_context() -> Option<Context> {
    if !enabled() {
        return None;
    }
    CONTEXT.with(Cell::get)
}

struct ContextGuard(Option<Context>);
impl ContextGuard {
    fn enter(context: Context) -> Self {
        Self(CONTEXT.with(|slot| slot.replace(Some(context))))
    }
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CONTEXT.with(|slot| slot.set(self.0));
    }
}

/// Install context only while this future is polled. Spawned child futures
/// must capture current_context() and call with_context() themselves.
pub async fn with_context<T>(context: Context, future: impl Future<Output = T>) -> T {
    if !enabled() {
        return future.await;
    }
    let mut future = pin!(future);
    poll_fn(|cx| {
        let _context = ContextGuard::enter(context);
        future.as_mut().poll(cx)
    })
    .await
}

fn record(event: Event) {
    let mut buffer = buffer().lock().unwrap_or_else(|error| error.into_inner());
    if buffer.events.len() < MAX_EVENTS {
        buffer.events.push(event);
    } else {
        buffer.dropped = buffer.dropped.saturating_add(1);
    }
}

/// Drain completed intervals. In-flight requests and reply timing stay live.
pub fn drain_events() -> Vec<Event> {
    if !enabled() {
        return Vec::new();
    }
    std::mem::take(
        &mut buffer()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events,
    )
}

/// Cumulative dropped event/reply-registration count; never silently truncate.
pub fn dropped_events() -> u64 {
    if !enabled() {
        return 0;
    }
    buffer()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .dropped
}

pub(crate) fn interval(
    context: Option<Context>,
    stage: &'static str,
    start_ns: u64,
    end_ns: u64,
    worker: Option<usize>,
) {
    record(Event {
        context,
        stage,
        start_ns,
        end_ns,
        cpu_ns: 0,
        worker,
    });
}

pub(crate) struct Span {
    event: Option<Event>,
}
impl Span {
    pub(crate) fn new(
        stage: &'static str,
        context: Option<Context>,
        worker: Option<usize>,
    ) -> Self {
        Self {
            event: timestamp().map(|start_ns| Event {
                context,
                stage,
                start_ns,
                end_ns: start_ns,
                cpu_ns: 0,
                worker,
            }),
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(mut event) = self.event {
            event.end_ns = clock_ns(libc::CLOCK_MONOTONIC);
            record(event);
        }
    }
}

pub(crate) async fn handler<T>(
    context: Context,
    worker: usize,
    scheduled_ns: Option<u64>,
    future: impl Future<Output = T>,
) -> T {
    if !enabled() {
        return future.await;
    }
    let mut span = Span::new("handler", Some(context), Some(worker));
    if let (Some(start_ns), Some(event)) = (scheduled_ns, span.event) {
        let stage = if context.opcode == super::super::abi::fuse_opcode::FUSE_READ as u32 {
            "inline_dispatch"
        } else {
            "spawn_schedule"
        };
        interval(Some(context), stage, start_ns, event.start_ns, Some(worker));
    }
    let mut future = pin!(future);
    poll_fn(|cx| {
        let _context = ContextGuard::enter(context);
        let poll_start_ns = clock_ns(libc::CLOCK_MONOTONIC);
        let start = clock_ns(libc::CLOCK_THREAD_CPUTIME_ID);
        let result = future.as_mut().poll(cx);
        let used = clock_ns(libc::CLOCK_THREAD_CPUTIME_ID).saturating_sub(start);
        let poll_end_ns = clock_ns(libc::CLOCK_MONOTONIC);
        if let Some(event) = &mut span.event {
            event.cpu_ns = event.cpu_ns.saturating_add(used);
        }
        record(Event {
            context: Some(context),
            stage: "handler_poll",
            start_ns: poll_start_ns,
            end_ns: poll_end_ns,
            cpu_ns: used,
            worker: Some(worker),
        });
        result
    })
    .await
}

pub(crate) async fn callback<T>(future: impl Future<Output = T>) -> T {
    let _span = Span::new("callback", current_context(), None);
    future.await
}

pub(crate) trait CallbackFuture: Future + Sized {
    fn diagnostic_callback(self) -> impl Future<Output = Self::Output> {
        callback(self)
    }
}
impl<F: Future> CallbackFuture for F {}

pub(crate) fn reply_enqueued(unique: u64) {
    let Some(context) = current_context().filter(|context| context.unique == unique) else {
        return;
    };
    let start_ns = clock_ns(libc::CLOCK_MONOTONIC);
    let mut buffer = buffer().lock().unwrap_or_else(|error| error.into_inner());
    if buffer.replies.len() < MAX_EVENTS {
        buffer.replies.insert(unique, (context, start_ns));
    } else {
        buffer.dropped = buffer.dropped.saturating_add(1);
    }
}

/// Starts at response dequeue; includes reply preparation, kernel write and
/// retries. Separate reply_queue covers enqueue to dequeue. Neither is CPU.
pub(crate) fn reply_dequeued(data: &crate::raw::FuseData) -> Span {
    if !enabled() {
        return Span { event: None };
    }
    let end_ns = clock_ns(libc::CLOCK_MONOTONIC);
    let header = match data {
        futures_util::future::Either::Left(data) => super::reply_tracker::wire_header(data, None),
        futures_util::future::Either::Right((head, body)) => {
            super::reply_tracker::wire_header(head, Some(body))
        }
    };
    let Ok((_, _, unique)) = header else {
        return Span { event: None };
    };
    let entry = buffer()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .replies
        .remove(&unique);
    let Some((context, start_ns)) = entry else {
        return Span { event: None };
    };
    interval(Some(context), "reply_queue", start_ns, end_ns, None);
    Span {
        event: Some(Event {
            context: Some(context),
            stage: "reply_completion",
            start_ns: end_ns,
            end_ns,
            cpu_ns: 0,
            worker: None,
        }),
    }
}
