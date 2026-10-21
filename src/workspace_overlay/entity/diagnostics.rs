//! Opt-in cumulative timings for the entity experiment, never acceptance data.
//!
//! Enable before process startup with `BREWFS_ENTITY_DIAGNOSTICS=1`. Timers
//! overlap across stages and concurrent operations; their sum is not occupancy.
//! Names are static program labels, and no keys, paths or payloads are recorded.

use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

const MAX_METRICS: usize = 512;
static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("BREWFS_ENTITY_DIAGNOSTICS").is_ok_and(|value| value == "1"));
static METRICS: [Metric; MAX_METRICS] = [const { Metric::new() }; MAX_METRICS];
static METRIC_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTRATION: Mutex<()> = Mutex::new(());
static DROPPED_REGISTRATIONS: AtomicU64 = AtomicU64::new(0);

tokio::task_local! {
    static OPERATION: &'static str;
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
        if let Some(active) = self.active.as_ref() {
            let elapsed_ns = active.start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            for metric in active.metrics.iter().flatten() {
                metric.record(elapsed_ns, active.round_trips, active.rows);
            }
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

/// Capture the current label before spawning: `tokio::spawn(inherit(future))`.
/// Ordinary task locals are not inherited by newly spawned Tokio tasks.
pub fn inherit<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let operation = enabled()
        .then(|| OPERATION.try_with(|operation| *operation).ok())
        .flatten();
    async move {
        if let Some(operation) = operation {
            OPERATION.scope(operation, future).await
        } else {
            future.await
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
    serde_json::json!({
        "schema": "brewfs-entity-diagnostics-v1",
        "enabled": enabled(),
        "samples": samples,
        "dropped_metric_registrations": DROPPED_REGISTRATIONS.load(Ordering::Relaxed),
        "notes": [
            "Cumulative concurrent/nested timings overlap and are not exclusive occupancy.",
            "operation=null is the aggregate; operation-specific rows must not be added to it.",
            "Redis RTT includes client queue/network/server work; server time comes from Redis commandstats.",
            "Redis request counts include script cache misses/load/retry, but exclude connection-manager internal handshake/reconnect traffic.",
            "The packed owner validator uses a separate catalog connection, outside entity Redis request counts."
        ],
    })
}
