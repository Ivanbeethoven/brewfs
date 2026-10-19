# Overlay-on-packed handoff (2026-10-17)

Read this first when resuming the overlay-on-packed performance work. It
replaces the overlay part of
[2026-10-12-packed-v3-final-handoff.md](2026-10-12-packed-v3-final-handoff.md).

## Repository state

- Integration branch: `codex/packed-metadata-aliyun-20260930` (PRs target this
  branch, not `main`).
- Head after this work: `9c0b195` (merge of PR #30), plus this handoff doc PR.
- PR #30 `refactor(overlay): immutable lower + non-rotating overlay; drop seal`
  contains two commits:
  - `e7a345d`: SPEC/docs only;
  - `0742303`: code.
- Earlier rounds 1–22 were merged as PRs #10–#29. Per-round notes are in the
  PR descriptions.
- Commits on this host need an explicit identity:
  `GIT_AUTHOR_NAME/GIT_COMMITTER_NAME="Xiaoyang Han"`,
  `GIT_AUTHOR_EMAIL/GIT_COMMITTER_EMAIL=lux1an@qq.com`.
- To change a PR base: `gh api -X PATCH repos/Ivanbeethoven/brewfs/pulls/N -f base=...`
  (`gh pr edit --base` fails here).

## Current model (final)

- One **immutable packed-v3 lower** plus one **writable overlay**. The overlay
  head never rotates while a writer lease is live.
- There is no seal transaction, no compaction/flatten and no strict lower mode.
- New lowers come only from packed publication, run under the workspace's
  single active writable lease.
- Maintained references:
  - [architecture doc](../../architecture/workspace-overlay.md): has the
    head-writer and lease audit table;
  - [overlay implementation spec](../specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md):
    2026-10-17 banner, §14–§16, §24;
  - [operator spec](../specs/2026-09-03-brewfs-workspace-operator-lifecycle-spec.md).
- CLI: `init-volume`, `init-packed-volume`, `create --from <rev>`,
  `fork <rev>`, `list`, `inspect`, `diff`, `discard`. The commands
  `snapshot`, `commit` and `fork <workspace-uuid>` were removed.
- Operator: `BrewFSWorkspaceSnapshot` works only for packed workspaces;
  native workspaces go to `Failed`.

## Key design decisions

1. **Sole-writer safety is the only write authority.** Every mutation CAS
   checks:
   - the lease row;
   - `workspace.active_lease`;
   - the packed writer-authority and open records;
   - a backend-time deadline (`lease.expires_at_ns`).

   Regression tests in `native_effective_export_tests.rs`:
   - `invariant_second_mount_cannot_acquire_a_live_lease`;
   - `invariant_commit_after_lease_expiry_fails_closed`;
   - `invariant_commit_after_lease_loss_fails_closed_and_blocks_takeover`;
   - `invariant_no_head_change_under_an_active_lease`.
2. **No head changes under a foreign live lease.** Every head writer (binding
   install/publish, packed journal commit, initial bootstrap, native complete
   driver, clean restore/headless) requires the caller's own live lease.
   `acquire_lease` refuses a second lease while one is live. The audit table is
   in the architecture doc.
3. **Immutable-lower memos.** The lower is authenticated once at attach. Lower
   inode, permission, reverse-alias and lookup data are memoized for the
   lifetime of the binding. Upper caches serve only while `LeaseValidity`
   proves the lease is held (rounds 18–19).
4. **Commit view cache (round 23).** `packed_permissions.rs::CommitViewCache`:
   - A commit reuses its own post-commit view bytes (plus head reverse state
     and extent generation; slice-delete keys are guessed absent) as the CAS
     conditions, so it skips the view pre-read.
   - Any outside change only fails the CAS. The commit then does one fresh
     read: `Fenced` or `Busy` from the cache means re-read.
   - Recovery-token writers never use the cache.
   - Test: `sole_writer_commit_skips_the_view_pre_read_and_survives_renewal`.
5. **Test-only lower seeding.** `src/workspace_overlay/test_base_fixture.rs`
   with `#[cfg(test)] install_test_base` replaces seal in GC/diff/resolver
   tests. It is not a production API.

## Measured overlay-vs-native ratios

Runner: `tools/perf/run_overlay_vs_native.sh`. Small files: 2000 × 16 KiB for
rw; 2000 × 100 KiB for stat/full. Redis metadata, RustFS objects, 16 workers,
arms alternated, 6 runs pooled.

Round 23 (`$R/ovn-{hot27,noseal1}-r23p{a,b}`): `hot27` is the merged #29 build,
`noseal1` is #30. Errors: 0 in all 168 runs. The host ran about 2× slower than
in rounds 18–22, so compare only within this table.

| Mode | #29 overlay | #30 overlay | Change | #30 overlay/native |
| --- | ---: | ---: | ---: | ---: |
| stat | 959 | 937 | noise | 0.75 |
| full | 201 | 199 | noise | 0.57 |
| write | 169 | 186 | +10.4% | 0.55 |
| readback | 451 | 459 | +1.8% | 0.81 |
| mixed | 358 | 380 | +6.2% | 0.80 |

- 32 workers rw (`$R/ovn-noseal1-w32`): 0 errors. Overlay/native: write
  185/404, readback 492/600, mixed 373/517.
- Real-mount probe: `$R/ovn-noseal1-probe` passed.

### Round 24 (PR #32, `a281922`)

Write-path cuts: cached commits check only ws/head/lease/writer/open/CONTROL;
derived keys are carried across commits; packed `resolve_inode_delta` uses
the lease-bound permission cache. Order-swapped rw vs #30
(`$R/ovn-{noseal1,r24}-r24p{a,b}`), 6 runs pooled, 0/72 errors:

| Mode | #30 overlay | #32 overlay | Change | #32 overlay/native |
| --- | ---: | ---: | ---: | ---: |
| write | 188 | 203 | +7.9% | 0.59 |
| readback | 462 | 463 | noise | 0.79 |
| mixed | 392 | 413 | +5.2% | 0.86 |

Redis per written file: 98.7 → 72.6 cmds and 1047 → 866 µs server time
(native: 33 cmds, 428 µs). Remaining write gap: 2 Lua CAS commits per file
(still 3.2 EVALSHA vs native 2.0) plus the per-op permission reads.

### Round 25 (open decision from the lease-bound cache)

Order-swapped rw vs #32 (`$R/ovn-{r24,r25}-r25p{a,b}`), 6 runs pooled,
0/78 errors including 32 workers:

| Mode | #32 overlay | r25 overlay | Change | r25 overlay/native |
| --- | ---: | ---: | ---: | ---: |
| write | 213 ± 7 | 222 ± 4 | +4.3% | 0.62 |
| readback | 493 ± 25 | 500 ± 15 | +1.5% (noise) | 0.83 |
| mixed | 434 ± 17 | 444 ± 17 | +2.1% (noise) | 0.90 |

Client round trips per created file: 12 → 11 (MGET 9 → 8). The rest is the
create commit's parent/new-inode snapshot, which feeds a CAS and so must be
fresh, plus two Lua CAS commits.

### Round 26 (rejected, not merged)

Seeding a freshly created inode's row (and the absent `security.capability`
xattr) into the lease-bound cache cut client round trips per created file
from 11 to 9 (MGET 8 → 6) and Redis load from 71.4 to 68.8 cmds/file.
Throughput did not move. Order-swapped vs #34 (`$R/ovn-{r25,r26}-r26p{a,b}`),
6 runs pooled, 0/78 errors:

| Mode | #34 | r26 | Change |
| --- | ---: | ---: | ---: |
| write | 227.0 ± 5.8 | 227.5 ± 7.9 | +0.3% |
| readback | 511.1 ± 13.8 | 500.1 ± 17.8 | −2.1% |
| mixed | 454.9 ± 17.8 | 453.4 ± 16.1 | −0.3% |

The code was reverted. Conclusion: the remaining small MGETs are no longer the
write bottleneck. Per file, the two Lua CAS commits (~790 µs Redis server
time, vs native 428 µs total) and the data path dominate. Further write gains
need a cheaper commit script (fewer checked keys and ZADD index writes per
commit), which is a larger protocol change not started.

### Round 27 (rejected, not merged): ungated data commits

Observation: overlay write plateaus at ~230 files/s regardless of workers
(1w 0.78x, 16w 0.63x, 32w 0.51x native). Hypothesis: the mount-wide
`mutation_gate` serializes commits. Removing it from `write()` (CAS-safe;
new test showed no lost updates) made write ~30% slower at 16 workers:
#34 234 files/s vs ungated 163–166 files/s (`$R/ovn-{r25,r27}-r27w16p{a,b}`;
run stopped early, the 32w arms were not run). Redis commandstats explain it:
EVALSHA per file 3.2 → 11.5 and MGET 9.8 → 15.5. Every data commit CASes the
same head-layer record (`next_sequence`), so ungated commits collide and
retry. The gate was serializing commits that conflict anyway.

Conclusion: the write ceiling is the per-workspace head-layer sequence
counter that every commit bumps. Removing it needs a row-ordering change
(per-inode or block-reserved sequences; resolver newest-wins and persisted
format affected). Only after that would narrowing the gate help. Not started.

### Round 29 (in progress on `codex/overlay-write-a-r29`): write scaling design

Committed, not merged: `6693d39` per-layer extent generation
(`packed/v3/native-extent-generation/<layer>`). GC reservations check every
census layer's generation; mutation-checked by
`native_slice_extent_landing_behind_the_scan_cursor_blocks_the_reservation`
(PR #38). It removes cross-workspace data-commit contention only; this
single-workspace benchmark is unaffected, so it is unbenchmarked.

Findings that block scaling within one workspace:
1. Every data commit bumps the head's `next_sequence` (round 27: ungating
   `write()` alone tripled EVALSHA/file and lost 30%).
2. Every data commit also bumps its head layer's extent generation, which is
   the same per-workspace hotspot. The proposed fix: exempt extent commits that
   only write slice IDs freshly allocated by this mount. A GC reservation
   for such a SID cannot predate its first commit; needs proof plus tests
   covering reuse paths (truncate, fallocate, copy-up).
3. Eight admin paths decide "head is empty" from `next_sequence == 1 &&
   owned_* == 0`: `read_unmounted_packed_source`,
   `finish_clean_publication`, `read_clean_published_view`,
   `finish_committed_clean_snapshot`, `finish_initial_publication`,
   `restore_initial_before_seed`, `claim_initial_open`,
   `finish_committed_initial_snapshot`. Sequence blocks / head-free data
   commits break this. Safe replacement only where no writer can exist (no
   live lease, checked in the CAS; acquire refused meanwhile), plus a bounded
   row scan of the head's delta prefixes. Each site must be verified and
   tested individually; a wrong site publishes a lower missing overlay data.

Order to resume: (3) per-site emptiness via lease-absent + row scan, tests
first; then (2) the fresh-SID exemption with mutation-checked tests; then
per-mount sequence blocks and inode-row CAS for data commits; then ungate
`write()`; then the oracle, the full gate, the probe, the 32-worker run and
the order-swapped comparison with a 1/16/32-worker table.

### Round 30 (PR #39, `503ab08`): group commit + per-layer extent generation

CPU sampling at 16 workers showed that overlay write is not CPU-bound or
Redis-bound: Redis was at 0.33 cores and the host was 68% busy. The cost is
the mount-wide gate serializing every commit with its round trip inside.
Group commit (one leader drains up to 64 queued writes into one versioned
mutation under the gate) keeps today's conflict detection unchanged.

Write scaling (overlay files/s, overlay/native), order-swapped, 0/144 errors:

| Workers | #34 | #39 | #39 overlay/native |
| --- | ---: | ---: | ---: |
| 1 (2 runs) | 43.7 | 43.9 | 0.86 |
| 16 | 209 ± 8 | 213 ± 6 | 0.64 |
| 32 | 210 ± 8 | 234 ± 6 (+11.1%) | 0.56 |

Readback/mixed unchanged (16w 0.82/0.90, 32w 0.83/0.84). EVALSHA per file
3.26 → 2.72. Write still plateaus near 210–235 files/s, so the cap is now
partly elsewhere: create commits (still one per file, gated) and the head
next_sequence. The next candidate is group-committing creates the same way.

Rejected designs:
- merging the create and first-fsync commits (breaks O_EXCL visibility);
- full metadata copy-up of the lower (no write-op gain for new files; breaks
  O(1) fork and change-proportional diff).

## Verified

| Gate | Result |
| --- | --- |
| fmt, clippy (overlay and default, all targets), tokio+overlay check, `cargo test --workspace --lib --bins` | pass |
| overlay + fuse lib tests | 1066 pass; 3 known failures (below) |
| operator | 56 tests pass |
| real mount | probe passed, 32-worker run 0 errors |
| perf | order-swapped comparison above |

Logs: `$R/build/noseal1/gate.txt` and `*.log`.

## Unverified / left

- **Docker workspace-overlay compose smoke** for the rewritten harness has not
  run. It now builds the lower with `packed_v3_snapshot_fixture` and forks by
  revision. The harness helpers were checked against real CLI output only. CI
  `docker-workspace-overlay-smoke` runs it on PRs.
- **Operator clippy** fails on a `must_use` lint from rustc 1.99 in
  `operator/brewfs-operator/src/workspace/admin.rs:73` and `gc.rs:44,233`.
  `gc.rs` is untouched by this work; `admin.rs` was edited, but not at line 73.
  This was not re-checked on the base commit.
- **Biggest remaining gap: write (0.55×).** Each file costs about 3 commits
  (create, fsync, close), each a server-side Lua CAS. The pre-read is gone.
  Next idea, not started and needing user approval: merge the create and first
  fsync commits for a newly created inode. A single-Lua-script commit was
  estimated in round 13 at ≤5%.
- **stat (0.75×) and full (0.57×)**: remaining cost is per-op permission and
  path work plus FUSE read count (rounds 11–12 found no single hotspot).

## Known bugs / pre-existing failures

These fail deterministically, also on the base commit:

- `g13a_kv_released_lease_is_not_an_expired_recovery_root`;
- `kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`;
- `migration_marker_fences_entity_and_hot_native_packets_without_writes`.

Flaky tests: `source_file::source_*` (under load) and
`writable_acl_independent_sqlite…`.

## Host, build and disk

- ECS `brewfs-singapore-v2`. The repo is at `/mnt/brewfs-data/src/brewfs`
  (`/home/hxy/brewfs` is a symlink to it).
- `$R=/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z`.
- Builds:
  - Use `CARGO_BUILD_JOBS=4` and run under
    `systemd-run --user --scope -p MemoryHigh=24G -p MemoryMax=28G -p MemorySwapMax=24G`.
  - Check `cat /proc/sys/vm/swappiness` = 10 before each build; it resets to 0
    silently (`sudo sysctl -w vm.swappiness=10`). There is a 32G swapfile.
  - Never run a release build and a test build at the same time (OOM risk).
  - Release builds take about 40 min; the lib test build about 10–20 min.
- Disk: 85% used (37G free). Each round's build dir is about 4 GB
  (`$R/build/hot2*`, `noseal1`). Ask the user before deleting merged rounds'
  binaries. Keep `hot27` (#29 baseline) and `noseal1` (#30).
- A stuck FUSE connection makes `ps` hang. Fix:
  `echo 1 | sudo tee /sys/fs/fuse/connections/<n>/abort`.
- Never run ignored real-backend tests with a broad filter; use `--exact`.

## Exact commands

```bash
cd /mnt/brewfs-data/src/brewfs
R=/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z
# Full gate + release binaries into $R/build/<round>/ (template):
cat $R/build/noseal1/gate.sh
# Real-mount probe:
OVN_FILES=100 OVN_WORKERS=1 OVN_EPOCHS=1 OVN_REPEATS=1 OVN_MODES="stat rw" \
  OVN_RW_FILES=200 OVN_RW_MIXED_OPS=200 OVN_ARTIFACT_ROOT=$R/ovn-<round>-probe \
  OVN_BINARY=$R/build/<round>/brewfs OVN_FIXTURE=$R/build/<round>/packed_v3_snapshot_fixture \
  bash tools/perf/run_overlay_vs_native.sh
# 32-worker rw: same with OVN_WORKERS=32 OVN_REPEATS=1 OVN_MODES=rw.
# Order-swapped comparison: run arms in the order base pa, cand pa, cand pb,
# base pb with default OVN_* settings; pool per binary. Template: /tmp/r23.sh
# (not persistent; the loop is reproduced in this doc's history).
```

Unset `BREWFS_WORKSPACE_ROW_CACHE_TTL_MS` before perf runs (the default is
60 s). Run perf alone; never alongside a build or a profiling run.
