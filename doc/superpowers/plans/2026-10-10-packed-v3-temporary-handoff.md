# BrewFS packed-v3 临时交接文档

更新时间：2026-10-10
工作区：`/home/hxy/brewfs`
当前分支：`codex/packed-metadata-aliyun-20260930`
当前提交：`706b6c5 feat(packed-v3): close generation and runner audit gaps`
PR：[#160](https://github.com/brewfs/brewfs/pull/160)

## 交接边界

本轮只维护 packed-v3。没有为旧 v2/004 增加兼容协议或新的验收目标；代码中保留的旧 helper 仅服务已有 v3 统一执行器测试路径。元数据验收重点是 Redis 和 TiKV，数据对象路径重点是 RustFS/S3，真实 FUSE、operator lifecycle 和成对性能实验必须用外部环境证据签收。

不要把本地编译或文档状态写成完整 SPEC 验收。G11、G15、G16 本轮关闭的是窄子契约；G08、G10、G12、G13、G14 以及完整 S/X 仍需要外部生命周期验证。

## 本轮已经完成

- **G11 generation fence**：wire-005 `FetchedSources` 绑定 manifest 派生的 `ReadGeneration`；计划 generation 不匹配时在复制 inline/remote bytes 前返回 typed `ReadViewChanged`/`StaleView`，readonly adapter 保留 retryable downcast，observer 分类为 `FailureClass::Generation`。
- **G15 frame policy**：p90 policy 带认证 training trace digest；样本数、range、rank 和 histogram 累加均有边界/checked arithmetic；runner 把冻结 policy 传给 v3 fixture 并写入 manifest/profile provenance。
- **G16 local runner**：记录 toolchain、scanner seed、layout controls 和 durable `packed-v3-resource-journal-v1`。Compose、mount、worker、temporary work 的 ownership/cleanup 都有 terminal event；成功运行若仍有资源存活会 fail closed，失败运行也必须留下 cleanup decision。
- **证据文档**：packed-v3 gap audit、system readiness 和 G16 resource-journal checkpoint 已更新，明确列出开放项。

## 本地验证证据

- `python3 -m unittest -v tools.perf.test_packed_p90_policy`：7 passed。
- `python3 -m unittest -v tools.perf.test_packed_resource_journal tools.perf.test_packed_run_manifest tools.perf.test_packed_local_runner`：18 passed。
- `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p brewfs --features workspace-overlay --bin packed_v3_snapshot_fixture`：通过。
- `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test -p brewfs --features workspace-overlay bound_source_rejects_a_plan_from_another_generation_before_copying -- --nocapture`：1 passed。
- `cargo fmt --all --check`、`git diff --check`、`bash -n tools/perf/run_packed_local.sh`、Python compilation：通过。
- 合计 Python 回归 25 passed；没有在本地声称真实 Docker/FUSE/Redis/TiKV campaign 已完成。

## PR/CI 状态

PR #160 已推送并附加。首次 CI run `38058651092` 的 Redis/TiKV smoke 和 Rust job 在 runner 收到 shutdown signal 时以 exit 143 结束；Rust 在中止前已报告 `1177 passed; 0 failed; 225 ignored`，smoke 日志没有代码断言失败。stress-ng、pjdfstest 和 Results web 已通过。失败 jobs 已按基础设施原因重跑；重跑 job 114236178984、114236179178、114236179216 仍分别以 runner shutdown/exit 143 收尾，且 Rust 重跑在中止前再次报告 `1177 passed; 0 failed; 225 ignored`。这批 CI 失败没有代码断言或编译错误证据，合并前应保留该限制并按仓库权限决定是否使用 admin merge。

如果重跑仍失败，先检查 job log 最后是否为 `The runner has received a shutdown signal`。只有出现实际编译、断言或 FUSE/后端错误时才修改代码；纯 exit 143 应重新运行或记录为 CI 基础设施故障。

## 当前 SPEC 差距

1. **Redis/TiKV 外部生命周期**：需要在 Compose/API-server 环境完成 init、fork、mutation、seal、remount、并发 stale retry、cleanup 和故障恢复；本地 typed fence 测试不能替代这项证据。
2. **RustFS/S3 对象路径**：需要真实 RustFS/S3 的 wire-005 root/blocks/group/container/index/cold/descriptor/external 读写、ETag/HEAD、range/decoder/output/cache 预算和失败重试记录。
3. **真实 FUSE 与 operator**：需要真实 mount/unmount、并发 mutation 无混合 generation bytes、有限整体 retry、Kubernetes CRD/CEL、conditions/finalizer/lease、operator recovery。Ready 不能替代 storage 验收。
4. **GC/publish/recovery**：需要 durable reader pins、history retirement、staging roots、manifest-last、crash/reopen recovery、对象图 mark/sweep 和 orphan 回收证据。
5. **实验签收**：需要 static/dynamic/inline/p90 的同内容 paired 对照，Redis/TiKV/RustFS/S3 端到端 trace，active/drain/unmount/GC 分阶段计时，消融和接受/拒绝记录。G17 仍开放。
6. **云 dispatch/release reproducibility**：local runner 已有窄契约，但 release 构建、cloud dispatch、10k 真实 FUSE run、artifact retention 和源码/vendor 完整复现仍未签收。

## 下一位 agent 的顺序

1. 等 PR #160 重跑结束；若仅 exit 143，保留当前代码并记录 CI infra；若有真实失败，先做最小修复并重复本地定向验证。
2. CI 满足仓库合并条件后合并 PR #160，记录 merge commit 和主分支 CI。
3. 合并后以 Redis、TiKV、RustFS/S3 为三条独立 campaign 入口，先完成小规模控制 smoke，再扩展到真实 FUSE 和 paired experiment；每次运行保留 manifest、resource journal、toolchain 和 failure artifact。
4. 只在外部证据齐全后更新 G08/G10/G11/G12/G13/G14/G17 的验收状态；不要删除开放项来制造“全部 SPEC 完成”。

## 关键文件

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
