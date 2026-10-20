# Entity mount performance experiment, 2026-10-20

Branch: `codex/overlay-entity-namespace-20261019`, continuing from `44888ed`.
The integration branch `codex/packed-metadata-aliyun-20260930` remains unchanged.
Implementation delivery commits are `24d93d3` (namespace mount operations and
metadata-only tests) and `782fc8e` (fresh packed bootstrap, explicit opt-in and
FUSE adapter). The following runner/documentation commit records the completed
experiment. None of these commits is merged to the integration branch, whose
local and remote tip remains `6b542707b978e7d9791b8ae59a768c7c19e58f28`.

The owner's final scope for this turn is performance measurement, with minimum
opt-in FUSE wiring and one create/write/fsync/byte-verified-readback/unmount
sanity check per available arm. The unfinished enforced adapter-pairing, offline admission,
paginated-cleanup and combined-stack qualification changes were removed before
building. No crash injection, takeover simulation, importer or cutover is added.
Existing durable-intent tests and data APIs remain.

## Implemented wiring

The explicit `--volume-format workspace-entity-v1 --experimental-entity-mount`
path creates a fresh experimental namespace over a standalone packed manifest.
It requires a non-nil entity volume/workspace UUID and Redis/S3 configuration.
The existing packed reader, lower-owner registration and entity lease APIs are
used during setup. The dedicated FUSE adapter calls current-entity namespace
and file-I/O APIs; normal opens do not update atime. Local inode handles retain
their open permissions, and create returns its handle after committed visibility.
The legacy format/default dispatch is unchanged.

Orderly unmount stops request admission, clears the retained handle registry
and drains existing owned operations while lease renewal remains alive. Upper
data and durable lower ownership are retained. The benchmark harness owns its
dedicated services and removes those resources after unmount. Harness cleanup
is not evidence of namespace GC or process-crash recovery.

## Measurement protocol and status

Artifacts: `/mnt/brewfs-data/artifacts/overlay-entity-perf-20261020`.
Completed group: `entity-mount-20261020T062722Z-f0bb328c-de32-4387-9266-47ae55290c70`.
`summary.json` records `completed-available-preliminary`, exit 0: all 24
available phase rows completed with zero errors and 2,000 files each. The 12
requested `overlay-native` phase slots are explicitly skipped.

Use the unchanged `tools/perf/smallfiles_rw.py` workload used by
`tools/perf/run_overlay_vs_native.sh`: 2,000 files, 16 KiB/file, 20 directories,
O_EXCL create, full write/fsync/close timing and deterministic byte-verified
readback. The final requested arms are native flat-v1, entity overlay on frozen
native lower (`overlay-native`), and entity overlay on packed-v3 lower
(`overlay-packed`), all using Redis and RustFS. Compare available arms at
1/16/32 client workers in both arm orders. All rows are preliminary,
not performance acceptance results. R31 workspace-v1 is not a comparison arm.

`overlay-native` is skipped: the only real entity `ImmutableLower` and
`ImmutableLowerData` implementation is `PackedLower`. There is no adapter for
frozen native Redis inode/dentry/xattr/chunk metadata and RustFS data, no native
lower binding in the entity descriptor, and no experimental mount dispatch
for that binding. Native-base/frozen metadata code in other format paths does
not implement these entity traits. Building that missing arm would require new
lower integration outside the final scope; it is not replaced by a legacy
overlay or a packed metadata import.

All arms use Redis/RustFS, release executables, disabled payload caches,
zero metadata TTL and direct I/O. This experiment uses 4 MiB chunks/blocks for
available arms because entity I/O has a bounded chunk size. Entity FUSE also uses a 1 MiB request cap and zero
readahead, versus legacy 4 MiB and 16 MiB. Those limits are inactive for this
16 KiB direct-I/O workload. Kernel permission checking differs: entity uses
its namespace permission checks and disables `default_permissions`; legacy
enables it. Results must not be described as identical implementations or
substituted into the historical accepted tables.

The preserved packed-v3 fixture executable is used only to publish the lower
graph into RustFS. The current release executable runs both available arms.

### Preliminary results, not acceptance results

Each cell below is the median of two samples, one from each arm order. Units
are files/s. Every measured row is **preliminary**. Neither these results nor
the smoke qualify production admission or establish a performance win.

| Status | Workers | Phase | native | overlay-native | overlay-packed | packed/native |
| --- | ---: | --- | ---: | --- | ---: | ---: |
| Preliminary | 1 | write | 51.63 | skipped | 20.08 | 0.389 |
| Preliminary | 1 | readback | 113.40 | skipped | 32.04 | 0.283 |
| Preliminary | 16 | write | 351.03 | skipped | 111.90 | 0.319 |
| Preliminary | 16 | readback | 602.50 | skipped | 173.05 | 0.287 |
| Preliminary | 32 | write | 419.83 | skipped | 135.00 | 0.322 |
| Preliminary | 32 | readback | 616.47 | skipped | 199.05 | 0.323 |

Raw samples below follow execution order: repetition 1 runs native then
overlay-packed; repetition 2 runs overlay-packed then native.

| Status | Workers | Phase | native samples | overlay-packed samples |
| --- | ---: | --- | --- | --- |
| Preliminary | 1 | write | 51.358659, 51.907830 | 19.973833, 20.190344 |
| Preliminary | 1 | readback | 111.932570, 114.868809 | 32.341227, 31.747102 |
| Preliminary | 16 | write | 368.790675, 333.261976 | 114.246639, 109.545171 |
| Preliminary | 16 | readback | 635.603556, 569.395203 | 171.856106, 174.250484 |
| Preliminary | 32 | write | 418.893113, 420.757842 | 135.925242, 134.075311 |
| Preliminary | 32 | readback | 612.362775, 620.584829 | 199.889584, 198.210428 |

Entity write throughput rises from 111.90 to 135.00 files/s between 16 and 32
workers (20.6%); native rises 19.6% over the same worker change. The entity path
still achieves only about 0.32x native. This does **not demonstrate removal of
the write plateau** or identify the remaining binding constraint. Historical
r31 achieved 216.89/236.89 files/s at 16/32 workers, but r31 was not a measured
arm in this run; different geometry and implementation mean this is context,
not a matched r31 regression percentage. No accepted table is replaced.

The timed workload creates new upper files and reads those same upper bytes.
It does not measure lower reads or copy-up, despite the packed lower being
bound in the overlay arm. There are only two samples per cell; there is no
mixed/stat/full, POSIX, namespace-GC or crash qualification in this run.

### Smoke and cleanup evidence

`smoke/native/smoke.json` and `smoke/overlay-packed/smoke.json` each record one
successful O_EXCL create, write, fsync and exact 16 KiB readback. Both mounts
then exited and unmounted cleanly. Every one of the 14 mounts (two smokes and
12 comparison cells) has an `unmount-proof.txt` with `process_exit=0`,
`forced_kill=0`, `process_alive=false` and `mounted=false`.

The final `cleanup-proof.txt` records successful Compose teardown and resource
queries, empty container/volume lists, zero remaining FUSE mounts and
`all_mount_processes_clean=1`. `compose-down.log` records removal of the
dedicated Redis/RustFS containers and RustFS volume. This is harness resource
cleanup, not proof of per-namespace object reclamation. No additional smoke
scenarios were run.

### Reproduction and source identity

From this branch, build the overlay-enabled release binary, then use the
existing small-file workload through the new adapter-aware harness:

```bash
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 \
  cargo build --release --features workspace-overlay --bin brewfs
ENTITY_MOUNT_ARTIFACT_ROOT=/mnt/brewfs-data/artifacts/overlay-entity-perf-repro \
ENTITY_MOUNT_BINARY=/mnt/brewfs-data/src/brewfs/target/release/brewfs \
  bash tools/perf/run_entity_mount_comparison.sh
```

The runner requires the preserved packed fixture producer named by
`ENTITY_MOUNT_FIXTURE` (its recorded default is under
`overlay-packed-lifecycle-e980d59-20261012T153232Z/build/r31`), Docker Compose,
Python with boto3, Linux FUSE, `allow_other` and passwordless sudo. It creates
fresh Redis/RustFS services and buckets, runs the two smokes and ordered matrix,
and removes its owned services/volumes. Per-mount YAML, exact commands, service
image identities, phase JSON and teardown proofs remain in the group directory.
The original command used `ENTITY_MOUNT_ARTIFACT_ROOT` above and
`ENTITY_MOUNT_BINARY=/mnt/brewfs-data/artifacts/overlay-entity-perf-20261020/build/brewfs`.

The measured binary SHA-256 is
`39cca3a5c6adacc344fd7118bd81aff421d54e046e9dcdc59c8a325fa3e80d15`.
It was built from `44888ed` plus the mount-wiring source changes, before their
delivery commits. `build/rust-source-sha256.json` and the group's matching
source manifest identify all 720 Rust files, including new untracked files;
`build/revision.txt`, worktree patches and `binary.sha256` retain provenance.
The release build passed in 71m55s with the repository's release debuginfo
setting. Both measured arms used this exact binary;
the preserved r31 binary only produced the immutable lower fixture.
The host has eight logical CPUs and 30 GiB RAM, runs Linux 6.8.0-139-generic
and rustc 1.99.0 (`b940084d7`, 2026-09-28). About 50 GiB remained on the
251 GiB data filesystem during the post-measurement gate. The group's
`backend-image-ids.txt` records Redis image `29e8589c3f9b…` and RustFS image
`1803faef5762…`; the complete image IDs are retained there.

Direct CLI users must also specify bounded geometry, for example
`--chunk-size 4194304 --block-size 4194304`: the normal 64 MiB chunk default
exceeds the experimental entity API's 16 MiB maximum. The runner sets the
4 MiB geometry explicitly in YAML.

### Local gate

The overlay workspace check and shell/report checks passed before measurement.
The post-measurement default workspace build/test, strict default/overlay
clippy and focused new-mount tests are recorded in `build/gate.tsv` with
individual command logs. The default workspace library/binary test gate passed:
1,179 passed, zero failed, 225 ignored (65.90 seconds of test execution).
All selected gate commands passed:

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo check --workspace --features workspace-overlay` | pass before measurement |
| Default `cargo check --workspace` and `cargo build --workspace` | pass |
| No-default-feature checks for `fuse-tokio-runtime` and `fuse-io-uring-runtime` | pass |
| `cargo test --workspace --lib --bins` | 1,179 passed, zero failed, 225 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| Same strict clippy with `--features workspace-overlay` | pass |
| `memory_entity_mount_namespace_operations` with the overlay feature | one passed, zero failed |
| `experimental_entity_mount_requires_explicit_fresh_inputs` with the overlay feature | one passed, zero failed |
| Required four perf-shell syntax checks and three report/matrix script tests | pass |
| New runner shell/embedded Python syntax and `git diff --check` | pass |

The gate uses `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0`; the exact command sequence is in `build/gate.sh`.
The focused overlay library executable took 16m31s to compile; both selected
tests then completed successfully. The ignored Redis namespace unit test was
not selected in this turn. Actual Redis/RustFS coverage here is the two real
mount smokes and measured workload, not a claim about the entire service suite.

## Known limitations

Production admission and reopening a persistent entity workspace remain
disabled. Fresh dedicated fixture scopes are a harness precondition; there
is no enforced adapter pairing, offline proof about external old writers or
readers, or cross-catalog ownership-transfer protocol. Process-crash takeover
authority is unproven and remains an admission blocker. Cleanup remains bounded
and has no new pagination; combined-stack qualification is not claimed.
Offline conversion and atomic cutover are deferred; existing old workspaces
continue using their old format.

Same-parent creates still serialize on the affected parent lock across backend
operations. Directory enumeration is bounded by the transaction's row count
(approximately 2,045 upper children when inode and dentry checks are needed).
The adapter intentionally supports only a subset of setattr and does not
establish general POSIX qualification.
Review also found that the adapter retains the initial `O_APPEND` value and
does not honor later `fcntl(F_SETFL)` changes; its readdir authorization
requires execute as well as read permission. Neither behavior is exercised by
this root-only O_EXCL workload. They remain known prototype defects; no extra
scenario or POSIX repair was added to the owner's final measurement scope.

Historical failures remain documented in the
[data checkpoint](2026-10-20-overlay-entity-data.md): three established legacy
overlay failures and two known source-file flakes. They must not be presented
as regressions introduced by this mount wiring.
The full overlay/all-features suites and separate operator gate are not rerun
here; the historical operator `clippy::double_must_use` issue is outside the
workspace clippy checks. The data checkpoint also retains the LocalFs inventory
(146 constructor sites in 56 files, including 95 directly constructing tests).
This mount work adds no LocalFs constructor or object-store mock: its new unit
tests are metadata-only, and all mounted object I/O uses the S3 adapter against
the owned RustFS service.
