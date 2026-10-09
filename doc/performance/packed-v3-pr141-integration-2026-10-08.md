# PR #141 与 packed-v3 整合报告（草稿）
## 合并后定点修复（2026-10-09）

## 2026-10-09 并发收尾审计

审计发现旧的 `topology_gate: Mutex<()>` 和 `TopologyTxn` 自动加入 `CONTROL` 检查会把不同 workspace 的 Redis/TiKV 元数据操作串行化。当前修复移除了进程级全局 gate；普通 entity-key packet 只读取并校验不可变 catalog header，不把 `CONTROL` 放进 CAS 锁集合。只有实际修改 header 的 packet、迁移、packed permission 和 native reverse 维护路径保留显式 `CONTROL` authority。相关子模块的 phase/seed/recovery/publication 路径依赖有界 CAS 重试，不再引用全局 gate。

Ordinary workspace forks and mounted renewals filter read-only header checks before the final CAS; preparation still validates the header. The five PR141 entity-CAS regressions pass, including the 64-way fork race.

小规模验证：`cargo check --locked --workspace --all-targets`、`cargo check --locked --features workspace-overlay --tests`、`cargo fmt --all --check`、`git diff --check` 均通过；Redis/TiKV 真实服务验收仍受 diagnostic03 的 RustFS EIO 阻塞。Redis `ensure_key_index` 的 SCAN/ZADD/marker 初始化仍需要并发启动验收，当前未把它标记为正式通过。


PR #141 已在 `upstream/main`，当前工作树保留其三方整合结果和本地 packed-v3 改动；未执行会覆盖脏工作树的强制 merge。源码审查没有发现冲突标记。针对 Redis/TiKV 共享元数据路径补上了 v3 open 的有界读取：单 key routing、三 key routing、最终 authority packet 和 open-record 全部使用 `get_many_consistent_with_time_bounded`，固定 `max_records/data_requests=32`、key 1 KiB、value 12 KiB、总响应 256 KiB、transport response 128 KiB，在后端物化前拒绝超限值。当前 `kv_store.rs` SHA256 为 `089a94ca22a8a5ae40672de94bcbeef03c1ff414890b33e4d0f4f283c8524ce0`，`tikv.rs` SHA256 仍为 `c4bfe3adf516f89af54131adbdf9af0854f48b40e032bfc690579a6954133f65`。

旧 clustered packed-metadata v2 已从 workspace-overlay 的公开编译面移除，`packed_v2_snapshot_fixture.rs` 已删除；Cargo 只声明 `packed_v3_snapshot_fixture`。`native-packed-base/workspace-native-v2` 属于独立原生卷格式，本轮保留，避免把原生对象布局与 packed-v3 元数据版本混为一谈。`clustered_snapshot/` 的历史源文件仍留在脏工作树中但不再被模块引用或编译。

本次修复后的 `cargo check --locked --workspace --all-targets`、`cargo check --locked --workspace --all-features --all-targets` 与 `cargo check --locked --features workspace-overlay --tests` 均通过（均使用单 job、`CARGO_INCREMENTAL=0`、`CARGO_PROFILE_DEV_DEBUG=0`）；`cargo fmt --all --check` 和 `git diff --check` 也通过。重复的完整 lib-test 链接因约 14 GiB RSS 在无编译错误的情况下主动中止，已有的 gate09 55/55 与 store 502 passed 证据仍保留；未把中止误记为测试通过。

Candidate04 外置报告绑定 active production `src/workspace_overlay/stores/tikv.rs` SHA256 `c4bfe3adf516f89af54131adbdf9af0854f48b40e032bfc690579a6954133f65`；上面的合并后定点修复是在 Candidate04 冻结之后完成的。

PR #141 的 entity-key CAS 逻辑已按当前 packed-v3 工作树手工整合；由于工作树包含大量未提交 packed-v3 改动，不能把它描述为 clean merge，尚未提交；分支 `codex/packed-metadata-aliyun-20260930` 的 HEAD 仍为 `a429b0e1bc1c158af06ecf738e552062123d6e00`。Redis/TiKV entity catalog、原子 lease/global-ID routes 与 active pointers、迁移 fence、完整 lost-reply successor confirmation 已整合；NativeHold、reverse completeness、永久 slice deletion fence 和原 deadline 仍是必要条件。Initial finish 使用独立 64-key tier，普通 source 仍限 32 keys。支持范围为 packed-v3。 Active tikv.rs 当前包含 production sorted-union lock-order delta 与 tests-only recording hook。

已有三处正常续期竞争修改通过确定性回归：`record_open` preparation 遇 EBUSY 整体重启；`load_packed_binding_record` 仅在 final bounded authentication 明确 false 时重读完整 authority；mounted `renew_owned` 在 preparation Busy 或明确 CAS false 后重建、重认证 scoped view。均保留原 64 次上限；已提交未知错误仍只作 exact-successor confirmation，未确认则返回，不重放 mutation；grant/release 保留一轮 false→Fenced。现有 budget 和 read/authentication limits 保留。

| 已完成的有限回归 | 实际 RED → GREEN | 原始证据 |
| --- | --- | --- |
| 六项 preparation/binding 回归 | RED02：2 pass / 4 fail；GREEN02：6 pass / 0 fail | [facts](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-closeout-facts01/FACTS.md) |
| 十一项合并回归 | mounted-renew-red01：8 pass / 3 fail；green01：11 pass / 0 fail | [RED receipt](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/kv-sdk-development/pr141-mounted-renew-red01/result.json)、[GREEN receipt](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/kv-sdk-development/pr141-mounted-renew-green01/result.json) |

新十一项 RED→GREEN 唯一 production 差异为 `packed_admin/packed_mounted_session.rs`；全部测试输入相同，独审确认实际测试与冻结候选仅函数排列不同、所有 body 一致。上述结果不等于当前最终源码的完整系统验收，runner/编译耗时不作为性能数据。
锁序 TDD 已完成一轮真实 RED→GREEN：fixed-filter RED 的实际 Cargo 输出为 2 passed / 4 failed，使用 [run_owned_metadata_red01_fixed_filter.py](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-tikv-lock-order-tests-candidate01/run_owned_metadata_red01_fixed_filter.py)；[RED result](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-tikv-lock-order-red01/pr141-tikv-lock-order-red01/result.json) SHA256 2d6ac914fa4f9deef7e8192caa480ddbc8a4663f28beb29dd8d31dac67bdec75，[RED cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-tikv-lock-order-red01/pr141-tikv-lock-order-red01/cargo.log) SHA256 796e82cf2dbedc5e317ff3e4ce32ad1209425fa3049605c1fa8cf60181f8098d。production active 源上同一六项 selector GREEN 为 6 passed / 0 failed；[GREEN result](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-tikv-lock-order-green01/green-test01/result.json) SHA256 5b674f8211e7170cf3061a0a7da891686df4a3427c7e744372a5b384c4da2f8c，[GREEN cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-tikv-lock-order-green01/green-test01/cargo.log) SHA256 e08a653679f320b2a5eab5408ecad48f8024fa669e1d0534e8f31b456cd5a9cd。RED/green receipts 均保持 source/config/Git 不变；该有限回归不等于完整 CLI04/real61 验收。

当前阻塞来自新一次实际 TiKV + RustFS 诊断。失败产物保留：

| 运行 | 已建立的事实与结论边界 | 原始失败日志 |
| --- | --- | --- |
| diagnostic02 | 05:12:47.935117Z 联合 writer heartbeat 返回 Backend/PessimisticLock/Deadlock；wait chain 显示两事务以 workspace ↔ head-layer 相反顺序等待。lower-read-complete=14055ms，之后 upper I/O 返回 code 116/ESTALE，panic-cleanup=30172ms。心跳死锁已证实，完整 ESTALE 因果链仍未签收。 | [cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic02/metadata-services/cli-rustfs-tikv-clean01/cargo.log) |
| diagnostic03 | lower-read-complete=14369ms 后 upper I/O 返回 OS code 5/EIO；日志未出现 Deadlock 或 PessimisticLock。该运行仍是 0/1 失败诊断，原因未定，不能据此归因或宣称已修复。 | [cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic03/metadata-services/cli-rustfs-tikv-clean01/cargo.log) |
| diagnostic01 | lower 完成后 upper I/O code 5/EIO；30556ms 是 panic-cleanup 标记，不是精确 EIO 时刻。原因未定。 | [cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic01/metadata-services/cli-rustfs-tikv-clean01/cargo.log) |
| 历史 CLI03 TiKV clean | lower/upper I/O 后 terminal-status 断言失败；原因未定，不能归因为 diagnostic02 的死锁。 | [cargo.log](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/rustfs-cli-lifecycle03/metadata-services/cli-rustfs-tikv-clean01/cargo.log) |

diagnostic03 cargo log SHA256 为 87b144872c0080d3159ab24ae6adf81be32680b1031055e58c99649052879323；metadata result SHA256 为 bb4aa5370faba787f19e004fa2b2af150ffe3d961c243108eb542a96d55d16f0；outer runner receipt SHA256 为 21d2bf80b7636640e997d6131bf21f32006a6c273797a39a8505f47bcc5405e4；cleanup receipt SHA256 为 cf2f6bab7b6eb1a0532e9651762737b6925d19d4e923c65834247c5ac62fc714。RustFS removal/absence、metadata absence、credentials destruction、fault retirement 均已记录；clock03 [receipt](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic-clock03/result.json) SHA256 为 a58f25bdfe3a2278dca70bba71f5706630cc263d1ebc2362077a77a96c17e417，记录 passed=false、restored=true、restore_timer_retired=true、clock_regressions=[]、large_clock_adjustments=[]。diagnostic03 没有 Deadlock/PessimisticLock 日志，但 EIO 仍使正式 CLI04/61 出口阻塞；不得重跑本诊断或据此签收原因。
diagnostic02 log SHA256 为 `20febd839bf8ca09dd172d4e964ba547127273ad5a1ae596ae074ed14c87b845`。其 [outer receipt](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic02/runner-result.json) 记录 passed=false，但 RustFS/metadata absence、credentials destruction、fault retirement 均 true；[clock receipt](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-cli-tikv-clean-diagnostic-clock02/result.json) 记录 passed=false、restored=true、restore_timer_retired=true、无 clock regression 或大幅调整。清理成功不改变测试失败结论。

针对已证实的锁序问题，production sorted-union delta 已定点应用到 active tikv.rs，最终 active 源 SHA256 为 c4bfe3adf516f89af54131adbdf9af0854f48b40e032bfc690579a6954133f65。它为全部 checks∪writes 的首次加锁建立统一 raw-byte 顺序，checked key 使用一次 fresh FOR_UPDATE 并核对全部 duplicate expectations，unchecked write-only key 使用纯 lock、不返回旧 value，随后保留原 put/delete 顺序与 last-write 语义；bounded authentication 只排序借用 refs，保留 duplicate 拒绝、字节/请求预算、deadline、至多两次 request-certified conflict 和 unknown handling。active 测试 recording hook 在 union 成功路径记录实际 scoped key 与 check_begin != check_index。
SDK 的 put/delete 仍会重锁本事务已持有的键；production delta 保证所有 distinct keys 的首次获取顺序，未消除这些 RPC，也未把 Deadlock 或未知提交错误改成 Busy 重放。锁序 candidate 的外置静态检查为 20/20，active source 已通过同一六项真实 RED/GREEN 回归；这些证据不覆盖正式 CLI04/61。production candidate 与 tests-only hooks 的 pins 仍见 pr141-tikv-lock-order-candidate01/REVIEW.md。

| 下一出口 | 必须建立的真实证据 | 当前结果 |
| --- | --- | --- |
| 锁顺序 TDD | fixed-filter RED 实际 2/4；production GREEN 六项真实 selector | RED 2 passed / 4 failed；GREEN 6 passed / 0 failed，已保留 receipts |
| fresh binary07 | 最终 production CLI build，完整 source/config/Git 稳定 | 已通过；[binary07 result](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/kv-sdk-development/pr141-cli-rustfs-binary07/result.json)，/home/hxy/brewfs/target/debug/brewfs，946951440 bytes，SHA256 232225c0f15f4a0d2ef346dec5bef25731a97e712604a619e5c65bdd58f67773 |
| formal CLI04 + clock04 | 真实 Redis/TiKV + RustFS clean/kill/pending-upload 共 6 例 | blocked/pending；diagnostic03 0/1 EIO，保留失败证据，不重跑 |
| full gate09 | 同一完整最终源码下原 55 项 Rust 检查 | 已通过：55/55 checks，911 inputs；[verification.json](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/full-rust-gate-pr14109-20261009/verification.json) SHA256 f2c3ec41172aaf0f666c74d121ff8c854b35e0c6e9013fedd88df51abfa1fddb |
| binary08 witness | 实际 executable path、size、SHA256 等于 binary07，source/config/Git 不变 | 已通过；[binary08 result](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/kv-sdk-development/pr141-cli-rustfs-binary08/result.json) 与 binary07 同 path/946951440 bytes/SHA256，result receipt SHA256 42df405dc48b87765fd4581897081abb6189038d2f52b33a24276cdbec17af81 |
| strict03 + clock03 | 27 例真实 Redis/TiKV metadata | blocked/pending；formal CLI04/61 未因 diagnostic03 失败而补跑 |
| RustFS04 + clock04 | 22 例实际 RustFS lifecycle | pending；未将 diagnostic03 单例当作 22 例验收 |
| SID02 + clock02 | 6 例实际 RustFS SID | pending |
| 最终 frozen audit | 新 binary 绑定先冻结；exit 0、passed=true，实际 Rust55 / real61 与完整原始证据一致 | blocked/pending；diagnostic03 失败阻止 final acceptance |
| 独立 live cleanup | 实际当前 process/container/mount absence 与 clock restoration | diagnostic03 cleanup 与 clock restoration 已成功，但完整最终 live cleanup 仍 pending |

要求 real61 = 27+22+6+6，尚未观测本次完整通过数；旧批次和三个失败诊断不补齐正式六例。CLI 与最终 auditor 均须绑定 binary07，binary08 已证明 gate 后实际字节相等；diagnostic03 的 EIO 使 formal CLI04/61 与最终 acceptance 保持 blocked/pending。不得把没有 Deadlock/PessimisticLock 的 diagnostic03 日志写成根因，也不得重跑该诊断。任一阶段失败都保留原 receipt 并停止后续验收。

| 最终动态字段 | 当前值 |
| --- | --- |
| complete_source_manifest_sha256 | f2c3ec41172aaf0f666c74d121ff8c854b35e0c6e9013fedd88df51abfa1fddb |
| complete_source_input_count | 911 |
| actual_rust_check_count | 55 |
| actual_real_pass_count | null |
| final_acceptance_passed | null |

最终 source/count 必须由真实完整 receipt 动态提供；815/909 不预填。本次有限整合验收也不覆盖下列未签边界，详见 [SPEC-INVENTORY](C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/pr141-report-supplement01/SPEC-INVENTORY.md)：

| 未签边界 | 剩余要求 |
| --- | --- |
| Source/attempt history | authenticated durable summary、安全 terminal retirement、100/1,000-cycle 真实验收；保留历史仍可能耗尽普通 32-key proof。 |
| Scoped native GC | 完整 census 的 4,096-row / 32-MiB 上限、无关 extent 扫描；需 scoped complete reachability/shared-owner proof 与跨 restart 有界进度。 |
| Retained v3 migration | packed retained/active prefixes 仍被拒绝；需可恢复且保留 roots/journals/snapshots/aliases 的 v3-only migration 或 export/import。 |
| Fragmented exporter | 单 chunk 1,024 extents 上限；需有界 merge/window 与 1,025+ row seal/remount/resume/GC 证据。 |
| Kubernetes / TLS | operator/admin/GC/finalizer 已接入；真实 API-server CRD/CEL、双 leader、Pod/PVC recovery、credentials 与 TLS/authentication 验收未完成。 |
| 完整 S / 三创新性能实验 | 先签收同源 S corpus、请求/内存/cookie/取消/清退，再冻结 matched 性能与消融及 placement/executor、provenance、cache/TTL 控制；已有 static1MiB / SizeOnly / inline-off 开关不等于实验验收。 |

此文件仅为外置草稿；candidate04 只记录已存在的 receipts，没有修改活动报告、运行 Cargo/服务/auditor，也没有宣称全部 SPEC、Kubernetes、TLS、规模及三创新性能完成。


## 2026-10-09 final source reconciliation

PR #141 is retained as merge parent ff0e6d1 through merge commit 24b3b38. The automatic merge mixed the older upstream workspace implementation into packed-v3 source files, so the final reconciliation keeps the already validated packed-v3 Redis/TiKV implementation and the non-conflicting upstream runner/operator updates. The ordinary entity CAS, migration marker, lease lifecycle, and GC fencing paths remain in place; this branch does not add packed-v2 or v5 compatibility.

The reconciliation closes three PR141 safety gaps in src/workspace_overlay/stores/kv_store.rs: released leases remain GC roots through the configured grace period, layer deletion only accepts sealed or already-deleting layers, and compaction authenticates the expected parent layer with revision_from_layer. The final kv_store.rs SHA256 is 99f818da3c56598d16aab9e3d6230ac111891f4c1db1f9f0ee7919be2a1f8a, and tikv.rs remains c4bfe3adf516f89af54131adbdf9af0854f48b40e032bfc690579a6954133f65.

Validation after reconciliation: cargo fmt --all --check, git diff --check, and CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 cargo check --locked --workspace --all-targets passed. The four entity-CAS preparation tests and the two concurrent-fork selectors passed.
