use std::sync::atomic::{AtomicU64, Ordering};

pub const SIZE_CLASS_COUNT: usize = 4;

#[derive(Default)]
pub struct PackedRuntimeMetrics {
    data_range_gets: AtomicU64,
    data_range_bytes: AtomicU64,
    logical_bytes: AtomicU64,
    overscan_bytes: AtomicU64,
    frames_decoded: AtomicU64,
    coalesced_ranges: AtomicU64,
    inflight_singleflight: AtomicU64,
    pipeline_current: AtomicU64,
    pipeline_peak: AtomicU64,
    prefetched_logical_bytes: AtomicU64,
    data_cache_hits: AtomicU64,
    decoded_frame_cache_hits: AtomicU64,
    decoded_frame_cache_misses: AtomicU64,
    decoded_frame_cache_evictions: AtomicU64,
    window_cache_hits: AtomicU64,
    window_cache_misses: AtomicU64,
    window_remote_fetches: AtomicU64,
    size_class_frames: [AtomicU64; SIZE_CLASS_COUNT],
    size_class_raw_bytes: [AtomicU64; SIZE_CLASS_COUNT],
    size_class_overscan_bytes: [AtomicU64; SIZE_CLASS_COUNT],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PackedRuntimeMetricsSnapshot {
    pub data_range_gets: u64,
    pub data_range_bytes: u64,
    pub logical_bytes: u64,
    pub overscan_bytes: u64,
    pub frames_decoded: u64,
    pub coalesced_ranges: u64,
    pub inflight_singleflight: u64,
    pub pipeline_bytes_current: u64,
    pub pipeline_bytes_peak: u64,
    pub prefetched_logical_bytes: u64,
    pub data_cache_hits: u64,
    pub decoded_frame_cache_configured_bytes: u64,
    pub decoded_frame_cache_entries: u64,
    pub decoded_frame_cache_resident_bytes: u64,
    pub decoded_frame_cache_hits: u64,
    pub decoded_frame_cache_misses: u64,
    pub decoded_frame_cache_evictions: u64,
    pub window_cache_hits: u64,
    pub window_cache_misses: u64,
    pub window_remote_fetches: u64,
    pub frames_by_size_class: [u64; SIZE_CLASS_COUNT],
    pub frame_raw_bytes_by_size_class: [u64; SIZE_CLASS_COUNT],
    pub overscan_by_size_class: [u64; SIZE_CLASS_COUNT],
}

impl PackedRuntimeMetrics {
    pub fn record_data_range(&self, bytes: u64) {
        self.data_range_gets.fetch_add(1, Ordering::Relaxed);
        self.data_range_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_data_cache_hit(&self) {
        self.data_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_logical_bytes(&self, logical_bytes: u64) {
        self.logical_bytes
            .fetch_add(logical_bytes, Ordering::Relaxed);
    }

    pub fn record_overscan(&self, overscan_bytes: u64, class: usize) {
        self.overscan_bytes
            .fetch_add(overscan_bytes, Ordering::Relaxed);
        let class = class.min(SIZE_CLASS_COUNT - 1);
        self.size_class_overscan_bytes[class].fetch_add(overscan_bytes, Ordering::Relaxed);
    }

    pub fn record_frame(&self, class: usize, raw_bytes: u64) {
        self.frames_decoded.fetch_add(1, Ordering::Relaxed);
        let class = class.min(SIZE_CLASS_COUNT - 1);
        self.size_class_frames[class].fetch_add(1, Ordering::Relaxed);
        self.size_class_raw_bytes[class].fetch_add(raw_bytes, Ordering::Relaxed);
    }

    pub fn record_coalesced_ranges(&self, count: u64) {
        self.coalesced_ranges.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_singleflight(&self, count: u64) {
        self.inflight_singleflight
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn pipeline_acquire(&self, bytes: u64) {
        let current = self.pipeline_current.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.pipeline_peak.fetch_max(current, Ordering::Relaxed);
    }

    pub fn pipeline_release(&self, bytes: u64) {
        let _ =
            self.pipeline_current
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_sub(bytes))
                });
    }

    pub fn record_prefetched_logical_bytes(&self, bytes: u64) {
        self.prefetched_logical_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_decoded_frame_cache_hit(&self) {
        self.decoded_frame_cache_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_decoded_frame_cache_miss(&self) {
        self.decoded_frame_cache_misses
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_decoded_frame_cache_eviction(&self) {
        self.decoded_frame_cache_evictions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_window_hit(&self) {
        self.window_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_window_miss(&self) {
        self.window_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_window_fetch(&self, bytes: u64) {
        self.window_remote_fetches.fetch_add(1, Ordering::Relaxed);
        self.record_data_range(bytes);
    }

    pub fn snapshot(&self) -> PackedRuntimeMetricsSnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        PackedRuntimeMetricsSnapshot {
            data_range_gets: load(&self.data_range_gets),
            data_range_bytes: load(&self.data_range_bytes),
            logical_bytes: load(&self.logical_bytes),
            overscan_bytes: load(&self.overscan_bytes),
            frames_decoded: load(&self.frames_decoded),
            coalesced_ranges: load(&self.coalesced_ranges),
            inflight_singleflight: load(&self.inflight_singleflight),
            pipeline_bytes_current: load(&self.pipeline_current),
            pipeline_bytes_peak: load(&self.pipeline_peak),
            prefetched_logical_bytes: load(&self.prefetched_logical_bytes),
            data_cache_hits: load(&self.data_cache_hits),
            decoded_frame_cache_configured_bytes: 0,
            decoded_frame_cache_entries: 0,
            decoded_frame_cache_resident_bytes: 0,
            decoded_frame_cache_hits: load(&self.decoded_frame_cache_hits),
            decoded_frame_cache_misses: load(&self.decoded_frame_cache_misses),
            decoded_frame_cache_evictions: load(&self.decoded_frame_cache_evictions),
            window_cache_hits: load(&self.window_cache_hits),
            window_cache_misses: load(&self.window_cache_misses),
            window_remote_fetches: load(&self.window_remote_fetches),
            frames_by_size_class: std::array::from_fn(|index| load(&self.size_class_frames[index])),
            frame_raw_bytes_by_size_class: std::array::from_fn(|index| {
                load(&self.size_class_raw_bytes[index])
            }),
            overscan_by_size_class: std::array::from_fn(|index| {
                load(&self.size_class_overscan_bytes[index])
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_metrics_track_pipeline_and_size_classes() {
        let metrics = PackedRuntimeMetrics::default();
        metrics.record_data_range(128);
        metrics.record_logical_bytes(100);
        metrics.record_overscan(28, 2);
        metrics.record_frame(2, 100);
        metrics.record_coalesced_ranges(1);
        metrics.pipeline_acquire(128);
        metrics.pipeline_release(128);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.data_range_gets, 1);
        assert_eq!(snapshot.data_range_bytes, 128);
        assert_eq!(snapshot.logical_bytes, 100);
        assert_eq!(snapshot.overscan_bytes, 28);
        assert_eq!(snapshot.frames_by_size_class[2], 1);
        assert_eq!(snapshot.pipeline_bytes_peak, 128);
        assert_eq!(snapshot.pipeline_bytes_current, 0);
    }
}
