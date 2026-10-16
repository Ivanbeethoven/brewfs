# packed-v3/005 当前收尾交接（2026-10-12）

更新时间：2026-10-12 UTC  
工作区：/home/hxy/brewfs  
分支：codex/packed-metadata-aliyun-20260930  
当前基线：e980d592801388c3c359d7e82b966df84f636e6f；2026-10-13 轮次改动见本文首节对应 PR（分支 codex/packed-v3-busy-native-budget-split）。
工作树：干净  
PR：历史 PR #161、#162、#164 已合并；当前收尾证据以 e980d592 云端 artifact 和 2026-10-13 ECS 本机 artifact 为准。

## 2026-10-13 overlay + packed-v3/005 统一生命周期轮次（最新，优先阅读）

基线 `e980d59`；本轮源码改动随本 PR 提交，评审时的候选 diff 为
`build/candidate3.diff`（SHA-256 `1b950c0c…`）。全部实验在 ECS 主机 `brewfs-singapore-v2` 本机执行，未使用 GitHub Runner。
证据根目录：`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z`（下称 `$R`）。

### 结论分级

| 范围 | 状态 | 证据 |
| --- | --- | --- |
| overlay control 矩阵（Redis/TiKV × no-writer C8、C1、C2、默认压力 C8、quiesce C8、降频 C8，各 3 次，严格模式） | **passed**（候选 3） | `$R/overlay/c3-matrix-summary.json`；36/36 行退出 0，fork 210/210、seal 210/210，资源账本全部 passed、Compose 资源归零 |
| 同一矩阵在原始 `e980d59` 上 | **rejected** | `$R/overlay/matrix.jsonl` 前 9 行：Redis 默认 C8 seal 4/8、no-writer C8 7/8；TiKV 默认 C8 5/8、C1 0/1；失败全为 `workspace is busy` |
| 真实后端 packed 生命周期核心集（18 例，Redis+TiKV，RustFS/S3 + 运行时/管理员 IAM） | **rejected**：16/18 | `$R/runs/packed-lifecycle-rustfs-20261012T235716Z-274316`；两例 pending-upload 失败 |
| 同一核心集，LocalFS 对象 | **rejected**：15/18 | `$R/runs/packed-lifecycle-localfs-20261012T233539Z-262034`；另有一例 TiKV 间歇 EIO |
| RustFS SID GC/孤儿回收（6 例，管理员仅读/删，PUT 被 IAM 403 拒绝） | **passed** 6/6 | `$R/runs/packed-lifecycle-rustfs-20261013T005543Z-434700` |
| 初始 history 删除 + native 层 GC（2 例） | **rejected**（e980d59 与候选 3 相同失败） | `run_at` 返回 `Busy`，见下 |
| paired acceptance：native-tikv vs packed 元数据放置（同 release、fixture、trace、RustFS、预算，各 3 次交替） | **passed**（full、stat 两组） | `$R/paired-final3/accept-{full,stat}-*`，表见下节 |
| 消融（static/dynamic、inline、codec、p90） | **incomplete** | runner `tools/perf/run_packed_ablation.sh` 已就绪未运行；p90 无独立训练 trace |
| overlay workspace vs 原生性能对照 | **incomplete** | 尚未建立同 trace 对照 runner |
| ACK/operator E2E、Aliyun managed backend | **blocked**（凭据/集群缺失，未变） | — |

### `workspace is busy` 设计修复（2026-10-13 最终形态，替代下文第 2 项的逐点补丁）

根因：`TopologyTxn::commit` 把两类**预提交** Busy 原样抛给调用方——packet 准备时发现 sibling seal/fork 推进了共享 root/inventory/topology epoch，以及 TiKV 在提交前耗尽 key-lock 重试并已回滚。此时尚无任何写入，本质是普通 CAS 冲突，但 17 个调用点中只有 7 个各自特判了 Busy，其余（install_compaction、acquire_lease、fast_forward_commit、mark_workspace_deleting 等）直接失败，表现为并发 seal 下零星 `workspace is busy`。

修复：在 `TopologyTxn::commit` 一处把这两类 Busy 归一为 `Ok(false)`；所有调用点都已处于 `CAS_MAX_RETRIES`（256 次，每次 ≤100 ms 退避）循环中，按既有 false 路径从新读取重建。只有整轮预算耗尽才返回 `workspace is busy`。先前 7 处逐点 `match Busy` 补丁已删除，规则只保留在一处。提交后的未知结果、Fenced 和后端错误保持原样（fail-closed 不变）。直接调用 `prepare_topology_packet` 的路径（如 `release_lease` 和 packed 管理路径）不在本次范围内。

验证：`every_topology_transition_rebuilds_after_presubmit_busy`、`acquire_lease_retries_busy…`、`seal_flatten_retries_busy…`、`seal_retries_busy_returned_by_topology_commit` 均通过（`$R/build/busy-tests-central2.log`）。中心化修复的 release（SHA-256 `701b5c1f…`，镜像 `sha256:d1930e07…`）重跑 overlay 严格模式矩阵：Redis/TiKV × no-writer C8、C1、C2、默认压力 C8 各 3 次，24/24 行退出 0，fork 与 seal 全部成功，资源账本 passed、资源归零（`$R/overlay/cz-matrix-summary.json`）。seal p50/p95/p99：Redis 默认 C8 0.121/0.180/0.192 s，TiKV 默认 C8 4.547/5.861/5.875 s。

### native-tikv 读路径预算修复（2026-10-13，paired acceptance 前置）

首次 10k 文件 paired 运行中 native-tikv 臂 `full` 模式 479/10000 读失败（ENOMEM），packed 臂 0 失败。复现与定位（`$R/paired-probe/`）：

1. `NativePackedPlacementProvider::prepare_unified_read_observed` 为每次读固定预留 1 MiB decode workspace（默认池 8 MiB），16 worker 并发时直接拒绝。改为按记录实际字段长度计费（`NativeInode::clone_bytes`，约 4 KiB）。
2. 放大 workspace 后仍失败：共享 placement 入口每次读先预留至多 2 MiB 的 Plans arena，16 个并发读超过 32 MiB 池容量即 ENOMEM，而 packed 臂因读路径更短未触发。改为 `V3MountBudget::admit_waiting_for_plans`：只等待 Plans 池，不持有任何部分预留，arena 在执行前收缩、读完成即释放，故等待总能前进；超过池容量、预算关闭或其他池耗尽仍立即失败。inode 入口与 placement 入口共用该函数。

3. 仍有 333/10000 失败。新增 debug 级别的预算拒绝归因（`brewfs::packed_v3_budget` target，记录被耗尽的池）后定位：非特权 FUSE 每次 open 都解析路径，`get_paths_bytes_owned` 固定预留 8 MiB workspace，4 个并发 open 就占满 32 MiB 池，读路径 4 KiB 的 clone 预留随之失败。改为路径 arena 从 1 MiB 起按实际工作量（pending + 结果的 2 倍）增长，上限仍为 8 MiB；增长时先释放旧 arena 再等待，避免持有部分预留排队。`names_raw` 与读路径 clone 预留也改为等待而非立即 ENOMEM。

TDD：`native_concurrent_read_preparation_does_not_exhaust_default_workspace` 在恢复固定 1 MiB 计费的二进制上以 ENOMEM 失败（`$R/build/tdd-native-red.log`）；最终版本同时覆盖 16 个并发读、16 个并发路径解析与读交错，要求全部成功且 workspace 峰值低于池容量。`native_placement` 15/15 通过；`workspace_overlay` 954 passed，其余失败与 e980d59 基线同一集合（`writable_acl_independent_sqlite…` 在基线上间歇失败）。真实 10k 文件 native-tikv `full` 探测（release `c8c96b92…`）：10000/10000、0 错误，decode workspace 峰值 16 MiB（原先打满 32 MiB）（`$R/paired-probe/native-full-final3-*`）。

后续范围：按用户指示，sealed base 只要求基本功能；本节之后的未完成项中，凡与直接性能无关的（初始 history GC Busy、pending-upload 契约、TiKV 间歇 EIO）不再深究。

### paired acceptance 结果（release `c8c96b92…`，10000×100 KiB，16 worker，2 epoch，TTL0/direct-io，RustFS 回环）

runner：`tools/perf/run_packed_paired.sh`（每组独立 TiKV namespace、RustFS 生命周期；arm 每轮交替顺序；trace、checksum、binary 必须一致，任一运行失败或清理失败即 rejected）。native-tikv 的导入（约 320 s）是独立的未计时步骤；两组 cleanup proof 均为零。表中为 3 次中位数，files/s 与延迟取第 2 epoch。

| 模式 | arm | mount s | active s | files/s | p50 ms | p95 ms | p99 ms |
| --- | --- | --- | --- | --- | --- | --- | --- |
| full | packed | 0.24 | 172.18 | 117.5 | 141.8 | 171.0 | 183.6 |
| full | native-tikv | 93.55 | 305.44 | 69.8 | 214.0 | 236.9 | 248.0 |
| stat | packed | 0.35 | 60.11 | 354.8 | 49.6 | 58.4 | 62.4 |
| stat | native-tikv | 96.49 | 111.35 | 212.0 | 70.2 | 80.2 | 85.3 |

结论：同一数据路径下 packed 元数据放置在 full 与 stat 两种模式均快约 1.7 倍，挂载耗时从约 95 s 降到 0.3 s 以下。两组 summary 原始版本因 runner 把多 epoch trace 当成一个集合而误判 rejected，已保留为 `summary.original-trace-check-bug.json`；修正后的 `packed_paired_summary.py` 按每次运行的 epoch 序列比较，trace/checksum/binary 均一致，状态 passed。

### 本地 CI gate（PR 提交前）

`cargo fmt --check`、`check/build --workspace`、四组 feature check、`cargo test --workspace --lib --bins`（1177 passed / 0 failed）、`cargo clippy --workspace` 均通过。`cargo clippy --all-targets --features workspace-overlay -D warnings` 有 3 个错误（`remote.rs:1076`、`source.rs:10`、`borrowed_alias.rs:858`），在未修改的 e980d59 上同样存在，非本 PR 引入。

### 本轮修复（均有失败证据→修复→回归）

1. **维护型 CAS 丢失 CONTROL 检查（PR141 合并后回归）**。普通实体 CAS 不锁 CONTROL 是 PR141 的设计，但 packed carrier 发布、carrier 元数据删除和首次 ClaimInitial Prepare 是维护型转换，测试契约要求其检查集包含精确的 catalog header。修复位置：`complete_driver.rs` commit_owned 的 authority extras、`borrowed_alias.rs` `prepare_carrier_metadata_deletion`、`kv_store.rs` 终结化 extra-check 白名单（与本次 census 读取比较）、`native_begin.rs` ClaimInitial。效果：真实后端 native factory、history retirement、fork alias 在 Redis/TiKV 均由 failed 变为 passed；`workspace_overlay` 单元 942→953 passed。
2. **seal flatten 与 lease acquire 未重试预提交 Busy**。新增 seal 步骤归因日志后定位：Redis 失败全部来自 `install_compaction`（seal 的 flatten 步），TiKV no-writer 剩余失败来自 `seal acquire`（`acquire_lease`）。两者在 `txn.commit()` 的 packet 准备阶段收到 `Busy` 时直接 `?` 返回，而 begin/advance/hash/commit seal 已在同一预算内重试。修复为同一有界 `retry_backoff` 重建（256 次，单次 ≤100 ms）；未知错误仍原样返回。两项均有 TDD：`seal_flatten_retries_busy_returned_by_compaction_commit`、`acquire_lease_retries_busy_returned_by_topology_commit`，先在去掉修复的二进制上失败（`$R/build/tdd-red.log`、`tdd-acquire-red.log`），再通过。
3. **seal 步骤归因**：`lifecycle::seal_step` 在不改变错误值的前提下记录失败步骤（begin/quiesce/drain/hash/commit/flatten），用于 contention 分类。
4. **overlay harness 写入控制**：`BREWFS_WORKSPACE_WRITERS`（0/1/2）、`BREWFS_WORKSPACE_WRITER_INTERVAL`、`BREWFS_WORKSPACE_WRITER_QUIESCE`（暂停→每个 mount `sync`→确认→seal 批次→恢复，30 s 有界）。metrics 记录 `profile_class`：只有 2 writer、20 ms、无 quiesce 才是 `default-pressure`，其余标记 `diagnostic`。默认值保持原高频压力。

### overlay 矩阵（候选 3，`release` SHA-256 `9e4bfd04…`，镜像 `sha256:870e8405…`）

| profile | 次数 | fork | seal | seal p50/p95/p99 (s) | 每次写入文件数 |
| --- | --- | --- | --- | --- | --- |
| redis no-writer C8 | 3 | 24/24 | 24/24 | 0.113/0.168/0.183 | 0 |
| redis C1 默认 | 3 | 3/3 | 3/3 | 0.039/0.040/0.040 | 10–11 |
| redis C2 默认 | 3 | 6/6 | 6/6 | 0.048/0.059/0.059 | 12 |
| redis C8 默认压力 | 3 | 24/24 | 24/24 | 0.126/0.199/0.199 | 20–22 |
| redis C8 quiesce | 3 | 24/24 | 24/24 | 0.109/0.192/0.196 | 8 |
| redis C8 降频 200 ms | 3 | 24/24 | 24/24 | 0.119/0.181/0.202 | 4 |
| tikv no-writer C8 | 3 | 24/24 | 24/24 | 3.777/4.837/4.960 | 0 |
| tikv C1 默认 | 3 | 3/3 | 3/3 | 0.675/1.018/1.018 | 8–12 |
| tikv C2 默认 | 3 | 6/6 | 6/6 | 1.199/1.559/1.559 | 11–13 |
| tikv C8 默认压力 | 3 | 24/24 | 24/24 | 4.903/6.816/7.079 | 34–41 |
| tikv C8 quiesce | 3 | 24/24 | 24/24 | 3.805/4.962/4.988 | 6–10 |
| tikv C8 降频 200 ms | 3 | 24/24 | 24/24 | 4.454/6.044/6.105 | 22–26 |

这是 overlay workspace-v1 seal/fork 的 control 证据（`workspace snapshot` 走 native seal + flatten），不是 packed-v3 发布或 paired acceptance。TiKV 默认压力 C8 已不依赖降频或 quiesce 通过；降频与 quiesce 行仍只作诊断。
候选 2（未含 acquire 修复）曾有 TiKV no-writer C8 2/3 次失败于 `seal acquire`，保留在 `matrix.jsonl`。

### 真实后端 packed 生命周期覆盖（候选 3 测试二进制 `ae8c449f…`）

runner：`docker/compose-xfstests/run_packed_lifecycle_real_backends.sh` + `docker-compose.packed-lifecycle-backends.yml`。每次运行拥有独立 Compose 项目、回环端口、每例 UUID metadata namespace、独立 RustFS bucket 组与新建的运行时/管理员 IAM 身份（`tools/testing/rustfs_owned_iam.py`，运行时 DELETE 403、管理员 PUT 403 在开跑前实测）。每次生成 `run-manifest.json`、`toolchain.json`、`profile.env`、`summary.json`、`resource-journal.json`、`cleanup-proof.txt`、`binary.sha256`、`cases.sha256`，并记录镜像 RepoDigest：redis `sha256:29e8589c…`、pd `sha256:5cf66a73…`、tikv `sha256:0524a207…`、rustfs `sha256:1803faef…`。所有运行的 cleanup proof 均为零（FUSE 挂载、测试进程、容器/卷/网络、IAM 用户）。

RustFS/S3 下 Redis 与 TiKV 均通过的阶段：实际 CLI overlay 挂载→变更→clean release；kill -9→租约过期→PVC 恢复→恢复源 repack/publish→remount 读到新 carrier；原始挂载 shutdown→drain→headless seal/repack→S1 发布→pin→carrier fork；native factory（变更→quiesce→capture→repack→原子 carrier/head 发布）；reader pin 阻止 history retirement→pin 释放→grace→旧 head 对象 DELETE；C2 sibling fork + 每 reader pin + child repack + alias retirement + carrier 元数据删除；reader pin 重开与崩溃 reader 过期；中止 journal 回收；SID 孤儿/GC。

### 未通过与未完成

- **pending-upload kill 恢复（Redis/TiKV × LocalFS/RustFS，4/4 失败）**：测试在对象 PUT 被拒绝期间等待 PVC dirty 记录，90 s 超时。测试配置为 `writeback_mode=upload_before_commit`，而 VFS 只在 `CommitBeforeUpload` 时创建 writeback 持久化缓存（`src/vfs/fs/mod.rs`），因此该配置下不会产生 dirty 记录。原始 `e980d59` 同样失败。属测试/产品契约不一致，未修改，**rejected**。
- **TiKV 间歇 EIO**：TiKV 下真实 FUSE 读刚写入的文件偶发 EIO（不同运行落在不同用例：CLI mount mutation、headless snapshot），直接执行读计划时 `payload_matches=true`。Redis 未复现。根因未定位，**rejected**。
- **初始 history 删除 + native GC**：history 已退役，但随后 `WorkspaceGc::run_at` 返回 `Busy`；`e980d59` 相同失败，未定位，**rejected**。
- **单元测试 5 个确定性失败**（e980d59 已存在，非本轮引入）：`g13a_kv_released_lease_is_not_an_expired_recovery_root`、`kv_v3_open_topology_rejects_binding_change_between_routing_and_final_read`、`migration_marker_fences_entity_and_hot_native_packets_without_writes`、`packed_mount_renewal_busy_exhausts_exactly_64`、`packed_stat_binding_authentication_busy_exhausts_exactly_64`。
- **paired acceptance、消融、overlay vs direct packed 对照**：未运行，**incomplete**。packed 发布仍只能经 operator crate 或被忽略的库测试触达，brewfs CLI 没有 packed seal/publish/GC 子命令，因此“同一 release binary + 同一 executor”的 paired 流程需要先补一个可执行入口。
- **本地 CI gate**：见下一节结果；未全部通过前不得提交。

### 主机变更（均记录在 `$R/build/toolchain.txt`）

安装 `protobuf-compiler 3.21.12`（etcd-client 构建依赖）；`/etc/fuse.conf` 增加 `user_allow_other`（备份 `.pre-brewfs-20261012`）；安装 rustfmt 组件；新增 32 GiB swapfile `/mnt/brewfs-data/swapfile`。库测试编译 rustc 峰值约 29 GiB RSS，曾两次导致主机硬重启；之后所有构建在 `systemd-run -p MemoryHigh=24G -p MemoryMax=28G -p MemorySwapMax=24G` 下运行。`/etc/sysctl.d/99-apsara-sysctl.conf` 会把 `vm.swappiness` 重置为 0，构建前需重新设为 10，否则 cgroup OOM。

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

## 2026-10-12 current e980 closeout

- 当前权威源码为 `e980d592801388c3c359d7e82b966df84f636e6f`，工作树保持干净；本轮只保留 packed-v3/005 目标。
- operator v3 云端 artifact：`/mnt/brewfs-data/artifacts/operator-v3-e980d592-20261012T104900Z/`。`cargo test --features workspace-operator` 57 passed，Clippy 以 `-D warnings -A clippy::double_must_use` 通过，CRD 生成和 diff 通过，资源清理通过；Kubernetes API server unavailable，因此 operator E2E/recovery 仍 blocked。
- 新加坡 ECS SSH 已恢复。`brewfs-singapore` 使用 Windows OpenSSH、公钥 `C:/Users/HXY/.ssh/brewfs-singapore`，远端已验证 `MaxSessions=10`、SFTP、pubkey-only、Git、POSIX shell、home 可写和 `/mnt/brewfs-data`。安全组保留 `188.253.126.208/28` 规则，并补充当前实际出口 `188.253.126.202/32`；临时 `0.0.0.0/0` 诊断规则已删除。修复后已复测公钥 SSH、SFTP 与远端 Claude Code 2.1.296。
- 本地 Claude Desktop 3P 配置已在 `%LOCALAPPDATA%\\Claude-3p\\configLibrary\\00000000-0000-4000-8000-000000157210.json` 加入 `sshHostAllowlist=["47.237.124.152"]` 和 `sshTransport="system-openssh"`，原文件保留 `.bak-20261012-193118`。本地 `~/.ssh/config` 已创建 `brewfs-singapore` 条目，host key 已写入 `known_hosts`。需要完全退出并重新打开 Claude Desktop 后，Code 环境选择器才会读取新配置。
- 远端 Claude Code 2.1.296 已安装到 `/home/hxy/.local/bin/claude`，并通过 `/usr/local/bin/claude` 提供 PATH 入口。ccswitch 仍是本地 gateway；当前 `inferenceGatewayBaseUrl=http://127.0.0.1:15721/claude-desktop`，按 Claude 文档远端主机不能访问设备 localhost，因此 SSH session 的推理网关仍是外部门禁，不能宣称远端 Claude session 已完成。
- ECS 包月仅做了阿里云 `ModifyInstanceChargeType` 的 DryRun：`PrePaid`、`Period=1`、`PeriodUnit=Month`、`IncludeDataDisks=true` 校验通过，当前实例仍未改为包月，也未产生费用。
- 当前 release 构建使用独立 `/mnt/brewfs-data/target-packed`、单 job、`RUSTFLAGS=-C debuginfo=0`；结果以 `build-packed-e980d592-*` artifact 为准。构建完成后才允许运行 RustFS/S3 严格 smoke；未完成前不更新性能 acceptance。

## 2026-10-14 overlay frozen-base 优化与 overlay-vs-native 对比

范围：overlay（packed 作为只读 lower）对比 native（纯 Redis 元数据 + RustFS 对象，无 packed）。同一 release binary、同一 2000×100 KiB 语料、同一 scanner trace（shuffle seed 20261001，16 workers，2 epochs），TTL0/direct-io/keep-cache0，零 payload cache，单机 loopback Redis + RustFS。

改动（默认 frozen，`with_strict_packed_lower_validation()` 保留旧的逐操作严格校验）：
- packed lower 视为冻结基座：stat/lookup/getxattr/get_acl/权限快照不再逐操作做 binding 认证、reader-pin 校验和前后两次 PWB 重读；权限快照改为一次原子 MGET。所有 mutation 仍在自身 CAS 中重新认证 binding/head/lease。
- 读请求和多步枚举（readdir 分页、listxattr、get_paths）仍读取最新 writable head，并用一次 MGET 校验 head/lease，因此同 epoch upper 写入和 lease 丢失仍 fail-closed；只有 binding 撤销检测是 strict-only。
- 只读 `record_open` 不再提交空的认证 CAS；写/append open 和 strict 模式保留。
- overlay upper 读准备 arena（4 MiB Plans）改为等待式 admission 并保留 512 KiB headroom，解析后收缩到实际大小；修复了提速后在并发下暴露的 13/2000 ENOMEM。

基线（旧 binary，MONITOR 诊断）：每个 FUSE lookup 约 1300 GET、790 STRLEN、620 TYPE、260 EVALSHA；overlay stat 1.5 files/s（100 文件），full 2.9 files/s（200 文件）。artifact：`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn/ovn-20261013T142413Z-856883`、`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-diag/ovn-20261013T153021Z-864206`。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-frozen3/ovn-20261014T044617Z-928478`，binary sha256 `cc0f1ac4…59b7`，3 reps 中位数，全部 0 错误，cleanup 归零；`summary-per-mode.json` = passed）：

| mode | arm | files/s | p50 ms | p95 ms | p99 ms | mount s | Redis cmds / file-epoch |
|---|---|---|---|---|---|---|---|
| full | native | 753.7 | 10.1 | 12.6 | 14.4 | 0.25 | 44.7 |
| full | overlay | 152.4 | 50.8 | 59.2 | 63.4 | 0.33 | 180.2 |
| stat | native | 3209.6 | 2.4 | 2.9 | 3.2 | 0.23 | 12.9 |
| stat | overlay | 672.4 | 11.4 | 13.5 | 15.5 | 0.33 | 28.3 |

结论：overlay stat 相比基线约 450 倍提升，full 约 50 倍；但仍为 native 的约 1/5（stat 4.8×、full 4.9× 差距），**未达到“接近 native”**。剩余热点：full 模式每 file-epoch 约 91 MGET、17 TIME、12 EVALSHA（读请求 head/lease fence、upper extent/inode 查询、packed lower 读路径）。下一步候选：按 head sequence 缓存 head/lease fence、合并 inode+extent+fence 为单次 MGET。原 `summary.json` 因跨 mode 比较 checksum（stat 模式 checksum=0）被标为 rejected，保留未改；runner 已改为按 mode 校验。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/frozen3/status.txt`）：release、fmt、clippy（workspace 与 `--all-targets --features workspace-overlay -D warnings`）、`cargo test --workspace --lib --bins`（1177 passed）、`git diff --check` 全部通过。overlay suite 959 passed / 4 failed，4 个均为既有失败：e980d59 的 3 个确定性失败 + `source_mutation_replacement_symlink_and_limits_fail_closed`（仅全量负载下失败，单独运行在 base 与 candidate 上均 5/5 通过）。`writable_acl_independent_sqlite_instances_keep_mode_acl_during_atime_writes` 在 merged-base 测试二进制上同样 9/10 失败，非本次引入。两个 `*_exhausts_exactly_64` 测试的期望已从 64 改为 store 现有的 `CAS_MAX_RETRIES`=256（base 上即已失败）。新增回归测试：frozen 读路径 0 次认证往返且 strict >0；frozen 写 open 仍提交认证 CAS；并发准备超过 Plans 容量时等待而非 ENOMEM。

## 2026-10-14 overlay 热路径与小文件读写对比（续）

新增 `tools/perf/smallfiles_rw.py` 和 runner 的 `rw` 模式：同一挂载内依次执行 write（2000 个 16 KiB 新文件，O_EXCL+fsync）、readback（逐字节校验读回，打乱顺序）、mixed（2000 次操作，30% 新建写、70% 读已写文件，种子固定），每步之间 drop_caches，各自独立 row 与 Redis commandstats。

本轮改动：
- frozen 模式下读请求与 chunk 准备使用一次一致性读（`read_fenced_inode`：ws/head/lease + 本 inode 两层行），取代 load_layer + inode MGET + fence MGET；请求与 fetcher 的 fence 改为按 inode（`validate_inode_read_fence`），其他文件的写不再使该读重启。strict 模式保持原整头 fence。
- frozen 模式 `mutation_version` 只取最新 head + 不可变 base；权限快照与 commit 的 Metadata 预留按实际上界而非固定 16/32 MiB。
- 只读 open 的 `record_close` 不再为已链接文件取 mutation version。
- open 的反向路径遍历：并发 16 MiB arena 由每挂载 8 个信号量限流（避免与 FUSE 请求争用 256 MiB Metadata 池而 ENOMEM）；被并发本地提交 fence 的遍历有界重试（64 次），修复 mixed 的 EBUSY。
- 卸载 drain：先回收已 release 且数据已提交/上传的 writer，再检查 256 文件上限；此前写入 >256 个小文件后 drain fail-closed，导致 lease 未释放、下一次挂载 `workspace writer was fenced`。
- 尝试过按 TTL 的上层行缓存，因测试中直接经 store 的写入读到旧行而回退，未保留。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot6/ovn-20261014T222454Z-1087430`，binary sha256 `30861a97…700b`，3 reps 中位数，30/30 行 0 错误，trace/checksum 按 mode 一致，cleanup 归零，status=passed）：

| mode | native files/s | overlay files/s | overlay/native | native p50/p95/p99 ms | overlay p50/p95/p99 ms | Redis cmds/op native→overlay |
|---|---|---|---|---|---|---|
| stat | 3264.5 | 696.8 | 0.21 | 2.4/2.9/3.1 | 11.0/13.0/14.9 | 12.9→28.3 |
| full | 768.2 | 153.2 | 0.20 | 10.0/12.6/14.1 | 50.7/58.9/62.9 | 44.7→164.3 |
| write | 652.0 | 215.4 | 0.33 | 23.4/29.7/47.3 | 74.2/81.1/86.4 | 33.2→271.3 |
| readback | 1272.4 | 485.4 | 0.38 | 11.9/16.4/20.6 | 32.3/40.5/46.2 | 33.0→153.9 |
| mixed | 1005.2 | 259.4 | 0.26 | 13.6/25.6/28.7 | 49.7/126.4/168.9 | 31.5→383.1 |

结论：overlay 小文件读写现在功能上可用（修复前 readback/mixed 在 16 并发下必失败，且写入后卸载 fail-closed），但仍为 native 的 0.2–0.4，**未接近 native**；stat/full 与上一轮持平，融合 fence 未带来可测吞吐提升。剩余主要成本：open 的 path-walk 权限检查（非 root uid，native 同样执行但每级仅 1 次 HGET）、每个 FUSE op 的权限快照 MGET、写路径每次 create/fsync 的多个 Lua CAS（packed writer authority/native reverse/extent 派生行）。下一步候选：为 frozen base 合并权限快照与 dentry/inode 读、减少 create/fsync 的派生 CAS、复用 VFS 级 attr 结果。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot6/status.txt`）：release、fmt、clippy（workspace 与 `--all-targets --features workspace-overlay -D warnings`）、`cargo test --workspace --lib --bins`（1177 passed）、`git diff --check` 通过；overlay suite 959 passed / 4 failed，均为既有失败（e980d59 的 3 个确定性失败 + `source_root_pins…` 全量负载下偶发）。新增测试：drain 在文件上限低于历史 writer 数时仍成功；frozen 读路径 0 次认证往返测试保持通过。

## 2026-10-15 overlay 热路径第 3 轮

改动：
- slice ID 按 64 个一块预留（一次 allocator CAS），inode 分配仍逐个（发布时作为下限使用）。
- frozen overlay 的 FUSE access 检查直接用一次权限快照（存在性+属性+ACL），不再先单独 stat；通过 `MetaLayer::permission_snapshot_answers_access()` 仅对 frozen overlay 打开，native 与 strict 模式顺序不变。
- 修复 PR #10 引入的回归：等待式 arena admission 无上限等待，导致 `fuse::cancel_tests::packed_prepare_admission_returns_enomem_and_next_read_recovers` 永久挂起（此前门禁未选中 fuse:: 测试，未被发现）。现在等待最多 2 s 后按原耗尽错误 fail-closed；lower 准备 arena 恢复原有无 headroom 等待（headroom 会饿死最小 Plans 容量下的同帧 follower）。门禁现已包含 `fuse::`。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot8/ovn-20261015T020405Z-1125281`，binary sha256 `9ab96317…a39e`，3 reps 中位数，36 行全部通过、0 错误、cleanup 归零）：

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay | Redis cmds/op 上一轮→本轮 |
|---|---|---|---|---|---|
| stat | 3006.9 | 678.4 | 0.23 | 696.8 | 28.3→25.1 |
| full | 763.9 | 157.1 | 0.21 | 153.2 | 164.3→154.1 |
| write | 644.9 | 209.6 | 0.32 | 215.4 | 271.3→253.4 |
| readback | 1195.1 | 500.7 | 0.42 | 485.4 | 153.9→146.9 |
| mixed | 962.1 | 254.3 | 0.26 | 259.4 | 383.1→371.6 |

结论：Redis 命令数下降 3–11%，但吞吐与上一轮持平（±4%，在噪声内）。说明 overlay 与 native 的差距目前主要不在 Redis 往返次数，而在本地 CPU/锁/调度路径（此前 perf 采样显示 overlay 在 7.4 s 窗口内消耗约 2.9 核，native 远低于此）。下一步应先做 CPU profile（dwarf 调用图）定位 meta_layer/packed 读路径的本地开销，再决定优化方向，而不是继续减少 Redis 往返。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot8/status.txt`）：release、fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1072 passed / 4 failed（均为既有失败）、`git diff --check` 全部通过。

## 2026-10-15 overlay 第 4 轮：CPU profile 与 packed lower 属性 memo

CPU profile（dwarf，stat 模式）：packed 索引页缓存命中率约 99.99%（inode_index 106372 hit / 4 miss），但每次 overlay stat 仍走完整索引树（预算 admission、async reader 锁、多次页缓存查找）。由于 packed 快照不可变，已解析的 inode 属性在 reader 生命期内不变。

改动：
- `PackedV3ReadonlyMeta::stat_fresh` 增加按 inode 的不可变属性 memo（上限 1M 条）；`metadata_bytes == 0` 时禁用（保持"每次都观测后端/注入故障"的语义，`fuse_init_tests::packed_hot_inode_failures_preserve_io_errno…` 依赖此语义）；transport drain 或预算关闭后不再命中并清空，保持 fail-closed。
- overlay 挂载的 `.stats` 现在包含 packed lower 的索引缓存、预算与 S3 读观测计数（此前 overlay 挂载未安装该扩展）。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot9/ovn-20261015T044840Z-1182738`，binary sha256 `c82ffcb8…b52a`，3 reps，30/30 通过，cleanup 归零）。同目录下 `ovn-20261015T043910Z-1168872` 与残留的 native perf 采样任务重叠（两臂 mount 时间同时升至 1.1–1.8 s），作为受污染运行保留，不用于结论。

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay |
|---|---|---|---|---|
| stat | 3148.1 | 723.0 | 0.23 | 678.4 |
| full | 755.8 | 163.2 | 0.22 | 157.1 |
| write | 623.2 | 204.7 | 0.33 | 209.6 |
| readback | 1187.1 | 497.1 | 0.42 | 500.7 |
| mixed | 971.2 | 251.1 | 0.26 | 254.3 |

结论：stat +7%、full +4%，rw 三项持平；overlay 仍为 native 的 0.22–0.42。profile 显示剩余 CPU 主要在 tokio 调度、Redis 管线编码/发送与 S3 SDK（签名/HTTP），而非单一可消除的热点；继续逼近 native 需要结构性改动（例如减少每个 FUSE op 跨越的 async 任务/锁层次，或在 VFS 层复用 attr 结果）。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot9/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1072 passed / 4 failed（均为既有失败）、`git diff --check` 全部通过。

## 2026-10-15 overlay 第 5 轮：上层行缓存（挂载级 opt-in）

改动：`WorkspaceMetaLayer::with_upper_row_cache(ttl)` 为只读路径缓存上层 inode/dentry/xattr 行和用于 access 判定的权限快照。
- 仅 CLI 挂载启用（持有独占 workspace lease，是上层唯一写者）；测试夹具默认关闭（此前被回退的版本正是因为夹具直接经 store 写入而读到旧行）。
- 仅 frozen 模式生效，strict 模式不使用。
- 每次本地提交前后、视图切换时整体失效；generation ticket 防止与提交并发的读插入旧行。
- 变更路径不经过缓存：mutation 仍读取新的权限快照并在自身 CAS 中校验。
- TTL 与 native Redis 客户端 inode 缓存相同（500 ms），可用 `BREWFS_WORKSPACE_ROW_CACHE_TTL_MS` 调整，0 关闭。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot10/ovn-20261015T065442Z-1209957`，brewfs sha256 `a032de33…381f`，3 reps，30/30 通过，cleanup 归零）：

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay | 变化 |
|---|---|---|---|---|---|
| stat | 2976.2 | 1009.0 | 0.34 | 723.0 | +40% |
| full | 753.4 | 202.0 | 0.27 | 163.2 | +24% |
| readback | 1136.2 | 621.3 | 0.55 | 497.1 | +25% |
| mixed | 919.2 | 261.8 | 0.28 | 251.1 | +4% |
| write | 611.6 | 205.0 | 0.34 | 204.7 | 0% |

overlay p50：stat 10.6→7.6 ms，full 47.6→38.3 ms，readback 31.2→24.6 ms。写路径不读缓存，未变化。记录的 fixture sha256 曾因拷贝竞态记录为空文件哈希，已用实际文件重写（`binary.sha256.note`）；运行日志显示 fixture 正常生成 2000 文件/111 组。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot10/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1073 passed / 5 failed（3 个 e980d59 确定性失败 + 2 个 source_file 负载下偶发，均为既有）、`git diff --check` 全部通过。

## 2026-10-15 overlay 第 6 轮：写路径（路径遍历 memo + 复用已提交 head）

改动：
- open 的反向路径遍历结果按 inode 存入挂载级 row cache（与其他上层行同一提交失效规则）；只缓存完整成功的遍历，配额/环/视图错误不缓存。
- 每次 packed 提交返回其写入的 head 记录；下一次 frozen mutation 直接以它作为期望版本，省去一次 head 读。该值仅为提示：若期间 head 被其他写入改变，提交 CAS 以 Busy 失败，既有重试会重新读取 head。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot11/ovn-20261015T083321Z-1237081`，brewfs sha256 `09445fec…d442`，3 reps，30/30 通过，cleanup 归零）：

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay | 变化 |
|---|---|---|---|---|---|
| stat | 3054.6 | 1002.8 | 0.33 | 1009.0 | 持平 |
| full | 760.4 | 200.7 | 0.26 | 202.0 | 持平 |
| readback | 1126.5 | 624.9 | 0.55 | 621.3 | 持平 |
| write | 611.6 | 225.1 | 0.37 | 205.0 | +10% |
| mixed | 938.5 | 273.6 | 0.29 | 261.8 | +5% |

overlay write p50 78.4→70.9 ms。剩余写路径成本：每次提交内的 native extent/reverse 派生检查与权限快照读取；读路径成本：每次 FUSE read 约 4.5 次 head/lease fence MGET。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot11/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1074 passed / 4 failed（均为既有）、`git diff --check` 全部通过。

## 2026-10-16 overlay 第 7 轮：结构性减少读写往返

设计与不变量：
- 读：frozen 模式下 `begin_request` 的融合捕获（head/lease + 本 inode 两层行，一次一致读）作为请求视图，经 task-local 传给同一请求内的 chunk 准备与执行；chunk 不再重复捕获，执行器的前/后 generation 检查不再各发一次 fence。请求结束时的 `ensure_current` 仍按 inode 行重新校验：lease 丢失或同 epoch 对本文件的上层写入（必然改写其 inode 行）都会让请求失败并由读者重试，任何字节返回前完成。task-local 之外（如 spawn 出去的工作）退回完整的逐 chunk fence。
- 写：packed 提交原先在视图读之后，还要分别点读 native-reverse 层状态、extent generation 与 slice 删除标记。这些键可由请求行预先算出，现在并入同一次一致视图读（上限 32 个 slice）。每个值仍是最终 CAS 的 check，陈旧值只会让 CAS 失败并进入有界重试，不会提交错误状态。
- inode 分配：挂载运行时 store 每次 allocator CAS 预留 64 个 inode ID（与 slice 相同）。packed 发布取 max(allocator, 最高 native inode+1) 作为下限，预留只会留空洞。admin/发布/测试 store 仍逐个分配。
- 未做：把整个写提交改成单个 Lua CAS（需在 Lua 中重写 PWB/binding/writer authority 的 fail-closed 校验，风险大）。

实测每操作 Redis 往返（单 worker MONITOR）：create 6.5→3.5（native 2.5），fsync 6.1→3.1（native 4.0），read 6.0→4.5（native 0.4），open 28→28（native 4）。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot12/ovn-20261015T104201Z-1262737`，brewfs sha256 `23740b71…e9d1`，3 reps，30/30 通过，cleanup 归零）：

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay | 变化 |
|---|---|---|---|---|---|
| stat | 3028.2 | 964.9 | 0.32 | 1002.8 | −4%（噪声内） |
| full | 774.3 | 204.4 | 0.26 | 200.7 | 持平 |
| readback | 1084.5 | 680.7 | 0.63 | 624.9 | +9% |
| write | 585.8 | 276.8 | 0.47 | 225.1 | +23% |
| mixed | 903.3 | 300.9 | 0.33 | 273.6 | +10% |

剩余：open 的反向路径遍历（约 9 次反向索引页扫描/次，写入后提交使 path memo 失效，单次 open 的文件难以命中）；读请求开头融合捕获之后仍有一次立即的 fence 检查（尝试跳过，但会违反"请求刚开始即检测到 lease/binding 撤销"的既有测试契约，已回退），以及 extent 范围扫描重复一次。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot12/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1076 passed / 3 failed（e980d59 既有确定性失败）、`git diff --check` 全部通过；提交内容与门禁/压测的 diff 完全一致。

## 2026-10-16 overlay 第 8 轮：open 路径遍历的命名空间缓存

问题：上一轮的 path memo 与通用行缓存共用，每次提交（大多是只写数据的 write/fsync）都会清空它，单次 open 的文件几乎不命中；每次 open 仍做约 9 次反向索引页扫描。

改动：
- 新增独立的命名空间缓存（与行缓存同 TTL、同样仅 CLI 挂载启用、仅 frozen 模式）。只有可能改变路径的提交才使其失效：写入任何 dentry 行或删除 inode（覆盖 create/mkdir/link/unlink/rename/rmdir）；只写数据的提交不再清空它；视图切换两者都清空。
- 路径遍历中每个祖先目录已验证的 (parent, name) 别名按 inode 缓存，同目录下多个文件的 open 复用父目录别名。
- 同样的 generation ticket、仅缓存完整成功结果；所有元数据层写入都经过 `commit_versioned_mutation`（已逐一核对），失效点不会遗漏。

实测 open 往返 28→18.7（反向页扫描 9→5），native 4。

结果（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot13/ovn-20261015T122039Z-1290274`，brewfs sha256 `63987d54…cbb4`，3 reps，30/30 通过，cleanup 归零）：

| mode | native files/s | overlay files/s | ratio | 上一轮 overlay | 变化 |
|---|---|---|---|---|---|
| stat | 3240.1 | 1036.5 | 0.32 | 964.9 | +7% |
| full | 740.1 | 225.0 | 0.30 | 204.4 | +10% |
| readback | 1178.6 | 774.9 | 0.66 | 680.7 | +14% |
| write | 613.7 | 268.1 | 0.44 | 276.8 | −3%（噪声） |
| mixed | 900.5 | 385.1 | 0.43 | 300.9 | +28% |

mixed p99 173→94 ms，readback p50 22.5→19.8 ms。

门禁（`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/build/hot13/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、`git diff --check` 通过；overlay+fuse 套件首轮因数据盘写满（ENOSPC）未能编译（日志 `test-overlay.enospc.log`），按用户选择删除 hot3–hot11、frozen3 的旧二进制（约 40 GB，保留 sha256/日志/diff，见各目录 `BINARIES_REMOVED.txt`）后重跑：1075 passed / 5 failed（均为既有失败）。提交内容与门禁/压测 diff 完全一致。

## 2026-10-16 overlay 第 9 轮：提交只失效自己写入的行缓存键

被拒候选（hot14，已回退）：上层 extent 的两层前缀扫描合并为一次 Lua（`scan_prefixes_bounded`）。单测通过，但 16 worker 压测各模式与第 8 轮相差 ±4% 以内（overlay write 268→267、readback 775→774、mixed 385→378），无收益，**rejected**。证据：`/mnt/brewfs-data/artifacts/overlay-packed-lifecycle-e980d59-20261012T153232Z/ovn-hot14/ovn-20261015T141744Z-1317718`，diff `build/hot14/hot14.diff`。

问题：单 worker MONITOR（`ovn-diag/ovn-20261015T142153Z-1328376`）显示，每次 write 前一次提交都会清空整个上层行缓存，因此每个文件 open/create 都要重新读根目录与父目录 inode、dentry、ACL xattr，约多 10 次 MGET。

改动：
- `commit_versioned_mutation` 前后只失效本次提交写入行对应的缓存键：每个 inode/extent/xattr 行所属 inode 的 `Inode` 与 `Permission`，每个 dentry 行的 `Dentry(parent, name)`，每个 xattr 行的 `Xattr(ino, name)`。每个缓存值只依赖自己键的行，键一一对应，不会漏。
- 含 ACL 行的提交没有逐键映射，仍清空整个行缓存；命名空间缓存规则不变；视图切换仍全部清空。
- 每次失效都推进 generation，失效前开始的读不会回填任何键；插入发现竞争时只删除自己插入的键。

同机、同一时段背靠背对照（先第 8 轮 hot13 二进制，再本轮），3 reps，两组均 30/30 通过、cleanup 归零：
- hot13：`ovn-hot13-paired/ovn-20261015T160651Z-1350478`
- hot15：`ovn-hot15-paired/ovn-20261015T161137Z-1361770`，brewfs sha256 `d3d0f040…f247`

| mode | native files/s | overlay hot13 | overlay hot15 | 变化 | ratio |
|---|---|---|---|---|---|
| stat | 2483 | 861 | 885 | +3% | 0.36 |
| full | 665 | 189 | 197 | +4% | 0.30 |
| write | 540 | 238 | 253 | +6.5% | 0.47 |
| readback | 1010 | 684 | 681 | 持平 | 0.67 |
| mixed | 809 | 332 | 365 | +10% | 0.45 |

mixed p99 112.5→77.2 ms，write p99 82.0→79.5 ms。客户端 Redis 往返/操作：write 29.7→18.1，mixed 70.1→48.9，readback 不变（29.6）。此时段主机整体比第 8 轮记录时慢约 15%（native full 740→645），因此本表只与同时段的 hot13 对照比较；单独首跑 `ovn-hot15/ovn-20261015T160044Z-1338897` 紧跟构建、页缓存被挤出，native 同样下降，不作为结论。

剩余：readback 每次读仍约 14 MGET + 8 次 TIME/MGET fence（`hot_mutation`/读 fence）；写提交约 7 次 EVALSHA（视图读、native-reverse 状态与行的 4 次前缀扫描、最终 CAS）。

## 2026-10-16 overlay 第 10 轮：路径遍历信任后端证明的短页

问题：readback 每个文件在 open 的路径权限检查中，对目标与每个祖先、每层各做一次反向索引页扫描；即使第一页只有 1 行（远少于 32 行上限），也还要再扫一个空页确认结束（单 worker trace：readback 阶段 780 次反向扫描中 260 次是这种空确认页）。

改动：
- `WorkspaceKvBackend::short_scan_page_is_end()`（默认 false）。只有 Redis 返回 true：其分页是单个 Lua 内的 `ZRANGEBYLEX … LIMIT max_records`，唯一的截断条件是行数上限，字节超限与索引幽灵都直接报错而非截断。TiKV 的分页可能因 RPC/region 上限返回短页，保持原语义。
- `WorkspaceNamePage.end` 由 `bounded_name_page` 设置（短页且后端可证明）；仅 native-reverse 路径遍历使用它提前结束每层扫描。完整性仍由遍历结束时的 `confirm_native_reverse_authority` 精确 CAS 认证。
- 测试：内存后端可选开启该语义，35 个别名（32 + 3 行）在两种语义下结果相同，反向扫描页数 4→3；真实 Redis 测试验证满页后续仍有行、短页之后确为空。

同机两组对照（hot15→hot16 与 hot16→hot15 交替顺序），每组 3 reps，4 组均 30/30 通过、cleanup 归零；表中为两组合并 6 次中位数：
- `ovn-hot15-paired2/ovn-20261015T182305Z-1389942`、`ovn-hot16-paired2/ovn-20261015T182720Z-1400656`
- `ovn-hot16-paired3/ovn-20261015T183234Z-1411462`、`ovn-hot15-paired3/ovn-20261015T183644Z-1422070`

| mode | native files/s | overlay hot15 | overlay hot16 | 变化 | ratio |
|---|---|---|---|---|---|
| stat | ~2990 | 981 | 983 | 持平 | 0.33 |
| full | ~760 | 220 | 219 | 持平 | 0.29 |
| write | ~570 | 287 | 278 | −3%（两组方向相反：284→275、291→291；改动不触及写路径） | 0.47 |
| readback | ~1080 | 728 | 771 | +5.9% | 0.71 |
| mixed | ~880 | 431 | 451 | +4.5% | 0.50 |

mixed p99 66.1→58.1 ms，readback p99 35.0→33.5 ms；客户端 Redis 往返/操作 readback 29.5→28.4、mixed 48.0→44.0。

操作记录：首次运行真实 Redis 测试时用 `--ignored real_redis_` 过滤，误选中会挂载 FUSE 的 ignored 测试并卡在 `request_wait_answer`；通过 `/sys/fs/fuse/connections/<minor>/abort` 中止连接、lazy umount 后进程正常退出，未留下 D 状态任务。之后只用 `--exact` 运行两个 Redis 后端测试（`build/hot16/test-redis-real.log`，2/2 通过）。

## 2026-10-16 overlay 第 11 轮（rejected）：跳过融合捕获后的立即 fence

候选（hot17，已回退，diff `build/hot17/hot17.diff`）：`UnifiedReadRequestFence::validated_at_capture()`。frozen 模式下 `begin_request` 的融合捕获本身就是一次一致读并校验了同一 head/lease + inode 行 fence，VFS 两处读路径据此跳过捕获后紧接着的 `ensure_current`，最终 fence 保留。新增测试（VFS 跳过仅限立即检查、捕获后丢失 lease 仍在最终检查失败；KV 融合 fixture 报告 validated、SQLite fixture 不报告）均通过；焦点套件只有 3 个既有失败。

两组交替顺序对照（hot16→hot17、hot17→hot16），4 组均 30/30 通过：
- `ovn-hot16-r11pa/ovn-20261015T202230Z-1448134`、`ovn-hot17-r11pa/ovn-20261015T202646Z-1458955`
- `ovn-hot17-r11pb/ovn-20261015T203104Z-1469697`、`ovn-hot16-r11pb/ovn-20261015T203514Z-1480353`

| mode | overlay hot16 | overlay hot17 | 变化 |
|---|---|---|---|
| stat | 985 | 990 | +0.5% |
| full | 221 | 213 | −3.6% |
| write | 280 | 276 | −1.3% |
| readback | 767 | 761 | −0.8% |
| mixed | 449 | 444 | −1.3% |

Redis 往返/操作 readback 28.4→24.4、mixed 43.7→40.9，但吞吐全部在噪声内，**rejected**。

结论：readback 已不再受 Redis 往返数限制。单 worker 诊断（`ovn-diag-hot16`）中 overlay 每次 FUSE read 平均 1.89 ms，native 0.95 ms；S3 GET 次数与总延迟相近（337 次，616 ms vs 540 ms），lookup 相近（256 vs 230 µs）。多出的约 0.95 ms/read 在本地读路径（请求 fence/预算准入/计划构造/task-local 视图），下一步应先做 overlay 读路径的 CPU/await 剖析，而不是继续削减往返。

## 2026-10-16 overlay 第 12 轮（rejected）：只读 open 的权限检查移出全局 mutation_gate

剖析（hot16 二进制，16 worker，rw 模式，`ovn-prof-hot16`、`ovn-cpu-hot16`）：
- perf（DWARF，499 Hz）：overlay 进程平均约 1.9 核（8 核主机），约 54% 样本在 Redis 客户端调用路径、约 11% 在多路复用连接驱动任务，S3 SDK 约 7%；路径遍历（`packed_paths_bytes_owned`）约 8%，权限快照约 5%，均以 malloc/free/realloc 为主，没有单一热点。
- 每秒 CPU 采样：rw 阶段主机忙碌 55–79%，其中 rustfs 1–3 核、brewfs 2–3.5 核、redis-server 0.2–0.6 核；Redis 服务端 overlay 时约 26–41% 忙碌（native 5–14%），每次 readback 约 370 µs 服务端时间（native 44 µs），主要是 Lua 脚本。
- 单 worker 时间线：一次 readback 约 9 ms，其中路径遍历（反向索引权威读→最终确认 CAS）约 1.9 ms（21%）；剩余为 4 次融合 fence、权限快照 MGET、extent 扫描以及约 2 ms 的 S3 GET。

候选（hot18，已回退，diff `build/hot18/hot18.diff`）：`record_open` 原先在全局 `mutation_gate` 下做权限快照读取，所有 open 串行。只读、不提交的 open 改为在门外完成授权，进门后只发布 open 计数；新增 `deletions` 计数器（每次删除 inode 的提交尝试之后递增），若授权期间有删除提交则回退到原有完整门内路径重新读取。新增竞态测试（final unlink 先排队、open 在门外授权后排在其后，必须返回 NotFound 且不计数）通过；把删除检查禁用后该测试失败（`build/hot18/test-mutation.log`），证明其有效。原有 open/close 竞态测试与焦点套件（仅 3 个既有失败）均通过。

两组交替顺序对照，4 组均 30/30 通过：
- `ovn-hot16-r12pa/ovn-20261015T225856Z-1511886`、`ovn-hot18-r12pa/ovn-20261015T230310Z-1522585`
- `ovn-hot18-r12pb/ovn-20261015T230717Z-1533113`、`ovn-hot16-r12pb/ovn-20261015T231124Z-1543744`

| mode | overlay hot16 | overlay hot18 | 变化 |
|---|---|---|---|
| stat | 1020 | 1030 | +1.0% |
| full | 218 | 221 | +1.1% |
| write | 285 | 285 | 持平 |
| readback | 777 | 772 | −0.6% |
| mixed | 452 | 451 | 持平 |

全部在噪声内，**rejected**。open 的门内串行不是瓶颈。

阶段结论（第 11、12 轮）：在当前主机上，继续削减单个往返或解除局部串行都不再改变吞吐。overlay 每次操作的 Redis 服务端时间是 native 的 4–8 倍（Lua 脚本，尤其写提交约 740 µs/op），客户端 CPU 分散在 Redis 编解码与分配上，且 rustfs 与 brewfs 共享 8 核。下一步应是结构性改动而非继续微调：把写提交的视图读 + native-reverse 检查 + 最终 CAS 合并为单个 Lua（需在 Lua 中重写 fail-closed 校验），或给 readback 的 open 路径权限检查提供与 native 相同的 inode 级权限缓存语义。

## 2026-10-16 已知问题：32 worker 下 mixed 出现 ENOMEM/EIO（既有，非近期回归）

并发扩展测试（仅 rw 模式、1 rep，诊断用，不作为验收数字）：

| workers | 二进制 | overlay write | overlay readback | overlay mixed |
|---|---|---|---|---|
| 8 | hot16 | 296（0 错误） | 690（0 错误） | 424（0 错误） |
| 16 | hot16 | 285（0 错误） | 777（0 错误） | 452（0 错误） |
| 32 | hot16 | 291（0 错误） | 864（0 错误） | **525/2000 错误** |
| 32 | hot13（第 8 轮） | 270（0 错误） | 885（0 错误） | **1028/2000 错误** |

证据：`ovn-scale-w8`、`ovn-scale-w32/ovn-20261015T232611Z-1557684`、`ovn-scale-w32-hot13`。native 在 32 worker 下三种模式均 0 错误（write 733、readback 1236、mixed 1051）。

现象：应用收到 `ENOMEM`（open/read）与 `EIO`（fsync）。日志：`try_commit failed … packed v3 object limit exceeded: mount metadata_owned budget temporarily exhausted: used≈266.9 MB, requested=2104238, limit=268435456`，随后 `commit_chunk dropping failed slice after retry budget exhausted` 与 `vfs.fsync_snapshot_failed`。fsync 向应用报告了失败，因此不是静默丢数据，但该次写入的数据未提交。

原因（已核对代码，未改动）：mount 的 Metadata 池为 256 MiB。路径遍历每个持有 16 MiB arena，最多 8 个并发（`MAX_CONCURRENT_PATH_WALKS`，共 128 MiB）；每个 frozen 写提交持有 `FROZEN_SNAPSHOT_BYTES`（2 MiB）的 mutation version，提交时再用**不等待**的 `admit` 申请约 2.1 MiB 的行/packet 上限（`validate_mutation_allocation_bounds`）。32 个并发 worker 时剩余 128 MiB 不足，提交的 admission 立即失败。由于提交方已持有同池的 mutation-version permit，直接改成等待式 admission 可能互相等待，只能靠 2 s 超时退出；修复需要重新设计这一池的配额（例如为提交单独预留、或缩小路径遍历 arena 并按需增长），尚未实施。

写路径测量（单 worker MONITOR，`ovn-diag-hot16`）：overlay 每次 write 约 14.5 ms，native 约 10 ms，两者都含约 6 ms 的 S3 上传/fsync 等待。overlay 多出的约 4.5 ms 分布在每个文件约 3 次提交上（create、fsync、close），每次为视图读 EVALSHA + 最终 CAS EVALSHA 及其间隔，约 1.2–1.5 ms。把视图读并入 CAS 的单 Lua 方案每次提交最多省约 0.25 ms，估计 write 最多改善约 5%，风险（在 Lua 中重写 fail-closed 校验）与收益不成比例，暂不实施。

## 2026-10-16 overlay 第 13 轮：修复 32 worker 下 Metadata 池耗尽（ENOMEM/EIO）

根因（`brewfs::packed_v3_budget=debug` 复现，`ovn-w32-budgetlog`）：路径遍历 16 MiB arena 用**不等待**的 `admit` 直接占用 Metadata 池（最多 8 个，128 MiB），与每个 FUSE 请求 2 MiB、frozen mutation version 2 MiB、提交上限约 2.1 MiB 等小额非等待申请抢同一个 256 MiB 池；32 worker 时小额申请被拒（293 次约 2 MiB 拒绝、219 次 16 MiB 拒绝）。只调大池（512 MiB，env，同一二进制）即 0 错误，峰值 339 MiB，确认是池内竞争而非泄漏。

改动：路径遍历 arena 改为 `admit_waiting_with_headroom(Metadata, 16 MiB, 64 MiB)`：只有在申请后仍留 64 MiB 空闲时才拿 arena，否则有界等待（`ADMISSION_WAIT` 2 s）。等待中的遍历只持有调用方的 2 MiB 请求额度，arena 随遍历结束释放，不会自锁。小额非等待申请不再被饿死。新增单测 `headroom_arena_waits_instead_of_taking_the_small_charge_reserve`。

32 worker（3 reps）：
- 修复前（`base-9e4f204`，从干净 9e4f204 重建）：r1 mixed 560/2000 错误，随后卸载报 `packed cutoff/drain is unverified`，r2/r3 的 overlay 挂载因 `workspace writer was fenced` 无法启动，整组 rejected（`ovn-base-w32/ovn-20261016T014552Z-1581863`）。
- 修复后（hot19）：9/9 overlay 步骤 0 错误，组 passed（`ovn-hot19-w32/ovn-20261016T005343Z-1574164`）。overlay/native：write 276/725，readback 849/1152，mixed 431/942。

16 worker 交替顺序对照（base→hot19、hot19→base，4 组均 30/30 通过，6 次合并中位数）：stat 974→993（+2.0%）、full 217→220（+1.7%）、write 278→289（+4.0%）、readback 744→766（+3.0%）、mixed 437→454（+3.9%），无回归。证据：`ovn-base-9e4f204-r13p{a,b}`、`ovn-hot19-r13p{a,b}`。

门禁（`build/hot19/gate.txt`）：fmt、clippy（workspace 与严格 overlay）、`cargo test --workspace --lib --bins`（1177 passed）、overlay+fuse 套件 1079 passed / 5 failed（3 个 e980d59 确定性失败 + 2 个 source_file 负载下偶发，均为既有）、`git diff --check` 通过。

## 2026-10-17 overlay 第 14–16 轮：不可变 lower 的权限/查找数据按绑定生命周期永久缓存；create 只失效新 inode 的路径缓存

用户选择方案 (a)：lower 的权限检查保持正确，但其输入数据（不可变、已认证的快照）在 frozen 模式下对整个绑定生命周期缓存，无过期、无再校验；strict 模式不缓存。用户也批准了 upper 层的 inode 级权限缓存。

改动：
- `WorkspacePackedLower`（frozen 专用、生命周期等于该绑定）：
  - `inode_hot` / `inode_cold`：lower inode 的 hot 属性与 cold 权限输入（symlink target、三个权限 xattr），以原调用条件按需读取后缓存；
  - `reverse_aliases`：lower 反向名称（完整分页结果）缓存；
  - 三个 moka 缓存按字节加权，各 64 MiB 上限，淘汰只会导致重读不可变快照。
- `PackedV3ReadonlyMeta::lookup_with_attr_bytes`：(parent, name) → (inode, attr) 结果（含不存在）缓存，与已有 `attrs` 缓存同生命周期、同启用条件（`metadata_bytes != 0`）、同关闭/drain 失效；64 MiB 字节上限。去掉了每次 lookup 的 GroupMeta range GET 与解码。
- upper：命名空间缓存原先在任何写 dentry 的提交后整体清空。新增 `RowsWritten::of_create`：仅 create 走 `commit_created_mutation`，且只有当提交恰好是一条 Put dentry、指向本提交新建（Present、parent_hint 等于该 dentry 的父目录）的 inode、且无删除时，才只失效该新 inode 自己的 Paths/Aliases；其余一律全清（误用只影响性能）。新建 dentry 无法改变任何已有 inode 的路径或别名。
- upper 的 inode 级权限缓存（`RowKey::Permission`，按提交逐键失效，第 9 轮）此前已存在，本轮未改变其语义。

中途问题：hot20 二进制是在 hot/cold 拆分修复之前的旧代码上构建的（单一 `permission_inputs` 对 root 也请求 hot 属性，触发 `packed root attributes missing`，真实挂载上 root readdir/stat 返回 EIO）；已标记 `build/hot20/STALE.txt`，其压测结果作废。之后每个候选先跑真实挂载探针（`/tmp/ovn_probe.sh`：root listdir、stat、子目录 listdir）再压测。

验证：
- 单测：`frozen_lower_permission_and_lookup_inputs_are_fetched_once_per_binding`（清空 upper 缓存后重复权限/lookup/路径查询，lower range GET 不再增加；upper chmod 仍覆盖缓存的 lower 输入）、`strict_lower_permission_inputs_are_never_memoized`、`create_keeps_existing_paths_cached_but_other_namespace_commits_clear_them`（create 保留已有路径缓存；通用提交、指向已有 inode 的 link、带删除的提交都全清）。焦点套件仅 3 个既有失败。
- 32 worker（`ovn-hot22-w32/ovn-20261016T182154Z-1765004`）：stat/full/rw 15/15 overlay 行全部 0 错误，组 passed。overlay files/s：stat ~2114、full ~351、write ~290、readback ~883、mixed ~525。
- 16 worker 交替顺序对照（hot19→hot22、hot22→hot19，4 组均 30/30 通过，6 次合并中位数）：

| mode | native | overlay hot19 | overlay hot22 | 变化 | ratio |
|---|---|---|---|---|---|
| stat | 3069 | 1024 | 2309 | +125% | 0.33 → 0.75 |
| full | 790 | 227 | 347 | +53% | 0.29 → 0.44 |
| write | 633 | 293 | 313 | +7% | 0.50 → 0.49 |
| readback | 1202 | 773 | 807 | +4% | 0.67 → 0.67 |
| mixed | 993 | 455 | 553 | +21% | 0.48 → 0.56 |

stat p99 10.7→5.0 ms，full p99 45.6→36.2 ms，mixed p99 57.4→49.5 ms；mixed 每操作 Redis 往返 43.9→30.3。证据：`ovn-hot19-r16p{a,b}`、`ovn-hot22-r16p{a,b}`（另有一轮未加字节上限的 hot21 对照 `ovn-hot{19,21}-r15p{a,b}`，结论一致）。

## 2026-10-17 overlay 第 17 轮（诊断，未改代码）：upper 行缓存 TTL

单 worker MONITOR（hot22，`ovn-diag-hot22`）：readback 每文件仍有 8 次融合 fence（每个文件两次 FUSE read：数据 + EOF，每次 begin + 立即 ensure + 最终 ensure + chunk 视图），一次完整路径遍历（新建文件首次 open，反向索引权威读 → 确认 CAS），以及 3.3 次 10-key 权限快照读。mixed 中对已遍历文件的再次访问在默认 500 ms TTL 下已过期，因此重新遍历。

实验（同一 hot22 二进制，只改 `BREWFS_WORKSPACE_ROW_CACHE_TTL_MS`，两组交替顺序，每组 3 reps，4 组均通过）：

| mode | overlay TTL 500 ms | overlay TTL 60 s | 变化 | Redis 往返/操作 |
|---|---|---|---|---|
| write | 295 | 299 | +1% | 18.1 → 18.0 |
| readback | 790 | 792 | 持平 | 28.4 → 28.1 |
| mixed | 554 | 638 | +15% | 30.7 → 15.7 |

证据：`ovn-hot22-ttl-{def,t60}-{pa,pb}`。native 同期 write ~640、readback ~1175、mixed ~988；60 s 时 mixed 比值 0.56 → 0.64。

未改默认值的原因：TTL 是本挂载之外写入（以及失去租约后）的陈旧上限。本挂载自己的提交按键立即失效，视图切换全部失效；但 frozen 模式的 stat/lookup/权限读本身不做远程 fence，若租约丢失（心跳只记录 `packed-v3 joint heartbeat fenced` 后退出），挂载可在 TTL 内继续返回缓存的 upper 元数据。默认 500 ms 与 native Redis 客户端一致。安全地延长需要：在心跳失败/租约丢失时清空 upper 与命名空间缓存并拒绝后续元数据读，再把默认 TTL 提高；该改动待用户确认。

## 2026-10-17 overlay 第 18 轮：upper 行缓存与租约绑定，默认 TTL 500 ms → 60 s

第 17 轮记录了：TTL 延长到 60 s 可让 mixed +15%，但 TTL 当时是租约丢失后唯一的陈旧上限。本轮先补上这一点，再延长默认值。

改动：
- 新增 `lifecycle::LeaseValidity`：首次获取租约（发出请求前的时刻 + TTL）建立本地"确知持有租约"的截止时间；每次续约成功按"该次续约发出前的时刻 + TTL"推进（只增不减）；续约失败永久撤销。原生心跳（`LeaseHeartbeat`）与 packed 联合心跳（`PackedMountRuntime`）都更新它；`WorkspaceMountSession::lease_validity()` 暴露。
- `WorkspaceMetaLayer::with_lease_bound_row_cache`：upper 行缓存与命名空间缓存只有在 `authoritative()` 为真时才提供；一旦截止时间过去或被撤销，立即清空两个缓存并之后一直绕过（读走存储）。租约是排他的，且每个 upper 变更都在 CAS 中重新校验租约，因此截止时间之前不可能有其他持有者修改 upper 行。
- CLI 挂载：启用租约绑定，默认 TTL 改为 60 s（`LEASE_BOUND_ROW_CACHE_TTL`，仅作工作集上限，不再是陈旧上限）；`BREWFS_WORKSPACE_ROW_CACHE_TTL_MS` 仍可覆盖。

测试：`deadline_extends_on_renewal_and_revocation_is_permanent`（单调推进、过期、撤销永久）、`lease_bound_row_cache_stops_serving_once_the_lease_cannot_be_proven`（撤销与过期两种情况下，缓存不再返回旧行、读到存储的新值、缓存被清空）、`session_lease_validity_follows_real_renewals_and_lease_loss`（真实会话：300 ms TTL、50 ms 心跳，600 ms 后仍有效；在底下释放租约后下一次续约失败即撤销）。

结果：
- 真实挂载探针通过（`ovn-hot23-probe`）。
- 32 worker（`ovn-hot23-w32/ovn-20261016T210130Z-1864759`）：15/15 overlay 行 0 错误；overlay stat ~2834、full ~404、write ~305、readback ~951、mixed ~648。
- 16 worker 交替顺序对照（hot22→hot23、hot23→hot22，4 组均 30/30 通过，6 次合并中位数）：

| mode | native | overlay hot22 | overlay hot23 | 变化 | ratio |
|---|---|---|---|---|---|
| stat | 3384 | 2310 | 2541 | +10% | 0.68 → 0.75 |
| full | 824 | 365 | 381 | +4.5% | 0.44 → 0.46 |
| write | 678 | 301 | 324 | +7.8% | 0.47 → 0.48 |
| readback | 1287 | 813 | 892 | +9.7% | 0.67 → 0.69 |
| mixed | 1063 | 569 | 691 | +21% | 0.57 → 0.65 |

mixed 每操作 Redis 往返 30.4 → 15.7，mixed p99 48.5 → 45.6 ms。证据：`ovn-hot22-r18p{a,b}`、`ovn-hot23-r18p{a,b}`。本时段主机 native 偏快（stat 3384、readback 1287），故 hot22 的比值低于第 16 轮记录。

## 2026-10-17 overlay 第 19 轮：租约可证明时，frozen 读 fence 在本地完成

问题：小文件 readback 每个文件两次 FUSE read（数据 + EOF 探测），每次在 begin 与结束时各做一次远程融合 fence（head/lease + 本 inode 两层行），约 8 次/文件。

依据：第 18 轮的 `LeaseValidity` 证明本挂载仍持有排他租约时，不可能有其他写者修改 upper 行，且本挂载每次提交在提交前后都会失效其写入 inode 的缓存行。因此，若该 inode 的缓存行仍等于读请求所用的行，存储中的行也必然未变。

改动：
- `WorkspaceMetaLayer::lease_proven_inode_rows`：frozen + 融合 fence 可用 + 租约绑定 + 租约可证明时，用缓存的 head/base 对与该 inode 的缓存行（未命中则读一次并入缓存）作为请求视图，跳过捕获时的远程融合读；加载后再次确认租约仍可证明。
- `row_cache::LeaseLocalFence`：`ensure_current` 及 chunk 的 `ensure_generation` 先做本地检查（缓存行与请求所用行相等且租约仍可证明）；任何不一致（缓存被提交失效、租约过期/撤销、未命中）都退回原有的远程 per-inode fence，远程 fence 仍对任何变化 fail closed（Busy → ReadViewChanged 重试，失去租约 → Fenced）。
- strict 模式、无租约绑定、或存储不支持融合 fence 时行为与之前完全相同。

测试：`lease_proven_read_fence_is_local_and_falls_back_to_remote_on_doubt`（KV 融合 fixture + 租约绑定：预热后 begin + 两次 ensure_current 不产生任何 timed read；本 inode 的 chmod 使缓存失效，原请求退回远程 fence 并返回 ReadViewChanged；撤销租约证明后 fence 重新走远程，真实释放租约后请求以 Fenced 失败）。

结果：
- 真实挂载探针通过（`ovn-hot24-probe`）。
- 32 worker（`ovn-hot24-w32/ovn-20261016T225059Z-1929054`）：15/15 overlay 行 0 错误；readback ~1025、mixed ~672。
- 16 worker 交替顺序对照（hot23→hot24、hot24→hot23，4 组均 30/30 通过，6 次合并中位数）：

| mode | native | overlay hot23 | overlay hot24 | 变化 | ratio |
|---|---|---|---|---|---|
| stat | 3367 | 2422 | 2413 | 持平（两组方向相反） | 0.72 |
| full | 841 | 394 | 410 | +4% | 0.47 → 0.49 |
| write | 665 | 303 | 304 | 持平 | 0.46 |
| readback | 1187 | 826 | 952 | +15% | 0.66 → 0.80 |
| mixed | 1022 | 663 | 687 | +4% | 0.63 → 0.67 |

Redis 往返/操作：readback 28.1 → 16.1，mixed 15.7 → 7.3；readback p99 30.9 → 27.0 ms。证据：`ovn-hot23-r19p{a,b}`、`ovn-hot24-r19p{a,b}`。
