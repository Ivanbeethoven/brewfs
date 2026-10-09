# Packed v3 G07 budget shutdown validation (2026-10-06)

This checkpoint closes one mount-budget lifecycle sub-contract. When Stored/Raw
admission is blocked by an existing owned Raw permit, closing the mount budget
wakes the waiter, returns a limit error, and leaves no partial Stored charge.
The pre-existing Raw owner remains valid until its final drop and then all pools
return to zero.

Focused command:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1       cargo test --no-default-features       --features workspace-overlay,fuse-io-uring-runtime --lib       workspace_overlay::packed_v3::wire005::budget::tests::shutdown_wakes_blocked_stored_raw_admission_without_partial_charge       -- --exact --nocapture --test-threads=1

Result: 1 passed, 0 failed (1958 filtered). The test exercises the actual
V3MountBudget::admit_when_available and close paths; it does not use a fake
semaphore or synthetic counter.

The full G07 requirement remains open for all native/004 paths, complete
queue/pin/cache/raw/decoder/output lifecycle, real FUSE shutdown and matched
RSS evidence.
