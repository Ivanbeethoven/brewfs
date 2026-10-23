# Overlay performance issues #192–#202

Base: `a8459dc92142bf94224e4f4af903f592cef5cb13` (`origin/main` when this
branch was created). Entity correctness commit `159a08f` is preserved on
its separate branch. Security issues and CSI correctness are outside scope.

## Reproduced mechanisms and fixes

Every fix below followed an executed failing regression against the
unchanged baseline production code. Baseline failures were assertions for
the issue's actual symptom, not compilation errors.

| Issue | Root cause | Minimal fix | Baseline result |
| --- | --- | --- | --- |
| #194 | `EntityData::read_object` serializes HEAD before an authenticated GET. | Remove HEAD; request at most descriptor length plus one byte so trailing corruption remains detectable. Retain exact-length and SHA-256 checks. | 1 HEAD, expected 0 |
| #195 | `EntityFuse::opendir` discards inode attributes already loaded by namespace readdir; readdirplus gets them again one at a time. | Retain directory, actual parent, and child attributes from the validated open snapshot; keep the fresh directory access check and zero TTL. | 12 metadata reads, expected 1 |
| #196 | Flushed release always submits fsync's snapshot and read-plan CAS, including untouched handles. | Record writes/truncates on the handle under its FIFO inode lock. Clean release keeps admission, locking, and sticky error reporting; explicit fsync and mutated handles retain the full barrier. | 1 read and 1 commit, expected neither |
| #198 | Every dentry mutation except fresh create globally invalidates packed path/alias caches. | Supply the complete changed file set, including an overwritten destination, to before/after invalidation. Directory changes and unknown shapes retain global invalidation. | Unrelated warmed path needs another reverse-index walk |
| #199 | Hot, cold, and reverse memos independently load after concurrent misses. | Coordinate concurrent fills per memo kind/inode, then recheck the existing bounded cache. Cache absence; preserve retryable errors and cancellation. | 16 hot values/16 reads; 16 cold values/48 reads; 32 reverse-page visits instead of 2 |
| #200 | Directory reads hold their checkpoint mutex throughout remote replay and merge. | Clone the best checkpoint under lock, perform I/O outside the lock, then publish the cursor after existing fixed-view validation. | Offset 0 times out behind paused offset 64 |
| #202 | Create authorization loads the parent permission snapshot, then creation preparation loads it again. | Read one fresh operation-local snapshot for both; preserve exact mutation-version validation and CAS. Never use lease-cached permissions as a writeback template. | 2 parent snapshot reads, expected 1 |

New regression names:

- `upper_object_read_uses_bounded_get_without_metadata_head`
- `readdirplus_reuses_attributes_captured_by_opendir`
- `clean_fuse_release_avoids_metadata_barrier_but_explicit_fsync_keeps_it`
- `packed_file_churn_preserves_unrelated_path_cache`
- `packed_hot_inode_memo_coalesces_concurrent_misses`
- `packed_cold_inode_memo_coalesces_concurrent_misses`
- `packed_reverse_alias_memo_coalesces_concurrent_page_walks`
- `packed_directory_slow_page_does_not_block_other_offsets`
- `packed_create_reuses_fresh_parent_permission_snapshot`

Additional guards cover missing/short/corrupt/extended object payloads,
actual parent attributes, cache absence/error retry, and cancellation without
blocking other inode keys. The primary tests also check explicit fsync,
truncate release, sticky errors, overwritten destinations, and directory moves.

## Evidence and validation status

Persistent artifacts:
`/mnt/brewfs-data/src/brewfs-perf-evidence-20261023`.

Valid baseline logs are `194-before-isolated3.log`,
`195-before-isolated3.log`, `196-before-isolated3.log`,
`198-before-isolated3.log`, `199-hot-before-isolated3.log`,
`199-cold-before-isolated3.log`, `199-reverse-before-corrected.log`,
`200-before-isolated3.log`, and `202-before-isolated3.log`. Each has
**0 passed, 1 failed** and Cargo exit 101. The earlier reverse test had a
wrong fixture-name assertion and is not reproduction evidence; the corrected
test demonstrated 32 versus 2 page visits before the reverse-cache fix.

The first candidate run (`regressions-after1.log`) executed **13 tests:
13 passed, 0 failed, 0 ignored**. A relay restart killed its wrapper but
left Cargo running, so this log has a genuine test summary without a
captured Cargo exit status. Complete recorded repeats
`regressions-after2.log`, `regressions-after3.log`, and
`regressions-after4.log` each passed **13/13**, with 0 failures, 0 ignored,
and Cargo exit 0. Each primary regression has now passed three recorded runs.

The exact regression command, from the disposable harness, was:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-performance/target \
cargo --config profile.test.package.brewfs.codegen-units=512 \
  test -p brewfs --lib --features workspace-overlay \
  -- --nocapture --test-threads=1
```

Baseline executions used the same environment and Cargo build configuration,
with the individual regression name after `--features workspace-overlay`
and `-- --nocapture`. Their preserved headers record the complete command
and source-patch hash; `before-corrected-production-equivalence.json`
preserves the baseline source verification.

Full overlay test executable builds exceeded the safe 22/24 GiB memory
limits. Those compile-only failures are preserved and are not counted as
regression evidence. A disposable harness excludes unrelated test modules
and functions; reversing only those attributes yields byte-identical source
to the candidate across all 534 Rust source files. The harness runs the
same regression functions and production bodies. Its exclusions are not
part of this branch, and its results do not substitute for full CI gates.

All builds/tests use `CARGO_BUILD_JOBS=2`, run serially, and preserve commands,
revision, source patch/hash, environment, output, and exit status. All **17
recorded local gates passed**. The tested patch SHA-256 was
`dfde02dd232a2c5ef21dfa609c74bd0f5603f91157348d31b1e8a60bba383fba`, verified
against the complete tracked/untracked patch before committing. Production
source was unchanged between those runs and the fix commits.

The gate environment was `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
CARGO_PROFILE_DEV_DEBUG=0`, with `CARGO_TARGET_DIR` set to this worktree's
absolute `target` directory. Exact commands and results:

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | exit 0 |
| `bash -n docker/compose-xfstests/run_perf_in_container.sh` | exit 0 |
| `bash -n docker/compose-xfstests/run_redis_perf.sh` | exit 0 |
| `bash -n docker/compose-xfstests/run_juicefs_perf_in_container.sh` | exit 0 |
| `bash -n docker/compose-xfstests/run_juicefs_perf.sh` | exit 0 |
| `bash docker/compose-xfstests/test_perf_report_delta.sh` | exit 0 |
| `bash docker/compose-xfstests/test_juicefs_direct_matrix.sh` | exit 0 |
| `bash docker/compose-xfstests/test_juicefs_perf_report.sh` | exit 0 |
| `cargo check --workspace --features workspace-overlay` | exit 0 |
| `cargo clippy --workspace --all-targets --features workspace-overlay -- -D warnings` | exit 0 |
| `cargo check --workspace` | exit 0 |
| `cargo build --workspace` | exit 0 |
| `cargo check -p brewfs --no-default-features --features fuse-tokio-runtime` | exit 0 |
| `cargo check -p brewfs --no-default-features --features fuse-io-uring-runtime` | exit 0 |
| `cargo test --workspace --lib --bins -- --test-threads=1` | 1,196 passed, 0 failed, 225 ignored; exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `git diff --check` | exit 0 |

Interrupted attempts are preserved separately; another task starting a
concurrent build caused those cancellations, not a source failure. Vendor
warnings remain in the complete output; the requested strict clippy commands
both exited 0. The 225 ignored workspace tests are not counted as passes.
Broader focused module suites and overlay acceptance are still pending.
No throughput benchmark or overall performance acceptance is claimed yet.

## Fix commits

| Issues | Commit | Scope |
| --- | --- | --- |
| #194, #195, #196 | `077a5a3` | Entity object reads, readdirplus attributes, and clean release |
| #198, #199, #200, #202 | `ebd0397` | Packed cache fills/invalidation, directory checkpoints, and parent snapshot reuse |

Every listed issue has a matching baseline failure and three passing
after-runs. The full 13-case regression/guard suite passed in each run.

## Issues requiring design changes

- **#192:** Named open resolves the selected inode before acquiring its FIFO
  lock, then clears `read_covered`. The expanded snapshot proves atomic
  agreement between old parent/dentry observations and newly discovered
  inode/permission rows, with fresh authority and backend time. Removing
  this reread requires a backend read-generation or conditional-read
  contract; cached hints alone do not prove the same view.
- **#193:** Chunk observations discover ownership/descriptors. Prefetch proves
  their joint view, and post-transport validation rechecks metadata and
  lifecycle authority using backend time. Delta-only reads cannot replace
  these fences under the existing API. A generation contract must account
  for expiry, concurrent changes, and ABA.
- **#197:** Chunk keys use unpadded decimal indices: lexical order differs
  from the numeric recovery cursor. Seeking lexically at that cursor can
  skip live rows, and recovery currently verifies stale bytes behind the
  cursor. A bounded numeric scan or batch/cursor contract is required;
  this base also predates the separate #188 correctness fix.
- **#201:** Each cold ancestor's parent ID is discovered from that ancestor's
  current row. Collecting IDs first already costs one dependent read per
  ancestor. Stale cached/lower hints cannot authorize a moved directory.
  Batching requires a validated discovery contract, not merely the existing
  mutation CAS.

These four issues remain open and unpatched; no unsafe shortcut is proposed.
