# Aliyun packed-v3 vs JuiceFS Redis/TiKV million-file validation (2026-10-01)

## Status

The matched 10k smoke passed for all three filesystems/backends. The 1M run is
approved to start on a fresh disposable ECS after the runner changes pass local
gates. No 1M result is recorded yet.

## Workload contract

- one disposable `ecs.u1-c1m4.2xlarge` (32 GiB, 100 GiB ESSD);
- one OSS bucket/internal endpoint, unique root prefix;
- 1,000,000 independent 4 KiB files;
- 3 directory levels, fanout 10, 1,000 files per leaf;
- shared `tools/perf/smallfiles_scan.py`, 16 workers;
- tools run in order: tree, stat, full-read;
- a fresh mount and successful `drop_caches=3` before every tool;
- persistent payload cache, window cache and ordinary data prefetch disabled;
- JuiceFS attr/entry/dir-entry/open/data cache disabled;
- fixture/import, mount/warm-up, scanner active and drain durations reported
  separately;
- full reads validate every byte and require the same file count, byte count and
  checksum on all three sides.

The comparison rows are:

1. packed-metadata-v3, metadata prefetch `auto`, decoded-frame cache 0
   (`strict-cold`);
2. JuiceFS 1.3.1 + Redis strict caches;
3. JuiceFS 1.3.1 + TiKV v6.5.3 strict caches.

Any explicit packed decoded-frame cache result is reported separately as
`warm-frame-cache`; it cannot replace row 1.

## 10k smoke result

The smoke used the same layout at 10,000 files (`10×10×100`) and 4 KiB/file.
All rows passed full payload validation with checksum `1273096`, 40,960,000
payload bytes and zero errors. Cache directories were zero bytes/files before
and after every JuiceFS tool.

| tool | packed v3 files/s | JuiceFS+Redis files/s | JuiceFS+TiKV files/s |
| --- | ---: | ---: | ---: |
| tree | 56,568.50 | 47,553.74 | 15,730.31 |
| stat | 15,153.93 | 7,331.85 | 2,736.77 |
| full | 1,290.64 | 1,122.20 | 769.82 |

Latency and payload request evidence:

- packed stat p50/p95: 0.682/1.497 ms; data range GETs: 0;
- Redis stat p50/p95: 1.639/2.060 ms;
- TiKV stat p50/p95: 4.560/6.391 ms;
- packed full p50/p95: 8.747/33.357 ms, 723 physical data ranges,
  130,301,952 fetched bytes for 40,960,000 logical bytes;
- Redis full p50/p95: 13.781/21.356 ms;
- TiKV full p50/p95: 19.148/28.103 ms.

The smoke therefore confirms runner correctness and establishes that the
metadata-only packed scenario is favorable, but it is not the requested 1M
result.

## Diagnostics captured

- scanner JSON/log: files, directories, stat calls, bytes, checksum, errors,
  files/s, MiB/s, p50/p95 and scanner RSS;
- active, unmount drain and active+drain seconds;
- packed metadata warm-up, metadata/data GET and bytes, overscan, frame,
  coalescing, singleflight, pipeline and cache metrics;
- JuiceFS metrics before/after each tool;
- Redis command calls, memory and key count before/after;
- PD cluster status, store count and TiKV storage command totals before/after;
- cache and page-cache proof plus ECS memory/disk proof.

## Resource isolation and cleanup

JuiceFS data uses an explicit per-run OSS prefix instead of bucket-root
`chunks/`. Redis and TiKV use different volume/data/work identities. TiKV
cleanup waits for processes, force-kills leftovers and removes its TiUP home.
Every run is protected by ECS auto-release and outer `finally` cleanup.

The 10k smoke ECS was stopped/deleted after completion. An independent
`DescribeInstances` query returned zero instances for its id, and an OSS JSON
listing returned zero objects under the smoke root prefix.

## Completed lexicographic 1M baseline

The first 1M campaign completed all three rows with the lexicographic scanner.
Every full row validated 1,000,000 files, 4,096,000,000 payload bytes, checksum
`127493920` and zero errors. Local data caches remained disabled/empty.

| tool | packed v3 | JuiceFS+Redis | JuiceFS+TiKV |
| --- | ---: | ---: | ---: |
| tree files/s | 10,203.53 | 79,147.18 | 46,359.94 |
| stat files/s | 4,595.13 | 6,089.22 | 2,186.64 |
| full files/s | 681.04 | 1,144.90 | 674.13 |
| full MiB/s | 2.66 | 4.47 | 2.63 |
| full p50/p95 ms | 19.54 / 48.44 | 12.43 / 23.79 | 21.64 / 35.63 |

Active+drain seconds were:

- packed tree/stat/full: 99 / 219 / 1,471 seconds (approximately, from mount
  uptime and scanner/runtime evidence; concise runner rows recorded drain
  separately after the smoke fix);
- Redis: 14.69 / 168.00 / 877.91 seconds;
- TiKV: 23.72 / 460.85 / 1,488.53 seconds.

Packed metadata behavior changed with scale. A 512 MiB metadata budget warmed
only 31/245 inode-index pages and 913/4,111 GroupMeta pages. The full phase
recorded 666 inode-index remote GETs and 8,262 GroupMeta GETs. Packed full read
issued 136,883 data ranges and fetched 32.65 GiB for 4.096 GiB logical bytes;
this random 4 KiB layout therefore has substantial frame overscan. Redis kept
about 2,002,230 keys and used about 422 MiB.

Interpretation:

- packed beats TiKV stat by about 2.10x and is approximately tied on full read
  (1.01x), but loses tree;
- packed loses Redis at 1M even though it led the matched 10k stat smoke;
- this access order is lexicographic and benefits the physical packed order, so
  it is not a GPU DataLoader claim.

The campaign root prefix and ECS were independently verified empty/deleted after
completion.

## Pending GPU-like shuffled validation

A follow-up scanner profile uses deterministic per-epoch shuffle, batch size
256, 16 worker batches, bounded in-flight batches and two epochs. It still
performs stat/open/read-to-EOF and validates every payload byte. Epoch 1 is the
cold/shuffled result; epoch 2 measures repeated-epoch behavior. Packed strict
and explicit warm-frame-cache profiles remain separate. The lexicographic table
above remains a namespace/sequential baseline only.

## 1M acceptance

The 1M result is valid only if all three tools pass for all three rows, every
full-read checksum/byte count matches, all drop-cache/cache proofs are present,
and post-run ECS/prefix verification is zero. A TiUP/download/mount timeout or
cleanup failure invalidates the run and stops the campaign; Redis numbers must
not substitute for a missing TiKV result.
