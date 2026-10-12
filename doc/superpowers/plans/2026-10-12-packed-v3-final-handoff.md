# packed-v3/005 当前收尾交接（2026-10-12）

更新时间：2026-10-12 UTC  
工作区：/home/hxy/brewfs  
分支：codex/packed-metadata-aliyun-20260930  
当前 HEAD：13a45b1
工作树：干净  
PR：#161、#162、#164 已合并；#163 已关闭并由 #164 替代；#166 当前包含资源账本 fail-closed 与 RustFS 镜像源配置，等待 CI 后合并。

## 范围和硬约束

本交接只接受 packed-v3/005。v2、旧 wire、flat、CR 和 v5 兼容路线均不在目标内，也没有恢复兼容别名。后续性能和集成测试统一在 ECS i-bp138fribj3vquxxrpe6 上执行：8 vCPU、32 GiB、512 GiB 数据盘（/mnt/brewfs-data）。 Docker/RustFS 镜像优先使用 w23geq7ncegg6etqsl.xuanyuan.run。本地 WSL 只做静态审计、文档和轻量验证。

CI 的 workspace overlay control smoke 仅设置 `BREWFS_WORKSPACE_ALLOW_EXPECTED_CONTENTION=1` 以记录并放行明确的 `workspace is busy` contention；脚本默认值为严格模式，任何未知错误仍使运行失败。云端正式实验继续使用严格模式，预期 contention 只能作为 rejected diagnostic evidence。

## 当前已确认的代码和测试结果

| 目标 | 当前证据 | 状态 |
| --- | --- | --- |
| Redis 元数据 control-only | /mnt/brewfs-data/artifacts/eece983-control-redis-rerun-20261011T230656Z；当前 binary SHA-256 805ed09c7d1bee58fdfbe0ced65e3bb2725c59703f3414a7e74adef1c2303c06；dual-mount isolation、fork 8/8、seal 8/8、revision distinct、退出码 0、资源清理通过 | 已通过 |
| TiKV 无 writer 单 seal | /mnt/brewfs-data/artifacts/eece983-tikv-direct-seal-20261011T231926Z；fork 与单 seal 退出码 0。原始清理证明曾失败，后续 posthoc cleanup 已完成；历史 run 仍按 cleanup 失败保留 | 操作通过，批次不作为 clean acceptance |
| TiKV 默认 writer 压力 C1/C2/C8 | C1 seal 0/1、C2 seal 0/2、C8 seal 1/8；失败为 workspace is busy 或明确 bounded-read region/key-lock diagnostic，未知形状仍 fail-closed | 未通过 |
| TiKV 降低 writer 频率诊断 | t-hz06zri5c1i2328；约 50 writes/s 降到 5 writes/s 后 C1 fork/seal 通过；artifact /mnt/brewfs-data/artifacts/eece983-tikv-slowwriter-c1-20261011T233500Z，容器、volume、network 已清理 | 诊断通过，不替代默认压力出口 |
| RustFS/S3 当前 HEAD smoke | /mnt/brewfs-data/artifacts/eece983-rustfs-smoke-20261011T233500Z；当前 binary SHA 805ed09c...，RustFS digest sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c，wire 005，固定 trace c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662，100 files、tree/stat/smallfiles 均 0 errors，runner exit 0，资源清理通过 | 小规模 correctness/control smoke 已通过 |
| Python 性能 runner | python3 -m unittest discover -s tools/perf -p 'test_packed_*.py'：30 passed；campaign/runner 测试 10 passed | 已通过 |
| operator | 默认单元 17 passed；workspace-operator 全 feature 57 passed；manifests、脚本和 Compose 静态检查通过 | 局部通过 |
| Kubernetes/ACK E2E | 缺 API-server、Redis/TiKV namespace、Result Vault 和 managed backend 凭据 | 外部阻断 |

RustFS smoke 保留 toolchain.json、profile.env、run-manifest.json、summary.json，以及经审计隔离的 resource-journal.retrospective-rejected.json 和 retrospective-resource-evidence.json。由于原始 resource journal 是事后推断生成，严格 contemporaneous resource-journal validation 标记为 unavailable；不能把它当成严格资源账本通过。远端审计确认 BrewFS 容器、进程、挂载、volume 和 network 均已清理，artifact 目录仅作为证据保留。

## PR #162 CI 收尾结果（2026-10-12）
`c7016ba` 的 CI run `38185393737` 中，Rust 前置检查和 `Test workspace` 通过，`Test all features` 因 GitHub runner shutdown signal（exit 143）中断，未发现源码测试断言失败。Redis control smoke 的 fork 为 8/8，但 concurrent seal 为 7/8；TiKV fork 为 8/8，但 concurrent seal 为 5/8。失败请求均返回明确的 `workspace is busy`，与高频 mounted writer 下的已知 contention 边界一致；这些结果保留为失败证据，不能标为 acceptance。Rust job 已按原 commit 重跑，后端 smoke 结果不重跑、不拼接。

## TiKV 结论

无 writer 的单 seal 可以完成；挂载 writer 以高频率更新共享 catalog 时，单节点 TiKV 的 region/key-lock contention 会使有限 bounded-read/CAS 重试耗尽，外层返回 Busy 或保守失败。降低 writer 频率后通过，说明当前失败由测试压力和 writer/seal 协调造成，不能把默认 C8 记为通过，也不能通过放宽错误分类伪造成功。

下一轮若继续 TiKV，应在 seal 前实现或验证后端感知的 writer quiesce/协调协议，或在固定总 deadline 内采用有预算的退避；仍须保留未知 diagnostic 的 fail-closed 行为，并重跑默认压力、C1、C2、C8 以及完整 fork→mutation→drain→seal→remount→GC 生命周期。

## SPEC 差距

以下出口仍未关闭：

1. Redis/TiKV 完整生命周期：mutation、durable drain、seal/repack、S1 publish、remount/next-fork、reader pins、旧 head retirement 和 GC。
2. RustFS/S3 完整对象语义：parent context、namespace/codec、external digest、journal/atomic rotation、pins、orphan 和 packed GC。当前 smoke 只覆盖 100 文件读取。
3. 三个创新点的 paired acceptance：native 与 packed 使用同一 placement、executor、trace、资源预算和 release binary，至少三次重复，报告 active/drain/unmount/GC 以及 p50/p95/p99。
4. 消融：static/dynamic、inline on/off、metadata/data codec、p90 policy；还没有同源 trace 的完整结果。
5. 真实 FUSE/operator recovery：API-server/CEL、conditions/finalizer/lease、leader loss、recovery、PVC/数据盘清理和 ACK E2E。
6. managed-backend SPEC：Aliyun Redis/Tair、OSS、JuiceFS 对照和 Result Vault 尚无齐全外部凭据与结果。

因此当前系统可以进入下一轮实验，但不能把“所有 SPEC 已完成”写成事实；当前权威状态是“代码基线和小规模云 control smoke 完成，完整验收仍 active/incomplete”。

## 证据和磁盘卫生

保留上述云端 artifact、失败诊断和本交接文档。TiKV 慢 writer 诊断的容器、volume、network 已删除；远端当前没有活动容器或本 run 的残留 volume/network。不要删除 canonical proof 和失败 artifact；可回收的本地 target、镜像缓存和未绑定临时输出可继续清理。任何新的云 run 必须使用独立 bucket/prefix/metadata namespace，并生成相同五类 manifest/journal 文件。



## 2026-10-12 本轮执行记录

- `eb0376d` 将 workspace overlay suite 与 dual-mount harness 的实验入口收紧到 Redis/TiKV，并显式拒绝 sqlite/sqlx；`bash -n`、帮助文本和 sqlite 拒绝路径已验证。
- 当前 HEAD 的 ECS release 构建已重新提交，前一次失败原因是 Cloud Assistant `/bin/sh` 不支持 `pipefail`/process substitution；本次使用 POSIX shell，构建结果必须以 `/mnt/brewfs-data/artifacts/build-6cf1827-workspace-overlay-*/binary.sha256` 为准。
- 用户提供的镜像源 `w23geq7ncegg6etqsl.xuanyuan.run` 已纳入云实验约束；RustFS 使用已记录 digest，Redis/TiKV 镜像仍需在每行保存 RepoDigest。
- 本轮只允许把 Redis/TiKV control-only、TiKV contention 诊断和 RustFS/S3 小规模 FUSE smoke 记为对应范围的 correctness/diagnostic 证据；完整生命周期、native paired acceptance、ACK/operator 和 managed backend 继续保持 incomplete/blocked。
## 2026-10-12 云端校准结果

当前 release binary 已在 ECS 构建成功：`/mnt/brewfs-data/artifacts/build-6cf1827-workspace-overlay-20261012T011449Z/brewfs`，SHA-256 为 `92288b815646edba4db76c768078ce19484fc6754a996cf8831df91a8aa1e47e`。构建日志记录 `Finished release profile`；fixture 源文件在 `eece983` 与当前 `6cf1827` 的 hash 相同（`a88c178da2eb1d584b5700e77852443fca5d6f7981e25e9ee4daa71fdf7b70cc`），但当前 fixture 独立编译在 Cloud Assistant deadline 内未完成，因此 RustFS 行显式记录为 reused-fixture correctness evidence，不作为严格 binary/toolchain paired performance acceptance。

Redis 当前 binary control smoke artifact：`/mnt/brewfs-data/artifacts/6cf1827-redis-control-rerun-20261012T020200Z`。seed、双 workspace isolation 和 concurrent fork 均通过（8/8）；concurrent seal 为 3/8，5 个失败的 stderr 均为 `workspace is busy`。post-run cleanup proof 显示 Compose container/volume/network、主机 BrewFS PID 和 FUSE mount 全部为零；`acceptance.json` 为 `rejected`。该场景同时运行每 20ms writer 和 8 个 seal，8 个 seal 还共享全局 `packed/v3/topology-generation` 与 `alloc/sealed_version`，因此只能记录为联合 contention 压力证据；当前没有 no-writer C8 对照，不能把 writer 认定为唯一根因。

RustFS/S3 packed-v3/005 full-read smoke 使用镜像 `w23geq7ncegg6etqsl.xuanyuan.run/rustfs/rustfs:latest`，RepoDigest 为 `sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c`。三次独立 100×100 KiB、zstd/zstd、size-only、inline-on、TTL0/direct-io/keep-cache0 行分别为：

- `/mnt/brewfs-data/src/brewfs/docker/compose-xfstests/artifacts/packed-local-20261012T023607Z-34773`
- `/mnt/brewfs-data/src/brewfs/docker/compose-xfstests/artifacts/packed-local-20261012T024319Z-35618`
- `/mnt/brewfs-data/src/brewfs/docker/compose-xfstests/artifacts/packed-local-20261012T024335Z-36377`

三行均 `errors=0`、checksum `5050`、trace hash `36f0b83bcda0a6dd2e9a56851fb0942f499ecfe314623b5704b78a3418687066`，严格本地 resource journal 均 finalized passed，清理后的 cloud sidecar 明确标记为 retrospective/rejected。scanner 只产生 p50/p95，p99 为 unavailable；三行状态为 `valid-correctness`，不宣称性能接受、native paired 或完整对象生命周期已完成。

本轮新增 `workspace-control-resource-journal-v1` 为 workspace-v1 control runner 的独立资源账本；它记录 build/backend/control/test/cleanup 阶段、Compose resource snapshots、down 退出码和归零核验。该账本不冒充 packed-v3 run-manifest；任何 snapshot 或 Compose cleanup 失败都会使 control run 非零。
