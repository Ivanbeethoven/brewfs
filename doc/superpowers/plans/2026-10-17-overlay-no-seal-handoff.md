# Overlay-on-packed delivery handoff (updated 2026-10-19)

> **Historical implementation checkpoint.** This document describes shipped
> r31, not the proposed replacement. On 2026-10-19 the owner directed an
> ordinary per-entity upper namespace design. The maintained
> [architecture](../../architecture/workspace-overlay.md) and
> [implementation spec](../specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md)
> now describe that unimplemented target and its staged migration. R32's
> unvalidated performance drafts were archived and removed, with no candidate
> benchmark or merge; see the [investigation](2026-10-19-overlay-write-r32.md).

Read this first when taking over the delivered overlay-on-packed work. It
replaces the overlay part of
[2026-10-12-packed-v3-final-handoff.md](2026-10-12-packed-v3-final-handoff.md).
This is a delivery checkpoint, not an instruction to start another optimization.
The final all-mode sweep was interrupted; do not rerun the long release
benchmark unless requested. Completed artifacts and incomplete coverage are
distinguished below.

## Repository state

- Repository: `/mnt/brewfs-data/src/brewfs`.
- Integration and delivery target: `codex/packed-metadata-aliyun-20260930`,
  remote `https://github.com/Ivanbeethoven/brewfs.git` (not `main`).
- Verified merged code head: `0d4e36cab1ae0cbb5b2b31d1d42f1447b0b90107`
  (PR #41). The delivery documentation commit follows that code baseline.
- At takeover, `codex/overlay-delivery-docs` pointed to the same commit as the
  local and remote integration branch. Only the architecture and implementation
  spec had uncommitted draft edits; those were reconciled with source for this
  documentation delivery. No Rust or benchmark-runner changes are included.
- Earlier rounds 1–22 are PRs #10–#29. Relevant merged history:

| PR | Merge | Contents |
| --- | --- | --- |
| #30 | `9c0b195` | Immutable lower/non-rotating overlay, seal removal, commit-view cache; docs `e7a345d`, code `0742303` |
| #31 | `9192564` | Initial handoff, `0d64f50` |
| #32 / #33 | `a281922` / `fed519b` | Narrower cached commit checks and cached write-path rows (`7471da2`); round-24 results |
| #34 | `1467e60` | Cached open decision (`46f3e9a`) and round-25 results |
| #35 / #36 | `89963c9` / `0227f9b` | Rejected round-26/27 experiment records; rejected code is absent |
| #37 / #38 | `24b345f` / `99e6fdf` | Concurrency oracle (`04bfbb9`) and GC scan-cursor regression (`6b1fc10`) |
| #39 | `503ab08` | Per-layer extent generation (`6693d39`) and data group commit (`2b55e8a`) |
| #40 | `7149664` | Round-30 results, `c994146` |
| #41 | `0d4e36c` | Seed created inode paths, `760457a`; despite its branch name, **no create group commit** |

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
- Delivered references at commit `6b54270` (use `git show 6b54270:<path>`):
  - `doc/architecture/workspace-overlay.md`: r31 head-writer/lease audit;
  - `doc/superpowers/specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md`:
    r31 fields, group commit, GC and cache contract. Current versions of these
    two paths describe the proposed replacement, not this checkpoint;
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
4. **Commit view cache (rounds 23–24).** Store-side
   `packed_permissions.rs::CommitViewCache`:
   - A commit reuses its own post-commit view bytes (plus head reverse state
     and extent generation; slice-delete keys are guessed absent) as the CAS
     conditions, so it skips the view pre-read.
   - A stale comparison, cached `Fenced` or derived `Busy` forces a fresh read
     and full revalidation within the bounded retry loop.
   - Recovery-token writers never use the cache.
   - The cached view compares workspace/head/lease/writer/open/CONTROL plus
     the derived reverse/extent/slice checks; lower/PWB3/recovery rows rely on
     the sole-writer invariant. CONTROL is compared for migration fencing,
     never written by ordinary mutations.
   - Test: `sole_writer_commit_skips_the_view_pre_read_and_survives_renewal`.
5. **Test-only lower seeding.** `src/workspace_overlay/test_base_fixture.rs`
   with `#[cfg(test)] install_test_base` replaces seal in GC/diff/resolver
   tests. It is not a production API.
6. **Data group commit and GC (PR #39).** One queue leader takes up to 64
   writes, then commits under the existing mutation gate. Inode size/version
   updates fold per inode; visible extents keep queue order. Head sequence
   allocation, expected-head CAS, lease deadline and slice deletion checks
   remain. Every layer with changed extent rows has its generation bumped
   atomically.
   GC reservations check all census layers' generations plus inventory/root
   authority, closing the behind-scan-cursor insertion race. There are no
   sequence blocks or fresh-SID exemptions. See the architecture for the proof.
7. **Lease-bound caches and create paths (PR #41).** Upper/permission rows are
   invalidated before and after affected commits. Namespace paths/aliases
   survive data-only commits; creates invalidate only the new inode, other
   namespace changes clear them. Invalidation tickets reject stale inserts.
   New inode paths are seeded only from complete parent paths (or the known
   root). Losing `LeaseValidity` clears/bypasses mutable caches; immutable lower
   memos remain binding-scoped. The default 60-second TTL is a retention bound.

## Final measured ratios and evidence coverage

`$R` throughout this document is
`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z`.
Final release binaries are `$R/build/r31/{brewfs,packed_v3_snapshot_fixture}`.
Their SHA-256 identities, also recorded in each measured group, are:

```text
2a5146cb21a00334d4240a1b23a0b1b26a12b71202ee2494ef435f37fbfc249b  brewfs
3a142f121cc3cd3d5b74a372b52ce363529f2fbd31d8a26915058ced9d69b235  packed_v3_snapshot_fixture
```

Completed **r31 / PR #41** overlay/native files/s ratios:

| Workers | stat | full read | write | readback | mixed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 1.00 | 0.71 | 0.86 | 1.06 | 1.02 |
| 16 | — | — | 0.63 | 0.91 | 0.85 |
| 32 | — | — | 0.57 | 0.88 | 0.87 |

The dash means no completed final-build measurement. Do not fill it using a
different binary or treat the interrupted sweep as passed.

| Workers / source | Mean overlay / native files/s (stat; full; write; readback; mixed) | Samples per mode/arm |
| --- | --- | ---: |
| 1: `ovn-final-r31-w1/ovn-20261019T104030Z-2752928` | 289.47/288.11; 50.98/71.42; 44.27/51.60; 97.43/91.88; 74.42/73.24 | 2 |
| 16: `ovn-r31-r31w16p{a,b}/ovn-*/summary.json` | —; —; 216.89/344.01; 539.64/595.03; 415.32/486.83 | 6 |
| 32: `ovn-r31-r31w32p{a,b}/ovn-*/summary.json` | —; —; 236.89/414.74; 548.37/625.92; 455.76/522.25 | 6 |

For pooled comparisons use the arithmetic mean of `rows[].files_per_sec` for
each arm, then divide overlay by native. The runner's `groups` fields are
per-invocation **medians**, so pooling those medians gives different numbers.
The one-worker run has two repetitions, so its mean and median coincide.

All five source groups above have `status=passed`: 92 rows, zero errors,
matching workload traces/checksums and `cleanup-proof.txt` confirming no
compose resources or FUSE mounts left. The PR #41 comparison also ran r30
base arms (`ovn-r30-r31w{16,32}p{a,b}`): 144 total rows across both binaries,
zero errors. Relative to r30, r31 readback improved 15.6% at 16 workers and
6.0% at 32; write/mixed changes were noise. The real-mount r31 probe passed
8/8 rows (`ovn-r31-probe/ovn-20261019T101259Z-2688475`).

Profile: Redis + RustFS; identical binary and workload within each native /
overlay pair; stat/full files 100 KiB, rw files 16 KiB; mixed 30% writes with
fsync; FUSE TTL 0, direct I/O, keep-cache 0; compression/payload caches/prefetch
disabled; default lease-bound metadata cache. One worker uses 500 files and
500 mixed operations; 16/32 workers use 2,000. Workers count workload threads;
the FUSE worker pool stays at 16. Stat/full have two epochs and
report the last epoch's files/s. Setup, mount and teardown are separate from
active files/s. These focused results do not establish broad fio or TiKV parity.

### Interrupted sweep and historical stat/full coverage

- `ovn-final-r31-w16/ovn-20261019T104512Z-2762851` was interrupted during the
  first overlay rw arm when the previous agent exited. It has 11 completed
  rows, **no group summary or cleanup proof**. Its two-repetition stat/full
  pairs yield provisional 0.72/0.57, excluded from the completed-results table.
  Native rw rows alone are not a comparison. The r31 w32 all-mode sweep was
  never started.
- The latest completed 16-worker stat/full comparison is PR #30:
  `ovn-noseal1-r23p{a,b}`, six samples per arm, 937.32/1242.37 = **0.75** and
  198.85/349.92 = **0.57**.
- The latest completed 32-worker stat/full artifact is the older `hot26`
  build, before PR #29: `ovn-hot26-w32/ovn-20261017T021238Z-2052596`, three
  samples per arm, 2411.30/3070.92 = **0.79** and 412.79/781.41 = **0.53**.
  Host speeds differed substantially across rounds; these are historical
  ratios, not final-build measurements or scaling comparisons.
- Delivery preserved the partial artifact, saved mount diagnostics in
  `$R/build/delivery-20261019/interrupted-mount-before.txt`, and normally
  unmounted its orphan `/tmp/brewfs-ovn.cpqnU3/overlay-rw-r1/mnt`. No runner,
  brewfs daemon or compose container was still running at that inspection.

## Historical experiment record (not the final table)

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

### Round 25 (PR #34, `1467e60`): open decision from the lease-bound cache

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

The code was reverted. At that round, the remaining small MGETs were not the
write bottleneck. The two Lua CAS commits (~790 µs Redis server time per file,
vs native 428 µs total) and the data path dominated. A cheaper commit script
was discussed but not implemented; later rounds used group commit instead.

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

The head-layer `next_sequence` makes ungated writes conflict. No ordering or
persisted-format redesign was delivered; PR #39 amortizes the existing CAS
with group commit while retaining the gate and counter.

### Round 29 (merged through PR #39): per-layer extent generation

Merged: `6693d39` per-layer extent generation
(`packed/v3/native-extent-generation/<layer>`). GC reservations check every
census layer's generation; mutation-checked by
`native_slice_extent_landing_behind_the_scan_cursor_blocks_the_reservation`
(PR #38). It removes cross-workspace data-commit contention only; this
single-workspace benchmark does not measure the cross-workspace benefit.

Historical alternatives considered, **not implemented**:
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

That proposed sequence-block/head-free design was superseded by PR #39's
group commit. The eight admin emptiness checks, head counter and all extent
generation checks are unchanged. This list is historical analysis, not a
delivery follow-up plan.

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
3.26 → 2.72. Write still plateaued near 210–235 files/s; create commits remained
individually gated and the head still advanced `next_sequence`. Create group
commit was discussed but not implemented. PR #41 delivered path-cache seeding
only.

Rejected designs:
- merging the create and first-fsync commits (breaks O_EXCL visibility);
- full metadata copy-up of the lower (no write-op gain for new files; breaks
  O(1) fork and change-proportional diff).

## Verified

Delivery rechecked source, merge ancestry, artifact rows/profiles and the
following lightweight gates. It did not rerun tests or a release benchmark.

| Gate / evidence | Result and scope |
| --- | --- |
| Delivery `cargo fmt --all --check` | Pass |
| Delivery `cargo check --workspace --features workspace-overlay` | Pass; default runtime plus overlay, 2m22s; dependency warnings remain |
| Delivery `git diff --check` | Pass |
| Saved r31 fmt, shell syntax/report tests, clippy default + overlay (`--workspace --all-targets -- -D warnings`), tokio+overlay check | Pass |
| Saved r31 `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins` | Pass; BrewFS lib 1,178 passed, 225 ignored; this is the default-feature gate |
| Saved r31 `cargo test --workspace --lib --features workspace-overlay -- workspace_overlay fuse::` | **Failed**: 1,071 passed, 4 failed, 122 ignored, 1,438 filtered; three baseline failures plus source_file flake |
| Saved r31 operator tests | 56 passed; following clippy failed |
| Saved r31 release build | Pass; both binaries preserved |
| Saved oracle, group-write ordering/failure-isolation, created-path and GC scan-cursor tests | Pass in r31 feature log |
| Real mount / measured runs | r31 probe passed; completed sources and exact coverage above |

Delivery logs: `$R/build/delivery-20261019/{gate.txt,fmt.log,check-workspace-overlay.log,diff-check.log}`.
Prior build commands and results: `$R/build/r31/{gate.sh,gate.txt,*.log}`;
`r30` and `noseal1` retain their own logs. The gate script records exit codes
and continues after failures, so `done` and `release exit=0` do **not** mean the
whole gate passed. Review `test-overlay exit=101` and `operator exit=101`.

## Unverified / left

- **Final-build stat/full at 16/32 workers:** no completed all-mode groups;
  retain the distinction between historical, partial and completed numbers.
- **Docker workspace-overlay compose smoke:** no local completed evidence for
  the rewritten harness. It builds a lower with `packed_v3_snapshot_fixture`
  and forks by revision; helper output was checked against the real CLI.
  The CI job is `docker-workspace-overlay-smoke`; remote CI is not certified
  by this handoff. The focused OVN runner uses compose backends but is not
  that lifecycle smoke test or the broad fio/POSIX acceptance matrix.
- **Write:** final r31 0.63×/0.57× at 16/32 workers (217/237 files/s).
  Creates are individually gated, and data batches retain the head sequence
  and per-layer generation. There is no fixed "three commits per file"
  accounting after group commit. Merging create with first fsync was rejected
  for `O_EXCL` visibility; no such work is pending delivery.
- **Read/stat:** permission/path work and FUSE read count remain. The measured
  one-worker working set is smaller, so it is not a controlled scaling curve.
- **Other limits:** no `statfs` usage counters; bounded native GC census can
  return `Busy`; no new cross-workspace contention benchmark, TiKV performance
  comparison, broad fio pass or full POSIX requalification in this delivery.

## Known bugs / pre-existing failures

The same three failures are recorded in pre-no-seal `build/hot27/test-overlay.log`,
`build/noseal1/test-overlay.log` and `build/r31/test-overlay.log`. They were not
rerun or fixed during documentation delivery:

- `g13a_kv_released_lease_is_not_an_expired_recovery_root`;
- `kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`;
- `migration_marker_fences_entity_and_hot_native_packets_without_writes`.

- **Flaky `source_file`:** r31 failed
  `source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`
  at `source_file.rs:587`; r30 failed
  `source_mutation_replacement_symlink_and_limits_fail_closed`. The first also
  failed in `hot27`. This is prior load-sensitive behavior, not a green feature
  suite. `writable_acl_independent_sqlite…` is an earlier reported flake, absent
  from the r31 failure list; it has not been separately revalidated here.
- **Operator clippy:** rustc 1.99 reports `clippy::double_must_use` under
  `-D warnings` for `#[must_use]` functions returning an already-must-use type:
  `operator/brewfs-operator/src/workspace/admin.rs:73`, `gc.rs:44` and `gc.rs:233`.
  Confirmed in r31 `operator.log` after the 56 passing tests. The r30/r31
  performance commits do not touch these files; base-commit clippy was not
  independently rerun, so its baseline status remains a prior attribution.

## Host, build and disk

- Inspected 2026-10-19 on ECS `brewfs-singapore-v2`. Repo:
  `/mnt/brewfs-data/src/brewfs`
  (`/home/hxy/brewfs` is a symlink to it).
- `$R=/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z`.
- Toolchain: rustc `1.99.0 (b940084d7 2026-09-28)`, cargo
  `1.99.0 (5f94df478 2026-08-27)`. RAM 30 GiB; swap 31 GiB visible
  (32 GB swapfile), about 128 MiB used at inspection; swappiness was 10.
- Builds:
  - Use `CARGO_BUILD_JOBS=4` and run under
    `systemd-run --user --scope -p MemoryHigh=24G -p MemoryMax=28G -p MemorySwapMax=24G`.
  - Check `cat /proc/sys/vm/swappiness` = 10 before each build; earlier sessions
    observed resets to 0 (`sudo sysctl -w vm.swappiness=10`).
  - Never run a release build and a test build at the same time (OOM risk).
  - Release builds historically take about 40 min; lib test builds 10–20 min.
    No release build was needed for this delivery.
- Disk at inspection: `/dev/vdb`, 251G total, 202G used, 37G available, 85%.
  `target` 48G; operator target 7G; `$R/build` 24G. Docker images 9.51GB and
  build cache 5.08GB. Recheck before another long run.
- Saved binaries exist for `r25`, `r30`, `r31`. `hot27` and `noseal1` binaries
  are already absent; their logs, hashes and measured artifacts remain. No
  accepted artifacts or saved binaries were removed during delivery.
- The interrupted mount was unmounted normally; no abort was required.
  If a future FUSE request is stuck, preserve diagnostics before considering
  a targeted connection abort. Do not abort other mounts or assume a D-state
  request can always be killed; see the repository's FUSE limitations.
- Never run ignored real-backend tests with a broad filter; use `--exact`.

## Exact commands (recorded for reproduction, not run during delivery)

Requirements: `/dev/fuse`, `fusermount3`, passwordless `sudo -n` for Docker,
mount setup and dropping caches, Docker Compose, Python 3 with `boto3`, and
the backend images named by the runner. Use the saved binaries; `revision.txt`
records the runner checkout, while `binary.sha256` identifies the executed
code. Preserve both. The runner alternates native/overlay order each repetition.

```bash
set -euo pipefail
cd /mnt/brewfs-data/src/brewfs
R=/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z
unset BREWFS_WORKSPACE_ROW_CACHE_TTL_MS OVN_REDIS_MONITOR OVN_PERF_ARM OVN_RUST_LOG
export OVN_FILE_BYTES=102400 OVN_RW_FILE_BYTES=16384 OVN_RW_WRITE_RATIO=0.3
export OVN_SCANNER_SEED=20261001 OVN_EPOCHS=2
repro_stamp=$(date -u +%Y%m%dT%H%M%SZ)
sha256sum -c "$R/build/r31/binary.sha256"
```

Quick real-mount correctness probe, matching the saved r31 probe (use the
environment above):

```bash
OVN_FILES=100 OVN_WORKERS=1 OVN_EPOCHS=1 OVN_REPEATS=1 OVN_MODES="stat rw" \
  OVN_RW_FILES=200 OVN_RW_MIXED_OPS=200 \
  OVN_ARTIFACT_ROOT="$R/repro-$repro_stamp-r31-probe" \
  OVN_BINARY="$R/build/r31/brewfs" \
  OVN_FIXTURE="$R/build/r31/packed_v3_snapshot_fixture" \
  bash tools/perf/run_overlay_vs_native.sh
```

Completed r30/r31 comparison: six rw repetitions per binary and worker count,
outer build order base pa, candidate pa, candidate pb, base pb:

```bash
for workers in 16 32; do
  for entry in r30:pa r31:pa r31:pb r30:pb; do
    round=${entry%:*}
    arm_order=${entry#*:}
    OVN_FILES=2000 OVN_RW_FILES=2000 OVN_RW_MIXED_OPS=2000 \
      OVN_WORKERS="$workers" OVN_REPEATS=3 OVN_MODES=rw \
      OVN_ARTIFACT_ROOT="$R/repro-$repro_stamp-$round-w$workers-$arm_order" \
      OVN_BINARY="$R/build/$round/brewfs" \
      OVN_FIXTURE="$R/build/$round/packed_v3_snapshot_fixture" \
      bash tools/perf/run_overlay_vs_native.sh
  done
done
```

Exact final-sweep profile. Only run if explicitly requested: the original
finished w1, stopped during w16, and never started w32:

```bash
for workers in 1 16 32; do
  files=2000
  if [[ "$workers" == 1 ]]; then files=500; fi
  OVN_FILES="$files" OVN_RW_FILES="$files" OVN_RW_MIXED_OPS="$files" \
    OVN_WORKERS="$workers" OVN_REPEATS=2 OVN_MODES="stat full rw" \
    OVN_ARTIFACT_ROOT="$R/repro-$repro_stamp-final-r31-w$workers" \
    OVN_BINARY="$R/build/r31/brewfs" \
    OVN_FIXTURE="$R/build/r31/packed_v3_snapshot_fixture" \
    bash tools/perf/run_overlay_vs_native.sh
done
```

To recalculate the completed final table without mounting, building or running
the workload:

```bash
python3 - <<'PY'
import json
import statistics
from pathlib import Path

root = Path('/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z')
for workers, pattern in [(1, 'ovn-final-r31-w1'),
                         (16, 'ovn-r31-r31w16p[ab]'),
                         (32, 'ovn-r31-r31w32p[ab]')]:
    rows = []
    paths = sorted(root.glob(pattern + '/ovn-*/summary.json'))
    assert len(paths) == (1 if workers == 1 else 2)
    for path in paths:
        data = json.loads(path.read_text())
        assert data['status'] == 'passed', path
        rows.extend(data['rows'])
    for mode in ('stat', 'full', 'write', 'readback', 'mixed'):
        values = {arm: [row['files_per_sec'] for row in rows
                        if row['mode'] == mode and row['arm'] == arm]
                  for arm in ('overlay', 'native')}
        if not all(values.values()):
            print(workers, mode, 'not completed on r31')
            continue
        overlay, native = (statistics.mean(values[arm])
                           for arm in ('overlay', 'native'))
        print(workers, mode, len(values['overlay']),
              f'{overlay:.2f}/{native:.2f} = {overlay/native:.2f}')
PY
```

Delivery consistency check (no release build or performance run):

```bash
cargo fmt --all --check
systemd-run --user --scope -p MemoryHigh=24G -p MemoryMax=28G -p MemorySwapMax=24G \
  env CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo check --workspace --features workspace-overlay
git diff --check
```

The archived `$R/build/r31/gate.sh` records the earlier full gate and release
build flags; do not rerun it in place, because it writes accepted artifact
paths. Run performance alone, never alongside a build or profiling run.
