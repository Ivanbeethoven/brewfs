# Entity read batching: request counts and preliminary performance

This continuation is on `codex/overlay-entity-rtt-20261020`. All performance
results here are **preliminary, not acceptance results**. They cover the existing
WRITE/READBACK workload on new upper files with Redis and real RustFS. They do
not establish mixed-I/O, copy-up, production admission or native-class parity.

The predecessor is `df846766c14860ef97ea1fdb79a0bb8d3ef88179`, documented by
`ebe20712`. The earlier entity branch was already merged into the actual default
`main` by [PR #42](https://github.com/Ivanbeethoven/brewfs/pull/42), merge
`3cb2082500aac0476f275b107228f7dd14a087ce`. This continuation does not merge more
changes to main.

The retained changes are stage 1 read batching (`8a0f8b8`) and the narrow
Redis script follow-up (`16c2aca7e8d152fa73e55651d2c7ffefb623d5cd`), with service
gate alignment in `49da510`. The fsync follow-up was rejected and reverted.
The latest comparison below uses a fresh stage 1 control; earlier results
remain separate.

## Scope and implementation

“Snapshot” in internal helper names means only an operation's existing batched
read of authority and required inode, dentry, ACL/xattr and chunk rows. There is
no new snapshot feature, backend API, Lua transaction kind, metadata schema,
persistent object, mount mode, lifecycle object or cross-operation cache.

Commit `8a0f8b8` reuses the existing `EntityBackend::read` and `READ_LUA`, which
already return selected rows and backend TIME atomically. The existing
operation-local `Prepared` read set records whether one atomic read covered all
of its observations. Dependent reads include previous observations and newly
needed rows together. Disagreement never overwrites an earlier observation;
it requires complete revalidation/retry. Covered read-only completion omits
the redundant read-only transaction. Data reads now observe the actual stored
geometry row and reject a mismatch.

Named inode-lock acquisition and directory SCAN invalidate coverage, including
an empty scan. Payload reads retain validation after object transport. Mutation
transactions retain exact authority, entity and absence checks; permissions and
O_EXCL are recalculated after conflict. Upper values/tombstones still resolve
before lower fallthrough. Mutable upper state and operation decisions are not
reused across FUSE operations; the existing immutable lower memos remain.

A metadata read can linearize at its complete, lease-fenced atomic read. If
immutable lower metadata resolution subsequently spans lease expiry, that
already-admitted read can succeed where the old trailing read-only transaction
would have returned `Fenced`. The immutable pinned lower and retained operation
protect its value and lifetime. This is not an end-of-metadata-resolution lease
guarantee; do not claim identical expiry-error timing. Reads started with an
expired lease still fail, and payload post-transport fences remain.

Commit `49da510` aligns the service runner with the mandatory workspace/lib/bins
test graph. Its summary parser requires one nonempty successful suite and allows
only empty successful companions, with no failed or ignored tests.

## Stage 1 measured changes

Diagnostic runs use 200 files of 16 KiB, 16/32 workers, one repetition, fsync per
file, Redis MONITOR and existing stage timers. Counts include setup, attempted
requests and small background contributions. Native pipelines commands, so its
external command count must not be called its network RTT count.

| Phase | Workers | Predecessor commands/file | Batched reads | Native in candidate run |
| --- | ---: | ---: | ---: | ---: |
| WRITE | 16 | 15.720 | 12.475 | 7.110 |
| WRITE | 32 | 15.780 | 12.475 | 7.110 |
| READBACK | 16 | 16.015 | 10.010 | 5.005 |
| READBACK | 32 | 16.015 | 10.005 | 5.000 |

WRITE uses 7.360 READ_LUA, 5.105 transaction and 0.005 SCAN scripts/file after
batching, plus 0.005 health PING commands. READBACK uses 9.005 READ_LUA and 1.000
read-only transaction/file; its 16-worker sample also has 0.005 PING commands.
READBACK still includes separate LOOKUP, OPEN, payload READ, EOF READ and FLUSH
operations.

| FUSE operation | Predecessor metadata RTT attempts/op | Batched reads |
| --- | ---: | ---: |
| LOOKUP | 2 | 1 |
| GETATTR | 2 | 1 |
| Inode OPEN | 2 | 1 |
| CREATE | 2 | 2 |
| WRITE | 3 | 3 |
| FSYNC / FLUSH | 2 | 2 |
| READ, payload/EOF combined mean | 3 | 2 |
| RELEASE | 0 | 0 |

The predecessor's 32-worker WRITE LOOKUP actually averages 2.018692 attempts
(1,296 attempts across 642 callbacks); the other measured LOOKUP cells average
two. Those extra attempts are not independently classified as CAS retries.

The READ mean combines 200 payload and 200 EOF callbacks: payload falls from
four calls to three, EOF from two to one. Object HEAD/GET are additional object
requests, not metadata RTTs. A named SDK open retains its separate post-lock
validation; the ordinary FUSE OPEN uses the known inode.

Throughput runs are separate from MONITOR: 2,000 files of 16 KiB, fsync per file,
two repetitions with opposite arm orders at each worker count. Values below
are the median of two samples (also their arithmetic mean); entity/native is
the ratio of those medians. Two samples do not establish statistical significance.

| Phase | Workers | Predecessor entity files/s | Batched reads files/s | Current native files/s | Entity/native | Raw entity change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| WRITE | 16 | 272.210 | 296.354 | 383.704 | 0.772 | +8.87% |
| WRITE | 32 | 311.518 | 361.052 | 436.964 | 0.826 | +15.90% |
| READBACK | 16 | 342.434 | 438.570 | 633.188 | 0.693 | +28.07% |
| READBACK | 32 | 378.386 | 534.686 | 633.361 | 0.844 | +41.31% |

All four cells improve raw entity throughput in both orders. The normalized
WRITE improvement at 16 workers is small and order-sensitive: +2.04% using
cell medians, +5.90%/-1.77% by order. Native itself rose 6.69% there. Do not call
this a conclusive relative WRITE win. That cell's second-order p99 rose 4.62%
(80.522 to 84.241 ms), while mean/p50/p95 improved. Both READBACK cells and
32-worker WRITE improved p95/p99 in both orders. At 32 workers native READBACK
ranged from 576.965 to 689.756 files/s while entity ranged 532.568–536.805;
the corresponding entity/native ratios therefore vary substantially.

## Stage 1 correctness and artifact provenance

The 24-step gate passed for the exact frozen source, before the release build
and measurements. It includes fmt, default/overlay workspace check, default
workspace build, four runtime feature checks, strict all-targets Clippy for
default and overlay, shell/report checks and `git diff --check`.

| Suite | Passed | Failed | Ignored | Explicit exclusions |
| --- | ---: | ---: | ---: | ---: |
| Unfiltered default workspace lib/bins | 1,179 | 0 | 225 | 0 |
| Overlay workspace library | 2,338 | 0 | 405 | 5 |
| Overlay binary tests | 9 | 0 | 0 | 0 |
| Real-service packed-lower suite | 7 | 0 | 0 | Other tests outside filter |
| Real-service entity suite | 104 | 0 | 0 | Other tests outside filter |

The three established legacy overlay failures remain excluded:
`g13a_kv_released_lease_is_not_an_expired_recovery_root`,
`kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`, and
`migration_marker_fences_entity_and_hot_native_packets_without_writes`.
The two known source-file flakes also remain excluded:
`source_mutation_replacement_symlink_and_limits_fail_closed` and
`source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`.
This is not an unfiltered overlay pass. The separate operator `double_must_use`
issue is outside the workspace gate and is not repaired here.

Both arms passed mount/create/write/fsync/byte-verified-readback/unmount smokes.
All eight diagnostic and sixteen throughput phases completed with zero workload
errors and successful checksum verification. Each group's cleanup proof records
zero owned containers, volumes and remaining FUSE mounts, with all mount
processes clean. These are isolated harness cleanup proofs, not production GC
or admission qualification. `overlay-native` remains skipped because the frozen
native-lower adapter/binding/runtime wiring does not exist.

The host artifact root is
`/mnt/brewfs-data/artifacts/overlay-entity-rtt-20261020`. Its
`stage1-batched-read/` contains the frozen source manifest, patch, exact gate
commands/results, service logs, preserved release executable, diagnostic and
throughput raw groups, `audit.json`, `audit.md`, and `accepted-commit.json`.
`baseline/` preserves predecessor/native/fixture executables and evidence.
Source, executable hashes, workload traces, smoke and cleanup proofs are checked
by the stage audit. The release build completed in 43m11s; it was not interrupted.
Redis/RustFS image IDs also match the predecessor in both diagnostic and
throughput groups.

To reproduce the retained stage 1 using its preserved binaries, use a checkout
of the matching source so the runner records accurate provenance. The runner
owns isolated Redis/RustFS resources and performs the existing minimal smokes.
It requires Linux FUSE, Docker Compose, Python/boto3, passwordless sudo and
`allow_other` support. Do not run it alongside builds or other workloads.

```bash
git worktree add --detach /tmp/brewfs-read-batching-repro 8a0f8b8
cd /tmp/brewfs-read-batching-repro
entity_art=/mnt/brewfs-data/artifacts/overlay-entity-rtt-20261020
sha256sum -c "$entity_art/stage1-batched-read/binary.sha256"
sha256sum -c "$entity_art/baseline/binary.sha256"
export ENTITY_MOUNT_BINARY="$entity_art/stage1-batched-read/brewfs"
export ENTITY_MOUNT_NATIVE_BINARY="$entity_art/baseline/native-brewfs"
export ENTITY_MOUNT_FIXTURE="$entity_art/baseline/packed_v3_snapshot_fixture"
export ENTITY_MOUNT_WORKERS=16,32
ENTITY_MOUNT_ARTIFACT_ROOT="$entity_art/reproduce-stage1-diagnostic" \
  ENTITY_MOUNT_DIAGNOSTIC=1 ENTITY_MOUNT_FILES=200 ENTITY_MOUNT_REPETITIONS=1 \
  bash tools/perf/run_entity_mount_comparison.sh
ENTITY_MOUNT_ARTIFACT_ROOT="$entity_art/reproduce-stage1-throughput" \
  ENTITY_MOUNT_DIAGNOSTIC=0 ENTITY_MOUNT_FILES=2000 ENTITY_MOUNT_REPETITIONS=2 \
  bash tools/perf/run_entity_mount_comparison.sh
```

Exact local gate commands are preserved in artifact `gate_stage.sh`; its
`stage1-batched-read/gate.tsv` and `gate-proof.json` record the successful run.
The mandatory unfiltered default test command is
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins`.
Only the overlay test command applies the five documented exclusions above.

## Rejected fsync follow-up

`stage2-fsync` is **rejected and reverted**, with no code commit. Retained fsync
behavior remains that of stage 1 (`8a0f8b8`); final source also includes the
stage 3 script change below. The candidate removed the read-only
transaction after an upper-inode fsync/flush's initial authority/entity read.
It retained FIFO inode locking, admission, completed upload/publication, sticky
errors, recovery and stored geometry checks. Lower-only resolution retained a
fresh full validation after resolution; conflicts retried the operation. FLUSH
still ran its own barrier even after a successful FSYNC. Its 24-step correctness
gate and artifact-completeness checks passed with the documented exclusions.
That evidence did not establish a qualified performance change.

The diagnostic request reduction is real. These external Redis commands/file
include the same 200-file profile and small background contributions:

| Phase | Workers | Retained stage 1 | Rejected fsync candidate |
| --- | ---: | ---: | ---: |
| WRITE | 16 | 12.475 | 10.475 |
| WRITE | 32 | 12.475 | 10.470 |
| READBACK | 16 | 10.010 | 9.005 |
| READBACK | 32 | 10.005 | 9.010 |

Candidate WRITE uses 7.360 READ_LUA, 3.105 mutating transaction and 0.005 SCAN
scripts/file; READBACK uses 9.005 READ_LUA and no transaction scripts. The
remaining 0.005 differences are health PING commands. FSYNC and FLUSH each
fall from two attempted metadata RTTs per callback to one. CREATE remains two,
WRITE three, and the payload/EOF READ mean two. These candidate counts are not
the behavior of the retained source. Native commands/file are 7.110 WRITE and
5.000 READBACK in this diagnostic, with native pipelining still distinct from
network RTT count.

The original uninstrumented comparison used the same two opposite-order
repetitions and 2,000-file profile as stage 1. All rows below are preliminary;
values are two-sample means/medians. E/N change compares the entity/native
ratio to that of the corresponding stage 1 group, using unrounded values.

| Phase | Workers | Stage 1 entity / native files/s | Candidate entity / native files/s | Raw entity change | E/N change |
| --- | ---: | ---: | ---: | ---: | ---: |
| WRITE | 16 | 296.354 / 383.704 | 321.571 / 373.832 | +8.509% | +11.375% |
| WRITE | 32 | 361.052 / 436.964 | 367.068 / 452.791 | +1.666% | −1.887% |
| READBACK | 16 | 438.570 / 633.188 | 491.657 / 687.039 | +12.105% | +3.318% |
| READBACK | 32 | 534.686 / 633.361 | 557.881 / 630.043 | +4.338% | +4.887% |

WRITE32 lost relative to native in both orders (−2.125%/−1.872%). In the first
order, entity p99 rose from **128.924 to 400.121 ms**, while p50/p95 improved
and same-order native p99 improved. The saved logs and aggregate counters do
not explain the tail; the separate diagnostic run cannot attribute an event
in the uninstrumented run. No host, object-store or network cause is proved.

An adjacent matched recheck ran the unchanged stage 1 binary followed by the
unchanged fsync candidate, each with the same frozen native/fixture binaries,
16/32 workers, two opposite arm orders and existing workload. No build competed
with these runs. This is a separate comparison, not pooled with the original:

| Phase | Workers | Fresh stage 1 entity / native files/s | Candidate entity / native files/s | Raw entity change | E/N change |
| --- | ---: | ---: | ---: | ---: | ---: |
| WRITE | 16 | 294.342 / 368.173 | 312.363 / 385.262 | +6.122% | +1.415% |
| WRITE | 32 | 362.440 / 445.843 | 386.157 / 441.349 | +6.544% | +7.629% |
| READBACK | 16 | 464.244 / 659.061 | 483.523 / 680.115 | +4.153% | +0.928% |
| READBACK | 32 | 524.953 / 667.713 | 525.068 / 689.720 | +0.022% | −3.170% |

The 400.121 ms outlier did not repeat and remains unresolved. The recheck still
does not show a consistent improvement: first-order normalized changes were
−4.434% WRITE16, −9.871% READBACK16 and −10.224% READBACK32. WRITE32's second
order p99 rose 12.548%, from 117.680 to 132.447 ms. Native control drift and
the order-sensitive results prevent a stable incremental throughput/tail
claim; two samples do not establish statistical significance or prove the code
caused a universal regression. The request reduction alone is insufficient to
retain this candidate.

The original diagnostic/throughput groups and both recheck groups completed
their expected phases with zero workload errors, verified checksums and
successful minimal smokes.
Their cleanup proofs show no owned containers, volumes or FUSE mounts and clean
mount-process exits. Binary/source manifests, workload traces, runner and
Redis/RustFS image identities match the intended comparison. The original
candidate source manifest SHA256 is
`1325d79322cd6319198440528b09f948878603503316870fa91d4f15dd1b4dd9`;
its preserved executable SHA256 is
`0c0b549ceff6927f60ec4e7badc876763bf36c252e80eafc5d3fd2ad0f69c8d6`.

Under the artifact root above, preserve `stage2-fsync/diagnostic/`,
`stage2-fsync/throughput/`, both stages' `throughput-recheck/` directories,
`stage2-fsync/audit.json`, `stage2-fsync/rejection.json`, `independent-stage2-review.md`,
`independent-stage2-recheck-review.md` and the exact invocation script
`recheck_stage2.sh`. The first report records the original concern; the second
records the completed recheck and rejection. The reverted source patch remains
in the frozen artifact. The rejected fsync optimization is absent from retained source.

## Retained Redis script follow-up

`stage3-pcall` is retained as the narrow script optimization in
`16c2aca7e8d152fa73e55651d2c7ffefb623d5cd`, directly on stage 1; the rejected
fsync change is absent. Existing READ_LUA and TRANSACTION_LUA preflights use
protected STRLEN calls to validate row type and size, removing redundant TYPE
calls while retaining GET. WRONGTYPE maps to the existing corruption result;
other Redis errors propagate. Byte/key bounds, exact authority/entity
predicates, backend TIME and validation before the first write remain.
No feature, API, transaction kind, schema, cache, persistent object, mount mode
or filesystem behavior is added. The preflights add no Redis command permission
requirement and remove the TYPE requirement, so ACL failures need not occur at
the same point as before. The stage 1 lease-linearization caveat above remains.

The primary comparison is the adjacent fresh stage 1 control followed by the
candidate, using preserved native/fixture binaries and the existing
uninstrumented 2,000-file profile. Each cell has two opposite-order samples;
means equal medians. Every result is **preliminary, not acceptance evidence**.

| Phase | Workers | Fresh stage 1 entity / native files/s | Script change entity / native files/s | Raw entity change | E/N change |
| --- | ---: | ---: | ---: | ---: | ---: |
| WRITE | 16 | 291.483 / 384.523 | 307.098 / 365.896 | +5.357% | +10.720% |
| WRITE | 32 | 365.690 / 442.963 | 378.202 / 461.362 | +3.421% | −0.703% |
| READBACK | 16 | 454.478 / 659.512 | 497.940 / 638.892 | +9.563% | +13.099% |
| READBACK | 32 | 505.895 / 659.146 | 559.225 / 687.167 | +10.542% | +6.034% |

All eight paired raw entity rates and mean/p50/p95 latencies improve. The sole
p99 increase is WRITE32's first order, 120.441 to 120.527 ms (+0.072%). The
evidence supports retaining the small script change, with explicit limits:
WRITE32 loses relative to native in both orders (−0.531%/−0.770%). READBACK32's
second order also loses 7.578% relative to native, whose throughput rises
11.753% in that pair. There is no proven relative 32-worker WRITE improvement,
gap closure or native parity. Two samples do not establish statistical
significance, and native drift remains visible rather than normalized away.

The 200-file MONITOR diagnostic shows the same application script counts as
stage 1: WRITE 12.470/file (7.360 READ_LUA, 5.105 transaction and 0.005 SCAN),
READBACK 10.005/file (9.005 READ_LUA and 1.000 read-only transaction). External
commands/file are WRITE 12.470/12.475 and READBACK 10.010/10.010 at 16/32 workers;
the extra 0.005 entries are health PINGs. CREATE/WRITE/FSYNC/FLUSH remain
2/3/2/2 attempted metadata RTTs per callback; LOOKUP/GETATTR/inode OPEN remain
one, payload READ three, EOF READ one, and RELEASE zero. These counts prove
there is no application RTT reduction in this step.

Within those scripts, diagnostic TYPE calls fall from 134.215 to zero per WRITE
file and 101.030 to zero per READBACK file at both worker counts. STRLEN and GET
counts remain unchanged. The uninstrumented comparison separately records
inclusive EVAL/EVALSHA server service time, averaged over the two orders:

| Phase | Workers | Fresh stage 1 script ms/file | Script change ms/file | Change |
| --- | ---: | ---: | ---: | ---: |
| WRITE | 16 | 1.921113 | 1.639490 | −14.659% |
| WRITE | 32 | 1.879366 | 1.649739 | −12.218% |
| READBACK | 16 | 1.221938 | 0.953298 | −21.985% |
| READBACK | 32 | 1.240571 | 0.967892 | −21.980% |

In that larger workload, nested TYPE calls fall from 129.5215/WRITE file and
101.011/READBACK file to zero. Those nested commands' service time is already
included in EVAL/EVALSHA; adding it would double count. These are server service
measurements, not client elapsed time or a complete per-file latency budget.
STRLEN service time rises, so subtracting TYPE time alone would not predict
the measured inclusive savings.
The candidate's first READBACK repetitions each include two extra script calls
in the commandstats window (10.0025 versus 10.0015/file); their origin is not
attributed by the saved counters. Diagnostic per-callback counts stay unchanged.

The exact source passed the full 24-step gate, including the mandatory
unfiltered default workspace lib/bins tests, default/overlay strict Clippy and
real Redis/RustFS suites. Test counts and the five established overlay
exclusions are the same as in the correctness table above; this is not an
unfiltered overlay pass. The existing Redis bounds test also checks that a
STRLEN ACL denial remains a backend error with no metadata write. The release
build completed in 42m22s. Diagnostic, fresh-control and candidate groups all
complete with zero workload errors, byte-verified readback, successful minimal
smokes and clean mount/container/volume teardown. Source/binary manifests,
runner, image identities and workload traces match the intended comparison.

The artifact root contains `stage3-pcall/` with the frozen patch, gate proof,
release executable, `diagnostic/`, `parent-control-throughput/`, `throughput/`,
`audit.json` and `accepted-commit.json`. The independent fresh-control audit is
`independent-stage3-review.md`; `stage3_parent_control.sh` preserves the
exact control invocation. Source manifest SHA256:
`e95973d888f45cadb2fbbad9e50408bfcb79f836733c1571e7e97f0b7089f9d8`.
Release executable SHA256:
`40125fb22011d972bf0b7e1faba7417287e77cad01a88298af9569a64d9efdd1`.
The stage audit's comparison to the earlier stage 1 run is historical; use the
fresh control table here and independent report for this retention decision.

To reproduce the final throughput comparison with the preserved executable:

```bash
git worktree add --detach /tmp/brewfs-script-probe-repro 16c2aca
cd /tmp/brewfs-script-probe-repro
entity_art=/mnt/brewfs-data/artifacts/overlay-entity-rtt-20261020
sha256sum -c "$entity_art/stage3-pcall/binary.sha256"
sha256sum -c "$entity_art/baseline/binary.sha256"
unset ENTITY_MOUNT_SMOKE_ONLY BREWFS_ENTITY_DIAGNOSTICS
ENTITY_MOUNT_BINARY="$entity_art/stage3-pcall/brewfs" \
  ENTITY_MOUNT_NATIVE_BINARY="$entity_art/baseline/native-brewfs" \
  ENTITY_MOUNT_FIXTURE="$entity_art/baseline/packed_v3_snapshot_fixture" \
  ENTITY_MOUNT_ARTIFACT_ROOT="$entity_art/reproduce-stage3-throughput" \
  ENTITY_MOUNT_WORKERS=16,32 ENTITY_MOUNT_FILES=2000 \
  ENTITY_MOUNT_REPETITIONS=2 ENTITY_MOUNT_DIAGNOSTIC=0 \
  bash tools/perf/run_entity_mount_comparison.sh
```

For diagnostic counts, use a separate artifact root, 200 files, one repetition
and `ENTITY_MOUNT_DIAGNOSTIC=1`. Do not compare MONITOR timings to the throughput
table. The fresh parent is the exact stage 1 executable and matching checkout
used by `stage3_parent_control.sh`; a new parent run must keep its own artifacts.

## Remaining calls and deferred alternatives

The measured WRITE/READBACK application-command targets of at most 8/6 per file
remain unmet. CREATE needs its entity/permission read and allocation/publication
transaction. WRITE needs the read, durable Pending reservation before PUT and
publication after PUT. Upper payload READ needs the initial map batch, dependent
slice/owner/descriptor batch and post-transport validation. EOF uses one read;
FSYNC/FLUSH retain two after the rejected experiment. Static review identified
no additional demonstrated redundant warm-path request removable by batching
known keys within this scope.

Replacing READ_LUA's GET loop with MGET was statically evaluated and deferred;
it is unimplemented and unmeasured. GET and MGET have different command ACL
requirements, and separate selectors can authorize each GET without one
selector authorizing the complete MGET. MGET also materializes an aggregate
intermediate reply instead of one row reply at a time. These compatibility and
memory-shape changes exceed this narrowly scoped candidate; the assessment
does not establish that MGET is universally infeasible.

Durable Pending reservation before PUT, publication after PUT, dependent
slice/descriptor reads, HEAD+GET authentication and payload post-transport
validation remain. Generic ranged object reads can return partial results, so
requesting one extra byte cannot replace HEAD's exact-length proof under the
existing adapter contract. No reservation queue/credits, cross-operation
expectation cache or handle cache is introduced. Earlier broader candidates
were reverted before measurement; their interrupted build has no performance
result. See artifact `RUNNING.md` for preserved history.

The architecture and canonical specification are
[workspace-overlay.md](../../architecture/workspace-overlay.md) and
[overlay-workspace.md](../specs/overlay-workspace.md). The preceding experiment
is [the batching checkpoint](2026-10-20-overlay-entity-batching.md).
