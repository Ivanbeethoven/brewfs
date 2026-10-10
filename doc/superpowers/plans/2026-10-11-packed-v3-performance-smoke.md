# packed-v3 性能验证小规模结果

更新时间：2026-10-11
工作区：/home/hxy/brewfs
分支：codex/packed-metadata-aliyun-20260930
HEAD：d5921a3d

本记录只覆盖 packed-v3/005 的可复现小规模 control smoke，不把 debug 单次结果升级为性能验收。Redis/TiKV 元数据生命周期、RustFS/S3 完整 paired campaign、operator 和大规模 release/FUSE 仍按临时交接文档开放。

## 控制变量

- 后端：仓库 Compose 的 RustFS latest，S3 endpoint 127.0.0.1:19000。
- FUSE：真实 /dev/fuse，direct I/O，keep-cache=0，TTL=0，metadata prefetch/off，decoded/frame-window cache=0。
- corpus：100 files × 1024 bytes，2 levels、10 dirs/level、1 file/leaf，wire 005，size-only frame，inline on，metadata/data zstd，random-small-file。
- request trace：固定 shuffle seed；full run 为 20261011，stat run 为 20261012。
- binary：cargo build -p brewfs --features workspace-overlay --bin brewfs --bin packed_v3_snapshot_fixture 生成的 debug binary；因此结果是 correctness/control smoke，不能作为 release 性能结论。

## 通过结果

### Full data read

Artifact：docker/compose-xfstests/artifacts/packed-local-20261010T224707Z-1550448/

- manifest/resource journal：passed，exit status 0。
- 100/100 files，102400/102400 payload bytes，0 errors；request trace aded0a228a1dfe3e194365f4b566528667c80b281f23cf0e5b6e18fd730d0232。
- mount 0.3235 s，active 5.4079 s，drain 0.1379 s，wall 5.8693 s。
- active bandwidth 0.01806 MiB/s；active+drain 0.01761 MiB/s；wall 0.01664 MiB/s。
- scanner p50 27.6835 ms，p95 31.32431 ms，peak scanner RSS 约 21 MiB。
- FUSE/object observer 记录了 logical bytes、HTTP attempts、range fetch、inline raw union 和预算峰值；backend failures=0、budget rejections=0。
- cleanup：mount/process/compose/work 全部 false，terminal journal 完整。

### Metadata stat read

Artifact：docker/compose-xfstests/artifacts/packed-local-20261010T224740Z-1551300/

- manifest/resource journal：passed，exit status 0。
- 100/100 files，0 errors，stat p50 10.20353 ms，p95 11.946053 ms，peak scanner RSS 约 20 MiB。
- active 3.5587 s，drain 0.1338 s，wall 4.0182 s。
- payload_bytes=0 时 timing 明确写入 bandwidth=null，避免把 metadata-only 运行伪装成数据吞吐。
- cleanup：mount/process/compose/work 全部 false。

## 失败证据和修复

- packed-local-20261010T223541Z-1540892：fixture 重复收到 --access-profile，exit 2；资源清理完整。
- packed-local-20261010T223737Z-1542969：使用未启用 workspace-overlay 的旧 debug binary，CLI 不认识 packed-metadata-v3，exit 1；重建 feature-enabled binary 后复现通过。
- packed-local-20261010T224147Z-1545199：fixture 和真实 FUSE 读取完成，但单 epoch scanner 输出 epochs 标量，manifest validator 错误拒绝；修复 scalar/list 兼容后通过。

本轮代码修复：删除 fixture 命令重复的 --access-profile；manifest validator 仅在 epochs 为列表时展开，否则把顶层 summary 当作单 epoch；stat/tree timing 在没有 payload bytes 时输出空吞吐值；新增对应 runner 回归断言。

## 验证门禁

- python3 -m unittest discover -s tools/perf -p test_*.py：39 passed。
- cargo fmt --all --check、相关 shell bash -n、git diff --check：通过。
- cargo test --workspace --lib --bins：1177 passed、0 failed、225 ignored。

## 不能由本记录关闭的门禁

- 不能用上述 debug/tiny/single-run 结果填写 native/packed acceptance table 或声明 G08/G10/G11/G12/G13/G14/G17 完成。
- 仍需固定 trace 的 native/packed paired runs、static/dynamic/inline-off/p90 消融、release build、多次重复和 p90/CI 规则。
- Redis/TiKV 元数据需真实 fork→mutation→durable drain→seal/repack→remount→GC；RustFS/S3 需 parent context、namespace/codec、journal/rotation/pins/GC；operator/FUSE 故障与 recovery 仍开放。
