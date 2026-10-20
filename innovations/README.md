# Innovations

本目录记录本项目已实现的技术改进、源码位置、实验依据、失败假设与维护边界。“创新”指项目内的设计与工程贡献，不作算法首创或所有场景最优的声明。

## BrewFS × RustFS 整文件读写协同

| 阅读内容 | 文档 |
|---|---|
| 技术链路、决策规则、内存所有权、服务端配合 | [Design](brewfs-rustfs/design.md) |
| 测试对象、标准、绝对结果、图表与不利结果 | [Evaluation](brewfs-rustfs/evaluation.md) |
| 上游关系、受测版本、合并与后续更新方法 | [Maintenance](brewfs-rustfs/maintenance.md) |
| 并发修复、撤回的预取方案与仍未达标的性能维度 | [Adverse-dimension maintenance](brewfs-rustfs/adverse-maintenance.md) |
| Agent 混合负载、FUSE 端到端与 no-dynamic-block 对照方案 | [Agent mixed FUSE plan](brewfs-rustfs/agent-mixed-fuse-plan.md) |
| Hint/缓存/写回 review、恢复初始化修复与拒绝候选记录 | [Code review](brewfs-rustfs/code-review.md) |
| 三场景六档混合文件的完整结果、图表及剩余瓶颈（44/45 项门槛通过） | [Agent mixed FUSE results](brewfs-rustfs/agent-mixed-fuse-results.md) |
| 完整机器可读统计及版本指纹 | [Evidence](brewfs-rustfs/evidence/README.md) |

本轮测试入口是 **BrewFS 的真实 FUSE 挂载目录**。应用执行完整文件写入和读取，经 BrewFS 缓存、Redis 元数据与 S3 接口访问 RustFS；运行环境为 Aliyun ACS。单独的 RustFS HTTP 测试用于校验协议及布局，不能代替这些文件系统性能结果。

原性能收益对应的受测组合为 BrewFS `687925b724f3550a47cc530f875e0470a693a022` 与 RustFS `cc4926fac1107aee76540f28bf22dbd47fcbca24`。后续缓存发布与孤立预取的并发修复单独测试，结果和未达标项目见 [不利维度维护](brewfs-rustfs/adverse-maintenance.md)，不能把原收益直接归因于这些修复。本文档集整理的是该数据路径的改进，其他 packed metadata / workspace 设计仍以 [现有文档索引](../doc/README.md) 为准。
