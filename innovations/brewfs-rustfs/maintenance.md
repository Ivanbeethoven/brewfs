# Upstream maintenance and integration

## 维护对象与版本

| 仓库/分支 | 用途 |
|---|---|
| [brewfs/brewfs main](https://github.com/brewfs/brewfs) | BrewFS 正式上游；本轮沿用已有 [PR #167](https://github.com/brewfs/brewfs/pull/167) 集成 |
| [Ivanbeethoven/brewfs codex/rustfs-ec-block-size-hint](https://github.com/Ivanbeethoven/brewfs/tree/codex/rustfs-ec-block-size-hint) | 本轮维护分支；代码基于上游 `3b6f528`，受测实现为 `687925b` |
| [Ivanbeethoven/rustfs perf/frame-aware-batch-read](https://github.com/Ivanbeethoven/rustfs/tree/perf/frame-aware-batch-read) | 已验证服务器依赖，固定 `cc4926f` |
| [Ivanbeethoven/rustfs main](https://github.com/Ivanbeethoven/rustfs) | 官方 RustFS 主线镜像；2026-10-19 快进到 `9a1f3d2c4cd5dfe3335a7af142e317f4d0dffcd8` |
| [rustfs/rustfs main](https://github.com/rustfs/rustfs) | 官方服务器上游；同步主线不代表已经包含或验证本研究扩展 |

RustFS 官方主线在受测基底 `7147063` 后已有其他更新，不能把主线直接替换成 `cc4926f` 的等价实验依赖。当前账户能维护自己的 fork，但没有直接向 `rustfs/rustfs` 推送权限；本轮没有声称扩展已合入 RustFS 官方主线。

历史 `feat/dynamic-block-size` 位于 `a9e4a36`，与受测 `perf/frame-aware-batch-read` 分歧；不要只凭名字将其当作最新受测版本。研究源自 marshawcoco 的 dynamic-block-size 工作，BrewFS 最初适配提交为 `e779c07`；`de77445` 增加 auto，`e5fec71` 增加完整 GET/LRU，`687925b` 完成写预算、共享回复和 auto 修正。

BrewFS fork 的旧 `main` 包含 `85d2e15`、`7b94b64` 两个历史独有提交，其中有大量压缩后的 packed/overlay 改动。它和官方主线整体合并存在跨模块冲突。本轮从官方主线衍生的维护分支及 PR #167 没有该冲突；不以强推覆盖旧 main 来伪装同步，旧提交保留。后续开发应明确选择官方 main 或上述维护分支。

## 已验证边界与合并原则

本次文档整理只增加说明、固定结果与图表，并纠正 YAML 示例/配置文档仍写成 4 MiB auto 的注释；没有改变受测源码。合并前检查上游是否仍为受测基底，确认所有源码、Cargo/lockfile、vendor 和构建配置未变，再复用已有测试。若出现新代码或冲突解决，须为受影响路径补测，不能沿用旧二进制结果。

现有上游 PR 的早期说明曾记录：all-features 测试 14 项失败，其中 13 项在独立上游 overlay 过滤运行中复现，另一个 ACL/atime 并发失败未被该基线复现；overlay clippy 有两类诊断，operator clippy 有 32 个 `double_must_use` 诊断。这些历史检查未在本轮重新宣布通过。当前默认/hint 工作区测试及具体通过项见 [Evaluation](evaluation.md)，它们不等于 all-features、完整 POSIX 或所有 CI 通过。

上游合并使用明确的 PR head SHA，保留完整技术和验证说明。按本项目要求，运行验证使用 ACS，复用固定二进制及已闭合结果，不等待 GitHub Actions 来替代本地/ACS 验证。合并后检查目标 main 包含提交，运行源码树与受测版本一致；PR 最终合并状态及 merge SHA 以 GitHub 为准。

## 下一次同步 RustFS 的方法

1. 从固定 `cc4926f` 新建独立集成分支，记录目标官方 SHA；不移动受测依赖引用来覆盖旧证据。
2. 检查 B hint 的 PUT/MPU、HEAD 和布局持久化；检查内部兼容键、旧对象读取、healing 和 q 帧边界。官方变更如替换相关读路径，应保留源码级差异记录。
3. 编译后运行受影响正确性检查；只在确有代码变化时补充同资源、同对象、同缓存和同计时口径的 BrewFS FUSE 对照。保留所有未达标结果。
4. 确认收益及正确性后，再更新 BrewFS 文档中的推荐 RustFS SHA 和二进制指纹。将 upstream 同步、机制修改、实验结果分清楚。

本轮不自动合并历史实验分支中的所有候选。小 q、不同 syscall/mmap 策略和调度改动的独立结果不能被本轮整文件验证替代。

## 部署与回退

受测 ACS 二进制使用：

```sh
cargo build --locked --release --bin brewfs --no-default-features \
  --features fuse-tokio-runtime,gateway-s3,gateway-webdav,rustfs-ec-block-size-hint
```

运行时使用 `--s3-rustfs-ec-block-size-hint auto` 或 YAML `data.s3.rustfs_ec_block_size_hint: auto`。ACS 对应内核采用 Tokio FUSE 运行时；这不是要求所有部署关闭 io_uring。

RustFS 的本轮公开环境开关记录在 [provenance.json](evidence/provenance.json)。凭据通过环境或 Secret 提供，不写入本目录。应检查实际对象 HEAD/四盘布局，不只检查配置是否被接受。

省略 hint 运行时配置可退出依赖该模式的完整 GET/LRU 策略；编译时去掉 feature 可排除扩展请求字段。已创建对象的布局必须继续由兼容读者识别。写预算释放及缓存回复共享属于通用数据路径改进，不会仅因省略 hint 就自动回滚；完整回退需选用较早 BrewFS 版本。

临时资源清理必须先导出原始记录及校验和，再按创建清单核对资源身份并删除。当前实验记录、图表和受测版本保留，运行实验的临时 Pod/PVC/服务/Secret 及四块云盘已删除。
