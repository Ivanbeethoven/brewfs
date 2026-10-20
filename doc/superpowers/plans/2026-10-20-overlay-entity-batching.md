# Entity request batching: baseline and implementation checkpoint

Date: 2026-10-20. Branch: `codex/overlay-entity-namespace-20261019`.
**Status: refactor and correctness gates in progress. Three intermediate debug
attempts completed zero workload phases after stack-overflow aborts. Corrected
candidate measurements are pending; no speedup or performance acceptance is
claimed.**

The source checkpoint is the working tree after `7dddbef` (WIP instrumentation,
lower memo and create allocation) and `d6a8b77` (canonical docs merge). The
batching source is now frozen for the ongoing gate; its final executable and
matched measurement evidence are pending. The maintained contracts are
[overlay-workspace](../specs/overlay-workspace.md) and
[workspace architecture](../../architecture/workspace-overlay.md). The earlier
[mount experiment](2026-10-20-overlay-entity-mount.md) remains a separate result.

## Owner-requested WIP merge checkpoint

The owner requested committing and merging the current WIP before a separate
structural refactor. The remote `main` branch exists and GitHub identifies it
as the default, so it is the requested merge target rather than the historical
integration branch. This checkpoint is not performance acceptance.

The current gate passed formatting and the overlay workspace check. Seven
packed-lower tests passed against real Redis/RustFS. The entity suite then
reported 97 passed and four failures: the memory and Redis variants of
`batched_file_io_has_one_publication_and_preserves_bytes` and
`fragmented_write_splits_publication_without_reupload_or_lost_refs` all hit
the test wrapper's `settled file operation unexpectedly scanned metadata`
assertion. Those failures have not yet been diagnosed or fixed. Subsequent
gate steps and candidate measurements have not run. Their completion belongs
to the new branch after the requested WIP merge.

## Scope and evidence boundary

Use the existing WRITE/READBACK workload with Redis and real RustFS for native
and entity over packed-v3. `overlay-native` remains skipped: an entity-compatible
frozen-native lower adapter, binding and mount bootstrap are missing. No arm
substitution, new scenario, admission framework, conversion, cutover or crash
work is added. Production entity admission stays disabled; only the existing
fresh-fixture experimental opt-in selects the entity FUSE path. The integration
branch and old-format defaults remain separate.

This workload creates new upper files and reads those bytes back. It measures
neither packed-lower payload reads nor lower copy-up. The 200-file instrumented
profile is diagnostic; MONITOR and timers perturb its timings. Only the separate
uninstrumented profile supplies the preliminary throughput baseline below.

## Frozen baseline provenance

Artifact root:
`/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/baseline/`.
The authoritative summaries are `diagnostic-evidence.md`,
`throughput-audit.json` and `build-provenance.json` under that root; raw phase
artifacts and exact runner sources remain alongside them.

| Item | Recorded identity |
| --- | --- |
| Frozen checkout | `/mnt/brewfs-data/src/brewfs-entity-diagnostic-baseline` |
| Base revision | `cfa000af39414e14f0e0149181b92da497c1a8fb`, plus preserved instrumentation-only patch |
| Rust source manifest | 721 files; `rust-source-sha256.json`, SHA256 `2b2b2482c8f7226677aab3db9b0e2a97aa584de5374ab7918d6a4477e9eedd05` |
| Preserved executable | `baseline/brewfs`, SHA256 `290477da246979bdd1a1417f285ddcb20e2e01fb8354428da42a273b45ad618a` |
| Compiler | rustc 1.99.0 (`b940084d7`, 2026-09-28); cargo 1.99.0 (`5f94df478`, 2026-08-27) |
| Host | `brewfs-singapore-v2`, Linux `6.8.0-139-generic` |
| Release build | Exit 0, 38m 56s; `CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0`, `--features workspace-overlay --bin brewfs` |
| Candidate executable/source hash | Pending; do not reuse the baseline identity for the working candidate |

The build used `CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs/target`.
No Cargo build or test ran concurrently with the measurements. Both compared
arms used the same preserved executable. The throughput run disabled diagnostics
on that executable; the diagnostic run enabled the entity instrumentation and
captured both arms' Redis traffic.

The first diagnostic group was
`diagnostic/entity-mount-20261020T084132Z-74cfd662-3133-48fd-8d1d-a3743f0dba2f`.
Its 1-worker phases completed, but the group is **incomplete overall**: after
the 16-worker WRITE workload finished 200 files without errors, the runner
killed the MONITOR reader after five seconds of post-workload draining. That
capture has 166,928 lines, no end marker, and `signal_stop=true`. Its partial
command counts are excluded. Increasing only the runner drain allowance to
30 seconds yielded the complete 16/32-worker retry:
`diagnostic-retry/entity-mount-20261020T084500Z-1af620a8-3ab1-4eec-9df2-c3ccaab3453d`.
Rust source and executable hashes did not change. The two runner versions are
preserved in their groups, and the interrupted group also cleaned up.

## Preliminary uninstrumented baseline

Group:
`throughput/entity-mount-20261020T084745Z-f7cd3d4e-1a50-4d41-9388-420da8cefbd8`.
Each cell uses two reversed-order samples: native then overlay, followed by
overlay then native. Each phase processes 2,000 files of 16 KiB, with fsync per
file, 4 MiB chunks, direct I/O, payload read cache and prefetch disabled. Values
are medians in files/s; ratios divide the entity median by the native median.

| Status | Workers | WRITE native | WRITE entity | Ratio | READBACK native | READBACK entity | Ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Preliminary baseline | 16 | 358.388200 | 114.513479 | 0.319524 | 637.358680 | 176.611681 | 0.277099 |
| Preliminary baseline | 32 | 448.404334 | 140.819408 | 0.314046 | 685.946272 | 205.422030 | 0.299472 |

All 16 available phases completed 2,000 files with zero errors and matching
workload trace hashes. Two smokes passed; all ten mount processes exited and
unmounted without forced kills. Teardown left zero owned containers, volumes
or FUSE mounts. The independent audit matched all 721 Rust files and both
measured executable hashes to the frozen build. Raw samples and counter deltas
are in `throughput-audit.json`. These later baseline numbers do not replace the
earlier mount checkpoint's distinct binary/sample provenance.

## Diagnostic Redis commands and request/reply attempts

The following 200-file observations use completed 1-worker phases from the
initial group and completed 16/32 phases from its retry. All these included
captures have both markers, zero malformed lines, no unknown script hashes,
zero workload errors and zero dropped entity timer registrations. Setup and
background lease/health work are inside the windows and divided by 200 files.

**External commands are not network round trips.** Native sends several
commands in MULTI/EXEC pipelines. Entity RTT spans count awaited backend
request/reply attempts, excluding the packed catalog's separate connection.
Lua-nested commands are server work and are excluded from external totals.

| Diagnostic commands/file | 1 worker | 16 workers | 32 workers |
| --- | ---: | ---: | ---: |
| Native WRITE, all external commands | 7.120 | 7.110 | 7.110 |
| Native WRITE, EVALSHA only | 2.110 | 2.110 | 2.110 |
| Entity WRITE, all external commands | 113.810 | 113.770 | 114.400 |
| Entity WRITE, EVALSHA only | 113.775 | 113.760 | 114.390 |
| Native READBACK, all external commands | 5.005 | 5.000 | 5.000 |
| Native READBACK, EVALSHA only | 0.000 | 0.000 | 0.000 |
| Entity READBACK, all external commands | 75.090 | 75.880 | 76.640 |
| Entity READBACK, EVALSHA only | 75.065 | 75.870 | 76.630 |

Native WRITE includes 1.105 CREATE_ENTRY_LUA, 1.000 WRITE_SLICE_LUA and 0.005
LOOKUP_WITH_ATTR_LUA calls per file; the excess creates are directory setup.
The earlier “8 backend requests/file” observation is not the counting unit of
this matched capture and must not be substituted for its external-command
totals or described as a measured native RTT count.

At 16/32 workers, entity WRITE spends 84.895/85.300 READ_LUA calls and
11.460/11.505 TRANSACTION_LUA calls per file. Packed-owner checks add
8.700/8.790 BOUNDED_TIMED_GET_LUA plus 8.700/8.790 CAS_LUA calls. SCAN_LUA
is 0.005/file and background PING is 0.010/file. READBACK has
54.600/55.115 READ_LUA, 9.070/9.115 transactions and 12.200/12.400 combined
packed-owner catalog calls per file.

| Diagnostic entity RTTs/file, by FUSE phase/scope | 1 worker | 16 workers | 32 workers |
| --- | ---: | ---: | ---: |
| WRITE phase, all entity connection attempts | 96.375 | 96.360 | 96.810 |
| WRITE phase, CREATE scope | 15.000 | 15.000 | 15.000 |
| WRITE phase, WRITE scope | 25.000 | 25.000 | 25.000 |
| WRITE phase, FSYNC scope | 12.000 | 12.000 | 12.000 |
| WRITE phase, FLUSH scope | 12.000 | 12.000 | 12.000 |
| WRITE phase, all LOOKUP scopes | 30.775 | 30.775 | 31.225 |
| READBACK phase, all entity connection attempts | 63.065 | 63.670 | 64.230 |
| READBACK phase, OPEN scope | 6.000 | 6.060 | 6.000 |
| READBACK phase, all READ scopes | 15.025 | 15.000 | 15.040 |
| READBACK phase, FLUSH scope | 12.000 | 12.000 | 12.000 |
| READBACK phase, all LOOKUP scopes | 30.000 | 30.570 | 31.150 |

WRITE has one CREATE, WRITE, FSYNC and FLUSH per file; the diagnostic windows
also contain about 3.21 FUSE LOOKUPs/file. The scoped rows do not include every
setup/background scope and must not be summed with the aggregate row.
READBACK requests payload plus EOF, so multiple READ scopes are expected.
RELEASE itself has zero entity RTTs here; preceding FUSE FLUSH still costs 12.
Native per-operation RTT spans were not instrumented.

## Measured cause and limits of attribution

The entity baseline already has no mount-wide mutation gate. Namespace locks
include affected parent/inode locks; data locks are per inode. Their elapsed
waits are negligible in this workload. The dominant measured expansion is
repeated metadata request/validation work, not evidence of a resurrected global
gate or an object-PUT-only bottleneck.

| Diagnostic WRITE elapsed span, ms/file | 16 workers | 32 workers |
| --- | ---: | ---: |
| Namespace lock wait | 0.053772 | 0.043291 |
| Data inode lock wait | 0.014725 | 0.010794 |
| Entity READ request/reply | 225.527938 | 448.861567 |
| Mutating commit request/reply | 14.463324 | 26.920187 |
| Read-only commit request/reply | 16.169300 | 32.118676 |
| Allocation request/reply | 6.202592 | 12.805236 |
| Packed-owner validation | 48.618550 | 94.634741 |
| Object PUT | 17.270049 | 19.573153 |
| All FUSE LOOKUP scopes | 119.925056 | 238.215302 |
| FUSE CREATE | 56.668005 | 107.136639 |
| FUSE WRITE | 91.564071 | 162.161029 |
| FUSE FSYNC | 33.561168 | 68.130866 |
| FUSE FLUSH | 32.726674 | 66.332402 |

These cumulative elapsed spans overlap across nesting and concurrency. They
are not an exclusive time budget, CPU time or mount occupancy. The near
doubling of metadata wait at 32 workers contrasts with PUT's 17.27 to 19.57
ms/file, while request counts remain roughly constant per file.

Separately sampled mount CPU is 19.10/18.30 ms/file for entity versus
5.60/5.15 for native at 16/32 workers. Entity Redis CPU is
22.505425/22.187995 ms/file. Using their actual sampling windows gives entity
mount CPU 0.663066/0.640485 CPU-s/s and Redis 0.720193/0.699686 CPU-s/s.
Diagnostic inclusive EVAL service is 19.890765/20.000115 ms/file, roughly
174.85 us/call; it includes nested Lua work and MONITOR overhead. The matched
uninstrumented baseline instead records median inclusive EVAL service
5.30031575/5.02522125 ms/file, about 48.4/45.8 us/call. Diagnostic service time
must not be used to infer uninstrumented server capacity. Per-operation CPU and
an exclusive client/network/server latency partition were not measured.

Source inspection explains the repeated calls: authority and entity rows were
read separately; path operations repeated ancestor/ACL resolution; data writes
separately allocated, registered Pending, marked Uploaded and published; fsync
and flush entered recovery before establishing that no truncate intent existed.
Packed fallback also repeatedly validated the durable owner on its catalog
connection. The old packed index reader already had a 4 MiB cache: these counts
do not prove an object GET for every lower key. Upper rows/tombstones and
new-inode bounds already avoided inappropriate lower fallback. A copied-up
inode alone does not remove inherited lower xattrs/chunks.

## Intermediate cache/create debug checkpoint: incomplete

An intermediate checkpoint attempted to measure the lower-cache,
single-authority-read and atomic-create changes before the full batching
continuation. Its audit is
`/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/checkpoint-cache-create/diagnostic-audit.json`;
`build-provenance.json`, `rust-source-sha256.json`, the preserved `brewfs`
executable and each attempt's raw logs remain in that directory.

| Intermediate checkpoint identity | Value |
| --- | --- |
| Checkout | `/mnt/brewfs-data/src/brewfs-entity-checkpoint` |
| Revision | `d6a8b775ec292280113fc19d175020695b586200` |
| Source | 723 Rust files, matching that HEAD; manifest SHA256 `8f72b4c0571d9013441a7cc884e9dde5b97d887bf6d42b22d65e2ca956931a80` |
| Executable SHA256 | `43e013527ea11abcea397336897f80ae95d17c8f7c842a9b63affcdc83d646d3` |
| Build | Unoptimized dev, `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4`; exit 0 in 9m 06s |
| Intended workload | Diagnostic only: 200 files, 16 workers, one repetition, WRITE and READBACK for each available arm |
| Native executable | Preserved baseline release binary; debug timings were never eligible for comparison with it |

All three attempts used that same intermediate entity executable without source
changes between retries. Native's minimal smoke passed, but entity initialization
aborted before the overlay smoke workload could run. Every group has an empty
`rows` array, **0/4 available phase cells**, null throughput medians, runner
exit 1 and overlay process exit 134. The two missing-native-lower cells remained
explicitly skipped. There are no usable per-file command counts or candidate
throughput samples from this checkpoint.

| Diagnostic attempt | Main-thread stack limit, KiB | Observed abort | Completed phase cells |
| --- | ---: | --- | ---: |
| `diagnostic/entity-mount-20261020T090336Z-53d6515a-d4c1-4f86-9ef0-048a310c8915` | 8,192 | Main-thread stack overflow | 0/4 |
| `diagnostic-stack64/entity-mount-20261020T090529Z-f9b88544-8705-4db8-84b3-0ccad1e41c75` | 65,536 | Main-thread stack overflow | 0/4 |
| `diagnostic-stack512/entity-mount-20261020T090843Z-0225d6d3-4d5c-4a47-90c3-15273e608d49` | 524,288 | Tokio worker stack overflow after main initialization | 0/4 |

Compose teardown succeeded for each attempt, but the aborted mounts were **not
clean process exits**. The 512 MiB-stack attempt initially left one disconnected
owned FUSE mount: `mountpoint -q` missed it, while `/proc` mount information
still showed it. A subsequent exact `fusermount3 -u` removed it; that group's
`supplemental-unmount-proof.json` preserves the original cleanup evidence and
records an empty remaining-owned-mount list. The final audit records zero owned
containers, volumes or FUSE mounts for all three attempts. Cleanup does not
convert these failed/incomplete attempts into passing smokes or measurements.

The working source now changes the memo loader API to a lazy `FnOnce` factory
and boxes the selected asynchronous fill. Cache hits therefore need not construct
the loader, and nested inode/cold/directory loader future state is kept out of
the caller's inline future and Moka initializer. Source tests cover loader-state
size and construction on a hit. This correction is absent from the intermediate
binary above and **has not yet produced a measured mounted candidate**. The
observed aborts establish an intermediate debug execution failure, not a measured
throughput regression or proof that the corrected runtime succeeds. No more
stack-limit retries are counted as performance evidence.

## Current refactor and retained invariants

The first WIP introduced a 64 MiB weighted lower metadata memo, coalesced async
fills/root-child prefetch, session-bound local admission for owner-safe cache
hits, one-snapshot lifecycle authority, and inode allocation inside create's
publication transaction. The current continuation removes duplicate preparation
and publication work rather than adding another parallel authority cache.

| Candidate change | Removed ordinary-path work | Retained checks and limits |
| --- | --- | --- |
| Shared operation `Prepared` snapshot for namespace and data | Duplicate data `ReadSet`; separate authority/entity reads for known keys | Fresh exact observations, backend time, row/byte bounds, conflicts rebuilt from fresh state |
| Bounded parent/dentry hints | Serial discovery reads when relevant keys are already known | 4,096 entries per hint cache; hints nominate keys only, never authorize identity/permissions |
| Retained path-open preparation | Second ancestor/name walk after target lock acquisition | Exact discovery predicates survive lock wait; changed name/ACL produces conflict |
| Same-operation attributes | Extra attr request after truncate or nonexclusive create fallback | Return only attributes established by the successful operation; no cross-operation permission cache |
| Batched slice reservation | Separate allocator RPC plus per-slice Pending registration | One transaction allocates exact IDs and Pending owners before PUT; all validation before first write |
| Durable PUT receipt | Separate hot-path Uploaded commit/read | Receipt retains exact Pending bytes/session admission; unknown reservation prevents PUT |
| Group publication | Repeated per-chunk publication and independent refcount planning | Maps/descriptors/inode and aggregated per-slice membership deltas commit together; underflow/deleting state rejected |
| No-intent fsync/I/O path | Unconditional empty truncate-recovery reads | Initial snapshot observes intent; fsync retains FIFO inode lock, sticky errors and final read-only authority validation |

Same-inode serialization, append offsets, lower-visible-size masking, O_EXCL
visibility, noatime, lower ownership and upload-before-publication remain
required. Unrelated inodes do not share a mutation mutex or mutable head.
Known conflicts may refresh exact authority/rows; uncertain transport or commit
outcomes are not blindly replayed. A multi-chunk operation can report only a
prefix whose metadata publication was acknowledged. Pending/unpublished objects
retain recovery ownership, and read pins/retirement holds are unchanged.

The implementation now preflights each publication's transaction and stored-row
byte bounds. If a group exceeds a known preparation bound, it drops that view,
halves the group and reads only the selected rows again. Initial chunk snapshots
also shrink on a known aggregate read bound. This retains support for writes
whose individual chunks fit but whose combined ownership plan does not. Each
acknowledged group contributes to the returned prefix; no uncertain commit is
replayed or split. Uploads remain protected by their Pending records until
publication. The final source is frozen, but gate/runtime results for this
change remain pending.

Reads retain an additional bounded fallback. If eager fetching of requested
chunk maps exceeds a known aggregate metadata bound, the reader refreshes a
minimal inode/authority snapshot, clips the request to the current visible size,
then fetches only visible chunk maps. A shrink can expose EOF before its tail
cleanup completes; large hidden tail maps must not turn that EOF read into an
error. This fallback does not ignore malformed rows, arbitrary backend failures
or genuine visible-range bounds.

The new source tests are **not yet run** at this checkpoint:

- `eof_after_visible_cut_ignores_oversized_invisible_tail_metadata` uses memory
  metadata and rejects object I/O; it installs the normal visible shrink cut
  with oversized aggregate tail metadata still present and expects EOF.
- `fragmented_write_splits_publication_without_reupload_or_lost_refs` uses
  memory metadata plus the real S3 adapter against RustFS.
- `redis_fragmented_write_splits_publication_without_reupload_or_lost_refs`
  uses real Redis plus RustFS. Both fragmented-publication tests exercise the
  same grouping path and check published bytes, upload counts and ownership.

The two RustFS tests are ignored by default and require the existing configured
real-service gate with `--include-ignored`. A test present in the frozen source
is not passing evidence; no new crash scenario is introduced here.

The unused standalone `EntityBackend::allocate`, `allocate_upload`,
`register_upload`, `uploaded` and singleton `reserve_upload` APIs, including the
Redis allocation mode, are deleted. Their tests now use batch reservation and
receipt publication. The asynchronous membership-planning wrapper remains
because truncate/discard/orphan cleanup uses it; it delegates to the same pure
planner. Actual truncate intents still run the bounded recovery walker. No new
crash-injection or takeover scenario is added.

### Removal list for the final delivery report

These describe the frozen source changes, not measured improvements:

| Removed or replaced code/work | Replacement or retained path |
| --- | --- |
| Data-local `ReadSet` and its duplicate resolution/preparation logic | Namespace `Prepared` and the shared operation snapshot, with exact observed rows and authority |
| Full second parent/name walk during path open | Carry discovery predicates through the target inode lock and validate them once |
| Extra attribute request after truncate or existing-file create fallback | Return the successful operation's inode attributes to the FUSE caller |
| Standalone `EntityBackend::allocate` and Redis allocation mode | Typed atomic inode allocation for create and atomic batch `reserve_slices` for upload registration |
| Ownership `allocate_upload`, `register_upload`, `uploaded` and singleton `reserve_upload` APIs | Batch Pending reservation before transport, then durable PUT receipts and aggregate publication |
| Separate Uploaded metadata transition in ordinary writes | Final transaction promotes exact Pending rows together with map/owner/inode publication |
| Per-chunk independent ownership publication on the ordinary multi-chunk path | Aggregate deltas once per slice; split only known preparation bounds, retaining uploaded receipts and acknowledged-prefix semantics |
| Unconditional no-op truncate-recovery reads before ordinary I/O/fsync | Read the intent in the operation snapshot; run recovery only for a present intent |
| Eager construction/embedding of nested lower-cache loader futures | Lazy loader factory and boxed selected fill; corrected mounted evidence remains pending |

The asynchronous `plan_chunk_membership` adapter remains for truncate,
discard and orphan retirement; it calls the shared pure membership planner.
Actual truncate recovery, durable Pending owners, deletion reservations,
reference counts, reader retirement holds and sticky uncertain-I/O errors are
retained. No head counter, global mutation gate, mutable-layer digest or old
commit-view cache is introduced.

## Candidate validation and comparison: pending

| Checkpoint | Status |
| --- | --- |
| Frozen baseline build, diagnostics and preliminary throughput | Completed with the limits above |
| Intermediate cache/create debug checkpoint | Incomplete: three aborts, each 0/4 phase cells; no usable counts or throughput |
| Lazy boxed memo-loader correction | In working source; corrected mounted measurement pending |
| Snapshot/ownership/grouping implementation | Source frozen; compilation and runtime gate results pending |
| EOF hidden-tail fallback and real RustFS fragmented-publication tests | Added to frozen source; not yet run |
| Focused memory and real Redis/RustFS correctness checks | Pending for the final candidate source |
| Required fmt/check/build/clippy/workspace test gates | Pending for the final candidate source |
| Candidate source manifest and executable hash | Pending |
| Matched 200-file diagnostic counts/timers | Pending |
| Minimal smoke and 16/32-worker reversed-order comparison | Pending |
| Accepted performance change | None |

| Preliminary comparison | Workers | Native WRITE | Entity WRITE | Native READBACK | Entity READBACK | Entity delta vs baseline |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Candidate, pending | 16 | Pending | Pending | Pending | Pending | Pending |
| Candidate, pending | 32 | Pending | Pending | Pending | Pending | Pending |

| Diagnostic candidate counts | CREATE RTTs/op | WRITE RTTs/op | WRITE commands/file | READBACK commands/file |
| --- | --- | --- | --- | --- |
| Pending matched measurement | Pending | Pending | Pending | Pending |

The three inherited legacy overlay failures remain
`g13a_kv_released_lease_is_not_an_expired_recovery_root`,
`kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read` and
`migration_marker_fences_entity_and_hot_native_packets_without_writes`.
The two documented source-file flakes are
`source_mutation_replacement_symlink_and_limits_fail_closed` and
`source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`.
Historical operator `clippy::double_must_use` is outside the Cargo workspace.
See [inherited evidence](2026-10-20-overlay-entity-data.md#inherited-failures-and-evidence-provenance)
and the [r31 handoff](2026-10-17-overlay-no-seal-handoff.md#known-bugs--pre-existing-failures).
Do not label these new regressions or describe a failing broad run as green.

## Documentation audit

The full repository audit covers 147 Markdown files and 526 local relative
links, including heading fragments: **zero broken links**. Canonical packed and
overlay specs, workspace architecture and this checkpoint have zero heading or
fence advisories. The 81 optional style advisories elsewhere are inherited from
historical/vendor documents; they are not new broken links or malformed fences
in these maintained documents. The 257 external links were classified but not
network-validated. `git diff --check` passed at the audit checkpoint.

Results are preserved at
`/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/candidate/docs-links.json`.
The reproducible checker remains at
`/mnt/brewfs-data/artifacts/docs-spec-consolidation-20261020/check_relative_links.py`.
This documentation audit runs no Cargo command, service, FUSE mount or benchmark
and does not change the pending candidate validation status above.

## Reproduction

From the frozen checkout, using its preserved executable:

```bash
ENTITY_MOUNT_DIAGNOSTIC=1 ENTITY_MOUNT_FILES=200 \
ENTITY_MOUNT_WORKERS=16,32 ENTITY_MOUNT_REPETITIONS=1 \
ENTITY_MOUNT_ARTIFACT_ROOT=/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/baseline/diagnostic-retry \
ENTITY_MOUNT_BINARY=/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/baseline/brewfs \
bash tools/perf/run_entity_mount_comparison.sh

ENTITY_MOUNT_DIAGNOSTIC=0 ENTITY_MOUNT_FILES=2000 \
ENTITY_MOUNT_WORKERS=16,32 ENTITY_MOUNT_REPETITIONS=2 \
ENTITY_MOUNT_ARTIFACT_ROOT=/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/baseline/throughput \
ENTITY_MOUNT_BINARY=/mnt/brewfs-data/artifacts/overlay-entity-cost-20261020/baseline/brewfs \
bash tools/perf/run_entity_mount_comparison.sh
```

Groups use fresh IDs/scopes and preserve workload trace hashes, commands,
source/binary manifests, service configuration and cleanup evidence. Candidate
runs must use a separately frozen executable and artifact root with matching
release flags and workload settings. MONITOR runs and throughput runs remain
separate; later results must fill the pending tables from their actual artifacts.
