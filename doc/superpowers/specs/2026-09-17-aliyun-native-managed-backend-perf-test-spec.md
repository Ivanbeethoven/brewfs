# Aliyun ECS Native BrewFS/JuiceFS 性能测试 Spec（草案）

- 状态：待确认
- 日期：2026-09-17
- 适用对象：BrewFS 与 JuiceFS 的可重复性能对比
- 结果服务：`$env:BREWFS_RESULTS_URL`（自建 Result Vault 的地址）

## 1. 目标与非目标

### 目标

建立一条不依赖 Docker 的 Aliyun ECS 性能测试流程：

1. 本地 WSL 构建 Linux `brewfs` 二进制，并在上传前确认 commit 与目标分支一致。
2. 使用一份长期保留的 Ubuntu 24.04 ECS 自定义 VM 镜像。
3. 每次测试只把本地 `brewfs` 二进制上传到临时 ECS；测试程序、fio、xfstests、FUSE、AWS OSS 客户端依赖和 JuiceFS 客户端由镜像提供。
4. 对象数据使用 Aliyun OSS，元数据使用 Aliyun Redis/Tair；测试 ECS 不启动 RustFS、Redis Server、Docker 或 Windows VM。
5. BrewFS 与 JuiceFS 使用相同 ECS 规格、相同 OSS、相同网络位置、相同测试数据规模、相同 fio 参数和相同结果上传流程。
6. 测试完成后无条件删除临时 ECS 及其临时数据盘；性能结果归档到 Result Vault，二进制临时上传记录被删除。

### 非目标

- 不在测试 ECS 上编译 Rust。
- 不把 Docker 镜像作为测试入口。
- 不把 RustFS 或自建 Redis 作为 Aliyun OSS/Redis 的替代品。
- 不在没有结果上传地址、OSS 配置或 Redis 配置时创建 ECS。

## 2. 总体架构

```text
Windows 控制端
  ├─ WSL Ubuntu：构建 target/docker/brewfs
  ├─ Aliyun CLI：创建临时 ECS / RunCommand / 删除 ECS
  └─ Result Vault client：上传单个二进制与最终性能归档

长期保留的 ECS VM 镜像（Ubuntu 24.04）
  ├─ fio / xfstests / FUSE / stress-ng / profiling helpers
  ├─ aws CLI（用于 OSS 初始化和对象检查）
  ├─ 固定版本 JuiceFS 客户端
  ├─ native BrewFS/JuiceFS perf runner
  └─ 不包含 Redis Server、RustFS、Docker

临时 ECS（每轮测试创建，测试结束删除）
  ├─ /usr/local/bin/brewfs：本轮唯一上传的测试二进制
  ├─ 本地 NVMe/云盘：缓存、fio 工作集、测试日志
  ├─ Aliyun OSS：对象数据
  ├─ Aliyun Redis/Tair：BrewFS/JuiceFS 元数据
  └─ Result Vault：最终结果 zip
```

## 3. 长期保留的 VM 镜像

### 3.1 镜像内容

镜像 builder 在 Ubuntu 24.04 x86_64 上预装并验证：

- `fuse3`、`libfuse3-3`、`xfstests`、`fio`、`stress-ng`、`xfsprogs`、`acl`、`attr`、`bc`、`dbench`、`quota`、`strace` 等现有 runner 依赖；
- `aws` CLI，仅用于 OSS bucket/prefix 的初始化、清理和连通性检查；
- 固定版本的 JuiceFS Linux amd64 客户端；
- `run_native_perf.sh` 以及 BrewFS/JuiceFS 现有性能 runner；
- `/opt/xfstests-dev`、runner 源码和镜像 manifest；
- Cloud Assistant Agent 及基础诊断工具。

镜像不预置以下内容：

- OSS AccessKey、SecretKey、STS token、Redis 密码或 TLS 证书；
- Redis Server、RustFS、MinIO、Docker、Docker Compose；
- 本轮 BrewFS 二进制。

镜像只在维护时创建一次并长期保留。builder ECS、builder 数据盘和所有测试 ECS 都必须在流程结束时删除。

### 3.2 镜像验收

镜像创建后必须通过 Cloud Assistant 只读检查：

```text
/usr/bin/fio
/usr/local/bin/aws
/usr/local/bin/juicefs
/opt/xfstests-dev/check
/opt/brewfs-perf/native/run_native_perf.sh
/opt/brewfs-perf/image-source-commit
```

同时确认：

- `docker`, `docker compose`, `redis-server`, `rustfs` 不作为测试依赖；
- `systemctl list-units` 中没有测试流程自动启动的 Redis/RustFS 服务；
- 镜像 manifest 记录 Ubuntu 版本、runner commit、JuiceFS 版本和构建时间；
- 镜像内不存在任何云服务密钥。

## 4. Aliyun 托管后端

### 4.1 OSS

测试使用 Aliyun OSS S3 兼容接口：

- `OSS_ENDPOINT`：显式传入地域 endpoint，例如 `https://oss-cn-hangzhou.aliyuncs.com`；
- `OSS_REGION`：显式传入 `cn-hangzhou` 等地域；
- `OSS_BUCKET`：预先创建的专用 bucket；
- `OSS_PREFIX`：每轮测试唯一前缀，例如 `perf/20260917/<run-id>/`；
- `OSS_ACCESS_KEY_ID`、`OSS_SECRET_ACCESS_KEY`：通过 Cloud Assistant 环境注入，禁止写入命令行日志、镜像或结果文本。

每个 workload 使用独立的 prefix。BrewFS 与 JuiceFS 可以共用一个 bucket，但不得复用对象 prefix，以免残留对象污染下一轮结果。测试开始时 runner 只清理本轮 prefix；不得删除 bucket 中其他前缀。

优先使用 RAM/STS 的短期凭证。若当前账号只能使用长期 AccessKey，则凭证必须通过本地环境变量或安全参数传入，脚本不得把它们写入 git、镜像或 Result Vault。

### 4.2 Redis/Tair

测试使用 Aliyun Redis/Tair 实例作为元数据后端：

- `REDIS_URL`：通过 `redis://` 或 `rediss://` 显式传入；
- Redis/Tair 实例必须允许测试 ECS 所在 vSwitch/安全组访问；
- 若启用 TLS，镜像需要预置 CA，而不是预置密码；
- BrewFS 与 JuiceFS 不应并发写入同一个元数据 namespace。

默认隔离策略：

- BrewFS 使用独立 logical DB 或独立 Redis/Tair 实例；
- JuiceFS 使用另一 logical DB 或另一实例；
- runner 为每轮生成唯一 namespace/run-id，并在开始前做连通性和空 namespace 检查；
- 测试结束后只清理本轮 namespace，禁止执行全实例 FLUSHALL。

如果目标 Tair 配置不支持 logical DB，必须改用两个独立的 Redis/Tair 实例或服务端支持的 key prefix 方案，并在结果 manifest 中记录隔离方式。

## 5. 凭证与参数注入

控制端只保存非敏感测试配置和资源 ID。敏感值使用环境变量传给 runner，建议变量名如下：

```powershell
$env:BREWFS_RESULTS_URL = 'https://<your-result-vault>'
$env:BREWFS_PERF_IMAGE_ID = 'm-xxxxxxxx'
$env:BREWFS_PERF_VSWITCH_ID = 'vsw-xxxxxxxx'
$env:BREWFS_PERF_SECURITY_GROUP_ID = 'sg-xxxxxxxx'
$env:BREWFS_OSS_ENDPOINT = 'https://oss-cn-hangzhou.aliyuncs.com'
$env:BREWFS_OSS_REGION = 'cn-hangzhou'
$env:BREWFS_OSS_BUCKET = 'brewfs-perf'
$env:BREWFS_OSS_ACCESS_KEY_ID = '从安全存储注入'
$env:BREWFS_OSS_SECRET_ACCESS_KEY = '从安全存储注入'
$env:BREWFS_REDIS_URL = 'rediss://:password@r-xxxx.redis.rds.aliyuncs.com:6379/0'
```

实际脚本可以把上述值映射为 BrewFS 的 `BREWFS_S3_*`、`BREWFS_META_URL` 和 JuiceFS 的 `JFS_S3_*`、`JFS_META_URL`。结果 manifest 只保留 endpoint hostname、bucket、region、Redis DB/隔离方式和脱敏后的配置摘要，不保留密码、AccessKey 或 token。

## 6. 单轮测试流程

### 6.1 本地构建与一致性检查

```powershell
wsl.exe bash -lc 'cd /path/to/brewfs && \
  git fetch origin main && \
  test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" && \
  CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --workspace --bin brewfs --release'
```

控制端在上传前记录：

- `git rev-parse HEAD`；
- 目标分支和 remote commit；
- `sha256`；
- 二进制文件大小；
- Rust/Cargo profile；
- VM image ID。

如果本地 commit 与目标分支不一致，流程在创建 ECS 之前失败。

### 6.2 创建与准备 ECS

1. 使用固定 image ID、instance type、zone、vSwitch、security group 创建一台按量付费 ECS。
2. 设置自动释放时间，最长不超过本轮允许的测试窗口。
3. 通过 Result Vault 创建临时 binary run，上传并记录 SHA-256。
4. Cloud Assistant 下载二进制到 `/usr/local/bin/brewfs`，校验 hash 和执行权限。
5. 通过 native runner 检查 OSS 和 Redis/Tair 连接；连接失败不得开始 benchmark。
6. 初始化本轮 OSS prefix 和独立 metadata namespace。

### 6.3 BrewFS 与 JuiceFS 运行

为了避免系统状态和缓存影响，默认一轮只运行一个 workload：

1. 创建临时 ECS；
2. 运行 BrewFS 全部场景；
3. 导出并上传 artifact；
4. 卸载文件系统、清理本轮后端 namespace/prefix；
5. 删除 ECS 和其临时数据盘；
6. 用同一 image ID、instance type 和参数创建另一台临时 ECS；
7. 运行 JuiceFS 全部场景；
8. 导出并上传 artifact；
9. 再次清理并删除资源。

若为节省时间需要同一台 ECS 顺序运行两个 workload，必须卸载并清空本地缓存、使用不同 OSS prefix 和 metadata namespace，并在结果中明确记录“同机顺序运行”，不能与独立 ECS 结果混为同一 baseline。

默认工具集合：

```text
fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw
fio-bigread fio-bigwrite metaperf dirstress dirperf looptest
```

默认参数以仓库现有 compose runner 和 `AGENTS.md` 为准。任何云端缩短参数（例如 `PERF_FIO_SIZE` 或 `PERF_FIO_RUNTIME`）必须写入 artifact manifest，不能只在控制台命令里出现。

### 6.4 结果上传

最终结果归档至少包含：

- `perf-summary.tsv`、工具原始输出和 fio JSON/日志；
- BrewFS/JuiceFS 启动参数的脱敏快照；
- workload、git commit、binary SHA-256、image ID、ECS instance type、zone；
- OSS endpoint/region/bucket/prefix；
- Redis/Tair hostname、端口、DB/namespace 隔离方式；
- FUSE、内核、fio、JuiceFS 和 runner 版本；
- 清理日志和后端连通性诊断；
- `*_effective_wall_bw_mib_s` 与 `*_effective_active_plus_drain_bw_mib_s` 等写回成本指标。

归档通过 `POST <ResultVaultUrl>/api/runs` 上传。上传成功后保留最终 run；只用于下发 BrewFS 二进制的临时 run 在测试 `finally` 中删除。结果上传失败必须使任务失败并留下本地归档路径，但仍要继续删除 ECS 和数据盘。

## 7. 资源清理不变量

无论测试成功、失败、超时、Cloud Assistant 重启或上传失败，都必须执行：

- 停止并卸载 BrewFS/JuiceFS；
- 清理本轮本地挂载点、缓存和临时日志；
- 删除本轮 OSS prefix；
- 删除本轮 Redis/Tair metadata namespace；
- 删除临时 binary upload run；
- 停止并删除临时 ECS；
- 确认 `DescribeInstances` 不再返回本轮 instance ID；
- 确认没有挂载到该 ECS 的临时数据盘残留。

长期保留的内容仅包括：

- 已确认的 VM 自定义镜像；
- Result Vault 中的最终性能归档；
- 本地或仓库中的脚本、spec 和审计日志。

## 8. 性能有效性与通过标准

测试不得只报告某一个 fio 场景。每次 BrewFS/JuiceFS 对比必须：

- 使用同一 VM image、ECS 规格、zone、vSwitch 和安全组；
- 使用同一 OSS bucket region、相同对象存储配置和相同 prefix 生命周期；
- 使用相同 Redis/Tair 类型、规格、TLS 设置和隔离策略；
- 使用相同 fio size、runtime、direct mode、compression、cache budget、writeback/drain 语义；
- 至少重复 3 次，报告中位数和离散程度；
- 将 `fio-randrw`、延迟、关闭/flush/drain 时间作为一等指标；
- 记录 OSS PUT/GiB、平均对象大小、partial-tail 比例、slice 数量和 Redis commandstats；
- 任何只改善 runtime throughput、却把成本转移到 close/flush/drain 的结果不得作为接受结果。

## 9. 待确认项

在实现 managed-backend runner 之前，需要确认以下值：

1. OSS endpoint、region、bucket 是否已创建，测试账号是否只允许访问指定 bucket/prefix。
2. Redis/Tair 是单实例多 DB、两个实例，还是不支持 DB 的 Tair 配置；是否启用 TLS。
3. 测试 ECS 到 OSS、Redis/Tair、Result Vault 的网络路径是否都可达。
4. 目标 ECS instance type 和临时数据盘大小。
5. JuiceFS 固定版本，以及是否允许 image 内置该版本而不在每轮上传。
6. 先执行最小 `looptest`，还是直接执行完整 fio + metadata 矩阵。

在这些值确认前，不创建新的测试 ECS，不修改长期镜像，也不开始正式性能测试。
