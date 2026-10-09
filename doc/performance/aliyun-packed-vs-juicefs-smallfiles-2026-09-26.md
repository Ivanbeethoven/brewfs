# Aliyun Packed 与 JuiceFS 小文件对照（2026-09-26，公平重测）

## 测试条件

测试使用同一台 Aliyun ECS：`ecs.u1-c1m4.2xlarge`（32 GiB 内存、100 GiB ESSD）、
同一 OSS bucket、同一目录形状和同一份逐文件内容。测试结束后 ECS、OSS 对象和
bucket 均已清理。

- 10,000 个文件，每个 100 KiB，逻辑数据量 1,024,000,000 字节；
- 两级目录，10 个子目录/级，每个叶目录 100 个文件，共 110 个目录；
- 第 `n` 个文件内容是确定性的 8-byte 序号模式，扫描器校验内容头、大小、总字节数和 checksum；
- packed 使用 `packed-metadata-v1`，每个文件一个独立 immutable slice，逻辑 offset 为 0；
- JuiceFS 使用 Redis 7、本地元数据、OSS、4 MiB block、`compress=none`；
- 两边每个 profile 都清理数据 cache，并执行 `sync; echo 3 >/proc/sys/vm/drop_caches`；
- packed 的 `read_memory_bytes=0`、`read_ssd_bytes=0`、prefetch 全关闭；
- JuiceFS strict profile 使用 `cache-size=0`、`prefetch=0`。

## 结果

| Profile | packed v1 | JuiceFS strict | packed 相对 JuiceFS |
| --- | ---: | ---: | ---: |
| 严格冷读（第一次） | 235.636 s，42.45 files/s | 155.289 s，64.42 files/s | 0.659x |
| 严格冷读（packed 复测） | 207.838 s，48.13 files/s | 155.289 s，64.42 files/s | 0.747x |
| 严格冷读（range-fix candidate） | 208.809 s，47.91 files/s | 155.289 s，64.42 files/s | 0.744x |
| packed 两次平均 | 221.737 s，45.10 files/s | 155.289 s，64.42 files/s | 0.700x |

两次 packed 都通过完整内容校验，cache hit 为 0；第一次有 9,996 次 range GET，
复测有 9,999 次 range GET。新的 range-fix candidate 同样通过完整内容校验，cache
hit 为 0，`brewfs_s3_get_ops_total=9994`、`brewfs_read_range_gets_total=9994`、
`brewfs_read_full_gets_total=0`，读取字节为 1,024,000,000，checksum 为
`1273096`。它把每文件一次 legacy 探测去掉了，但总耗时与旧复测几乎相同（208.809 s
vs 207.838 s），说明当前瓶颈仍是每个小文件一个 OSS range 请求和 FUSE/对象存储
往返延迟，而不是 namespace 探测；因此不能把这项修正表述为吞吐领先。

## 读路径归因与修正

这次 v1 结果变慢的主要原因已经从挂载统计中确认，而不是 Redis 或数据缓存命中：

- 10,000 个 100 KiB 文件产生 20,000 次 FUSE read；每个文件的第二次请求是到达
  文件末尾后的 EOF 查询，并没有对应的 OSS 数据请求；
- 每个冷文件实际产生约一个 `chunks-v2` 头部布局探测和一个数据 range GET，
  因此 `brewfs_s3_get_ops_total` 约为 20,000，而 `brewfs_read_range_gets_total`
  约为 10,000；
- 普通 `ObjectBlockStore` 必须保留 `chunks/` legacy 回退，所以默认不能删除这个
  探测；`packed-metadata-v1` 的 manifest 已经限定不可变数据引用为 framed
  `chunks-v2` 对象，因此 packed 挂载启用 `versioned_objects_only`，跳过每个冷
  block 的 namespace 探测。只有新生成的 manifest 声明
  `PACKED_DATA_UNCOMPRESSED_BLOCKS` 时，读路径才进一步跳过 framed header 并直接
  做 payload range GET；没有该能力位的旧 v1 manifest 仍安全地走一次完整 framed
  GET，避免把压缩 bytes 当作文件内容。新构建的云端 candidate 已确认该路径：
  `brewfs_s3_get_ops_total` 与 `brewfs_read_range_gets_total` 均为 9,994，且没有
  full GET。

这里的 `chunks-v2` 只表示数据块对象的 versioned namespace，不是
`packed-metadata-v2`。本报告的云端测试仍是 `packed-metadata-v1`；真正的 v2 是
`BRFSM002/BRFCL002/BRFDS002` 分簇元数据和物理 placement 方案，尚未接入这条 FUSE
验收命令，不能用本表代表 v2 性能。

## 结论与格式限制

此前的 `2.11x` 和 `44x` 结果来自所有小文件共享同一个 `(slice_id, block_index,
offset)` 和重复内容，属于缓存污染/重复数据 fixture，已撤回。

当前 v1 producer 会把 extent value 的第四字段校验为文件逻辑 offset，v1 的
`SliceDesc` 也没有独立的物理 `slice_offset`。因此不能在 v1 fixture 中把多个文件
放入一个 4 MiB 对象再写不同物理 offset；这样会被格式校验拒绝。此次重测改为每文件
独立 slice，结果只代表当前 v1 读路径和元数据索引，不代表 v2 物理 packed data
placement。要验证跨文件 block packing，必须先实现 v2 extent/placement 的物理 offset
字段和读取路径，再使用相同 fixture 重新对照。

## 证据与复现入口

- `docker/compose-xfstests/artifacts/aliyun-fair-v1-10k/remote-output.log`
- `docker/compose-xfstests/artifacts/aliyun-fair-v1-10k-repeat/remote-output.log`
- `docker/compose-xfstests/artifacts/aliyun-v1-rangefix-10k/remote-output.log`
- `docker/compose-xfstests/artifacts/aliyun-fair-jfs-10k/remote-output.log`
- `docker/compose-xfstests/aliyun/run_aliyun_packed_native.sh`
- `docker/compose-xfstests/aliyun/run_aliyun_perf.ps1`
- `docker/compose-xfstests/aliyun/run_aliyun_juicefs_native.sh`
- `docker/compose-xfstests/aliyun/run_aliyun_juicefs_compare.ps1`
