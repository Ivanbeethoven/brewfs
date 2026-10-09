# Packed v3 外部大文件实施契约

本设计接续全部 SPEC 目标中的 G03。它定义实现顺序和格式约束；具体接线和最终验收
状态见[external验证记录](../../performance/packed-v3-external-validation-2026-10-04.md)，
不签收 S/X 或启动性能实验。旧 PM08 group admission 限制 logical 64 MiB、
data 16 MiB、256 extent records；新 source importer 超过这些边界时自动路由到 PM09。

## 问题与兼容约束

把整文件读进 Vec、增大 GM07 cap 或把没有 placement 的 regular inode 解释为 hole，
都不能满足 external-large。需要磁盘分页 extent inventory、独立认证外部 chunks，
以及只读取请求区间所需页/frame 的既有 UnifiedReadPlan 执行路径。

- 004、PM07、PM08、GM07 的已有字节和解码含义保持原样。
- 新发布必须使用独立 PM09 payload，旧 reader 明确拒绝；PM09 要求独立 EX09
  契约段，不能只把 PM08 的四字节 magic 换成 PM09 就成功解码。
- PM09 保留 RA05/SI05 源属性；LargePlacements root 改由新 payload 声明为
  必需的每 regular-inode placement selector。非 regular inode 不进入此集合。
  PM07/PM08 的空 reserved root 不获得新含义。
- producer 验证 selector 集合与 regular inode 集合完全闭合；missing、orphan、
  wrong kind、size mismatch、duplicate/conflicting selector 均阻止 finish。
  缺失 selector 不能降级为 GroupMeta 或 all-hole。

## 物理和逻辑认证链

每个 selector 明确选择 group 或 external；group selector 继续使用 canonical inode
locator 与 GM07，external selector 绑定 inode、logical EOF、实际 data bytes、extent
count、逻辑 data-runs digest 和一个嵌套 IP05 extent root。all-hole 外部文件可使用
经过认证的空 extent tree；空树与缺少 selector 是不同状态。

嵌套 range leaf 绑定 inode、file_offset/logical_len、container ordinal、frame ordinal、
raw offset/raw length。range fences 必须与 value 的逻辑覆盖完全一致、有序且不交叠，
只允许显式 sparse gaps 变成零。未知 version、错误 inode、越 EOF、整数溢出、重叠、
错误 frame/raw length 必须拒绝。每页使用现有 IP05 256 KiB/1024 records/16 层上限，
不能在 selector 中保存全量 extent refs。

物理 payload 用 BRFLD005/LD05 chunk，复用 FD05 descriptor pages 与 manifest 的
Containers/Frames 两棵认证树。每个 chunk raw/stored 各有固定预算和 frame-count 上限，
只积累当前 chunk，写入/回读校验完成后丢弃。FD05 同时固定该 chunk 的 digest、length、
profile、size table 和 frame descriptors，frame GET 保持独立 raw/zstd 校验。
完整文件大小不再决定单次分配；单个请求仍受 allocation/shared budget 限制。

## 源捕获、别名与取消

1. 使用 no-follow regular fd，保存初始 metadata/path token 与真实 blocks/cold 属性。
   SEEK_DATA/HOLE 流式产生逻辑 data runs；磁盘 spool 保存 runs，内存只保留当前页。
2. 在任何 payload 上传前选择 group/external。选择考虑 logical EOF、实际 data bytes、
   将生成的 extent-record 数和 group raw/stored budget；不得在失败后悄悄丢弃已上传
   selector，或者把部分数据发布成 hole。
3. external 路径只持有当前 bounded frame/chunk。每次 payload 读取、最后一个 chunk、
   index construction、manifest finish 前后验证初始 token。源变化使此次操作失败，
   返回 manifest ref 仍不是 workspace head publication。
4. 用 dev/ino 聚合后的同一个 source identity 只生产一次 external placement；别名
   共享 selector，同时保留各自 raw dentry 与反向名称。visible-links/reject-external
   的 nlink 契约不变。producer 校验别名热属性、cold、blocks 和 logical-runs identity。
5. 取消/失败清理私有 spool，不返回可发布 manifest。已经上传的 CAS orphan 由后续
   G13 grace/reachability GC 回收；不能通过清理全部 prefix 删除其他快照共享对象。

## 读取接线与内存出口

`prepare_inode_read` 先按 manifest payload/selector 分派；external range scan 只路由
与请求交集的 extent pages。读取的 descriptors 和 chunks 仍来自已认证的 roots，
进入既有 PackedFrame / UnifiedReadPlan / executor。已取 frame 的 map 用
`(container_ordinal, frame_ordinal)`，避免多个 chunk 的 ordinal=0 相互覆盖。
planner/fetcher 绑定 manifest generation；hardlink inode 使用同一个 canonical selector。

单次 prepare 的 allocation 记账至少包括 output、selector/page、segment/descriptor
集合、stored/raw decode scratch 和 retained frames。跨请求持有到最后 consumer 释放的
permit 是 G07 独立必需出口；本批不能以单 read 的 cap 宣称 mount-wide 预算完成。

## 实施顺序和验收

先保存源码基线并运行真实 CLI 缺失回归，随后按以下最小批次推进：

1. PM09/EX09、selector/extent codecs 和旧 payload 逐字节兼容；unknown/missing/trailing/
   downgrade/identity/range/count tampering 全部 fail closed。
2. 私有分页 source runs 与 bounded LD05 producer；闭合校验、hardlink 共享、upload
   corruption、failure/cancellation 及源 mutation/replacement 拒绝。
3. authenticated external range→同 executor；多 chunk、跨 descriptor/extent page、hole、
   EOF、零长度、allocation refusal 和重算内部 hashes 后的外部 ref 替换拒绝。
4. directory/single-file CLI 自动选择与 provenance；source guard 覆盖 objects/manifest/
   sidecar 路径。只在新 payload 生成时声明 external 能力。
5. 同迭代完整 AGENTS gate 与实际 raw/zstd FUSE：>64 MiB dense、>64 MiB all-hole、
   >256 sparse extents、跨目录 hardlinks、完整/partial/跨页/跨 chunk 读取、source attrs
   和 EROFS；最后由 kernel mountinfo 确认正常卸载与本次资源回收。

测试 corpus 总量有界、debug correctness 不作为性能结果。保留原始 red/失败记录，
新运行使用独立 artifact 目录；五份 SPEC 总目标、S/X 和性能交付状态分别记录。
