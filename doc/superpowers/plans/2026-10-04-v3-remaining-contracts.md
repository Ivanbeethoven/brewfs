# v3 后续实现契约（2026-10-04）

当前用户要求只保留 v3。v2 和其他历史兼容移出任务；本计划只列 v3 三创新需要的
实现；后续设计不能作为功能签收。当前集成候选包含readonly Linux ACL、typed stat、
PM10/IP06加权分页和v3-only入口，完整门禁与实挂载验收未完；其后按下列依赖继续。

## G05：加权目录 rank/select

当前 readdir 从 Groups 前缀首部扫描 group refs，再按 entry_count 扣 cookie。
页有界，但深 cookie 的请求数仍线性增长。保留 streaming builder，把当前 005
索引改为 mandatory counted 契约：manifest PM10、index IP06。

- 内部 child 加 `subtree_weight:u64`；Groups 叶子权重由 GR05.entry_count 导出，
  其余索引叶子权重为1。父记录权重与所选child的实际权重必须相等，所有和用checked
  运算；零分支、溢出、旧payload或缺少contract均拒绝，不退回扫描。
- PM10固定Groups root总dentry weight。分页先计算parent32前缀的lower-bound rank，
  checked加cookie，再select所在group；保留有界路径栈只遍历后续所需group。
- offset保持u64到选中group；目录cookie最大 `i64::MAX-2`。页跨目录前核对parent，
  不下载邻目录GroupMeta。cursor绑定manifest content digest和parent。
- builder每层仍只有一个有界buffer；spool排序/分页可复用，不加载全目录summaries。
  可移植native frozen的rank/select路径栈算法，但不能使用其整Vec builder或零count回退。
- 明确浅/中/深cookie实际range GET图、无前置GroupMeta/数据/cold读取；变量组大小、
  255-byte/raw names、跨group/page、rewind/EOF/empty、eviction/refetch和损坏拒绝均验收。
  raw/zstd真实36k目录是第一实挂载门禁，规范要求的更大规模与内存证据另行完成。

## G04：可写 ACL 原子契约

当前Readonly capability只能声明冻结ACL的查询与访问。开启ReadWrite前需实现
同一提交版本的mode/access ACL读取、原子set/remove/chmod，以及create/default继承。

Readonly cold失败目前可返回EIO，但旧`stat_ino -> Option`吞hot index/backend错误，
xattr前置guard把它变成ENOENT。还需typed stat接口和实际损坏hot page负测，不能把
所有None都当作文件不存在。

- `get_inode_permissions`返回同一版本的hot attrs和access ACL，避免stat旧值/xattr
  新值的混合授权。set-access ACL同步mode；remove保持mode；default不改变mode。
- create事务读取parent/default ACL，按请求mode决定继承，写inode+dentry+access ACL；
  directory再保留原样default ACL。没有default时才用umask。已有inode的open不继承。
- FUSE create/mknod目前丢弃ABI umask，必须贯穿vendor trait/object-safe/logfs/BrewFS；
  writable协商POSIX_ACL/dont_mask后，mkdir不能提前屏蔽请求mode。
- namespace mutation一次写入xattrs；携带inode/parent/ACL读取条件。冲突回到调用层
  重新resolve和计算，不能CAS失败后重用陈旧inode或继承ACL。
- overlay已有批量inode+xattr CAS；namespace请求要扩同批xattrs。Redis native
  setattr不能再读cache后整体save；TiKV在write_txn中锁node并补xattr路径；SQL把
  existence/ACL读取移入锁行事务。进程内mutex不能代替独立实例并发验证。
- ACL mutation成功后失效inode/open-file/kernel权限缓存。owner/root、flags、default
  kind、symlink、empty/base ACL规则用实际Linux源对照；拒绝不能留下半更新。
- 每backend单独验证并发set/chmod、parent-default/create、commit failpoint、hardlink、
  rename、revoke、seal/remount、错误lease/head；通过后才单独开启ReadWrite。

## G02：冻结源

旧SnapshotBacked只是调用者policy标签，读取仍按可变路径重新open，不能复用它签收。
仅枚举enum、只读bind mount、再次stat或前后archive digest都不构成原子view。
v3 importer需要持有至manifest验证完成的FrozenSourceLease/Provider，由真实filesystem
snapshot或明确application freeze提供稳定root、身份与guard。目录和payload读取必须
经同一个provider/no-follow fd；原BestEffortDetected可作为明确较弱的独立策略。

其后继续G06/G07完整请求计数与mount共享预算、G08/G09同executor与pipeline、
G10–G14 binding/fallback/fence/publication/recovery/GC/operator，最后G15–G17实验控制、
可复现runner、匹配实验与交付。系统出口完成前不冻结实验或启动最终campaign。
