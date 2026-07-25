[CmdletBinding()]
param(
    [string]$WslcCompose = $env:WSLC_COMPOSE,
    [string]$BrewfsBinaryDir,
    [string]$AptMirror = $env:BREWFS_APT_MIRROR,
    [switch]$Keep
)

$ErrorActionPreference = "Stop"
$ScriptDir = $PSScriptRoot
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir "..\..")).Path
$ComposeFile = Join-Path $ScriptDir "wslc-brewfs-perf.yml"
$ArtifactsDir = Join-Path $ScriptDir "artifacts"
$ProjectName = "brewfs-wslc-perf-{0}-{1}" -f (Get-Date -Format "yyyyMMddHHmmss"), $PID

if (-not $WslcCompose) {
    $command = Get-Command wslc-compose -ErrorAction SilentlyContinue
    if (-not $command) {
        throw "wslc-compose was not found; add it to PATH or set WSLC_COMPOSE"
    }
    $WslcCompose = $command.Source
}

if (-not $BrewfsBinaryDir) {
    $BrewfsBinaryDir = Join-Path $RepoRoot "target\docker"
}
$BrewfsBinaryDir = (Resolve-Path $BrewfsBinaryDir).Path
$BrewfsBinary = Join-Path $BrewfsBinaryDir "brewfs"
if (-not (Test-Path -LiteralPath $BrewfsBinary -PathType Leaf)) {
    throw "Linux brewfs binary not found at $BrewfsBinary"
}

New-Item -ItemType Directory -Path $ArtifactsDir -Force | Out-Null
$env:BREWFS_BINARY_DIR = $BrewfsBinaryDir
if (-not $env:WSLC_COMPOSE_SDK_TIMEOUT_SECS) {
    $env:WSLC_COMPOSE_SDK_TIMEOUT_SECS = "0"
}

function Invoke-WslcCompose {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$ComposeArgs)

    & $WslcCompose -f $ComposeFile -p $ProjectName @ComposeArgs
    if ($LASTEXITCODE -ne 0) {
        throw "wslc-compose failed with exit code $LASTEXITCODE"
    }
}

try {
    Write-Host "[wslc-compose] Starting project $ProjectName"
    Invoke-WslcCompose up -d

    $execArgs = @("exec")
    if ($AptMirror) {
        $execArgs += @("-e", "BREWFS_APT_MIRROR=$AptMirror")
    }
    $execArgs += @("perf", "sh", "/wslc-tools/run_test.sh")
    Invoke-WslcCompose @execArgs

    $writeReport = Get-Content (Join-Path $ArtifactsDir "fio-write.json") -Raw | ConvertFrom-Json
    $readReport = Get-Content (Join-Path $ArtifactsDir "fio-read.json") -Raw | ConvertFrom-Json
    $writeJob = $writeReport.jobs[0]
    $readJob = $readReport.jobs[0]
    if ($writeJob.error -ne 0 -or $readJob.error -ne 0) {
        throw "fio reported an I/O error"
    }

    [pscustomobject]@{
        WriteMiBPerSecond = [math]::Round($writeJob.write.bw / 1024, 2)
        WriteIops         = [math]::Round($writeJob.write.iops, 2)
        ReadMiBPerSecond  = [math]::Round($readJob.read.bw / 1024, 2)
        ReadIops          = [math]::Round($readJob.read.iops, 2)
        Artifacts         = $ArtifactsDir
    } | Format-List
}
finally {
    if ($Keep) {
        Write-Host "[wslc-compose] Keeping project $ProjectName"
    }
    else {
        Write-Host "[wslc-compose] Cleaning up project $ProjectName"
        & $WslcCompose -f $ComposeFile -p $ProjectName down --volumes --timeout 5
    }
}
