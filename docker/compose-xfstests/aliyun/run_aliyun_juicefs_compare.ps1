[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstanceId,
    [Parameter(Mandatory = $true)][string]$S3Bucket,
    [string]$S3Region = 'cn-hangzhou',
    [string]$S3AccessKey,
    [string]$S3SecretKey,
    [string]$ObjectPrefix = ('brewfs-jfs-{0}' -f (Get-Date -Format 'yyyyMMdd-HHmmss')),
    [string]$JuiceFsBinaryPath,
    [string]$RunnerPath = (Join-Path $PSScriptRoot 'run_aliyun_juicefs_native.sh'),
    [int64]$SmallFileCount = 10000,
    [int64]$SmallFileSizeBytes = 102400,
    [int]$DirLevels = 2,
    [int64]$DirsPerLevel = 10,
    [int64]$FilesPerDir = 100,
    [string]$ArtifactDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Resolve-Executable([string]$Name, [string[]]$Candidates = @()) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw "找不到 $Name"
}
$aliyunCandidates = @()
if ($env:LOCALAPPDATA) { $aliyunCandidates += (Join-Path $env:LOCALAPPDATA 'AliyunCLI\aliyun.exe') }
$Aliyun = Resolve-Executable 'aliyun' $aliyunCandidates

if ($JuiceFsBinaryPath) {
    if (-not (Test-Path -LiteralPath $JuiceFsBinaryPath)) {
        throw "找不到 JuiceFS 二进制: $JuiceFsBinaryPath"
    }
    $JuiceFsBinaryPath = (Resolve-Path -LiteralPath $JuiceFsBinaryPath).Path
} else {
    $juiceFsCandidates = @(
        (Join-Path $PSScriptRoot 'juicefs'),
        (Join-Path ([IO.Path]::GetTempPath()) 'juicefs')
    )
    if ($env:LOCALAPPDATA) {
        $juiceFsCandidates += (Join-Path $env:LOCALAPPDATA 'JuiceFS\juicefs.exe')
    }
    $JuiceFsBinaryPath = Resolve-Executable 'juicefs' $juiceFsCandidates
}

function Invoke-Checked([string]$File, [string[]]$Arguments) {
    $output = & $File @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "命令失败: $File $($Arguments -join ' ') $($output -join ' | ')"
    }
    return $output
}
function Invoke-AliyunJson([string[]]$Arguments) {
    return ((Invoke-Checked $Aliyun $Arguments) -join ' ' | ConvertFrom-Json)
}
function Quote-Bash([string]$Value) {
    $replacement = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $replacement) + "'"
}
function Get-Credentials {
    if ($S3AccessKey -and $S3SecretKey) { return @($S3AccessKey, $S3SecretKey) }
    $paths = @()
    if ($env:USERPROFILE) { $paths += (Join-Path $env:USERPROFILE '.aliyun\config.json') }
    if ($env:HOME) { $paths += (Join-Path $env:HOME '.aliyun\config.json') }
    foreach ($path in $paths) {
        if (-not (Test-Path -LiteralPath $path)) { continue }
        $config = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
        $profile = @($config.profiles | Where-Object { $_.name -eq [string]$config.current })[0]
        if ($profile -and $profile.mode -eq 'AK') {
            return @([string]$profile.access_key_id, [string]$profile.access_key_secret)
        }
    }
    throw '未找到 Aliyun AK/SK'
}
function Publish([string]$Path, [string]$Key) {
    Invoke-Checked $Aliyun @('oss', 'cp', $Path, "oss://$S3Bucket/$Key", '--region', $S3Region, '--force') | Out-Null
}
function Sign([string]$Key, [int]$Timeout = 28800) {
    $out = Invoke-Checked $Aliyun @('oss', 'sign', "oss://$S3Bucket/$Key", '--region', $S3Region, '--timeout', [string]$Timeout)
    $match = [regex]::Match(($out -join ' '), 'https?://[^\s]+')
    if (-not $match.Success) { throw "签名失败: $Key" }
    return $match.Value.TrimEnd('.', ',')
}

if (-not $ArtifactDirectory) {
    $ArtifactDirectory = Join-Path $PSScriptRoot '..\artifacts\aliyun-jfs-native'
}
New-Item -ItemType Directory -Force -Path $ArtifactDirectory | Out-Null
$script:CredentialKey = $null

$credentials = Get-Credentials
$S3AccessKey = $credentials[0]
$S3SecretKey = $credentials[1]
$prefix = $ObjectPrefix.TrimEnd('/')
$credentialPath = Join-Path ([IO.Path]::GetTempPath()) ("brewfs-jfs-{0}.env" -f [Guid]::NewGuid().ToString('N'))
$credentialText = @(
    "export AWS_ACCESS_KEY_ID=$(Quote-Bash $S3AccessKey)"
    "export AWS_SECRET_ACCESS_KEY=$(Quote-Bash $S3SecretKey)"
    "export AWS_DEFAULT_REGION=$(Quote-Bash $S3Region)"
) -join ([char]10)
[IO.File]::WriteAllText($credentialPath, $credentialText, [Text.Encoding]::ASCII)

try {
    Publish $JuiceFsBinaryPath "$prefix/bin/juicefs"
    Publish $RunnerPath "$prefix/bin/run_aliyun_juicefs_native.sh"
    $script:CredentialKey = "$prefix/bootstrap/s3-credentials.env"
    Publish $credentialPath $script:CredentialKey

    $binaryUrl = Sign "$prefix/bin/juicefs"
    $runnerUrl = Sign "$prefix/bin/run_aliyun_juicefs_native.sh"
    $credentialUrl = Sign $script:CredentialKey
    $remote = @'
#!/usr/bin/env bash
set -Eeuo pipefail
export DEBIAN_FRONTEND=noninteractive
WORK=/opt/juicefs-native
ARTIFACT_DIR="$WORK/artifacts"
mkdir -p "$WORK" "$ARTIFACT_DIR"
if ! apt-get update -qq; then
  sed -i 's|^deb cdrom:|# deb cdrom:|' /etc/apt/sources.list || true
  apt-get update -qq
fi
apt-get install -y -qq ca-certificates curl fuse3 python3 redis-server util-linux procps
modprobe fuse 2>/dev/null || true
curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/juicefs" __JUICEFS_URL__
curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/run_juicefs.sh" __RUNNER_URL__
curl --fail --location --retry 5 --connect-timeout 20 --output "$WORK/s3-credentials.env" __CREDENTIAL_URL__
chmod 0755 "$WORK/juicefs" "$WORK/run_juicefs.sh"
chmod 0600 "$WORK/s3-credentials.env"
source "$WORK/s3-credentials.env"
rm -f "$WORK/s3-credentials.env"
export AWS_EC2_METADATA_DISABLED=true
export JUICEFS_BIN="$WORK/juicefs"
export JFS_NATIVE_WORK="$WORK"
export JFS_NATIVE_ARTIFACT_DIR="$ARTIFACT_DIR"
export JFS_S3_BUCKET=__S3_BUCKET__
export JFS_S3_REGION=__S3_REGION__
export JFS_SMALLFILE_COUNT=__COUNT__
export JFS_SMALLFILE_SIZE=__SIZE__
export JFS_DIR_LEVELS=__LEVELS__
export JFS_DIRS_PER_LEVEL=__FANOUT__
export JFS_FILES_PER_DIR=__FILES_PER_DIR__
export JFS_VOLUME_NAME=__VOLUME_NAME__
export JFS_PREFETCH_CACHE_SIZE_MIB=4096
export JFS_PREFETCH_BLOCKS=16
export RUST_LOG=warn
if bash "$WORK/run_juicefs.sh"; then
  :
else
  status=$?
  echo "--- JuiceFS native runner failed (exit=$status) ---"
  for log in "$ARTIFACT_DIR"/prepare.log "$ARTIFACT_DIR"/scan-*.log "$WORK"/juicefs-*.log; do
    if [[ -f "$log" ]]; then echo "### $log"; tail -n 80 "$log" || true; fi
  done
  exit "$status"
fi
cat "$ARTIFACT_DIR/perf-summary.tsv"
'@
    $values = @{
        '__JUICEFS_URL__' = (Quote-Bash $binaryUrl)
        '__RUNNER_URL__' = (Quote-Bash $runnerUrl)
        '__CREDENTIAL_URL__' = (Quote-Bash $credentialUrl)
        '__S3_BUCKET__' = (Quote-Bash $S3Bucket)
        '__S3_REGION__' = (Quote-Bash $S3Region)
        '__COUNT__' = (Quote-Bash ([string]$SmallFileCount))
        '__SIZE__' = (Quote-Bash ([string]$SmallFileSizeBytes))
        '__LEVELS__' = (Quote-Bash ([string]$DirLevels))
        '__FANOUT__' = (Quote-Bash ([string]$DirsPerLevel))
        '__FILES_PER_DIR__' = (Quote-Bash ([string]$FilesPerDir))
        '__VOLUME_NAME__' = (Quote-Bash ("jfs-" + $prefix.Replace('/', '-')))
    }
    foreach ($key in $values.Keys) { $remote = $remote.Replace($key, [string]$values[$key]) }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($remote))
    $run = Invoke-AliyunJson @('ecs', 'RunCommand', '--region', $S3Region, '--Type', 'RunShellScript', '--InstanceId.1', $InstanceId, '--CommandContent', $encoded, '--ContentEncoding', 'Base64', '--Timeout', '172800', '--KeepCommand', 'false', '--Name', 'brewfs-juicefs-native')
    $invokeId = [string]$run.InvokeId
    if (-not $invokeId) { throw 'RunCommand 未返回 InvokeId' }
    $deadline = (Get-Date).AddHours(48)
    while ((Get-Date) -lt $deadline) {
        $result = Invoke-AliyunJson @('ecs', 'DescribeInvocationResults', '--region', $S3Region, '--InvokeId', $invokeId)
        $item = @($result.Invocation.InvocationResults.InvocationResult)[0]
        if ($item) {
            Write-Host "invocation status=$($item.InvocationStatus)"
            if ($item.InvocationStatus -in @('Success', 'Failed', 'Stopped', 'Error', 'Terminated')) {
                $decoded = if ($item.Output) { [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($item.Output)) } else { '' }
                Set-Content -LiteralPath (Join-Path $ArtifactDirectory 'remote-output.log') -Value $decoded -Encoding UTF8
                Write-Host $decoded
                if ($item.InvocationStatus -ne 'Success') { throw "远程 JuiceFS 测试失败: $($item.ErrorInfo)" }
                break
            }
        }
        Start-Sleep -Seconds 10
    }
}
finally {
    Remove-Item -LiteralPath $credentialPath -Force -ErrorAction SilentlyContinue
    if ($script:CredentialKey) {
        try { Invoke-Checked $Aliyun @('oss', 'rm', "oss://$S3Bucket/$script:CredentialKey", '--region', $S3Region, '--force') | Out-Null } catch { Write-Warning $_ }
    }
}
