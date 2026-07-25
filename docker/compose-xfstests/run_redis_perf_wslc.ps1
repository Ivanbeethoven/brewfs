[CmdletBinding()]
param(
    [string]$WslcCompose = $env:WSLC_COMPOSE,
    [string]$BrewfsBinaryDir,
    [string]$AptMirror = $env:BREWFS_APT_MIRROR,
    [ValidateSet("s3", "local-fs")]
    [string]$DataBackend = "s3",
    [string[]]$Tools = @(
        "fio-bigwrite",
        "fio-bigread",
        "fio-seqread",
        "fio-seqwrite",
        "fio-randread",
        "fio-randwrite",
        "fio-randrw"
    ),
    [string]$ArtifactsDir,
    [switch]$Keep
)

$ErrorActionPreference = "Stop"
$ScriptDir = $PSScriptRoot
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir "..\..")).Path
$ComposeFile = Join-Path $ScriptDir "wslc-brewfs-perf.yml"
$ProjectName = "brewfs-wslc-perf-{0}-{1}" -f (Get-Date -Format "yyyyMMddHHmmss"), $PID
$SupportedTools = @(
    "fio-bigwrite",
    "fio-bigread",
    "fio-seqread",
    "fio-seqwrite",
    "fio-randread",
    "fio-randwrite",
    "fio-randrw"
)
$Tools = @($Tools | ForEach-Object { $_ -split '[,\s]+' } | Where-Object { $_ })
if ($Tools.Count -eq 0) {
    throw "Tools must contain at least one fio profile"
}
$unknownTools = @($Tools | Where-Object { $_ -notin $SupportedTools })
if ($unknownTools.Count -ne 0) {
    throw "Unsupported fio profile(s): $($unknownTools -join ', '). Supported values: $($SupportedTools -join ', ')"
}
if (-not $ArtifactsDir) {
    $ArtifactsDir = Join-Path (Join-Path $ScriptDir "artifacts") $ProjectName
}
$ArtifactsDir = [System.IO.Path]::GetFullPath($ArtifactsDir)

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
$env:BREWFS_DATA_BACKEND = $DataBackend
$env:BREWFS_ARTIFACTS_DIR = $ArtifactsDir
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

function Start-WslcService {
    param([string]$Service)

    # Service creation is idempotent. Retry it because the WSLC SDK can
    # transiently drop an RPC response while it is bringing up a container.
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Invoke-WslcCompose -ComposeArgs @("up", "-d", $Service)
            return
        }
        catch {
            if ($attempt -eq 3) {
                throw
            }
            Write-Warning "wslc-compose could not start $Service (attempt $attempt of 3); retrying"
            Start-Sleep -Seconds 2
        }
    }
}

function Add-FioEnvironment {
    param([System.Collections.Generic.List[string]]$Arguments)

    Get-ChildItem Env: |
        Where-Object { $_.Name -like "PERF_FIO_*" } |
        Sort-Object Name |
        ForEach-Object {
            $Arguments.Add("-e")
            $Arguments.Add(("{0}={1}" -f @($_.Name, $_.Value)))
        }
}

function Read-FioReport {
    param([string]$Tool)

    $path = Join-Path $ArtifactsDir "$Tool.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing fio report for $Tool at $path"
    }
    $report = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    $jobs = @($report.jobs)
    if ($jobs.Count -eq 0) {
        throw "fio report for $Tool has no jobs"
    }
    $failedJobs = @($jobs | Where-Object { [int]$_.error -ne 0 })
    if ($failedJobs.Count -ne 0) {
        throw "fio profile $Tool reported an I/O error"
    }

    $readKiB = [double](($jobs | ForEach-Object { [double]$_.read.bw } | Measure-Object -Sum).Sum)
    $writeKiB = [double](($jobs | ForEach-Object { [double]$_.write.bw } | Measure-Object -Sum).Sum)
    $readIops = [double](($jobs | ForEach-Object { [double]$_.read.iops } | Measure-Object -Sum).Sum)
    $writeIops = [double](($jobs | ForEach-Object { [double]$_.write.iops } | Measure-Object -Sum).Sum)
    [pscustomobject]@{
        Profile           = $Tool
        ReadMiBPerSecond  = [math]::Round($readKiB / 1024, 2)
        ReadIops          = [math]::Round($readIops, 2)
        WriteMiBPerSecond = [math]::Round($writeKiB / 1024, 2)
        WriteIops         = [math]::Round($writeIops, 2)
        Artifact          = $path
    }
}

try {
    Write-Host "[wslc-compose] Starting project $ProjectName"
    # WSLC SDK service creation is reliable when dependencies are started one
    # at a time; a single multi-service up can lose its RPC response.
    Start-WslcService -Service redis
    Start-WslcService -Service rustfs
    Start-WslcService -Service perf

    $execArgs = [System.Collections.Generic.List[string]]@("exec")
    if ($AptMirror) {
        $execArgs.Add("-e")
        $execArgs.Add("BREWFS_APT_MIRROR=$AptMirror")
    }
    $execArgs.Add("-e")
    $execArgs.Add("PERF_TOOLS=$($Tools -join ' ')")
    Add-FioEnvironment -Arguments $execArgs
    $execArgs.Add("perf")
    $execArgs.Add("sh")
    $execArgs.Add("/wslc-tools/run_test.sh")
    Invoke-WslcCompose -ComposeArgs $execArgs.ToArray()

    $reports = @($Tools | ForEach-Object { Read-FioReport -Tool $_ })
    if ($DataBackend -eq "s3") {
        $objectPath = Join-Path $ArtifactsDir "rustfs-objects.json"
        if (-not (Test-Path -LiteralPath $objectPath -PathType Leaf)) {
            throw "Missing RustFS object report at $objectPath"
        }
        $objectReport = Get-Content -LiteralPath $objectPath -Raw | ConvertFrom-Json
        if (-not $objectReport.Contents -or $objectReport.Contents.Count -eq 0) {
            throw "fio completed but no data objects were found in RustFS"
        }
    }

    Write-Host "[wslc-compose] Completed fio profiles against $DataBackend"
    $reports | Format-Table Profile, ReadMiBPerSecond, ReadIops, WriteMiBPerSecond, WriteIops -AutoSize
    Write-Host "[wslc-compose] Artifacts: $ArtifactsDir"
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
