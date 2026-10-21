# Entity/native critical-path cost model, 2026-10-21

The v3 diagnostic model is complete. The next selected experiment is to spawn
ordinary mutable FUSE READ through the existing owned child-task path;
implementation and performance validation have not started. All performance
rows remain diagnostic/preliminary, not acceptance results or an optimization
win.

## Scope and method

Retained implementation is `16c2aca`; measurement base HEAD is `3cef527` on
`codex/overlay-entity-rtt-20261020`. The current Rust changes add opt-in
diagnostics to retained entity and native paths. The rejected CREATE and
interrupted WRITE call-fusion candidates are absent.

The comparison uses native and entity/overlay-packed on real Redis + RustFS:
32 workers, 2,000 files × 16 KiB, per-file fsync in WRITE, byte-verified
READBACK, two opposite arm orders. The same release executable runs both arms
with diagnostics off for the control, then on for the diagnostic group.
MONITOR is disabled. No overlay-native substitute, additional workload,
admission, crash, conversion or cutover work is included.

Core instrumentation records actual Redis request attempts, entity/native
object intervals, native fsync/flush waits and inherited owned-task polls.
Shared FUSE hooks record ingress, lane queue, executor dispatch, callbacks,
handler polls and reply boundaries. The scanner times its existing operations
without adding filesystem calls. Raw buffers are bounded to 1,048,576 core and
524,288 FUSE events; zero drops and complete attribution are required.

Per-file wall categories must be disjoint and sum to the measured per-file
elapsed mean. Keep observed overlap, unassigned work, asynchronous tails and
kernel/untraced residuals visible. CPU and server service belong in separate
resource tables, not as additional wall categories. Poll wall is not CPU;
summed concurrent file latency is not occupancy. Redis RTT includes server
queueing, execution, transport and client/runtime waits. Subtracting Lua or
all-command service does not establish pure network/client time. Compare
diagnostic rates against their control and report instrumentation perturbation.

## Validation and executable provenance

The frozen `critical-path-v2` Rust source passed all **24 gate commands** and
final source/HEAD/index verification. Default workspace tests passed **1,179**
library tests with 225 ignored and none filtered. Overlay tests passed
**2,338 library + 9 binary tests**, with 405 ignored and the same five explicit
pre-existing exclusions. Real-service filters passed **7 packed-lower + 104
entity tests**, with no owned containers or volumes left. Both strict Clippy
gates, feature/runtime checks, shell/report checks and formatting passed.
Release build completed in **47:38.49**, exit 0.

The five overlay exclusions are preserved, not presented as new regressions:

- `g13a_kv_released_lease_is_not_an_expired_recovery_root`
- `kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`
- `migration_marker_fences_entity_and_hot_native_packets_without_writes`
- `source_mutation_replacement_symlink_and_limits_fail_closed`
- `source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`

The vendored asyncfuse dependency also passed a separate sequential supplement:

| Supplemental check | Result |
| --- | --- |
| Vendor formatting | Passed |
| Tokio library tests | 47 passed; 0 failed; 0 ignored; 0 filtered |
| Tokio runtime compile check | Passed |
| io-uring runtime compile check | Passed |
| async-io runtime compile check | Passed |
| io-uring library tests | 89 passed; 0 failed; 15 ignored; 0 filtered |
| io-uring library tests, diagnostics enabled | 89 passed; 0 failed; 15 ignored; 0 filtered |

All **seven supplemental commands** exited 0; frozen source verification
passed afterward. The Tokio test build emitted four dead-code warnings;
this supplement is not described as a warning-free Clippy run. The 15 ignored
io-uring cases were not executed by these commands.

V3 changes only `tools/perf/run_entity_mount_comparison.sh`. Its frozen input
comparison proves identical built Rust inputs and reuses the exact v2 binary,
SHA-256 `84ca682f105ae30c943e873a8ed9ff4658245e357ae9fbdd0659aeffa5f176cc`.
The runner shell check and `git diff --check` passed. This is reuse of the
completed Rust gate/build, not a claim of a second release build or full gate.

Evidence: [v2 gate proof](/mnt/brewfs-data/artifacts/overlay-create-write-rtt-20261021/critical-path-v2/gate-proof.json),
[supplemental results](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/supplemental-gate/results.tsv),
[supplemental commands](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/supplemental-gate/commands.txt),
[v3 binary reuse proof](/mnt/brewfs-data/artifacts/overlay-create-write-rtt-20261021/critical-path-v3/binary-reuse-proof.json).

## Why v2 native READBACK cannot explain its control

V2 completed its execution/identity/byte/cleanup audit with zero trace-buffer
drops, but large trace dumps between WRITE and READBACK changed native metadata
cache state. Diagnostic WRITE-end→READBACK-start gaps were **12.293523426 s**
and **12.857427229 s** in the two native orders. The active native CLI path uses
`MetaClientConfig::default()` → `effective_ttl()` → the 10-second fallback,
then `InodeCache::new(...).time_to_idle(...)`. The Redis-specific 500 ms factory
is not selected by this mount path. Both observed gaps exceed the active idle
expiry, and native READBACK metadata traffic changed in both orders:

| Native READBACK counter | V2 control calls | V2 diagnostic calls |
| --- | ---: | ---: |
| EVALSHA | 1 | 2,022 |
| HGET | 1 | 2,022 |
| GET | 4,000 | 4,020 |
| EXEC | 2,000 | 2,000 |
| LRANGE | 2,000 | 2,000 |

This establishes changed metadata work, not just timer cost. Do not use the
v2 native READBACK spans to partition the control's latency or as a matched
final cost model. Redis CPU windows also spanned trace serialization and were
much wider than scanner phases; their low sampled-core ratios cannot establish
low Redis utilization during the workload. V2 evidence remains preserved in
the [independent audit](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/audit-v2.md)
and [raw counter/gap evidence](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/readback-commandstats-v2.json).

V3 moves large trace serialization outside the complete WRITE/READBACK pair,
while keeping per-phase Redis/CPU snapshots. The independent audit passes:
eight phases per group, both orders, two smokes, six clean mounts, identical
recorded Rust/runner/backend/binary identities, zero scanner/Redis errors and
2,000 byte-verified files per phase. All captures have zero dropped events;
maximum pair captures contain 421,348 core and 437,301 FUSE events. The model
attributes all 2,000 file timelines in every phase with zero boundary gaps,
conflicts, errors or warnings; unassigned asynchronous/maintenance events
remain explicit.

Native WRITE→READBACK gaps are now **1.504379 s / 1.480410 s**, below the active
10-second idle expiry. Native READBACK has **zero foreground LOOKUP RTTs** in
both orders. GET/EXEC/LRANGE totals match the controls at 4,000/2,000/2,000;
the v2 refresh wave is absent. The first-order wider counter window has one
extra EVALSHA/HGET versus control; aggregate counters do not identify its
source. It is retained as a reviewed discrepancy, not silently attributed.
Shared before/after trace files cover both phases; the model deduplicates them
and uses raw timestamps/request identities to separate phases.

Evidence: [v3 independent audit and per-order measurements](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/audit-v3.md),
[full per-file model](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/cost-model-v3.json),
and [per-opcode RTT evidence](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/v3-opcode-rtt.json).

## V3 measured throughput and latency

Each row combines two complete opposite-order samples for compact reporting.
Rates are their two-sample median; latencies are actual equal-file-weighted
means, not `workers / files_per_sec`. Per-order rates, p95/p99 and identities
remain in the linked independent audit and raw artifacts.

| Group, preliminary | Phase | Native files/s | Entity files/s | Entity/native | Native mean ms | Entity mean ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Uninstrumented control | WRITE | 460.580 | 369.698 | 0.802679 | 68.859145 | 85.902617 |
| Uninstrumented control | READBACK | 701.852 | 536.095 | 0.763830 | 45.087641 | 59.063637 |
| Diagnostic | WRITE | 450.798 | 355.638 | 0.788906 | 70.278003 | 89.271056 |
| Diagnostic | READBACK | 676.773 | 523.561 | 0.773614 | 46.727802 | 60.479103 |

The control-to-diagnostic rate changes vary by order: native WRITE
−1.288%/−2.998%, entity WRITE −7.228%/−0.229%, native READBACK
−5.251%/−1.809%, entity READBACK −2.950%/−1.712%. These combine observer effects
and ordinary variation between sequential groups; they are not timer-only
causal overhead estimates or optimization deltas. Partition diagnostic means
only; do not scale their categories to the control means.

## Observed per-file wall partition

Values are mean ms/file across the two diagnostic orders. Categories are
disjoint, using documented interval unions and overlap attribution. Displayed
values can differ from totals by rounding. Client poll wall includes CPU,
descheduling and synchronous waits; CPU is reported separately below.

| Wall category, ms/file | WRITE native | WRITE entity | READBACK native | READBACK entity |
| --- | ---: | ---: | ---: | ---: |
| Metadata RTT pending, including server | 3.816958 | 32.193011 | 0.667561 | 15.606465 |
| Object PUT pending | 34.517819 | 36.354009 | 0 | 0 |
| Object GET pending | 0 | 0 | 2.933053 | 1.713741 |
| Object HEAD pending | 0 | 0 | 0 | 3.079362 |
| Client poll wall | 2.633280 | 8.240489 | 1.427818 | 6.511108 |
| FUSE lane queue | 1.554809 | 0.758636 | 4.227663 | 23.450152 |
| Other FUSE dispatch/reply | 4.973153 | 3.652251 | 2.819808 | 3.113177 |
| Explicit observed overlaps | 0.046091 | 0.052901 | 0.001105 | 0.026062 |
| Barrier/callback/handler remainder | 0.110920 | 0.121448 | 0.031430 | 0.086881 |
| Kernel/untraced syscall residual | 22.448730 | 7.721964 | 34.481776 | 6.741168 |
| Workload-local gaps | 0.176244 | 0.176346 | 0.137588 | 0.150987 |
| **Measured total** | **70.278003** | **89.271056** | **46.727802** | **60.479103** |

WRITE shows substantially more entity metadata wait and poll wall, while PUT
pending time is similar. READBACK exposes a concrete queueing mechanism:
**23.004402 of entity's 23.450152 ms/file lane queue (98.10%)** overlaps another
READ handler occupying the same lane. Native has **3.938746 of 4.227663
ms/file (93.17%)**. The ordinary Tokio/io-uring worker awaits READ inline, so
later requests on that lane cannot dequeue while it waits. This is measured
head-of-line blocking, not a guessed runtime queue.

The larger native syscall residual is unresolved. The observation boundary
does not measure kernel queue time before userspace ingress or all workload
resumption delays. Do not label it kernel service, Python/GIL delay, FUSE
round-trip overhead or any other specific cause without further evidence.

The native OPEN residual occurs mainly **between** FUSE requests within the
same syscall: WRITE 12.172/12.918 ms and READBACK 21.052/22.097 ms in the two
orders. After the last reply, the corresponding residual is only
0.313/0.265 ms and 0.114/0.123 ms. This narrows its location without proving
kernel, scheduler or GIL causality. See the
[conserved syscall decomposition](/mnt/brewfs-data/artifacts/overlay-critical-path-20261021/v3-syscall-residual-decomposition-v2.json).

The exclusive fsync-barrier remainder inside the combined remainder row is
0.05356 ms/file native and 0.05469 entity for WRITE. Native's full fsync drain
is about 38.06 ms, including PUT and metadata already counted above. Entity's
full fsync and flush spans are about 6.04 and 6.01 ms, also including metadata.
They must not be added again as independent barrier waits.

## Requests and separate resource ledger

| Phase | Arm | Foreground logical Redis RTT/file | Wider-window EVAL/EVALSHA calls/file | Payload object operations/file |
| --- | --- | ---: | ---: | --- |
| WRITE | Native | 4 | 2.011 | 1 PUT |
| WRITE | Entity | 12 | 12.047 | 1 PUT |
| READBACK | Native | 2 | 0.0005 | 1 GET |
| READBACK | Entity | 10 nominal | 10.002 | 1 HEAD + 1 GET |

Entity WRITE uses 7 read + 3 mutation + 2 read-only transaction requests.
READBACK is 10.001 RTT/file in the first order and 10 in the second: two
additional attributed requests over 2,000 first-order files remain counted.
Native READBACK has one OPEN and one READ request/file. A native pipeline is
one observed RTT attempt and can contain several commands; its command count
is not its RTT count. Lua also executes nested commands. Wider-window script
counts include setup/maintenance, so they need not equal foreground counts.
No-MONITOR commandstats cannot establish exact per-file external command
boundaries; raw request hooks establish the RTT counts above.

These are mean resource ms/file across both diagnostic orders, **not additive
wall categories**. Poll CPU is part of mount CPU; Lua service is part of Redis
work already included within client RTT elapsed.

| Phase | Arm | Mount CPU | Matched handler/task poll CPU | Redis CPU | Inclusive Lua service | Workload CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| WRITE | Native | 4.965000 | 1.685930 | 0.532560 | 0.322355 | 0.764757 |
| WRITE | Entity | 6.787500 | 4.126252 | 1.894813 | 1.677913 | 0.662169 |
| READBACK | Native | 3.940000 | 1.191174 | 0.172392 | 0.000030 | 0.662190 |
| READBACK | Entity | 6.720000 | 4.198513 | 1.257537 | 0.975958 | 0.623977 |

Mount/Redis snapshots bracket wider windows than scanner phases and include
unassigned/background work; the ordinary control has no CPU measurements.
Redis sample windows are 3.636–6.547 s versus 2.933–5.709 s scanner phases.
Process counters have 10 ms tick resolution. Native Lua service excludes its
direct/pipelined commands; summing every command's service double-counts nested
work. These averages cannot rule out server burst contention or prove a pure
network/client bottleneck. Object spans combine transport and object service,
without a separate RustFS server-service measurement.

## Selected next experiment; no implementation or measured win

The next candidate changes the shared ordinary mutable worker to spawn READ
through the **same existing owned child-task path used by other operations**.
It targets the demonstrated inline lane blockage, retaining ownership/join,
request/reply memory, inflight limits and existing filesystem ordering. It
does not change metadata format, transactions, object transport, fsync or the
specialized read-only lane. Both native and entity scheduling are affected.

A conditional sensitivity calculation gives scale, not a forecast established
by an experiment. If 25–50% of entity's 23.450152 ms/file lane queue is removed
without moving that time elsewhere or changing other costs, diagnostic mean
READBACK falls from 60.479103 ms to 54.616565–48.754027 ms, corresponding to
approximately **11–24% higher throughput** at unchanged effective concurrency.
WRITE contains no measured inline READ blocking in this workload, so its
direct improvement is expected to be roughly zero. Added task overhead,
backend concurrency and shifted waits can invalidate either expectation;
native may improve too, so this is not a predicted entity/native ratio gain.

No source implementation, candidate validation or candidate benchmark has run.
The result must be established by the correctness gates and matched 16/32
worker comparisons in both orders, including tails, errors, bytes, drain and
cleanup. Retain only a supported improvement; otherwise revert the candidate
and preserve its negative evidence.

## Earlier experiments remain accurately classified

The one-call CREATE candidate passed all 24 gates and completed its matched
comparison, reduced CREATE RTT/callback **2→1**, and reduced the prior 200-file
MONITOR diagnostic's WRITE scripts/file **12.470→11.470**. It nevertheless
failed consistent throughput/tail retention: WRITE changes were **+2.849% /
−1.010%** at 16 workers and **−1.871% / −8.567%** at 32, ordered native→entity /
entity→native. The three losses remained losses normalized to native, with
WRITE p99 increases **9.375%, 9.122%, 10.312%**. It was exactly reverted; no
CREATE win shipped. The earlier `stage2-fsync` rejection remains separate.

The independent WRITE two-call v2 candidate passed all 24 gates and frozen
source verification, but the owner stopped its release build before any
diagnostic or performance comparison. No completed candidate executable/hash
or WRITE performance result exists. Its patch was preserved and exactly
reverted. This is an interrupted experiment, not a correctness failure or a
measured performance rejection.

Evidence: [CREATE independent review](/mnt/brewfs-data/artifacts/overlay-create-write-rtt-20261021/create-onecall-v1/independent-measurement-review.md)
and [WRITE interruption record](/mnt/brewfs-data/artifacts/overlay-create-write-rtt-20261021/write-twocall-v2/interrupted.md).
