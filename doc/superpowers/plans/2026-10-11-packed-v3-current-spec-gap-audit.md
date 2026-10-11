# packed-v3/005 当前 SPEC 差距审计

更新时间：2026-10-11  
工作区：/home/hxy/brewfs  
分支：codex/packed-metadata-aliyun-20260930  
审计基线：HEAD 98f292e1，工作树干净且与 origin 同步  
关联交接：[2026-10-10-packed-v3-temporary-handoff.md](2026-10-10-packed-v3-temporary-handoff.md)  
性能记录：[2026-10-11-packed-v3-performance-smoke.md](2026-10-11-packed-v3-performance-smoke.md)

## 审计结论

本轮只接受 packed-v3/005；v2、旧 wire、flat、CR 和 v5 兼容不属于验收范围。当前提交已完成 Redis CAS 竞争退避、TiKV bounded scan 固定时间戳 key-lock 重建，以及云 runner 的 packed payload 和 LF 输入约束。它们修复了已知失败模式，但没有把任意局部测试升级为系统或性能验收。

最新云端 RustFS/S3 smoke 在 32 GiB ECS 上完成 tree、stat、smallfiles 三个 100 文件 workload，均为 0 errors。这只证明小规模 packed-v3/005 对象读取路径和 runner 账本可用。Redis/TiKV 元数据完整生命周期、TiKV 并发 C8、operator recovery、GC、native/packed paired 实验和正式性能接受仍未完成。

## G01–G17 当前分层

| 层级 | 项目 | 当前可签收范围 | 必须补的出口 |
| --- | --- | --- | --- |
| 窄契约已具备 | G01、G06、G07、G09、G15、G16 | v3/005 负向语义、HTTP terminal 账本、admission close、singleflight/profile/cancel、placement/p90 控制、runner provenance/resource journal 的局部测试 | 真实多后端、生产 FUSE、release 多次重复、端到端 cleanup 和 paired 接受规则 |
| 实现与局部证据 | G02、G03、G04、G05、G08、G10、G11、G12、G13、G14 | source/sparse/ACL、分页库存、统一 executor、binding、generation fence、same-head publication、PWB root、capability/CRD 的窄子项 | Redis/TiKV/RustFS 实际组合，挂载并发、发布恢复、pins、object graph GC、Kubernetes/operator 生命周期 |
| 完全开放 | G17 | 没有当前 HEAD 的完整三创新 paired acceptance | AGENTS gate、消融、固定 trace、native/packed 同 placement、结果归档和最终接受/拒绝记录 |

表中“窄契约”只描述可复核的局部行为；不能用它关闭相应完整 SPEC。旧阶段的 stores、diagnostic、local debug 结果继续保留为历史证据，不能与 HEAD 98f292e1 的云端结果拼成新通过批次。

## 云端测试政策与实际证据

后续性能和集成测试统一使用独立云主机 ECS i-bp138fribj3vquxxrpe6（8 vCPU、32 GiB、系统盘 100 GiB、数据盘 512 GiB，挂载 /mnt/brewfs-data），自动释放时间为 2026-10-13T09:56Z。OSS bucket 为 brewfs-tikv-100k-20260928。每次运行必须记录 ECS 规格、数据盘、commit、binary SHA-256、固定 trace、后端 namespace/prefix、阶段耗时和 cleanup proof，并在每次 run 使用独立 prefix/namespace。

当前云端 task t-hz06zpl80x3h9mo 使用 HEAD 98f292e1 的 release binary（SHA-256 b7757759b764bd67468efc607affa3a93b5f3cb6fb8538bd5a1ace99e12c72f2），完成：

- packed-tree：100/100、0 errors、discovery 5.140589 s；
- packed-stat：100/100、0 errors、p50 18.968117 ms、p95 31.102575 ms；
- packed-smallfiles：100/100、0 errors、102400 payload bytes、p50 43.936339 ms、p95 68.252816 ms；
- 三项共享 trace SHA-256 c86a7f80931cce89ad640ae0b222788da402093694c433174ecba369bd2c8662。

canonical proof 为 [cloud-proof-summary.txt](../../../docker/compose-xfstests/artifacts/aliyun-cloud-smoke-20261011/cloud-proof-summary.txt)，原始输出为同目录 remote-output.log，本地同步目录为 work/aliyun-cloud-rerun-98f292e。该 smoke 不含 Redis/TiKV metadata lifecycle、native/packed paired、operator、GC 或三次重复的接受统计。

## SPEC 级缺口和关闭条件

1. **TiKV 竞争出口**：用 HEAD 98f292e1 的新 binary 在云端执行 control-only C8，必须得到 concurrent-fork 8/8、concurrent-seal 8/8、无未知 bounded-read diagnostics，并保留 metrics、日志和 cleanup proof。官方 readiness 镜像若网络失败，必须单独记录环境阻断，临时 readiness workaround 不能自动算通过。
2. **Redis/TiKV 真实生命周期**：在同一固定 trace 上完成 S0→fork A/B→mutation→durable drain→seal/repack→S1 publish→remount/next-fork→GC，分别记录 authority、journal、reader pins、旧 head retirement 和无误删结果。
3. **RustFS/S3 完整对象出口**：补 parent context、namespace/codec、external digest、journal/atomic rotation、pins、orphan 和 packed GC；当前 tree/stat/smallfiles 只是读取 smoke。
4. **三创新 paired 实验**：native 与 packed 使用同一 immutable placement、executor、trace、资源预算和 release binary；至少重复三次，报告 active、drain、unmount、GC 分段耗时及 p50/p95/p99。必须完成 static/dynamic、inline-on/off、metadata/data codec 和 p90 policy 消融。
5. **operator 与交付门禁**：完成 API-server/CEL、Redis/TiKV Kubernetes E2E、conditions/finalizer/lease、leader loss、recovery 和 PVC/数据盘清理；CI 的 Redis、TiKV、Rust 和必需检查全部通过后才能合并 PR #161。
6. **managed-backend SPEC 对齐**：2026-09-17 managed-backend spec 要求 OSS 与 Aliyun Redis/Tair、同规格 ECS、三次重复、JuiceFS 对照和 Result Vault。当前云端 RustFS/S3 smoke 不满足这些托管后端条件，也没有 JuiceFS paired 结果；在配置和证据齐全前不能把草案标为完成。

## 资源和证据卫生

canonical proof、失败诊断和当前交接文档必须保留。测试结束后删除临时 ECS、数据盘、Compose/镜像和未绑定上传对象；可重建的 target 与缓存可清理以保证磁盘空间。任何 cleanup 失败都应使 run 失败并保留诊断，不得用成功 workload 结果覆盖资源残留。
