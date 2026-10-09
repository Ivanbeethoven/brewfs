# Packed v3 G12 history-anchor validation (2026-10-06)

This checkpoint closes one narrow durable-publication integrity gap. SQLite
workspace binding reads now validate the immutable version-1 PWB3 history anchor
inside packed_binding_current_tx, which is shared by v3 open/remount view
validation and guarded binding reads. A current pointer whose version-1 anchor
is missing or decodes to another workspace/version fails with
WorkspaceError::CorruptMetadata; it cannot be exposed as a Ready view.

The focused regression test creates the initial binding, routes the current
pointer to a version-2 record, removes the version-1 anchor, and verifies that
the guarded SQLite load rejects the torn lineage:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --features workspace-overlay --lib stores::binding_tests::public_sqlite_binding_requires_initial_history_anchor_for_current_version -- --nocapture

Result: 1 passed, 0 failed (1997 filtered). cargo fmt --all -- --check and
git diff --check also pass.

This is one G12 fail-closed/readiness guard. It does not claim that the full
build/upload/verify/manifest/head+binding CAS journal, crash injection,
remount resume, or reachability-GC contract is complete.
