use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct DiskHealth {
    error_count: AtomicU64,
    success_count: AtomicU64,
    bypassed: AtomicBool,
    probe_inflight: AtomicBool,
    last_error_time: AtomicU64,
    error_threshold: u64,
    recovery_threshold: u64,
    reset_window_secs: u64,
}

/// Releases a half-open probe even when its caller is cancelled or misses.
pub(crate) struct DiskHealthOperation<'a> {
    health: &'a DiskHealth,
    probe: bool,
}

impl Drop for DiskHealthOperation<'_> {
    fn drop(&mut self) {
        if self.probe {
            self.health.probe_inflight.store(false, Ordering::Release);
        }
    }
}

impl Default for DiskHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskHealth {
    #[cfg(test)]
    pub(crate) fn elapse_cooldown_for_test(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_error_time
            .store(now - self.reset_window_secs - 1, Ordering::Relaxed);
    }

    pub fn new() -> Self {
        Self {
            error_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            bypassed: AtomicBool::new(false),
            probe_inflight: AtomicBool::new(false),
            last_error_time: AtomicU64::new(0),
            error_threshold: 3,
            recovery_threshold: 10,
            reset_window_secs: 60,
        }
    }

    pub fn is_bypassed(&self) -> bool {
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let last = self.last_error_time.load(Ordering::Relaxed);
            if last > 0 && now.as_secs().saturating_sub(last) > self.reset_window_secs {
                self.error_count.store(0, Ordering::Relaxed);
            }
        }
        self.bypassed.load(Ordering::Acquire)
    }

    fn cooldown_elapsed(&self) -> bool {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| {
                now.as_secs()
                    .saturating_sub(self.last_error_time.load(Ordering::Relaxed))
                    > self.reset_window_secs
            })
    }

    /// A cheap preflight before reserving write capacity or copying data.
    pub(crate) fn can_attempt(&self) -> bool {
        !self.is_bypassed()
            || (self.cooldown_elapsed() && !self.probe_inflight.load(Ordering::Acquire))
    }

    /// Normal operations proceed freely; after bypass cooldown, admit only one
    /// probe at a time until the existing success threshold restores the tier.
    pub(crate) fn try_operation(&self) -> Option<DiskHealthOperation<'_>> {
        let probe = self.is_bypassed();
        if probe
            && (!self.cooldown_elapsed()
                || self
                    .probe_inflight
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err())
        {
            return None;
        }
        let operation = DiskHealthOperation {
            health: self,
            probe,
        };
        // A previous probe may have failed between preflight and reservation.
        if probe && !self.cooldown_elapsed() {
            return None;
        }
        Some(operation)
    }

    pub fn record_error(&self) {
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            self.last_error_time.store(now.as_secs(), Ordering::Relaxed);
        }
        self.success_count.store(0, Ordering::Relaxed);
        let count = self.error_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.error_threshold {
            // Publish the error timestamp before admitting cooldown probes.
            self.bypassed.store(true, Ordering::Release);
            tracing::warn!(
                errors = count,
                "disk cache entering bypassed mode after repeated I/O errors"
            );
        }
    }

    pub fn record_success(&self) {
        let count = self.success_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.recovery_threshold && self.bypassed.load(Ordering::Acquire) {
            self.bypassed.store(false, Ordering::Release);
            self.error_count.store(0, Ordering::Relaxed);
            self.success_count.store(0, Ordering::Relaxed);
            tracing::info!(successes = count, "disk cache recovered from bypassed mode");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_probes_are_exclusive_and_released_on_drop() {
        let health = DiskHealth::new();
        for _ in 0..3 {
            health.record_error();
        }
        assert!(health.try_operation().is_none());
        health.elapse_cooldown_for_test();
        let probe = health.try_operation().expect("cooldown permits a probe");
        assert!(!health.can_attempt());
        assert!(health.try_operation().is_none());
        drop(probe);
        let probe = health
            .try_operation()
            .expect("cancelled probe releases admission");
        health.record_error();
        drop(probe);
        assert!(health.try_operation().is_none());
    }
}
