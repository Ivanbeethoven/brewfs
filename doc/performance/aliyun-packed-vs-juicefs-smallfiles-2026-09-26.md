# Aliyun Packed 与 JuiceFS 小文件对照（2026-09-26）

## 测试条件

测试使用同一台 Aliyun ECS：`ecs.u1-c1m4.2xlarge`（32 GiB 内存，100 GiB ESSD），同一 OSS bucket，
同一目录形状和同一份逻辑内容。测试完成后 ECS、OSS 对象和 bucket 均已清理。

- 10,000 个文件，每个 100 KiB，逻辑数据量 1,024,000,000 字节（约 976.56 MiB）；
- 两级目录，10 个子目录/级，每个叶目录 100 个文件，共 110 个测试目录；
- 文件内容均为 `0x5a`，每个文件都完整读取；
- packed 和 JuiceFS 的扫描器都验证文件数、大小、完整读取字节数和 checksum；
- packed 使用 immutable packed fixture；小文件共享同一个 4 MiB data block；
- JuiceFS 使用 Redis 7 本地元数据，OSS driver，4 MiB block，`compress=none`；
- 每个 profile 都卸载、清理本地数据 cache，并执行 `sync; echo 3 >/proc/sys/vm/drop_caches`。

## 结果

| Profile | packed | JuiceFS | packed 相对 JuiceFS |
| --- | ---: | ---: | ---: |
| 严格冷读，无数据 cache/prefetch | 91.154 s，109.70 files/s | 192.581 s，51.93 files/s | 2.11x |
| 首轮扫描，开启 range/cache prefetch | 4.329 s，2310.15 files/s | 192.459 s，51.96 files/s | 44.46x |

严格冷读的 packed profile 使用 `read_memory_bytes=0`、
`read_ssd_bytes=0`、`range_background_prefetch=false`。这是当前无缓存
对照结果，packed 仍然约快 2.11 倍。

预取 profile 使用 1 GiB packed block-cache budget 和
`range_background_prefetch=true`。统计显示：

- `brewfs_read_background_prefetch_total=1`；
- `brewfs_read_range_gets_total=1`、`brewfs_read_full_gets_total=0`；
- `brewfs_read_block_cache_hits_total=9784`；
- `brewfs_cache_hit_ratio=0.999591`；
- `brewfs_s3_get_ops_total=3`、`brewfs_s3_get_bytes_total=204808`。

这说明小文件预取路径已生效：第一个文件触发 range read 和一次完整 block
background prefetch，后续同一共享 block 的读取直接命中 block cache。此前
百万级 profile 把读内存、SSD、VFS prefetch 和 range prefetch 全部关闭，所以
不会出现这个效果。

JuiceFS 的 prefetch profile 使用 4 GiB cache、`prefetch=16`。它与严格冷读
几乎相同，说明这份数据在 JuiceFS 中是每个文件独立的对象；扫描第一个文件
时没有可复用的跨文件数据 block。两边的逻辑数据相同，但物理对象布局不同，
因此预取结果也反映了 packed 的共享 immutable block 设计。

## 证据与复现入口

远端摘要保存在：

- `docker/compose-xfstests/artifacts/aliyun-jfs-compare-packed-cold/remote-output.log`
- `docker/compose-xfstests/artifacts/aliyun-jfs-compare-packed-prefetch-stats/remote-output.log`
- `docker/compose-xfstests/artifacts/aliyun-jfs-compare-juicefs-10k-final/remote-output.log`

原生 ECS runner：

- `docker/compose-xfstests/aliyun/run_aliyun_packed_native.sh`
- `docker/compose-xfstests/aliyun/run_aliyun_perf.ps1`
- `docker/compose-xfstests/aliyun/run_aliyun_juicefs_native.sh`
- `docker/compose-xfstests/aliyun/run_aliyun_juicefs_compare.ps1`

本次是 10k 低成本对照。它足以验证小文件 range prefetch 的行为和方向，
但不应直接外推到百万级 JuiceFS：JuiceFS 预填充已经产生约 1 GiB 的真实
对象写入，百万个 100 KiB 文件会明显放大写入时间、对象数量和清理成本。
