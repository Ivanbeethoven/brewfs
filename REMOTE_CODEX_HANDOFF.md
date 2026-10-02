# BrewFS packed-v3 handoff — 2026-10-02

## Repository and safety

- Working directory: `/home/hxy/brewfs`.
- Branch: `codex/packed-metadata-aliyun-20260930`.
- Baseline at takeover: `abcdda0ba975af50cf4147c37d271099e316f8f7`.
- Retained validation/measurement fix: `dd418fa` (`test: validate packed dynamic frames and effective FUSE TTL`).
- `origin` is the personal repository, `upstream` the public repository.
- Keep `.claude/` untracked. Do not reset/checkout user work, remove credentials,
  print tokens/passwords/AK/SK, or commit local generated artifacts.
- Only this session owns the current working-tree edits. The other interactive
  session is no longer running; do not resume concurrent writers in this tree.

Historical remote transport is documented in
`doc/operations/remote-codex-migration.md`. SSH alias `brewfs-frp-sea` reaches
Windows; `ssh.exe` can forward commands to WSL `Ubuntu-24.04`, user `hxy`.
Credential profiles remain operator-owned and must not be deleted.

## What is already implemented

- Independent PM06/GC04/GM06/II05 formats in the 004 envelope.
- Fenced pageable group/inode indexes and byte-weighted metadata tiers.
- Bounded strict/coalesced payload streams, digest checks, class-aware demand
  coordinator and a 250 µs collection delay.
- Explicit aligned window/read-ahead and decoded-frame cache, both off by
  default for strict demand-only data reads.
- Shared tree/stat/full scanner, deterministic shuffled epochs, native Aliyun
  packed/Redis/TiKV harness and process-tree timeout cleanup.
- `ReadGeneration`/`ReadSource`/`compose_overlay_plan` primitives. The standalone
  packed readonly mount works through the legacy slice/BlockStore facade;
  actual workspace packed-lower binding, lifecycle publication and retries are
  **not complete**.
- Runtime metrics are logged at unmount; complete `.stats`/Prometheus packed
  export is still missing.

Important commits already on the baseline:

- `7d97082`: streamed packed ranges and runtime metrics.
- `7f2f27d`: matched shared scanner.
- `2b112b1`: opt-in decoded-frame cache.
- `535e3b8`: successful reads only in logical-byte counters.
- `89e662e`: decoded cache hits avoid reopening the container.
- `900075e` through `abcdda0`: cloud million-file harness and completed result
  documentation. Do not rerun these campaigns merely because the old handoff
  used to describe them as pending.

## Existing performance observations

The final historical cloud summary is
`doc/performance/aliyun-packed-v3-vs-juicefs-triple-1m-2026-10-01.md`.

- Ordered 1M stat: packed 4,595.13 files/s, Redis 6,089.22, TiKV 2,186.64.
  Packed stat is about 2.10x TiKV, but tree is slower and full read is only
  approximately tied with TiKV.
- Shuffled 1M epoch 1: packed demand-cold 186.54 files/s, explicit decoded
  4 GiB profile 369.05, Redis 826.84, TiKV 590.73. There is no general packed
  victory and no actual GPU training-throughput measurement.
- Local 32 MiB decoded-frame A/B: mean +19.91% throughput and -20.40% p95,
  valid only as an explicit `warm-frame-cache` observation.

### Newly discovered measurement limitations

The cloud runner exported `BREWFS_METADATA_CACHE_TTL_MS`, but FUSE reads
`BREWFS_CACHE_TTL_MS`. The current correction explicitly forwards the effective
name to the mount command. Historical numeric results remain observations;
claims of identical actual TTL/cache semantics require revalidation.

`data_range_gets=0` does not mean no file data was transferred: inline bytes are
fetched and retained as part of GroupMeta. Moka weighted resident bytes are not
process RSS; scanner RSS and filesystem-daemon RSS are different metrics.

The valid archived shuffled warm row records 1,284,176 inode-index GETs and
1,236,238 GroupMeta GETs across the two epochs, about 933.3 GiB of metadata-range
bytes. The old phrase "tens of thousands" was incorrect. GroupMeta stored bytes
include inline payload. The strict r2 epoch-2 artifact contains errors and must
not replace the later successful strict row.

## Current iteration closeout

### Index-budget candidate: withdrawn

The manifest-aware split would reclaim the unused group-index half of the
64 MiB pool for inode pages without increasing total metadata bytes. It passed
budget edge and real authenticated-page residency tests, but actual local
RustFS/FUSE constrained-budget runs had EIO/ENOENT in both the exact `abcdda0`
baseline and candidate. No successful paired throughput result exists.

The production candidate and its temporary allocation tests were removed with a
targeted patch. Original production budget allocation is restored. Full details:
`doc/performance/packed-v3-index-budget-candidate-2026-10-02.md`.

Preserved local evidence:

- `docker/compose-xfstests/artifacts/local-packed-index-budget-20261002/`
- `docker/compose-xfstests/artifacts/local-packed-index-budget-20261002-ttl1/`
- `docker/compose-xfstests/artifacts/local-packed-index-budget-20261002-tiny/`

Each contains raw profiles/errors/build or cleanup logs. Baseline source was
built from `git archive abcdda0`, not inferred from binary timestamps. Builds
were unoptimized and matched; they are diagnostics, not release benchmarks.

### Retained work

1. Catalog regression covers the builder→manifest→GroupMeta→remote-read chain
   for 200 KiB/512 KiB/1 MiB/10 MiB/32 MiB, random/sequential profiles, inline
   and non-inline Tiny files, EOF/frame boundaries and a p90 hint. It does not
   prove full size-class FUSE/lifecycle support.
2. Cloud runner now forwards the actual FUSE TTL environment variable.
   `tools/perf/test_packed_runner_ttl.sh` verifies the actual `start_mount`
   invocation with a test CLI at 0/1,000/60,000 ms and a conflicting parent env.
3. Spec checkpoint and historical-result caveats corrected. Detailed staged
   experimental plan written:
   `doc/superpowers/plans/2026-10-02-brewfs-three-innovations-experiment-plan.md`.

## Dynamic block / packed metadata audit

Dynamic frame selection, same-class co-pack, multi-frame extents and catalog
execution are real, not merely proposed. Default caps are random 4 MiB and
sequential 8 MiB; the default Tiny target is 256 KiB. Actual stored frame length
is not always equal to the target. The fixture still caps files at 4 MiB and
passes `p90=None`.

Still incomplete:

- zstd independent frame codec and compressed/restart GroupMeta;
- external large-object DataRef and sparse producer/inventory;
- common manifest-profile/frame-consistency and target 256-frame/file bound;
- cold attributes and hardlink placement semantics;
- range descriptor authentication anchored in the manifest;
- P5 workspace packed lower/VFS/publish/recovery integration.

Strict remote per-frame digest comes from an unauthenticated descriptor table;
it is self-consistency, not full content authentication. Full-object local open
has stronger checks. Never claim complete authenticated container reads without
an independently authenticated directory/ref or an explicit full verification
mode. Do not silently mutate 004/GM06/GC04 interpretation.

## Verification and remaining gates

Current-iteration raw validation logs are under the first artifact directory:
`final-check.log`, `final-build.log`, runtime feature checks,
`final-packed-tests.log`, `final-workspace-tests.log`, `final-clippy.log`.
Consult their completion/status rather than treating old 65-test/1097-test prose
as proof for an arbitrary future tip. AGENTS.md's workspace lib/bins test is a
hard gate. GitHub CI has stricter all-feature/-D warnings/operator steps; local
minimum-gate completion is not a claim those independent jobs passed.

## Final local validation (this iteration)

- fmt/diff check, required shell syntax/report tests and shared scanner tests:
  passed. The TTL regression reproduces the old failure and passes the correction.
- `cargo check --workspace` and `cargo build --workspace`: passed.
- tokio/io-uring runtime checks, with and without `workspace-overlay`: passed.
- packed feature suite: 66 passed, 0 failed, including the mixed size/profile
  corpus. Full workspace lib/bins gate: 1097 passed, 225 ignored, 0 failed.
- `cargo clippy --workspace`: exit 0 with pre-existing warnings. No assertion is
  made that all-feature/operator or `-D warnings` GitHub jobs passed.
- The first chained gate command exceeded its background time limit during
  workspace tests; independent completed logs supersede the incomplete log:
  `final-workspace-tests-complete.log` and `final-clippy-complete.log`.

## Next work, in order

1. Diagnose constrained-budget mounted-FUSE EIO with a small reproducer before
   trying more cache tuning. Do not accept numbers from failing scans.
2. Fix/request-graph measurement and manifest-bound descriptor authentication.
3. Run the mixed size/partial-read corpus in actual FUSE and add offline builder
   controls for static-frame, dynamic-frame, inline and size-table experiments.
4. Follow the experiment plan: separate packed metadata effect from frame size,
   inline/cache/prefetch and total setup/drain effects.
5. Finish packed lower generation-fenced workspace binding and publication
   before running the three-innovation lifecycle experiments.
6. Only after small gates pass, run matched JuiceFS+TiKV smoke/scale-up, with
   actual equal TTL and stated total client/server resources. No unbounded or
   unnecessary repeat of existing million-object imports.

## Resource state

No cloud server or credential object was created in this iteration. All local
RustFS/FUSE runs used traps to stop daemons/unmount, remove only freshly generated
temporary fixture directories, and run Compose `down -v --remove-orphans`.
Accepted/failed referenced artifact logs remain preserved. Verify process,
mount and container/volume state again before starting the next experiment.

## Delivery

The retained tests and TTL forwarding are committed in `dd418fa`; this handoff,
closeout and experiment plan form the documentation follow-up. Keep `.claude/`
and artifacts out of commits. Verify the personal branch push before closeout.
Report actual commits, test results, withdrawn candidate and pending
correctness/feature gates without a new performance-win claim.
