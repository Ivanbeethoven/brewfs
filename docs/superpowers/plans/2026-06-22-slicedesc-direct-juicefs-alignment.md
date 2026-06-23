# SliceDesc Direct JuiceFS Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Directly upgrade BrewFS `SliceDesc` to carry JuiceFS-style logical-position plus object-range semantics, then use that richer model to reduce random-write slice fragmentation and small partial-tail uploads.

**Architecture:** Keep the existing `SliceDesc` type name and migrate the structure in place. `offset` and `length` remain the logical chunk range; new object-range fields describe where that logical range lives inside the physical slice object. Old metadata must deserialize into the new structure with `object_offset = 0` and `object_size = length` so current test stores and existing JSON metadata remain readable.

**Tech Stack:** Rust, serde/rkyv metadata serialization, Redis/etcd/database metadata stores, BrewFS chunk reader/writer/compactor, JuiceFS reference semantics from `pkg/meta/slice.go` and `pkg/chunk/cached_store.go`.

---

## Design Constraints

- Do not introduce `SliceDescV2` or a parallel descriptor type. The existing `SliceDesc` is the canonical format after this change.
- Preserve the meaning of existing public fields:
  - `offset`: logical byte offset inside the BrewFS chunk.
  - `length`: logical visible length in bytes.
  - `slice_id`: physical slice object id.
  - `chunk_id`: BrewFS chunk id.
- Add object mapping directly to `SliceDesc`:

```rust
pub struct SliceDesc {
    pub slice_id: u64,
    pub chunk_id: u64,
    pub offset: u64,
    pub length: u64,
    #[serde(default)]
    pub object_offset: u64,
    #[serde(default = "default_object_size")]
    pub object_size: u64,
}
```

Implementation note: `default_object_size` cannot see `length`, so use a custom `Deserialize` helper or `#[serde(default)] object_size = 0` plus a `normalized()` method that returns `length` when `object_size == 0`. Keep serialization writing a non-zero `object_size` for new metadata.

- Reader block addressing must change from:

```text
physical_offset = logical_read_start - slice.offset
```

to:

```text
physical_offset = slice.object_offset + (logical_read_start - slice.offset)
```

- Writer commit of ordinary new slices must write:

```text
object_offset = 0
object_size = slice.data.len()
```

- Compaction and delayed deletion must delete full physical objects, not just logical fragments. Until shared physical object references are introduced, every committed writer-created `slice_id` should still be owned by one descriptor, so deletion remains straightforward.

## Root Cause Being Addressed

BrewFS currently stores only the logical contiguous range for each slice. That makes every random cached write tend to become an independent physical object and metadata entry. JuiceFS separates logical chunk position from physical object offset and can represent a logical segment as a view into a larger slice object. Without that separation, BrewFS cannot safely:

- compact metadata by splitting or preserving partial old slices,
- read a subrange from a larger physical object,
- merge random writes into larger object uploads without losing object offset information,
- avoid turning sparse holes into zero-filled overwrites.

The immediate performance target remains the measured randwrite/randrw bottleneck:

```text
randwrite: upload_batch avg=0.3MiB, partial_tail=0.96, flush_wait=480.72s/25628 slices.
randrw:    upload_batch avg=0.3MiB, partial_tail=0.97, flush_wait=574.29s/33787 slices.
```

## Task 1: Directly Extend `SliceDesc`

**Files:**
- Modify: `src/chunk/slice.rs`
- Test: `src/chunk/slice.rs`

- [x] **Step 1: Add RED serialization test for legacy JSON**

Add a test that deserializes old JSON without object fields:

```rust
#[test]
fn test_slicedesc_legacy_json_defaults_object_range() {
    let json = br#"{"slice_id":7,"chunk_id":9,"offset":1024,"length":4096}"#;
    let desc: SliceDesc = serde_json::from_slice(json).unwrap();

    assert_eq!(desc.slice_id, 7);
    assert_eq!(desc.chunk_id, 9);
    assert_eq!(desc.offset, 1024);
    assert_eq!(desc.length, 4096);
    assert_eq!(desc.object_offset, 0);
    assert_eq!(desc.physical_size(), 4096);
}
```

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::slice::tests::test_slicedesc_legacy_json_defaults_object_range -- --nocapture
```

Expected before implementation: compile failure or field/method missing.

- [x] **Step 2: Add object mapping fields and helpers**

Add fields directly to `SliceDesc`:

```rust
#[serde(default)]
pub object_offset: u64,
#[serde(default)]
pub object_size: u64,
```

Add helpers:

```rust
pub fn physical_size(&self) -> u64 {
    if self.object_size == 0 {
        self.length
    } else {
        self.object_size
    }
}

pub fn physical_offset_for(&self, logical_offset: u64) -> u64 {
    self.object_offset + logical_offset.saturating_sub(self.offset)
}

pub fn logical_end(&self) -> u64 {
    self.offset + self.length
}
```

Run the RED test again. Expected after implementation: pass.

- [x] **Step 3: Update existing `SliceDesc` literals**

Every `SliceDesc { ... }` literal must set:

```rust
object_offset: 0,
object_size: length,
```

When `length` is a field expression, use that expression. For tests where exact object-size behavior does not matter, still set it explicitly to avoid accidental zero default in new metadata.

## Task 2: Reader Uses Physical Object Offset

**Files:**
- Modify: `src/chunk/reader.rs`
- Test: `src/chunk/reader.rs`

- [x] **Step 1: Add RED reader test for object offset**

Upload one physical object and publish only a logical view into it:

```rust
#[tokio::test]
async fn test_reader_uses_slicedesc_object_offset() {
    let layout = ChunkLayout {
        chunk_size: 16 * 1024,
        block_size: 4 * 1024,
    };
    let store = Arc::new(InMemoryBlockStore::new());
    let meta = create_meta_store_from_url("sqlite::memory:").await.unwrap().layer();
    let backend = Arc::new(Backend::new(store.clone(), meta.clone()));
    let data: Vec<u8> = (0..layout.block_size as usize).map(|i| (i % 251) as u8).collect();
    let slice_id = meta.next_id(SLICE_ID_KEY).await.unwrap() as u64;
    let uploader = DataUploader::new(layout, backend.as_ref());
    uploader
        .write_at_vectored(slice_id, 0u64.into(), &[bytes::Bytes::copy_from_slice(&data)])
        .await
        .unwrap();
    meta.append_slice(
        77,
        SliceDesc {
            slice_id,
            chunk_id: 77,
            offset: 4096,
            length: 1024,
            object_offset: 2048,
            object_size: layout.block_size as u64,
        },
    )
    .await
    .unwrap();

    let mut r = DataFetcher::new(layout, 77, backend.as_ref());
    r.prepare_slices().await.unwrap();
    let out = r.read_at(4096u64.into(), 1024).await.unwrap();

    assert_eq!(out, data[2048..3072].to_vec());
}
```

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::reader::tests::test_reader_uses_slicedesc_object_offset -- --nocapture
```

Expected before reader change: test fails by reading from physical offset `0` instead of `2048`.

- [x] **Step 2: Change read mapping**

In `DataFetcher::read_at` and `DataFetcher::read_at_into_prepared`, replace:

```rust
let slice_offset = SliceOffset::from(l - slice.offset);
```

with:

```rust
let slice_offset = SliceOffset::from(slice.physical_offset_for(l));
```

Keep `slice_len = r - l`; the logical interval calculation still uses `offset/length`.

- [x] **Step 3: Run reader tests**

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::reader::tests:: -- --nocapture
```

Expected: all reader tests pass.

## Task 3: Writer Emits New Canonical Descriptor Shape

**Files:**
- Modify: `src/vfs/io/writer.rs`
- Modify: `src/vfs/fs/mod.rs`
- Test: `src/vfs/io/writer.rs`

- [x] **Step 1: Update writer commit descriptor**

In `SliceHandle::desc_for_commit`, emit:

```rust
Some(SliceDesc {
    slice_id,
    chunk_id: s.chunk_id,
    offset: s.offset,
    length,
    object_offset: 0,
    object_size: length,
})
```

- [x] **Step 2: Update writeback crash recovery descriptor**

In `VFS::reupload_recovered_slice`, emit the same canonical object range:

```rust
object_offset: 0,
object_size: record.length,
```

- [ ] **Step 3: Run writer and recovery focused tests**

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs vfs::io::writer::tests:: -- --nocapture
```

Expected: writer behavior remains unchanged for ordinary writes.

## Task 4: Metadata Stores and Serialization Compatibility

**Files:**
- Modify: `src/meta/stores/redis/mod.rs`
- Modify: `src/meta/stores/etcd/mod.rs`
- Modify: `src/meta/stores/database/mod.rs`
- Modify: `src/meta/entities/slice_meta.rs`
- Test: metadata store tests under `src/meta/stores/*/tests.rs`

- [ ] **Step 1: Keep serialized JSON backward readable**

The added serde defaults must make existing JSON metadata readable. Do not require an online migration for JSON-format metadata.

- [ ] **Step 2: Audit tracing fields**

Where tracing logs `offset` and `len`, keep logical fields and optionally add physical fields:

```rust
object_offset = slice.object_offset,
object_size = slice.physical_size(),
```

- [x] **Step 3: Update database model conversion**

The SQL `slice_meta` entity now persists the physical range directly:

```rust
pub object_offset: i64,
pub object_size: i64,
```

`DatabaseMetaStore::init_schema` must also migrate older sqlite/postgres tables by adding these columns when missing and backfilling old rows with:

```text
object_offset = 0
object_size = length
```

Redis, etcd, and TiKV store serialized `SliceDesc` values, so they rely on serde defaults for old data and explicit fields for newly written data.

- [x] **Step 4: Add database round-trip regression test**

Add and keep:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs meta::stores::database::tests::test_slice_round_trip_preserves_object_range -- --nocapture
```

Expected: `append_slice/get_slices` preserves non-zero `object_offset` and full `object_size`.

- [ ] **Step 5: Run metadata tests**

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs meta::stores::redis::tests:: -- --nocapture
```

If etcd/database test suites are cheap in this checkout, run them too. If they require external services, document the skip and rely on unit-level serialization tests plus Redis.

## Task 5: Compactor Uses Logical Coverage and Physical Size

**Files:**
- Modify: `src/chunk/compact/compactor.rs`
- Modify: `src/chunk/slice.rs`
- Test: `src/chunk/slice.rs`

- [ ] **Step 1: Add tests for object-offset preserving coverage**

Add unit tests where two descriptors have the same logical ranges but different `object_offset`. `calculate_fragmentation`, `remove_fully_covered`, and `find_replaced_ids` must use logical `offset/length`, not physical object offsets.

- [x] **Step 2: Update delayed deletion data**

`encode_delayed_data` should encode `slice.physical_size()` rather than `slice.length`, so deleting a descriptor created from a larger physical object accounts for the whole object size when that slice id is removed.

For now, do not create multiple live descriptors sharing one `slice_id`. That requires refcount semantics and is a separate task.

- [ ] **Step 3: Run compactor tests**

Run:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::compact:: -- --nocapture
```

Expected: compaction remains correct for existing one-descriptor-per-object writes.

## Task 6: Add Multi-Descriptor Commit Interface

**Files:**
- Modify: `src/meta/store.rs`
- Modify: `src/meta/layer.rs`
- Modify: `src/meta/client/mod.rs`
- Modify: metadata store implementations as needed.
- Test: metadata client/store focused tests.

Before the writer can publish one physical object as several logical views, metadata must be able to append an ordered descriptor list while updating file size once. This is the bridge between the direct `SliceDesc` structure change and the writer performance work.

- [x] **Step 1: Add `write_slices` default to metadata traits**

Add a default method beside `write`:

```rust
async fn write_slices(
    &self,
    ino: i64,
    chunk_id: u64,
    slices: &[SliceDesc],
    new_size: u64,
) -> Result<(), MetaError>
```

Default behavior:

```text
if slices.is_empty(): return Ok(())
write(first slice, new_size)
append_slice(remaining slices in order)
```

This keeps all metadata backends correct before adding backend-specific atomic implementations.

- [x] **Step 2: Override database store with one transaction**

For `DatabaseMetaStore`, insert all descriptors in row order and update inode size in one transaction. This keeps same-object multi-segment commits ordered and avoids partial metadata exposure.

- [x] **Step 3: Teach `MetaLayer` cache invalidation to handle descriptor lists**

Invalidate reader/file caches for every logical descriptor range. Update inode size only once with the max `new_size`.

- [x] **Step 4: Add ordered multi-slice metadata test**

Create two descriptors sharing the same `slice_id` but different logical offsets and object offsets. Commit them through `write_slices`, read `get_slices`, and assert order and exact fields are preserved.

## Task 7: Enable JuiceFS-Style Slice Views Incrementally

**Files:**
- Modify: `src/chunk/compact/compactor.rs`
- Modify: `src/chunk/reader.rs`
- Modify: `src/meta/stores/*`
- Test: reader and compactor tests.

- [ ] **Step 1: Implement safe split-view compaction**

After Task 1-5 pass, add a compactor path that can preserve the uncovered left/right portions of an old physical slice by emitting descriptors with adjusted `object_offset` instead of forcing whole old slices to remain.

Example:

```text
old: logical [0, 4096), object_offset 0, object_size 4096
new: logical [1024, 2048), object_offset 0, object_size 1024

preserved old left:  logical [0, 1024), object_offset 0
preserved old right: logical [2048, 4096), object_offset 2048
```

- [ ] **Step 2: Do not delete shared physical objects**

Before split-view compaction is enabled, add an object-reference check. If any surviving descriptor has the same `slice_id`, do not stage delayed deletion for that physical object.

- [ ] **Step 3: Add tests for overlapping rewrite reads**

Read the whole logical block after partial overwrite and verify:

```text
old prefix + new middle + old suffix
```

Expected: the reader gets all three ranges from their correct physical offsets.

## Task 8: Cached Random Write Batching on Top of New Descriptor Semantics

**Files:**
- Modify: `src/vfs/io/writer.rs`
- Modify: `src/vfs/cache/page.rs`
- Test: writer tests.

- [x] **Step 1: Split SliceState logical view from physical object**

Add a small logical-segment list to `SliceState`:

```rust
struct SliceSegment {
    offset: u64,
    length: u64,
    object_offset: u64,
}
```

`SliceState.data` remains the physical object buffer. `SliceState.offset` should stop being the only logical range. Existing contiguous writes can still merge into one segment.

- [x] **Step 2: Append non-overlapping random writes physically**

When a writable slice cannot accept a write at the requested logical offset but has physical capacity left, append the new bytes to the physical object and add a `SliceSegment`:

```text
segment.offset = logical write offset
segment.length = write length
segment.object_offset = old physical object len
```

Reject reuse when:

```text
the write overlaps an existing segment and request ordering is older,
the target physical block range has already been dispatched/uploaded,
the physical object would exceed chunk size / freeze limit,
the write would create sparse zero-fill semantics.
```

- [x] **Step 3: Commit all logical segments**

Change `SliceHandle` from `desc_for_commit()` to `descs_for_commit()` returning ordered descriptors:

```text
slice_id = same physical object id
offset/length = segment logical range
object_offset = segment physical start
object_size = full physical object len
```

Use `MetaLayer::write_slices` to commit them as an ordered list.

- [x] **Step 4: Preserve overlay reads**

`overlay_dirty` and read-after-write must resolve logical offset through the segment list, not through `SliceState.offset` alone. Add tests where two non-contiguous cached writes share one physical object and are both readable before and after commit.

- [ ] **Step 5: Reuse full-block cached writeback only when safe**

Only emit a larger physical object when all pages for a block/range are present or when read-modify-write has materialized old bytes. The new `object_offset` semantics allow future split views, but they do not make sparse zero-fill safe by themselves.

- [ ] **Step 6: Gate with focused perf**

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

Keep changes only if:

```text
upload_batch avg increases above 0.3MiB,
partial_tail ratio drops materially below 0.96/0.97,
randwrite wall improves or remains within 3%,
randrw wall improves or remains within 3%,
reader/writer correctness tests pass.
```

**2026-06-22 focused perf notes:**

All runs used `fio-randwrite fio-randrw`, `direct=0`, `io_uring`, `iodepth=1`,
S3/RustFS, Redis metadata, `commit_before_upload`, FUSE workers=6, and
post-write drain.

| Candidate | randwrite | randwrite drain | randrw | randrw drain | Fio BW summary | Decision |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| Historical pre-segment best (`perf-run-1782105644-31160`) | 154s | 1s | 177s | 0s | randrw 332/148 MiB/s | Baseline to beat |
| Segment coalescing + old pending 1GiB/2GiB (`perf-run-1782121659-18935`) | 160s | 14s | 200s | 37s | randrw 265/119 MiB/s | Reject as-is; fewer PUTs but worse tail |
| Segment coalescing + writeback permits=12 (`perf-run-1782122451-8578`) | 165s | 14s | 201s | timed out/interrupted | randrw pending/inflight grew to GiB scale | Reject; higher writeback concurrency worsens tail |
| Segment coalescing + pending 256MiB/512MiB (`perf-run-1782231294-1729`) | 157s | 8s | 167s | 36s | randrw 328/147 MiB/s | Keep profile tuning; main workload improves while preserving BW |
| Same plus write memory 1GiB (`perf-run-1782231983-5107`, randrw only) | n/a | n/a | 87s | 2s | randrw 139/62 MiB/s | Reject; tail improves by throttling too hard |
| Same plus write memory 2GiB (`perf-run-1782232323-8641`, randrw only) | n/a | n/a | 86s | 0s | randrw 200/91 MiB/s | Reject; throughput loss still too large |

Current interpretation:

- Direct `SliceDesc` object-range support plus cached segment coalescing cuts
  PUT/metadata amplification substantially. Example: randwrite PUT count drops
  from about 23k to about 3k and upload batch average rises from about 0.4MiB
  to about 5.4MiB.
- The new bottleneck is writeback backlog shape, not descriptor correctness.
  Large physical slice objects keep fewer objects in flight, but each object
  has higher PUT latency; if the profile allows too much pending/dirty data,
  close/post-drain carries a large tail.
- Lowering pending soft/hard to 256MiB/512MiB is beneficial for the current
  coalescing design and is safe to keep in the perf profile. Lowering write
  memory to 1GiB or 2GiB removes the tail by imposing too much foreground
  backpressure and loses too much randrw throughput.

Next perf target:

- Keep 4GiB write memory and pending 256MiB/512MiB.
- Reduce post-drain without dropping randrw below 95% of the 4GiB throughput
  run. Candidate directions: earlier remote-upload dispatch for staged dirty
  slices, smarter coalesced-slice freeze cadence, or adaptive coalescing size
  based on remote upload latency.

## Required Verification Gates

Run these before any commit that changes production Rust:

```bash
cargo fmt --check
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::slice::tests:: -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs chunk::reader::tests:: -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs vfs::io::writer::tests:: -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
cargo test -p brewfs --bin brewfs meta::stores::redis::tests:: -- --nocapture
```

Run full CI-equivalent gates before declaring the branch ready:

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --workspace -- -D warnings
```

Run perf after correctness gates:

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
  --writeback-throughput-profile \
  --tools "fio-bigwrite fio-bigread fio-randwrite fio-randrw"
```

## Commit Policy

- Commit Task 1-2 together only after slice serialization and reader object-offset tests pass.
- Commit writer/recovery updates separately after writer tests pass.
- Commit compactor/shared-object semantics separately after compactor tests pass.
- Do not commit a performance candidate unless the focused perf gate shows no randwrite/randrw regression.
