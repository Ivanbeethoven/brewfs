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

**预期结果：`RESULT: 27 passed, 0 failed`**（2026-09-08 在 WSL Ubuntu-22.04 实测）。

覆盖内容：

- 桶操作：list_buckets / head_bucket / 错误桶返回 404
- 对象基本操作：put（ETag=MD5）、get、range 读、head、content-type、NoSuchKey
- list_objects_v2：prefix、delimiter/common-prefixes、MaxKeys 截断与 ContinuationToken 分页
- copy_object
- 分段上传：create/upload_part/list_parts/complete（ETag 带 `-2` 后缀）、abort 后 NoSuchUpload
- 目录对象：PUT `key/`、GET 为空体、列表中以 `key/` 呈现且 size=0
- 删除：单删后 NoSuchKey、空目录自动收敛、批量 delete_objects

## 5. 单元测试

```bash
cargo test --features gateway-s3 gateway::
```

11 个用例覆盖 path 映射（桶名校验、逃逸 key 拒绝）、list 语义（排序/max_keys/start_after/分隔符）、multipart 路径与合并 ETag 格式。

## 6. 手工验证要点（可选）

- **跨端可见**：网关写入的对象同时也是卷内普通文件，可再用 `brewfs mount`（FUSE）或 SDK Client 挂同一 meta/数据后端查看 `docs/readme.txt` 等路径。
- **协议元数据**：`brewfs.s3.etag` / `brewfs.s3.meta` 存放在文件 xattr；multipart 状态位于 `/.brewfs.sys/s3/uploads/`，桶根下的 `.brewfs.sys` 在列表中被隐藏。
- **坏凭据**：改错 access-key 后请求应返回 403（s3s SimpleAuth 拒绝）。
- **mtime 语义**：VFS attr 的 mtime 为纳秒，网关负责转换为 S3 的 `LastModified`（此前实测曾因按秒解析导致 time crate panic，已修复——回归时留意）。

## 7. 已知限制（MVP 范围外）

- 不支持：ACL、版本、加密、CORS、生命周期、PUT bucket 策略、GET/PUT bucket location 以外的桶级配置接口
- CopyObject 仅支持 `bucket/key` 形式的 `x-amz-copy-source`（不支持 ARN）
- list 接口未实现 `encoding-type`、`fetch-owner`、V1 `NextMarker` 的完整语义（仅 truncation 时返回）
- 多网关实例并发对同一 key 的写锁是进程内的（DashMap），跨实例不互斥
