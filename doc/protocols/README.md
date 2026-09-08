# BrewFS 协议网关总体设计

本文档是 BrewFS 多协议接入的总设计，定义协议网关层的公共架构、共用约定与协议选型。
各协议的详细 spec：

| 协议 | Spec | 状态 |
|---|---|---|
| S3 Gateway | [s3-gateway.md](s3-gateway.md) | 已立项，首个实现目标 |
| WebDAV（网盘） | [webdav.md](webdav.md) | 已立项 |
| NFS | [nfs.md](nfs.md) | 已立项 |
| 其他协议评估 | 见本文 §6 | SMB / SFTP / HDFS 等 |

## 1. 背景与目标

BrewFS 目前对外只有两类入口：

- **FUSE mount**：POSIX 接口，需要内核 FUSE 与挂载权限；
- **console / control plane**：管理面，不是数据面。

JuiceFS 在此之上的成熟实践是 `juicefs gateway`（S3）与 `juicefs webdav`：同一套文件系统核心，
不经内核，直接以网络协议对外提供数据访问。本设计为 BrewFS 补齐这一层，目标是：

1. **同一 volume，多协议并存**：FUSE、S3、WebDAV、NFS 可同时挂在同一份 meta/data 后端上；
2. **不经过内核 FUSE**：网关进程内直接构造 `VFS`/`Client`，避免 FUSE 上下文切换与挂载权限要求；
3. **协议语义忠实**：不是"能通就行"，每个协议都要定义清楚一致性语义、错误码映射与不支持的特性；
4. **可测试**：每个协议都有客户端工具的端到端验证（aws cli / mc、litmus / davfs2、mount -t nfs）。

非目标：

- 不做协议网关的横向扩展调度（多网关实例的负载均衡由部署层解决，见各 spec 的一致性章节）；
- 不在网关层实现对象存储后端能力（versioning、对象锁、生命周期等，见 S3 spec 的显式排除项）。

## 2. 分层架构

```
┌─────────────────────────────────────────────────────────┐
│  FUSE mount   │  S3 gateway  │  WebDAV  │  NFS server   │   ← 前端（平级）
├─────────────────────────────────────────────────────────┤
│  SDK Client (path, std::fs 风格)  │  VFS (inode/handle)   │   ← 消费层
├─────────────────────────────────────────────────────────┤
│  MetaClient 缓存 │ Chunk/Block 数据路径 │ 读写缓存/写回     │
├─────────────────────────────────────────────────────────┤
│  MetaStore (sqlite/postgres/redis/etcd/tikv)            │
│  ObjectBackend (localfs / S3 兼容对象存储)                │
└─────────────────────────────────────────────────────────┘
```

代码布局（新增 `src/gateway/`，与 `src/fuse/` 平级）：

```
src/gateway/
├── mod.rs            # 公共：GatewayContext 构造、系统目录、xattr 常量、锁
├── s3/               # S3 网关（s3s crate）
├── webdav/           # WebDAV 网关（dav-server crate）
└── nfs/              # NFS 网关（nfsserve crate）
```

### 2.1 消费层选择

BrewFS 核心对前端暴露三层抽象（详见 `doc/architecture/arch.md` 与源码）：

| 层 | 类型 | 特点 | 适用协议 |
|---|---|---|---|
| SDK | `Client` / `ClientBackend`（`src/sdk_fs.rs`） | path-based、`io::Result`、公开稳定 API | WebDAV、S3 |
| VFS | `VFS<S, M>`（`src/vfs/fs/mod.rs`） | path-based 公开 + inode/handle 内部 API | NFS（需要 inode 文件句柄） |
| FUSE | `impl asyncfuse::raw::Filesystem for VFS` | 内核挂载 | —（已存在） |

约定：

- **WebDAV / S3 网关基于 `Client`**：路径天然映射 URL，无需 inode；
- **NFS 网关基于 `VFS`**：NFSv3 的文件句柄需要稳定的 inode 号，且是无状态协议（无 open/close），
  需要直接使用 VFS 的 inode 级 API。当前 `VFS` 的 `*_ino` / `*_at` 方法为 `pub(crate)`，
  NFS 实现时需要：提升可见性，或新增公开 wrapper（推荐后者，见 [nfs.md](nfs.md) §3）。

### 2.2 CLI 形态

沿用现有 clap derive 结构（`src/config.rs` 的 `Command` 枚举），新增一个子命令树：

```bash
brewfs gateway s3     --listen 0.0.0.0:9000 --access-key ... --secret-key ... \
                      --meta-url sqlite:///tmp/meta.db --data-backend local-fs --data-dir /data
brewfs gateway webdav --listen 0.0.0.0:9001 [--user u --password p] <同一批后端参数>
brewfs gateway nfs    --listen 0.0.0.0:2049 [--squash root|all|none] <同一批后端参数>
```

- 后端连接参数（`--meta-url` / `--data-backend` / `--data-dir` / S3 后端参数 / 缓存参数）与
  `mount` 子命令完全复用同一套定义，保证"mount 能连的 volume，gateway 就能连"；
- 每个协议是独立 Cargo feature：`gateway-s3`、`gateway-webdav`、`gateway-nfs`，默认全部开启，
  可用 `--no-default-features --features gateway-s3` 裁剪二进制；
- 网关进程注册到与 mount 相同的 session/heartbeat 机制（meta 层已有 session 概念），
  使 `info`/`console` 能看到网关实例。

## 3. 共用约定

所有协议网关共享以下约定，集中在 `src/gateway/mod.rs` 定义常量与工具函数。

### 3.1 系统目录 `/.brewfs.sys/`

网关内部状态统一放在 volume 根下的隐藏目录（类比 JuiceFS gateway 的 `/.sys`）：

```
/.brewfs.sys/
├── s3/
│   ├── uploads/<hh>/<upload-id>/     # S3 multipart：每个 part 一个普通文件
│   │   ├── part-1, part-2, ...       #   part 数据（xattr 记录 etag）
│   │   └── .target                   #   xattr: 目标 bucket/key
│   └── tmp/<uuid>                    # PutObject/Complete 的暂存文件
├── webdav/                           # WebDAV 暂无内部状态
└── locks/                            # 跨网关实例的分布式锁文件（后续）
```

- 所有协议的 list 操作必须在根目录过滤 `.brewfs.sys`；
- FUSE 挂载下该目录可见但应视为内部实现（文档中声明不要直接使用）；
- GC 约定：超过 24h 未更新的 `uploads/` 与 `tmp/` 条目由网关的后台清理任务回收
  （等价 JuiceFS gateway 的 `cleanup()` 周期任务）。

### 3.2 xattr 命名

协议元数据一律存 xattr，前缀 `brewfs.<proto>.`：

| xattr | 内容 | 协议 |
|---|---|---|
| `brewfs.s3.etag` | 对象 ETag（小对象 MD5 或 multipart 的 md5-of-md5s-N） | S3 |
| `brewfs.s3.meta` | JSON：`x-amz-meta-*` 用户元数据 + 白名单系统头 | S3 |
| `brewfs.s3.tags` | JSON：对象 tagging | S3 |
| `brewfs.s3.dirobj` | 空值标记：该目录是 S3 "目录对象"（key 以 `/` 结尾显式创建） | S3 |
| `brewfs.dav.deadprops` | JSON：PROPPATCH 的 dead properties | WebDAV |

选择 xattr 而非 JuiceFS 的 `atime==0` 目录对象标记，原因：xattr 语义显式、不污染时间戳、
FUSE/其他协议下行为可预期。JuiceFS 的 atime 技巧在 spec 中仅作为参考记录。

### 3.3 原子发布

所有"完整对象/文件落位"的写路径（S3 PutObject、CompleteMultipartUpload、CopyObject、
WebDAV PUT 可选开启）遵循同一模式：

1. 写入 `/.brewfs.sys/<proto>/tmp/<uuid>`；
2. 数据落盘（按协议语义决定是否 fsync/flush）；
3. `rename` 到最终路径（BrewFS rename 具备原子性，xref `RENAME_NOREPLACE` 修复 #81）；
4. 失败路径清理 tmp。

### 3.4 一致性语义

- **单网关实例**：直接走 meta 事务，读写强一致（等同 FUSE 单挂载点）；
- **多实例（同 volume 多网关 / 网关与 FUSE 并存）**：以 MetaClient 缓存失效粒度为界，
  提供 close-to-open 一致性；协议层不额外承诺。各 spec 中写明该协议可见的弱化点
  （如 S3 list 可能短暂滞后、NFS 客户端属性缓存）；
- **写冲突**：单实例内用进程内锁（dashmap 按 key 分片）；跨实例写同一 key 的串行化
  依赖 meta 层 rename/create 的原子性，跨实例分布式锁（flock on `/.brewfs.sys/locks/`）
  列入后续里程碑（参考 JuiceFS `NewNSLock` 用 meta.Flock 的实现）。

### 3.5 认证与身份

| 协议 | MVP | 后续 |
|---|---|---|
| S3 | 静态 access-key/secret-key（flag 或 env），SigV4 由 s3s 处理 | 多租户密钥对、STS 兼容层 |
| WebDAV | HTTP Basic（flag 或 env）；可关闭 | 复用 console 的 Bearer token、HTTPS |
| NFS | AUTH_SYS + squash 选项（root_squash 默认） | Kerberos（依赖 crate 生态，暂不承诺） |

身份到文件系统的映射：网关操作以可配置的 `uid/gid`（默认 root）调用核心；
NFS 按 AUTH_SYS 凭据 + squash 规则映射到 `CallerIdentity`（`src/fs.rs` 已有该结构，
当前为 `pub(crate)`，随 NFS 里程碑一并公开）。

### 3.6 错误码映射原则

每个 spec 附完整映射表，总原则：

- `NotFound` → S3 `NoSuchKey/NoSuchBucket`、WebDAV 404、NFS `NFS3ERR_NOENT`；
- `PermissionDenied` → 403 / 403 / `NFS3ERR_ACCES`；
- `AlreadyExists` → 409（S3 按操作细分）/ 405 或 412 / `NFS3ERR_EXIST`；
- `DirectoryNotEmpty` → S3 `BucketNotEmpty` / WebDAV 409 / `NFS3ERR_NOTEMPTY`；
- 其余内部错误 → 500 / 500 / `NFS3ERR_IO`，并带 request id 记 tracing 日志。

## 4. 技术选型

| 协议 | crate | 版本 | 说明 |
|---|---|---|---|
| S3 | [`s3s`](https://crates.io/crates/s3s) | 0.15 | S3 Service Adapter：实现 `S3` trait 即得完整 HTTP/SigV4 服务；RustFS 同源框架，生态对齐 |
| WebDAV | [`dav-server`](https://crates.io/crates/dav-server) | 0.11 | 实现 `DavFileSystem`/`DavFile` trait；支持 axum 集成、可插拔 lock system |
| NFS | [`nfsserve`](https://crates.io/crates/nfsserve) | 0.11 | NFSv3 + mount 协议 over TCP；实现 `NFSFileSystem` trait（inode 语义，正合 VFS） |

运输层统一 tokio + axum（HTTP 类协议），与现有 `console` 模块的服务器模式
（`src/console/server.rs`、bearer token 认证）保持一致。

选型时考虑过但否决的方案：

- **自实现 S3 HTTP 层**（axum 手写路由 + 手写 SigV4）：工作量大且兼容性风险高，s3s 已成熟；
- **嵌入 MinIO**（JuiceFS 路线）：Go 生态产物，Rust 无对应物；
- **内核 nfsd / Samba 重导出 FUSE 挂载**（JuiceFS 官方 NFS/SMB 建议）：可行但要求先 FUSE mount，
  违背"不经内核"目标；作为过渡方案写入 NFS spec 的备选章节。

## 5. 与 FUSE 挂载的关系

网关与 FUSE 挂载是同一 volume 的并列前端：

- 数据互通：S3 PUT 的对象在 FUSE 下是同名文件（`bucket/key` → `/bucket/key`），反之亦然；
  WebDAV/NFS 同理（路径即路径）；
- 元数据互通：协议特有元数据（etag、tags、deadprops）在 FUSE 下以 xattr 可见；
- 锁不跨协议互通（S3 无锁语义；WebDAV lock 与 POSIX lock 暂不做双向映射，见 WebDAV spec）。

## 6. 其他协议评估

| 协议 | 结论 | 理由 |
|---|---|---|
| **SMB** | 暂不实现，给过渡方案 | 无成熟 Rust SMB server crate；过渡方案：FUSE mount + Samba 重导出（同 JuiceFS 官方 NFS 建议的形态）。长期若生态出现可用 crate（或自研）再立项 |
| **SFTP/FTP** | 不立项 | 价值低于上述三者；SFTP 可经由 SSH + FUSE mount 天然获得 |
| **HDFS** | 长期方向 | JuiceFS 以 Java SDK（JNI over libjfs）实现；BrewFS 需要先有稳定的 C ABI（cbindgen over SDK Client），工程量大，列入远期 |
| **CSI / K8s** | 独立线 | 已有 operator 雏形（`operator/`），走控制面路线，不在本协议网关范围 |
| **NFSv4** | 随 NFS spec 评估 | nfsserve 仅实现 v3；v4 的有状态语义（open/lock/delegation）与 VFS 句柄模型契合度高，但 crate 生态缺失，列入远期 |
| **pNFS / 9P** | 不立项 | 生态与需求均不足 |

## 7. 测试与验收总原则

每个协议里程碑必须包含：

1. **单元测试**：操作映射层（tmp/rename、xattr 读写、路径过滤 `.brewfs.sys`）；
2. **集成测试**（`tests/`，进程内构造 VFS + 内存/本地后端，起真实监听端口）；
3. **端到端验证**（docker compose 或本地脚本，真实客户端工具）：
   - S3：`aws s3 cp/ls/rm/mb` + `mc` 全流程；
   - WebDAV：`litmus` 测试套件 + `davfs2` 挂载 + Windows 网络驱动器映射冒烟；
   - NFS：`mount -t nfs -o vers=3` + 常规文件操作 + xfstests 子集；
4. **一致性检查**：协议写入 → FUSE 挂载读取（反向亦然）的交叉验证脚本。

## 8. 里程碑总览

详见 [ROADMAP.md](ROADMAP.md)。首个实现目标为 S3 网关（生态价值最高，
与 BrewFS 现有的 S3 后端故事闭环："数据可存于 S3，也可通过 S3 协议被访问"）。
