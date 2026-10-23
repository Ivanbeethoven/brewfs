# #223: bandwidth acquisition above bucket capacity

Issue: <https://github.com/brewfs/brewfs/issues/223>.
Branch base: `origin/main`, `a8459dc92142bf94224e4f4af903f592cef5cb13`.

At 1 MiB/s, `BandwidthLimiter::new` creates a bucket with 16 tokens, each
representing 64 KiB. `acquire_tokens` requested batches of 32, ignored governor's
`InsufficientCapacity(16)`, and subtracted those rejected tokens from the debt.
The ordinary 4 MiB download therefore bypassed throttling entirely. A framed
4 MiB upload charged only its final token.

The fix retries a rejected batch at the capacity returned by governor, without
subtracting any debt until acquisition succeeds. It preserves the configured
refill rate, initial burst, and the existing 32-token batching ceiling.

The regression tests use the public upload/download acquisition methods with
the actual full-block and framed-upload request sizes. Each must take at least
2.9 seconds after the 1 MiB initial burst, and must finish within 10 seconds.

Before changing production code (`223-before.log`, exit 101):

```text
4 MiB at 1 MiB/s with a 1 MiB initial burst bypassed throttling: 194.137µs
framed 4 MiB upload at 1 MiB/s bypassed throttling: 101.822µs
test result: FAILED. 0 passed; 2 failed; 0 ignored
```

The same two tests passed three times (`223-after-1.log` through
`223-after-3.log`), each with:

```text
test result: ok. 2 passed; 0 failed; 0 ignored; finished in 6.07s
```

Additional validation:

- Bandwidth module: 6 passed, 0 failed.
- Workspace libraries/binaries: 1,198 passed, 0 failed, 225 ignored.
- Overlay resolver integration suite: 14 passed, 0 failed.
- Formatting, default and overlay workspace checks, workspace build, Tokio and
  io-uring feature checks, and strict all-target Clippy with default and overlay
  features: passed.
- Four perf-script syntax checks, three perf-report shell tests, and
  `git diff --check`: passed.

[commands.md](commands.md) lists every exact command and result.
[validation.tar.gz](validation.tar.gz) contains all 23 complete logs, both source
patches, and the runner scripts. The archive preserves the dependency warnings
from `asyncfuse` and `tikv-client`; no after/gate command failed. These are local
correctness checks, not mounted throughput measurements or the complete packed
format unit suite. No performance comparison table was changed.
