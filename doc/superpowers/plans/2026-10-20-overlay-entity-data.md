# Entity overlay: packed lower, file I/O and recovery

Experiment: `codex/overlay-entity-namespace-20261019`, continuing from `1c55148`
and ultimately based on `b6f297e`. Integration branch
`codex/packed-metadata-aliyun-20260930` remains unchanged at `6b54270`.
Production admission for `workspace-entity-v1` remains disabled. No benchmark,
performance comparison, conversion or merge is part of this continuation.

The implementation is split into these commits; the gates below validate their
combined source, with checksums tying the pre-commit artifacts to the final tree:

| Commit | Change |
| --- | --- |
| `db4ad57` | Strict durable create-only S3 publication |
| `fda36bf` | Truncate/retirement recovery and shared inode coordination |
| `eada219` | Entity current-map file I/O and shared RustFS test client |
| `a383b1c` | Authenticated packed lower and durable collector-visible owners |
| `66908e5` | VFS SDK dispatch and FUSE/native handle isolation |
| `4b7635d` | Isolated Redis/RustFS functional gate |

This document supersedes the missing-data-path list in the
[previous stage-2 checkpoint](2026-10-19-overlay-entity-stage2.md); that
checkpoint's historical validation record remains intact.

## Implemented contract

The packed adapter authenticates the manifest and its exact inode boundary
through the existing packed-v3 reader. Attributes, raw dentries, xattrs and
payload reads use that reader. Current chunk maps retain immutable `LowerRange`
locators; object, authentication and transport errors never become absence.
Upper rows and tombstones suppress lower fallback. Stored lower ranges cannot
move bytes from another inode or expose bytes past `lower_visible_size`.

A collector-visible durable owner is acquired from a live packed-reader pin
before attach. It participates in the existing packed/native root census and
root-generation check, protecting packed objects and borrowed native roots
after the source reader lease expires. This administrative root fence is not
an entity mutation head. Terminal owner records prevent delayed reattachment
after release. Release requires the entity lifecycle's proof that this exact
volume/namespace/lower has completed quiescent deletion. Acquiring the owner
before constructing the namespace can retain an extra owner after interruption;
the caller must not expose the namespace before that acquisition succeeds.

The SDK currently composes these adapters through trusted constructors. Callers
must match the packed owner's volume/namespace IDs to `EntityKeys`, match the
lower binding, and use the authoritative entity/packed backend pairing. A
deletion proof must come from that same entity namespace backend. The generic
lower trait checks manifest identity, not owner namespace identity; a future
production factory must enforce this pairing. Standalone `PackedLower` reads
outside an entity session require caller-managed quiescence before owner release.

`EntityData` provides write-through file I/O. It persists chunk geometry once,
then materializes mapping metadata on first modification. Each uploaded chunk
replaces only its written interval. Object identity, checksum, current map,
inode size and distinct chunk-to-slice memberships are published together after
durable create-only PUT. Upper physical keys use a separate
`workspace-entity-v1/<volume>/<namespace>/<slice>` domain. An allocator value
alone is never treated as uniqueness in the legacy object keyspace.

All namespace wrappers for one writer session share inode locks. A file write
holds its inode lock through upload and publication; another inode remains
independent. Append chooses EOF while holding this lock. Multi-chunk failure
returns only the acknowledged prefix and retains the I/O error for fsync.
Owned tasks retain their handle/admission/lock after caller cancellation.
Open access survives chmod/unlink, and foreign or insufficient handles fail.

The existing VFS handle read/write/flush/fsync/close methods have an explicit
experimental entity dispatch. `attach_entity_file` requires the entity format
and an already authorized bound file; `truncate_entity_file` supplies its
O_TRUNC/ftruncate operation. These handles bypass legacy slice history, caches,
head fences and mutation drivers. Path operations use `EntityNamespace`.
This SDK seam is not a general entity mount factory, and inode-only kernel
writeback/mmap entry points are not admitted for this format.
Every FUSE entry point accepting a live entity handle rejects it before native
inode, page-cache or lock-table side effects. Tests use colliding and mismatched
native inode numbers to check this boundary. Closed handles are removed; this
does not change legacy stale-handle or inode-only request validation.

Shrink atomically publishes the smaller size, the monotonic lower boundary and
an inode-local truncate intent. Cleanup replaces the boundary tail with holes
and removes later current maps with their ownership changes. Each step advances
the durable cursor in the same transaction. Reads honor the visible cut;
writes/extensions finish cleanup first. Namespace mutations check the absence
of this intent, so they cannot change the cut's recovery fence. Extension
creates holes and never increases lower visibility.

File fsync waits behind preceding inode operations, reports retained I/O failures,
then completes pending truncate cleanup and validates current lease/metadata
authority. The implementation has no deferred file-data buffer. Strict S3 PUT
uses conditional creation and the service's acknowledged durability policy.
An S3 service returning unsupported conditional PUT fails closed; the old OSS
unconditional fallback is not used by entity writes. Redis metadata durability
still depends on deployment persistence/replication policy. The LocalFs adapter
has no entity durable-PUT implementation; it is not a qualified substitute.

Maintenance closes admission and drains retained reads, handles and uploads.
Pending uploads, orphan chunks and namespace discard have durable replay state;
chunk removal and owner decrements commit together, and underflow fails before
effects. Object deletion requires a reservation, a nonreplaying transport and
acknowledgment before the permanent object-ID tombstone. Lower ownership is
released last. Retired objects remain protected until maintenance proves that
the last reader has finished.

## Conservative boundaries

Crash injection reconstructs adapters/recovery walkers against surviving
metadata and injects lost commit replies. It does not establish remote process
death, host power-loss behavior, or safe timeout-only writer takeover. A local
maintenance token proves an actual drain; lease expiry alone never supplies
that proof. A completed local drain permits cleanup followed by a new writer
generation. A lost successful resume reply remains uncertain and fails closed;
it cannot manufacture a second independent session from stored lease bytes.
Production admission remains disabled pending external recovery authority and
mounted qualification.

The reference scans remain bounded to 4096 rows/16 MiB; large namespaces fail
closed instead of truncating cleanup. Maps have at most 4096 spans. File requests
are bounded to 16 MiB and 64 chunks; chunk geometry is at most 16 MiB. These are
explicit experiment bounds, not scalable paginated production implementations.
General conversion, immutable export/ownership transfer, other metadata backend
qualification, full namespace/POSIX parity and real FUSE qualification remain.
The namespace subset still lacks rename exchange, chown, capability handling,
symlink traversal, special-file opens and XATTR_CREATE/XATTR_REPLACE entry points.
TiKV, SQLx and Redis Cluster are not qualified by standalone Redis tests.

There are at most 256 active permanent entity lower owners per existing packed
store. Released owner tombstones are retained. First attach still requires a
live packed-reader pin; an O(1) fork directly from a permanent entity owner is
not implemented. Existing owners can reopen without reviving the source lease.

The shared packed frame reader currently string-wraps payload checksum failures
as a backend error. The adapter propagates that error; it never substitutes
absence or zero bytes. The payload-tampering test requires the specific
`packed v3 hash mismatch for wire005 coalesced frame` reason, allowing its
existing backend wrapper or a typed corruption error. This continuation does
not change the shared reader's error taxonomy.

## Object test policy and existing LocalFs inventory

New object-I/O tests use the production `S3Backend` against a disposable real
RustFS service. The shared `entity/test_objects.rs` helper requires explicit
`BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT`, `BREWFS_PACKED_GRAPH_TEST_S3_BUCKET`,
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`. It follows the existing packed
graph audit's loopback HTTP endpoint and `brewfs-graph-test-<UUID>` bucket
contract. Bucket creation runs once per test executable; individual fixtures
use distinct object identities. Each fixture gets its own SDK client so it
cannot retain pooled connections on another test's finished Tokio runtime.
Both SDK and adapter mutation retries are disabled. The harness must supply a
fresh bucket per executable invocation and clean up its disposable service.

These tests are ignored by default and fail when explicitly selected without
the required configuration. Object wrappers may pause or inject a failure at
a real request boundary; their object bytes still pass through S3/RustFS.
Memory metadata/lower fixtures remain suitable for transaction and mapping
unit contracts, but do not establish object transport or physical durability.
Local temporary directories used for producer spool/source input are not object
backends. No new test constructs `LocalFsBackend`.

Coverage is split across fixtures. `recovery_tests.rs` uses byte arrays as a
mapping oracle and acknowledges deletion tokens without an object backend;
its memory/Redis runs test metadata transitions, cursor replay and ownership
invariants, not physical deletion or persisted payloads. Separate `data_tests`
exercise actual S3 PUT/GET/DELETE, including an ambiguous delete reply. The
packed-lower tests compose real packed payloads with memory entity metadata;
their Redis variant puts the packed owner/census in Redis but retains memory
entity metadata. Redis entity data and VFS fixtures use a synthetic immutable
lower. These suites do not yet qualify the combined Redis entity + real packed
lower + VFS stack. The packed native-root test isolates the durable owner from
other publication roots; it is not a complete physical packed-graph retirement
test. No fixture establishes process-kill or host-reboot recovery.

The [tracked functional runner](../../../tools/testing/run_entity_data_tests.sh)
starts and removes its own Redis/RustFS Compose project, supplies the required
credentials, and runs `packed_entity_lower`,
`workspace_overlay::entity` and `entity_vfs` sequentially with `--include-ignored
--test-threads=1`. It rejects zero-test or still-ignored suites and saves
per-filter counts/logs, source/toolchain/image provenance and cleanup proof.
Its Rust checksum manifest includes tracked and nonignored untracked source
and test files, so an uncommitted continuation is not represented only by its
tracked diff.
It compiles the feature-enabled library tests; it runs no benchmark or FUSE mount:

```bash
ENTITY_DATA_ARTIFACT_ROOT=/absolute/path/to/artifacts \
  PACKED_LIFECYCLE_REDIS_IMAGE=redis:7.2-alpine \
  PACKED_LIFECYCLE_RUSTFS_IMAGE=rustfs/rustfs:latest \
  bash tools/testing/run_entity_data_tests.sh
```

The runner requires Linux, Docker Compose v2 (direct Docker access or passwordless
sudo), Python 3 and the repository's Rust build dependencies. Its shell syntax
was checked; the validation record below identifies the actual executed gates.

The committed baseline `1c551481a856dfcddffd414add614d8d48a4895f` contains
**146 `LocalFsBackend::new` constructor sites in 56 Rust files**:

| Scope | Constructor sites | Interpretation |
| --- | ---: | --- |
| Direct test-function bodies | 102 | 95 distinct `#[test]`/`#[tokio::test]` functions |
| Test-only helpers | 34 | 34 helper functions; their transitive test callers are not counted |
| Runtime/CLI/SDK | 5 | Snapshot fixture, CLI local client, HDFS local client, two VFS local builders |
| Examples | 2 | Local mount and persistence examples |
| Benchmark helpers | 2 | Local backend/client constructors |
| Fuzz helper | 1 | Local VFS constructor |

Thus **136 test-only constructor sites are not 136 test functions**, and the
95 directly constructing tests are not the total number of tests using LocalFs.
The inventory reads committed blobs with `git show`; it excludes this
continuation's drafts. Full paths, line numbers, enclosing functions and scope
are saved in `localfs-baseline-inventory/localfs-inventory.tsv` under the
evidence directory below, with a JSON summary and the reproducible
`localfs-baseline-inventory.py` script. The parser dependencies are
`tree-sitter==0.26.0` and `tree-sitter-rust==0.24.2`; from the repository root run
that script with `--rev 1c551481a856dfcddffd414add614d8d48a4895f --output <directory>`.
The raw site list can also be reproduced with
`git grep -n 'LocalFsBackend::new' 1c551481a856dfcddffd414add614d8d48a4895f -- '*.rs'`.

Further migration should start with shared packed producer/catalog/reader
fixtures, then lifecycle/GC/FUSE helpers, preserving each test's assertions
against the real service. Pure codec/mapping tests should remain independent of
object transport. This inventory does not exempt existing adapter tests from
later migration. Existing-test migration and any changes to the ten non-test
sites remain separate owner-directed work; no LocalFs hardening or existing
constructor migration is included in this continuation.

## Validation and reproduction

Evidence directory:
`/mnt/brewfs-data/artifacts/overlay-entity-data-20261020`.

The accepted focused run is
`entity-data-20261020T032430Z-508c9c14-be09-433b-8d4d-b1dfd6c83b9e` under that
directory. It tested the uncommitted source on `1c55148`; the Rust source
checksum manifest covers the actual files, including untracked new modules.
`committed-source-verification.json` in the evidence root verifies all 715 Rust
files against code commit `4b7635d`, with no changed, added or missing files.
The run's `summary.json`, `cases.tsv`, per-filter logs and `cleanup-proof.txt` record:

| Focused suite | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Packed lower / durable owner / authentication | 6 | 0 | 0 |
| Entity storage / namespace / data / recovery | 78 | 0 | 0 |
| Entity VFS SDK and FUSE exclusion boundary | 2 | 0 | 0 |

All 21 new object-I/O tests use real RustFS: 13 data tests, six packed-lower
tests and two VFS tests. Metadata-only Redis tests also run in this gate.
The runtime is rustc 1.99.0 (`b940084d7`), Cargo 1.99.0, x86_64 Linux, four
Cargo jobs, incremental compilation disabled and dev/test debug information
disabled. The owned Redis endpoint was port 60097 and RustFS was port 38489;
fresh reproduction selects new ports. Compose removal succeeded with no owned
containers or volumes left. No benchmark, release performance run or FUSE mount
was performed.

Workspace fmt, default/overlay check and strict default/overlay clippy pass. The final
broad overlay Cargo run recorded **2318 passed, four inherited failures and
400 ignored** in 572.66 seconds; it is not green. Its failures are the three
legacy overlay failures listed below plus
`source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`.
The other documented source-file flake did not recur. The overlay binaries
passed separately (nine snapshot-fixture tests; the other binaries have no tests).
The preliminary broad binary run had the same counts and is retained separately
from this final Cargo gate. The final default workspace library/binary gate
passed: **1179 passed, zero failed, 225 ignored** (132.35 seconds for the
library; the default binaries have no tests). Workspace build and all four
no-default-feature checks passed: Tokio and io_uring, each with and without
`workspace-overlay`. The final gate driver completed at 2026-10-20 04:29:25 UTC.
Its aggregate exit status does not override the explicit overlay-test failure
record. Shell syntax and the three perf-report harness tests listed in
`AGENTS.md` also passed; these are shell tests, not performance measurements.

The workspace has no `rfuse3` member; the conditional standalone `rfuse3` checks
therefore do not apply. The separately built operator is outside these gates.
No Cargo build or test process from this continuation remains running.
At completion the host runs Linux `6.8.0-139-generic`; `/mnt/brewfs-data` has
53 GiB available (79% used), and the rebuildable `target` directory is 47 GiB.
Accepted performance artifacts were retained. The RustFS/Redis test project's
cleanup proof reports no remaining owned containers or volumes.

Run the tracked service gate as shown above. Reproduce the workspace checks with:

```bash
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo fmt --all --check
cargo check --workspace --features workspace-overlay
cargo clippy --workspace --all-targets --features workspace-overlay -- -D warnings
cargo test --workspace --features workspace-overlay --lib --bins -- --test-threads=1
# A failing library target stops Cargo before binaries; execute them explicitly.
cargo test --workspace --features workspace-overlay --bins -- --test-threads=1
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --bins -- --test-threads=1
cargo build --workspace
cargo check -p brewfs --no-default-features --features fuse-tokio-runtime,workspace-overlay
cargo check -p brewfs --no-default-features --features fuse-io-uring-runtime,workspace-overlay
cargo check -p brewfs --no-default-features --features fuse-tokio-runtime
cargo check -p brewfs --no-default-features --features fuse-io-uring-runtime
git diff --check
```

The artifact root retains commands, exit statuses and earlier failed attempts.
Initial compilation found fixture/module wiring errors; preliminary strict
clippy found the deprecated test-only `AtomicUsize::fetch_update` call. These
were corrected before the passing final check/clippy run. The first RustFS run
(`entity-data-20261020T025504Z-e4156751-ac70-4218-aa1f-0cd97ddc343c`) passed four
packed tests and failed two test expectations. The expiry injection had advanced
the synthetic clock into the subsequent lost-reply case; that case now restores
its independent starting clock. Payload tampering was rejected through the
existing reader's Backend wrapper; its assertion now requires the exact wire005
frame hash-mismatch reason. Success, zero fallback and unrelated I/O errors
still fail that assertion. The passing run above verifies both corrections;
the shared production reader was not changed.

The stray empty untracked `|RwLock` file was inspected and deleted as requested.

### Inherited failures and evidence provenance

These failures precede this continuation and remain tracked separately:

- `g13a_kv_released_lease_is_not_an_expired_recovery_root`
- `kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`
- `migration_marker_fences_entity_and_hot_native_packets_without_writes`
- Source-file flake: `source_mutation_replacement_symlink_and_limits_fail_closed`
- Source-file flake: `source_root_pins_real_attributes_and_detects_namespace_changes_and_replacement`

The previous stage recorded 2306 passed, five failed and 378 ignored in its
overlay library run. Its baseline failure comparison came from dirty r31-era
`7149664`, not a pristine `b6f297e` checkout; see the prior checkpoint for exact
artifact provenance. This continuation does not reclassify these as new defects
or describe a failing full overlay run as green.

The separately built operator's historical `clippy::double_must_use` failure
under rustc 1.99 remains in the r31 handoff. It is outside the Cargo workspace
and is not covered by this continuation's default/overlay workspace clippy gate.
