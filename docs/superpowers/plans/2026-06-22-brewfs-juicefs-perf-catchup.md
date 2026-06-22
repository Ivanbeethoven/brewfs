# BrewFS JuiceFS Perf Catchup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align BrewFS/JuiceFS perf reporting and run small, reversible optimization attempts until BrewFS closes the bigread, bigwrite, randwrite, and randrw gaps without regressing correctness tests.

**Architecture:** Treat benchmark semantics as part of the implementation: report fio-visible throughput and durable drain separately before changing code. Optimize one bottleneck at a time, prefer config or local hot-path changes, and keep only changes that improve focused perf while passing local CI gates.

**Tech Stack:** Rust, FUSE, S3/RustFS object backend, Redis metadata, fio, docker compose perf harness.

---

### Task 1: Align Perf Report Semantics

**Files:**
- Modify: `docker/compose-xfstests/run_juicefs_perf_in_container.sh`

- [x] **Step 1: Add BrewFS-equivalent runtime accounting to JuiceFS reports**

Copy the BrewFS report fields into JuiceFS report generation:

```text
fio job_runtime
wall-job_runtime
wall/job_runtime
active_io_runtime
wall-active_io
wall/active_io
Tail marker
```

- [x] **Step 2: Validate shell syntax**

Run:

```bash
bash -n docker/compose-xfstests/run_juicefs_perf_in_container.sh
bash -n docker/compose-xfstests/run_perf_in_container.sh
```

Result: JuiceFS report now includes `fio job_runtime`, `active_io_runtime`, wall deltas, ratios, and a tail marker. Syntax checks passed for the BrewFS/JuiceFS perf scripts.

### Task 2: Focused Config Trial

**Files:**
- Candidate modify: `docker/compose-xfstests/run_redis_perf.sh`

- [x] **Step 1: Run current focused BrewFS baseline**

Run:

```bash
PERF_TOOLS="fio-bigwrite fio-bigread fio-randwrite fio-randrw" \
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
  --tools "fio-bigwrite fio-bigread fio-randwrite fio-randrw"
```

Result: `perf-run-1782100216-4744` completed with all four tools. Key focused numbers: randwrite `109.32 MiB/s`, wall `155s`; randrw read/write `259.55/116.09 MiB/s`, wall `169s`.

- [x] **Step 2: Trial higher writeback upload concurrency**

Run the same command with:

```bash
BREWFS_WRITEBACK_UPLOAD_CONCURRENCY=12
```

Result: `BREWFS_WRITEBACK_UPLOAD_CONCURRENCY=12` (`perf-run-1782101035-12438`) improved randwrite wall to `149s` but regressed randrw wall to `171s`; do not promote this as a default profile change.

### Task 3: Cached Writeback Coalescing Optimization Attempt

**Files:**
- Candidate modified and reverted: `src/vfs/io/writer.rs`

- [x] **Step 1: Add a focused regression test**

Add a test proving cached writeback slices below `freeze_min_bytes` are not auto-frozen before explicit flush.

Result: the test failed before the change and passed after the change during the attempt.

- [x] **Step 2: Defer cached-only auto-freeze below target size**

Change auto-flush selection so cached-only slices below `freeze_min_bytes` use the existing cached-writeback grace policy instead of being frozen by the normal age/idle path.

Focused result: `perf-run-1782101959-32366` improved randwrite from `109.32` to `132.39 MiB/s` and reduced randwrite PUTs from `23331` to `19574`; randrw wall changed from `169s` to `168s`.

Full-gate result: `perf-run-1782102734-27489` regressed randrw visible throughput to read/write `174.33/78.32 MiB/s` and wall `174s` versus the latest default full run at `259.55/116.09 MiB/s` and wall `169s`. The candidate was reverted.

- [x] **Step 3: Run local Rust gates**

Run:

```bash
cargo fmt --check
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test -p brewfs --bin brewfs chunk::store::tests:: -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test -p brewfs --bin brewfs vfs::io::reader::tests:: -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p brewfs --lib -- -D warnings
```

Result: `cargo fmt --check`, perf script `bash -n`, and writer `test_auto_flush_` tests passed during the attempt. Clippy was not run because the code candidate failed the full perf gate and was reverted.

### Task 4: Full Perf Gate

**Files:**
- Inspect: `docker/compose-xfstests/artifacts/*/report.md`
- Inspect: `git diff`

- [x] **Step 1: Run full four-tool BrewFS perf**

Run the accepted candidate across:

```text
fio-bigwrite
fio-bigread
fio-randwrite
fio-randrw
```

Result: `perf-run-1782102734-27489` rejected the code candidate due randrw throughput/wall regression.

- [x] **Step 2: Compare focused perf**

Compare:

```text
fio visible BW
script wall
active_io_runtime
post-write drain
GET/PUT count and avg latency
partial_tail ratio
```

- [x] **Step 3: Keep only effective changes**

Commit only if the candidate has measurable benefit and no test regression. Revert config/code candidates that do not improve the target workloads.

Result: code/config performance candidates were reverted or not promoted. Keep only the JuiceFS report-accounting fix and the updated plan.

### Task 5: Next Bottleneck After This Candidate

Continue with a second candidate focused on the remaining close/flush tail:

- Tune or redesign the cached writeback auto-freeze age so 60s randwrite/randrw does not still produce tens of thousands of idle partial tails.
- Inspect commit ordering and `reject_unique` behavior, because randwrite still creates many slices and one-block upload batches.
- Compare against JuiceFS file-by-file writeback batching behavior before changing correctness-sensitive commit sequencing.

### Task 6: 2026-06-22 Aligned BrewFS/JuiceFS Gate

**Files:**
- Inspect: `docker/compose-xfstests/artifacts/perf-run-1782104992-17545/report.md`
- Inspect: `docker/compose-xfstests/artifacts/juicefs-perf-run-1782103383-17123/report.md`

- [x] **Step 1: Rerun JuiceFS with BrewFS-equivalent runtime accounting**

Result: `juicefs-perf-run-1782103383-17123` completed with the same fio tools and now reports `fio job_runtime`, `active_io_runtime`, wall deltas, and post-write drain.

Important JuiceFS numbers:

```text
bigread    2.33 GiB/s, wall 0s, active 0.429s
bigwrite   3.16 GiB/s, wall 1s, active 0.316s, post-drain 14s
randwrite  250.56 MiB/s, wall 143s, active 60.297s, post-drain 103s
randrw     read/write 114.73/53.73 MiB/s, wall 61s, active 60.453s, post-drain 97s
```

- [x] **Step 2: Rerun BrewFS with the same writeback-throughput profile**

Result: `perf-run-1782104992-17545` completed with `BREWFS_WRITEBACK_MODE=commit_before_upload`, `BREWFS_FUSE_WORKERS=6`, `BREWFS_S3_MAX_CONCURRENCY=16`, `BREWFS_UPLOAD_CONCURRENCY=32`, `BREWFS_WRITEBACK_UPLOAD_CONCURRENCY=6`, and `BREWFS_CACHED_SUB_BLOCK_AUTO_FREEZE_MIN_AGE_MS=30000`.

Important BrewFS numbers:

```text
bigread    1.26 GiB/s, wall 2s, active 0.794s
bigwrite   1.14 GiB/s, wall 2s, active 0.881s, post-drain 2s
randwrite  131.80 MiB/s, wall 155s, active 66.675s
randrw     read/write 327.43/146.07 MiB/s, wall 169s, active 63.831s
```

Interpretation:

```text
BrewFS bigread/bigwrite still trail JuiceFS by roughly 1.8x/2.8x visible bandwidth.
BrewFS randwrite is about 0.53x JuiceFS visible write bandwidth.
BrewFS randrw visible bandwidth is higher than JuiceFS, but wall time is 2.77x worse because close/flush tail is +105s.
JuiceFS shifts much of writeback cost to explicit post-drain; BrewFS pays it inside fio-visible close/flush.
```

BrewFS writeback evidence:

```text
randwrite: upload_batch=23177 avg=0.3MiB, partial_tail=0.96, auto_idle=21636, reject_unique=16258, flush_wait=480.72s/25628 slices.
randrw:    upload_batch=25644 avg=0.3MiB, partial_tail=0.97, auto_idle=23338, reject_unique=37567, flush_wait=574.29s/33787 slices.
```

- [x] **Step 3: Try cached-sub-block auto-freeze age = 120s**

Run:

```bash
BREWFS_CACHED_SUB_BLOCK_AUTO_FREEZE_MIN_AGE_MS=120000 \
PERF_TOOLS="fio-randwrite fio-randrw" \
PERF_FIO_DIRECT=0 \
PERF_FIO_IOENGINE=io_uring \
PERF_FIO_IODEPTH=1 \
PERF_FIO_PREFILL_DRAIN=true \
PERF_FIO_PREFILL_REMOUNT=true \
PERF_FIO_COLD_READ_CLEAR_CACHE=true \
PERF_FIO_POST_WRITE_DRAIN=true \
bash docker/compose-xfstests/run_redis_perf.sh \
  --writeback-throughput-profile \
  --tools "fio-randwrite fio-randrw"
```

Result: `perf-run-1782105644-31160` was rejected. Randwrite wall stayed `154s`, randrw wall worsened to `177s`, and p99 write latency worsened for randwrite. Do not promote this config.

### Task 7: Next Implementation Candidate

The next code candidate should not simply increase cached sub-block age. The 120s test converted many idle freezes into explicit flush freezes, but did not reduce the number of small upload batches.

Required direction:

- Add a chunk-local cached writeback batcher that can merge multiple cached-only sub-block slices into block-sized upload units before upload, while preserving metadata slice ordering.
- Keep overlapping write ordering strict: older overlapping writes must not overwrite newer bytes, but older non-overlapping appends should be merged into the same physical upload where possible.
- Reduce `upload_batch avg=0.3MiB` toward at least `2MiB` in randwrite/randrw without increasing wall time.
- Gate every candidate with: writer unit tests, `cargo fmt --check`, focused `fio-randwrite fio-randrw`, then full `fio-bigwrite fio-bigread fio-randwrite fio-randrw`.

### Task 8: Safe Full-Block Cached Writeback Batcher

**Files:**
- Modify: `src/vfs/io/writer.rs`
- Modify: `src/vfs/io/cached_block_assembler.rs`
- Test: writer unit tests around `write_at_cached`, dirty overlay, flush, and upload metrics.

The JuiceFS comparison points at a specific architectural gap. JuiceFS keeps cached writes inside `wSlice` pages and uploads at block granularity when a block is ready; BrewFS currently lets many random cached 4KiB writes become independent partial-tail slices. BrewFS cannot safely copy JuiceFS' zero-fill sparse behavior because BrewFS `SliceDesc` exposes a contiguous logical range; writing zero-filled holes would overwrite older file data. The BrewFS batcher must therefore emit only safe full blocks or explicitly materialized ranges.

- [ ] **Step 1: Add RED test for full-block cached page assembly**

Create a writer test with `layout.block_size = 4096` and `page_size = 1024`. Issue four `write_at_cached` calls for all pages in one block in shuffled offset order. Before the implementation this should fail by reporting multiple cached sub-block live slices or multiple upload batches. After the implementation it should flush as one block-sized cached-only upload.

Expected assertions after implementation:

```rust
assert_eq!(breakdown.upload_batch_ops, 1);
assert_eq!(breakdown.upload_partial_tail_ops, 0);
assert_eq!(breakdown.upload_batch_bytes, layout.block_size as u64);
```

- [ ] **Step 2: Add RED test for overlay while pages are still buffered**

Use the same cached page assembly path before flush. Read each written page through `read_dirty_if_fully_covered`. The test must prove newly written dirty pages are visible even if they have not yet been emitted as a slice.

Expected assertions after implementation:

```rust
assert_eq!(
    file_writer.read_dirty_if_fully_covered(page_offset, page_len).await?.unwrap(),
    expected_page
);
```

- [ ] **Step 3: Add RED test preventing zero-hole corruption**

Write only page 0 and page 3 of a block, then flush. The safe behavior is either two explicit partial writes or read-modify-write with real old bytes. It must not produce a single block-sized zero-filled slice unless missing pages were read from the existing file image.

Expected assertions after implementation:

```rust
assert!(
    breakdown.upload_partial_tail_ops >= 1 || breakdown.read_modify_write_full_block_ops >= 1,
    "sparse cached pages must not become a zero-filled full-block overwrite"
);
```

- [ ] **Step 4: Implement only the safe path first**

Start with a conservative batcher:

```text
cached write page -> per-file/per-chunk/per-block assembler
if block has every page:
  drain full block into normal SliceState as one block-sized cached write
else:
  keep pages visible to dirty overlay, but do not upload as a full block
flush:
  drain incomplete pages as current partial-tail slices unless RMW support is implemented
```

Do not add read-modify-write in the first patch. RMW changes read path, old-data consistency, and upload volume; it needs a separate perf gate.

- [ ] **Step 5: Focused perf gate**

Run:

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
  --writeback-throughput-profile \
  --tools "fio-randwrite fio-randrw"
```

Keep the candidate only if it reduces `upload_batch_ops`, increases average upload batch size, and does not worsen either randwrite or randrw wall time by more than 3%.

- [ ] **Step 6: Full aligned perf gate**

Run the full aligned BrewFS/JuiceFS set again:

```text
fio-bigwrite
fio-bigread
fio-randwrite
fio-randrw
```

The candidate is acceptable only if it does not regress bigread/bigwrite and moves randwrite toward JuiceFS while preserving randrw wall time.
