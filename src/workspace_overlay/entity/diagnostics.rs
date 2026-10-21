//! Opt-in cumulative timings for the entity experiment, never acceptance data.
//!
//! Enable before process startup with `BREWFS_ENTITY_DIAGNOSTICS=1`. Timers
//! overlap across stages and concurrent operations; their sum is not occupancy.
//! Names are static program labels, and no keys, paths or payloads are recorded.

use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use asyncfuse::raw::diagnostics as fuse_diagnostics;

const MAX_METRICS: usize = 512;
static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("BREWFS_ENTITY_DIAGNOSTICS").is_ok_and(|value| value == "1"));
static METRICS: [Metric; MAX_METRICS] = [const { Metric::new() }; MAX_METRICS];
static METRIC_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTRATION: Mutex<()> = Mutex::new(());
static DROPPED_REGISTRATIONS: AtomicU64 = AtomicU64::new(0);
const MAX_TRACE_EVENTS: usize = 1_048_576;
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static DROPPED_TRACE_EVENTS: AtomicU64 = AtomicU64::new(0);

tokio::task_local! {
    static OPERATION: &'static str;
    static INODE: u64;
}

#[derive(serde::Serialize)]
struct TraceEvent {
    name: &'static str,
    operation: Option<&'static str>,
    unique: u64,
    nodeid: u64,
    opcode: u32,
    pid: u32,
    inode: Option<u64>,
    start_ns: u64,
    end_ns: u64,
    cpu_ns: u64,
}

fn monotonic_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC matches Python time.monotonic_ns and the FUSE trace.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    if result == 0 {
        (time.tv_sec as u64).saturating_mul(1_000_000_000) + time.tv_nsec as u64
    } else {
        0
    }
}

fn start_trace(name: &'static str) -> TraceEvent {
    let context = fuse_diagnostics::current_context();
    TraceEvent {
        name,
        operation: OPERATION.try_with(|operation| *operation).ok(),
        unique: context.map_or(0, |context| context.unique),
        nodeid: context.map_or(0, |context| context.nodeid),
        opcode: context.map_or(0, |context| context.opcode),
        pid: context.map_or(0, |context| context.pid),
        inode: INODE.try_with(|inode| *inode).ok(),
        start_ns: monotonic_ns(),
        end_ns: 0,
        cpu_ns: 0,
    }
}

fn record_trace(event: TraceEvent) {
    if let Ok(mut events) = TRACE_EVENTS.lock()
        && events.len() < MAX_TRACE_EVENTS
    {
        events.push(event);
    } else {
        DROPPED_TRACE_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

fn thread_cpu_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    if result == 0 {
        (time.tv_sec as u64).saturating_mul(1_000_000_000) + time.tv_nsec as u64
    } else {
        0
    }
}

async fn measured_task<F: Future>(future: F) -> F::Output {
    if !enabled() {
        return future.await;
    }
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|context| {
        let mut event = start_trace("client.task_poll");
        let cpu_start = thread_cpu_ns();
        let result = future.as_mut().poll(context);
        event.cpu_ns = thread_cpu_ns().saturating_sub(cpu_start);
        event.end_ns = monotonic_ns();
        record_trace(event);
        result
    })
    .await
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    name: &'static str,
    operation: Option<&'static str>,
}

struct Metric {
    key: OnceLock<Key>,
    count: AtomicU64,
    elapsed_ns: AtomicU64,
    round_trips: AtomicU64,
    rows: AtomicU64,
}

impl Metric {
    const fn new() -> Self {
        Self {
            key: OnceLock::new(),
            count: AtomicU64::new(0),
            elapsed_ns: AtomicU64::new(0),
            round_trips: AtomicU64::new(0),
            rows: AtomicU64::new(0),
        }
    }

    fn record(&self, elapsed_ns: u64, round_trips: u64, rows: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.elapsed_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.round_trips.fetch_add(round_trips, Ordering::Relaxed);
        self.rows.fetch_add(rows, Ordering::Relaxed);
    }
}

pub fn enabled() -> bool {
    *ENABLED
}

fn metric(key: Key) -> Option<&'static Metric> {
    let find = || {
        METRICS[..METRIC_COUNT.load(Ordering::Acquire)]
            .iter()
            .find(|metric| metric.key.get() == Some(&key))
    };
    if let Some(metric) = find() {
        return Some(metric);
    }
    // Only first registration takes a lock. Completed samples use atomics.
    let Ok(_guard) = REGISTRATION.lock() else {
        DROPPED_REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    if let Some(metric) = find() {
        return Some(metric);
    }
    let index = METRIC_COUNT.load(Ordering::Relaxed);
    if index == MAX_METRICS {
        DROPPED_REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let metric = &METRICS[index];
    if metric.key.set(key).is_err() {
        DROPPED_REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    METRIC_COUNT.store(index + 1, Ordering::Release);
    Some(metric)
}

fn metrics(name: &'static str) -> [Option<&'static Metric>; 2] {
    let aggregate = metric(Key {
        name,
        operation: None,
    });
    let scoped = OPERATION
        .try_with(|operation| *operation)
        .ok()
        .and_then(|operation| {
            metric(Key {
                name,
                operation: Some(operation),
            })
        });
    [aggregate, scoped]
}

struct ActiveSpan {
    metrics: [Option<&'static Metric>; 2],
    start: Instant,
    round_trips: u64,
    rows: u64,
    trace: TraceEvent,
}

/// An elapsed-time sample, recorded even on an early return or cancellation.
pub struct Span {
    active: Option<ActiveSpan>,
}

impl Span {
    pub fn start(name: &'static str) -> Self {
        Self {
            active: enabled().then(|| ActiveSpan {
                metrics: metrics(name),
                start: Instant::now(),
                round_trips: 0,
                rows: 0,
                trace: start_trace(name),
            }),
        }
    }

    /// Count observed client request/reply attempts, not Lua-internal commands.
    pub fn add_round_trips(&mut self, count: u64) {
        if let Some(active) = self.active.as_mut() {
            active.round_trips = active.round_trips.saturating_add(count);
        }
    }

    /// Rows requested/validated/written, with the interpretation in each label.
    pub fn add_rows(&mut self, count: u64) {
        if let Some(active) = self.active.as_mut() {
            active.rows = active.rows.saturating_add(count);
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(mut active) = self.active.take() {
            let elapsed_ns = active.start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            for metric in active.metrics.iter().flatten() {
                metric.record(elapsed_ns, active.round_trips, active.rows);
            }
            active.trace.end_ns = monotonic_ns();
            record_trace(active.trace);
        }
    }
}

pub fn record(name: &'static str, elapsed_ns: u64, round_trips: u64, rows: u64) {
    if enabled() {
        for metric in metrics(name).into_iter().flatten() {
            metric.record(elapsed_ns, round_trips, rows);
        }
    }
}

/// Attach an operation label to nested spans in this future's task context.
pub async fn with_operation<F: Future>(operation: &'static str, future: F) -> F::Output {
    if enabled() {
        OPERATION.scope(operation, future).await
    } else {
        future.await
    }
}

/// Diagnostic correlation only; does not retain inode state or affect I/O.
pub async fn with_inode<F: Future>(inode: u64, future: F) -> F::Output {
    if enabled() {
        INODE.scope(inode, future).await
    } else {
        future.await
    }
}

/// Capture the current label before spawning: `tokio::spawn(inherit(future))`.
/// Ordinary task locals are not inherited by newly spawned Tokio tasks.
pub fn inherit<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let operation = enabled()
        .then(|| OPERATION.try_with(|operation| *operation).ok())
        .flatten();
    let inode = enabled()
        .then(|| INODE.try_with(|inode| *inode).ok())
        .flatten();
    let context = enabled().then(fuse_diagnostics::current_context).flatten();
    async move {
        let scoped = async move {
            let scoped_inode = async move {
                if let Some(inode) = inode {
                    INODE.scope(inode, measured_task(future)).await
                } else {
                    measured_task(future).await
                }
            };
            if let Some(operation) = operation {
                OPERATION.scope(operation, scoped_inode).await
            } else {
                scoped_inode.await
            }
        };
        if let Some(context) = context {
            fuse_diagnostics::with_context(context, scoped).await
        } else {
            scoped.await
        }
    }
}

/// Snapshot after local drain for consistent totals. Aggregate rows and rows
/// grouped by operation describe the same samples and must not be added.
pub fn snapshot_json() -> serde_json::Value {
    let mut samples = Vec::new();
    for metric in &METRICS[..METRIC_COUNT.load(Ordering::Acquire)] {
        let Some(key) = metric.key.get() else {
            continue;
        };
        samples.push(serde_json::json!({
            "name": key.name,
            "operation": key.operation,
            "count": metric.count.load(Ordering::Relaxed),
            "elapsed_ns": metric.elapsed_ns.load(Ordering::Relaxed),
            "round_trips": metric.round_trips.load(Ordering::Relaxed),
            "rows": metric.rows.load(Ordering::Relaxed),
        }));
    }
    let trace = TRACE_EVENTS
        .lock()
        .map(|mut events| std::mem::take(&mut *events))
        .unwrap_or_default();
    let fuse_trace: Vec<_> = fuse_diagnostics::drain_events()
        .into_iter()
        .map(|event| {
            serde_json::json!({
                "stage": event.stage, "start_ns": event.start_ns, "end_ns": event.end_ns,
                "cpu_ns": event.cpu_ns, "worker": event.worker,
                "unique": event.context.map_or(0, |context| context.unique),
                "nodeid": event.context.map_or(0, |context| context.nodeid),
                "opcode": event.context.map_or(0, |context| context.opcode),
                "pid": event.context.map_or(0, |context| context.pid),
            })
        })
        .collect();
    serde_json::json!({
        "schema": "brewfs-entity-diagnostics-v1",
        "enabled": enabled(),
        "samples": samples,
        "dropped_metric_registrations": DROPPED_REGISTRATIONS.load(Ordering::Relaxed),
        "trace_events": trace,
        "fuse_trace_events": fuse_trace,
        "dropped_trace_events": DROPPED_TRACE_EVENTS.load(Ordering::Relaxed),
        "dropped_fuse_trace_events": fuse_diagnostics::dropped_events(),
        "notes": [
            "Cumulative concurrent/nested timings overlap and are not exclusive occupancy.",
            "operation=null is the aggregate; operation-specific rows must not be added to it.",
            "Redis RTT includes client queue/network/server work; server time comes from Redis commandstats.",
            "Redis request counts include script cache misses/load/retry, but exclude connection-manager internal handshake/reconnect traffic.",
            "The packed owner validator uses a separate catalog connection, outside entity Redis request counts."
            ,"Trace intervals use CLOCK_MONOTONIC nanoseconds and drain at each snapshot; intervals overlap and require union/clipping before wall-time accounting."
        ],
    })
}
