# NFS Gateway Spec

状态：已立项
依赖：`nfsserve = "0.11"`（feature `gateway-nfs`）
CLI：`brewfs gateway nfs --listen <addr:2049> [--squash root|all|none] [后端参数...]`

## 1. 目标与非目标

### 目标

- 以 **NFSv3**（RFC 1813，含 mount 协议 MOUNTv3）直接暴露 BrewFS volume，
  客户端 `mount -t nfs -o vers=3,tcp` 即可使用，**不经过内核 FUSE**；
- 文件句柄 = BrewFS inode 号（稳定、重启可恢复），支持 `no_root_squash` 之外的标准 squash；
- 语义忠实：写路径 COMMIT 保证、readdirplus、符号链接/硬链接/mknod 全映射；
- 性能不低于"FUSE mount + kernel nfsd 重导出"方案（理论上更优：少一层内核往返）。

### 非目标

- NFSv4 / v4.1 / pNFS（有状态语义需要另一套设计，见 §9 评估）；
- UDP 传输（nfsserve 为 TCP；现代客户端默认 TCP，可接受）；
- Kerberos / RPCSEC_GSS（AUTH_SYS only）；
- NLM（网络锁管理器）——v3 配套的锁协议，nfsserve 未实现，见 §6。

## 2. 背景：为什么自实现而不是重导出

JuiceFS 官方方案是"FUSE mount + kernel nfsd 重导出"（`/etc/exports` 配 `fsid=N`）。
该方案对 BrewFS 同样有效，作为**过渡方案**写入部署文档；但它有三个根本限制：

1. 依赖 FUSE 挂载权限与内核态数据路径，容器/非特权环境不友好；
2. 两层缓存（NFS 客户端页缓存 ↔ kernel nfsd ↔ FUSE ↔ BrewFS 缓存）一致性窗口叠加；
3. 无法在网关层做协议感知的优化（如 COMMIT 直接映射 fsync 策略、readdirplus 批量取属性）。

自实现基于 `nfsserve` crate：实现 `NFSFileSystem` trait 即获得完整 NFSv3+mount 服务。

## 3. 文件句柄与 VFS 访问层

```
NFS client (kernel)
      │ NFSv3 over TCP (port 2049)
      ▼
┌────────────┐  NFSFileSystem trait   ┌──────────────┐  inode API   ┌─────┐
│ nfsserve    │ ─────────────────────▶ │ BrewFsNfs     │ ───────────▶ │ VFS │
│ (tcp server)│   fileid = inode       │ (src/gateway/nfs)│            └─────┘
└────────────┘                        └──────────────┘
```

关键决策：

- **fileid 即 inode**：`fileid: u64 = ino`。BrewFS inode 由 meta 层分配，稳定且唯一，
  满足 NFS 句柄"重启后仍可解析"的要求（stale handle → `NFS3ERR_STALE`）；
- **VFS inode API 公开化**：`VFS` 的 inode 级方法（`stat_ino`、`readdir_ino`、`child_of`、
  `mkdir_at_new`、`create_file_at_with_attrs`、`rename_at_with_known_attrs`、`parent_of`、`path_of` 等，
  `src/vfs/fs/mod.rs:1250-4465`）当前为 `pub(crate)`。实现时新增公开 wrapper：

  ```rust
  // src/vfs/handles.rs（新）
  pub struct InodeAccess<S, M> { vfs: VFS<S, M> }
  ```

  只暴露 NFS 需要的子集，保持 VFS 内部演进自由。不直接改 `pub`（避免把内部 API 表面固化）；
- **句柄与路径的互转**：NFS 侧绝大多数操作直接给 inode；少数需要路径的场景
  （诊断日志、stale 恢复提示）用 `path_of(ino)`。

## 4. 操作映射表（NFSPROC3）

| NFS 过程 | BrewFS（VFS inode API）映射 | 说明 |
|---|---|---|
| `NULL` / `MOUNT` / `UMNT` | 直接应答 | mount 协议返回根句柄 + auth flavors `[AUTH_SYS]` |
| `GETATTR` | `stat_ino(ino)` | 属性缓存见 §5 |
| `SETATTR` | `set_attr`（mode/uid/gid/size/atime/mtime） | size 变化走 `truncate` 路径；guard（ctime 检查）支持 |
| `LOOKUP` | `child_of(dir_ino, name)` + `stat_ino` | |
| `ACCESS` | 按 squash 后的 uid/gid 对 `stat_ino` 的 mode 位做权限判定 | 不真正以该身份执行（MVP），语义等价 `access(2)` |
| `READ` | `open(ino, read)` → `read(fh, off, len)` → `close` | 无状态：每次独立 open/close（BrewFS open 开销小）；后续可加 fh 池 |
| `WRITE` | `open(ino, write)` → `write(fh, off, data)` → `close` | stable_how 语义见 §5 |
| `CREATE` | `create_file_at_with_attrs(dir, name, attrs)` | EXCLUSIVE 模式：以 verifier 存 xattr 实现幂等（`brewfs.nfs.createverf`） |
| `MKDIR` / `RMDIR` | `mkdir_at_new` / `rmdir_at` | 非空 → `NFS3ERR_NOTEMPTY` |
| `REMOVE` | `unlink`（按 dir+name） | |
| `RENAME` | `rename_at_with_known_attrs` | 跨目录 rename 由 meta 事务保证原子 |
| `LINK` / `SYMLINK` / `READLINK` | `link_by_ino` / `create_symlink_at` / `readlink_ino` | |
| `MKNOD` | 按类型分发（fifo/socket 由 meta 特殊文件支持） | 无权限创建 device 时返回 `NFS3ERR_PERM` |
| `READDIR` / `READDIRPLUS` | `readdir_ino(ino)`（内部已分页） | readdirplus 批量带 attr，显著减少客户端 GETATTR 往返；cookie/verifier 用 (ino, 条目序号) 编码，快照内单调 |
| `FSSTAT` | `stat_fs()` | |
| `FSINFO` | 静态通告：`rtmax/wtmax = 1 MiB`、`rtpref/wtpref = 256 KiB`、time_delta=1ms、`properties = FSF3_LINK|SYMLINK|HOMOGENEOUS|CANSETTIME` | |
| `PATHCONF` | 静态：`linkmax/chown_restricted=true/no_trunc=true/name_max=255` | |
| `COMMIT` | 对该 inode `open` + `fsync`（或直接触发写回队列 flush 该文件） | §5 |

错误码映射：`NotFound→NOENT`、`PermissionDenied→ACCES`、`AlreadyExists→EXIST`、
`DirectoryNotEmpty→NOTEMPTY`、非目录→`NOTDIR`、是目录→`ISDIR`、
句柄对应 inode 已不存在→`STALE`、空间不足→`NOSPC`、其他→`IO`。

## 5. 写路径与 COMMIT 语义（重点）

NFSv3 无 open/close，客户端写经过其页缓存，依赖 `WRITE` 的 `stable_how` 与 `COMMIT` 保证持久性：

| 客户端请求 | BrewFS 行为 |
|---|---|
| `WRITE stable=UNSTABLE` | 写入即返回（数据进 BrewFS 写路径/写回缓存），返回 `committed=UNSTABLE` + write verifier |
| `WRITE stable=FILE_SYNC` | 写入并等该文件数据落盘（`flush` 语义），返回 `FILE_SYNC` |
| `WRITE stable=DATA_SYNC` | 同 FILE_SYNC（BrewFS 元数据与数据同事务，无单独 data-only 通道） |
| `COMMIT` | `fsync` 目标 inode：确保该文件所有已写数据到达持久层（或写回缓存已排队且副本安全——
  按 mount 的 writeback 配置如实选择：writeback 模式下 COMMIT 语义以
  "写入 SSD 写回缓存 + meta 已记录"为准，与 FUSE 挂载的 fsync 策略保持一致） |

- **write verifier**：网关启动时生成的随机 u64，重启变更，客户端据此重发未提交写；
- 性能要求：`UNSTABLE` 写绝不触发 fsync；`COMMIT` 粒度为单文件，不全局刷盘；
- 大顺序写吞吐目标：≥ 同机 FUSE 直挂的 70%（少了内核 nfsd 一层，预期更高，留有余量）。

## 6. 锁（NLM 缺失的应对）

- NFSv3 的文件锁由带外协议 NLM 提供，nfsserve 未实现；
- **MVP 行为**：客户端 `lockf/fcntl` 将失败或退化为本地锁（取决于客户端挂载选项
  `nolock`/`local_lock`）。文档强制建议挂载参数：
  `mount -t nfs -o vers=3,tcp,nolock`（单写者/读多场景）；
- 后续：评估自实现 NLMv4（ONC-RPC 直写，工作量中等）或向 nfsserve 上游贡献；
  BrewFS 侧已有 `get_plock/set_plock`（`src/vfs/fs/mod.rs:4471/4482`）可承接。

## 7. 认证、squash 与身份映射

- AUTH_SYS（uid/gid 明文随包）；
- `--squash`：
  - `root`（默认）：客户端 uid0 → 匿名 uid/gid（默认 65534，可配 `--anon-uid/--anon-gid`）；
  - `all`：所有用户 → 匿名；
  - `none`：信任客户端身份（仅可信网络）；
- 身份使用：squash 后的 (uid,gid,groups) 构造 `CallerIdentity`（`src/fs.rs:279`，
  随本里程碑公开化）参与权限判定；文件创建继承该身份；
- 导出访问控制：MVP 单导出（整个 volume）+ `--allow <cidr,...>` IP 白名单；
  多导出/子目录导出列后续。

## 8. 与 FUSE/其他网关的一致性

- NFS 客户端属性缓存（默认 3-60s）是主要弱一致窗口：FUSE 侧改文件后，NFS 客户端
  最坏 60s 才看到；文档建议对强一致需求客户端 `mount -o actimeo=0,noac`；
- 网关内不额外缓存 attr（每次 GETATTR 走 MetaClient，其缓存失效机制与 FUSE 挂载一致）；
- 重命名/删除正在被 NFS 客户端打开的文件：POSIX 语义由 meta 层 inode 引用计数保证
  （与 FUSE 的 silly-rename 需求不同，NFS 客户端自己会处理 `.nfsXXXX` 模式，无需网关配合）。

## 9. NFSv4 评估（远期）

- v4 核心差异：有状态（open/lock stateid、delegation）、复合操作、单一端口、UTF-8 域名；
- 与 BrewFS 契合点：VFS 句柄模型可承接 open state；delegation 可对接 MetaClient 缓存失效；
- 障碍：Rust 生态无 v4 server crate（nfsserve 仅 v3），需自研或上游共建——工作量远超 v3；
- 结论：v3 先行覆盖"Unix 集群共享"场景；v4 待 v3 稳定后按需求立项。

## 10. 过渡方案：kernel nfsd 重导出（文档项）

实现落地前/不满足自实现条件时，部署文档提供：

```bash
brewfs mount /mnt/brewfs ...
# /etc/exports: /mnt/brewfs *(rw,async,fsid=1,no_subtree_check)
exportfs -ra
```

注明：`fsid` 必须显式（FUSE 文件系统无设备号）；`async` + BrewFS writeback 组合的性能提示。
（JuiceFS 官方即此方案，`juicefs/docs/en/deployment/nfs.md`。）

## 11. 测试计划

1. **单元测试**：verifier/cookie 编码、错误码映射、squash 判定、EXCLUSIVE CREATE 幂等；
2. **集成测试**（`tests/nfs_gateway_test.rs`）：进程内起服务 + `mount -t nfs -o vers=3,tcp,port=<p>,mountport=<p>`
   （测试容器内需 nfs-common），跑 create/read/write/rename/link/readdir 断言；
3. **e2e**：WSL/compose 内真实挂载，跑：
   - 常规：`cp/rsync/tar` 大文件与小文件集；
   - `xfstests` 通用用例子集（NFS 适配组）；
   - 并发：多客户端同目录创建/重命名；
   - 重启网关 → 客户端 stale handle 与重挂载恢复；
4. **交叉一致性**：NFS 写入 → FUSE/S3 读取验证。

## 12. CLI 参数

```
brewfs gateway nfs \
  --listen 0.0.0.0:2049 \
  [--squash root|all|none] [--anon-uid 65534 --anon-gid 65534] \
  [--allow 10.0.0.0/8,192.168.0.0/16] \
  [--max-read 1MiB --max-write 1MiB] \
  <与 mount 相同的后端参数>
```

## 13. 里程碑

- **M4（首个实现）**：§4 全表 + AUTH_SYS/squash + COMMIT 语义 + mount 挂载冒烟 + e2e 脚本。
- **后续**：NLM、fh 池/属性缓存优化、多导出、`xfstests` NFS 组全量、NFSv4 立项评估。

## 14. 参考

- nfsserve crate：<https://crates.io/crates/nfsserve>（NFSv3+mount，TCP）；
- RFC 1813（NFSv3）、RFC 1094（mount）、RFC 5716（squash/安全相关背景）；
- JuiceFS NFS 部署文档（重导出方案）：`juicefs/docs/en/deployment/nfs.md`；
- BrewFS VFS inode API：`src/vfs/fs/mod.rs:1250-4465`；CallerIdentity：`src/fs.rs:279`。
