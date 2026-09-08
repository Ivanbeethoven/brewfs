# S3 Gateway Spec

状态：已立项（首个实现目标）
依赖：`s3s = "0.15"`（feature `gateway-s3`）
CLI：`brewfs gateway s3 --listen <addr> --access-key <ak> --secret-key <sk> [后端参数...]`

## 1. 目标与非目标

### 目标

- 以 S3 兼容协议暴露一个 BrewFS volume，支持 `aws cli`、`mc`、s3fs、各类 SDK（boto3/aws-sdk）直接访问；
- 覆盖单 bucket 与多 bucket 两种模式（见 §3）；
- 覆盖核心对象操作与 multipart 上传（§4 映射表中标 ★ 的操作）；
- 与 FUSE 挂载数据互通：S3 写入的对象可在 FUSE 下按路径读取，反之亦然；
- 流式数据路径：GET/PUT 不整对象入内存，range 请求走 `read_at`。

### 非目标（显式排除，调用返回 NotImplemented）

- Bucket/Object versioning（`ListObjectVersions` 浅实现或直接 501）；
- Object Lock / Retention / Legal Hold；
- SSE（服务端加密）与对象级压缩；
- Bucket policy / IAM / STS（MVP 只有静态密钥对，见 §6）；
- Event notification、Lifecycle、Replication、Inventory；
- Presigned URL 由 s3s 的 SigV4 校验天然支持（客户端侧功能），不额外开发。

## 2. 架构

```
aws cli / mc / SDK
      │ HTTP + SigV4
      ▼
┌─────────────┐   S3 trait (s3s)   ┌──────────────┐   ClientBackend (path ops)
│ s3s::service │ ─────────────────▶ │ BrewFsS3      │ ────────────────────▶ Client ─▶ VFS ─▶ meta/data
│ (axum/tokio) │                   │ (src/gateway/s3)│
└─────────────┘                   └──────────────┘
                                         │ xattr: brewfs.s3.{etag,meta,tags,dirobj}
                                         │ 内部状态: /.brewfs.sys/s3/{uploads,tmp}
```

- HTTP/SigV4/路由/序列化全部交给 s3s；我们只实现 `s3s::S3` trait 的方法；
- 存储映射层（`BrewFsS3`）基于 SDK `Client`（path-based），不依赖 FUSE；
- 大对象分段流式转发：s3s 的请求 body 是 `Stream<Byte>`，写侧以固定窗口（默认 4 MiB，
  对齐 chunk block 大小）`write_at` 到 tmp 文件；读侧用 s3s 的 `StreamingBlob` 包装
  `read_at` 迭代器。

## 3. Bucket 模型

| 模式 | 开启方式 | 映射 | 说明 |
|---|---|---|---|
| 单 bucket（默认） | `--bucket <name>`（缺省为 volume 名） | 整个 volume = 一个 bucket；`/` 是 bucket 根 | 与 JuiceFS gateway 默认行为一致 |
| 多 bucket | `--multi-buckets` | 顶层目录即 bucket：`/<bucket>/<key>` | `CreateBucket` = mkdir 顶层目录；`ListBuckets` = readdir `/` 并过滤合法 bucket 名与 `.brewfs.sys` |

约束：

- bucket 名合法性按 S3 规则（3-63 字符、小写、无连续点等）校验，不合法目录在多 bucket 模式下不出现在 ListBuckets；
- 两种模式下 object key → 路径的映射都是直接的 `/<key>`（单 bucket）或 `/<bucket>/<key>`（多 bucket），**不做 key 编码**；
  key 中的 `..`、以 `/` 开头等畸形输入在入口校验拒绝（`InvalidArgument`），防止逃逸 bucket 根；
- `HeadBucket` = stat bucket 根目录；`DeleteBucket` = rmdir 空目录（非空返回 `BucketNotEmpty`）。

## 4. 操作映射表

★ = MVP 必须；○ = 后续里程碑；✗ = 显式不支持（501）。

| S3 操作 | 状态 | BrewFS 映射 |
|---|---|---|
| PutObject | ★ | 流式写 `/.brewfs.sys/s3/tmp/<uuid>` → flush → `rename` 到目标路径（父目录按需 `mkdir_p`）；随后写入 etag/meta/tags xattr。覆盖写 = 同流程（rename 原子替换） |
| GetObject | ★ | `stat` + `read_at` 流式返回；支持 `Range`（含 suffix range）、`If-Modified-Since` 等条件头 |
| HeadObject | ★ | `stat` + 读 xattr 还原 ETag/Content-Type/user meta；目录对象判定见 §5 |
| DeleteObject | ★ | `unlink`；随后自底向上回收变空的父目录，直到 bucket 根或遇到目录对象（§5）——模拟 S3 扁平命名空间 |
| DeleteObjects（批量） | ★ | 循环单删，逐项汇报错误（S3 语义允许部分失败） |
| ListObjectsV1 / V2 | ★ | 基于 `readdir` 的前缀树遍历，实现 delimiter/prefix/marker(start-after)/max-keys；根目录过滤 `.brewfs.sys`；目录对象按 §5 规则出现或隐藏 |
| CreateBucket / DeleteBucket / HeadBucket / ListBuckets | ★ | 见 §3 |
| CopyObject | ★ | 服务端内完成：源 `read_at` → tmp 文件 → rename（不经客户端）；xattr 按 `MetadataDirective` 复制或替换。大数据量后续可优化为 chunk 级克隆（需要 chunk 层支持，记为后续项） |
| CreateMultipartUpload | ★ | 建目录 `/.brewfs.sys/s3/uploads/<hh>/<upload-id>/`，`.target` 占位文件 xattr 记录目标 bucket/key、content-type、user meta；`<hh>` = upload-id 前 2 位十六进制扇出 |
| UploadPart | ★ | 流式写 `uploads/<hh>/<id>/part-<n>`（覆盖同 n 即重写），part etag 记 xattr `brewfs.s3.etag` |
| ListParts | ★ | readdir upload 目录 + 逐 part stat/xattr |
| ListMultipartUploads | ★ | 扫描 `uploads/` 两级目录，读 `.target` xattr 还原 key，支持 prefix/delimiter 过滤 |
| CompleteMultipartUpload | ★ | 校验 part 序号连续性与 etag；按序 `read_at` 各 part 追加写 tmp 文件（流式，不整读）→ 计算 S3 multipart etag（md5(part-md5...) + "-" + N）→ rename 落位 → 写 xattr → `remove_dir_all` upload 目录 |
| AbortMultipartUpload | ★ | `remove_dir_all` upload 目录 |
| PutObjectTagging / GetObjectTagging / DeleteObjectTagging | ○ | xattr `brewfs.s3.tags` |
| GetObjectAttributes | ○ | 组合 stat + xattr |
| SelectObjectContent | ✗ | 501 |
| Versioning 系列 | ✗ | 501 |
| Object Lock / Retention | ✗ | 501 |
| SSE-KMS/SSE-C | ✗ | 501（SSE 头出现时可选择拒绝或忽略，MVP 选择拒绝并返回 `NotImplemented`，避免"假装加密"） |
| Bucket policy / ACL / CORS / Lifecycle / Notification / Replication | ✗/○ | MVP 返回静态默认值或 501；CORS 后续按部署需要加 |

### 条件写（If-None-Match / If-Match）

MVP 支持 `If-None-Match: *`（create_new 语义 → `AlreadyExists` 映射 `PreconditionFailed`），
其余条件头在 MVP 记录并忽略，后续里程碑补齐。

## 5. 目录语义（S3 扁平空间 ↔ POSIX 层级）

这是 S3 网关最难做对的部分，规则如下（与 JuiceFS gateway 行为对齐，但用 xattr 代替 atime 技巧）：

1. **隐式目录**：`PUT a/b/c.txt` 自动 `mkdir_p a/b`；list `a/` 时 `a/b/` 作为 CommonPrefix 出现。
   这些目录没有 xattr 标记，DeleteObject 后被回收（逐级向上，遇非空停）。
2. **显式目录对象**：`PUT a/b/`（key 以 `/` 结尾，零长度 body）创建真实目录并打
   `brewfs.s3.dirobj` xattr；它在 list 中作为一条零字节对象出现（key 带尾 `/`），
   DeleteObject 父级回收遇到它停止；`--hide-dir-objects` 选项可在 list 中隐藏。
3. **文件与目录同名冲突**：POSIX 下不可能共存；若 FUSE 侧手工构造出与对象同前缀的结构，
   list 结果以目录优先（与 JuiceFS 一致），文档声明避免混用。
4. `HEAD key/`（带尾斜杠）命中目录对象返回 200；命中隐式目录返回 404（S3 真实行为）。

## 6. 认证与安全

- MVP：单一静态密钥对。`--access-key/--secret-key` 或环境变量 `BREWFS_S3_ACCESS_KEY/BREWFS_S3_SECRET_KEY`；
  SigV4 校验由 s3s 的 auth 组件完成（支持 header 签名与 presigned URL）；
- 未配置密钥时拒绝启动（不允许匿名读写）；`--anonymous-read` 显式开启匿名只读（公共桶场景）；
- TLS：MVP 为 HTTP；生产部署建议前置反向代理（nginx/caddy）终结 TLS。
  原生 TLS（rustls）列后续里程碑；
- 后续：多密钥对（静态配置表）、与 console 认证打通。

## 7. 一致性与并发

- **写-写冲突**：单实例内 dashmap 按 `(bucket,key)` 分片互斥，串行化同一 key 的
  Put/Complete/Delete/Rename 路径；不同 key 完全并发；
- **写-读冲突**：读旧对象期间发生覆盖写，rename 原子性保证读者拿到完整旧版或完整新版，
  不会读到半个对象（tmp+rename 模式的核心收益）；
- **multipart 并发**：同一 upload-id 的 part 上传并发安全（不同 part 不同文件）；
  同一 part number 重复上传后者覆盖前者（S3 语义）；
- **多网关实例**：MVP 不保证跨实例的 key 级串行化（依赖 rename/create 原子性兜底，
  语义为 last-writer-wins）；跨实例分布式锁（`/.brewfs.sys/locks/` + meta flock）列入 M3；
- **list 一致性**：以 meta 层 readdir 时点快照为准，分页遍历期间的并发增删不保证边界精确
  （与 S3 实际一致：list 是弱一致）。

## 8. 错误码映射

| BrewFS 错误 | S3 响应 |
|---|---|
| `NotFound`（对象） | 404 `NoSuchKey` |
| `NotFound`（bucket 根） | 404 `NoSuchBucket` |
| `AlreadyExists`（bucket） | 409 `BucketAlreadyExists` / `BucketAlreadyOwnedByYou` |
| `DirectoryNotEmpty` | 409 `BucketNotEmpty` |
| `PermissionDenied` | 403 `AccessDenied` |
| `InvalidInput`（key 畸形/part 序号错） | 400 `InvalidArgument` / `InvalidPartOrder` |
| 空间不足 / 配额 | 507 `InsufficientStorage`（自定义 message） |
| 其他内部错误 | 500 `InternalError` + request id |

etag 不匹配的 part 校验 → 400 `InvalidPart`；upload-id 不存在 → 404 `NoSuchUpload`。

## 9. 性能目标与要求

- PUT/GET 大对象（≥1 GiB）稳态吞吐不低于同机 FUSE 挂载直写的 80%（无内核路径，理论上应更高）；
- 全程流式：单连接内存占用有界（≤ 2 × 4 MiB 缓冲 + 协议栈开销），与对象大小无关；
- 并发：默认 tokio worker 全核；单 key 串行不成为全局瓶颈；
- list 每页 max-keys=1000 的 p99 延迟 < 200ms（sqlite meta，10 万对象规模内）。

## 10. CLI 参数

```
brewfs gateway s3 \
  --listen 0.0.0.0:9000            # 监听地址
  --access-key / --secret-key      # 静态密钥（或 env）
  [--bucket <name>]                # 单 bucket 名（默认 volume 名）
  [--multi-buckets]                # 多 bucket 模式
  [--hide-dir-objects]             # list 隐藏目录对象
  [--anonymous-read]               # 匿名只读
  [--domain <base>]                # virtual-host 风格访问（后续）
  <与 mount 相同的后端参数>
```

## 11. 测试计划

1. **单元测试**（`src/gateway/s3/` 内）：key→路径校验、目录对象判定、etag 计算、
   分页遍历的 marker 边界、`.brewfs.sys` 过滤；
2. **集成测试**（`tests/s3_gateway_test.rs`）：进程内 `VFS`（sqlite memory + 本地目录数据后端）
   + 起 s3s 服务于随机端口，用 `aws-sdk-s3`（已在依赖树）做客户端断言全映射表；
3. **端到端**（`scripts/` 或 compose）：真实二进制 + `aws s3 cp/ls/rm/mb/rb`、`mc cp/ls`，
   含 multipart（>8 MiB 文件强制分片）、FUSE 交叉读写验证；
4. **兼容性冒烟**：boto3、s3fs 挂载读、presigned URL 下载。

## 12. 里程碑拆分

- **M1（本 spec 首个实现）**：单 bucket；§4 全部 ★；静态密钥；流式 PUT/GET；
  集成测试 + aws cli e2e。
- **M3（完整化）**：多 bucket、tagging、GetObjectAttributes、条件头补全、
  跨实例锁、CORS、rustls、mc 全功能回归。

## 13. 参考

- JuiceFS gateway：`pkg/gateway/gateway.go`（tmp+rename、`/.sys`、xattr etag/meta、
  DeleteObject 父目录回收、multipart 目录模型、cleanup 周期任务）；
  文档 `juicefs/docs/en/guide/gateway.md`；
- s3s crate：<https://crates.io/crates/s3s>（RustFS 同源 S3 服务框架）；
- 本目录 [README.md](README.md) §3 共用约定。
