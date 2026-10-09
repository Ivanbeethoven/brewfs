# Packed-v3 carrier、fork 与首次初始化验证

当前范围仅 packed-v3。主线是 Redis/TiKV 上的实际发布、引用保留、恢复与回收；
本页记录开发验证，不代表全部 SPEC、FUSE 或 Kubernetes 验收通过。

最新终态为 `runtime-integrated-diagnostic04`：46项全部执行，38通过、8失败、
0未执行；所有阶段source/config/Git不变，自有Redis/PD/TiKV及proxy清理和独立absence
检查通过。阶段计数依次为fork0/2、bootstrap2/0、initial composer1/1、clean-source负例4/0、
original shutdown18/0、headless0/2、headless route2/0、native resume11/1、specialized auth0/2。阶段runner仅在整阶段成功时报告
accepted passed；本页实际通过数从原cargo harness逐项汇总，不混淆两种计数。
Redis BuildingEmpty本轮通过；失败为readback-cancelled的journal_not_future守卫。旧十二项native通过保留历史范围。
最新native阶段log SHA为`9226d1abb423a89d62c52175d3464dd3172712ed4a21fab0fc67e06aa886aa92`。
普通stores05已终态328/0/101 ignored；SDK03已122/0/1 ignored。
metadata-followup05已编译并通过stores05，真实生命周期仍有上述失败。
终态后followup06接入fork目录修复、Original PCR/Recovered PMR历史hot租约完整认证与CONTROL
hydration、初始composer/clock固定诊断及specialized auth测试契约修正；fmt/diff及8处源码
delta/其余769冻结输入不变检查通过。新8项失败路径重验runtime-closeout-diagnostic05正在运行，
尚无终态；详见
[当前元数据增量](packed-v3-metadata-followup-validation-2026-10-08.md)。
上轮普通stores02实际295通过、28失败、99 ignored，source/Git不变，失败证据保留。
其log SHA为`2f5e9f1a94ef51966bd85b40a0cf8059318bc7093d03137bd447ea6d3fdd2c15`。

TiKV透明Lock response probe实际0通过/1失败，前四个回复正常，后三个均为非空
KeyError的retryable+WriteConflict组合，无region error；并非空占位或已证明的LockInfo。
源、库测试binary不变，自有服务/proxy及独立absence清理通过；log SHA为
`b6565a8060311f154d0f918b132540a5093740892a334fde61a8d6634f581847`。
证据位于外置`mount-gate-audit01/tikv-lock-response-shape-stage01/`；该binary是Rust库测试，
不作为实际生产CLI的验证证据。

最新接入见`candidate-root-integrations/metadata-followup03/`：完整PWA handoff packet、
ordinary journal同holder重附着夹具、native-rebind canonical预算、scoped hot-head/PWA
PCR发现及logical expiry精确保留断言。stores03已终态为323通过、0失败、99 ignored，
source/Git不变，log SHA为`5b5cf0636fe7a2c501e8c0ea800ffe80b4aa44e2963d5cc40fc23c3e08dddcdf`。
原28项普通失败包括26项root故障控制均已通过。
followup04已精确接入TiKV严格冲突处理、typed fork mount/clean publication与两项真实
discovery竞态回归；格式检查通过。SDK01编译类型错误、0测试，其后仅修正两个借用Key
转换；SDK02已终态为119通过、0失败、1 ignored，source/Git不变，log SHA为
`33a51209befa7f5393aacc36acc0bb6fc6f22e3d32dd7e6170f46d4a92744b69`。
集成BrewFS的stores04已终态326/0；reason04观测已完成，修复后的真实后端重验仍待执行。中央operator leader GC
仍是独立生产缺口，collector CLI草稿不能关闭该出口。
不得据静态独审关闭SPEC或混算旧失败阶段。

## 已执行结果

| 批次 | 实际结果 | 验证范围 |
| --- | --- | --- |
| 原完整元数据 attempt12 | 26 阶段、64 通过、0 失败、0 未执行 | 原有 factory、恢复、Prepare、真实进程退出、历史回收、有限读取、pins、public binding 与 no-replay |
| carrier codec RED 接线 | 编译通过、3 通过 | descriptor/claim 编码、完整身份、损坏及半对拒绝 |
| carrier 实际 RED | 0 通过、2 预期失败 | Redis/TiKV 的实际发布事务缺失 descriptor/claim，被原事务断言拒绝 |
| carrier GREEN + fork RED 普通 stores | 292 通过、0 失败、79 ignored | 全 stores；ignored 用例仍需单独真实执行 |
| carrier 实际 GREEN | 12 通过、0 失败 | 原 factory8 加冲突/错误 native revision4；含取消等待、未知回复与不重发 |
| fork 实际 RED | 0 通过、2 预期失败 | Redis/TiKV 实际子 workspace 创建缺少 packed 写组，记录的 packed birth packets 为0，要求为2 |
| fork GREEN + bootstrap RED 编译 | 编译通过、codec3 通过 | session76019 终态 exit0，source/Git 不变 |
| fork GREEN 首轮诊断 | 0 通过、2 失败；bootstrap2 未执行 | 已完成创建断言后，实际回读被测试传入的1MiB准备预算拒绝；同时两处SPEC命名修改使全输入清单变化，不签收冻结源码门禁 |
| fork GREEN 第二轮诊断 | 0 通过、2 失败；bootstrap2 未执行 | source/config/Git 不变；实际子workspace重打包完成后，临时mount shutdown关闭了夹具共用budget，后续binding校验拒绝 |
| bootstrap 独立实际 RED | 0 通过、2 预期契约失败 | source/config/Git 不变；真实首次安装事务被原same-CAS断言抓住缺失PBIInstalled写组。外层runner预期的后置错误位置不准确，原receipt保留，单独解释实际前置断言 |
| runtime 编译03 | 3 通过、0 失败 | carrier codec；source/Git 不变，进程终态 exit0 |
| runtime 实际首轮 | 0 通过、2 失败；40 未执行 | Redis/TiKV fork 在 producer 上传回读处被 observer 拒绝：读取未提供 typed class。source/config/Git 不变，服务和 proxy 独立清理核对通过 |
| runtime 编译04 | 326 通过、2 失败、4 ignored | 已分类的上传回读成功测试和旧 GM05 拒绝通过；失败为旧统计预算常量与腐败上传测试查错 ledger。source/Git 不变，进程终态 exit101 |
| runtime 编译05 | 328 通过、0 失败、4 ignored | 修复两个严格控制；保留最低预算、少1字节拒绝、HashMismatch 与认证/传输账本区分；source/Git 不变，进程终态 exit0 |
| observer 完整控制 RED | 30 通过、1 失败 | 全量枚举渲染测试另有旧类别数和预算常量；source/Git 不变，失败位置在输出能力校验 |
| observer 全量枚举首轮 | 30 通过、1 失败 | 完整渲染捕获新标签使统计行209B，超过保留的208B上限；source/Git 不变。新标签现缩为 `publication_verify`，断言及预算未放宽 |
| observer 全量枚举重验 | 31 通过、0 失败 | source/Git 不变，终态 exit0；完整枚举、恰好最低预算及默认预算控制通过 |
| runtime 实际第二轮 | 0 通过、2 失败；40 未执行 | source/config/Git 不变；真实 child repack 完成后，夹具的旧 binding reader 仍持 pin，旧 alias 退休正确返回 Busy；服务和 proxy 独立清理核对通过 |
| fork reader 修复编译 | 3 通过、0 失败 | source/Git 不变、终态 exit0；修复夹具编译及carrier codec控制，不能代替真实退休验证 |
| runtime 实际第三轮 | 0 通过、2 失败；40 未执行 | source/config/Git 不变；旧 reader 关闭后的 alias 退休通过，后续父图回收的 current-history census 返回 Fenced；自有服务及 proxy 独立确认已清理 |
| runtime 剩余诊断 | 4 通过、36 失败；0 未执行 | 全部剩余40项实际执行，和第三轮同一冻结源码；bootstrap2通过，其它阶段保留原失败；自有服务及 proxy 独立确认已清理 |
| mounted-session stores compile02 | 210 通过、96 失败、99 ignored | 编译成功，source/Git不变；多数旧夹具缺少canonical预算，另有正常clock、精确CAS控制与生产授权/恢复缺口 |
| mounted-session 真实诊断 | 16 通过、26 失败、0 未执行 | 42项同一冻结源码全量执行；source/config/Git不变，自有服务及proxy独立确认已清理；失败原日志完整保留 |

上表明确标注输入不变的批次按各自冻结输入签收；首次 fork 诊断的 SPEC 修改例外
保留失败，不将它描述为输入未变。原 attempt12 及真实
carrier/fork 批次的自有 Redis/PD/TiKV 容器与 proxy，均在终态后独立核对消失。
RED 的失败只是证明测试抓住缺口，不能作为通过的系统用例。

## 已接入但尚未完成验证

carrier descriptor/claim 与原有 carrier/head/PWB/journal/registry/native holds
同一发布 CAS 安装。未知回复只允许以原绝对截止时间执行一次完整 no-op CAS，
检查每个原始条件及实际 successor；不进行 point reads，也不重发 mutation。

packed 子 workspace 的 current/claim/history1、private alias、inode floor 和
native holds 已加入原创建 CAS。alias 复用 source graph，不复制 source RootRow
或改变其 memberships。逻辑退休仅允许 Deleting，保留较新的 current/history，
source 物理回收另外检查 alias census。公开 packed fork 必须使用显式 packed
API：缺失双 marker 不能被普通 native API 当作识别 packed 类型的依据。

当前活动树已精确接入 fork、retained-carrier、首次 bootstrap、initial composer、
原 mount clean release、headless snapshot 和上传恢复。此前 read helper 准备预算、
子 mount 共用预算和缺失首次安装写组已分别修正；历史失败保留。实际42项
首轮在 fork2 的未分类上传回读处停止，其余40未执行，不能据普通单元测试签收。
producer 与 upload resume 改用 `PublicationVerification`，整对象 metadata+payload
验证流量单独统计，范围与逐字节/digest校验保留。runtime 编译05已通过328项；
observer全量统计控制已单独通过31项。第二轮真实矩阵在 fork2 的旧 alias 退休
处停止，另40项未执行：夹具仍持有旧 binding reader，不应提前回收其 alias。
现补上关闭前拒绝的断言，再关闭该 reader，保留原有关闭后退休及图不变断言；
生产保护未放松。修复编译及3项codec控制已通过。第三轮及随后剩余诊断现均已终态：
同一冻结源码共42个唯一用例，4通过、38失败、0未执行；整阶段保守签收仅bootstrap2。
部分native resume在目标故障注入之前失败，不能证明其恢复语义。

已接入但尚未编译的GC修正区分“允许遍历真正Deleting current head”和“允许删除exact
target”：前者仍要求workspace的head/epoch准确，后者仍仅限target。其他workspace的
ancestry保留引用；新回归覆盖真实Deleting转移、dependent/disjoint图，以及stale head、
stale epoch、Active+tombstone和Deleting ancestor拒绝，不放松回收门禁。
其余失败已定位到VFS detached stats同时持writer和stats extension强引用，以及TiKV
缺少receipt请求实际要求的8KiB response decoder。外置候选保持原owners-zero断言、
4KiB record/8KiB response上限，未接入前不能算修复完成。

bootstrap 候选以真实 root-only native view 和 live lease 为输入，在 PUT 前使用
PBI3 与原 PRO3/PRR3/PRM3、PackedUploadGuard 建立 durable staging。完整图审计后，
首次 PWB、root mapping、members adoption 与 PBIInstalled 应同 CAS 写入。
独立 RED 已捕获首次安装 endpoint 的缺失写组，该写组现已接入。first bootstrap→
actual VFS/native capture/factory→carrier Snapshot 组合现已接入但实际验证待重跑，
不能把裸 producer/install 或编译通过当作完整初始化。

joint mount grant/heartbeat/clean release、scoped writer、bounded authentication、
真实binary CLI控制及上述stats/8KiB修复现已作为61文件stage04开发候选接入。
逐文件after guards通过，formatter仅修改本包/GC范围。开发树在consistent backend和
canonical mount budget具备时报告mount capability，尚无公开生命周期验收结论。
首轮编译因两个夹具调用module-private `clean_exact_cas` 返回E0624，0项执行，
source/Git不变。该编译接线需修正后再运行原42项与真实CLI/FUSE/恢复控制。

## 证据

外置 recovery evidence 根下：

- `metadata-system-closeout07/actual-test-counts.json`：原64项逐项日志、冻结源码及独立清理。
- `carrier-fork-active-integration01/`：每个精确 apply_patch 阶段的 before/expected/applied；其它源码不变。
- `carrier-fork-real-validation01/carrier-red01/`：carrier 两项真实 RED 与清理。
- `carrier-fork-real-validation01/carrier-green-fork-red01/`：carrier12 GREEN、fork2 RED 与清理。
- `carrier-fork-real-validation01/fork-green-bootstrap-red01/ACTUAL-FAILURE.md`：实际fork0/2、bootstrap未执行、输入变化及独立清理；不修改原外层遗漏计数的receipt。
- `carrier-fork-real-validation01/fork-green-bootstrap-red02/`：实际fork0/2、完整输入不变、临时mount共用budget的shutdown问题与独立清理。
- `carrier-fork-real-validation01/bootstrap-red01/ACTUAL-CONTRACT-RED.md`：实际首次安装same-CAS断言抓住缺PBIInstalled；外层后置marker预期错误不能抹掉前置契约失败，亦不证明lost-reply行为。
- `kv-sdk-development/carrier-green-fork-red-stores01/`：全 stores292/0/79 ignored。
- `carrier-fork-bootstrap-static01/result.json`：14文件的外置完整文本整合；不是编译或行为证据。
- `carrier-fork-real-validation01/runtime-closeout-green01/`：实际fork0/2、40未执行、冻结输入和独立清理。
- `kv-sdk-development/runtime-closeout-compile03/`、`runtime-closeout-compile04/`、`runtime-closeout-compile05/`：各自完整终态、源码清单和日志哈希。
- `runtime-readback-repair01/`：typed上传回读和旧GM05拒绝的精确补丁。
- `runtime-readback-controls-repair01/`：两个严格测试控制修复，其它冻结输入不变。
- `kv-sdk-development/runtime-observer-control-red01/`、`runtime-observer-controls-repair01/`：实际全量统计失败及仅测试/注释修复；GREEN需独立终态。
- `kv-sdk-development/runtime-observer-control-green01/`、`runtime-observer-label-repair01/`：全枚举实际捕获209B统计行的终态失败及仅标签缩短修复。
- `kv-sdk-development/runtime-observer-control-green02/`：完整31项终态通过；日志SHA `0e18b748791f6e187f1afc8afa5c48c98518099e1ce9061ff6e84df9265d8aba`。
- `carrier-fork-real-validation01/runtime-closeout-green02/`：实际fork0/2、40未执行、冻结输入和独立清理；日志SHA `dfaeabe6ef54788d751f2016b85576c489d5eb5776890e58744d355a1ced1342`。
- `fork-old-reader-retirement-repair01/review.json`：精确夹具修复、before/after/patch哈希及原失败；需要独立真实重验。
- `carrier-fork-real-validation01/runtime-closeout-green03/`：第三轮真实失败、冻结源码与独立清理；日志SHA `aeae29ed245170b40baa460fbc81b282f724a25f416100002553cc0cb3cb554a`。
- `carrier-fork-real-validation01/runtime-remaining-diagnostic01/`：其余40项全量诊断，各阶段原日志及独立清理。
- `runtime-diagnostic-summary01/result.json`：同源42项的去重汇总及验收边界。
- `current-deleting-head-repair01/integration.json`：GC生产与回归的精确接入；编译、真实后端重验待执行。

三份实际日志 SHA-256：

- carrier RED：`4cfe2c1e6337d520cacdc496a357214901c85a96c57e480e03dee745e8dc6796`
- carrier factory GREEN：`8b7ec8856a991a3ce6660e6288688de41d5987f5accf9192bc2f8c908e4739b1`
- carrier counterfactual GREEN：`73533d54f5044c4a4b0d60a1340e8c1c0cd654fff324cc0262a4678a81242f61`

完整仓库门禁、实际 mounted/operator 生命周期、恢复窗口和系统 S/X 仍未签收。
系统完成后再冻结三创新实验。
