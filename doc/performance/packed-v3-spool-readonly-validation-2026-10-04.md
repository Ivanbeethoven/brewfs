# v3-only readonly 与有界私有spool事务候选

状态：已接入并完成行为red/green；366项源码、47项同迭代完整gate、8次真实FUSE和
3项真实Btrfs测试已统一核对。36k第三次仍失败，不宣称全部SPEC、S/X或性能接受。
用户范围仅v3，旧wire/旧PM/catalog回退不再作为兼容出口。

## 原因与实现

六项生产SQL keyset seek已减少前缀扫描，但36k导入两次仍在600秒超时，均未挂载。
失败spool显示source36,003已完整，producer只有9,219identities；每个inode的allocation、
claim、locator、reverse各产生独立提交。此次按至多128目录项批量事务写私有索引，
allocation也按128项写，保持默认SQLite FULL同步/DELETE journal、单连接、leaf编码/
hardlink签名/link-count校验。transaction内部直接使用同一连接，避免嵌套pool等待。

每个group的上传和byte verification发生在事务前。取消/error drop SQLx Transaction
执行rollback；producer保留poisoned状态，已提交prefix也不能finish。私有spool不是
crash-resume协议，最终manifest认证、source fence及workspace publication持久性未降低。
source库存capture/assignment的小事务仍保留，不通过放宽deadline或关闭同步制造通过。

readonly现在只持mandatory authenticated当前snapshot context；删除公开旧catalog
constructor/accessor、Option fallback和空legacy manifest投影。两项旧fixture改为真实
PM10 producer结果，覆盖inline/framed读取、directory/reverse/prepared/EROFS。
共享输入类型尚有历史模块实现，但当前readonly运行入口不再接受旧catalog。

## 已执行证据与开放边界

artifact：`docker/compose-xfstests/artifacts/packed-v3-completion-20261004-spool-batch/`；
readonly单独red/green位于`packed-v3-completion-20261004-v3-only-internal/`。

- readonly API red：3项compile-fail预期均失败，证明旧constructor/accessor实际仍可编译。
  接入后4项readonly行为测试与3项API compile-fail全部通过。
- 私有spool原实现的production API red：10通过、2失败；129dentries实测389commits，
  9条输入末尾冲突后残留8identities/16locator+reverse records。
- 接入后`active-green01.log`157项packed通过、3项真实Btrfs测试仍ignored；后者必须
  通过独立真实provider harness执行，不能把ignored算通过。
  包含取消后sole connection释放、hardlink link-count rollback、allocation冲突当前
  chunk回滚/已提交prefix保留、poisonedfinish拒绝和owned目录清理。
- `source-changes.json`相对已签收frozen-source仅变更4个Rust文件：readonly、producer、
  source_namespace、spool；无新增/删除源码。完整47checks含原46项及API doctests。

统一签收记录为本批`checkpoint.json`，由`verify-checkpoint.py`核对：47项exit0；
default lib/bin1,023/1,107、overlay1,354、fixture7、vendor tokio/io-uring各13、
native/packed180、readonly compile-fail3全部通过。ACL/namespace/external/deep
raw/zstd共8次实挂载零错误并正常卸载。深路径harness修正TTL变量后独立重跑
`fuse-deep-source02`，不用首次变量拼写错误的运行证明TTL生效。真实Btrfs positive、
guard撤销与manifest上传后guard撤销各显式运行1项，通过并完成owned资源清理。
source/binary身份一致，无BrewFS mount/daemon/fixture/D-state。

相同固定binary的第三次`fuse-36k-batch01`仍在600秒fixture超时，未进入挂载。
`failed-source-diagnostics.json`以只读方式检查保留spool：source_inodes/source_paths
均36,003项、snapshot_inode已分配29,571/未分配6,432；records/inode_identities均0，
integrity_check=ok。证明本次卡在source编号逐条提交，尚未触达producer批量事务。
三次失败的work/spools/diagnostics均保留；不能把producer定向green当成36k接受。

代理SQLite-only机制重放保留默认FULL/DELETE，512实际记录两库内容digest完全相同；
2048→8commits、8200→40fdatasync仅解释提交机制，不是Rust importer或FUSE性能证据。
最终仍以固定binary、同36k规模/600秒期限的raw/zstd完整导入、分页、重启与正常卸载
为准；三次失败保留。下一批补source capture/master/visible/assignment的有界事务，
不关闭同步、不延长期限或缩减规模。原20秒teardown hang未解释，G06/G07和可写workspace生命周期
继续开放，最终实验campaign仍等待系统S/X。
