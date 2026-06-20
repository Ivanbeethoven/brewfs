# BrewFS Randwrite/Randrw Unique-Append Optimization Round

## Goal

Reduce BrewFS buffered random-write small-slice amplification without reintroducing the cached-block assembler path that regressed `fio-randrw`.

## Rejected Prior Attempt

The cached-block assembler experiments are intentionally not merged. They reduced some small PUTs in `fio-randwrite`, but every wired variant either increased wall time or regressed `fio-randrw`:

- `perf-run-1781958601-26769`: page-only assembler; `fio-randwrite` 151s, `fio-randrw` 177s, no ready full blocks.
- `perf-run-1781959515-7371`: range assembler; `fio-randwrite` 167s, `fio-randrw` 197s.
- `perf-run-1781960405-26446`: range assembler plus flush bypass; `fio-randwrite` 164s, `fio-randrw` 189s, `fio-randrw` PUT/GiB regressed heavily.

Conclusion: do not continue the assembler architecture until the flush/partial-drain design is replaced. Use smaller writer-state changes that preserve existing `SliceState` semantics.

## Root Cause For This Round

The stable writer path still showed high cached write fragmentation:

- Baseline artifact: `docker/compose-xfstests/artifacts/perf-run-1781947303-21098`
- `fio-randwrite`: `slice_create=28347`, `reject_unique=21766`, `partial_tail=27825`, PUT/GiB written `3434.0`.
- `fio-randrw`: `slice_create=24422`, `reject_unique=29830`, `partial_tail=23913`, PUT/GiB written `2764.8`.

`ChunkHandle::find_slice_or_create` rejected all writes with an older FUSE `unique` than the current slice's `max_write_unique`, even when the write was a non-overlapping append. Overlap must remain rejected to protect last-writer-wins, but append cannot overwrite newer bytes and can safely coalesce.

## Implemented Change

In `src/vfs/io/writer.rs`, older-unique rejection is now limited to `PageWriteAction::Overlap`. Older non-overlapping appends can reuse the existing writable slice.

The same file also fixes the `flush timeout` diagnostic to report the true `pending/total` slice count instead of accidentally formatting the inode as the pending count.

Regression test added:

```text
vfs::io::writer::tests::test_cached_older_append_reuses_slice_but_older_overlap_is_rejected
```

The test verifies:

- older append after a newer cached write keeps one live slice and records reuse;
- older overlap still creates a separate slice and increments `slice_reject_older_unique_ops`.

## Verification

Local code gates:

```bash
cargo fmt --check

CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test -p brewfs --bin brewfs \
  vfs::fs::tests::io_tests::test_fs_parallel_writes_to_distinct_files -- --nocapture

CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test --workspace --lib --bins

CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo clippy -p brewfs --lib -- -D warnings
```

Result:

- fmt check: passed
- focused parallel-write regression: `1 passed; 0 failed`
- workspace lib/bin tests: `514 passed; 0 failed; 159 ignored`
- clippy lib: passed

Focused perf command:

```bash
PERF_TOOLS="fio-randwrite fio-randrw" \
PERF_FIO_DIRECT=0 \
PERF_FIO_IOENGINE=io_uring \
PERF_FIO_IODEPTH=1 \
PERF_FIO_PREFILL_DRAIN=true \
PERF_FIO_PREFILL_REMOUNT=true \
PERF_FIO_COLD_READ_CLEAR_CACHE=true \
PERF_FIO_POST_WRITE_DRAIN=true \
bash docker/compose-xfstests/run_redis_perf.sh \
  --s3 \
  --writeback-throughput-profile \
  --tools "fio-randwrite fio-randrw"
```

Candidate artifact:

```text
docker/compose-xfstests/artifacts/perf-run-1781962097-24710
```

## Perf Result

Compared with `perf-run-1781947303-21098`:

| Scenario | Baseline | Candidate | Result |
| --- | ---: | ---: | --- |
| `fio-randwrite` wall | 154s | 146s | 5.2% faster |
| `fio-randwrite` post-drain | 14s | 0s | improved |
| `fio-randwrite` wall-effective write BW | 55.7 MiB/s | 63.0 MiB/s | +13.1% |
| `fio-randwrite` PUT/GiB written | 3434.0 | 2399.9 | -30.1% |
| `fio-randwrite` slice creates | 28347 | 21023 | -25.8% |
| `fio-randwrite` older-unique rejects | 21766 | 15783 | -27.5% |
| `fio-randwrite` partial tails | 27825 | 20479 | -26.4% |
| `fio-randrw` wall | 176s | 176s | neutral |
| `fio-randrw` post-drain | 12s | 8s | improved |
| `fio-randrw` read BW | 194.4 MiB/s | 210.0 MiB/s | +8.1% |
| `fio-randrw` write BW | 86.8 MiB/s | 93.7 MiB/s | +8.0% |
| `fio-randrw` PUT/GiB written | 2764.8 | 2706.8 | -2.1% |
| `fio-randrw` partial tails | 23913 | 23648 | -1.1% |

Decision: accept the change. It materially improves pure random write amplification and does not regress mixed randrw.

## Next Bottleneck

The remaining `fio-randrw` gap is still auto-idle cached-only partial tails:

- Candidate `fio-randrw`: `auto_idle=22397`, `partial_tail=23648`, `partial_tail_ratio=0.967`.
- `reject_unique` did not improve in randrw (`29830 -> 29848`), so the mixed workload is dominated by overlapping random writes, not append-safe reordering.

Next safe target: reduce auto-idle partial-tail sealing for cached-only random writes without delaying explicit fsync/close or hurting read latency. A promising direction is a bounded per-chunk dirty-range coalescing policy for overlapping cached writes, but it must be designed around JuiceFS-style page ownership and verified with direct randrw guards before implementation.
