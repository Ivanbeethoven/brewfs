# G14 capability and binding status validation (2026-10-06)

This checkpoint closes one small G14 implementation sub-contract: the workspace
operator now carries explicit packed-v3 capability and PWB3 binding state in
cluster/workspace status, and it derives a fail-closed PackedLowerReady
condition. A revision tuple alone never produces a packed Ready condition.

The current operator catalog is still native-only. Reconcile publishes
WorkspaceCatalogReady=True for the native workspace-v1 catalog and
PackedLowerReady=False with reason UnsupportedCapability. This is an explicit
capability boundary; it does not claim packed mount, publication, recovery,
lease fencing, or reachability-GC completion.

## Source

- branch: codex/packed-metadata-aliyun-20260930
- crd.rs SHA256:
  6b2bf36251d18425195e851cde9438e37a8a4c6aa3c1086b51852ac14315279d
- controller.rs SHA256:
  a632b62fdc4c9b8d4a9387466fb78f6a7ba8e7ba32c0e93f97450de06f7765a8

The source is intentionally limited to
operator/brewfs-operator/src/workspace/crd.rs and controller.rs. Existing
workspace/admin.rs changes remain in the shared worktree and are not replaced.

## TDD evidence

The first focused build was RED: controller references to the new capability
fields failed because WorkspaceClusterStatus had not yet carried them
(E0560 for capabilities/conditions and E0609 for capabilities). The
candidate then added the status model and condition helper.

Focused GREEN:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0       cargo test --features workspace-operator --bin brewfs-operator       packed_capability_status_is_fail_closed_without_binding -- --nocapture

Result: 1 passed, 0 failed.

Operator workspace-operator unit suite:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0       cargo test --features workspace-operator --bin brewfs-operator -- --nocapture

Result: 25 passed, 0 failed.

Build and formatting:

    cargo fmt --all --check
    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --features workspace-operator
    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check
    git diff --check -- operator/brewfs-operator/src/workspace/crd.rs       operator/brewfs-operator/src/workspace/controller.rs

All completed successfully. The regular and workspace-operator checks were run
serially after the initial shared target lock.

## Remaining G14 scope

Still open and deliberately not implied by this checkpoint:

- runtime loading/verification of a packed binding in operator reconcile;
- v3 mount workload wiring and PWB3 generation/head/lease fence;
- finalizer durable drain and MountSession side records;
- Redis/TiKV Kubernetes end-to-end tests;
- operator leader-elected GC and full packed object-graph roots;
- failure injection, recovery, real FUSE and lifecycle acceptance.

Therefore the G14 row in the gap audit remains open except for the explicit
capability/condition sub-item implemented here.
