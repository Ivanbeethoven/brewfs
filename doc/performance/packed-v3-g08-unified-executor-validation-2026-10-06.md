# Packed v3 G08 unified executor validation (2026-10-06)

This checkpoint records the native/packed same-snapshot sub-contract already
present in the v3 path. NativePackedPlacementProvider emits the same
UnifiedReadPlan/UnifiedReadSourceFetcher vocabulary as PackedV3ReadonlyMeta;
the native placement block store rejects the legacy direct path, so a valid
native read must use the prepared unified executor.

Focused regressions:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1       cargo test --no-default-features       --features workspace-overlay,fuse-io-uring-runtime --lib       native_tikv_same_snapshot_plan_and_data_match_packed_without_namespace_pages       -- --nocapture --test-threads=1

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1       cargo test --no-default-features       --features workspace-overlay,fuse-io-uring-runtime --lib       native_minimum_roots_first_valid_adapter_read_and_real_release_fit_original_control       -- --nocapture --test-threads=1

Both tests passed 1/1. The first compares native and packed plans and executes
both against the same immutable snapshot, checking byte equality and namespace
identity. The second goes through the real filesystem adapter read and checks
minimum-root admission/release accounting.

This is not a full G08 acceptance: the four-way metadata/frame matrix,
matched real FUSE lifecycle, external backends and the S/X performance gates
remain open.
