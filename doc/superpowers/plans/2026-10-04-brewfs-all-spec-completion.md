# BrewFS 全部 SPEC 持续目标

2026-10-08 收尾复测 diagnostic05 已终态：8项全部执行，5通过、3失败；
source/config/Git不变，自有服务和代理清理及独立absence检查通过。
Redis fork/initial composer/取消后恢复与TiKV两项postsubmission故障通过。
剩余为TiKV fork在Quiesced前Busy，以及Redis/TiKV新borrowed fork首次pin快照Fenced。
对象runtime/admin凭据分离12文件及absence修正已精确接入，尚未编译/真实权限验证。
生产CLI6、operator/中央GC及完整同迭代门禁仍开放，目标继续active。

2026-10-08 最新终态：仅packed-v3；stores05为328通过、0失败、101 ignored；
SDK03为122通过、0失败、1 ignored，均source/Git不变。
真实diagnostic04已执行全部46项：38通过、8失败、0未执行；source/config/Git不变，
自有Redis/PD/TiKV与代理清理及独立absence核验通过。original shutdown18/0、route2/0；
fork0/2、initial composer1/1、headless0/2、native resume11/1、specialized auth0/2。
终态后已接fork目录修复及固定阶段诊断，hot lease/CONTROL hydration与故障契约修复待
统一构建。生产CLI6、operator/权限/中央GC、完整同迭代门禁仍未通过；目标保持active。
下文均按各自历史源码与验证范围保留，不用于签收本轮。

2026-10-08 最新增量：产品/API/配置/文档仅packed-v3。普通stores04终态为
326通过、0失败、99 ignored；carrier/fork/runtime同源diagnostic03真实矩阵
44项已终态，28通过、16失败、0未执行，source/config/Git不变且自有服务与代理
独立清理确认。native上传恢复最新11/1，Redis BuildingEmpty的after-page authority失败；
original shutdown8/10、headless0/2、route1/1、fork0/2。旧native12/0和原attempt12的64项
通过不等于当前生命周期通过。
TiKV透明Lock probe实际0/1，确认真实非空retryable+WriteConflict，不能忽略error。
完整PWA handoff packet、ordinary journal同holder夹具、native-rebind共享预算及
scoped hot-head/PWA发现修正的stores03已终态为323通过、0失败、99 ignored，source/Git不变。
followup04已精确接入严格TiKV冲突处理、typed fork mount/clean publication及真实discovery
竞态回归，原三处格式修正完成。SDK01仅编译类型错误、0测试，最小借用转换修正后SDK02
已终态为119通过、0失败、1 ignored，source/Git不变；stores04已终态326/0。
reason04实际0/1，三次reason2的key/primary/TS全匹配；四类ExecDetailsV2需Lock专用解析。
metadata-followup05六文件已精确接入且fmt/diff通过，尚未编译/真实验收。
详见[当前元数据增量](../../performance/packed-v3-metadata-followup-validation-2026-10-08.md)。
FUSE后端重验待执行；operator中央leader GC独立缺口仍开放。
真实生产CLI/FUSE/operator、发布后原会话完成证明与Pod退出前PVC保留仍须实测，
完整仓库门禁仍开放。
目标保持active；先完成系统正确性与完整门禁，再冻结三创新实验。
最新边界见[carrier/runtime验证](../../performance/packed-v3-carrier-fork-bootstrap-validation-2026-10-08.md)。

2026-10-08 当前状态：仅 packed-v3，继续完成全部必需 SPEC；尚未关闭目标。
当前代码收尾集中在 Redis/TiKV 发布、恢复、reader pins 和 GC。前一轮真实后端
64项为56通过、8失败，均已执行；TiKV Get 锁错误曾阻塞部分验收。
SDK 严格分类和固定 data deadline 已接入，最新普通测试 113/0、1 ignored。
TiKV 有限同事务读取继续逻辑已接入；16 项批量读取测试全通过，同轮 stores
旧夹具修正已通过；initial1 生产及3项精确修正后的完整 stores 回归为
289 通过、0 失败、73 ignored，source/Git 不变。真实 Redis/TiKV focused19
已终态为 12 通过、7 失败、零未执行；source 不变，自有服务全部清理。
针对实际附加耗时字段的严格认证已完成 RED 26/3→SDK 全普通 GREEN 113/0、
1 ignored。Deleting-only initial1 退休与两个真实 Redis/TiKV 回收测试已接入；
全 stores 已通过，真实 focused25 已25通过/0失败/0未执行，source/config/Git 不变，
自有服务及代理独立确认已清理。完整 attempt12 已终态 exit0：26阶段、64通过、
0失败、0未执行；每阶段 source/config/Git 不变，全部日志已逐项绑定，并独立
核对自有 Redis/PD/TiKV 容器 ID 与代理 start ticks 均已消失。
完整同迭代门禁、剩余挂载/生命周期/operator 出口仍需执行。
证据及具体边界见[元数据收尾记录](../../performance/packed-v3-metadata-closeout-validation-2026-10-07.md)。
系统验收通过后才冻结三创新实验；以下记录保持各自历史源码与验收范围。

当前增量：readonly OPEN/OPENDIR Box-only队列在red03编译成功、5controls通过后，
原严格held-final-packet预算断言真实101；complete修复后的green03 session26106
实际exit0，vendor/root编译+19vendor exact+3root exact全过，549输入不变。
RED/GREEN原final-packet文件字节完全一致；独审receipts为
`07008c9f60b3838624cf073bfb6145dbdf5656868a4763f9bdfa073eeaa90d29`及
`730d58aee36ad3b5a3b97ac71cbf433ae46cb635f70de3ddbd341e9ed7d38f68`。
启动preflight red01编译后8controls过、两项现有public API断言真实失败；最小
preflight生产修正及无heap的inline Mutex/get_mut Sync适配后green02 session61400
实际exit0，compile+10exact全过，550输入不变，真实两模式及四partial reserve错误
均在物理startup前退还Roots。其后fmt为另一个源快照，不称旧字节相同。
packed/native真实provider最小预算green02 session57414已实际exit0：compile加4项exact，
551输入不变，真实最小Roots15024128，一字节更少由原validator拒绝。实际provider经
MetaLayer/VFS验证inline grant、read/RELEASE、fullRoots拒绝/无新后端请求及恢复；native
fixture为MemoryKv，不签TiKV或真实挂载。named packed factory布局green01也已exit0：
compile加2项exact，child320/8、holder88/8、outer136/8，原136+304+32=472<=512通过。
两者独审receipt：d46df187c0467d8f1799f9afc07fdab31705547d7f3c98eac0432b72880c162a。
完整gate08 session83027已真实exit1（ad5b5e）：49项中48通过，551输入不变；唯一失败
为vendor io-uring旧unmount peak测试漏记新base536，实际1424/旧预期888。supplement07
未运行。终态后应用并单文件fmt了独审test-only精确适配，新增prepare-time base精确断言，
原production/Roots64KiB及512/304/32与全部后续清理断言保留。focused session21211真实
exit0（94e0bb）：compile+4exact全部通过、551输入不变，含typed failure/public Session
join/原strict held-final-packet；实测512+536+376=1424。原失败gate和integration before/
after/diff及两tool terminal已分别保留。新完整49+串行17 gate09/supplement08在parent36375
已真实结束，chunk09c565/exit0，49/49及17/17全部通过；两轮551输入、配置及环境一致，
canonical manifest为1dea532ee60dca293e4522c19d81a84f386921d76073933119ae66512e3b539f。
tool-terminal36375.json已保存；独审后绑定candidate05，串行冻结新runtime并做fresh normal/
strict live。ignored后端/系统出口不算已验收。仍只签具体子项，v3/005范围不变。
完整mount/native joins、public取消owned closing及全部SPEC/S/X继续OPEN。

66项联合独审已实际exit0（8c880a），receipt984f3fa1179e5b7814e495df5c75915defcb7bf4d1d6e0de376c0062a74ee9c3。
candidate05真实证据绑定exit0（a52bcc），binding SHA8358e66e1cb7dffc9d8782ba4f6d7cf9b4aec02fe1f334f198f7ca11d7f09be7。
新io-uring/tokio runtime在session13798已串行构建完成，真实8850c1/exit0，backup逐文件一致。
fresh io-uring/raw normal在session74317已真实3a3c2d/exit0，参数保持10000 entries/2MiB cache/128MiB RSS。
freeze/backup及normal独审均实际exit0，receipts分别为0a1823d8caf71d2c962278f2c9eb91a226b1730055e9aaaf15a335b8b8e06b47
及f5ce24d78d5d639b0ad47a7e05fb1ceeb282f7abf7bd54a59559153f8864d4f6。
normal17阶段和普通卸载通过；64并发实际仅1成功8192B、63 ENOMEM12，不称64成功。
wrapper04真实20offline及新seal也均exit0，identity02为8d03399dde9df69d6bd990f544c550ba433fe5252c9182424103259a2c389050。
原严格pending-body SIGTERM在session74676真实845bbd/exit1：concurrent阶段worker smaps采样ESRCH3、
同Popen poll尚无终态，memory_check在发送SIGTERM前拒绝；没有观察生产shutdown行为。
失败清理daemon0/mount消失/无survivors/errors不算严格GREEN，原失败完整保留。
仅新run02/work02路径、同sealed程序/断言/errno的一次retry在session75674运行。
production source保持551输入未变，sampler竞态、full SPEC/final Roots继续OPEN。

上述retry75674也已真实c66c92/exit1，在SIGTERM前同memory gate再次采样ESRCH3
（/proc/1129999/smaps_rollup，label cache-refetch）；不继续同版重试。
run01 pid1128845实际为post-pressure-read-before snapshot worker，concurrent仅旧stage标签；
Popen回收锁竞争与exit-mm先于waitable两种原因仍未区分，需有界同owner终态确认修复。
全部Cargo/FUSE结束后root以apply_patch接入独审G13十项tests-only并fmt，六路径delta与before/
after/diff已保存，其他551基线输入完全不变；red prepare真实51e3d0/exit0，新增3文件为554输入。
freeze receipt为f55daab064de60c96224caf8ce38b8c5f11398060de7cb0952d7490e6fb5e471，
真实red-observe session5252已944fb6/exit0：compile、十项actual exact1，5个原语义panic101与5controls pass。
随后仅database/kv两个生产文件apply+fmt，其余552输入及全部测试字节不变；GREEN session3844
真实d6fdd7/exit0，compile+10exact全通过，554输入不变。完整联合独审也已36c21b/exit0，
receipt为6f58f599d07ef60bb7edbb911b25eb10f965b58d7f1e7b587c1975946b0ccb9a，原tests/config/env/git/argv相同。
新完整gate10+串行supp09在parent1330运行，554 canonical source为
335b8d51333e6cfcf19cddda8c1c8ff487d29ccc2f8cb2b4eb532824e25c01ab，未到终态不签完整门禁。
详见[GC验证报告](../../performance/packed-v3-gc-grace-fork-validation-2026-10-06.md)。
仅catalog native子项focused GREEN，不关闭packed graphGC、真实后端、CONTROL总量预算与全部SPEC/S/X。

最新批次（2026-10-05）：完整FS/reply-failure race一Box修复后session57703实际exit0，
compile及3exact全部通过、543输入不变；原失败green01保留。联合独审receipt为
`7aa3678cfabcf471f2ca874792f95cde844251ffd6c8951d745ca1345dc9b0b9`。
fmt后的session5414实际exit0：compile+29exact通过、543输入不变，实际PackedVFS
同生产factory布局child320/8、holder24/8、outer152/8，152+304+32=488<=512，
ordinary outer仍6352/8。Stage A session5507编译后两项现有API真实RED已保存；
current29/layout独审实际exit0，580文件/543savedsource不变；receipt为
`094a4e81e90d6fb4a699a27a0cbb4b914bf6cd81d43a11f77c8a58ee22e29aa3`。
最小生产修复与显式旧typed control预期适配后session49566实际exit0，compile及
5exact通过，543同源输入。失败prepare会drop后notify并按引用await owned Session，
原errno优先于次级EIO/panic、ordinary helper为0；只签fixture/runtime task等待子项。
Stage A完整独审实际exit0，RED/GREEN各556文件及543源码不变；receipt为
`fccc73e3c6bb9c7b660da0d1e04333986c824b2fe07119157a9b78cae5986a27`。
完整gate07会话48429已终态exit1，49项中46通过、3失败，543冻结输入不变：
overlay/all-features同一新Roots断言失败，strict overlay clippy为测试tuple复杂度。
failed gate/两项修正独审receipt为
`4f759dc114c5a8d16d082470f8c8e50b5ae51ad9d493d488ba9834119973f348`。
test-only生命周期修正和等价展开类型别名后session32927实际exit0，compile、3exact
及strict overlay clippy全过，543输入不变。实测idle8519944、file_client200、
initialized_mount10699016；真实RELEASE精确退client，成功MetaLayer::shutdown_session
再退lazy mount-owned coordinator/registry至idle。原Control32768/Metadata64MiB、
完整stats/cancel/recovery断言不放宽。supplement06未对失败源码运行；下一生产定向
批次稳定后再跑新完整49+串行17，旧门禁与失败gate07都不签当前runtime/live。
永久客户端deadline、取消cleanup ownership、startup rollback、完整reply/native joins、
全部stored-future/私有队列预算与final Roots/allocator窗口继续开放；G10–G14/S/X及
最终三创新实验开放。当前范围仅v3/005。以下checkpoint保持原历史时点。

当前未验收源码批次（2026-10-05）：typed client-close/关闭READ errno已集成，
session27719以539同源输入完成compile0、15控制GREEN及commit-failure wake原断言RED。
最小notify修复后的session63100实际exit0：541同源输入、16控制全部GREEN，
完整ordinary worker future实测6352B/align8、原prepare future136B；10个fh耗尽
测试实际8失败、generic stats和packed dir两个控制通过。后者不证明零临时admission。
新增真实Metadata满池目录顺序测试在session96555编译后实际RED：句柄耗尽却先
返回out of memory，rejections0→1；542输入无漂移。原fh-first生产候选已静审通过
并在这些RED保存后应用，session96031实际exit0：compile和同断言28项GREEN全过，
542同源输入，实际大小仍6352B/136B。随后session25739的真实socket EPIPE/等待中
FS prepare测试编译0、typed public failure控制0，原wakeup断言实际RED；生产修复
仍外部待独审。此批仍未完成新完整门禁/实挂载验收，全部SPEC/S/X及实验campaign
开放，旧门禁仅对应其旧冻结源码。

当前门禁终态：gate06会话9145和supplement05会话72354均exit0，49+17全部通过，
535输入canonical SHA256为`e40db44494c120ef56676fc93f9aa49a7961dcffeb7fa195da23a5263fa3a763`。
独立合并复核exit0，原52命令完整覆盖且633审核输入前后不变。candidate03绑定通过，
新两runtime冻结会话23655运行中；fresh normal/strict SIGTERM尚未运行。
replyGroup/native join/client-close fence候选仅在外部准备，未接active/未编译。
全SPEC/S/X仍OPEN；同源门禁不是客户端RELEASE或原生线程完整join证明。

当前源码批次见
[reply retirement 验证](../../performance/packed-v3-reply-retirement-validation-2026-10-05.md)：
两项ownership、两项closed/discarded-response、两项真实wire close-admission均已实际
compile→预期RED→GREEN。新socket终态回归io-uring60/Tokio28通过，async-io编译通过；
实际VFS preparation大小136B在原512Roots内。完整gate06运行中，supplement05待串行，
全reply/native join、真实pending-body SIGTERM及全部SPEC/S/X仍OPEN。下段为已冻结前驱。

最新 checkpoint 已更新为
[Plans admission 验证](../../performance/packed-v3-plans-admission-validation-2026-10-05.md)：
gate04/supplement03 的49+17项及两runtime冻结通过，532输入一致；fresh挂载01被9p拒绝，
改Linux目录后的normal02首个inline read为ENOMEM，两次失败保留且清理无残留。
4项现有API真实RED已复现Plans满池自耗尽；两入口最小修复及strict4MiB/minimum2MiB
回归已落地，6项exact GREEN全部通过。新完整gate05会话49068已exit0，49项全部通过、
532输入不变且独立终态核验通过。supplement04会话88026已exit0，17项全通过，
两门同源同环境并有独立pair核验。新binding与runtime冻结会话49797均exit0，
新产物与保护副本放C且独审通过。首io-uring/raw normal会话48562已exit0，原17阶段、
物理eviction/refetch/内存/普通卸载通过，完整保护58318已exit0，独立行为复核已exit0。
随后strict pending-body SIGTERM会话55988 exit1：read已cancel记录后普通卸载EBUSY、
daemon exit1；worker最终零字节/ENODEV失败。清理释放hold/外部普通卸载只属失败清理，
无残留；evidence及独立Linux work完整保护33191已exit0。退出生产修复与其余矩阵开放。
下述旧checkpoint保留其历史范围；G10/G12–G14及S/X、最终实验与全部SPEC出口仍开放。

最新未验收整合与失败记录见本文件末尾的 2026-10-05 checkpoint；历史批次绿门禁
只证明当时冻结源码。当前唯一格式为 v3/005、PM11/BP11/FD06/IP06。

本轮补充：旧 normal-diag02 已定位到 wrapper 退出 worker 的 smaps_rollup ESRCH；
严格 owned-Popen sampler 保留原断言并只在同一 child.poll 证明退出后省略读取。
normal-sampler03/coldpressure04 均仍因同对象/range eviction-refetch 未证明而失败。
认证 IP06 的目标页 payload 下界254704B，大于原131072B缓存，原warm前提不成立；
新 fresh 目标/2MiB探针与硬cache owner许可候选分开验收，均不借旧失败签收。
取消 sampler01 终态已消费、完整D保护且无survivor：logical delivered=0、terminal=1，
原日志确有unique512 READ→INTERRUPT→cancel，但旧regex吞掉namespace后的event。
v9新parser真实trace等6项验证通过（旧5项red），85条原assert AST全部相同；旧记录不改。
6个fresh取消case已终态：四runtime/codec及logged通过；Control32768在实际body inflight
期间打开完整.stats返回ENOMEM而失败。正常清理无残留，容量/完整观测断言不降低。
v10另补missing/非法u64、namespace嵌入、取消release flags边界，11项green/85assert保持。
handle8436已exit0，独立核验前5项真实日志、D保护、stats/内存及cleanup通过；最小Control
必须生产修复。新freshstrict01有效geometry/2MiB压力仍未证明same-identity refetch；
严格body-hold SIGTERM probe复现普通unmount EBUSY、daemon exit1而mount/reader仍活。
两者均完整保留并正常失败清理，不能以cleanup签收正常退出。root已接retention stage01
现有API测试，handle79491真实编译中；生产缓存owner/32KiB并发stats/卸载顺序修复未验收。
G05/G06/G07及G10–G14仍须生产修复、真实red→green、后端/生命周期与新完整gate；S/X开放。

最新完整门禁为 `final-gate07`，412 输入 SHA256
`64f809eb7d27f8c882c563bf76a6f8831412c2f1474ca0da4b43f820132ad8ef`，52项全部通过。
独立 verification 已核对全部冻结源码、D保护日志与精确命令；无 Cargo/target 进程残留。
gate05 的 overlay 4 项真实失败已保留并经两处测试修正、定向验证通过；gate06 因
覆盖遗漏主动停止，不能记作行为失败或验收。详见本文件末尾补充及最新 handoff。

用户最新范围修正为“只保留 v3，不需要给 v2 等做兼容”。这是对“完成所有 SPEC”
目标的收敛：完成 v3 的全部必需实现与验收，以及三创新所依赖的 workspace/operator
生命周期和大目录契约。v2 独立产品、v1/v2/旧 wire 兼容与旧 flat/CR 兼容不再是出口。
当前以 v3 wire 005 为实现入口；新字段使用明确 payload 版本，但无需维护历史解码路径。
历史实现与证据保留原时间范围，不据此投入兼容工作或宣称 v3 完成。
持续工作已按用户“请继续”恢复；只有当前范围的实现、验收和交付完成后才关闭目标。

## 范围与独立出口

| 规范 | 必须关闭的出口 | 当前可引用的证据 / 未完成边界 |
| --- | --- | --- |
| [workspace-overlay implementation](../specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md) | 适用于 v3 的生命周期 DoD：Redis/TiKV 独立实例 CAS、真实 sibling mounts、POSIX、fencing、seal/commit crash、reachability GC | native 生命周期已有代码与库测试；需接 v3 binding/fallback/publication；旧 default-flat 兼容不再验收 |
| [operator lifecycle](../specs/2026-09-03-brewfs-workspace-operator-lifecycle-spec.md) | v3 workspace/mount/snapshot、权限/lease/finalizer/audit、Redis/TiKV Kubernetes E2E 和文件系统回归 | 必需 v3 capability/binding/conditions 与后端证据开放；旧 flat CR 兼容移出范围 |
| [large-directory](../specs/2026-09-22-brewfs-packed-metadata-large-directory.md) | v3 restart/raw names、union/collision、稳定 cookie eviction/refetch、hot protection、活动 inode生命周期和有界扫描 | IP06 prefix rank/weighted select 已实现，库 GET-budget 与历史36k分页/重启子项已有证据；真实 cookie eviction/refetch、typed GET、hot protection、更大规模与完整内存出口仍开放 |
| [packed v3 readonly smallfiles](../specs/2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md) | §12、P0–P6 与 G01–G17 中 v3 全部要求；source/POSIX/sparse/large、认证、metrics/budget、同 executor、pipeline、binding/fence/publication/recovery/GC、布局控制/runner/性能与交付 | bounded namespace和external已签收；完整POSIX/frozen view、读取预算与mutable packed继续开放；历史 wire 兼容不再必需 |

v2 的 archive adapters、Data Seal、独立 WAL/session、multipart resume 和 v2 FUSE/规模
出口移出目标；v3 自身的一致 source、持久 publication/recovery 和预算要求仍必须完成。

## 继续实施的依赖

1. G02–G04：认证 source root/blocks → bounded namespace inventory → raw POSIX/cold/hardlink
   策略 → external-large producer/reader。源一致性声明及拒绝边界必须可执行。
2. G05–G09：深分页 → 完整请求图 → mount-wide permits 生命周期 → native/packed 同 executor
   → 005 singleflight/coalescing/cancellation。不能只给缓存 capacity 作为预算证据。
3. G10–G14：独立 binding → Absent/Hole fallback → mutation fence/整体重试 → journal/原子
   head+binding publication/recovery → reachability GC → operator capability/lifecycle。
4. v3 真实 POSIX/后端/故障/大目录出口，不再增加 v2 producer 或兼容桥。
5. G15/G16 v3 构建控制与可复现运行设施。
6. 系统出口通过后冻结最终实验包，再完成 matched 消融、性能接受/拒绝与 G17 commit/push。

每个 Rust 批次保存初始源码哈希、行为 red、green、同迭代 AGENTS gate、相关实挂载
和清理证据。额外 all-feature/operator/严格 CI 结果单列，不能把最小 gate 写成完整 CI。
既有 `.claude/`、vendor/005/runner 与其他未提交工作保留，产物不代替源码提交。

## 2026-10-04 source-stat 批次

已复现实际错误：8 MiB sparse source 的 `st_blocks=8`，旧 adapter 返回 16,384。
artifact 为 `docker/compose-xfstests/artifacts/packed-v3-completion-20261004-source-stat/`。

本批使用独立 PM08 payload 与 `BRFSI005/IP05/SI05` allocation root，不修改 PM07/004
解释。根 `RA05` 属性进入 manifest；每个非根 inode 的 block record 必须闭合到该
snapshot 的 inode set，missing/orphan/conflicting alias 均拒绝。readonly getattr/open/
lookup 两种名称接口统一读认证 blocks；single-file fixture 同时捕获 source parent
属性与 cold xattrs，并在 finish 前后重新验证 fd/path token。

这仍是 **single regular-file dentry + source parent attributes**，不是完整 inventory
或原子目录快照；nlink 与子树/完整 namespace 策略、特殊 inode、ACL 应用和 external-large
继续开放。初次定向行为 red 已保存，packed 定向 green 为122 passed；完整 gate 与
十次真实源挂载复测已通过：40项最终本地checks、122 packed、1,305 overlay lib通过，
10次FUSE零错误/正常卸载。原fixture build私有模块引用失败保留且单列修正后重建。
具体证据与属性覆盖边界见
[source-stat报告](../../performance/packed-v3-source-stat-validation-2026-10-04.md)。
本批只签收root/allocated-block子项，不关闭 G02/G03/G04。

## 目标状态

最新已签收目录批次见[namespace报告](../../performance/packed-v3-namespace-validation-2026-10-04.md)：
PM08 bounded namespace/root/blocks、raw paths、special/rdev、树内nlink/reject-external；
九类EROFS及record_open/rename/link修复。最终40项checks、overlay lib1,313 passed、
raw/zstd各540 entries实挂载通过，源码/binary身份与清理已核对。G02/G04只关闭这些子项。
后续raw xattr/.stats/removexattr四项red已修复并独立签收：
[名称边界报告](../../performance/packed-v3-namespace-posix-validation-2026-10-04.md)，
最终40项checks、overlay1,316/default lib-bin1,016/1,100/fixture3/vendor13通过，
raw/zstd名称与540-entry回归共四次FUSE通过，身份/清理已核对。ACL、其他raw mutation、
frozen-view/PATH_MAX与external-large当时仍开放，external已由下面的独立批次签收。

最新G03验收见[external报告](../../performance/packed-v3-external-validation-2026-10-04.md)：
PM09 required selectors、磁盘source runs、分页extents、bounded LD05 chunks、认证范围回读
与跨目录hardlinks已接通；40项最终checks、overlay1,328/default1,016/1,100、fixture3/
vendor13、6项CLI与raw/zstd各525-entry外部挂载及540-entry兼容回归全部通过。
source集合360文件，新增4/变更10/删除0；binary身份与正常卸载/资源核对完成。
G03签收，但不关闭G02/G04/G05–G17或五份SPEC独立出口。下一批ACL已经复现named-user
grant拒绝及POSIX ACL query不支持，artifact为`packed-v3-completion-20261004-acl/grant-red02/`，
其binary与G03最终验收相同；尚无ACL修复签收。外部版本契约见
[实施契约](2026-10-04-packed-v3-external-placement-plan.md)。

ACL、typed-stat、PM10/IP06加权分页与v3-only外部入口已接入下一集成候选，完整45项
门禁全部通过，ACL/namespace/external/hot故障raw/zstd共8次实挂载通过；36k导入
600秒超时，仍待有界游标修复及重验，见
[候选验证](../../performance/packed-v3-pagination-errno-validation-2026-10-04.md)。
旧PM与旧索引不再回读；源stat历史记录仅描述其当时版本，不要求保留其reader兼容。
尚未解释的20秒卸载超时保留为开放问题，后续成功retry不代替根因修复。

持续实现工作已恢复；S/X 与当前v3规范完成出口均未签收。
目前不执行最终性能 campaign，也不宣称系统已完善或所有 SPEC 已完成。

最新源库存事务批次已签收，见[源库存checkpoint](../../performance/packed-v3-source-batch-validation-2026-10-04.md)：
366源码相对spool批次仅生产`source_namespace.rs`与测试fixture`source_layout.rs`变化；
修正fixture后47项完整gate02通过，default1,023/1,107、overlay1,364、native packed190。
两生产binary逐字节一致，统一核对本批14次真实FUSE及重跑的3项Btrfs。相同36k期限内
raw/zstd各36,001 root entries/72 groups完整导入、分页cookie、active fd、fresh restart和
20秒正常卸载通过；另两个.stats检查是小语料。G05关闭此规模/库存子项，actual typed
GET/真实eviction与更大规模/内存仍开放，原teardown根因也开放。用户授权磁盘缓存清理后
环境恢复；旧三次失败work在首次恢复检查时已不存在，持久日志/诊断保留，新checkpoint
明确`failed_work_retention_complete=false`，不能再宣称原失败数据库仍保留。
G06/G07、可写ACL/G08候选已持久保护但未active签收；G09–G17与S/X继续必需。

下一 observer/budget/native 读取批次已接入活动源码，但仍未签收。完整 gate01发现
默认构建的中立 reply owner 被放入可选 workspace 模块；已移至常编译的 reader，
保留数据先释放、预算 owner 后释放的顺序。gate02默认构建和34项checks通过后，
默认 bin tests又发现两项packed测试缺少feature限定；该失败与冻结源码保留，owned
controller/Cargo已精确停止且无survivors。现补准确测试限定，native/default测试保留，
gate03按363文件manifest运行，SHA256为
`6faee2f71dc35fe474db5f289720b89fe7026accc405d3fdfab5f67c951a1938`。
gate03现已49项exit0，363文件逐哈希一致，verified D备份完成。default lib/bin为
1,051/1,135，overlay1,408，native packed202，fixture7，typed-native/observer1,366，
readonly compile-fail3。原vendor仅runtime gate为31项；另外按生产buffer-pool/file-lock/
unprivileged feature实跑io-uring33、Tokio19及显式kernel15均通过，不能混用两种计数。
Tokio/io-uring显式生产程序已固定，实际compiler-artifact features/Cargo JSON/source
与49项gate绑定。仍不能将本地绿门禁记成G06/G07完成。

首次实际io-uring/raw挂载10,066源entry/10,067 inode、21 group/64 frame；manifest-only
startup及正常失败卸载通过，但.stats worker错误要求open快照长度等于fstat fresh inode
长度，验证停止。精确WSL6.18.33.2 kernel源码的fuse_getattr传NULL file，解释该差异；
故障work/log保留。draft-v3记录inode尺寸、改为同FD两种分块完整读到EOF逐字节相同，
无生产改动；全新raw run02已通过上述串行/held检查，但16并发出现实际EIO，尚未签收。
两次完整失败work/D逐文件保护及正常清理已核对。取消/errno批次实际编译后证明5项
adapter行为red、2项LoggingFileSystem真实worker/control red及1项Admission计数red。
当前14文件/366 pinned输入的green03通过6 adapter、3registry、4实际worker/control，
fmt/diff通过；manifest为`3ed951c111affab5e0c2081dbdd8543222f1fc6e55ddd93cf4c58be7193e6557`。
完整新源码gate/FUSE、关闭未完成readonly任务、有界拒绝reply及minimum Control仍待验收。
详见[本轮进度证据](../../performance/packed-v3-observer-cancel-progress-2026-10-04.md)。

有限最大observer快照、4MiB正常read与两个1MiB reply allowance要求Output至少
31,902,976 bytes，default32MiB够用；旧30,854,400计算少算一个allowance，不能沿用。
独立kernel恢复证据只覆盖满足SUBMIT_STABLE、无polling及每tag quiescence前提的
释放；永久wedged kernel同时无CQE与无proof仍开放。真实物理GET图、实际cache eviction、
慢consumer、真正FUSE取消、较大规模RSS与正常/故障卸载仍需独立实测。

可写G04最终24文件green/red与6文件修复delta、G08七文件同D桥与三文件共享executor
delta均已冻结和D保护，仅静态验证。下一批以最新common按hunk接入，再做真实red→green、
完整gate及Redis/TiKV/FUSE/CAS/lease/GC生命周期，不以旧独立绿日志代替新候选验收。
系统S及实验X仍未通过；不开始最终性能campaign，不提交或推送未验收整合。

最新root-FD/Btrfs冻结源与六项SQL seek批次已通过46项完整gate、8次raw/zstd真实FUSE、
3项固定库binary真实Btrfs测试，366源码及固定binary/cleanup已核对。深路径5,474bytes
可实际回读；snapshot guard撤销（含manifest已上传后）拒绝可信ref。两次36k导入仍
600秒超时，未进入挂载，下一批补有界spool事务与readonly内部v3-only收口。详情见
[冻结源/seek报告](../../performance/packed-v3-frozen-source-seek-validation-2026-10-04.md)。

最新readonly内部v3-only和有界producer spool事务批次已通过47项完整gate、8次真实
FUSE及3项真实Btrfs；366源码仅4文件变化，固定binary/cleanup已核对，见
[spool/readonly报告](../../performance/packed-v3-spool-readonly-validation-2026-10-04.md)。
第三次36k仍600秒未挂载：source完整36,003但只分配29,571编号，producer尚未开始。
下一批补source库存有界事务；三次失败证据全部保留，G05与S/X继续开放。

## 2026-10-05 集成 checkpoint（尚未验收）

最新 `integration-focused02` 七步全部通过。376 个源码输入 manifest SHA256 为
`b743bfa6bdc5e599cf913200396e5fdbd4b445394af235df6db83fe84d0ca5a1`；原
cancel/minimum-Control fixture 全文件未改。existing API lib 3/bin 4、G15 lib 6/fixture
1、adapter 10、registry 5、packed wire005 132（3 ignored）、可写 ACL 23、vendor lifecycle
10 通过。另 production vendor io-uring 49（15 ignored）/Tokio 26 和 Native matched
plan/data/path 单测通过。源码/命令/日志/D 备份已逐哈希核验；不等同于实挂载验收。

新增既有 API 门禁先编译后真实复现 32 KiB capability 拒绝、cancelled ticket 提前释放
名额。现以最后 Flight owner 退休名额，固定结构预付 Roots、共享 key Arc 与 moved
recipe 独立 Plans owner、source/execution 持有完整 Plans 费用。worker 最多持有 8 个
collection future，逐真实 Job 增长 Vec，shutdown/drop/join 后才报告完成。32 KiB 强
读取保持原预算并通过；waiter/request/final-frame/receipt 固定 Control 费用保持。

`final-gate05` 52 项同源完整门禁进行中，378 输入 manifest SHA256 为
`229d1e14e329e0e542bbaca7c0a49f225cc9017046206c14b0c084b1dcb85beb`。
gate04 在执行前因遗漏 cbindgen.toml 被身份预检查拒绝；新 freeze 补齐并保留旧清单。
v7 十个 harness 文件已逐字节接入，但显式两 runtime build 与真实 FUSE 尚未执行。
下面保留 focused01 失败及原边界，不据此覆盖它的失败日志。

可写 ACL/G08/G09/G15 的 52 文件组合和 readonly worker/control 生命周期已接入活动
源码，不再仅为私人草稿。继续只支持当前 v3/005，格式是 PM11/BP11/FD06/IP06。
本批保留十二个独立行为 red（layout/CLI 2、minimum Control 1、vendor 5、existing
API 4）；两个双 body 测试在第二响应进入前超时，不能计作 destroy 行为 red。

`packed-v3-completion-20261004-integrated-contracts/root-validation/integration-focused01/`
七步均已终态且源码前后相同。existing API lib 3/bin 4、G15 lib 6/fixture 1、registry
5、可写 ACL 23 均通过。adapter 8 通过/2 失败；packed wire005 116 通过/9 失败/3 ignored；
vendor 是两处 fixture 缺字段导致编译失败，没有新的 vendor 行为验收。

修复顺序：保留 32 KiB 强预算，依据真实结构分配消除重复所有权；验证 pending body
不会阻塞独立 collection；更新 PM11 唯一格式测试并继续拒绝旧 payload；对 lazy
coordinator 执行真实 shutdown/join 后验证零 owner；Native fixture 转发真实原子
create-only；补 vendor fixture observer 字段。每个新 stage 独立保留，禁止覆盖失败日志。

v7 live harness 已冻结，仅 30 项静态/合成 checks；待 focused 通过后冻结新的 52 项
同源完整 gate，再构建 Tokio/io-uring 两显式程序。四 runtime/codec 组合、真实取消、
logging、最低 Control、故障卸载均未签收。G10–G14 三态 fallback、Expired recovery
grace GC roots、真实 CleanReleased/drain 等差距继续开放；Redis/TiKV/K8s 出口未完成。
系统 S、实验 X、最终 campaign、commit/push 均未完成。

本轮磁盘清理净增 C: 10.65 GiB，复核可用 54.20 GiB；仅删除闲置 benchmark Rust
依赖缓存及旧 shader cache。源码、活动 target、凭据、证据、程序、Docker/实验卷
身份保持。记录在 `D:/Codex-Recovery/brewfs-root-20261004/cleanup-second-request-20261004/`。

### 2026-10-05 格式测试修正及完整输入预检

gate05 终态为前35检查通过、overlay lib 1485通过/4失败/231 ignored。三个失败
来自 readonly 共用 fixture 的旧 PM10 断言；另一个是未认证 p90 hint 被当前 packer
正确拒绝。修正只涉及 readonly/catalog 两个测试文件；所有5尺寸×2profile 正常
frame/extent/inline/codec/offset/逐字节输出检查保持。未认证 hint 明确要求
UnsupportedFormat；可信历史 histogram 正向能力未因本修正签收。
`integration-format-corrections01` 的 readonly 5、corpus 1、fmt/diff 均通过，378 输入
SHA256 `2c0b86398dc0212edad690b67a61c513619b01c107e671786c04be159748efa0`；
源码和 D 逐文件保护一致。此定向结果仍不替代完整门禁。

gate06 主动停止时前33检查通过，下一 runtime check 为 -15；controller、Cargo
及其 owned groups 均终止。原因是原清单漏 tools/stats 两输入与5个 include_bytes
fixture，不是 Rust 行为 red。静态覆盖审计进一步固定27个实际 gate shell/Python
与 helper；原 gate05/06 不可通过事后补哈希升级为完整同源验收。

gate07 正式绑定412输入，执行前通过原 v7 builder.checked_sources、额外381 build
输入/27 runner输入、精确52命令和 source binding 检查。没有删除或降低任何断言，
不再强改所有未变化源码时间戳。D 记录在 `full-gate07/`，两 runtime build 与 v7
实 FUSE 尚未执行。G10a三态、G13a Expired grace、G13b fork/GC phantom、G14a retention
均仅私人候选/静审，需逐项真实 red→green 和后端出口。S/X、最终 campaign 与交付
仍开放。后续 free-block trim 未删除文件、未关闭 WSL；仅观测 host增约77MiB，
不能把 Linux 的15.9GiB trimmed 数字当作 Windows 净释放量。

gate07 已终态 exit0，52/52检查通过；独立 `full-gate07/verification.json` 核对412
源码及D副本、381 build/27 runner覆盖、全部日志和精确52命令，无Cargo/target残留。
实际 overlay lib 1489通过/231忽略，typed native/observer 1447通过/231忽略，default
bin 1147通过/225忽略。真实FUSE、系统S及全部SPEC仍未完成。门禁后再次trim不删文件、
不关闭WSL；host净增约55MiB，C可用48.15GiB。后续runtime两套冻结及保护副本均放D盘，
活动target、实验卷和全部失败证据保留。

两种显式runtime已构建并冻结于D盘，完整保护副本逐文件一致。首个v7真实挂载
`io-uring-raw-normal-01` 在17个记录操作stage完成后，于runner:465的sampler检查
失败；只有6条RSS/PSS样本，原runner未落盘sample_error具体原因，因此不能认定
已证明采样退出竞态，更不能签收memory/normal FUSE。矩阵已在此停止；完整work、
源码输入、对象、日志和终态均保留到D。挂载、daemon、owned groups、sampler、oracle
均结束，无清理错误。下一步使用独立诊断wrapper保留具体异常，原v7强断言不降低。

磁盘再清理旧benchmark target的1,255个>24小时非执行普通`.o`/`.a`，Linux释放
2.17GiB，C盘实测净增1.73GiB，当前可用43.59GiB。607个保护程序与412个冻结源码
hash均不变；active target、实验卷、Docker、凭据/.claude及所有证据保留。详细D记录
为 `cleanup-idle-objects-20261005/`，未关闭WSL。

## 2026-10-05 v3 lifecycle checkpoint（候选，尚未验收）

当前范围仍为 v3-only / 005 / PM11 / BP11 / FD06 / IP06。以下基础 API 的存在
不等于 G10d/G10e/G10f 或系统 S/X 完成，不能据此开放 packed workspace mount。

- SQLite 的 ws_v3_workspace_open 和 open/ready/renew/close API 记录
  owner/generation/expiry 与 Recovering/Ready。open 不再隐式建表；在同一事务
  校验必需表、volume header、workspace、head/祖先链及 current PWB3 身份，Ready
  要求固定两层。同 owner 重开重新检查 seal；history 有而 current 缺失、current
  无目标或损坏均拒绝。token 目前只约束 sidecar API，尚未成为 mount、mutation、
  hash/commit/abort recovery 的 ownership fence。必需表检查仅验证表存在，不代表
  已验证任意重建表的完整 schema 形状或约束。
- KV open/ready 先读取 workspace 路由，再以固定 CONTROL/workspace/head/sidecar
  键执行 consistent timed read 和 CAS；renew/close 读取单一 sidecar 键。候选新增
  owner 256-byte、单记录 4096-byte、ControlState 4-MiB 解码限制，拒绝 trailing
  bytes，校验 sidecar key/record 身份，并在后端 CAS 时检查 expiry deadline。
  这不是完整的 bounded open：网络取值仍先于解码限额，ControlState 仍为全量
  control blob；base/current PWB3、实际 recovery 与 mount 尚未完整接入。
- Redis/TiKV/Memory 的 scan_prefix_bounded 在后端限行；trait 默认返回
  UnsupportedCapability，不再全量扫描后截断。get_extent_deltas_bounded 先检查
  原始 sentinel，再解码/过滤，validate_read_fence 读取 workspace/head/lease/两层
  的同一版本。新增 extent 校验在 overlap 过滤前执行：拒绝零长度、logical/slice
  overflow，以及 layer/inode/chunk/sequence 和规范 key 身份不一致；不能依赖
  resolver 的后置校验，因为它会先过滤错误身份。范围索引、传输字节界限和完整
  G10f 仍缺；整个 chunk prefix 限行可能保守拒绝仅有少量 overlap 的查询。

本轮 packed readdir/list_xattr/lower-only record_open 草稿已精确撤回：目录合并仍
全量 materialize、权限/open counts 与 generation fence 不完整，不能接受。相关
路径继续 Unsupported/fail closed，supports_packed_workspace_mount() 继续 false。

历史定向证据为 SQLite stores 12 passed、KV stores 11 passed/2 ignored（真实
Redis/TiKV），fixed-key sidecar 2 passed，bounded extent/read fence 各 1 passed。
这些日志早于最新 schema、codec、expiry 和 same-owner reopen 修改，只作历史记录，
不得作为最新候选的 green。最新验证单独保存在
`D:/Codex-Recovery/brewfs-root-20261004/v3-sidecar-validation01/`，终态后追加实际结果。

修复后同源 `local-gate/stores.log` 为 59 passed/7 ignored（真实后端）；覆盖本批
extent identity/overflow/sentinel、SQLite partial schema/marker/view/binding、
SQLite 严格 sidecar 解码和 expiry rollback、KV deadline/codec/same-owner reopen。
SQLite 四 API 共享严格 decoder；提交前取时跨 deadline 时显式 rollback，再返回
Fenced。open 新/takeover 取新 expiry，live reopen/renew 取旧、新较小值，ready/close
取当前 expiry。此定向 green 仍不代替完整门禁与真实后端/FUSE 生命周期验证。

最新 TDD 行为证据：`extent-identity-red.log` 实际 1 failed，复现 head key 的
payload 被改成 base layer 后仍返回；`v3-sidecar-expiry-red01/sqlite-sidecar-red.log`
实际 1 passed/12 failed，分别复现 5 类坏 sidecar 记录接受及 7 类提交前跨 expiry
仍写入。日志保留，不以随后修复覆盖。SQLite 提交前 deadline 重验的候选只能证明
SQL staging 跨过 expiry 时回滚，不能表述为 durable commit 时刻的原子 deadline
fencing；完整恢复 owner fence 仍是独立未完成要求。

本轮最少门禁已终态：18 检查中 17 通过，overlay 严格 clippy exit101，不能签收
完整 CI。默认 workspace lib/bin 为 1063/1147 passed，各 225 ignored；overlay
lib/bin/fixture 为 1567/1658/8 passed，lib/bin 各 235 ignored，均 0 failed。
固定 482 输入的 manifest SHA256 为
`56c78c0dd34e643d290cba64d7da774654b0abeb9d3f518422b7f11593a72189`，逐阶段/终态
无源变化。该范围不是全部 CI；all-features/vendor/operator/真实后端等仍未执行。
随后仅 KV 测试辅助代码两项 clippy 样式修正另存 style-delta，stores 再次 59/7，
fmt/diff 和前后 hash 通过；原门禁不事后升级。详见
[候选验证报告](../../performance/packed-v3-sidecar-candidate-validation-2026-10-05.md)。

仍须完成完整 mount/recovery/PWB3 publication、mutation/generation fence、
reachability GC 和 operator capabilities，运行同源完整门禁与真实后端/FUSE
生命周期出口；系统 S、实验 X、最终性能 campaign 和交付继续开放。

补充只读证据复核：原 v7 normal-01 未持久化 sample_error，仍不可回填具体原因；
但后续 sampler wrapper 已存在于
`D:/BrewFS-agent-recovery/acl-review-20261004/gate07-v7-sampler-diagnostic-v1/`，
记录/hash一致。diag02 抓到 worker smaps_rollup ESRCH 及同 PID zombie；
normal-sampler03 有 135 样本且无 sampling error，omission 有同 owned Popen 退出证明。
后续 freshstrict01 仍失败于 target 同对象/范围 eviction-refetch。因此下一轮沿用
严格 sampler，诊断实际 target cache owner/retention/eviction 与完整 GET 范围；
不重复制作已存在的 sampler wrapper，也不降低 refetch/minimum-Control 断言。
这些是各冻结历史 binary 的诊断证据，不能签收当前源码的真实 FUSE 出口。

磁盘确认删除已执行：旧 `D:/Codex-Recovery/wsl-backup/ext4.vhdx` 不再存在；
活动镜像 `D:/WSL/Ubuntu-24.04/ext4.vhdx` 保留，Ubuntu/仓库正常可用。旧备份此前
已截断为零字节，本次仅删除空文件，不重复计算原文件释放量。2026-10-05 本次
复核 C 可用 249.58 GiB、D 48.80 GiB，Ubuntu 根盘约 675 GiB；这是快照而非保证。
# 2026-10-05 current reply-retirement batch (OPEN)

The [reply retirement checkpoint](../../performance/packed-v3-reply-retirement-validation-2026-10-05.md)
records two actual compile→behavior RED→GREEN tests and the subsequent vendor
55-pass regression. The new closed/discarded-response negative tests then
compiled and exposed the still-open terminal-failure defect. The current tree
is an intermediate TDD batch, not a new complete-gate or runtime acceptance.
Normal predecessor review is complete; strict pending-body SIGTERM remains
failed. Its worker finally returned zero bytes/ENODEV and exited1, rather than
remaining persistently blocked. Full G07 and all SPEC/S/X remain OPEN.

新同源 gate06/supplement05 已实际通过49+17项且独审通过；两 runtime freezer
session23655、fresh io-uring/raw normal session85184、完整保护及独审均exit0。
normal仅签该子项，64 reads为1成功/63 ENOMEM。wrapper02严格run01在SIGTERM前
sampler ESRCH/owned Popen poll None失败；run02实际SIGTERM后daemon0/mount消失，
但worker零bytes/ENODEV19/exit1仍失败，完整evidence和ext4 work均保护。
原errno/helper/预算/断言未降低。active追加3项真实adapter client-close tests，
session62997先编译再核预期RED；生产候选仍外部未编译，typedfailure/deadline、
storedfuture预收费、reply/native实际join及完整Roots仍须补齐。全部SPEC目标继续active。

### 2026-10-06 G14 capability boundary checkpoint

One small G14 sub-contract is implemented: operator cluster/workspace status now
carries explicit packed-v3 capability and PWB3 binding state, and a pure helper
derives a fail-closed PackedLowerReady condition. The native-only catalog reports
PackedLowerReady=False/UnsupportedCapability; a revision tuple is never treated
as packed Ready. Focused operator suite: 25 passed. See
doc/performance/packed-v3-g14-capability-status-validation-2026-10-06.md.

This does not close G14. Runtime packed binding loading/verification, mount
lease/finalizer durable drain, Redis/TiKV Kubernetes E2E, operator GC, failure
recovery, and real FUSE lifecycle acceptance remain open.

### 2026-10-06 G12 SQLite history-anchor checkpoint

One G12 integrity sub-contract is implemented: SQLite loading of a current PWB3
binding now requires a decodable version-1 history anchor for the same
workspace. A current pointer without that anchor fails closed with
`CorruptMetadata`, matching the KV catalog contract. The focused real SQLite
binding test passed; see
doc/performance/packed-v3-g12-history-anchor-validation-2026-10-06.md.

Publication/recovery state transitions, generation fencing across mount and
seal, object dependency closure, remote backend behavior, and the full
publish/remount/GC lifecycle remain open.
