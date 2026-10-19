# Current-entity namespace: stage 2 implementation checkpoint

Updated 2026-10-20 (Asia/Shanghai).

Experiment branch: `codex/overlay-entity-namespace-20261019`, based on `b6f297e`.
Integration branch: `codex/packed-metadata-aliyun-20260930`, unchanged.
This is correctness work. No performance benchmark or performance claim belongs
to this stage.

Code commits, validated together as the frozen source snapshot described below:

- `1f0db7c`: explicit format isolation, current rows, keys, codec and resolution.
- `ac158b1`: bounded memory/Redis transactions, lifecycle and ownership safety.
- `06f6d5e`: atomic namespace operations, noatime handles and focused tests.

The integration branch remains at `6b54270`; no merge was performed.

## Scope and conservative interpretation

The architecture calls stage 2 isolated storage/resolution and namespace
operations; implementation-spec stage 3 owns the production backend/VFS path.
This experiment implements the former plus a real Redis transaction substrate.
`--volume-format workspace-entity-v1` is recognized explicitly, but production
mounting fails closed. Existing implicit workspace selection remains
`workspace-v1`, and old format records are not reinterpreted or migrated.

In particular, reference lower-pin keys cannot protect objects from the existing
packed collectors. An authenticated packed reader/ownership bridge must be
qualified before mount admission. Calling the reference lower interface or
marking an upload complete is not proof of real object durability. No object
PUT/DELETE transport or fsync claim is made here.

## Implemented boundary

- Independent `entity/` modules; no dependency on the old head, layer sequence,
  mutation mutex, commit-view cache or group-commit path.
- Workspace-prefixed inode/dentry/xattr/chunk/orphan rows; volume-scoped immutable
  slice identities. Explicit tombstones mask lower values. Bounded complete chunk
  maps contain Data, LowerRange and Hole spans and normalize overwritten ranges.
- Plain signed-64-bit INCR allocation, seeded above lower inode IDs. Redis GET
  returns the exact decimal result, including above 2^53. Exhaustion fails;
  aborted operations can leave harmless allocator gaps. ID reservation is a
  separate authority-checked transaction; final create atomically validates
  absence/permissions and installs all visible namespace changes.
- Memory reference backend and Redis bounded row transactions. Types, packet
  bounds, predicate collisions and counter overflow are checked before effects.
  All keys in a transaction share one volume hash slot. Exact predicates cover
  affected entities and current lifecycle authority, without rewriting a shared
  head. Redis still executes Lua serially.
- Create/O_EXCL, rename/replace, hardlink, unlink/rmdir, chmod and xattr/ACL changes.
  Local affected-inode/parent locks use ascending inode order. Mutable ancestor,
  access/default ACL, sticky and setgid decisions participate in the read set.
  A conflicting predicate redoes the whole decision; ambiguous backend failures
  do not replay a mutation. Owned tasks retain guards after caller cancellation.
- Explicit noatime open: permission/lease validation uses a transaction with zero
  writes. Close releases a local handle and never changes linked metadata.
  O_TRUNC is not an ordinary open in this API: it remains a separate data mutation
  for stage 3, and no mounted new-format O_TRUNC path is admitted here.
- Durable whole-lower owners, writer identity/backend expiry, pending uploads,
  chunk-to-slice memberships and refcounts, retirement holds, deletion reservations
  and object-ID tombstones. Open handles and upload tokens hold local admission
  guards. Maintenance waits for these obligations to drain.
- Last unlink atomically records an orphan even with no open handles. Quiescent
  recovery removes one chunk and its membership/refcount/cursor in one transaction,
  then tombstones the inode and clears its orphan. Physical deletion is separately
  reserved and acknowledged; lower references are never upper ownership.

## Limits that remain explicit

The lower interface has no production packed adapter yet. The reference registry
and pin rows are not a substitute for integration with existing lower/GC roots.
TiKV/SQLx and Redis Cluster execution are not qualified by standalone Redis tests.
The physical upper-object naming scheme must be disjoint from legacy/lower slice
IDs, or receive an authenticated allocator handoff. The reference slice counter
starts at zero in its new metadata volume domain; that alone does not prove
uniqueness in an existing object-store keyspace.

This is a namespace API subset: rename exchange, chown, Linux capabilities,
symlink traversal, special-file open and XATTR_CREATE/XATTR_REPLACE entry points
are not implemented.
Device-node creation uses a conservative root-only reference rule. Callers supply
validated credentials and inode identities; the future VFS adapter must supply
the complete kernel/path authorization contract.

Only a fresh Ready namespace can acquire a writer. Expiry fences writes but is
not proof that crashed/stalled readers or uploads stopped. There is no automatic
expired-writer takeover or reopening after maintenance; a cross-process fencing
and recovery protocol remains necessary. Quiescence tokens are local admission
proofs, not fabricated process-death certificates.
If the writer lease expires while drain is waiting, maintenance fails closed
even after the local guards finish; the missing external recovery authority
must not be replaced by an assumption that elapsed time killed those readers.

Scans are bounded to 4096 returned rows and 16 MiB, and Redis scan work is bounded
to 2048 pages of COUNT 128. A large directory or unrelated database can fail closed.
Chunk maps admit at most 4096 spans. Ownership plan builders handle one chunk per
transaction; callers must aggregate shared slice deltas before combining chunks.
These bounds need paged production adapters, not silent truncation.

The normalized map helpers do not implement VFS writes, shrink recovery, append,
hole-punch or fsync. `lower_visible_size` masks absent-chunk inheritance; an
existing map must be pruned/holed before re-extension after shrink. Stage 3 must
implement the specified durable truncate protocol and prevent stale upload
publication. No file-data writes are admitted through the new mount format.

Retired upper data stays protected until proven maintenance. Final close may
leave its durable orphan for that walker. Empty-namespace discard releases the
lower owner last and refuses residual entities; a general namespace sweeper and
immutable publication transfer remain future work. Lost administrative replies
require reconciliation, not counter reset or pointer rewind.

## Validation record

Artifact directory:
`/mnt/brewfs-data/artifacts/overlay-entity-stage2-20261019`.

Storage/resolution checkpoint: the initial
`cargo check --workspace --features workspace-overlay` passed (2m24s), including
the reference schema, resolution, lifecycle and backend substrate. The completed
namespace and final local gates are recorded below.

Namespace checkpoint: all **53 focused entity tests passed**, including the
three explicitly selected Redis integration tests (0 failed, 0 ignored; 0.43s
test execution after 16m43s build/execution). This includes 20 namespace tests
covering O_EXCL, rename/link/unlink, permissions and noatime behavior. The Redis
namespace oracle observes one winner and 31 `EntityError::Exists` outcomes for 32 concurrent
creates of the same name. `entity-tests.log` records the result. This qualifies
the reference transaction contract, not a mounted filesystem or object transport.

The full overlay library gate completed with **2306 passed, 5 failed, 378
ignored** (573.83s execution). It is not green. The failure names and assertions
match previously recorded failures in unchanged legacy modules:

- `g13a_kv_released_lease_is_not_an_expired_recovery_root`
  (`g13_kv_gc_tests.rs:119`).
- `kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`
  (`kv_store.rs:6837`).
- `migration_marker_fences_entity_and_hot_native_packets_without_writes`
  (`kv_store.rs:8617`).
- `source_mutation_replacement_symlink_and_limits_fail_closed`
  (`source_file.rs:655`, historical r30 flake).
- `source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`
  (`source_file.rs:587`, historical hot27/r31 flake).

The new entity tests passed in this full run as well; the three ignored Redis
cases were executed by the focused gate above. Cargo stopped after the failing
library target, so that command alone does not certify the remaining binary
targets. Existing-format failures remain open work, not new-format acceptance
successes.

The required default-workspace gate
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins`
passed: **1179 passed, 0 failed, 225 ignored** (70.79s library test execution),
and both binary targets passed with zero tests. The additional default-format
test verifies explicit entity-format selection and fail-closed mount admission.

The completed compile/lint gates are:

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check`; `git diff --check` | Passed |
| Workspace check, with default features and with `workspace-overlay` | Passed, including final frozen overlay source |
| `cargo build --workspace` | Passed |
| Strict `cargo clippy --workspace --all-targets -- -D warnings`, default and overlay | Passed, including final frozen overlay source |
| No-default-feature Tokio and io_uring checks, each with/without overlay | All four passed |

The separate overlay binary gate passed: all **9 snapshot-fixture tests** passed,
and the main/stats targets passed with zero tests. `test-overlay-bins.log`
completes the binary coverage that the failed full library command did not run.
Cargo metadata confirms the only workspace members are `brewfs` and
`brewfs-stats`; the conditional `rfuse3` workspace-member gate does not apply.

Each source-file case passed once in isolation on both the preserved old
executable and the current workspace executable. The assertions above exactly
match the historical failures, and `source_file.rs` has the same blob since
`b5f9606`. See `baseline-replay/source-file-comparison.md` and
`source-file-current-replay/`. These replays document intermittency; they do not
replace the failed full gate or establish the timing cause.

The first focused-test compilation was interrupted before executing any tests
when the safety review changed session authority to include the full volume
prefix. Its partial log is `entity-tests-interrupted-compile.log`, not test
evidence. Initial strict clippy found two style errors, both corrected before
the final gate. Shell syntax and the three requested perf-report harness tests
passed without running a benchmark. The remaining 14 non-Rust workflow checks
also passed: 11 shell syntax checks, the packed-runner TTL test, four small-file
scanner tests and 30 packed-tool Python tests (`script-gates/`).

Host: `brewfs-singapore-v2`, Linux 6.8.0-139-generic x86_64, Rust/Cargo 1.99.0.
The initial disk check found 49 GiB available. Builds ran serially with four jobs,
incremental/debug information disabled, and a systemd user scope capped at
28 GiB RAM with 24 GiB swap. The isolated Redis oracle uses `redis:7.2-alpine`
(local image `29e8589c3f9b`), persistence disabled, bound to loopback only.
At completion, 48 GiB was free and the isolated Redis container
`brewfs-entity-backend-test-20261019` was stopped. The unrelated untracked
`|RwLock` file was retained unchanged.

Commands and exit statuses are retained in `commands.log` and `status.tsv`.
The frozen Rust diff against `b6f297e` is preserved as `candidate-source.patch`
with `candidate-source.sha256`; gates run before the logical commits, so build
provenance embeds the experiment branch and dirty base rather than a later
commit ID. Documentation edits do not alter that Rust snapshot.
The serial drivers are `run-gates.sh` and `run-final-gates.sh` in the artifact
directory. The latter checks the frozen overlay source and executes binary
targets skipped when the full library gate failed. Focused reference/Redis
reproduction, using an isolated disposable fixture, is:

```bash
docker run -d --rm --name brewfs-entity-stage2-repro \
  -p 127.0.0.1::6379 redis:7.2-alpine \
  redis-server --save '' --appendonly no
entity_redis_address=$(docker port brewfs-entity-stage2-repro 6379/tcp)
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export BREWFS_TEST_REDIS_URL="redis://${entity_redis_address}/"
cargo test -p brewfs --features workspace-overlay --lib workspace_overlay::entity -- --include-ignored
docker stop brewfs-entity-stage2-repro
```

The Redis tests use fresh volume UUIDs, clean their exact keys and explicitly
require the endpoint when their ignored integration cases are selected.
The recorded run used `redis://127.0.0.1:32768/`.

Historical feature-gate comparison: the three known r31 overlay failures are
preserved in the old `build/r31/test-overlay.log` under the
`overlay-packed-lifecycle-e980d59-20261012T153232Z` artifact root. Direct replay
of all three against a preserved pre-r32 executable also failed at the same
assertions. `baseline-replay/provenance.md` records its SHA256 and its embedded
**dirty r31-era `7149664`** provenance; it is not represented as a pristine
`b6f297e` executable. The historical `source_file` flake belongs to the overlay
feature suite, not the default workspace suite.

No release build, release benchmark, real FUSE mount or conversion is part of
this checkpoint. Tests of the existing runtime are regression evidence only;
they cannot qualify the missing new-format VFS path.
