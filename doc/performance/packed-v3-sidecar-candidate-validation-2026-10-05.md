# v3 sidecar 与 bounded extent 候选验证（2026-10-05）

本批只支持当前 v3/005；不提供 v2、004 或旧 packed payload 兼容。
**候选尚未验收；G10d/G10e/G10f、系统 S、实验 X 和全部 SPEC 继续开放。**

SQLite open 不再自动初始化缺失表；同一事务检查必需表、header、workspace、
head/祖先链和 current PWB3。Ready 要求固定两层；同 owner 重开重新检查 seal。
四个 sidecar API 共用严格解码，拒绝空/超长 owner、非正 generation、非 boolean
recovery flag 及 state/flag 不一致。SQL staging 后再检查 expiry，过期显式回滚。
首次 open/过期 takeover 用新 expiry，live reopen/renew 用旧、新较小值，ready/close
用当前 expiry。此应用层检查不能保证 SQLite 实际提交可见时刻的原子 deadline。

KV open/ready 使用固定键的 consistent timed read/CAS；renew/close 只读取 sidecar。
候选限制 owner、hot/sidecar 与 ControlState 解码大小，拒绝 trailing bytes，并在
后端 CAS 检查 deadline。ControlState 仍是全量 blob；网络字节数未在传输前限制，
base/current PWB3 和完整 recovery 尚未接入，不能称为完整 bounded open。

bounded extent 在后端限行并保留原始 sentinel 判断；过滤前校验长度、logical/slice
overflow、layer/inode/chunk/sequence 与规范 key 身份。这样损坏行不会被过滤成
Absent 后触发 lower fallback。范围索引、传输字节界限和完整预算仍缺。
packed readdir/list_xattr/lower-only record_open 草稿已撤回；mount capability 继续 false。

证据目录：`D:/Codex-Recovery/brewfs-root-20261004/v3-sidecar-validation01/`。
SQLite RED 独立保存在相邻 `v3-sidecar-expiry-red01/`，不覆盖失败日志。

| 验证 | 实际结果 |
|---|---|
| extent identity 行为 RED | 1 failed：head key 的 base payload 被错误返回 |
| SQLite sidecar 行为 RED | 1 passed/12 failed：5 类坏记录、7 类提交前过期仍写入 |
| 修复后 stores | 59 passed/7 ignored，真实 Redis/TiKV 未执行 |
| 默认 workspace 库/主程序 | 1063/1147 passed，各 225 ignored，0 failed |
| overlay 库/主程序/fixture | 1567/1658/8 passed，库与主程序各 235 ignored，0 failed |
| AGENTS 最少命令 | fmt、脚本、check/build、两 runtime、workspace tests、普通 clippy、diff 通过 |
| overlay 严格 clippy | exit 101，完整 CI 尚未通过 |

`local-gate/verification.json` 固定 482 输入，manifest SHA256
`56c78c0dd34e643d290cba64d7da774654b0abeb9d3f518422b7f11593a72189`；18 命令中
17 通过，仅严格 clippy 失败，逐阶段和终态源码未变化。该清单覆盖本地最少门禁和
附加 overlay 测试，**不是完整 CI 输入/命令清单**；all-features、vendor、operator/CRD
及真实后端/FUSE 出口不能从此报告推导为通过。

之后只修正 KV 测试辅助代码的两项 clippy 样式问题，另存 `style-delta/`：stores
再次 59 passed/7 ignored，fmt/diff 通过，源码前后 hash 一致。该 delta 不把原门禁
升级为新源码的完整 CI 验收。严格检查再次失败，lib test 仍报告 120 项错误；本轮
KV 测试辅助代码的两项告警已消除，其余包括 stores/clock_cas_tests.rs 的 clone
告警及其他模块告警，不能全部归为旧问题或据此签收。保留实际失败，不降低 CI 要求。

sidecar token 仍只约束自身 API；mount、mutation、hash/commit/abort recovery 的
owner fence、完整 publication/PWB3、reachability GC 和 operator capabilities 未完成。
本批未 commit/push，未启动最终性能 campaign。
