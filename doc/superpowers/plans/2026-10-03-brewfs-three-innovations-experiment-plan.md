# BrewFS 三创新实验方案（2026-10-03 更新）

**候选设计，尚未冻结执行。** 用户于2026-10-04要求先确定系统完善，再仔细规划实验。
[系统验收与实验进入条件](2026-10-04-brewfs-system-readiness.md)已复核当前源码身份，
确认核心三创新系统S出口及主实验X出口均未通过。本文的运行格、样本数和接受阈值
保留为草案；系统验收通过后再结合实际接口冻结，不以只读100/1,000文件成功启动campaign。

## 研究问题与进入条件

本方案围绕同一个生命周期：一致源 S0 → 共享不可变 lower → fork 独立 workspace →
upper 增量修改 → 一致 seal/repack → 原子发布 S1 → 首次消费/再次 fork → 可达性 GC。
三个创新点分别影响元数据定位、物理读取粒度、工作区隔离与发布成本。

对应 [SPEC 差距审计](2026-10-03-packed-v3-spec-gap-audit.md) 的 G01–G17 是实验依赖。
设计完成不表示这些功能完成；未支持的实验格记 `blocked`，不能给0错误或模拟吞吐填格。
本方案更新 [10月2日历史规划](2026-10-02-brewfs-three-innovations-experiment-plan.md)，
保留其旧证据，但以后以本方案的当前状态、控制和门禁为准。

| 创新点 | 可证伪假设 | 成功须同时解释 | 已知可能失效场景 |
| --- | --- | --- | --- |
| I1 readonly-packed metadata | 同placement/executor下，分页元数据减少逐文件远端定位与KV依赖，降低namespace冷访问成本 | tree/lookup/stat/open的latency、请求图、实际wire bytes、client+server资源、mount+warmup成本 | metadata churn、深cookie、重复ancestor routing、inline扩大GroupMeta |
| I2 dynamic data blocks | 构建时按size/profile/训练p90选frame，在局部随机读中降低raw过读，并在顺序读保持有效合并 | GET数量与粒度、raw decoded/有用bytes、descriptor成本、CPU、延迟和active+drain BW | 小frame过多GET/索引、全读、大frame共享放大、错误profile、压缩率变化 |
| I3 overlay workspace | 共享immutable lower并只写upper，降低fork和稀疏修改的总成本，保持隔离/一致性/可恢复发布 | private bytes、复制/上传量、seal/recovery/GC成本、S1首次消费、旧snapshot可读 | 大比例改写、全量repack、层深增长、generation churn、GC或退出成本转移 |

性能主张按workload限定。历史百万4KiB shuffled结果并未胜过JuiceFS；4KiB语料不替代
当前100KiB–1MiB目标。scanner不是GPU训练吞吐，不能用files/s直接宣称GPU利用率提高。

## E0：正确性、源语义与观测校准

所有性能行先过零错误门禁。按最小语料执行，不为了correctness创建大规模云资源。

| 门禁 | 测试与期望 | 依赖 |
| --- | --- | --- |
| 旧格式与005认证 | 004回读；固定trusted manifest ref后，替换descriptor/page/payload并重算内部hash仍拒绝；未知kind/flags/codec/feature fail closed | G01与已有wire回归 |
| range/codec失败 | 短读、额外chunk、末尾error、zstd串帧/尾随/大窗口、stored膨胀；无部分成功输出/失败cache admission | 复用已有回归，加真实transport |
| 一致source与POSIX | root/hot/cold、raw names、hardlink跨目录、子树nlink策略、ACL/DAC、special/rdev；源变化拒绝publish | G02,G04 |
| sparse/large | 0B、all-hole、SEEK边界、跨frame/EOF、10/32/>64MiB、>256 extents；size/blocks/content与源一致 | G03 |
| 目录与资源 | 深cookie计GET、不下载跳过的GroupMeta；多container/慢consumer/cancel/evict/shutdown预算回收且无挂起 | G05,G07 |
| 观测守恒 | counting backend与实际`.stats`交叉校准；probe/manifest/metadata/inline/payload/failed/retry都有口径 | G06 |

先LocalFS/counting backend，再真实loopback RustFS+FUSE。005选择必须显式且实际magic/codec
匹配。debug行只报告正确性。真实source/hardlink/ACL corpus与性能主语料分离，避免隐藏
诊断文件改变性能分母。后续代码批次已修复 GM07/IL05 语义负向校验，并通过
raw/zstd 各5类真实单文件源回读及 RustFS 100/1,000 文件 full 检查；证据见
[source correctness checkpoint](../../performance/packed-v3-source-validation-2026-10-03.md)。
这只完成 E0 的有界子集：完整一致 namespace/root/st_blocks、external-large、ACL、
共享预算和观测校准仍待验收。不得把这些 debug correctness 行填入 E1–E4 性能矩阵。

## E1–E3：元数据与动态块的因果消融

### 同BrewFS四格核心矩阵

主实验统一release构建、统一VFS executor、同logical namespace/content/trace、同
group/container分组规则、raw metadata/data codec、inline关闭、相同cache/TTL/并发策略。

| 行 | 元数据 M | frame D | 比较用途 | 当前可执行状态 |
| --- | --- | --- | --- | --- |
| M0D0 | native TiKV metadata | static 1MiB | 共同基准 | 普通native路径存在，但同placement/executor控制未接好：blocked G08,G15 |
| M1D0 | packed-v3 metadata | static 1MiB | I1相对M0D0 | static/inline-off控制未实现：blocked G15 |
| M0D1 | native TiKV metadata | dynamic size-only | I2在native侧；完整interaction | native→相同PackedFrame bridge未实现：blocked G08,G15 |
| M1D1 | packed-v3 metadata | dynamic size-only | 联合配置 | 已有 packed-v3 readonly，但主矩阵要求的inline-off/观测预算仍blocked G06,G07,G15 |

对于同一个D，两种M必须引用相同物理对象/extent/descriptor身份，以剥离metadata表示
变化。descriptor信任与校验要求同等；不能用不校验的native arm比有认证的packed。
所有行的普通native结果仍可作系统基线，但不因此称同executor因果格已实现。

元数据对象数/bytes随M改变、frame/descriptor数量随D改变，这是机制成本，应测量；
不能为了“相同对象数”隐藏它。group/container边界尽可能用预固定逻辑分组保持；若某
布局越过hard cap，只在共同合法语料上作主效应分析，并把packing溢出作为独立成本。

对吞吐Q和耗时T，预注册以下比较：

- I1：M1D0/M0D0，同时给M1D1/M0D1，检验布局是否改变metadata效果。
- I2：M1D1/M1D0，同时给M0D1/M0D0。
- interaction：`log(Q11/Q00) - log(Q10/Q00) - log(Q01/Q00)`；耗时同理但方向相反。
- interaction的方向、原始请求图与所有单格分布一起给出；缺一格就不给interaction结论。

### E1：元数据路径

tree/readdir、已知path随机lookup、stat(path)、open-first-read分别测。保持相同路径深度
和FUSE调用方式；catalog inode probe不等于stat(path)。primary为10k、100KiB独立文件，
metadata warm-off、8MiB显式metadata预算；warm-auto/eager只在两arm支持且记录setup时另测。
单独报告metadata fit/churn：先计算decoded working set，预算8MiB若能放下，不称churn。
需要churn时在single-page合法下调预算或扩大namespace，配置0字节作为明确不保留诊断行。

### E2：frame策略

先在 packed-v3 内比较static 256KiB、1MiB、4MiB、dynamic size-only；确认收益/退化后
才加dynamic size+训练p90。selector使用file size区分类别，p90影响target，不改变类别。
frame target、实际raw长度和stored长度分别记；不能把mount `block_size`当offline frame控制。
固定策略同样满足FD05的class/profile/range边界，尾frame可以短；不靠放宽旧wire实现arm。

主partial trace为4KiB/64KiB/200KiB，偏移包含头、随机、各arm所有frame边界的并集及EOF。
请求集合对所有arm完全相同，不能给每个arm重新生成只跨自己的边界的不同trace。
full-read为顺序/shuffle两种，1 worker给RTT与p99 guard，16 workers为primary。
随机profile是主行，顺序profile作为独立构建；实际测试access pattern固定，避免事后选最优。

p90从训练trace计算、freeze后写入builder provenance；测试trace与训练trace独立。
另外用错配trace评价鲁棒性，不用测试结果反过来修改p90再称泛化收益。

### E3：inline、compression、pipeline/cache的独立作用

主四格raw/inline-off完成后，按单一因素做以下nested对照，不做无界笛卡尔积：

1. fixed D下inline off/on：记录inline上传/下载/retained bytes和metadata churn。
2. fixed M/D/inline下 `(metadata,data)` codec：raw/raw、zstd/raw、raw/zstd、zstd/zstd；
   分别解释metadata压缩和payload压缩。实际raw fallback比例必须披露。
3. 固定对象与raw codec，005 sequential signal/coalescing off/on；zero persistent cache。
4. 固定对象，明确decoded-cache字节预算0/32MiB，第二epoch与cold-start分开。

coordinator、预取、inline、压缩与metadata布局均为独立变量。004的250µs vs1ms属于
旧版本单变量实验，使用同004对象和匹配release binary，不能用005/zstd结果证明250µs收益。
005目前缺pipeline支持；等G09通过后开放其行，不能复用004计数冒充通过。

## E4：overlay workspace的独立贡献与三创新联合链路

### 首先真实双workspace正确性

固定同一个S0 manifest，fork A/B。对A执行create、rename、unlink、hardlink、覆盖写、
partial overwrite、hole、truncate→extend；B和S0应逐字节/namespace不变。
upper full cover时lower metadata/payload GET均为0；partial仅resolve未覆盖区间；
explicit hole填零，Absent继续lower fallback。区分upper read/dirty-write overlay与远端lower。

在resolve/fetch/交付前注入同epoch data_version/sequence变化、head/lower binding/lease变化。
要求typed stale、整份输出丢弃、有限重试，不能只retry某frame形成混合视图。
按CA/container/FD/index/manifest/原子head+binding边界注入崩溃；验证依赖图闭合、幂等恢复。
依赖要求按真实refs的partial order，不强制冷对象必须早于所有container上传。
GC以S0/S1、workspace、lease/journal/活跃reader为roots；验证可达对象不删、orphan可收。

### 生命周期性能行

| 行 | 策略 | 比较意义 |
| --- | --- | --- |
| Wcopy | 对相同一致源执行完整copy/reimport后修改并发布 | 实用端到端基线；计完整复制与导入，不称仅切换overlay的因果arm |
| Wnative | 现有workspace delta + native sealed lower | 现有workspace系统基线；不凭其测试推断packed已接入 |
| Wpacked-static | workspace + authenticated packed-v3 static lower | I3与I1组合；需G10–G13和static控制 |
| Wpacked-dynamic | workspace + authenticated packed-v3 dynamic lower | 三创新完整链路；与Wpacked-static隔离D |

为单独估计I3，再加eager-upper materialization vs lazy-upper两行：相同lower binding、
同executor/dirty-layout/trace，区别仅是在fork时物化全部upper还是按需记录delta。
eager的总复制量计入fork。如果实现时必须改变metadata/数据布局，则报告为strategy/system
comparison，不给overlay单因素结论。不会把逻辑上不可比的八行标为“已完成2×2×2”。

首轮10k S0，1/2 workspaces，修改比例0/1%，4KiB partial overwrite；通过后再扩展
workspaces 8/32、比例0.1%/10%/100%，以及64KiB、增删rename/hole各自场景。
workspace数增长与改动比例扫描分开，避免32个完整copy一次占满disk或扩展云费用。

记录fork-ready p50/p95、copied/private bytes、upper PUT/bytes、lower GET、read/write p95/p99、
dirty-tail、fsync/close/drain、seal/repack CPU/wall/RSS、对象/索引复用、recovery与GC bytes/wall。
replication成本与临时spool磁盘也计入；不只测publication之后的快读。

`T_cycle`用外层时钟测量源视图捕获→fork→修改与durable drain→seal/repack/head CAS→
S1首次完整消费。源导入一次成本、N次fork摊销、GC后储存占用分别给出，不能重复计时
或用并行阶段之和替代实际wall。普通fsync/close不得静默触发repack。

## 数据集、cache与比较协议

| 语料 | 大小与内容 | 用途 / 执行级别 |
| --- | --- | --- |
| C0 correctness | tiny，0B、inline边界、256KiB/1/4/8MiB边界、10/32/>64MiB、稀疏/hardlink/cold/special | E0与E4正确性；性能分母独立 |
| C1 target-small | 10k×100KiB，再10k×1MiB；固定source seed/独立文件 | primary；逻辑约0.954GiB和9.766GiB |
| C2 heterogeneity | 200KiB/512KiB/1MiB/10MiB/32MiB预固定数量；先总量≤10GiB | E2局部读取/selector；真实large/sparse后才扩展 |
| C3 namespace | 相同10k文件，浅树vs单巨目录；之后100k | E1分页/churn/深cookie，path depth单独变化 |
| C4 historical-edge | 4KiB小文件、hotset与shuffled trace | 历史弱点复核，不能替代C1优势证明 |

每个语料记录内容SHA/size/inode身份。entropy分为seeded不可压缩与明确可压缩两类；
现有重复pattern fixture属于可压缩，不能把其zstd行写成普遍bandwidth提升。
不可压缩生成器/真实source trace需G15扩展；oracle和请求trace在arm外freeze。

strict demand-cold：每工具新mount、成功host drop-caches、TTL0、direct IO、keep-cache0，
persistent memory/SSD/window/decoded=0、data prefetch关闭，metadata预算独立显式。
这是本地cache cold，不保证OSS服务端cache cold；记录后端状态，不声称无法控制的cache。
metadata warm-off与warm-on两行，mount/setup和warmup算入wall；inline retention单列。
partial scanner的discovery会遍历namespace并形成metadata warming；不能忽略该阶段称
metadata-cold。主trace从预先生成的路径表开始，或fresh remount之后执行，且hash相同。

cold-pipelined只允许已支持版本的in-flight demand/合法sequential信号，不保留跨request
payload。aligned-window跨request复用另名window-reuse。warm/repeated-epoch用相等字节
预算；epoch2不叫cold。不支持的profile在runner必须拒绝或标blocked，不能静默忽略。

JuiceFS+TiKV为primary外部系统参考，Redis仅补历史/实现相关差异。相同源内容、路径树、
trace、workers、effective attr/entry/open TTL、client缓存、compression与direct-mode语义。
不能假设JuiceFS实际小对象都是4MiB；实际GET/bytes测量。native/TiKV与JuiceFS含server
CPU/RAM/网络；packed列client和object-store资源，禁止只对比client RSS得出总资源结论。
两系统无法严格匹配的cache/direct/ACL语义明确披露，对应结果只作系统profile对照。

已有百万行的TTL与inline口径不完整，保留其数值/错误状态。只有同dataset/trace/toolchain/
cache/codec且原始artifact完整的baseline可复用为匹配arm；否则只补必要10k参考，
不重跑无意义1M。云验证开始时最多一个disposable实例，10k先行，有deadline和storage上限。

## 指标、统计与接受规则

每行至少保存以下指标，未instrumented项为null并给reason，不能伪装成0：

| 指标组 | 必须字段 |
| --- | --- |
| 应用 | successful files/ops、logical bytes、checksum、errors、files/s或IOPS、p50/p95/p99、同trace hash |
| 请求图 | manifest/probe、IP05、GM07、FD05、CA05、inline、data range GET、PUT、实际HTTP attempts/SDK retries、requested/received/failed bytes |
| 放大与CPU | stored/wire、raw decoded与requested union、shared-frame bytes、merge-gap bytes、duplicate fetch、descriptor/name/hot/extent/inline bytes、hash/decode CPU |
| 内存/资源 | mount-wide queue/pinned/stored/raw/decoder/output/retained peak与budget、daemon RSS/PSS、scanner RSS、metadata server内存、spool disk |
| 全程成本 | source/import/build/upload/verify、mount/warmup、discovery、active、fsync/close/drain、unmount、seal/publish/first-consume/GC及outer wall |

全读：`active_BW = successful logical bytes / active_seconds`；
`active_plus_drain_BW = successful logical bytes / (active_seconds + drain_seconds)`；
`end_to_end_BW = successful logical bytes / outer_wall_seconds`。归属与重叠定义存入profile。
含全部metadata/inline的received/logical比值称overall wire ratio；payload wire/logical单列。
raw overfetch用实际解码frame raw bytes减去对应batch内所有请求的union有效区间；同frame
共享给多个consumer不能重复计union，重复下载另计。raw ratio与压缩wire ratio不可互换。
metadata-only以bytes/op和GET/op归一，inline payload传输仍报告，不用logical-data=0相除。

首轮单次只校准正确性/计数；候选先3对随机化顺序paired runs，primary接受行目标7对。
每对trace/seed/环境同一，fresh mount；顺序随机化或ABBA、保存order。
先预注册primary workload/指标，给所有单次行与paired ratios、中位数、方差、paired
bootstrap区间；样本少时区间仅描述，不宣称统计充分。区间宽或噪声大记inconclusive，
先诊断计时/后端竞争，不直接扩成百万files以掩盖噪声。

工程接受阈值预设：primary中位吞吐改善≥5%且paired区间方向支持改善；受影响p95/p99/
metadata/full-read与active+drain不出现未解释>5%回归。若收益落在噪声内，继续作为
功能/架构结果报告，不称性能优化接受。所有read/content/namespace/cleanup错误使行invalid。
共享可写路径修改必须跑randrw/direct/writeback/metadata guards，不将cache关闭或成本移到
close/seal作为胜利。每个接受code arm按AGENTS同迭代完整CI gate，并额外005/overlay/vendor。
现有clippy warnings不等于GitHub `-D warnings` gate已通过。

## 实施批次与runner合同

| 批次 | 产出 | 扩大条件 |
| --- | --- | --- |
| R0 | G01–G04 correctness、G06观测、G07预算与真实tiny mount | 内容/错误/资源/卸载全部通过 |
| R1 | G15控制；packed raw static1MiB vs size-only，100/1k校准→10k C1 partial/full | 放大变化可解释、三对paired稳定、当前CI通过 |
| R2 | G08 bridge，完整四格；另inline/codec诊断 | 每格同placement/executor证据完整 |
| R3 | G10–G13；A/B真实workspace→eager/lazy→Wstatic/Wdynamic全周期 | fence/publish/recovery/GC无错误，成本全部计入 |
| R4 | G16完成release云runner；10k target与必要JuiceFS匹配参考 | 7对代表行通过；资源/成本/latency guard通过 |
| R5 | 有依据选择100k/单巨目录；1M仅用于明确scaling问题 | 前级正确性与收益通过，不重跑既有无效组合 |

当前`tools/perf/run_packed_local.sh`可做100..10k的004/005 debug正确性；
`packed_v3_snapshot_fixture`支持wire/metadata-codec/data-codec，size≤4MiB且无static/inline-off。
`smallfiles_scan.py`和`packed_partial_scan.py`复用内容oracle，但cold discovery问题须处理。
未跟踪`aliyun/run_packed_campaign.py`目前只有dry-run/identity-check控制面，
`packed_campaign_remote.py`是远端执行原型；其存在不等于已完成部署、release provenance或
三端匹配dispatch。100k/6小时原型上限不能当作首轮默认，应由新R4合同约束至10k。

runner实现前先冻结artifact schema：run/profile/source/manifest/binary/vendor/trace SHA，
revision+dirty-source hashes，fixture policy/实际layout摘要，工具链/系统/limits/计时定义，
before-after stats、scanner JSON、stdout/stderr、error samples、resource journal与cleanup结果。
状态区分planned/blocked/invalid/valid-correctness/valid-performance/accepted/rejected。
完整明文命令做凭据脱敏；只保存credential来源类别，不保存值/配置/签名URL。

每次资源创建记durable journal、唯一OSS prefix、owned mount/container/volume/Redis/TiKV
数据域和instance id，设置实例auto-release和外层deadline。正常/失败/超时都只清理本次
owned资源并独立核验为零；referenced artifacts和operator-owned文件保留。cleanup失败
也不能接受性能行。此文设计实验，本轮没有创建云资源或执行任何性能接受campaign。
