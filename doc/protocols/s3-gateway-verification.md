# S3 Gateway 本地验证说明

本文档描述如何在本地（WSL）验证 BrewFS S3 网关（`feat/s3-gateway-mvp` 分支）。
对应的 spec 见 [doc/protocols/s3-gateway.md](s3-gateway.md)。

## 1. 前置条件

- Linux（WSL Ubuntu-22.04 已验证）+ Rust 工具链
- Python 3 + `boto3`（端到端脚本使用；`pip3 install --user boto3`）
- WSL 中代理变量若不可用，先 `unset http_proxy https_proxy all_proxy`

## 2. 构建

```bash
cargo build --features gateway-s3
```

`gateway-s3` 在 `default` features 中，直接 `cargo build` 亦可。

## 3. 启动网关（localfs + sqlite）

```bash
mkdir -p /tmp/brewfs-e2e/data
cargo run --features gateway-s3 -- gateway s3 \
  --listen 127.0.0.1:19101 \
  --access-key testkey --secret-key testsecret \
  --data-backend local-fs --data-dir /tmp/brewfs-e2e/data \
  --meta-backend sqlx \
  --meta-url "sqlite:///tmp/brewfs-e2e/meta.db?mode=rwc"
```

关键参数：

| 参数 | 说明 |
|---|---|
| `--listen` | S3 端点监听地址（默认 `0.0.0.0:9000`） |
| `--access-key/--secret-key` | SigV4 静态密钥（也可用环境变量 `BREWFS_S3_ACCESS_KEY/SECRET_KEY`，缺省则启动失败） |
| `--bucket` | 单桶模式暴露的桶名（默认 `brewfs`） |
| `--multi-buckets` | 多桶模式：顶层目录即桶 |
| `--hide-dir-objects` | 列表时隐藏显式目录对象 |
| 其余 | 与 `brewfs mount` 相同的后端参数（`--data-backend`、`--meta-url` 等） |

启动成功的标志：日志出现 `brewfs s3 gateway listening`，端口开始监听。

## 4. 运行端到端测试

```bash
python3 scripts/e2e_s3_gateway.py
```

默认连 `http://127.0.0.1:19101`（可用 `GW_ENDPOINT` 环境变量覆盖）。

**2026-09-09 在 WSL Ubuntu-22.04 实测：`RESULT: 76 passed, 0 failed`**（single-bucket fresh LocalFS + SQLite 卷）。另以 multi-bucket fresh 卷完成 namespace/bucket 边界检查，验证 active multipart 会阻止删桶、Abort 后空桶可删除且不会复现，并验证非法单桶配置在监听前失败。

覆盖内容：

- 桶操作：list_buckets / head_bucket / location，错误桶返回 NoSuchBucket
- 对象基本操作：put（ETag=MD5）、get、range 读、head、content-type、user metadata、NoSuchKey，以及慢消费 GET 与并发覆盖时的完整性
- 路径边界：拒绝单桶模式的 `/.brewfs.sys` 对象入口、前导/重复尾斜杠别名和非法单桶配置
- list_objects_v2：prefix、delimiter/common-prefixes、MaxKeys 可见条目计数、零容量页与 ContinuationToken 分页
- copy_object：普通复制与同源同目标覆盖
- 分段上传：create/upload_part/list_parts/complete、abort、bucket/key ownership、零容量/多页 part listing、覆盖现有对象和目录目标拒绝
- 目录对象：PUT/GET/HEAD/list、空体约束、`key`/`key/` 隔离与空对象 CopyObject
- 删除：单删后 NoSuchKey、空目录自动收敛、批量 delete_objects

## 5. 单元测试

```bash
cargo test --features gateway-s3 gateway::
```

22 个用例覆盖 path 映射（桶名、保留命名空间、别名和逃逸 key 校验）、list 可见条目分页、零容量页与分隔符语义、multipart 路径与合并 ETag 格式，以及固定 key/bucket-lock 分片。

## 6. 手工验证要点（可选）

- **跨端可见**：网关写入的对象同时也是卷内普通文件，可再用 `brewfs mount`（FUSE）或 SDK Client 挂同一 meta/数据后端查看 `docs/readme.txt` 等路径。
- **协议元数据**：`brewfs.s3.etag` / `brewfs.s3.meta` 存放在文件 xattr；multipart 状态位于 `/.brewfs.sys/s3/uploads/`，该卷级内部命名空间不会作为对象或 multi-bucket 暴露。
- **坏凭据**：改错 access-key 后请求应返回 403（s3s SimpleAuth 拒绝）。
- **mtime 语义**：VFS attr 的 mtime 为纳秒，网关负责转换为 S3 的 `LastModified`（此前实测曾因按秒解析导致 time crate panic，已修复——回归时留意）。

## 7. 已知限制（MVP 范围外）

- 不支持：ACL、版本、加密、CORS、生命周期、PUT bucket 策略、GET/PUT bucket location 以外的桶级配置接口
- CopyObject 仅支持 `bucket/key` 形式的 `x-amz-copy-source`（不支持 ARN）
- list 接口未实现 `encoding-type`、`fetch-owner`、V1 `NextMarker` 的完整语义（仅 truncation 时返回）
- 多网关实例并发对同一 key 的写锁是进程内固定分片，跨实例不互斥
