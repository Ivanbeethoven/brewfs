# Packed v3 G06 HTTP status terminal ledger validation (2026-10-06)

This checkpoint closes one narrow G06 observer sub-contract.
`ObservedHttpBody` now finalizes a response whose HTTP status is already known when
the SDK or caller drops the body before EOF as `HttpStatus` failure. A non-2xx
body is therefore not misreported as cancellation merely because an SDK retry
retired it early; bytes received before retirement remain in the failed-byte
ledger. Successful-status bodies retain the existing cancellation behavior when
they are dropped before EOF.

The focused regression constructs a pending non-2xx body, consumes one data
frame, drops it before EOF, and checks the terminal conservation row:

    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 \
      cargo test --no-default-features \
      --features workspace-overlay,fuse-io-uring-runtime \
      cadapter::read_observer::http::tests::dropped_error_body_is_terminal_http_failure_with_received_bytes \
      -- --exact --nocapture

Result: 1 passed, 0 failed (1956 filtered). The test proves
`failed=1`, `cancelled=0`, `received_failed=10`, and one `HttpStatus` failure
reason while `Counters::conserved()` remains true. `cargo fmt --all` and
`git diff --check` also pass.

This is only a terminal classification/received-byte guard for observed HTTP
attempts. It does not close the full G06 request graph: startup and every v3
object class, complete physical SDK retry attribution, mount-wide budgets,
raw decoded/union amplification across all paths, or paired real FUSE evidence
remain open. The gap audit must continue to mark G06 as open.
