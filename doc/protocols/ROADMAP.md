# 协议网关路线图

本文档是多协议接入的滚动计划，随每个里程碑完成而更新当前状态。
设计总纲见 [README.md](README.md)；各协议 spec：[S3](s3-gateway.md) / [WebDAV](webdav.md) / [NFS](nfs.md)。

## 现状（2026-09）

- S3 网关 M1 已由 PR #83 合入：支持 single/multi bucket、核心对象操作、multipart、静态 SigV4
  密钥和流式数据路径；SQLite + LocalFS 上 76 项 boto3 E2E 与 22 项单元测试通过。
- **Redis metadata + RustFS data backend 尚未做 S3 gateway E2E**。仓库已有该后端组合的
  FUSE/POSIX、xfstests 与性能验证，但不能替代对 `brewfs gateway s3` 端点的协议验证。
- WebDAV MVP 已在 PR #88 实现并通过本地协议 E2E：认证/匿名、原子/direct-write、HTTPS、COPY/MOVE、LOCK/PROPPATCH、ETag/条件写和路径安全均有验证；litmus、davfs2、Windows WebClient、Finder、rclone 等真实客户端验收仍待完成。
- NFS 与 S3 完整兼容性仍按以下里程碑推进。

## 里程碑

| 里程碑 | 内容 | 验收标准 | 状态 |
|---|---|---|---|
| **M0** 设计立项 | 本目录全部 spec + 路线图 | 文档评审通过（PR 合入） | PR #82 评审中 |
| **M1** S3 网关 MVP | `brewfs gateway s3`：single/multi bucket、核心对象操作、multipart、静态密钥、流式数据路径及单实例一致性边界 | SQLite + LocalFS：76 项 boto3 E2E、22 项单元测试、并发/生命周期专项回归及 CI 全绿 | 已完成（PR #83） |
| **M2** WebDAV 网关 | `brewfs gateway webdav`：§3 方法全表、memls、Basic auth、TLS、原子写、dead properties、条件写 | 本地协议 E2E 与 focused tests 通过；litmus、davfs2、Windows 网络驱动器、Finder、rclone 仍需外部客户端验收 | 实现完成，兼容性验收待完成 |
| **M3** S3 兼容性与后端矩阵 | Redis + RustFS gateway E2E/重启/跨端验证；tagging、GetObjectAttributes、条件头、list/CopySource 扩展、CORS；带租约/fencing 的跨实例锁 | Redis + RustFS 跑同一 boto3 suite 且 0 failure；AWS CLI/`mc`/s3fs/presigned URL smoke；双网关故障注入无撕裂或删桶重建 | 未开始 |
| **M4** NFS 网关 | `brewfs gateway nfs`：NFSv3+mount 全映射表、squash、COMMIT 语义、`InodeAccess` 公开 wrapper | `mount -t nfs -o vers=3` 冒烟；xfstests NFS 子集；NFS ↔ FUSE 交叉验证 | 未开始 |
| **M5** 生产化 | 多身份/ACL/policy/STS、原生 TLS、三协议 tracing/Prometheus、CI compose 准入、部署文档与性能/soak 基线 | CI 全绿；安全模型评审通过；性能报告达到各 spec 目标；Redis + RustFS 24h soak 无数据或 metadata 丢失 | 未开始 |

## 远期（未排期）

- S3 高级数据语义：Versioning、SSE、Object Lock/Retention、Lifecycle、Notification、
  Replication、Inventory 和 SelectObjectContent；详细依赖见 [s3-gateway.md](s3-gateway.md) §12；
- SMB：FUSE + Samba 重导出文档（过渡）；原生实现待 Rust 生态或自研决策；
- NFSv4：见 [nfs.md](nfs.md) §9；
- NLM：见 [nfs.md](nfs.md) §6；
- HDFS/Java SDK：需先稳定 C ABI（cbindgen over SDK Client）；
- console 与网关打通：Web 文件管理器、网关实例注册与监控（复用 session/heartbeat）。

## 工作方式约定

- 每个里程碑 = 至少一个 PR：spec 先行（本目录），代码随后；行为变更、测试、文档同 PR；
- 默认开发 e2e 可在本地 WSL（Ubuntu-22.04）使用 SQLite + LocalFS + 真实客户端复现；
- 每个已宣称支持的生产后端组合必须单独跑协议 endpoint E2E；底层 FUSE/VFS 测试不能替代网关测试；
- S3 的 Redis + RustFS 最小 smoke 纳入普通 CI，完整回归、重启和故障注入进入定时/手动 workflow；
- 依赖 crate（s3s/dav-server/nfsserve）版本升级需单独 PR 并附兼容性说明；
- 协议互通性回归：任何涉及 VFS/meta 的核心改动，需跑 `tests/` 下协议集成测试冒烟。
