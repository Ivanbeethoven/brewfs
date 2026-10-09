# Performance Evidence

This directory intentionally keeps only the current packed-metadata evidence.
Historical roadmaps and review notes were removed so that stale or
cache-contaminated numbers cannot be mistaken for an acceptance baseline.

## Current Validation

- [Current reply retirement TDD](packed-v3-reply-retirement-validation-2026-10-05.md)
  records two real worker/channel lifecycle tests passing after their intended
  RED, followed by terminal-response and bounded-close RED/GREEN. Current socket
  regressions pass io-uring60/Tokio28; actual VFS preparation fits the original
  Roots bound. Full gate06 is running. The predecessor normal mount is independently
  verified; strict SIGTERM, all thread joins and full SPEC/S/X stay open.

- [Latest Plans admission checkpoint](packed-v3-plans-admission-validation-2026-10-05.md)
  records the 532-input gate05/supplement04 49+17 build checkpoint, new frozen
  runtimes and an independently verified 17-stage normal FUSE run. Four exact
  behavior failures established preparation's whole-pool self-exhaustion; six
  strict/minimum-budget repair checks pass. Strict pending-body SIGTERM failed
  and its complete evidence/work are protected. Full SPEC/S/X stay open.

- [Current bounded source-inventory checkpoint](packed-v3-source-batch-validation-2026-10-04.md)
  binds366 source files to47 passed gate02 checks,14 pinned raw/zstd FUSE mounts
  and3 rerun real Btrfs tests. The same36k import now completes with weighted
  paging, cookies, active handles, fresh restart and normal cleanup. Production
  binaries remain byte-identical after a test-only sparse-fixture synchronization.
  Original failed work directories are absent after environment recovery; their
  persistent diagnostics remain and this retention limit is explicit. Actual
  typed GET attribution/physical eviction, teardown root cause and S/X remain open.

- [Current rooted/frozen source and import-seek checkpoint](packed-v3-frozen-source-seek-validation-2026-10-04.md)
  binds366 source files to46 passed checks,8 raw/zstd real FUSE cases and3 pinned
  real Btrfs tests. Deep paths beyond PATH_MAX, readonly snapshot guards and
  normal cleanup pass. Both36k imports timed out before mount; bounded spool
  transactions and remaining read/workspace/operator contracts remain open.
  Current scope is v3 only; S/X and performance acceptance remain false.

- [Latest external placement checkpoint](packed-v3-external-validation-2026-10-04.md)
  records automatic PM09 source routing, required selector closure, paged sparse
  extents and bounded authenticated LD05 chunks. Forty final checks, 1,328 overlay
  tests, six source CLI cases and four raw/zstd external/legacy namespace mounts
  pass. External full reads total 444,672,144 bytes, with cross-directory hardlinks,
  attributes, partial boundaries, EROFS and cleanup verified. G03 is closed;
  full SPEC/S/X remain open. POSIX ACL grant/query failures are reproduced separately.

- [Latest source-name and xattr checkpoint](packed-v3-namespace-posix-validation-2026-10-04.md)
  repairs four actual FUSE failures: source `.stats` shadowing, raw/UTF8 xattr
  alias confusion, listxattr EIO and readonly removexattr EIO. Forty final checks,
  1,316 overlay tests and four raw/zstd mounts pass with source/binary identities
  and normal cleanup. Full POSIX/ACL/frozen source view and system exits remain open.

- [Latest bounded namespace checkpoint](packed-v3-namespace-validation-2026-10-04.md)
  records PM08 directory inventory, raw paths, root/blocks, special inode kinds,
  explicit hardlink policies and readonly rename/link/open repairs. Forty final
  checks and raw/zstd 540-entry FUSE cases pass with source/binary identities and
  normal teardown. Frozen source view, ACL/other raw namespace boundaries, external
  large placement and packed workspace lifecycle remain open.

- [System readiness and experiment entry gates](../superpowers/plans/2026-10-04-brewfs-system-readiness.md)
  records the October 4 source/binary identity check and remaining source/read/
  packed-workspace publication/recovery/GC requirements. Core-system and main
  experiment readiness are both false. Experimental performance design remains
  a draft until those gates pass; correctness evidence retains its actual scope.

- [Latest source and semantic correctness checkpoint](packed-v3-source-validation-2026-10-03.md)
  records 005 GM07/IL05 semantic rejection, bounded actual Linux source capture,
  raw filename/ancestor permission fix, raw/zstd 10 source mounts and 100/1,000
  file RustFS full checks. Full inventory/root/blocks/large/lifecycle remain open;
  this is correctness evidence, not a performance comparison.

- [Current SPEC gap audit](../superpowers/plans/2026-10-03-packed-v3-spec-gap-audit.md)
  maps the actual uncommitted 005 implementation and inherited evidence to 17
  gates. G01 is now implemented with red/green evidence; G02–G04 record their
  bounded source/POSIX/sparse progress and remaining full-SPEC exits.

- [Current three-innovation experiment design](../superpowers/plans/2026-10-03-brewfs-three-innovations-experiment-plan.md)
  defines native/packed × static/dynamic controls, separate inline/codec/cache
  effects, workspace lifecycle costs, matched baselines and bounded scale gates.

- [Latest cold/hardlink correctness checkpoint](packed-v3-cold-hardlink-validation-2026-10-03.md)
  records actual 005 cold/hardlink FUSE validation and its inherited local gate;
  this is correctness evidence, not new performance acceptance.

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
  preserves the historical October 2 plan. Its implementation status and next
  steps are superseded by the current audit and experiment design linked above.

- [Packed-v3 naming and real Redis/TiKV validation](packed-v3-redis-tikv-naming-validation-2026-10-07.md)
  records the V3/wire005 naming contract, 17 executed real metadata test entry
  points, seven destructive-revalidation cases per backend, exact service
  cleanup and the remaining publication/recovery/GC boundaries.

Cold-read artifacts are valid only when the runner records zero data-cache
hits. The runner now forces zero read-memory/SSD budgets, disables prefetch,
requests kernel cache eviction, and fails a tool when any data-cache hit is
observed.
