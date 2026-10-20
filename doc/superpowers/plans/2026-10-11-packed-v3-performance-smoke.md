# packed-v3 性能验证小规模结果

更新时间：2026-10-11
工作区：/home/hxy/brewfs
分支：codex/packed-metadata-aliyun-20260930
HEAD：98f292e1（工作树干净；云端验证使用此提交构建）

本记录覆盖 packed-v3/005 的小规模 control smoke，并记录本轮 Redis/TiKV 后端结果。debug 单次结果不升级为性能验收；Redis/TiKV 完整元数据生命周期、RustFS/S3 paired campaign、operator 和大规模 release/FUSE 仍按临时交接文档开放。

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

## PR #161 Redis/TiKV 并发 fork 失败记录（2026-10-11）

本轮将 workspace-overlay control smoke 从 PR #160 收敛到 PR #161，真实后端仍是 Redis/TiKV 元数据路径。CI run `38092955788` 暴露了两类可复现失败，不能按“runner 不稳定”关闭：

- Redis concurrent fork：8 个并发操作仅 4 个成功，4 个返回 `Error: workspace is busy`。根因是共享 `layer-inventory-generation` 的 CAS 竞争在 64 次快速重试内耗尽；原实现 CAS 失败只 `yield_now()`，没有使用已有退避策略。
- TiKV concurrent fork：8 个操作 7 个成功，1 个返回 `FailedPrecondition: bounded read returned a key or region error`。details 为 `[66,82,69,1,2,1,0]`，对应 bounded Get 的 key-level lock 诊断；未知/未认证 key error 仍必须 fail-closed。

已提交的修复只针对上述根因：`borrowed_alias.rs` 在 carrier expectation CAS 失败时复用 `retry_backoff(attempt)`；TiKV bounded-read 增加固定形状的 `BRE + location=1 + family=key + subtype=locked + shape=complete` 识别，并设置独立、受预算限制的同一时间戳只读重试预算。重试保持固定 timestamp、原始 request budget 和总 deadline，不做锁解析、锁解决或写入重放。

相关失败 artifact 保留在：

- Redis：`C:/Users/HXY/AppData/Local/Temp/brewfs-overlay-redis-161`
- TiKV：`C:/Users/HXY/AppData/Local/Temp/brewfs-overlay-tikv-161-raw`

这些修复尚未完成 release 编译、定向 Rust 测试和 Redis/TiKV `--control-only` 重跑；在这些验证、CI 新 run 以及 cleanup 证据全部通过前，PR #161 禁止合并。Rust job 的 exit 143 仍按 runner shutdown 基础设施故障记录，不能替代后端并发验证。

## 2026-10-11 Redis/TiKV control-only 结果

本轮使用新 release binary（`target/docker/brewfs`，SHA-256 `b2236a51822197abc3d95375b39baa6d8ccc64ae02701bed89a8c50a98a3b3ab`）和重建的 `brewfs-workspace-xfstests:local` 镜像。

### Redis：通过

Artifact：`docker/compose-xfstests/artifacts/live-control-redis-20261011-sealfix/`。真实 FUSE dual-mount 隔离通过，mounted writers=2；并发 fork 8/8 成功，批量耗时 0.047734134 s（167.595 ops/s）；并发 seal 8/8 成功，批量耗时 0.885777314 s（9.032 ops/s）。脚本正常退出，artifact 保留 fork/seal JSON、mount/writer 日志和 `brewfs.log`。这验证了本轮 seal 阶段 `TopologyTxn::commit()` 遇 `Busy` 时的有限退避重试。

### TiKV：环境阻断及后续诊断

官方 runner 在 `curlimages/curl:8.10.1` 拉取时被配置 registry mirror 的 TLS handshake timeout 阻断，原始证据保留在 `docker/compose-xfstests/artifacts/live-control-tikv-20261011-sealfix/compose-up.log`。使用本机已有 `pingcap/tikv:v8.5.0` 作为临时 readiness 镜像后，PD/TiKV、init/seed/fork/isolation 均通过，但 concurrent-fork 在 8 个操作中第 7 个返回固定诊断 `FailedPrecondition: bounded read returned a key or region error`, details `[66,82,69,1,2,1,0]`；诊断 artifact 保留在 `docker/compose-xfstests/artifacts/live-control-tikv-20261011-sealfix-local-ready/`。本轮新增代码现将该确定的只读 key-lock 冲突在无剩余 batch slot 时映射为 `WorkspaceError::Busy`，让外层 topology/CAS 操作重建事务；需要新 binary 重建后重跑 TiKV C8 才能验证修复。该结果不能计为 TiKV 通过。

本轮新增 seal Busy focused Rust test 在最终链接阶段使 WSL 实例失去响应，未返回断言结果；该测试不记为通过。`cargo check --no-default-features --features fuse-tokio-runtime,workspace-overlay` 已通过（本轮修复后的代码需再次复核）。

## 验证门禁

- python3 -m unittest discover -s tools/perf -p test_*.py：39 passed。
- cargo fmt --all --check、相关 shell bash -n、git diff --check：通过。
- 历史基线 cargo test --workspace --lib --bins：1177 passed、0 failed、225 ignored；本轮新增 focused test 因 WSL 链接阶段实例失去响应，未形成通过证据。
- 本轮 release build：成功，binary SHA-256 `b2236a51822197abc3d95375b39baa6d8ccc64ae02701bed89a8c50a98a3b3ab`。
- 本轮 Redis control-only C8：fork 8/8、seal 8/8、dual-mount isolation PASS。
- 本轮 TiKV control-only：官方 readiness 镜像被 registry TLS timeout 阻断；本地 readiness workaround 暴露 1/8 bounded-read key-lock 失败，仍未通过。

## 不能由本记录关闭的门禁

- 不能用上述 debug/tiny/single-run 结果填写 native/packed acceptance table 或声明 G08/G10/G11/G12/G13/G14/G17 完成。
- 仍需固定 trace 的 native/packed paired runs、static/dynamic/inline-off/p90 消融、release build、多次重复和 p90/CI 规则。
- Redis/TiKV 元数据仍需真实 fork→mutation→durable drain→seal/repack→remount→GC；本轮只关闭 Redis control-only smoke，TiKV 仍待新 binary 重跑。RustFS/S3 需 parent context、namespace/codec、journal/rotation/pins/GC；operator/FUSE 故障与 recovery 仍开放。

## 2026-10-11 云端验证更正和后续入口

本节为当前 HEAD 98f292e1 的权威小规模云端记录。用户已要求所有后续测试上云；本地 RustFS/debug 结果保留为历史 correctness/control smoke，不能替代云端结果。执行 ECS 为 i-bp138fribj3vquxxrpe6，8 vCPU、32 GiB、系统盘 100 GiB、数据盘 512 GiB，挂载点 /mnt/brewfs-data；OSS bucket 为 brewfs-tikv-100k-20260928。云端 task 为 t-hz06zpl80x3h9mo，release binary SHA-256 为 b7757759b764bd67468efc607affa3a93b5f3cb6fb8538bd5a1ace99e12c72f2。

| workload | files | payload | errors | latency / rate | trace |
| --- | ---: | ---: | ---: | --- | --- |
| packed-tree | 100/100 | 0 bytes | 0 | discovery 5.140589 s；19.453023 files/s | c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662 |
| packed-stat | 100/100 | 0 bytes | 0 | p50 18.968117 ms；p95 31.102575 ms；13.136240 files/s | c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662 |
| packed-smallfiles | 100/100 | 102400 bytes | 0 | p50 43.936339 ms；p95 68.252816 ms；9.929362 files/s | c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662 |

三项 summary、profile、stats 和 trace 由云端同步到 docker/compose-xfstests/artifacts/aliyun-cloud-smoke-20261011/cloud-proof-summary.txt；原始输出见 remote-output.log，本地重跑归档见 work/aliyun-cloud-rerun-98f292e。packed-stat 和 packed-tree 的 payload_bytes 为 0，因此 mib_per_sec 保持 null 或 0，不能伪造数据带宽。该 smoke 证明 packed-v3/005 在 RustFS/S3 小规模读取路径上有 0 errors，但不证明 Redis/TiKV 元数据生命周期、operator recovery、GC 或 native/packed paired acceptance。

后续性能 campaign 固定使用该 ECS 和数据盘，逐次变更只允许写入独立 OSS prefix 与 metadata namespace。artifact 必须包含云主机规格、磁盘挂载、commit、binary hash、固定 trace、profile、阶段计时、请求账本、cleanup journal 和资源删除证明。TiKV concurrent fork/seal、真实 Redis/TiKV fork 到 GC、native/packed paired、static/dynamic/inline-off/p90 消融、operator recovery 和多次 release 重复均仍开放；任何一项未完成时不得更新 G08、G10、G11、G12、G13、G14 或 G17 为完成。

## 2026-10-12 current HEAD correction

The 2026-10-11 numbers above are historical evidence. Current authoritative status is recorded in ../superpowers/plans/2026-10-12-packed-v3-final-handoff.md. Redis control-only is now 8/8 fork and 8/8 seal on eece983; default TiKV writer-pressure C1/C2/C8 remains a contention failure, while no-writer and throttled-writer diagnostics pass. A fresh RustFS/S3 smoke on eece983 uses binary SHA 805ed09c... and artifact /mnt/brewfs-data/artifacts/eece983-rustfs-smoke-20261011T233500Z. These results do not close full lifecycle, paired performance, ablation, operator, or managed-backend SPEC gates.
