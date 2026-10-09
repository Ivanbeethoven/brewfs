# Packed-v3 及关联 SPEC review（2026-10-03）

## Context / scope

用户要求审查并修改相关 SPEC，本轮是 **文档 review**，不是继续写 Rust 功能或接受
性能优化。基于当前 worktree（HEAD `a429b0e`，已有未提交的 004/005/CI/vendor 改动），
对照代码和既有测试证据修正规范；不修改源码、依赖、CI、交接或性能结果，不运行云测试，
不提交/推送，不覆盖已有用户改动。

主文档：
[packed-v3 SPEC](../specs/2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md)。
关联文档仅补范围和交叉链接，不改旧 wire/schema/CR 语义：
[workspace-v1](../specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md)、
[clustered-v2](../specs/2026-08-24-brewfs-clustered-frozen-metadata-v2.md)、
[large-directory note](../specs/2026-09-22-brewfs-packed-metadata-large-directory.md)、
[operator lifecycle](../specs/2026-09-03-brewfs-workspace-operator-lifecycle-spec.md)。

## Confirmed findings / incorporated revisions

| 审查问题 | 已确认的依据/影响 | 修订 |
| --- | --- | --- |
| 历史004、当前005和未来目标混用 | 前言005正确，后文却把4KiB header/压缩/cold继续写在BRFPM004下；会造成错误reader/writer | v3版本契约、§4/5按版本列magic、字段、未实现项；旧checkpoint标历史 |
| envelope/header字段不存在 | 005 header没有container id/profile/table/region目录；实际在GC05 prefix和PM07/FD05 | §4.2精确envelope/body/footer字段，区分CRC与SHA域 |
| GroupMeta布局/记录与代码不一致 | 原文name arena/file table/file_record_id/cold ordinal及u8 prefix并不存在；FD在独立页 | §5列GM05/06/07兼容、u16前缀、32-entry restart runs、真实5-field extents、独立FD05 |
| 对象/页/范围单位混淆 | 64MiB是005body上限，完整对象加4096+64；页body cap不含envelope；256 cap是extent records | §4/5/9分别定义body/raw/stored/range/decoded和record/frame数量 |
| 认证自洽与信任锚混淆 | 对象内重算hash不能认证其来源；frame digest覆盖stored bytes，不存在额外raw hash | 固定caller trusted manifest ref、索引/ref链、stored digest后decode、完整scrub另列 |
| 请求图省略GET | 冷IP05/container/FD路由未计入“一个meta+一个frame”；5 GET回归不是完整path/open | §1/7/12分metadata-cold/warm/inline与各层lookup，不保证固定2 GET |
| “v3全部已有coordinator/cache” | 004有，005当前逐需prepared fetch并持有frames，未接跨FUSEsingleflight/prefetch | §7/8支持矩阵、prepare/execute实际阶段与尚未完成控制 |
| 4×/16MiB与partial read上限混淆 | planner使用4×/16×合并判据，range默认仍8MiB，独立frame不能任意拆 | §7.5区分倍率/range/单frame放大；§6不承诺C0任意请求≤2× |
| p90改了size class / runtime改变co-pack | selector按file size分class，p90只改target；构建frame不依赖未来请求 | §6目标与当前defaults分开，target≠stored长度，建布局与合并GET独立 |
| inline隐藏“零payload缓存”事实 | retained/warmed GroupMeta可能含文件bytes，data_range_gets=0不代表零payload传输 | §8.1披露inline retention，metadata-cold/warm与data-cache口径分别报告 |
| window复用冒充临时pipeline/已认证cache | 004 aligned window跨requestretain；decoded-cache admission不补齐未认证descriptor链 | §8分cold-start-window-reuse与纯in-flight，004自校验不称trusted认证 |
| shared32MiB预算过度承诺 | 004 batch permits、005单read检查/Moka weights不覆盖所有queues/pins/output/SDK或RSS | §9写为目标门禁；§7/12补所有生命周期、cancel/deadlock/teardown验收 |
| POSIX能力被一并称完成 | cold ACL lookup不是POSIX ACL应用；root attrs/sparse blocks/真实source仍缺；codec name cap与FUSE NAME_MAX不同 | §5.4/7.3/12明确raw/String、ACL/EROFS/DAC、NAME_MAX、st_blocks与source缺口 |
| hardlink完整枚举被master/观察列表代替 | master只定位read；reverse负责全membership；subtree native nlink可能含外部aliases | §5.4/7.3明确visible nlink策略、paged reverse与有界兼容API，不silent truncate |
| 仅head epoch足够保护mutable read | 同epoch write/hole/rename可变化，holder lease只fencewriter；upperAbsent不能先补hole | §7.4补可见mutation fence、output丢弃/retry；workspace关联说明保留native-v1语义 |
| 对象producer等同完整publication | 当前finish返回ref，并无source捕获/head-binding CAS/journal/GC；container先cold并非非法 | §10以依赖闭合partial order定义目标、独立binding与CAS/lease/GC；operator Ready非证明 |
| P0–P6要求再造现成模块/把未支持profile纳入规模表 | 已有catalog/codec/provider；大规模表不应强制重跑百万导入或未实现pipeline | §12/13复用现有入口，保留明确缺项、10k递进、证据/功能/性能三层验收 |
| wire/raw/逻辑/SDK计数口径混淆 | 压缩wire<logical不代表无overfetch；runtime backend不是完整HTTP attempts | §11分别定义requested/received/raw-union、inline/gaps/retry及RSS/active+drain |

## Code paths checked (read-only)

- `src/workspace_overlay/packed_v3/{wire,meta,layout,group,coordinator,remote,readonly,metrics}.rs`
- `src/workspace_overlay/packed_v3/wire005/{manifest,index,frame_directory,container,cold,producer,spool}.rs`
- `src/chunk/read_plan.rs`、`src/vfs/io/reader.rs`、`src/vfs/fs/mod.rs`、`src/posix.rs`、`src/main.rs`
- `src/workspace_overlay/{model,resolver/extent,meta_layer/mod,lifecycle/mod,compaction,gc}.rs`
- 既有验证记录：[cold/hardlink报告](../../performance/packed-v3-cold-hardlink-validation-2026-10-03.md)、
  [wire/producer/FUSE报告](../../performance/packed-v3-completion-correctness-2026-10-02.md)。

本review仅引用历史gates/fixtures的适用边界，不重跑或为新revision伪造通过数字。

## Remaining implementation gates (not fixed by documentation)

真实source inventory/consistent view/root POSIX metadata、SEEK_DATA/SEEK_HOLE和external
large placement、POSIX ACL应用、完整mount-wide admission/queues/pins/decode/输出寿命，
005跨请求pipeline/cache及完整请求分类，mutable workspace packed binding/read fences/
seal/repack/head-CAS/journal/recovery/reachability GC，static/dynamic控制和matched release
A/B仍未完成。未知kind/flags/name限制、深cookie复杂度等必须有独立negative acceptance；
不能把“应当”改为“当前已满足”来关掉门禁。

## Verification

- `git diff --check`：通过，文档变更无空白错误。
- 五份规范与本审查记录的local markdown links、代码围栏、真实heading重复检查：通过；
  fenced Rust attributes不是Markdown headings，检查须忽略code fences。
- 核对文档字段、magic、limits与上述实际代码；旧004 magic仅用于明确legacy段落，
  不给005发布路径生成BRFPM004。新增P5条件保留原workspace/operator schema与capability。
- 对比review开始时非文档文件SHA-256 snapshot，确保本轮没改源码/依赖/CI/vendor/
  交接等既有工作：非文档文件变更数为0；仅五份相关SPEC及此审查记录发生本轮编辑。
- 本轮没有Rust行为修改，**不执行新的Rust CI或性能campaign**，不更新README性能表，
  不提交/推送。本段记录文档检查，不称完整功能/性能任务已完成。
