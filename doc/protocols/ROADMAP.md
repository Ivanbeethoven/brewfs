# 协议网关路线图

本文档是多协议接入的滚动计划，随每个里程碑完成而更新当前状态。
设计总纲见 [README.md](README.md)；各协议 spec：[S3](s3-gateway.md) / [WebDAV](webdav.md) / [NFS](nfs.md)。

## 现状（2026-09）

- BrewFS 对外接口：FUSE mount、console（管理面）、control plane；无任何数据面协议网关。
- `doc/gap/` 将 `gateway`/`webdav` 标为 P2（"视产品方向"）、`06-roadmap.md` 建议"核心稳定后再做网关"。
  本路线图正式立项推进，理由：核心 POSIX 正确性基线已达成（708 xfstests 全后端通过，
  见 README「POSIX Correctness」），协议网关反向成为生态短板。

## 里程碑

| 里程碑 | 内容 | 验收标准 | 状态 |
|---|---|---|---|
| **M0** 设计立项 | 本目录全部 spec + 路线图 | 文档评审通过（PR 合入） | ✅ 本 PR |
| **M1** S3 网关 MVP | `brewfs gateway s3`：单 bucket、核心对象操作（Put/Get/Head/Delete/List/Copy）、multipart 全流程、静态密钥、流式数据路径 | 集成测试（aws-sdk-s3 断言）通过；`aws s3 cp/ls/rm` + `mc` e2e 通过；S3 写入 ↔ FUSE 读取交叉验证 | 🚧 进行中 |
| **M2** WebDAV 网关 | `brewfs gateway webdav`：§3 方法全表、memls、Basic auth、TLS | `litmus` basic/copymove/props/locks 全过；davfs2 挂载读写；Windows 网络驱动器映射冒烟 | ⬜ |
| **M3** S3 完整化 | 多 bucket、tagging、GetObjectAttributes、条件头、跨实例锁（`/.brewfs.sys/locks/`）、CORS、rustls | `mc` 全功能回归；多实例并发写同一 key 无撕裂 | ⬜ |
| **M4** NFS 网关 | `brewfs gateway nfs`：NFSv3+mount 全映射表、squash、COMMIT 语义、`InodeAccess` 公开 wrapper | `mount -t nfs -o vers=3` 冒烟；xfstests NFS 子集；NFS ↔ FUSE 交叉验证 | ⬜ |
| **M5** 生产化 | 三协议指标（tracing/Prometheus 埋点）、CI 准入矩阵接入 compose、部署文档（systemd/k8s/operator sidecar）、性能基线报告 | CI 全绿；性能报告达到各 spec §性能目标 | ⬜ |

## 远期（未排期）

- SMB：FUSE + Samba 重导出文档（过渡）；原生实现待 Rust 生态或自研决策；
- NFSv4：见 [nfs.md](nfs.md) §9；
- NLM：见 [nfs.md](nfs.md) §6；
- HDFS/Java SDK：需先稳定 C ABI（cbindgen over SDK Client）；
- console 与网关打通：Web 文件管理器、网关实例注册与监控（复用 session/heartbeat）。

## 工作方式约定

- 每个里程碑 = 至少一个 PR：spec 先行（本目录），代码随后；行为变更、测试、文档同 PR；
- 所有 e2e 验证可在本地 WSL（Ubuntu-22.04）复现：sqlite meta + local-fs data + 真实客户端；
- 依赖 crate（s3s/dav-server/nfsserve）版本升级需单独 PR 并附兼容性说明；
- 协议互通性回归：任何涉及 VFS/meta 的核心改动，需跑 `tests/` 下协议集成测试冒烟。
