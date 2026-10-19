# R32 WRITE investigation: candidate superseded by namespace redesign

Status: **diagnosis complete; unvalidated r32 candidate dropped by design
directive**. On 2026-10-19 the owner requested an ordinary per-entity upper
namespace instead of further refinement of the sequence/head design.
No r32 release, real-mount probe or candidate performance comparison was run.
All uncommitted Rust/test/runner changes were archived and removed. This is
a design supersession, not a measured performance rejection.

The replacement is specified as **proposed, not implemented** in the
[architecture](../../architecture/workspace-overlay.md) and
[implementation spec](../specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md).
No large refactor is part of this documentation change.

The experiment branch is `codex/overlay-write-r32-20261019`, based on `6b54270`.
The integration target is `codex/packed-metadata-aliyun-20260930`. The code
baseline is r31 / PR #41 (`0d4e36c`); `6b54270` completed its delivery documents.
The replacement design documents are delivered separately on
`codex/overlay-namespace-design-20261019`; the proposed runtime is not merged.
See the [delivery handoff](2026-10-17-overlay-no-seal-handoff.md) and
[architecture](../../architecture/workspace-overlay.md) for the established
immutable packed lower, non-rotating overlay, lease and GC design.

All artifact paths below are relative to:

```text
R=/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z
A=$R/write-gap-20261019-analysis
```

## Accepted baseline and measurement contract

The completed r31 comparisons contain six samples per build/arm/worker count,
pooled from `ovn-r31-r31w{16,32}p{a,b}/ovn-*/summary.json`. Values below are
arithmetic means of individual `files_per_sec` rows, with sample standard
deviation. They are not averages of the runner's per-group medians.

| Workers | r31 overlay files/s | r31 native files/s | Overlay/native |
| --- | ---: | ---: | ---: |
| 16 | 216.892 ± 3.741 | 344.005 ± 3.435 | 0.6305 |
| 32 | 236.892 ± 7.991 | 414.744 ± 9.294 | 0.5712 |

The profile creates 2,000 files in 20 directories, writes 16 KiB per file,
fsyncs and closes. Directory setup is outside active time. Metadata is Redis,
objects are RustFS, FUSE TTL is zero, direct I/O is enabled, keep-cache is zero,
and payload caches/compression/prefetch are disabled. Workload concurrency is
16 or 32; the FUSE pool remains 16 workers. The deterministic mixed phase has
2,000 operations, 603 writes, and verifies bytes on reads. The readback phase
also verifies payloads. Setup, mount, teardown and deferred FUSE release are
not a strict end-to-end durability timer merely because `os.close` is timed.

Preserved r31 executable identities:

```text
2a5146cb21a00334d4240a1b23a0b1b26a12b71202ee2494ef435f37fbfc249b  build/r31/brewfs
3a142f121cc3cd3d5b74a372b52ce363529f2fbd31d8a26915058ced9d69b235  build/r31/packed_v3_snapshot_fixture
```

`$A/README.md` and `existing-analysis.json` preserve the per-run sources and
baseline calculation. The earlier final all-mode sweep was interrupted; its
partial rows are excluded. No historical result substitutes for a missing
candidate or final-build measurement.

## What binds WRITE scaling

The mount-wide mutation gate is occupied for 91.2% of scanner active time at
32 workers. Its measured service cost is 3.703 ms/file, giving a same-work-mix
ceiling of about 270 files/s at continuous occupancy. The diagnostic run
observed 246.29 files/s. Doubling workers raises summed waiting per file from
21.320 to 58.074 ms while gate service remains about 3.7 ms/file. This is direct
evidence of serialized metadata work limiting scaling.

The primary measurements use an instrumented baseline, with no MONITOR or
perf recording, at:

- `ovn-r32-diagnostic-w16/ovn-20261019T132059Z-2780834`;
- `ovn-r32-diagnostic-w32/ovn-20261019T132232Z-2784370`.

Both groups passed all 12 workload rows in total, with zero errors, matching
traces/checksums and no remaining compose resources or FUSE mounts. Diagnostic
binary SHA-256 is
`0e3b45cb282b0327b114aaa356ebe818a7c82e38d1e115e41cbf9ad848ea4948`.
These single repetitions establish attribution; their rates do not replace
the accepted r31 comparison or establish any candidate gain.

| Measurement | 16 workers | 32 workers |
| --- | ---: | ---: |
| Diagnostic overlay / native files/s | 216.71 / 370.09 | 246.29 / 436.32 |
| Gate held / scanner active time | 82.0% | 91.2% |
| Create gate service, ms/file | 2.108 | 2.230 |
| Open gate service, ms/file | 0.519 | 0.553 |
| Close gate service, ms/file | 0.538 | 0.554 |
| Data-batch gate service, ms/file | 0.620 | 0.367 |
| Total held gate service, ms/file | 3.785 | 3.703 |
| Summed gate wait, ms/file | 21.320 | 58.074 |
| Data writes per committed batch | 1.946 | 4.024 |
| BrewFS CPU, overlay / native average cores | 1.928 / 2.195 | 2.045 / 2.387 |
| Redis CPU, overlay / native average cores | 0.296 / 0.274 | 0.304 / 0.286 |
| Object PUT elapsed, overlay / native mean ms | 21.949 / 22.408 | 23.630 / 32.453 |

At 32 workers, create accounts for 60.2% of gate service, open plus close for
29.9%, and data batches for 9.9%. The create stage includes 1.504 ms/file in
commit, 0.460 in fresh parent permission lookup, 0.134 in dentry resolution,
0.079 in actor checking, 0.024 in allocation and 0.007 in view capture.
Reverse-index preparation is nested inside commit and costs 0.537 ms/file;
it must not be added a second time. Existing inode allocation already reserves
blocks of 64 IDs. Data batching already works and grows with concurrency.

The overlay's object PUTs are faster than native at 32 workers; exactly 2,000
PUTs and 2,000 data members correspond to 2,000 successful files in each write
phase. The additional latency is in create/open and fsync: mean scanner syscall
times at 32 workers are 64.618 versus 26.317 ms for create/open and 58.087 versus
37.590 ms for fsync. Upload and gate elapsed spans overlap, so subtracting
their totals does not establish an additive critical path.

### Redis/server time versus client elapsed

Timed commit CAS calls average 0.709/0.770 ms at 16/32 workers. Redis commandstats
from the same runs put conservative upper bounds of 0.312/0.346 ms on server
execution per timed CAS, leaving at least 0.397/0.424 ms outside execution.
That remainder includes encoding, transport, Redis queueing outside command
execution and task scheduling; it is not a client CPU measurement.

The bound assigns **all** EVALSHA time in a wider interval to the timed CAS
subset, although that total also includes bounded reads, allocation and setup.
Server snapshots contain all timed CAS calls, and EVAL/SCRIPT LOAD deltas are
zero. Do not add Lua child-command durations to EVALSHA duration: that would
double-count execution. This bound and Redis's approximately 0.30-core process
CPU contradict an explanation based on Redis CPU saturation alone.

Independent MONITOR captures count 12.6255/12.2030 logical backend requests per
overlay file at 16/32 workers, versus 8.0000 for native. Every overlay create
still commits individually. Native MULTI pipelines count once, not once per
contained command. These are logical request counts, not packets or measured
round-trip durations. MONITOR perturbs latency and batch formation, so its
throughput and batch counts cannot be combined with primary stage timings.

Separate perf recordings, restricted to scanner write-active monotonic
boundaries, spread CPU across eight Tokio workers: each has about 10.5–11.3%
of process samples. At 32 workers, sampled self CPU is 46.91% in the BrewFS
binary, 32.50% in the kernel and 19.60% in libc. There is no single saturated
worker explaining the plateau. Reports preserve zero lost samples and 1/4
out-of-order warnings at 16/32 workers. An unsupported initial report sort was
detected and corrected; its failed outputs remain preserved.

Paired 400-file SLOWLOG+MONITOR runs give classified server means, but logging
strongly inflates them: overlay create averages 1.372/1.342 ms and data batches
1.534/2.293 ms at 16/32 workers. They describe that logged experiment only and
cannot be subtracted from clean stage timings. The retained script streams
and truncated argument prefixes were validated. A SLOWLOG-only follow-up was
completed with MONITOR/perf disabled. Overlay create mean/p95 is 297/554 µs
at 16 workers and 322/573 µs at 32; data-batch mean/p95 is 336/695 and
440/975 µs. Native create means are 192/200 µs and native data means 144/160 µs.
These windows include 400 files plus 20 setup directory creates, and retain
logging overhead. Without MONITOR's child-command stream, data counts are
attempts, not independently proven successful mutations. All 12 capture
windows have consecutive IDs, stay below the 100,000-entry capacity, and
complete with zero workload errors and clean teardown. See
`$A/SLOWLOG-ONLY-RESULTS.md` for exact provenance and limitations.

Sources: `$A/BASELINE-PROOF.md`, `STAGE-RESULTS.md`, `MONITOR-RESULTS.md`,
`PERF-RESULTS.md`, `SLOWLOG-RESULTS.md` and their named JSON/raw artifacts.
`DIAGNOSTICS.md` and `launch_diagnostics.sh` preserve exact diagnostic commands.

### Instrumentation limits

Only the disjoint gate-held spans may be summed. Held timers stop immediately
after lock release, so a scheduling-boundary overcount is possible. Wait spans
overlap among tasks; data wait measures the leader, not every queued follower.
The group leader can drain further batches before returning. Deferred release
can outlive `os.close`, and final counters include 250 ms settling. CPU snapshots
are taken around active time with recorded PID start identities, subject to
`/proc` tick resolution. These caveats prevent interpreting every timer as an
exclusive walltime component.

The archived diagnostic instrumentation is opt-in; it is not retained in the
current source after candidate removal. With `OVN_WRITE_DIAGNOSTICS=0`, Rust takes no added
timestamps or atomic counter updates; the scanner retains its original
open/write/fsync/close sequence, payload, operation order and worker pool.
Process/`.stats` snapshots and settling are disabled. SLOWLOG, MONITOR and perf
are also disabled for acceptance. Disabled timer checks still have a small
branch/OnceLock cost, which a candidate comparison would need to include.

## Design assessment and disposition

The binding constraint is the mount-wide lane enclosing permission reads,
create commits and grouped data commits. Removing its mutex alone would retain
the expected-head conflict and extent-generation conflict. It also exposes
cache-publication and open-count lifetime races that the current gate protects.

The historical r27 ungated-data experiment was partial but informative:
completed 16-worker groups fell from a baseline median 234.34 files/s to
166.47 and 163.16, with EVALSHA/file rising from 3.214 to 11.476/11.570.
Its baseline reverse-order group was incomplete and 32-worker arms never ran.
Those measurements are not results for r31 or for the later r32 draft.

The packed store has a 256-iteration inner retry bound; outer metadata loops
usually allow 64 attempts. A fresh head mismatch returns Busy to the outer
decision loop, so these do not imply 16,384 script executions or a wall-time
deadline. Cached permission/namespace decisions must be recalculated after
conflict. Historical failed-CAS counts and their tail distribution were not
preserved and are not inferred from EVALSHA totals.

Atime is absent from the measured FUSE CREATE/record_open/record_close path.
The separate legacy MetaLayer::open updates it. The diagnostic recorded zero
OtherHeld events and exactly 2,000 create/open/close events in each WRITE phase.
Native create is one atomic Lua operation without a client mount-wide mutex;
Redis still executes scripts serially and updates the allocator and parent.
Native ordinary open/close do not commit metadata.

Two draft directions were superseded:

1. Prefetch permission snapshots before the gate. This targeted at most the
   measured 29.9% open/close gate service. It had focused correctness evidence
   but no performance comparison.
2. Assign sequences/counters in the backend using exact per-row read proofs,
   shared local admission for create/data/open/close and per-inode/parent
   locks. This retained the shared head's accounting/version semantics and
   per-layer generation while removing stale-counter comparisons. Its later
   implementation/test draft was never compiled or validated.

**Drop both.** The second direction is incompatible with the owner's goal of
removing the sequence/head model, and keeping the first would add an unmeasured
optimization to the redesign. Original diagnostics and correctness lessons are
useful; neither candidate is shipped.

| Build/design | 16w WRITE overlay/native files/s | 32w WRITE overlay/native files/s | Candidate delta |
| --- | ---: | ---: | --- |
| Completed r31 acceptance | 216.892 / 344.005 | 236.892 / 414.744 | Reference |
| Prefetch-only draft | Not measured | Not measured | Unknown |
| Atomic-sequence r32 draft | Not measured | Not measured | Unknown |
| Proposed per-entity namespace | Not implemented | Not implemented | Unknown |

No win is claimed from source inspection or this table.

## Validation actually completed

Before prefetch behavior was added, the two deliberate prewarm race tests
failed and the lease-revocation bypass test passed. The full instrumented
baseline overlay library suite finished with 2,256 passed, 7 failed and
375 ignored in 577.92 seconds. Besides the two deliberate RED tests, failures
were the three documented baseline overlay failures,
source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement,
and writable_acl_independent_sqlite_instances_keep_mode_acl_during_atime_writes
(EBUSY). Sources: $R/build/r32/{red.log,red.status,baseline-feature.log}.

The SQLite failure occurred during observer.open(...RDONLY), before the
mode/ACL assertion. Independent native SQLite meta layers were racing legacy
atime and chmod; they did not use packed lower/cache/record_open. The log shows
bounded conflict exhaustion, not observed ACL corruption. The exact losing
version check was not traced.

The prefetch-only gate completed 42 focused tests (80.22 seconds), workspace
and overlay checks, workspace build, Tokio/io_uring checks with and without
overlay, and default workspace lib/bin tests (1,178 passed, 225 ignored).
The full overlay suite was **interrupted** for the owner's first design
change. The later atomic-sequence draft has no test/build result. Earlier
passes do not validate that draft.

Per-command statuses and logs are at $R/build/r32/gates/; INTERRUPTED.md records
the termination of run-u82.scope. All-features, clippy, transport/operator and
candidate performance acceptance were incomplete in that wrapper. Historical
operator clippy must_use failures and the three baseline overlay/source_file
failures remain documented in the r31 handoff.

## Preserved artifacts and next action

$A/superseded-r32-draft/ contains the complete removed tracked patch, untracked
source files, base commit, SHA-256 manifest and an explicit unvalidated-status
README. It includes diagnostic runner/scanner sources, so the saved diagnostic
binary is not mistaken for a build from the reverted current worktree.
No accepted baseline artifact or binary was deleted.

The earlier prepared release, probe, comparison and analyzer scripts remain
under $R/build/r32/ and $A/. They were not executed for a candidate and are
superseded; a new-format implementation needs new provenance and fresh gates.
The untracked empty file named |RwLock has unknown provenance and was left
untouched; it is not part of the delivery.

The next authorized work in this turn is the design/spec rewrite only. The
migration stages, replacement GC proof, truncate/recovery protocol, lifecycle
trade-offs and future acceptance requirements are in the maintained specs.
No release benchmark was rerun.

The documentation delivery restored all Rust and runner paths to `6b54270`,
then passed `cargo fmt --all --check` and
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --workspace --features workspace-overlay`
(2m19s; existing dependency warnings). Logs and exact environment are in
`$R/build/namespace-design-20261019/{status.json,fmt.log,check-overlay.log}`.
These checks verify the restored source, not an implementation of the proposed
model. The final documentation diff and local links were checked separately.
