# BrewFS packed-v3 临时交接文档

更新时间：2026-10-12
工作区：`/home/hxy/brewfs`
本地分支：`codex/packed-metadata-aliyun-20260930`
PR：[brewfs/brewfs#161](https://github.com/brewfs/brewfs/pull/161)（当前 OPEN；HEAD 6ab05ce 已推送；最新 CI run 38156082345 正在运行，Redis/TiKV/Rust 尚未满足合并门禁）

## 1. 交接范围和三项创新

本项目当前只接受 packed-v3/005。旧 v2、旧 wire、flat、CR 兼容不是本轮验收目标；代码中仍存在的旧 004 helper 不构成 v3 证据，也不应继续扩展兼容协议。

三个创新的完整范围是：

1. readonly packed metadata：Redis/TiKV 管理 workspace/catalog/manifest/binding，RustFS/S3 承载 packed 对象，真实 FUSE 负责读取和错误/卸载语义。
2. dynamic immutable data placement：同一 immutable placement 由 native/packed 共享计划和 executor；支持 static/dynamic frame、inline-off、metadata/data codec 和带训练 provenance 的 p90 policy。
3. overlay workspace：S0→fork A/B→mutation→durable drain→seal/repack→原子发布→remount/next-fork→GC，并由 operator 管理 capability、binding、lease、finalizer 和恢复。

这三项必须共同满足系统出口 S 后，才能进入主实验 X。局部代码测试、native-only 结果和文档不能把 S/X 标成完成。

## 2. 当前提交与本轮修改

本轮主要实现基线是提交 706b6c5（feat(packed-v3): close generation and runner audit gaps）；随后补充了临时交接文档、稳定 Rust API 修复和 CI 资源约束；当前基线 HEAD 为 6ab05ce（工作树干净且与 origin 同步）；426c358、6dd8378、98f292e1 已提交并推送。以 git log 和 PR 页面核对远端 head SHA。

本轮实现内容：

- G11：wire-005 `FetchedSources` 绑定 manifest 派生的 `ReadGeneration`；generation 不匹配在填充 buffer 前返回 typed `ReadViewChanged`/`StaleView`；readonly adapter 保留 retryable downcast，observer 分类为 `FailureClass::Generation`。
- G15：p90 policy 认证 training trace digest；sample count、range、rank、histogram 累加均有边界和 checked arithmetic；runner 将冻结 policy 传给 v3 fixture，并写入 manifest/profile provenance。
- G16：local runner 记录 toolchain、seed、layout controls 和 durable `packed-v3-resource-journal-v1`；Compose、mount、worker、temporary work 的 ownership/cleanup 有 terminal event，成功仍有资源存活时 fail closed，失败也保留 cleanup decision。
- CI 工具链：把 stable 已标记 deprecated 的 `Atomic::fetch_update` 改为等价的 `try_update`，避免后续 `clippy -D warnings` 将既有 warning 当成失败；这是兼容当前稳定 Rust 的小修复，不改变业务语义。

## 3. G01–G17 逐项状态

下表是交接基线。`窄契约` 表示代码和局部证据已具备，仍不能替代外部生命周期出口；`开放` 表示不应合并为“已完成”。

| ID | 当前状态/已有证据 | 仍需完成的出口 |
| --- | --- | --- |
| G01 | **窄项已签收**：005 writer/decoder 的 kind/mode/rdev/identity 与旧 payload 拒绝已覆盖。 | 继续只验收 v3；完整当前 payload/gate 仍按 CI 运行。 |
| G02 | **rooted/frozen source 窄项已签收**：root-FD/no-follow、Btrfs readonly lease、source guard 和 46 gate/8 FUSE/3 Btrfs 证据。 | frozen-view 协议、完整 workspace publication/recovery、single-file 弱一致性边界。 |
| G03 | **source sparse/blocks/external 窄项已签收**：PM09/EX09/PS09/LE09/LD05、有界 chunks、hardlink 复用、raw/zstd 525-entry FUSE。 | >64MiB dense/all-hole、300 段 sparse、跨 chunk/page/EOF/holes、blocks/attrs/EROFS 与 workspace release/GC 的新证据。 |
| G04 | **readonly ACL/typed errno 窄项已签收**：raw names、special、hardlink、xattr、readonly ACL。 | 可写 set/remove/chmod/create 继承、后端独立实例并发/事务故障、完整 POSIX。 |
| G05 | **rank/select、SQL seek、有界 36k inventory 窄项已签收**：raw/zstd 导入、分页 cookie、restart、active fd、20 秒卸载曾通过；失败工作证据保留。 | typed GET 图、physical eviction/refetch、较大规模/hot protection、RSS 和 teardown 根因。 |
| G06 | **HTTP status-drop 子契约已签收**：backend/validated/logical/http-attempt/semantic terminal 账本守恒，非 2xx body 丢弃按 HttpStatus 失败记账。 | startup/type/retry、raw/decoded/union、probe/manifest/IP06/inline/FD/CA/payload 的完整请求图和计数。 |
| G07 | **V3 admission shutdown 窄项已签收**：Stored/Raw 阻塞 admission close 唤醒、无 partial charge。 | read/queue/pin/stored/raw/decoder/output 全生命周期预算，cancel/failure/slow consumer/eviction/shutdown、真实 FUSE/RSS。 |
| G08 | **same-snapshot unified executor 窄项已签收**：native/packed 共享 UnifiedReadPlan/Fetcher，计划/字节回读和 adapter read 局部通过。 | 四格、真实 Redis/TiKV/RustFS/S3/FUSE、同 immutable placement 的完整实验路径。 |
| G09 | **005 singleflight/profile/cancel/join 窄项已签收**：22 passed，覆盖 bounded flight、同帧单 GET、profile 和 shutdown/join。 | 生产 FUSE 跨请求 trace、stats、eviction/refetch、cold/warm、取消/卸载和性能接受。 |
| G10 | **PWB3 binding/open/attach 窄项已实现**：独立 manifest、本地 fixture/KV 初次 install、authority、missing binding reject、Absent/Hole fallback。SQLite 只作为局部回归 fixture，不是后端验收目标。 | 全 namespace/hot/cold/full/partial 的真实 mounted A/B；fork→mutation→seal→remount；Redis/TiKV 组合验收。 |
| G11 | **generation fence 本轮完成窄项**：005 FetchedSources 和旧 helper 均 typed fence；readonly retryable 证据已通过。 | 并发真实 FUSE/native/packed 生命周期的有限整体 retry、全输出丢弃、无混合 generation bytes。 |
| G12 | **same-head publication 窄项已实现**：KV timed CAS、epoch/sequence/allocator、producer/readback/manifest-last 子项；本地 transaction 仅是回归 fixture，Redis/TiKV 是目标后端。 | IP06/FD/cold/container/external 依赖闭合、durable candidate journal/staging roots、effective-view/head rotation、crash/reopen/幂等 recovery。 |
| G13 | **native PWB root/ABA/grace 窄项已实现**：root generation、Deleting 扫描前复检、层集合代数和回归。 | durable reader pins、history retirement、workspace/lease/journal roots、005 object graph mark/sweep、orphan 回收、无误删。 |
| G14 | **capability/status/CRD 窄项已实现**：v3 capability/binding conditions、缺失能力不推导 Ready、CRD panic 修复。 | API-server/CEL、Redis/TiKV Kubernetes E2E、conditions/finalizer/lease、real packed binding 和 operator recovery/GC。 |
| G15 | **本轮控制窄项完成**：static/dynamic/inline-off/raw/zstd/p90 policy 和 provenance 已接 fixture/manifest。 | offline builder 完整 distribution、同内容/group/container 规则、实际输出统计和 paired experiment。 |
| G16 | **local runner 窄项完成**：toolchain/trace/layout/resource journal/cleanup proof。 | release build、cloud dispatch、三端同 trace、10k 真实 FUSE、artifact retention、未测字段 null 和 paired acceptance。 |
| G17 | **全开放**：已有历史 1M/TTL 记录不能替代当前 005 paired acceptance。 | 完整 AGENTS gate、消融、生命周期、接受/拒绝记录、源码/vendor 可复现、最终交付。 |

## 4. Redis/TiKV/RustFS 当前实情

最新 Redis/TiKV metadata 证据不是“从未验证”：stores05 为 328 passed/0 failed/101 ignored，SDK03 为 122 passed/0 failed/1 ignored。真实 diagnostic04 的 46 项全部执行，但只有 38 passed、8 failed；失败阶段为 fork 0/2、initial composer 1/1、headless 0/2、native resume 11/1、specialized auth 0/2。对象 runtime/admin 凭据分离已接入，但尚未完成编译和真实权限验证。生产 CLI6、operator/中央 leader GC、完整同迭代门禁仍开放。

RustFS/S3 有窄的真实证据：12 objects、55,228 bytes、48 HTTP attempts/24 ranges、HEAD 零 payload、账本与库存一致，missing cold/GC suffix 被拒绝，容器和进程清理通过。parent context/namespace/codec/external digest、journal/atomic rotation、pins 和 packed GC 仍开放；这项证据不能升级成完整 RustFS/S3 生命周期完成。

## 5. 本地验证和 CI 边界

已完成的局部和提交门禁验证：

- python3 -m unittest discover -s tools/perf -p test_packed_*.py：30 passed。
- cargo fmt --all --check、git diff --check、shell/Python syntax：通过。
- CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --workspace：通过。
- CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --workspace：通过。
- CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins：1177 passed、0 failed、225 ignored；bin tests 通过。
- CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --workspace：通过；只有既有 vendor dead-code/future-incompat 警告。
- cargo check -p brewfs --no-default-features --features fuse-tokio-runtime：通过。
- cargo check -p brewfs --no-default-features --features fuse-io-uring-runtime：通过。
- generation fence 定向 Rust test：1 passed；asyncfuse tokio runtime：47 passed。
- compose 脚本静态 gate（perf report、JuiceFS matrix/report）：通过。

完整 all-features 测试、operator/CRD diff 和真实 Redis/TiKV/RustFS/S3/FUSE lifecycle 仍由外部 CI/环境负责；focused tests 不能替代这些出口。

PR #160 最新 run 38061849001 已完成：Rust all-features job 以 exit 143 结束，Redis/TiKV overlay jobs 分别在 control smoke 阶段被 cancelled；日志明确为 The runner has received a shutdown signal/Terminated，没有测试断言、编译错误、OOM 或磁盘不足证据。stress-ng、pjdfstest、Results web 已通过。该 run 的失败应记录为 GitHub runner/service 基础设施中止，不归因于本轮代码回归；最终提交后仍需重跑 CI，若出现真实编译或断言失败再修复。


## 6. PR #161 当前未完成项与合并门禁

当前工作树基线 HEAD 为 98f292e1，源码与文档均已提交并推送到 PR #161。

- `src/workspace_overlay/stores/kv_store.rs`：compaction/control 与 begin/advance/hash/commit seal 的 pre-submit `Busy` 退避重试，并增加 MemoryBackend 回归覆盖。
- `src/cli.rs`：seal acquire/execute/release 阶段错误上下文。
- `src/workspace_overlay/stores/kv_store/packed_journal/registry/borrowed_alias.rs`：carrier expectation CAS 失败复用 `retry_backoff(attempt)`。
- `src/workspace_overlay/stores/tikv_bounded_read.rs`：固定 timestamp 的 key-lock retry budget；无剩余 batch slot 时返回 `WorkspaceError::Busy`，让外层 topology/CAS 重建只读事务。
- `vendor/tikv-client/src/bounded_codec.rs` 与 `vendor/tikv-client/src/common/errors.rs`：只识别固定形状的 key-level lock diagnostic，并暴露受限只读重试判定。
- `src/workspace_overlay/stores/kv_backend.rs`：明确 `Busy` 仅表示后端已证明未提交。
- `doc/superpowers/plans/2026-10-10-packed-v3-temporary-handoff.md`、`doc/superpowers/plans/2026-10-11-packed-v3-performance-smoke.md`：记录本轮证据和开放门禁。

PR #161 CI run `38092955788` 的真实失败必须先修复并重跑：Redis 8-way concurrent fork 为 4/8，4 个返回 `workspace is busy`；TiKV 为 7/8，1 个返回 `FailedPrecondition: bounded read returned a key or region error`，diagnostic details `[66,82,69,1,2,1,0]`。对应 artifact 保留于 `C:/Users/HXY/AppData/Local/Temp/brewfs-overlay-redis-161` 和 `C:/Users/HXY/AppData/Local/Temp/brewfs-overlay-tikv-161-raw`。Rust job 的 exit 143 是 runner shutdown，不能视为后端通过。

合并前的顺序固定为：

1. 完成 no-default-features release build，并复制启用 `workspace-overlay` 的 binary；不要复用未启用该 feature 的旧 binary。
2. 运行 `tikv_bounded_read::batch_tests` 与 `pr141_entity_cas_tests::concurrent_forks_from_one_revision_have_distinct_heads` 定向测试；若 WSL 链接阶段再次失去响应，记录为环境阻断，不宣称通过。
3. 使用 `BREWFS_WORKSPACE_CONCURRENCY=8` 运行 Redis、TiKV `--control-only --reuse-images`，确认 concurrent-fork/concurrent-seal 为 8/8、cleanup 完整，并检查 metrics/logs 中无新的 key diagnostic。Redis 已完成 8/8；TiKV 官方 readiness 镜像被 TLS timeout 阻断，本地 readiness workaround 暴露 1/8 key-lock 失败，已加入 Busy 外层重试修复，需新 binary 重跑。
4. 上述修复已提交并 push 到 PR #161；等待新 CI run，记录其结果并以 38131837909 作为当前最新门禁依据。
5. 仅当 Redis、TiKV、Rust 及必需检查全部通过后，才可按用户授权 merge；在此之前禁止合并。

修复后的 retry count 当前没有独立 metrics；`successful_operations_per_second` 已包含重试成本，不能解释为原始 backend 吞吐。

## 7. S/X 实验进入条件和下一步

系统 S 必须先证明：一致 S0→fork A/B→A mutation→durable drain→seal/repack→S1 原子发布→remount/next-fork→GC；源、权限、数据、namespace、恢复、资源均正确。实验 X 之后才能冻结：native/packed 同 placement/executor、static/dynamic/inline-off/p90 控制、release/provenance、固定请求 trace、Redis/TiKV/RustFS/S3 端到端、active/drain/unmount/GC 分段计时、paired 对照和接受/拒绝规则。

下一位 agent 的顺序：

1. retry 修复已提交并推送 PR #161；重跑并记录新的 CI run，纯 runner shutdown/exit 143 作为基础设施故障保留证据，但不能替代 Redis/TiKV 8/8 control smoke。
2. 在 Redis、TiKV、RustFS/S3 三条 campaign 入口执行小规模 control smoke，再扩展真实 FUSE、operator 和 paired experiment。
3. 只有外部证据齐全才更新 G08/G10/G11/G12/G13/G14/G17 的验收状态；不要用文档或合成数值关闭开放项。
4. 用户已授权合并；只有 Redis/TiKV/Rust 及必需检查通过后才执行 merge，并把 PR 检查结论和合并 commit 追加到本文件或 PR 记录。

关键文件：

- `src/workspace_overlay/packed_v3/remote.rs`
- `src/workspace_overlay/packed_v3/wire.rs`
- `src/workspace_overlay/packed_v3/wire005.rs`
- `src/workspace_overlay/packed_v3/wire005/manifest.rs`
- `src/workspace_overlay/packed_v3/readonly.rs`
- `tools/perf/run_packed_local.sh`
- `tools/perf/packed_run_manifest.py`
- `tools/perf/packed_resource_journal.py`
- `doc/superpowers/plans/2026-10-03-packed-v3-spec-gap-audit.md`
- `doc/superpowers/plans/2026-10-04-brewfs-system-readiness.md`
- `doc/superpowers/plans/2026-10-10-packed-v3-temporary-handoff.md`

## 7. 2026-10-11 性能 smoke 结果

详细记录见 doc/superpowers/plans/2026-10-11-packed-v3-performance-smoke.md。当前 feature-enabled debug runner 已在真实 RustFS Compose + FUSE 完成两个可复现 control smoke：full 100×1KiB（100/100、0 errors、manifest/journal passed、mount 0.3235s、active 5.4079s、drain 0.1379s、wall 5.8693s）和 metadata stat 100 files（100/100、0 errors、p50 10.20353ms、p95 11.946053ms、manifest/journal passed）。两者 cleanup 均证明 mount/process/compose/work 不存活。

本轮修复了 runner fixture 重复 access-profile、单 epoch summary 验证和 stat/tree 零 payload 除零，并保留失败 artifacts 作为证据。新 release binary SHA-256 为 `b2236a51822197abc3d95375b39baa6d8ccc64ae02701bed89a8c50a98a3b3ab`；Redis control-only C8 fork/seal 均 8/8，artifact 为 `docker/compose-xfstests/artifacts/live-control-redis-20261011-sealfix/`。TiKV 官方 readiness 拉取失败 artifact 为 `docker/compose-xfstests/artifacts/live-control-tikv-20261011-sealfix/compose-up.log`；本地 readiness workaround 的 1/8 key-lock 失败 artifact 为 `docker/compose-xfstests/artifacts/live-control-tikv-20261011-sealfix-local-ready/`。上述结果是 release control smoke，不能关闭 G17 或填写 acceptance table；Redis/TiKV metadata 完整生命周期、native/packed paired、release 多次重复、RustFS/S3 完整 journal/rotation/pins/GC、operator/recovery 仍开放。

## 8. 2026-10-11 当前 HEAD、云端资源和验证更正

本节覆盖本文件前面历史记录中仍引用 188dda3 的部分。当前可复核基线是 HEAD 6ab05ce，工作树干净，且与 origin/codex/packed-metadata-aliyun-20260930 同步。连续提交为 426c358、6dd8378、98f292e1：分别收敛 Redis CAS 退避、TiKV bounded scan 的固定时间戳 key-lock 重建，以及云 runner 的 512 GiB 数据盘、LF 上传规范和 packed payload 检查。以下证据不得与旧 HEAD 的历史数字混用。

用户已指定后续性能和集成测试全部在独立云主机执行。当前云端资源为 ECS i-bp138fribj3vquxxrpe6，8 vCPU、32 GiB 内存，系统盘 100 GiB，数据盘 512 GiB，数据盘挂载到 /mnt/brewfs-data；自动释放时间为 2026-10-13T09:56Z。对象测试使用 OSS bucket brewfs-tikv-100k-20260928。云端任务 t-hz06zpl80x3h9mo 使用 HEAD 6ab05ce 生成的 release binary，SHA-256 为 b7757759b764bd67468efc607affa3a93b5f3cb6fb8538bd5a1ace99e12c72f2。

RustFS/S3 packed-v3 smoke 已在该云端 runner 完成 100 files × 1 KiB：packed-tree、packed-stat 和 packed-smallfiles 均为 100/100、0 errors；smallfiles payload 为 102400 bytes；三项共享 trace SHA-256 c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662。packed-smallfiles p50/p95 为 43.936339/68.252816 ms，packed-stat p50/p95 为 18.968117/31.102575 ms，packed-tree discovery 为 5.140589 s。完整证明保存在 docker/compose-xfstests/artifacts/aliyun-cloud-smoke-20261011/cloud-proof-summary.txt，远端原始输出保存在同目录 remote-output.log；本地同步结果目录为 work/aliyun-cloud-rerun-98f292e。该批次是 RustFS/S3 correctness/control smoke，不是 Redis/TiKV 完整生命周期或三创新性能接受结论。

同一 ECS 将作为后续云测试的唯一执行入口。每次 run 必须记录 instance id、规格、数据盘挂载、HEAD、binary SHA-256、trace、后端 namespace/prefix、artifact 路径和 cleanup 结果；必须在 artifact 保留后卸载 FUSE、停止 compose 或 runner、删除临时 namespace/prefix，并检查资源无残留。WSL 本地只承担静态审计、文档、源代码检查和构建产物准备，不得把本地 debug smoke 当作性能接受证据。磁盘紧张时优先清理可重建 target、镜像缓存和未绑定临时输出，保留 canonical proof 和失败诊断。

PR #161 的最新 CI run 38131837909 已结束但整体失败：Redis overlay 通过，TiKV overlay 在 concurrent fork 失败，Rust job 也失败且倾向 runner 或基础设施中止；stress-ng、pjdfstest、Results web 通过。该 run 不能视为合并门禁通过。云端 workspace-overlay 镜像构建任务 t-hz06zpoiy2f1h4w 仍属于准备工作，不构成测试结果。下一步必须在上述 32 GiB ECS 上用 98f292e1 binary 重跑 TiKV control-only C8，并保存 concurrent-fork-metrics.json、concurrent-seal-metrics.json 和 cleanup proof；只有 TiKV 8/8、Redis 8/8、Rust 以及必需 CI 检查全部通过后，才可合并 PR。

## 2026-10-12 TiKV channel exhaustion remediation

Commit 6ab05ce (fix(tikv): retain bounded store client channel) is pushed to PR #161. Config.with_bounded_resources keeps max_cached_regions=0 and max_cached_stores=0, but sets max_cached_kv_clients=1 so each endpoint reuses one live RPC channel. The workspace-overlay configuration test asserts this bound. The previous value 0 created a new TCP channel for every region request; concurrent writer/seal TIME_WAIT exhausted ephemeral ports and produced Cannot assign requested address.

The vendor/tikv-client focused suite passed 123 tests, with 0 failures and 1 ignored; format and diff checks passed. The local workspace-overlay focused test and cargo check were interrupted by WSL memory pressure during large linking/checking and are not counted as passes. CI run 38156082345 is the current cloud gate; merge requires Redis, TiKV, Rust, and required jobs to pass. TiKV must report concurrent fork 8/8 and concurrent seal 8/8, with no Cannot assign requested address, and complete FUSE/container/volume/namespace cleanup.

Cloud testing remains pinned to ECS i-bp138fribj3vquxxrpe6 (8 vCPU, 32 GiB, 512 GiB data disk at /mnt/brewfs-data) using Docker mirror w23geq7ncegg6etqsl.xuanyuan.run. Every run records commit, binary SHA, trace, backend namespace/prefix, artifact path, and cleanup proof.
