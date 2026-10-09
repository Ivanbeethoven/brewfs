# Packed v3 G09 pipeline shutdown validation (2026-10-06)

This checkpoint closes a narrow 005 cross-request pipeline sub-contract. The
mount-owned V3 coordinator keeps one physical fetch for equal authenticated
frame demands, retains the worker body until the final waiter/consumer retires,
and consumes the original worker JoinHandle during shutdown. A cancelled
shutdown waiter can be resumed without replacing that handle, and a non-cancel
JoinError is retained before handle retirement.

Focused command:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1       cargo test --no-default-features       --features workspace-overlay,fuse-io-uring-runtime --lib       workspace_overlay::packed_v3::wire005::pipeline::coordinator::tests       -- --nocapture --test-threads=1

Result: 15 passed, 0 failed (1943 filtered). The suite covers same-frame
singleflight for supported profiles, one range fetch, first/last waiter
cancellation, raw/output ownership, capacity backpressure, independent
collections, worker shutdown, cancellation and JoinError propagation. The
JoinError fixture deliberately panics the controlled provider; the panic is
observed by the test harness and the test still passes after retaining the
error in the coordinator completion record.

This evidence does not close G08 native/packed physical-path convergence, the
full G09 production mount/FUSE matrix, or G10-G17 lifecycle and release exits.
