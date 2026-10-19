# Evidence index

此处保存已闭合实验的完整汇总统计、四轮绝对值和 FUSE 请求形状。JSON 是审计输入；面向阅读的解释和图表见 [Evaluation](../evaluation.md)。文件从已审计交付物直接复制，没有重采样或修改数值。

| 文件 | 内容 |
|---|---|
| `policy-summary.json` / `policy-comparisons.json` | 最终 88 组；62 个绝对汇总单元、31 项比较 |
| `resident-summary.json` / `resident-comparisons.json` | 64 组真实缓存热读；16 个汇总单元、8 项比较 |
| `summary.json` / `comparisons.json` | 第一阶段 184 组；130 个汇总单元、81 项比较，包含原始组合及固定 1 MiB EC 控制 |
| `fuse-request-sizes.json` | 八组实际 FUSE 请求大小诊断 |
| `export.json` / `delivery-audit.json` | 各阶段完整性计数和独立统计审计结果 |
| `provenance.json` | 源码/二进制/计划指纹、资源规格与公开服务器开关 |
| `adverse-prefetch-rejected.json` | 后续维护撤回的预取候选：36 组、25 项比较 |
| `adverse-publication.json` | 后续并发修复主矩阵：136 组、49 项比较，29 项通过、20 项未通过 |
| `adverse-resident-hot.json` | 后续并发修复驻留热读：64 组、8 项比较，全部通过门槛 |
| `adverse-audit.json` | 后续三阶段独立审计：82 项比较、229,792 次完整文件哈希 |

`ratio` 是候选/基线的同轮吞吐几何平均；`ci` 是其 95% 区间；p95 比越低越好。`passed` 同时要求吞吐和 p95 门槛，`clear_gain` 只描述明确吞吐提升，不能把这两个字段互换。

## Raw records

完整原始归档为独立交付物 `brewfs-write-hot-hint/raw-records.tar.gz`，约 57 MB，包含 7,499 个已校验文件。为避免把编译日志、云资源身份、临时实验脚本和大量原始记录塞入产品仓库，归档独立保留；此目录保存其不可变指纹和完整统计。

SHA-256：`e4fbe5d14430bc480017d3cf2224d427bb2c53ac3027823ae1c0b3c6c1a9b3d5`。

原始归档保留失败、中止、已撤回候选、布局检查器修正及观察连接恢复记录。正式比较阶段之间没有样本拼接。独立中文 HTML 报告 `BrewFS写入热读与自动提示.html` 和全部八张 PNG/SVG 图也随该交付物保留。

后续维护独立归档为 `brewfs-adverse-maintenance/raw-records.tar.gz`，共 3,932 个已校验文件，SHA-256：`ad764df184ae0e0647073efc67cb0652145f393d01797c297b1e78688029cc4a`。对应中文报告为 `不利维度维护结果.html`，结论是保留已验证的并发缺陷修复，尚未通过全部性能门槛；详见 [维护说明](../adverse-maintenance.md)。
