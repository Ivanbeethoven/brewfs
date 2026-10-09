# Packed-v3 Linux readonly ACL 验证（2026-10-04）

状态：ACL实挂载通过；完整门禁发现一项测试身份错误，已修正，待重新完整验收。
另一次namespace卸载超时原因仍开放，尚未签收。用户已将目标收敛至v3；
v2/其他历史兼容不再是完成条件。本文是正确性验证，不声明性能收益或完整系统完成。

## 实际问题和实现

此前源文件的Linux access ACL授予uid1000读权限，BrewFS挂载却返回EACCES，ACL查询
返回EOPNOTSUPP。`grant-red02/`保存源ACL字节、源/mount结果、正常卸载和旧binary身份。
启用用户态权限算法仍不够：默认mount已有kernel default_permissions，需同时协商
FUSE_POSIX_ACL，否则kernel会先按mode拒绝named-user grant。

新增独立Linux xattr v2 canonical codec，不改变BrewFS control JSON/AclRule的语义。
codec要求base singleton、named身份排序/唯一、mask规则、合法rwx/ID/version/length；
冷属性编码及读取验证access ACL与hot mode一致、symlink禁止access ACL、default仅目录。
source、producer和reader均校验；root在finish再校验，防止冷属性提交后改root mode。

MetaLayer声明独立POSIX ACL capability。当前v3冻结适配器声明ReadOnly；可写后端
维持Unsupported。支持的ACL通过get/list返回，size probe/ERANGE按FUSE契约；readonly
set/remove返回EROFS。冷属性除显式查询外，也可以为权限决策读取，不能宣称普通open
绝不取cold。ACL冷对象缺失、认证损坏或上下文不符返回EIO，不退回mode授权。

权限算法保留owner/named-user优先、mask和matched-group拒绝；单次组合操作必须由
一个匹配group条目授予全部请求位，不能把两个group各自的read/write拼成O_RDWR。
真实Linux源oracle `source-group-oracle.json`证明单独read/write成功，组合拒绝。

Linux还有zero-group快捷路径：hot mode的group bits为0时，内核跳过named ACL分支，
按owning-group或other权限判断。首轮实现遗漏它；`fuse-zero-mask-red/`证明named
user+mask0+other-read的源可读、mount拒绝。修复保留cold完整性检查，再按该Linux
分支判定；owning-group成员仍拒绝。该问题不是被删去的实验异常。

补充组从有界/proc status读取，验证完整real或filesystem uid/gid对；失败、字段
缺失、交叉/身份不符均拒绝。owner/named-user已可决定时不用额外读取/proc。
只验证第四项fsuid/fsgid也有错误：access()以real身份检查，而/proc显示持久身份。
`fuse-identity-red/`保存real31000/effective0的源access成功、mount失败。修复允许
完整real身份对；单测继续拒绝交叉组合。内部pid0请求只有传入gid，无补充组。

## 保留的开发和中断证据

artifact根为 `docker/compose-xfstests/artifacts/packed-v3-completion-20261004-acl/`。

- `cold-red.log`证明旧冷编码器接受畸形ACL；修复后的codec/context测试覆盖拒绝。
- `fuse-green/`在准备root-owned硬链接时触发Linuxprotected_hardlinks，尚未挂载；
  修复为chown前建立alias。`fuse-run02/`的source oracle暴露zero-mask规则遗漏。
- `fuse-run03/`和`fuse-final/`早期成功未覆盖两个后续反例，不作为最终接受。
- 用户中断终止了首轮gate和namespace/external脚本。仅33项gate完成，source身份
  未变但不作为最终门禁；namespace残留mount正常卸载，两个owned临时目录经范围
  核对后清理。记录为`interruption-recovery.json`，中断日志保留。
- 两个后续权限修复之后源码重新冻结，完整40项由`final-gate02/`从头运行。
  实挂载和binary身份将绑定到同一候选，不拼接中断前后的不同源码结果。
- `final-gate02/`全部40项已执行，只有overlay完整库测试失败：手工请求的uid/gid
  与helper内的PID42不对应，可信`/proc`校验正确拒绝。现有raw ancestor测试已统一
  用pid0内部请求，授权与拒绝分支都仅使用给定primary group；定向1项green已保存于
  `raw-permission-test-green.log`。生产身份校验未放宽；该失败gate不算接受。
- `fuse-final02/`的raw/zstd ACL、`fuse-external-final/`的raw/zstd external均通过。
  `namespace-final/`raw全部540-entry数据/属性/九类EROFS成功，但正常卸载之后20秒
  daemon未退出，被kill，作为失败保留。`namespace-final03/`debug raw/zstd通过，
  此后`namespace-repeat-01`至`06`按原日志级别串行raw/zstd共12次正常退出。
  复测通过没有解释第一次挂起；不放宽超时，不据此隐去失败。

## 最终验收与剩余范围

待新完整gate和退出问题验收后，由 `verification.json`验证raw/zstd ACL source/mount oracle、既有namespace
与external回归、源码/binary身份、正常卸载和资源状态后，在此填写结果。

本批只签收readonly ACL access和default bytes保留；继承/chmod的纯算法单测不能
证明可写端实现。原子mode/xattr、同版本权限读取、create/default继承、umask接线、
缓存revoke和独立实例并发继续属于G04。Frozen source、深cookie、完整metrics/预算、
同executor/pipeline、workspace publication/recovery/GC/operator及实验/交付继续开放。
另有既有typed-stat错误映射缺口：`stat_ino`的`.ok().flatten()`会把hot index/backend
错误变成None，FUSE xattr前置guard因此返回ENOENT。它没有授权绕过，但不等于完整
认证错误EIO契约。后续两项真实fixture负测均复现ENOENT而非EIO，typed stat/ancestor
lookup修复已接入，green与完整验收进行中；证据为
`packed-v3-completion-20261004-typed-stat/`，不由本批cold错误测试推断。
