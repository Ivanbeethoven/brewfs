# Packed v3 G09 pipeline narrow-contract validation (2026-10-06)

This checkpoint records the wire-005 cross-request pipeline sub-contract. It
does not claim the complete G09 experiment or the full packed-v3 acceptance
gate.

## Contract exercised

`V3FlightRegistry` admits a bounded mount-scoped flight and uses an immutable
key containing the read generation (lower snapshot and workspace head epoch),
read attribution, container kind/key/digest/length, access profile, frame
policy, descriptor identity and size-class limits. Equal keys share one
leader/body; a different profile, generation, descriptor, or attribution is a
different flight.

`V3DemandCoordinator` owns a bounded pending queue and collection window. The
worker may merge only submitted, compatible frames into one range GET. A body
is cancelled after its final logical waiter leaves, while a cancelled waiter
does not cancel followers. Shutdown aborts and joins the original collector,
wakes all waiters, and retains a provider join error before retiring its handle.

## Validation

Command (Ubuntu 24.04 WSL, `/home/hxy/brewfs`):

```text
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 \
  cargo test --no-default-features \
    --features workspace-overlay,fuse-io-uring-runtime \
    --lib workspace_overlay::packed_v3::wire005::pipeline \
    -- --nocapture --test-threads=1
```

Result: **22 passed, 0 failed, 0 ignored, 1936 filtered out**.

The focused profile check,
`same_frame_singleflight_is_unique_for_every_supported_profile`, runs the
actual coordinator against the backend probe for
`RandomSmallFile`, `SequentialSmallFile`, and `Mixed`. Each profile submits
the same authenticated frame twice; both waiters receive the same `Arc` and
the probe records exactly one range GET. Existing coordinator tests cover the
coalesced two-frame range, generation/attribution capture across a mount
phase transition, follower cancellation, body cancellation after the last
waiter, bounded raw backpressure, cancellation before opening a body, typed
workspace admission failure, collector shutdown/join, and terminal body
corruption. The two shutdown/join tests intentionally print the controlled
provider panic while still passing; the panic is the fixture's fault
injection, not an unhandled test failure.

Formatting and whitespace checks also passed:

```text
cargo +stable fmt --all -- --check
git diff --check
```

This checkpoint adds only the profile-matrix regression test and this evidence
record; no production pipeline code was changed in this validation pass.

## Remaining scope

The narrow contract does not close the complete G09/G15/G17 acceptance. Real
FUSE cross-request traces, `.stats` request-graph attribution, physical
eviction/refetch, cold-pipelined versus warm cache controls, matched paired
performance runs, and the later G08/G10–G13 workspace lifecycle gates remain
open. A 004 cache/coordinator result must not be reused as 005 evidence.
