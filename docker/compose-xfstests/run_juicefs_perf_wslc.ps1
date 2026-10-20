[CmdletBinding()]
param(
    [string]$WslcCompose = $env:WSLC_COMPOSE,
    [string]$AptMirror = $env:JUICEFS_APT_MIRROR,
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
    [string]$WslcStateRoot = $env:WSLC_COMPOSE_STATE_ROOT,
    [switch]$Keep
)

$ErrorActionPreference = "Stop"
$ScriptDir = $PSScriptRoot
$ComposeFile = Join-Path $ScriptDir "wslc-juicefs-perf.yml"
$ProjectPrefix = "juicefs-wslc-perf-{0}-{1}" -f (Get-Date -Format "yyyyMMddHHmmss"), $PID
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
    throw "Unsupported fio profile(s): $($unknownTools -join ', ')"
}

if (-not $ArtifactsDir) {
    $ArtifactsDir = Join-Path (Join-Path $ScriptDir "artifacts") $ProjectPrefix
}
$ArtifactsDir = [System.IO.Path]::GetFullPath($ArtifactsDir)
if (-not $WslcStateRoot) {
    $WslcStateRoot = Join-Path "D:\wslc-compose-tests" $ProjectPrefix
}
$WslcStateRoot = [System.IO.Path]::GetFullPath($WslcStateRoot)

if (-not $WslcCompose) {
    $command = Get-Command wslc-compose -ErrorAction SilentlyContinue
    if (-not $command) {
        throw "wslc-compose was not found; add it to PATH or set WSLC_COMPOSE"
    }
    $WslcCompose = $command.Source
}

New-Item -ItemType Directory -Path $ArtifactsDir -Force | Out-Null
$env:JUICEFS_ARTIFACTS_DIR = $ArtifactsDir
$env:WSLC_COMPOSE_STATE_ROOT = $WslcStateRoot
if (-not $env:WSLC_COMPOSE_SDK_TIMEOUT_SECS) {
    $env:WSLC_COMPOSE_SDK_TIMEOUT_SECS = "0"
}

function Invoke-WslcCompose {
    param(
        [string]$ProjectName,
        [Parameter(ValueFromRemainingArguments = $true)][string[]]$ComposeArgs
    )

    & $WslcCompose -f $ComposeFile -p $ProjectName @ComposeArgs
    if ($LASTEXITCODE -ne 0) {
        throw "wslc-compose failed with exit code $LASTEXITCODE"
    }
}

function Start-WslcProject {
    param([string]$ProjectName)

    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Invoke-WslcCompose -ProjectName $ProjectName -ComposeArgs @("up", "-d", "perf")
            return
        }
        catch {
            if ($attempt -eq 3) {
                throw
            }
            Write-Warning "wslc-compose startup failed (attempt $attempt of 3); retrying"
            Start-Sleep -Seconds 2
        }
    }
}

function Add-TestEnvironment {
    param([System.Collections.Generic.List[string]]$Arguments)

    Get-ChildItem Env: |
        Where-Object { $_.Name -like "PERF_FIO_*" } |
        Sort-Object Name |
        ForEach-Object {
            $Arguments.Add("-e")
            $Arguments.Add(("{0}={1}" -f @($_.Name, $_.Value)))
        }
    if ($AptMirror) {
        $Arguments.Add("-e")
        $Arguments.Add("JUICEFS_APT_MIRROR=$AptMirror")
    }
    if ($env:JUICEFS_INSTALL_URL) {
        $Arguments.Add("-e")
        $Arguments.Add("JUICEFS_INSTALL_URL=$($env:JUICEFS_INSTALL_URL)")
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
        throw "fio report for $Tool contains no jobs"
    }
    if (@($jobs | Where-Object { [int]$_.error -ne 0 }).Count -ne 0) {
        throw "fio profile $Tool reported an I/O error"
    }
    $bytes = [uint64](($jobs | ForEach-Object {
        [uint64]$_.read.io_bytes + [uint64]$_.write.io_bytes
    } | Measure-Object -Sum).Sum)
    if ($bytes -eq 0) {
        throw "fio profile $Tool transferred no data"
    }
    $readKiB = [double](($jobs | ForEach-Object { [double]$_.read.bw } | Measure-Object -Sum).Sum)
    $writeKiB = [double](($jobs | ForEach-Object { [double]$_.write.bw } | Measure-Object -Sum).Sum)
    [pscustomobject]@{
        Profile           = $Tool
        ReadMiBPerSecond  = [math]::Round($readKiB / 1024, 2)
        WriteMiBPerSecond = [math]::Round($writeKiB / 1024, 2)
        TransferredMiB    = [math]::Round($bytes / 1MB, 2)
        Artifact          = $path
    }
}

function Save-RustfsReport {
    param([string]$Tool)

    $path = Join-Path $ArtifactsDir "rustfs-objects.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing RustFS object report for $Tool"
    }
    $report = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if (-not $report.Contents -or $report.Contents.Count -eq 0) {
        throw "fio completed but RustFS contains no JuiceFS objects for $Tool"
    }
    Copy-Item -LiteralPath $path -Destination (Join-Path $ArtifactsDir "$Tool-rustfs-objects.json") -Force
}

$reports = [System.Collections.Generic.List[object]]::new()
for ($index = 0; $index -lt $Tools.Count; $index++) {
    $tool = $Tools[$index]
    $ProjectName = "$ProjectPrefix-$($index + 1)"
    try {
        Write-Host "[wslc-compose] Starting JuiceFS project $ProjectName for $tool"
        Start-WslcProject -ProjectName $ProjectName
        $execArgs = [System.Collections.Generic.List[string]]@("exec", "-e", "PERF_TOOLS=$tool")
        Add-TestEnvironment -Arguments $execArgs
        $execArgs.Add("perf")
        $execArgs.Add("sh")
        $execArgs.Add("/juicefs-tools/run_test.sh")
        Invoke-WslcCompose -ProjectName $ProjectName -ComposeArgs $execArgs.ToArray()
        $reports.Add((Read-FioReport -Tool $tool))
        Save-RustfsReport -Tool $tool
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
}

Write-Host "[wslc-compose] Completed JuiceFS fio profiles"
$reports | Format-Table Profile, ReadMiBPerSecond, WriteMiBPerSecond, TransferredMiB -AutoSize
Write-Host "[wslc-compose] Artifacts: $ArtifactsDir"
