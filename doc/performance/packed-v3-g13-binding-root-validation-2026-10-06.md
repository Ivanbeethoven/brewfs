# G13 PWB3 binding native-root validation - 2026-10-06

The 2026-10-07 destructive revalidation, phantom-generation fence and native
finalization inventory correction are tracked in
[catalog safety validation](packed-v3-g12-g13-catalog-safety-validation-2026-10-07.md).
The source batch has its own pending complete gate; the historical two-test
GREEN below does not certify these later changes.

This validation covers one G13 safety sub-contract: persisted v3 PWB3
binding records remain catalog roots for their native sealed base and writable
head layers while a workspace is entering or remains in Deleting.

The production catalog changes are:

- src/workspace_overlay/stores/kv_store.rs: gc_snapshot scans and decodes
  both packed/v3/history/ and packed/v3/current/ records and adds each
  binding's base_revision.layer_id and head_layer_id to root_layers.
- src/workspace_overlay/stores/database.rs: gc_snapshot scans and decodes
  all ws_v3_packed_bindings.record rows and adds the same two layer roots.
- src/workspace_overlay/stores/binding_tests.rs: the existing real packed
  producer fixture helpers are pub(crate) so the SQLite focused contract
  uses the production binding install API rather than an injected SQL row.

The focused tests first install a real catalog binding, release the mutation
lease, mark the workspace deleting, and inspect the production gc_snapshot.
They require both the PWB3 base and head layer IDs to remain roots. The KV
fixture uses the existing catalog PWB3 record fixture; the SQLite fixture uses
the real wire005 producer, authenticated snapshot proof, and
install_packed_lower_binding.

## GREEN evidence

Command:

    CARGO_BUILD_JOBS=1 cargo test --no-default-features --features workspace-overlay,fuse-tokio-runtime g13d_ --lib -- --nocapture


Result: 2 passed, 0 failed, 1949 filtered out; exit status 0.
The run exercised:

- g13d_kv_packed_binding_history_keeps_layer_roots_after_workspace_delete
- g13d_sqlite_packed_binding_history_keeps_layer_roots_after_workspace_delete

cargo fmt --all --check and git diff --check pass for the active tree.

## Scope and remaining G13 work

This proves only catalog native-layer root retention for PWB3 records. It does
not prove deletion of unreachable packed objects or closure over PM11 manifest,
container, index, frame-directory, cold-attribute, descriptor, or external-large
dependencies. The following remain open:

- packed object graph persistence and mark/sweep integration;
- binding-history retirement and orphan PWB3 cleanup;
- active reader pins and grace/revalidation across object deletion;
- real Redis/TiKV behavior and remote object-store delete/retry semantics;
- full fork -> mutate -> seal -> publish -> remount -> GC lifecycle.

The full G13/S/X acceptance status remains open.
