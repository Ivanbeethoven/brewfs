[CmdletBinding()]
param(
    [ValidateSet('run', 'create', 'status', 'destroy')]
    [string]$Action = 'run',
    [string]$InstanceId,
    [string]$RegionId = 'cn-hangzhou',
    [string]$ZoneId = 'cn-hangzhou-h',
    [string]$VSwitchId,
    [string]$SecurityGroupId,
    [string]$InstanceName,
    [string]$InstanceType = 'ecs.u1-c1m4.2xlarge',
    [ValidateRange(40, 1000)]
    [int]$SystemDiskSizeGiB = 100,
    [string]$ImageId = 'ubuntu_24_04_x64_20G_alibase_20260916.vhd',
    [ValidateSet('redis', 'none')]
    [string]$Backend = 'none',
    [ValidateSet('s3')]
    [string]$DataBackend = 's3',
    [ValidateSet('packed-metadata-v1')]
    [string]$VolumeFormat = 'packed-metadata-v1',
    [string]$PerfTools = 'packed-smallfiles packed-posix fio-seqread fio-randread',
    [int64]$PackedSmallFileCount = 1000000,
    [int64]$PackedSmallFileSizeBytes = 102400,
    [int]$PackedDirLevels = 3,
    [int64]$PackedDirsPerLevel = 10,
    [int64]$PackedFilesPerDir = 1000,
    [int64]$PackedFioFileSizeBytes = 67108864,
    [string]$PackedSmallFileReadBytes = '0',
    [string]$PackedExistingManifestKey,
    [switch]$PackedSkipFixture,
    [int]$FioRuntimeSeconds = 20,
    [string]$S3Bucket,
    [string]$S3Endpoint,
    [string]$S3Region = 'cn-hangzhou',
    [string]$S3AccessKey,
    [string]$S3SecretKey,
    [bool]$S3ForcePathStyle = $false,
    [string]$RepoRoot,
    [string]$WslDistribution = 'Ubuntu-24.04',
    [string]$BinaryPath,
    [string]$FixtureBinaryPath,
    [switch]$SkipBuild,
    [string]$ArtifactDirectory,
    [string]$ObjectPrefix,
    [string]$Repository,
    [string]$Ref,
    [switch]$ColdRead,
    [switch]$RunBench,
    [string]$AutoReleaseMinutes = '480',
    [switch]$KeepInstance,
    [switch]$NoCleanup,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:CreatedInstance = $false
$script:RunArtifactDirectory = $null
$script:CredentialBundleKey = $null

function Resolve-Executable([string]$Name, [string[]]$Candidates = @()) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw "找不到 $Name，请先安装并加入 PATH。"
}

$aliyunCandidates = @()
if ($env:LOCALAPPDATA) { $aliyunCandidates += (Join-Path $env:LOCALAPPDATA 'AliyunCLI\aliyun.exe') }
$Aliyun = Resolve-Executable 'aliyun' $aliyunCandidates

function Invoke-Checked([string]$File, [string[]]$Arguments) {
    $output = & $File @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "命令失败: $File $($Arguments -join ' ')$([Environment]::NewLine)$($output -join [Environment]::NewLine)"
    }
    return $output
}

function Invoke-AliyunJson([string[]]$Arguments) {
    $output = Invoke-Checked $Aliyun $Arguments
    return ($output -join [Environment]::NewLine | ConvertFrom-Json)
}

function Format-InstanceIds([string]$Value) {
    return '["' + $Value + '"]'
}

function Wait-Until([scriptblock]$Condition, [string]$Description, [int]$TimeoutSeconds = 900) {
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        try { if (& $Condition) { return } } catch { }
        Start-Sleep -Seconds 5
    } while ((Get-Date) -lt $deadline)
    throw "等待超时: $Description"
}

function Quote-Bash([string]$Value) {
    $replacement = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $replacement) + "'"
}

function Get-LocalRepoRoot {
    if ($RepoRoot) { return (Resolve-Path -LiteralPath $RepoRoot).Path }
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
}

function Get-WslPath([string]$WindowsPath) {
    $full = (Resolve-Path -LiteralPath $WindowsPath).Path
    if ($full -notmatch '^[A-Za-z]:\\') {
        throw "WSL 本地构建要求工作树位于 Windows 本地盘: $full"
    }
    $drive = $full.Substring(0, 1).ToLowerInvariant()
    $rest = $full.Substring(2).Replace('\', '/')
    return "/mnt/$drive$rest"
}

function Get-ConfiguredCredentials {
    $configCandidates = @()
    if ($env:USERPROFILE) { $configCandidates += (Join-Path $env:USERPROFILE '.aliyun\config.json') }
    if ($env:HOME) { $configCandidates += (Join-Path $env:HOME '.aliyun\config.json') }
    foreach ($path in $configCandidates) {
        if (-not (Test-Path -LiteralPath $path)) { continue }
        $config = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
        $profileName = [string]$config.current
        $profile = @($config.profiles | Where-Object { $_.name -eq $profileName })[0]
        if ($profile -and $profile.mode -eq 'AK' -and $profile.access_key_id -and $profile.access_key_secret) {
            return @([string]$profile.access_key_id, [string]$profile.access_key_secret)
        }
    }
    throw '未找到 Aliyun AK/SK。请配置 aliyun CLI，或显式传入 -S3AccessKey/-S3SecretKey。'
}

function Build-LocalBinaries {
    $root = Get-LocalRepoRoot
    if (-not $BinaryPath) { $script:BinaryPath = Join-Path $root 'target\release\brewfs' }
    if (-not $FixtureBinaryPath) { $script:FixtureBinaryPath = Join-Path $root 'target\release\packed_snapshot_fixture' }
    if ($SkipBuild) {
        if (-not (Test-Path -LiteralPath $BinaryPath) -or -not (Test-Path -LiteralPath $FixtureBinaryPath)) {
            throw '-SkipBuild 要求 -BinaryPath 和 -FixtureBinaryPath 都存在。'
        }
        return
    }
    $wslRoot = Get-WslPath $root
    $command = @"
set -Eeuo pipefail
cd $(Quote-Bash $wslRoot)
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_DEBUG=0
cargo build --release --features native-packed-base,frozen-base-metadata --bin brewfs --bin packed_snapshot_fixture
strip target/release/brewfs target/release/packed_snapshot_fixture
file target/release/brewfs target/release/packed_snapshot_fixture
"@
    $output = & wsl.exe -d $WslDistribution -- bash -lc $command 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "WSL 本地编译失败:$([Environment]::NewLine)$($output -join [Environment]::NewLine)"
    }
    Write-Host ($output -join [Environment]::NewLine)
    if (-not (Test-Path -LiteralPath $BinaryPath) -or -not (Test-Path -LiteralPath $FixtureBinaryPath)) {
        throw 'WSL 编译完成但没有找到 Linux ELF 二进制。'
    }
}

function Publish-OssObject([string]$Path, [string]$Key) {
    $uri = "oss://$S3Bucket/$Key"
    Write-Host "上传 OSS 对象: $Key"
    $output = & $Aliyun oss cp $Path $uri --region $S3Region --force 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "OSS 上传失败 ($Key): $($output -join [Environment]::NewLine)"
    }
}

function Get-OssSignedUrl([string]$Key, [int]$TimeoutSeconds = 172800) {
    $uri = "oss://$S3Bucket/$Key"
    $output = Invoke-Checked $Aliyun @('oss', 'sign', $uri, '--region', $S3Region, '--timeout', [string]$TimeoutSeconds)
    $joined = $output -join [Environment]::NewLine
    $match = [regex]::Match($joined, 'https?://[^\s]+')
    if (-not $match.Success) { throw "OSS 签名 URL 生成失败 ($Key): $joined" }
    return $match.Value.TrimEnd('.', ',')
}

function Publish-CredentialBundle([string]$Key) {
    $path = Join-Path ([IO.Path]::GetTempPath()) ("brewfs-oss-{0}.env" -f [Guid]::NewGuid().ToString('N'))
    $contents = @(
        "export AWS_ACCESS_KEY_ID=$(Quote-Bash $S3AccessKey)"
        "export AWS_SECRET_ACCESS_KEY=$(Quote-Bash $S3SecretKey)"
        "export AWS_DEFAULT_REGION=$(Quote-Bash $S3Region)"
    ) -join "`n"
    try {
        [IO.File]::WriteAllText($path, $contents, [Text.Encoding]::ASCII)
        Publish-OssObject $path $Key
        $script:CredentialBundleKey = $Key
    }
    finally {
        Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
    }
}

function New-EcsInstance {
    if (-not $VSwitchId -or -not $SecurityGroupId) {
        throw '创建 ECS 需要 -VSwitchId 和 -SecurityGroupId。脚本不自动创建 VPC。'
    }
    if (-not $script:InstanceName) { $script:InstanceName = 'brewfs-packed-{0}' -f (Get-Date -Format 'yyyyMMdd-HHmmss') }
    $release = (Get-Date).ToUniversalTime().AddMinutes([int]$AutoReleaseMinutes).ToString('yyyy-MM-ddTHH:mm:ssZ')
    $clientToken = [Guid]::NewGuid().ToString('N')
    $runArgs = @(
        'ecs', 'RunInstances', '--region', $RegionId,
        '--ImageId', $ImageId, '--InstanceType', $InstanceType,
        '--VSwitchId', $VSwitchId, '--SecurityGroupId', $SecurityGroupId,
        '--ZoneId', $ZoneId, '--Amount', '1', '--InstanceName', $InstanceName,
        '--ClientToken', $clientToken,
        '--InstanceChargeType', 'PostPaid', '--InternetChargeType', 'PayByTraffic',
        '--InternetMaxBandwidthOut', '20', '--AutoReleaseTime', $release,
        '--SystemDisk.Category', 'cloud_essd', '--SystemDisk.Size', [string]$SystemDiskSizeGiB,
        '--SystemDisk.PerformanceLevel', 'PL1',
        '--Tag.1.Key', 'brewfs-test', '--Tag.1.Value', $InstanceName
    )
    $result = Invoke-AliyunJson $runArgs
    $script:InstanceId = @($result.InstanceIdSets.InstanceIdSet)[0]
    if (-not $script:InstanceId) { throw 'RunInstances 未返回 InstanceId。' }
    $script:CreatedInstance = $true
    Write-Host "ECS 创建成功: $script:InstanceId"
    Wait-Until {
        $instance = Invoke-AliyunJson @('ecs', 'DescribeInstances', '--region', $RegionId, '--InstanceIds', (Format-InstanceIds $script:InstanceId))
        $state = @($instance.Instances.Instance)[0].Status
        Write-Host "  ECS state=$state"
        $state -eq 'Running'
    } 'ECS 启动' 900
}

function Get-RemoteCommand([string]$BinaryUrl, [string]$FixtureUrl, [string]$RunnerUrl, [string]$CredentialUrl) {
    $remote = @'
#!/usr/bin/env bash
set -Eeuo pipefail
export DEBIAN_FRONTEND=noninteractive
WORK=/opt/brewfs-packed-native
ARTIFACT_DIR="$WORK/artifacts"
mkdir -p "$WORK" "$ARTIFACT_DIR"

if ! apt-get update -qq; then
  if [[ -f /etc/apt/sources.list ]]; then
    awk '/^[[:space:]]*(deb|deb-src)[[:space:]]/ || /^[[:space:]]*#/ || /^[[:space:]]*$/ { print; next } { print "# disabled invalid apt source: " $0 }' /etc/apt/sources.list >/etc/apt/sources.list.brewfs-clean
    mv /etc/apt/sources.list.brewfs-clean /etc/apt/sources.list
  fi
  apt-get update -qq
fi
apt-get install -y -qq ca-certificates curl fio fuse3 python3 util-linux procps
modprobe fuse 2>/dev/null || true

curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/brewfs" __BINARY_URL__
curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/packed_snapshot_fixture" __FIXTURE_URL__
curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/run_packed_native.sh" __RUNNER_URL__
chmod 0755 "$WORK/brewfs" "$WORK/packed_snapshot_fixture" "$WORK/run_packed_native.sh"

curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/s3-credentials.env" __CREDENTIAL_URL__
chmod 0600 "$WORK/s3-credentials.env"
source "$WORK/s3-credentials.env"
rm -f "$WORK/s3-credentials.env"
export AWS_EC2_METADATA_DISABLED=true
export BREWFS_S3_BUCKET=__S3_BUCKET__
export BREWFS_S3_ENDPOINT=__S3_ENDPOINT__
export BREWFS_S3_REGION=__S3_REGION__
export BREWFS_S3_FORCE_PATH_STYLE=__S3_FORCE_PATH_STYLE__
export BREWFS_BIN="$WORK/brewfs"
export PACKED_FIXTURE_BIN="$WORK/packed_snapshot_fixture"
export BREWFS_NATIVE_WORK="$WORK"
export BREWFS_NATIVE_ARTIFACT_DIR="$ARTIFACT_DIR"
export PACKED_FIXTURE_PREFIX=__FIXTURE_PREFIX__
export PACKED_SKIP_FIXTURE=__PACKED_SKIP_FIXTURE__
export PACKED_EXISTING_MANIFEST_KEY=__PACKED_EXISTING_MANIFEST_KEY__
export PACKED_SMALLFILE_COUNT=__SMALLFILE_COUNT__
export PACKED_SMALLFILE_SIZE=__SMALLFILE_SIZE__
export PACKED_DIR_LEVELS=__DIR_LEVELS__
export PACKED_DIRS_PER_LEVEL=__DIRS_PER_LEVEL__
export PACKED_FILES_PER_DIR=__FILES_PER_DIR__
export PERF_PACKED_FIO_FILE_SIZE=__FIO_FILE_SIZE__
export PERF_PACKED_SMALLFILE_READ_BYTES=__READ_BYTES__
export PERF_FIO_RUNTIME=__FIO_RUNTIME__
export PERF_TOOLS=__TOOLS__
export BREWFS_READ_MEMORY_BYTES=0
export BREWFS_READ_SSD_BYTES=0
export BREWFS_PREFETCH_ENABLED=false
export BREWFS_RANGE_BACKGROUND_PREFETCH=false
export BREWFS_FUSE_READ_DIRECT_IO=1
export BREWFS_FUSE_KEEP_CACHE=0
export BREWFS_NOFILE_LIMIT=1048576
export RUST_LOG=warn

mem_kib="$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo)"
disk_bytes="$(df -B1 --output=size "$WORK" | tail -n 1 | tr -d ' ')"
cat >"$WORK/aliyun-resource-proof.env" <<EOF
instance_type=__INSTANCE_TYPE__
requested_memory_gib=32
requested_system_disk_gib=__SYSTEM_DISK_GIB__
mem_total_kib=$mem_kib
work_disk_bytes=$disk_bytes
s3_endpoint=__S3_ENDPOINT__
s3_bucket=__S3_BUCKET__
metadata_backend=none
container_runtime=none
EOF
[[ "$mem_kib" -ge 30000000 ]] || { echo "memory below 32 GiB" >&2; exit 1; }
[[ "$disk_bytes" -ge 90000000000 ]] || { echo "disk below 100 GB" >&2; exit 1; }

if bash "$WORK/run_packed_native.sh"; then
  :
else
  status=$?
  echo "--- packed native runner failed (exit=$status) ---"
  for log in "$ARTIFACT_DIR"/packed-fixture.log "$ARTIFACT_DIR"/brewfs.log "$ARTIFACT_DIR"/tools/*.log; do
    if [[ -f "$log" ]]; then
      echo "### $log"
      tail -n 80 "$log" || true
    fi
  done
  exit "$status"
fi
cat "$WORK/aliyun-resource-proof.env"
echo '--- packed native perf summary ---'
cat "$ARTIFACT_DIR/perf-summary.tsv"
echo '--- packed native tool tails ---'
for log in "$ARTIFACT_DIR"/tools/*.log; do
  echo "### $log"
  tail -n 12 "$log" || true
done
'@
    $values = @{
        '__BINARY_URL__' = Quote-Bash $BinaryUrl
        '__FIXTURE_URL__' = Quote-Bash $FixtureUrl
        '__RUNNER_URL__' = Quote-Bash $RunnerUrl
        '__CREDENTIAL_URL__' = Quote-Bash $CredentialUrl
        '__S3_REGION__' = Quote-Bash $S3Region
        '__S3_BUCKET__' = Quote-Bash $S3Bucket
        '__S3_ENDPOINT__' = Quote-Bash $S3Endpoint
        '__S3_FORCE_PATH_STYLE__' = Quote-Bash ($S3ForcePathStyle.ToString().ToLowerInvariant())
        '__FIXTURE_PREFIX__' = Quote-Bash $script:FixturePrefix
        '__PACKED_SKIP_FIXTURE__' = Quote-Bash ($PackedSkipFixture.ToString().ToLowerInvariant())
        '__PACKED_EXISTING_MANIFEST_KEY__' = Quote-Bash ([string]$PackedExistingManifestKey)
        '__SMALLFILE_COUNT__' = Quote-Bash ([string]$PackedSmallFileCount)
        '__SMALLFILE_SIZE__' = Quote-Bash ([string]$PackedSmallFileSizeBytes)
        '__DIR_LEVELS__' = Quote-Bash ([string]$PackedDirLevels)
        '__DIRS_PER_LEVEL__' = Quote-Bash ([string]$PackedDirsPerLevel)
        '__FILES_PER_DIR__' = Quote-Bash ([string]$PackedFilesPerDir)
        '__FIO_FILE_SIZE__' = Quote-Bash ([string]$PackedFioFileSizeBytes)
        '__READ_BYTES__' = Quote-Bash ([string]$PackedSmallFileReadBytes)
        '__FIO_RUNTIME__' = Quote-Bash ([string]$FioRuntimeSeconds)
        '__TOOLS__' = Quote-Bash $PerfTools
        '__INSTANCE_TYPE__' = Quote-Bash $InstanceType
        '__SYSTEM_DISK_GIB__' = [string]$SystemDiskSizeGiB
    }
    foreach ($key in $values.Keys) { $remote = $remote.Replace($key, [string]$values[$key]) }
    return $remote
}

function Invoke-PerfOnEcs {
    $artifact = if ($ArtifactDirectory) { $ArtifactDirectory } else { Join-Path $PSScriptRoot '..\artifacts\aliyun-native' }
    New-Item -ItemType Directory -Force -Path $artifact | Out-Null
    $script:RunArtifactDirectory = (Resolve-Path -LiteralPath $artifact).Path
    $commandText = Get-RemoteCommand $script:BinaryUrl $script:FixtureUrl $script:RunnerUrl $script:CredentialUrl
    $content = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($commandText))
    $run = Invoke-AliyunJson @(
        'ecs', 'RunCommand', '--region', $RegionId, '--Type', 'RunShellScript',
        '--InstanceId.1', $script:InstanceId, '--CommandContent', $content,
        '--ContentEncoding', 'Base64', '--Timeout', '172800',
        '--KeepCommand', 'false', '--Name', 'brewfs-packed-native'
    )
    $invokeId = $run.InvokeId
    if (-not $invokeId) { throw 'RunCommand 未返回 InvokeId。请确认 ECS Cloud Assistant Agent 在线。' }
    Write-Host "原生 packed 测试已提交: $invokeId"
    $deadline = (Get-Date).AddHours(48)
    $done = $false
    while (-not $done -and (Get-Date) -lt $deadline) {
        $result = Invoke-AliyunJson @('ecs', 'DescribeInvocationResults', '--region', $RegionId, '--InvokeId', $invokeId)
        $item = @($result.Invocation.InvocationResults.InvocationResult)[0]
        if ($item) {
            Write-Host "  invocation status=$($item.InvocationStatus)"
            if ($item.InvocationStatus -in @('Success', 'Failed', 'Stopped', 'Error', 'Terminated')) {
                $decoded = ''
                if ($item.Output) { $decoded = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($item.Output)) }
                Set-Content -LiteralPath (Join-Path $script:RunArtifactDirectory 'remote-output.log') -Value $decoded -Encoding UTF8
                Write-Host $decoded
                $done = $true
                if ($item.InvocationStatus -ne 'Success') { throw "远程原生测试失败: $($item.ErrorInfo)" }
            }
        }
        if (-not $done) { Start-Sleep -Seconds 10 }
    }
    if (-not $done) { throw '等待远程原生测试超时。' }
}

function Remove-EcsInstance {
    if (-not $script:InstanceId) { throw 'destroy 需要 -InstanceId。' }
    $instance = Invoke-AliyunJson @('ecs', 'DescribeInstances', '--region', $RegionId, '--InstanceIds', (Format-InstanceIds $script:InstanceId))
    $item = @($instance.Instances.Instance)[0]
    if (-not $item) { Write-Host "ECS 已不存在: $script:InstanceId"; return }
    if ($item.Status -ne 'Stopped') {
        if ($item.Status -ne 'Stopping') {
            Invoke-AliyunJson @('ecs', 'StopInstance', '--region', $RegionId, '--InstanceId', $script:InstanceId, '--ForceStop', 'true') | Out-Null
        }
        Wait-Until {
            $current = Invoke-AliyunJson @('ecs', 'DescribeInstances', '--region', $RegionId, '--InstanceIds', (Format-InstanceIds $script:InstanceId))
            @($current.Instances.Instance)[0].Status -eq 'Stopped'
        } "ECS $script:InstanceId 停止" 300
    }
    Invoke-AliyunJson @('ecs', 'DeleteInstance', '--region', $RegionId, '--InstanceId', $script:InstanceId, '--Force', 'true') | Out-Null
    Write-Host "ECS 删除任务已提交: $script:InstanceId"
}

try {
    if ($Action -eq 'status') {
        if (-not $InstanceId) { throw 'status 需要 -InstanceId。' }
        $script:InstanceId = $InstanceId
        Invoke-AliyunJson @('ecs', 'DescribeInstances', '--region', $RegionId, '--InstanceIds', (Format-InstanceIds $script:InstanceId)) | ConvertTo-Json -Depth 8
        return
    }
    if ($Action -eq 'destroy') {
        if (-not $InstanceId) { throw 'destroy 需要 -InstanceId。' }
        $script:InstanceId = $InstanceId
        Remove-EcsInstance
        return
    }
    if ($Action -eq 'create') { New-EcsInstance; return }

    if ($VolumeFormat -ne 'packed-metadata-v1' -or $DataBackend -ne 's3') {
        throw '原生 ECS runner 只接受 packed-metadata-v1 + Aliyun S3/OSS。'
    }
    if (-not $S3Bucket) { throw 'run 必须指定 -S3Bucket（Aliyun OSS bucket）。' }
    $expected = [int64]1
    for ($level = 0; $level -lt $PackedDirLevels; $level++) { $expected *= $PackedDirsPerLevel }
    $expected *= $PackedFilesPerDir
    if ($expected -ne $PackedSmallFileCount) { throw "packed 文件数量不一致: expected=$expected actual=$PackedSmallFileCount" }
    if ($PackedSmallFileSizeBytes -le 0 -or $PackedSmallFileSizeBytes -gt 4MB) { throw 'PackedSmallFileSizeBytes 必须在 1 到 4 MiB 之间。' }
    if ($PackedSkipFixture -and -not $PackedExistingManifestKey) { throw '-PackedSkipFixture 必须同时指定 -PackedExistingManifestKey。' }
    if (-not $S3Endpoint) { $S3Endpoint = "https://oss-$S3Region.aliyuncs.com" }
    if (-not $S3AccessKey -or -not $S3SecretKey) {
        $credentials = Get-ConfiguredCredentials
        if (-not $S3AccessKey) { $S3AccessKey = $credentials[0] }
        if (-not $S3SecretKey) { $S3SecretKey = $credentials[1] }
    }
    if (-not $ObjectPrefix) { $script:ObjectPrefix = 'brewfs-native-{0}' -f (Get-Date -Format 'yyyyMMdd-HHmmss') }
    $script:FixturePrefix = "$ObjectPrefix/fixture"

    Build-LocalBinaries
    Publish-OssObject $BinaryPath "$ObjectPrefix/bin/brewfs"
    Publish-OssObject $FixtureBinaryPath "$ObjectPrefix/bin/packed_snapshot_fixture"
    $nativeScriptPath = Join-Path $PSScriptRoot 'run_aliyun_packed_native.sh'
    Publish-OssObject $nativeScriptPath "$ObjectPrefix/bin/run_aliyun_packed_native.sh"
    $credentialKey = "$ObjectPrefix/bootstrap/s3-credentials.env"
    Publish-CredentialBundle $credentialKey
    $script:BinaryUrl = Get-OssSignedUrl "$ObjectPrefix/bin/brewfs"
    $script:FixtureUrl = Get-OssSignedUrl "$ObjectPrefix/bin/packed_snapshot_fixture"
    $script:RunnerUrl = Get-OssSignedUrl "$ObjectPrefix/bin/run_aliyun_packed_native.sh"
    $script:CredentialUrl = Get-OssSignedUrl $credentialKey 28800

    if (-not $InstanceId) { New-EcsInstance } else { $script:InstanceId = $InstanceId }
    if ($DryRun) { Write-Host "Dry run: 本地 Linux binary 已上传，未下发远程命令。instance=$script:InstanceId"; return }
    Invoke-PerfOnEcs
}
finally {
    if ($script:CredentialBundleKey) {
        try {
            $credentialUri = "oss://$S3Bucket/$($script:CredentialBundleKey)"
            Invoke-Checked $Aliyun @('oss', 'rm', $credentialUri, '--region', $S3Region, '--force') | Out-Null
            Write-Host '已删除临时 OSS 凭据对象。'
        } catch {
            Write-Warning "临时 OSS 凭据对象清理失败，限时签名 URL 仍会在 8 小时后过期: $($_.Exception.Message)"
        }
    }
    if ($Action -eq 'run' -and $script:CreatedInstance -and -not $KeepInstance -and -not $NoCleanup) {
        try { Remove-EcsInstance } catch { Write-Warning "ECS 自动清理失败: $($_.Exception.Message)" }
    }
}
