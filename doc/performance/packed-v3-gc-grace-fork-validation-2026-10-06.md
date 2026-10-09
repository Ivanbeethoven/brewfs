# Workspace GC 宽限期与 fork 竞态验证 — 2026-10-06

当前范围仅 v3/005。这一批修复 v3 workspace 生命周期依赖的两个 catalog GC 缺陷，
不增加旧 packed wire 兼容，不代表完整 packed 对象图 GC 或全部 SPEC 已完成。

## 实际缺陷与修复

SQLite 与 KV 的 GC root 扫描和删除前重验证，只保护 Active/Releasing lease，
把已经 reap 为 Expired 的 lease 排除在宽限期保护之外。修复把 Expired 纳入同一
`expires_at_ns > now_ns - grace` 条件；Released 仍不作为恢复 root。

KV fork 原来只检查 sealed base 和两个新 hot key。GC 完成 root 扫描后发布的新
workspace/head 不会使这份扫描失效。隔离用例实际观察到 fork 成功后，共享 slice77
及六字节数据仍被 GC 删除。修复在发布时比较并更新同一个 CONTROL topology 值，
与 GC 的原子重验证形成冲突和重试；同时发布新 workspace/head 的 hot rows。

生产改动仅 `stores/database.rs` 与 `stores/kv_store.rs`。CONTROL 的完整 map 编码、
历史记录增长和深层布局预算仍是未完成项；没有通过放宽预算处理这些问题。

## 实际 RED → GREEN

RED session5252 实际 chunk944fb6/exit0：编译通过，十项 exact 均真正运行一次，
其中五项以原语义断言失败101，另外五项正常控制通过。这里父进程 exit0 表示
观察结果符合 RED/control 计划，不能把五项 Rust 失败称为通过。

最小生产补丁接入并格式化后，GREEN session3844 实际 chunkd6fdd7/exit0：编译和
十项 exact 全部通过。RED/GREEN 的三个测试文件与全部测试断言字节完全一致；
554 输入仅两个生产文件改变，其他552输入不变。

| 实际用例 | 数量 | RED 结果 | GREEN 结果 |
| --- | ---: | --- | --- |
| SQLite Expired lease 扫描与删除重验证 | 2 | 原语义失败 | 通过 |
| KV Expired lease 扫描与删除重验证 | 2 | 原语义失败 | 通过 |
| root 扫描后 fork，保留真实共享数据 | 1 | 共享数据被删除 | 通过 |
| SQLite/KV Released lease 正常回收 | 2 | 通过 | 通过 |
| 删除先于 fork CAS，拒绝新 child | 1 | 通过 | 通过 |
| fork 删除后的实际数据回收与重试 | 1 | 通过 | 通过 |
| seal/flatten 后删除全部历史并实际回收 | 1 | 通过 | 通过 |

宽限期 GREEN 覆盖 -1、0、199、200、201ns 边界。RED 在首个失败断言停止，
不据此声称后续边界也已在 RED 中执行。两项正常生命周期控制通过新 store 重读、
缺失 hot layer、真实 block 读取失败及第二轮空 GC，检查缓存不复活和回收幂等。

## 证据与未完成范围

证据根：`C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/`。

- `g13-ten-exact-red01/run-red-observe`，verification SHA256
  `dfcf4966a55eab1bc81350347ac47916403cb8dafdaf9736405bd16f65ad32e8`。
- `g13-ten-exact-green01/run-green`，verification SHA256
  `3d21c1569a6190fc8a458fddd4f9e6c75b8d088929fbc069d425add94d537c11`。
- `g13-root-integration01` 与 `g13-root-production-integration01` 保存接入前后源码、
  完整 diff 和精确 delta；生产 delta manifest SHA256
  `e65982d6e3e2635cf6925424ba1d605f3b1152dc2753814ed8f1b2ebc4d4fd46`。
- RED/生产集成独审实际2da5f8/exit0，receipt
  `91c5d4f5cf82c97d761ae0fe92144926f080a785c58c90c326be7aefa25e8e0f`；
  完整 RED→两文件生产修复→GREEN 独审也已实际36c21b/exit0，receipt
  `6f58f599d07ef60bb7edbb911b25eb10f965b58d7f1e7b587c1975946b0ccb9a`。
  双方 runner/config/environment/Git/十项 argv 与测试字节均相同，全部554保存源码
  与每步及最终 guard 通过；该独审没有另行重读 active。

SQLite 和 MemoryBackend 使用实际 catalog API、resolver、collector 与非空
InMemoryBlockStore fixture；Noop barrier 仅用于已经写入该 fixture 的数据。
这不能证明 Redis/TiKV、远端耐久 drain、PM11/container/index/cold/descriptor 图、
reader pins、publication/recovery 或 Kubernetes 资源回收已通过。

完整 gate10/supplement09 已在 session1330 实际 chunkacfa67/exit0 串行结束：
49/49 与17/17 全部通过，两批554输入完全相同且逐步/终态无变化。canonical manifest
`335b8d51333e6cfcf19cddda8c1c8ff487d29ccc2f8cb2b4eb532824e25c01ab`。
主 verification SHA256 为 `ad7f6e01a07573b16ad73d5f7746ef1e77f23dd589d4854a3ebaff652e6a1247`；
补充为 `c9172155ab1587b4933e7975f0f36344cf0accab85a42ce12f021217366f54a6`。
组合独审实际326120/exit0，`full-gate10-supplement09-independent01/review02.json`
SHA256 `047de001e7f541ff8d5a6c244e32cf2f8d74e5dde2c1206df1516caa72ee1aab`。
独审核对66份日志、原计划、环境和两份保存源码；旧零测试命令仍不作行为验收。

本轮 candidate06已绑定真实终态，采样器12项离线测试通过；两runtime正在构建冻结，
新源码实挂载与严格pending-body SIGTERM仍未执行。所有SPEC、系统S与实验X继续开放。
