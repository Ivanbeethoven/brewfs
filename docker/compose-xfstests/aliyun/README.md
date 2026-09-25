# Aliyun 云端性能测试

这个目录提供 Aliyun ECS 上的原生 packed-metadata 验证入口。脚本在本机 WSL 编译 Linux BrewFS 和 fixture 二进制，把二进制上传到 Aliyun OSS，再由 ECS Cloud Assistant 直接运行。ECS 不安装 Docker、不启动 Compose、不启动 Redis/RustFS；packed metadata 和数据对象都来自指定的 Aliyun OSS/S3 bucket。

## 百万级 packed 小文件测试

`run_aliyun_packed_million.ps1` 是专用入口，目录布局默认是：

```text
root/
  d000..d009/
    d000..d009/
      d000..d009/
        f00000..f00999  (100 KiB each)
```

也就是 `10 x 10 x 10 x 1,000 = 1,000,000` 个文件。packed fixture 的小文件数据共享一个不可变 block，因此不会在 ECS 本地落下约 100 GiB 的重复 payload；全文件读模式仍会实际读取约 100 GiB 的逻辑数据并记录 payload bytes。

```powershell
# 先用 -DryRun 检查参数；需要已有 vSwitch 和安全组。
.\docker\compose-xfstests\aliyun\run_aliyun_packed_million.ps1 `
  -DryRun `
  -VSwitchId vsw-xxxxxxxx `
  -SecurityGroupId sg-xxxxxxxx

# 创建 ECS，构建当前本地工作树，发布 packed fixture，冷读扫描后自动释放 ECS。
.\docker\compose-xfstests\aliyun\run_aliyun_packed_million.ps1 `
  -VSwitchId vsw-xxxxxxxx `
  -SecurityGroupId sg-xxxxxxxx `
  -S3Bucket my-brewfs-test-bucket `
  -RegionId cn-hangzhou `
  -ZoneId cn-hangzhou-h `
  -ImageId ubuntu_24_04_x64_20G_alibase_20260916.vhd `
  -Ref main
```

默认使用 `ReadMode=full`，会读取每个 100 KiB 文件；若只想先验证元数据路径，可使用 `-ReadMode prefix`。测试固定关闭 BrewFS 数据缓存和预取，并要求 `drop_caches` 成功；结果不会把缓存命中当成冷读性能。`-KeepInstance` 可保留现场，`-NoCleanup` 禁止自动释放，完成后使用原 ECS runner 的 `-Action destroy` 清理。

默认会用当前工作树的 WSL2 `Ubuntu-24.04` 环境本地编译；也可以用 `-SkipBuild -BinaryPath ... -FixtureBinaryPath ...` 传入已经编好的 Linux ELF。OSS bucket 必须事先存在，上传的对象使用唯一前缀，测试结束后不会删除用户 bucket。

默认运行的对象和缓存约束：

- 目录布局为 `10 x 10 x 10 x 1,000 = 1,000,000` 个 100 KiB 文件。
- fixture 的 namespace、data rows 和 payload block 均发布到 OSS；挂载时没有元数据数据库，`packed-metadata-v1` 直接从 OSS manifest/index 对象读取。
- 每个工具开始前卸载并重挂载 BrewFS，删除本地 cache root，执行 `sync; echo 3 >/proc/sys/vm/drop_caches`，失败就拒绝产出性能结果。
- `read_memory_bytes=0`、`read_ssd_bytes=0`、prefetch 关闭、FUSE read direct-io 开启；结果明确是无缓存冷读。
- `packed-tree` 只遍历并校验百万文件的目录树；`packed-smallfiles` 做百万文件完整扫描，`packed-posix` 做只读语义检查，`fio-seqread`/`fio-randread` 只读 `bench/read.bin`。目录扫描必须报告 `walk_errors`，不能让 `os.walk` 静默跳过目录。

## ACK/Kubernetes 主流程

性能测试的推荐路径是本地构建镜像后交给 ACK 运行，避免在临时 ECS 上冷编译。`run_aliyun_perf_k8s.ps1` 使用 `Dockerfile.perf-local` 在本地 Docker builder 中构建 Linux BrewFS 镜像，推送到 GHCR（或其他可访问 registry），然后在已有 ACK 集群中创建 Redis/TiKV 依赖和特权 FUSE Job，并把 `/artifacts` 拷回本地。

```powershell
$env:BREWFS_RESULTS_URL = 'https://results.example.com'

.\docker\compose-xfstests\aliyun\run_aliyun_perf_k8s.ps1 `
  -KubeconfigPath $env:KUBECONFIG `
  -RegistryImage ghcr.io/ivanbeethoven/brewfs-perf `
  -GhcrUsername Ivanbeethoven `
  -GhcrToken $env:GHCR_TOKEN `
  -Backend redis -DataBackend local-fs `
  -ArtifactDirectory .\docker\compose-xfstests\artifacts\ack-redis
```

测试完成后脚本会在本地输出两个结果：完整结果目录和同名 `.zip` 归档。设置 `BREWFS_RESULTS_URL` 后，脚本还会自动把同一个 ZIP POST 到网站；`-ResultVaultUrl` 可临时覆盖环境变量，未配置时只保存在本地。网站不可用时不会丢弃本地结果，只会发出警告。归档包含性能报告、原始日志、BrewFS 日志、后端诊断和性能统计，便于上传或脱离集群查看（xfstests/LTP runner 的 artifacts 也使用同样的目录结构）。脚本会在容器中先生成单个 `tar.gz` 再下载，避免逐文件复制时出现 `unexpected EOF`。

默认情况下，无论测试成功还是失败，runner 都会清理本轮带有 `app.kubernetes.io/managed-by=brewfs-perf-runner` 标签的 Job、Redis/TiKV、RustFS、ConfigMap 和镜像拉取 Secret，避免共享 ACK 集群上留下持续占用节点的资源。需要保留现场或手工导出时使用 `-KeepJob`；之后通过 `-Action destroy` 清理。

若希望在测试进行时从另一终端手动导出，保留 Job 并延长结果保留窗口：

```powershell
$tag = 'aliyun-20260904-redis'
.\docker\compose-xfstests\aliyun\run_aliyun_perf_k8s.ps1 `
  -KubeconfigPath $env:KUBECONFIG -ImageTag $tag -Backend redis `
  -KeepJob -ArtifactHoldSeconds 1800

.\docker\compose-xfstests\aliyun\run_aliyun_perf_k8s.ps1 `
  -Action export -JobName "brewfs-perf-$tag" `
  -KubeconfigPath $env:KUBECONFIG -ArtifactDirectory .\artifacts\manual
```

`-Action export` 只能在 Pod 仍处于 Running 且 `perf.complete` 已出现的 hold 窗口内执行；默认 `emptyDir` 随 Pod 结束而消失。因此正常使用应直接等待 `-Action test` 自动导出。若需要测试结束后仍可导出，应为 Job 改用持久化卷（后续可增加 `-ArtifactPvc` 参数）。不带 `-KeepJob` 时，自动导出完成后会立即清理测试资源，不再等待 hold 窗口。

ACK 集群本身可使用 `operator/brewfs-operator/scripts/ack-e2e.ps1` 创建/销毁；K8s runner 不创建 VPC、节点或账号级网络资源。`run_aliyun_perf.ps1` 保留为 ECS/Cloud Assistant fallback，适合没有 ACK 集群的故障诊断，不是主性能测试路径。

## 前置条件

- Aliyun CLI 已配置，并具备 ECS、VPC 查询、RunCommand 权限。
- 目标地域已有可用的 VPC vSwitch 和安全组；脚本不会自动创建或删除账号网络资源。
- ECS 镜像内置 Cloud Assistant Agent，且能访问软件源和 GitHub/GHCR。
- 目标镜像在该地域可用。百万级入口默认使用 `ubuntu_24_04_x64_20G_alibase_20260916.vhd`，可用 `ecs DescribeImages` 查询并通过 `-ImageId` 覆盖；通用 runner 仍可单独传入 `-ImageId`。

## 原生 ECS 使用方式

```powershell
# 本地编译二进制，上传 OSS，ECS 直接连接 Aliyun OSS/S3，结束后自动释放 ECS
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 `
  -Action run `
  -VSwitchId vsw-xxxxxxxx `
  -SecurityGroupId sg-xxxxxxxx `
  -S3Bucket my-brewfs-test-bucket `
  -S3Region cn-hangzhou `
  -S3Endpoint https://oss-cn-hangzhou.aliyuncs.com

# 先创建 ECS，手动检查后再运行；不需要 Docker
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 `
  -Action create -VSwitchId vsw-xxxxxxxx -SecurityGroupId sg-xxxxxxxx
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 `
  -Action run -InstanceId i-xxxxxxxx -S3Bucket my-brewfs-test-bucket -KeepInstance

# 单独创建、查看和销毁
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 -Action create `
  -VSwitchId vsw-xxxxxxxx -SecurityGroupId sg-xxxxxxxx
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 -Action status `
  -InstanceId i-xxxxxxxx -RegionId ap-northeast-2
.\docker\compose-xfstests\aliyun\run_aliyun_perf.ps1 -Action destroy `
  -InstanceId i-xxxxxxxx -RegionId ap-northeast-2
```

## 参数

| ECS 脚本参数 | Compose 等价行为 |
| --- | --- |
| `-S3Bucket` | Aliyun OSS bucket，必须预先创建 |
| `-S3Endpoint` | 默认为 `https://oss-$S3Region.aliyuncs.com` |
| `-S3AccessKey/-S3SecretKey` | 默认读取本机 Aliyun CLI 当前 AK profile；也可显式覆盖 |
| `-SkipBuild -BinaryPath -FixtureBinaryPath` | 跳过 WSL 编译，使用已有 Linux ELF |
| `-PerfTools` | `packed-tree`、`packed-smallfiles`、`packed-posix`、`fio-seqread`、`fio-randread` 的子集 |
| `-PackedSkipFixture -PackedExistingManifestKey` | 诊断时复用已有 packed manifest，跳过百万 fixture 发布；仅用于已确认 manifest 的复核 |
| `-KeepInstance` | 测试后保留 ECS，便于检查远端日志 |

默认 ECS 为按量付费，并设置八小时自动释放时间；`run` 结束后还会主动释放实例，除非指定 `-KeepInstance` 或 `-NoCleanup`。脚本不会删除快照、VPC、vSwitch、安全组或 OSS bucket。创建后会在实例内校验内存至少 30,000,000 KiB、工作盘至少 90,000,000,000 字节，并把实际值写入 `aliyun-resource-proof.env`。远端结果摘要保存在 `-ArtifactDirectory` 指定目录的 `remote-output.log`。
