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

- [packed-v3-vs-juicefs-tikv-metadata-stat-2026-09-30.md](packed-v3-vs-juicefs-tikv-metadata-stat-2026-09-30.md)
  defines the matched metadata-only stat workload for packed v3 versus
  JuiceFS+TiKV, records the local packed FUSE baseline, and preserves the TiKV
  image-download blocker without claiming a win.

- [packed-v3-decoded-frame-cache-local-2026-10-01.md](packed-v3-decoded-frame-cache-local-2026-10-01.md)
  records the repeated local full-read A/B for the explicit 32 MiB exact
  decoded-frame cache, including request counts, latency, bounded residency,
  and rejected window/coalescing candidates. It is a warm-frame-cache result,
  not a strict-cold JuiceFS comparison.

- [aliyun-packed-v3-vs-juicefs-triple-1m-2026-10-01.md](aliyun-packed-v3-vs-juicefs-triple-1m-2026-10-01.md)
  records the completed 10k, ordered 1M and shuffled 1M packed/Redis/TiKV
  observations. Packed does not win the shuffled profile; historical TTL
  symmetry claims require revalidation after the runner forwarding correction.

- [packed-v3-index-budget-candidate-2026-10-02.md](packed-v3-index-budget-candidate-2026-10-02.md)
  closes out the withdrawn index-budget candidate: authenticated-page tests
  improved, but constrained-budget mounted-FUSE runs had EIO and no throughput
  claim is accepted. Includes the dynamic/packed corpus and TTL correction.

- [Three-innovation experiment plan](../superpowers/plans/2026-10-02-brewfs-three-innovations-experiment-plan.md)
  separates metadata/frame/inline/cache effects, names the incomplete packed
  lower lifecycle and authentication gates, and defines matched TiKV validation.

Cold-read artifacts are valid only when the runner records zero data-cache
hits. The runner now forces zero read-memory/SSD budgets, disables prefetch,
requests kernel cache eviction, and fails a tool when any data-cache hit is
observed.
