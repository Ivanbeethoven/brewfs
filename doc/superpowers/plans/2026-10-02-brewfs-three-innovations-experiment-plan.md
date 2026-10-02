# BrewFS 三项创新点联合实验规划（2026-10-02）

## 1. 研究主张与当前边界

本轮研究对象是一个生命周期，而不是三个互不相关的优化：

**共享不可变 S0 → fork 隔离 workspace → upper 增量写入 → 固定 generation 并 seal → 按访问粒度布局 → 发布 S1 → 只读挂载或下一次 fork。**

- **overlay-workspace** 决定哪些逻辑区间由 upper 覆盖、删除或打洞，并保持隔离和 generation 一致性。
- **readonly-packed metadata** 提供分页的 namespace、热属性和 data placement，减少逐文件元数据定位动作。
- **dynamic block** 在构建/发布时选择 frame 粒度；它不改变 POSIX offset、VFS chunk、upper dirty block 的语义。

应证明的是三者组合的成本与收益，不预设“所有场景快于 JuiceFS”。现有百万文件结果已有明确反例：ordered stat 对 JuiceFS+TiKV 约 2.10x，而 4KiB shuffled full-read 的最佳 packed 行仍低于 TiKV。历史实验是应用型 scanner，不是实际 GPU 利用率或训练吞吐实验。

### 当前实现审查

| 能力 | 当前状态 | 实验可宣称的范围 |
|---|---|---|
| size/profile/p90 selector、同类 co-pack、跨 frame extents | 已实现 | `choose_frame_layout` 和 `pack_group_files` 按 size/profile 选 frame，并非所有文件固定 4MiB；p90 仅有 API，fixture 未使用实际直方图 |
| GM06 placement、II05/GI04 routing、catalog/coordinator 读回 | 已实现 | readonly FUSE 经旧 slices/BlockStore facade 进入 catalog 的 unified packed plan；不是完整 P5 overlay mount |
| 小于 256KiB inline、224KiB/group budget | 已实现 | inline 能消除独立 data GET，但 bytes 仍通过 GroupMeta 下载并占 metadata cache，不能视为零物理数据读取 |
| 随机 4MiB / 顺序 8MiB 上限、raw codec 0 | 已实现 | 独立 raw frame；不存在 zstd 解压性能证据 |
| 压缩、restart table、authenticated descriptor directory | 未完成 | 当前 range path 只做 frame 自洽校验；descriptor digest 未形成到 manifest 的完整认证链 |
| sparse reader / sparse producer | 读取模型部分实现 / producer 未实现 | gap 零填与 metadata round-trip 不等于真实稀疏 inventory 发布与 FUSE 验收 |
| 大文件多 frame / external large DataRef | 部分实现 / 未实现 | 可测试 10/32MiB 单 container 内拆帧；大于 container hard limit 的 fallback 不可宣称 |
| upper/lower composition / packed lower 接线与发布 | 原语已实现 / P5 未完成 | `compose_overlay_plan` 单测不是 workspace+packed FUSE 端到端证据 |
| symlink/xattr/ACL/hardlink placement | 未完成或不完整 | 不能把完整 POSIX 能力作为本阶段已验收前提 |

参考代码：`src/workspace_overlay/packed_v3/{layout,group,meta,catalog,remote,readonly}.rs`、`src/chunk/read_plan.rs`。normative target 是 `doc/superpowers/specs/2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md`，以真实实现与检查表为准。

## 2. 优先解释 dynamic block 与 packed metadata 的耦合

当前默认 tiny frame target 是 256KiB，但 stored length 是实际有效 bytes，不应把 target 当作每次必读长度。共享 frame 的随机单文件请求仍会下载其中未被该文件使用的 bytes。

inline 同样不是免费：百万 4KiB fixture 的 GroupMeta stored total 约 1.013GB、inode-index 约 131MB。GroupMeta 中含 inline payload，因而“512MiB metadata 不够”不能只解释为 POSIX 元数据太大。实验要分拆 **hot fields、names、extents、descriptor、inline payload** 各自的 wire/decoded/retained bytes，分别报告请求和物理放大。

更小 frame 的好处是随机读 overscan 减少；代价是更多 descriptor、更多 frame/digest、可能更多 GET、更低压缩率。更大 group/container 能减少对象数和顺序 RTT，但不应强制随机读整个 container。分组、解码单元、一次 GET 和缓存单位是四个独立参数。

## 3. 首先补正确性与测量门禁（E0）

在任何新的优势主张之前：

1. metadata/index/directory 由可信 manifest ref digest 绑定；payload descriptor 必须得到独立认证。当前 raw frame digest 来自 container table，只说明 table+payload 自洽。先实现明确的新版本 authenticated directory/ref，保留 004 回读，或者将 004 安全边界明确标为受信任 immutable backend，不得宣称完整抗篡改认证。
2. counting backend 覆盖 header、index、GroupMeta、descriptor、payload 的实际请求和 bytes；不把 cache miss 等同 remote GET，也不把多次分层 cache hit 相加当作文件请求量。
3. 加入 short/overlong/interrupted/corrupt stream、失败不 admission、缓存 eviction 后恢复、source ordinal/inode/name 错配拒绝。
4. 核验 reader-side size-class table/profile/codec/长度一致性。invalid frame claim 应 fail closed，不能把损坏 placement 当 hole。
5. `BREWFS_CACHE_TTL_MS` 才是当前 FUSE 读取的变量。云 runner 使用 `BREWFS_METADATA_CACHE_TTL_MS` 时必须显式桥接；保留实际 mount 与 FUSE op counters，旧 TTL 对称主张暂不沿用。
6. 记录 daemon RSS/PSS 与 scanner RSS 两个独立指标。Moka weighted capacity 不等于 RSS，in-flight frame/active handle 额外驻留必须计量。
7. fixture、manifest、binary SHA256、git revision、dirty diff hash、完整命令、每阶段 logs、错误数、清理核验形成证据链；输出 summary 即便失败也不得当作有效结果。

本轮新增 corpus 回归覆盖 200KiB/512KiB/1MiB/10MiB/32MiB、random/sequential、p90=200KiB 下 10MiB 文件、inline 与非 inline tiny、文件尾及跨 frame 257B 读取。它走 builder→manifest encode/decode→catalog→remote ranges，不替代真实 FUSE 或完整 lifecycle 验收。

## 4. 主消融设计：先量每项贡献，再测组合（E1–E3）

### E1：readonly-packed metadata 本身的贡献

同一只读逻辑 namespace、相同 data placement/read executor，比较：

- 普通 BrewFS+TiKV metadata；
- packed metadata，保持 static frame layout。

分别执行 tree-only、已知 path stat、随机 lookup、open-first-read。拆分 discovery、path resolution、getattr 与 payload；不能把 catalog inode probe 速率冒充 `stat(path)` 速率。metadata warm-off、auto、eager 分开，报告 mount+warmup+scan 与扫描阶段。

基线 JuiceFS+TiKV 使用同样 external scanner 和逻辑树，但不将它与 BrewFS 的 metadata 编码因果消融混为一谈。JuiceFS 是整体系统对照，不是只有 KV 不同的相同 executor。

### E2：dynamic block 的独立贡献

保持 packed metadata、inline **关闭**、group/container 边界、cache policy、raw codec、executor 相同，比较：

- static 256KiB、1MiB、4MiB frame；
- size-only dynamic；
- size + 已知访问直方图 dynamic（p90 从预先收集的训练集生成，不能使用测试请求事后选择）。

static layout 控制需在 offline builder 显式实现/固化，不允许靠 mount 的 `block_size` 改变旧对象；控制 arm 不是生产优化，格式和认证要求与 dynamic 相同。

primary corpus：200KiB、512KiB、1MiB、10MiB、32MiB 等量独立内容，以及 4KiB–64MiB mixed+sparse（后者必须先完成 producer/large fallback）。request size：4KiB、64KiB、200KiB、full；offset 包含起点、随机位置、跨 frame 边界与 EOF。

指标：fetched/logical、GET/file、descriptor bytes/file、frame count、decode/hash CPU、p50/p95/p99、files/s、active+drain bandwidth。JuiceFS 小文件对象真实长度须测量，不能假设其所有小文件都下载 4MiB。

### E3：metadata × frame 的交互效应

理想四格（同一 BrewFS runtime）：

| Metadata | Data frame | 目的 |
|---|---|---|
| native TiKV | static | 对照 |
| packed | static | packed metadata 主效应 |
| native TiKV | dynamic | dynamic 主效应 |
| packed | dynamic | 联合收益与 interaction |

**当前实现没有 native-TiKV→PackedFrame 的同 executor bridge，故第三格未实现。** 先做前三阶段可执行的控制：packed-static vs packed-dynamic，以及 native-TiKV-static vs packed-static。只有补齐桥接后才给四格完整的因果结论。可用 interaction 指标为联合对数吞吐增益减去各自单项对数增益，并同时分析物理请求图；不得用 JuiceFS 的不同 executor 补不存在的第三格。

inline on/off 是 E3 的独立嵌套因素，不要把 inline 收益全部归给 frame selector。第二组控制逐项打开 locator admission、metadata prefetch、demand coalescing、sequential hint/readahead、decoded frame cache。一次只改一个因素。

## 5. 三创新生命周期实验：overlay + packed + dynamic（E4）

此阶段以 P5 实际 lower adapter、VFS unified executor 和 sealed publish binding 完成为前提。

### 正确性矩阵

- S0 fork 两个 workspace，A 的 create/rename/unlink/write 不影响 B 或 S0；lower 永远 immutable。
- upper full cover 不读 lower，partial cover 只读未覆盖的 lower 区间，explicit hole/truncate 不复活 lower 数据。
- upper epoch 或 lower digest 在 resolve/fetch 期间改变，整份 read plan 丢弃并重试，不暴露混合 generation。
- small-file inline、跨 frame 大文件、raw non-UTF8 names、hardlinks、sparse、冷属性在实际 FUSE 中验证；未实现项不能标记为通过。
- seal/repack/publish 完成 cold objects→containers→indexes→manifest→head CAS 后才能可见。各阶段注入失败，remount recovery；旧 snapshot/lease 继续可读，orphan GC 不误删可达对象。

### 性能矩阵

S0：先用 10k 独立 100KiB–1MiB 文件；workspace count 1/8/32，修改文件比例 0/0.1%/1%/10%，partial overwrite 4KiB/64KiB、新增/删除/rename/hole 分开。

对照：完整 copy/reimport、当前 workspace delta-only/native lower、workspace+packed static、workspace+packed dynamic。

记录 fork ready latency、bytes copied、upper private bytes、lower GET、PUT/GiB、write/read p95、close/fsync/drain、seal/repack wall 与 peak RSS、对象/索引复用比例、GC bytes。普通 close/fsync 不能静默触发全量 repack。比较“fork + 修改 + 发布 + 首次消费”总 wall，不能把成本移到 seal 或关闭阶段。

hypotheses：fork 开销不随全量 payload 增长；小改动复用绝大多数 immutable objects；发布后的 dynamic lower 降低局部随机读放大；upper dirty bytes 和 lower pipeline bytes 分别有界。

## 6. 数据集与公平协议

### 分规模渐进

- 微型：6–100 文件 size-class / error corpus，counting backend + LocalFS + 真实 FUSE。
- smoke：10k 独立内容，matching JuiceFS+TiKV；bounded timeout，失败即停。
- 中型：100k 和超大单目录，验证 cache churn、pagination、随机 partial read。
- 大型：1M 仅在前级正确性/请求图/收益通过后。existing Redis/TiKV rows 只能在同 dataset/TTL/direct/cache/compiler/profile 时复用，不重复无意义的大规模导入。

### 三种缓存协议（每个工具新 mount）

1. demand-cold：memory/SSD/decoded/window/data-prefetch=0、drop host cache；metadata retention 显式预算，warm-off 和 metadata-warm 两行。
2. cold-pipelined：持久/decoded data cache=0，明确 bounded sequential signal/window。当前 aligned-window cache若保留内容，另标 window reuse，不叫零数据缓存。
3. warm-frame-cache / repeated epoch：对两端给相等**字节**数据缓存预算；kernel buffered cache 和 client cache 分开计。第二 epoch 不叫 cold。

报告 client+metadata server 的总资源；TiKV/Redis 服务常驻内存与 packed client metadata budget 并列，不能只给 packed 加无限 RAM，或故意关 JuiceFS cache 后泛称生产性能胜利。严格 cache-off 与 production-warm 均需有行。

顺序、shuffle 与 hotset 三个 access traces；16 workers 主行，1 worker 随机延迟 guard。实际 GPU DataLoader 验证另阶段：相同模型/样本 decode/批量、报告 GPU idle 与 examples/s，不能只看文件读取速率。

## 7. 当前预算候选如何进入上述规划

本次仅测试 manifest-aware index budget split：固定总 metadata bytes、其他 tiers 不变，将 group-index 闲置份额给 inode-index。先做认证页 churn 回归和 paired local FUSE A–B–B–A；不得混入 frame size/inline 改动。

错误基线保留原始 log 并拒绝性能数字。如果旧版本自身出现 EIO/ENOENT，优先诊断正确性并记录 blockers，不靠候选“更快”掩盖错误。若接受，则加入 metadata residency 的 ablation；如果不稳定或没有收益，targeted patch 回退生产候选，保留测试与 rejected experiment 文档。

## 8. 验收和产物

- 每个 code arm 通过 AGENTS.md 本地门禁，特别是同迭代 workspace `cargo test --workspace --lib --bins`；packed feature suite 额外运行，0 tests 不算通过。
- 无 checksum/size/namespace 错误；无额外 generation 混合；无未解释的 FUSE teardown、drain、D-state 或 resource leak。
- 至少两次重复，最终规模建议 ≥3 对随机化执行顺序的 paired runs；报告中位数、分布与 variance，不以单次峰值决定接受。
- 新优化在目标 workload throughput/p95 有可解释改善，其他受影响读/写/metadata scenes 无 >5% material regression，`fio-randrw` 是可写/共享 executor 修改的硬 guard。
- 保存 binary/manifest/trace SHA、profile/cache proof、日志和 summary。daemon RSS 与 scanner RSS 分列，active 和 active+drain bandwidth 同时报。
- 测试创建的 mounts、processes、Compose services/volumes、临时 OSS prefix、ECS 在结束/失败后关闭并核验；不删除用户对象、凭据或已接受 artifact。

### 推荐实施次序

**完成当前预算候选 → 修正测量/认证门禁 → 联合 size-class corpus + FUSE → builder 显式 static/inline/size-table 实验参数 → packed/static-vs-dynamic 消融 → P5 lower bridge与generation/publish → lifecycle 总成本实验 → matched JuiceFS+TiKV 和 GPU DataLoader 大规模验证。**

这是实验规划而不是全部功能已完成的声明；每个阶段的可执行状态及缺口必须随实际验证更新。
