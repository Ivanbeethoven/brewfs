# Packed v3 G12 same-head publication validation — 2026-10-06

The 2026-10-07 exact-response-loss retry and issued-inode reservation correction
are tracked separately in
[catalog safety validation](packed-v3-g12-g13-catalog-safety-validation-2026-10-07.md).
That source batch still requires its fresh complete gate; the historical GREEN
below applies only to this checkpoint's source.

This checkpoint closes one durable publication sub-contract. It does not close
the complete seal, dependency-upload, crash-recovery, or reachability-GC
protocol.

The new `PublishPackedLowerBinding` request is a versioned replacement of an
already installed PWB3 binding while the writable head and sealed base remain
fixed. The request validates the old head/base/binding generation and rejects
republication of the same manifest.

SQLite performs one `BEGIN IMMEDIATE` transaction. It inserts history version
N+1, updates the current pointer with the old-version predicate, advances the
workspace head epoch and writable-layer sequence, raises the inode allocator
floor when needed, rechecks the lease, and commits. Any stale predicate,
lease loss, or injected pre-commit failure rolls back all rows.

The KV implementation performs one timed compare-and-swap over the control
routing value, workspace/head/base/lease, current binding, claim, old history,
and absent new-history key. It writes the new workspace/head/allocator/current
and history values as one CAS. The generic `KvWorkspaceStore` implementation
therefore exposes the same narrow operation to its Redis and TiKV backends;
live Redis/TiKV service tests remain open.

## GREEN evidence

Command:

```text
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 \
cargo test --no-default-features \
  --features workspace-overlay,fuse-io-uring-runtime \
  --lib publication -- --nocapture --test-threads=1
```

Result: 13 passed, 0 failed, 1943 filtered out.

The publication-specific cases include:

- SQLite version-2 history/current publication with epoch and sequence advance;
- KV version-2 history/current publication with the same checks;
- SQLite stale-generation and pre-commit failure preserving the old view; KV CAS failure preserving the old view.

`cargo +stable fmt --all -- --check` and `git diff --check` pass.

## Remaining G12 boundary

This does not prove PM10 dependency closure, manifest-last ordering, seal
journal stages, crash injection and idempotent resume/abort, old-lease reader
pins, head-layer rotation, or object reachability GC. Those remain open and
must be validated before claiming full G12 or the complete SPEC lifecycle.
