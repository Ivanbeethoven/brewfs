# BrewFS–RustFS cooperative I/O design

## 1. 三个大小层次必须分开

| 层次 | 含义 | 本轮观察或配置 |
|---|---|---|
| 文件/应用请求 | 应用读写的完整文件及 syscall 缓冲区 | 文件 4 KiB 至 64 MiB，多种大小分别测试 |
| FUSE 请求 | 内核交给 BrewFS 的一次读写 | 4 MiB WRITE 可实现；完整读取通常拆成 128 KiB READ |
| BrewFS 数据块 | 不可变 slice 内上传和缓存的数据单位 | 4 MiB；文件可对应多个对象，尾对象单独处理 |
| S3 编码对象 | 补零、压缩、格式头完成后的上传字节 | 未压缩的完整 4 MiB 数据块为 4 MiB + 4 bytes |
| RustFS EC 块 B | 对象的纠删码编码单元 | 当前 auto 对较大编码对象请求 1 MiB |
| 校验帧 q | 每个 shard 内实际 bitrot 校验帧 | 与 B 独立；本轮关闭新 q 布局写入 |

ACS 上实际 `max_read` 为 4 MiB，但 BDI read-ahead 为 128 KiB，因此协商上限不等于常见请求大小。`/sys` 只读，实验沿用正常内核行为，没有强制 direct I/O 来制造更大的请求。128 KiB 小写入仍聚合成 4 MiB 数据对象；无需让每次 FUSE 请求都等于 EC 块。

## 2. 以已定型对象为依据的 auto hint

```mermaid
flowchart LR
  A[应用写完整文件] --> B[FUSE WRITE]
  B --> C[内存 slice 与 SSD writeback 暂存]
  C --> D[后台 uploader 拆分数据块]
  D --> E[补零、压缩、格式头]
  E --> F[根据最终对象字节数选择 hint]
  F --> G[RustFS 校验提示并持久化 EC 布局]
```

**决策时机**：在 S3 上传入口已知道完整 wire length 时选择。它覆盖“先暂存再上传”和“暂存、上传并行”两条路径，也覆盖暂存恢复后重新上传。不能假设所有数据都先完整落 SSD，再由扫描器决定布局。

**数据依据**：原文件可能继续增长；一个 slice 可能拆成多个对象；SSD 可能预分配；压缩率会改变长度。因此文件总长、slice 长度、SSD 文件占用及本次 FUSE 请求长度都不能代替最终上传长度。当前实现不扫描 SSD，也不增加 journal 大小字段。

| 运行时配置 | 每个新对象的行为 |
|---|---|
| 不配置 hint | 不发送扩展元数据，使用原来的读/缓存策略 |
| `auto` 且编码对象 ≤ 1,048,576 bytes | 省略 hint，保留服务端默认和内联选择 |
| `auto` 且编码对象 > 1,048,576 bytes | 请求 1,048,576 bytes EC 块 |
| 固定 65,536 / 262,144 / 1,048,576 / 4,194,304 | 对每个新对象使用显式覆盖值 |

普通 PUT、vectored PUT、create-only PUT、普通 MPU 和 vectored MPU 共用选择函数。MPU 在创建上传时使用完整对象长度，不能根据单个 part 重新选择；重试保持同一对象的选择，小尾块不继承前一个大块的 hint。

请求字段为 `x-amz-meta-rustfs-ec-block-size-hint`。功能需同时编译 `rustfs-ec-block-size-hint` Cargo feature 并配置运行时选项；feature 默认关闭。未编译却配置 hint、非 S3 后端启用 hint、非法固定值都会明确报错。CLI 优先于 YAML。

读取使用对象已持久化的 EC 布局，不根据当前客户端配置重新解释旧对象。HEAD 的 `x-rustfs-effective-ec-block-size` 用于检查实际结果；请求中包含 hint 不代表服务端一定应用它。

**本轮修正**：旧 auto 对大对象请求 4 MiB，只是“减少 EC 分段”的假设。同客户端/同后端对照包含约 9.9% 的明确冷读劣化，故当前选择 1 MiB。在本轮 RustFS 默认布局下，这个值与默认 EC 大小一致；收益不能解释为“hint 数值变大”。详见 [实验结果](evaluation.md)。

## 3. 完整对象下载合并与缓存准入协同

完整文件读取会形成大量 128 KiB FUSE 请求。若一个片段探测格式头、另一个发 Range、预取又发完整 GET，就可能重复传输同一对象。hint 模式下的改进有两部分：

1. 将同一对象的并发片段汇入已有完整 GET SingleFlight，直接解析这次完整响应中的格式头，复用解码后的块；消除独立头探测与整块下载之间的重复请求。
2. 内存缓存选择 LRU，继续使用同一字节容量上限。连续片段会让已读旧块累积访问频率，TinyLFU 可能拒绝新块；LRU 让当前流的新块进入缓存，从而服务后续片段。

这不是无限缓存，也不是把多个不同对象合成一个 GET。完整响应仍执行长度、格式、解压和完整读取检查。mode 在挂载级启用，故 auto 省略小对象的上传字段时，该挂载仍采用协同读取策略。

本设计面向完整文件/完整数据块；稀疏 Range 访问未作为本轮收益结论。固定 hint 的独立 EC 对照保持相同 GET 合并和缓存策略，避免把减少重复请求的收益错误归到 EC 布局上。

## 4. 以远端完成事件释放写预算

先前已提交且上传成功的 slice 仍可能在 recent-slice 缓存里保留约两秒。数据虽已在 RustFS，内存页及写预算仍被占用，64 MiB 写预算容易让下一批写入等待。

现在在 `clear_recent_pending_if_complete` 中同时满足 `upload_complete()` 与 `SliceStatus::Committed` 时执行 `release_all()` 和 `update_usage(0)`，再清理 pending 记账。页面、预算和唤醒顺序一致，避免只改指标而没有释放实际占用。

上传未完成时不能释放：本地脏数据仍用于覆盖读取和恢复。回归测试先阻塞远端上传，确认预算保留；放行上传后确认立即归零，并核对远端完整字节。性能同时测上传完成和全部清债，证明没有把前台成本推迟到后台。

## 5. 共享不可变缓存字节的 FUSE 回复

原路径即使命中缓存，也要为每个小请求分配输出、清零和复制。新的 `ReadReply` 区分自有 `Vec<u8>` 与共享 `Bytes`：

- 只有 slice 元数据已缓存、最新 slice 完全覆盖请求、请求不跨 chunk/数据块且完整字节在内存中时，才返回缓存的 `Bytes::slice`。
- 正确处理文件 EOF、slice 相对偏移及块内偏移；保留读背压。
- 发生脏数据覆盖时转成独立 `Vec`，避免修改共享缓存；公共返回 `Vec` 的接口仍按原语义提供自有数据。
- FUSE 回复的数据和预算 guard 一起存活到最终消费者释放。这里减少的是用户态缓存到回复的复制，不宣称内核/S3/解码全链路零拷贝。
- 空洞、跨块、未命中继续走正常路径；workspace read provider 保留既有 fence 和输出所有权路径。恢复暂存数据存在时仍执行覆盖读取。

测试覆盖共享地址、覆盖修改隔离、偏移、空洞、EOF、缓存范围不足和元数据失效。只绕过任务调度的早期候选约 1% 变化，已删除；保留的是实测有收益的共享回复。

## 6. RustFS 端承担的工作

本轮服务器固定为 [cc4926f](https://github.com/Ivanbeethoven/rustfs/commit/cc4926fac1107aee76540f28bf22dbd47fcbca24)，本轮没有再修改 RustFS 源码。该分支已有：

| 机制 | 合同和实现边界 |
|---|---|
| 按对象解析 B hint | 鉴权与适用条件检查后选择；PUT/MPU 持久化有效布局；GET/HEAD 按对象布局工作 |
| q 帧元数据 | 内部兼容键记录实际校验帧；读取遵从持久化值，不因当前写开关改变而误解旧对象 |
| 按需数据 shard 读取 | `663c3fa` 在适用批读中先满足数据 shard 需求，避免不必要的 parity pread，保留恢复所需的路径 |
| 小窗口 positioned read | `69c2383` 对 Linux 上 ≤256 KiB 且短于整个文件的编码 shard 窗口选择 buffered `read_exact_at`；不是 O_DIRECT，也不是按 S3 文件大小选择 |
| 布局感知批读 | `cc4926f` 按已持久化帧计算物理窗口和校验和开销，受原 batch cap 限制；逐帧验证，解码仍使用逻辑 EC shard 大小 |
| healing 帧一致性 | `054577c` 在修复时遵从持久化帧大小，避免读路径支持新布局而 healing 用错帧边界 |

帧批读保持 inline、远端、CopySource 和超过 cap 时的既有路径。本轮配置启用 B hint、fleet confirmation、data-shard lockstep 与 mmap 路径；关闭新 q 写入，开启 ≥4 MiB 文件读后 page-cache reclaim，关闭写后 reclaim。因此不能把本轮性能写成“小 q 已被验证有收益”。历史诊断指标草稿也不作为吞吐依据，本轮使用文件操作计时和独立 FUSE/S3 计数。

服务端源码入口：[EC layout](https://github.com/Ivanbeethoven/rustfs/blob/cc4926fac1107aee76540f28bf22dbd47fcbca24/crates/ecstore/src/object_api/ec_block_layout.rs)、[batch I/O](https://github.com/Ivanbeethoven/rustfs/blob/cc4926fac1107aee76540f28bf22dbd47fcbca24/crates/ecstore/src/set_disk/core/io_primitives.rs)、[local reads](https://github.com/Ivanbeethoven/rustfs/blob/cc4926fac1107aee76540f28bf22dbd47fcbca24/crates/ecstore/src/disk/local.rs)、[healing](https://github.com/Ivanbeethoven/rustfs/blob/cc4926fac1107aee76540f28bf22dbd47fcbca24/crates/ecstore/src/set_disk/ops/heal.rs)。

## 7. BrewFS 源码导航

| 职责 | 源码 |
|---|---|
| feature、CLI/YAML 和 SDK 校验 | [Cargo.toml](../../Cargo.toml)、[config.rs](../../src/config.rs)、[cli.rs](../../src/cli.rs) |
| 对象大小选择与全部上传入口 | [cadapter/s3.rs](../../src/cadapter/s3.rs) |
| 读取策略暴露 | [cadapter/client.rs](../../src/cadapter/client.rs) |
| 编码、完整 GET、SingleFlight、缓存字节借用 | [chunk/store.rs](../../src/chunk/store.rs) |
| 同容量 LRU、内存范围共享 | [chunk/cache.rs](../../src/chunk/cache.rs) |
| 已上传 slice 释放、脏数据覆盖 | [vfs/io/writer.rs](../../src/vfs/io/writer.rs) |
| ReadReply、命中条件与 COW | [vfs/io/reader.rs](../../src/vfs/io/reader.rs) |
| FUSE 回复所有权与公共读接口 | [vfs/fs/mod.rs](../../src/vfs/fs/mod.rs)、[vfs/handles.rs](../../src/vfs/handles.rs) |
| 暂存恢复状态 | [vfs/cache/write_back.rs](../../src/vfs/cache/write_back.rs) |

路径表相对仓库根的导航由 [维护文档](maintenance.md) 解释；部署配置及历史验证另见 [auto hint 说明](../../doc/testing/rustfs-auto-object-hint.md)。
