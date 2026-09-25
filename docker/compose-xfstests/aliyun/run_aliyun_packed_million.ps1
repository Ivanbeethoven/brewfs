[CmdletBinding()]
param(
    [ValidateSet('run', 'create', 'status', 'destroy')]
    [string]$Action = 'run',
    [string]$InstanceId,
    [string]$RegionId = 'cn-hangzhou',
    [string]$ZoneId = 'cn-hangzhou-h',
    [string]$ImageId = 'ubuntu_24_04_x64_20G_alibase_20260916.vhd',
    [string]$VSwitchId,
    [string]$SecurityGroupId,
    [string]$InstanceName,
    # ecs.u1-c1m4.2xlarge is the 32 GiB class used by this profile.
    [string]$InstanceType = 'ecs.u1-c1m4.2xlarge',
    [ValidateRange(40, 1000)]
    [int]$SystemDiskSizeGiB = 100,
    [int64]$SmallFileCount = 1000000,
    [int64]$SmallFileSizeBytes = 102400,
    [ValidateRange(1, 32)]
    [int]$DirLevels = 3,
    [int64]$DirsPerLevel = 10,
    [int64]$FilesPerLeaf = 1000,
    [int64]$FioFileSizeBytes = 67108864,
    [string]$PerfTools = 'packed-smallfiles packed-posix fio-seqread fio-randread',
    [int]$FioRuntimeSeconds = 20,
    [string]$PackedExistingManifestKey,
    [switch]$PackedSkipFixture,
    [ValidateSet('full', 'prefix')]
    [string]$ReadMode = 'full',
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
    [string]$Repository = 'https://github.com/brewfs/brewfs.git',
    [string]$Ref = 'main',
    [string]$AutoReleaseMinutes = '480',
    [switch]$KeepInstance,
    [switch]$NoCleanup,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$expected = [int64]1
for ($level = 0; $level -lt $DirLevels; $level++) {
    $expected = $expected * $DirsPerLevel
}
$expected = $expected * $FilesPerLeaf
if ($expected -ne $SmallFileCount) {
    throw "SmallFileCount must equal DirsPerLevel^DirLevels*FilesPerLeaf: expected $expected, got $SmallFileCount."
}
if ($SmallFileSizeBytes -le 0 -or $SmallFileSizeBytes -gt 4MB) {
    throw 'SmallFileSizeBytes must be between 1 and 4 MiB.'
}

$scriptPath = Join-Path $PSScriptRoot 'run_aliyun_perf.ps1'
if (-not (Test-Path -LiteralPath $scriptPath)) {
    throw "Missing shared Aliyun runner: $scriptPath"
}

$readBytes = if ($ReadMode -eq 'full') { '0' } else { '1' }
$runnerParams = @{
    Action = $Action
    RegionId = $RegionId
    ZoneId = $ZoneId
    ImageId = $ImageId
    InstanceType = $InstanceType
    SystemDiskSizeGiB = $SystemDiskSizeGiB
    Backend = 'none'
    DataBackend = 's3'
    VolumeFormat = 'packed-metadata-v1'
    PerfTools = $PerfTools
    FioRuntimeSeconds = $FioRuntimeSeconds
    PackedSmallFileCount = $SmallFileCount
    PackedSmallFileSizeBytes = $SmallFileSizeBytes
    PackedDirLevels = $DirLevels
    PackedDirsPerLevel = $DirsPerLevel
    PackedFilesPerDir = $FilesPerLeaf
    PackedFioFileSizeBytes = $FioFileSizeBytes
    PackedSmallFileReadBytes = $readBytes
    PackedExistingManifestKey = $PackedExistingManifestKey
    PackedSkipFixture = $PackedSkipFixture
    S3Bucket = $S3Bucket
    S3Endpoint = $S3Endpoint
    S3Region = $S3Region
    S3AccessKey = $S3AccessKey
    S3SecretKey = $S3SecretKey
    S3ForcePathStyle = $S3ForcePathStyle
    RepoRoot = $RepoRoot
    WslDistribution = $WslDistribution
    BinaryPath = $BinaryPath
    FixtureBinaryPath = $FixtureBinaryPath
    SkipBuild = $SkipBuild
    ArtifactDirectory = $ArtifactDirectory
    ObjectPrefix = $ObjectPrefix
    Repository = $Repository
    Ref = $Ref
    AutoReleaseMinutes = $AutoReleaseMinutes
    ColdRead = $true
}
$runnerArgs = @(
    '-Action', $Action,
    '-RegionId', $RegionId,
    '-ZoneId', $ZoneId,
    '-ImageId', $ImageId,
    '-InstanceType', $InstanceType,
    '-SystemDiskSizeGiB', [string]$SystemDiskSizeGiB,
    '-Backend', 'none',
    '-DataBackend', 's3',
    '-VolumeFormat', 'packed-metadata-v1',
    '-PerfTools', $PerfTools,
    '-FioRuntimeSeconds', [string]$FioRuntimeSeconds,
    '-PackedSmallFileCount', [string]$SmallFileCount,
    '-PackedSmallFileSizeBytes', [string]$SmallFileSizeBytes,
    '-PackedDirLevels', [string]$DirLevels,
    '-PackedDirsPerLevel', [string]$DirsPerLevel,
    '-PackedFilesPerDir', [string]$FilesPerLeaf,
    '-PackedSmallFileReadBytes', $readBytes,
    '-PackedExistingManifestKey', $PackedExistingManifestKey,
    '-PackedFioFileSizeBytes', [string]$FioFileSizeBytes,
    '-S3Region', $S3Region,
    '-Repository', $Repository,
    '-Ref', $Ref,
    '-AutoReleaseMinutes', $AutoReleaseMinutes,
    '-ColdRead'
)

foreach ($name in @('InstanceId', 'VSwitchId', 'SecurityGroupId', 'InstanceName')) {
    $value = Get-Variable -Name $name -ValueOnly
    if ($value) {
        $runnerParams[$name] = $value
        $runnerArgs += "-$name"
        $runnerArgs += [string]$value
    }
}
if ($S3Bucket) { $runnerArgs += '-S3Bucket'; $runnerArgs += $S3Bucket }
if ($S3Endpoint) { $runnerArgs += '-S3Endpoint'; $runnerArgs += $S3Endpoint }
if ($S3AccessKey) { $runnerArgs += '-S3AccessKey'; $runnerArgs += $S3AccessKey }
if ($S3SecretKey) { $runnerArgs += '-S3SecretKey'; $runnerArgs += $S3SecretKey }
if ($S3ForcePathStyle) { $runnerArgs += '-S3ForcePathStyle' }
if ($RepoRoot) { $runnerArgs += '-RepoRoot'; $runnerArgs += $RepoRoot }
if ($WslDistribution) { $runnerArgs += '-WslDistribution'; $runnerArgs += $WslDistribution }
if ($BinaryPath) { $runnerArgs += '-BinaryPath'; $runnerArgs += $BinaryPath }
if ($FixtureBinaryPath) { $runnerArgs += '-FixtureBinaryPath'; $runnerArgs += $FixtureBinaryPath }
if ($SkipBuild) { $runnerArgs += '-SkipBuild' }
if ($PackedSkipFixture) { $runnerArgs += '-PackedSkipFixture' }
if ($ArtifactDirectory) { $runnerArgs += '-ArtifactDirectory'; $runnerArgs += $ArtifactDirectory }
if ($ObjectPrefix) { $runnerArgs += '-ObjectPrefix'; $runnerArgs += $ObjectPrefix }
if ($KeepInstance) {
    $runnerParams.KeepInstance = $true
    $runnerArgs += '-KeepInstance'
}
if ($NoCleanup) {
    $runnerParams.NoCleanup = $true
    $runnerArgs += '-NoCleanup'
}

Write-Host 'Aliyun packed million-small-file profile'
Write-Host "  instance_type=$InstanceType"
Write-Host "  system_disk=${SystemDiskSizeGiB}GiB ESSD"
Write-Host "  image=$ImageId"
Write-Host "  files=$SmallFileCount file_size=$SmallFileSizeBytes bytes read_mode=$ReadMode"
Write-Host "  hierarchy=${DirLevels} levels x ${DirsPerLevel} dirs/level x ${FilesPerLeaf} files/leaf"
Write-Host '  data cache: disabled; cold-read/drop-caches checks: enabled'

if ($DryRun) {
    Write-Host 'Dry run: no Aliyun API call was made.'
    Write-Host ("powershell -File `"{0}`" {1}" -f $scriptPath, ($runnerArgs -join ' '))
    exit 0
}

& $scriptPath @runnerParams
exit $LASTEXITCODE
