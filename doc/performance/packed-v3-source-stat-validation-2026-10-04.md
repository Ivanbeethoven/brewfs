# Packed v3 source root / allocated-block validation — 2026-10-04

用户要求完成全部 SPEC；持续目标及独立 v2/operator/large-directory 出口见
[全部 SPEC 清单](../superpowers/plans/2026-10-04-brewfs-all-spec-completion.md)。
本报告仅覆盖真实根目录属性和 allocated blocks 的版本化补齐，不宣称完整 namespace、
packed workspace 生命周期或性能接受。

## 问题与修复

8 MiB真实 sparse file 仅分配8个512B blocks；旧 readonly adapter 按 logical EOF
返回16,384。`source_allocation_blocks_survive_packed_getattr_and_lookup` 用实际文件捕获→
上传producer→认证manifest→readonly getattr复现该错误，behavior red为0 passed/1 failed。

新增独立 **PM08** payload；原 PM07、旧004及旧 bincode schema保持原解释。
PM08 manifest认证RA05根目录热属性和BRFSI005/IP05 allocation root；SI05 leaf绑定
inode与实际st_blocks。root属性无需GroupMeta，其他inode在getattr/open/lookup时读
有界allocation页。缺失record、错误inode、替换页、未知version或冲突hardlink blocks
直接报错。发布前验证inode集合与allocation集合闭合，root不能重用为dentry。

producer明确调用`set_root_attributes`启用PM08，所有非根inode随后必须调用
`set_inode_blocks`。这些更新中断/失败会阻止finish，不能把部分source属性丢弃后
发布旧payload。私有SQLite spool继续负责外排序，allocation树不保存全namespace Vec。

Linux single-file fixture同时pin源父目录fd，保存root size/blocks/mode/uid/gid/nlink/
atime/mtime/ctime与binary xattrs；子文件st_blocks保存到SI05。fd/path stat token在
捕获后及finish前后重新验证，检测普通变更和路径替换，忽略本次读取改变的atime。
provenance标明PM08、属性保存、`source-parent-attributes-only-not-full-inventory`。

## 验证与产物

本批artifact：
`docker/compose-xfstests/artifacts/packed-v3-completion-20261004-source-stat/`。
初始工作树与354个源码文件哈希保存在`initial-status.txt`、`initial-source-hashes.json`。

- `allocation-red.log`：真实稀疏块数行为red。
- `packed-green-repair.log`：packed定向122 passed/0 failed；包含PM07逐字节重新encode，
  PM08 schema/downgrade/trailing/truncation、authenticated allocation missing/misbinding/
  replacement、producer closure/conflict、真实root source mutation/replacement与block读回。
- `packed-green-first.log`/`packed-green.log`保留编译期测试修正；它们不是行为red证据。
- `final-gate/`保留同迭代AGENTS gate全部命令/退出码；fixture构建初次引用私有wire模块
  失败，修正为公开PackedWireError后在`repair-gate/`单列重建及fmt/diff结果。
  `accepted-gate-status.json`保存逐项最终结果与原始失败来源，不覆盖失败日志。
- `fuse/`保存raw/zstd各五例：dense、sparse、all-hole、empty、raw filename。
  每例完整/partial字节、文件属性/st_blocks、root属性/binary xattr、EROFS及正常卸载
  由扩展原有源oracle验证；执行的完整oracle保存在`expanded-source-fuse-check.py`。
- `accepted-source-hashes.json`、`fuse/source-fuse-binary-sha256.json`与`verification.json`
  用于核对accepted源码、实际mount binaries、测试计数、原始失败、链接与资源清理。

最终修正后40项本地gate均通过。workspace lib为1,015 passed/225 ignored，brewfs bin
为1,099 passed/225 ignored；workspace-overlay lib为1,305 passed/227 ignored，fixture
为1 passed，vendor为13 passed。定向packed为122 passed。原始fixture build失败保留，
实际binary在repair阶段重新构建。

十次真实source FUSE验证零错误，完整回读合计20,971,584 bytes，文件st_blocks与源
逐项相同；根size/blocks/mode/uid/gid/nlink/mtime/ctime八个字段和binary xattr匹配。
根atime的保存与读回由库回归覆盖，未把它记成上述实挂载八字段之一。全部正常卸载。
源码/binary身份与资源检查以`verification.json`为最终机器可读记录。不声明
all-feature/operator/严格`-D warnings` GitHub CI通过；旧warnings保持可见。

这些运行使用debug binary、零payload caches、TTL0/directIO/keep-cache0。source oracle
不drop host page cache，属于correctness验证。没有BrewFS/JuiceFS吞吐比较、云campaign
或新性能表更新；`.stats`仍受现有观测缺口限制。

## 未完成边界与下一步

G02/G03/G04继续开放：本批是single regular-file dentry加源父目录属性，visible child
nlink仍为1，不代表完整目录枚举或子树hardlink策略。Stat revalidation是检测协议，
不是原子目录snapshot；完整source必须提供明确的consistency/provenance与可执行拒绝。
special/rdev真实导入、POSIX ACL应用、raw xattr name端到端及>64MiB/>256 extents external
placement继续待补。PM08新allocation root也必须进入未来发布验证与GC对象图。

随后继续共享读取预算与观测、native/packed统一执行、005 pipeline、packed binding/
fence/publication/recovery/GC及各独立SPEC出口。S/X未签收，全部SPEC goal保持active；
系统验收完成后才冻结和执行最终实验。
