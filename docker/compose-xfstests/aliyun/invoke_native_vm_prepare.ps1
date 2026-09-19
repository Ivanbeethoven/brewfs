[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstanceId,
    [string]$RegionId = 'cn-hangzhou',
    [string]$RepositoryRoot = (Split-Path -Path (Split-Path -Path (Split-Path -Path $PSScriptRoot -Parent) -Parent) -Parent),
    [string]$PayloadDir = '/opt/brewfs-perf-image-prep',
    [string]$ResultVaultUrl = $env:BREWFS_RESULTS_URL,
    [string]$XfstestsArchivePath,
    [int]$ChunkSize = 6000,
    [int]$TimeoutMinutes = 120,
    [int]$PollSeconds = 30
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:Aliyun = $null

function Resolve-Executable([string]$Name, [string[]]$Candidates = @()) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw "找不到 $Name，请先安装并加入 PATH。"
}

$aliyunCandidates = @()
if ($env:LOCALAPPDATA) {
    $aliyunCandidates += (Join-Path $env:LOCALAPPDATA 'aliyun\aliyun.exe')
    $aliyunCandidates += (Join-Path $env:LOCALAPPDATA 'AliyunCLI\aliyun.exe')
}

function Invoke-Checked([string]$File, [string[]]$Arguments) {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $File
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    # The aliyun CLI writes UTF-8 JSON. Without an explicit encoding a child
    # process decodes its stdout with the console code page, which corrupts
    # non-ASCII fields (for example ECS "OSName" contains 位) and makes
    # ConvertFrom-Json fail with an unhelpful parse error.
    $startInfo.StandardOutputEncoding = [Text.Encoding]::UTF8
    $startInfo.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($argument in $Arguments) { [void]$startInfo.ArgumentList.Add([string]$argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) { throw "无法启动命令: $File" }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $output = @($stdout, $stderr) | Where-Object { $_ }
    if ($process.ExitCode -ne 0) {
        throw "命令失败: $File $($Arguments -join ' ')`n$($output -join [Environment]::NewLine)"
    }
    return ($output -join [Environment]::NewLine)
}

function Invoke-AliyunJson([string[]]$Arguments) {
    if (-not $script:Aliyun) { $script:Aliyun = Resolve-Executable 'aliyun' $aliyunCandidates }
    $output = Invoke-Checked $script:Aliyun $Arguments
    return ($output -join [Environment]::NewLine | ConvertFrom-Json)
}

function Invoke-AliyunJsonWithRetry([string[]]$Arguments, [int]$Attempts = 5) {
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        try {
            return Invoke-AliyunJson $Arguments
        } catch {
            $message = $_.Exception.Message
            if ($attempt -eq $Attempts -or $message -notmatch 'timeout|timed out|EOF|dial tcp|connection reset|temporarily unavailable') {
                throw
            }
            Write-Warning "Aliyun API transient failure; retrying ($attempt/$Attempts): $message"
            Start-Sleep -Seconds ([Math]::Min(30, $attempt * 5))
        }
    }
    throw 'Aliyun API retry loop exhausted.'
}

function Invoke-RemoteScript([string]$Script, [int]$TimeoutSeconds = 300, [int]$Attempts = 4) {
    $content = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Script -replace "`r`n", "`n")))
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        $run = Invoke-AliyunJsonWithRetry @(
            'ecs', 'RunCommand', '--region', $RegionId, '--Type', 'RunShellScript',
            '--InstanceId.1', $InstanceId, '--CommandContent', $content,
            '--ContentEncoding', 'Base64', '--Timeout', $TimeoutSeconds.ToString(),
            '--KeepCommand', 'false', '--Name', 'brewfs-native-vm-prepare'
        )
        $invokeId = $run.InvokeId
        if (-not $invokeId) { throw 'RunCommand 未返回 InvokeId。' }
        $deadline = (Get-Date).AddSeconds($TimeoutSeconds + 120)
        while ((Get-Date) -lt $deadline) {
            $result = Invoke-AliyunJsonWithRetry @('ecs', 'DescribeInvocationResults', '--region', $RegionId, '--InvokeId', $invokeId)
            $item = @($result.Invocation.InvocationResults.InvocationResult)[0]
            if ($item) {
                $status = [string]$item.InvocationStatus
                if ($status -in @('Success', 'Failed', 'Stopped', 'Error', 'Terminated', 'Aborted')) {
                    $output = ''
                    if ($item.Output) {
                        $output = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([string]$item.Output))
                    }
                    if ($status -eq 'Success') { return $output }
                    $restarted = ($status -eq 'Terminated' -and
                        ([string]$item.ErrorCode -eq 'ClientRestarted' -or [string]$item.ErrorInfo -match 'has been restarted'))
                    if ($restarted -and $attempt -lt $Attempts) {
                        Write-Warning "Cloud Assistant restarted during remote script; retrying ($attempt/$Attempts)."
                        Start-Sleep -Seconds 20
                        break
                    }
                    if ($restarted) { throw 'Cloud Assistant 反复重启，远程脚本无法完成。' }
                    throw "远程脚本失败: status=$status error=$($item.ErrorInfo)`n$output"
                }
            }
            Start-Sleep -Seconds 5
        }
    }
    throw '远程脚本重试次数耗尽。'
}

# Windows checkouts with core.autocrlf=true hand these files CRLF text. The
# payload is consumed by bash on the Linux VM, where a stray CR turns the first
# line into `$'\r': command not found`. Stage every harness file as LF so the
# image build does not depend on the line endings this checkout happens to have.
function Copy-LfFile {
    param([string]$Source, [string]$Destination)
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "payload source is missing: $Source"
    }
    $text = ([IO.File]::ReadAllText($Source) -replace "`r`n", "`n") -replace "`r", "`n"
    [IO.File]::WriteAllText($Destination, $text, (New-Object Text.UTF8Encoding($false)))
}

function New-PreparePayload {
    param([string]$PrepareEnv)

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $staging = Join-Path ([IO.Path]::GetTempPath()) "brewfs-native-vm-$([Guid]::NewGuid().ToString('N'))"
    $harness = Join-Path $staging 'harness'
    $setup = Join-Path $staging 'setup'
    New-Item -ItemType Directory -Path $harness, $setup -Force | Out-Null

    $composeDir = Join-Path $RepositoryRoot 'docker/compose-xfstests'
    $aliyunDir = Join-Path $composeDir 'aliyun'
    Copy-LfFile -Source (Join-Path $aliyunDir 'prepare_native_perf_vm.sh') -Destination (Join-Path $setup 'prepare_native_perf_vm.sh')
    foreach ($name in @('run_perf_in_container.sh', 'run_juicefs_perf_in_container.sh', 'perf_metadata_fallback.py')) {
        Copy-LfFile -Source (Join-Path $composeDir $name) -Destination (Join-Path $harness $name)
    }
    Copy-LfFile -Source (Join-Path $aliyunDir 'run_native_perf.sh') -Destination (Join-Path $harness 'run_native_perf.sh')
    Copy-LfFile -Source (Join-Path $RepositoryRoot 'tools/perf/perf_manifest.py') -Destination (Join-Path $harness 'perf_manifest.py')
    if ($PrepareEnv) {
        # Sourced by bash: the file must not contain CR characters, otherwise
        # the value keeps a trailing CR and curl rejects the URL.
        $envText = ($PrepareEnv -replace "`r`n", "`n" -replace "`r", "`n").TrimEnd() + "`n"
        [IO.File]::WriteAllText((Join-Path $setup 'prepare.env'), $envText, (New-Object Text.UTF8Encoding($false)))
    }

    $archive = Join-Path ([IO.Path]::GetTempPath()) "brewfs-native-vm-$([Guid]::NewGuid().ToString('N')).tar.gz"
    $tar = Resolve-Executable 'tar.exe'
    Invoke-Checked $tar @('-czf', $archive, '-C', $staging, '.') | Out-Null
    Remove-Item -LiteralPath $staging -Recurse -Force
    return $archive
}

function New-VaultArchiveUpload([string]$FilePath) {
    if (-not $ResultVaultUrl) { throw '上传测试依赖需要 -ResultVaultUrl 或 BREWFS_RESULTS_URL。' }
    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) { throw "找不到待上传文件: $FilePath" }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipPath = Join-Path ([IO.Path]::GetTempPath()) "brewfs-dependency-$([Guid]::NewGuid().ToString('N')).zip"
    $zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $zip.CreateEntry('xfstests-prebuilt.tar.gz', [IO.Compression.CompressionLevel]::NoCompression)
        $source = [IO.File]::OpenRead($FilePath)
        $destination = $entry.Open()
        try { $source.CopyTo($destination) } finally { $destination.Dispose(); $source.Dispose() }
    } finally {
        $zip.Dispose()
    }
    $curl = Resolve-Executable 'curl.exe'
    $response = Invoke-Checked $curl @(
        '--fail-with-body', '--silent', '--show-error', '--location',
        '-F', "archive=@$zipPath;filename=xfstests-prebuilt.zip",
        "$($ResultVaultUrl.TrimEnd('/'))/api/runs"
    )
    Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
    $run = ($response -join [Environment]::NewLine | ConvertFrom-Json)
    if (-not $run.id) { throw 'Result Vault 依赖上传未返回 run id。' }
    return [pscustomobject]@{
        Id  = [string]$run.id
        Url = "$($ResultVaultUrl.TrimEnd('/'))/api/runs/$($run.id)/files/xfstests-prebuilt.tar.gz"
    }
}

function Remove-VaultRun([string]$RunId) {
    if (-not $RunId -or -not $ResultVaultUrl) { return }
    try {
        $curl = Resolve-Executable 'curl.exe'
        Invoke-Checked $curl @('--fail-with-body', '--silent', '--show-error', '--location', '-X', 'DELETE',
            "$($ResultVaultUrl.TrimEnd('/'))/api/runs/$RunId") | Out-Null
        Write-Host "Result Vault 临时依赖记录已删除: $RunId"
    } catch {
        Write-Warning "Result Vault 临时依赖清理失败，请手动删除 run ${RunId}: $($_.Exception.Message)"
    }
}

if (-not $XfstestsArchivePath) {
    $XfstestsArchivePath = Join-Path $RepositoryRoot 'tests/scripts/xfstests-prebuilt/xfstests-prebuilt.tar.gz'
}

$dependencyRunId = $null
$prepareEnv = $null
if (Test-Path -LiteralPath $XfstestsArchivePath -PathType Leaf) {
    $archiveSize = (Get-Item -LiteralPath $XfstestsArchivePath).Length
    if ($archiveSize -gt 1024) {
        $upload = New-VaultArchiveUpload -FilePath $XfstestsArchivePath
        $dependencyRunId = $upload.Id
        $prepareEnv = "XFSTESTS_ARCHIVE_URL='$($upload.Url)'"
        Write-Host "xfstests prebuilt archive uploaded to Result Vault: run=$($upload.Id) size=$archiveSize"
    }
}

$archive = New-PreparePayload -PrepareEnv $prepareEnv
try {
    $bytes = [IO.File]::ReadAllBytes($archive)
    $base64 = [Convert]::ToBase64String($bytes)
    $chunks = [Math]::Ceiling($base64.Length / $ChunkSize)
    Write-Host "payload: $([Math]::Round($bytes.Length / 1KB, 1)) KiB, base64 $($base64.Length) chars, $chunks chunk(s)"

    for ($index = 0; $index -lt $chunks; $index++) {
        $start = $index * $ChunkSize
        $length = [Math]::Min($ChunkSize, $base64.Length - $start)
        $chunk = $base64.Substring($start, $length)
        $redirect = if ($index -eq 0) { '>' } else { '>>' }
        $script = "mkdir -p /tmp && printf '%s' '$chunk' $redirect /tmp/brewfs-image-prepare.b64"
        Invoke-RemoteScript -Script $script | Out-Null
        Write-Host "  uploaded chunk $($index + 1)/$chunks"
    }

    $launcher = @"
set -e
rm -rf '$PayloadDir'
mkdir -p '$PayloadDir'
base64 -d /tmp/brewfs-image-prepare.b64 > /tmp/brewfs-image-prepare.tar.gz
tar -xzf /tmp/brewfs-image-prepare.tar.gz -C '$PayloadDir'
rm -f /tmp/brewfs-image-prepare.b64 /tmp/brewfs-image-prepare.tar.gz
chmod 0755 '$PayloadDir/setup/prepare_native_perf_vm.sh'
setsid --fork nohup bash '$PayloadDir/setup/prepare_native_perf_vm.sh' '$PayloadDir' \
  >/var/log/brewfs-image-prepare.log 2>&1 </dev/null
sleep 2
echo LAUNCHED
"@
    $launchOutput = Invoke-RemoteScript -Script $launcher
    Write-Host ($launchOutput.Trim())

    $deadline = (Get-Date).AddMinutes($TimeoutMinutes)
    $lastStatus = ''
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds $PollSeconds
        $poll = Invoke-RemoteScript -Script @"
status="`$(cat /opt/brewfs-perf/.prepare-status 2>/dev/null || echo PENDING)"
printf 'STATUS=%s\n' "`$status"
if ! pgrep -f 'prepare_native_perf_vm.sh' >/dev/null 2>&1; then
    echo SCRIPT_GONE
fi
tail -n 6 /var/log/brewfs-image-prepare.log 2>/dev/null || true
"@
        $statusLine = ($poll -split "`n" | Where-Object { $_ -match '^STATUS=' } | Select-Object -First 1)
        $status = if ($statusLine) { $statusLine.Substring(7).Trim() } else { 'UNKNOWN' }
        if ($status -ne $lastStatus) {
            Write-Host "prepare status: $status"
            $lastStatus = $status
        }
        if ($status -eq 'READY') {
            Write-Host ($poll -split "`n" | Where-Object { $_ -notmatch '^STATUS=' } | Select-Object -Last 6 | Out-String)
            Write-Host "VM preparation finished: $InstanceId is ready for manual imaging."
            exit 0
        }
        if ($status -like 'FAILED*') {
            Write-Error "VM preparation failed: $poll"
            exit 1
        }
        if ($status -eq 'RUNNING' -and $poll -match 'SCRIPT_GONE') {
            throw "VM preparation process disappeared while status was RUNNING; inspect /var/log/brewfs-image-prepare.log on $InstanceId."
        }
    }
    throw "等待 VM 准备完成超时（$TimeoutMinutes 分钟）。"
} finally {
    Remove-Item -LiteralPath $archive -Force -ErrorAction SilentlyContinue
    Remove-VaultRun $dependencyRunId
}
