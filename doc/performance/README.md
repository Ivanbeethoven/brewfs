# Performance Evidence

This directory intentionally keeps only the current packed-metadata evidence.
Historical roadmaps and review notes were removed so that stale or
cache-contaminated numbers cannot be mistaken for an acceptance baseline.

## Current Validation

- [native-packed-base-large-scale-validation-2026-09-24.md](native-packed-base-large-scale-validation-2026-09-24.md)
  records the large-file fio read-path evidence and the 100,000-file functional
  validation. Its shared-slice small-file performance numbers are explicitly
  superseded and must not be used for comparison.

- [aliyun-packed-vs-juicefs-smallfiles-2026-09-26.md](aliyun-packed-vs-juicefs-smallfiles-2026-09-26.md)
  records the matched 10,000-file, 100 KiB Aliyun ECS strict cold-read
  comparison after removing the invalid shared-offset fixture.

- [aliyun-packed-v3-cold-read-2026-09-28.md](aliyun-packed-v3-cold-read-2026-09-28.md)
  records the matched packed-v3/JuiceFS 1,000-file full-payload comparison and
  the current high-metadata-latency boundary.

Cold-read artifacts are valid only when the runner records zero data-cache
hits. The runner now forces zero read-memory/SSD budgets, disables prefetch,
requests kernel cache eviction, and fails a tool when any data-cache hit is
observed.
