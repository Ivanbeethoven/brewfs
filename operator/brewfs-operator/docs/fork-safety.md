# Native workspace fork identity and cleanup

This change targets `codex/workspace-fork-20261022`, based on
`cdb5bbb1f971c7df853723e6ba6ab44eea1cb756`, for upstream issues
[#204](https://github.com/brewfs/brewfs/issues/204),
[#205](https://github.com/brewfs/brewfs/issues/205),
[#206](https://github.com/brewfs/brewfs/issues/206), and
[#210](https://github.com/brewfs/brewfs/issues/210). It does not productize or
merge the workspace fork feature into `main`.

The fork CRD makes all three spec identity fields immutable. Before adding
the finalizer or creating dependents, the controller persists the fork UID,
source Kubernetes UID and backend ID, cluster reference, and resource names
in `status.identity`. Root CEL transition rules prohibit changing or removing
that record, including by removing the entire status object. Recorded child
and snapshot backend IDs are also write-once. Status updates preserve these
values while a fork is Pending or Failed.

Snapshot and child adoption require the exact fork controller owner reference
and expected cluster/source identity. Child adoption additionally checks the
expected owner ID; ready snapshots must report the recorded source backend ID.
Source recreation and backend identity changes cause reconciliation errors.
Cleanup uses the persisted names and repeats the identity checks before deletion.

StorageClasses are cluster-scoped, so they cannot use a namespaced fork as a
Kubernetes owner reference. The controller records the fork UID and
`namespace/name` in explicit annotations. Adoption and deletion require these
markers, the full parameter map, and the expected provisioning policy. Deletes
of all three dependent resources use both UID and resourceVersion preconditions,
so replacement or mutation after verification causes a conflict.

StorageClass names retain a readable prefix and a 20-hex-character SHA-256
suffix over the complete persisted source identity. Components are encoded
unambiguously. Ownership checks remain mandatory in case of a hash collision.
StorageClass RBAC grants exactly `get`, `create`, and `delete`.

## Existing forks

Apply the regenerated CRD together with the controller update. Forks that
already have a finalizer but lack the persisted identity require manual
recovery. The controller refuses to infer ownership or cleanup targets from
their current spec. Earlier ownerless snapshots and StorageClasses without
the identity annotations are conflicts, even when their names match.

## Local validation

Validation uses unit tests only; it does not start a Kubernetes cluster.
All Cargo commands run from `operator/brewfs-operator`, with this environment:

```bash
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_CODEGEN_UNITS=16 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=/mnt/brewfs-data/src/target-forkfix-204-210
while [ "$(free -g | awk '/^Mem:/ {print $7}')" -lt 6 ]; do
    free -g
    sleep 60
done
```

The memory guard runs before every Cargo invocation. The original controller
decisions were extracted into helpers without adding the fixes, then the first
seven regressions were run. Compilation succeeded and all seven assertions
failed. The completed implementation has 16 focused regressions.

| Command | Result | Exit code |
| --- | --- | --- |
| `timeout -k 10 2700 cargo test --locked --features workspace-operator fork_regression_ -- --nocapture` (before fixes) | 0 passed, 7 failed, 58 filtered out | 101 |
| `timeout -k 10 2700 cargo test --locked --features workspace-operator fork_regression_ -- --nocapture` (after fixes) | 16 passed, 0 failed, 58 filtered out | 0 |
| `timeout -k 10 2700 cargo test --locked --features workspace-operator` | 74 passed, 0 failed, 0 ignored | 0 |
| `timeout -k 10 2700 cargo test --locked` | 18 passed, 0 failed, 0 ignored | 0 |
| `timeout -k 10 2700 cargo check --locked` | passed | 0 |
| `timeout -k 10 2700 cargo fmt --all --check` (TDD scaffolding before formatting) | formatting differences | 1 |
| `timeout -k 10 2700 cargo fmt --all` | formatted | 0 |
| `timeout -k 10 2700 cargo fmt --all --check` (final) | passed | 0 |
| `timeout -k 10 2700 cargo run --locked --features workspace-operator -- crdgen` | generated the committed CRD manifest | 0 |
| `timeout -k 10 2700 /mnt/brewfs-data/src/target-forkfix-204-210/debug/brewfs-operator crdgen` | reproduced the generated manifest | 0 |
| `diff -u manifests/crd.yaml /mnt/brewfs-data/src/forkfix-validation-204-210/verified-crd.yaml` | byte-for-byte match | 0 |
| `git diff --check` | passed | 0 |
| `timeout -k 10 2700 cargo clippy --locked --features workspace-operator --all-targets -- -D warnings` (informational, not a gate) | 34 pre-existing diagnostics in unchanged files | 101 |

The acceptance gates are formatting and the two operator test suites. Strict
Clippy is not a gate for this task: on Rust 1.99.0 it reports 32 pre-existing
`double_must_use` errors from `#[async_trait]` expansions in `admin.rs` and
`gc.rs`, plus two pre-existing `needless_borrow` errors in `admin.rs:321`.
Those files are unchanged; these diagnostics are unrelated to this fix.

The seven tests that failed before and passed after are:

```text
workspace::fork::tests::fork_regression_204_schema_freezes_spec
workspace::fork::tests::fork_regression_204_snapshot_rejects_foreign_owner_and_source
workspace::fork::tests::fork_regression_204_child_rejects_foreign_identity
workspace::fork::tests::fork_regression_205_storage_class_requires_owner_and_all_parameters
workspace::fork::tests::fork_regression_205_delete_is_bound_to_observed_uid
workspace::fork::tests::fork_regression_206_adversarial_names_do_not_collide
manifest_tests::fork_regression_210_storage_class_rbac_is_minimal
```

The additional passing regression tests are:

```text
workspace::fork::tests::fork_regression_204_schema_prevents_clearing_recorded_identity
workspace::fork::tests::fork_regression_204_cleanup_uses_recorded_identity_after_spec_edit
workspace::fork::tests::fork_regression_204_source_recreation_cannot_redirect_fork
workspace::fork::tests::fork_regression_204_pending_status_preserves_cleanup_identity
workspace::fork::tests::fork_regression_204_created_snapshot_has_controller_owner
workspace::fork::tests::fork_regression_204_recorded_backend_ids_cannot_be_replaced
workspace::fork::tests::fork_regression_205_storage_class_rejects_policy_changes
workspace::fork::tests::fork_regression_205_cluster_scoped_class_has_explicit_fork_identity
workspace::fork::tests::fork_regression_206_hash_covers_full_source_identity
```

The RBAC audit command was:

```bash
rg -n 'StorageClass|storageclasses|storage_api' src
rg -n -A 18 -B 2 'storage_api' src/workspace/controller.rs
```

Both commands exited 0. The only StorageClass API operations are point
`get_opt`/`create` during reconciliation and `get_opt`/`delete` during cleanup.
No StorageClass list, watch, update, or patch operation requires the removed verbs.
