[CmdletBinding()]
param(
    [string]$ResultVaultUrl = $env:BREWFS_RESULTS_URL,
    # Comma or space separated run names/ids; each may be a substring of the
    # vault run name. A single string keeps `pwsh -File ... -Run a,b` working,
    # since that invocation cannot bind a real array.
    #
    # The parameter deliberately is not called $Run: PowerShell variable names
    # are case-insensitive, so a [string]$Run parameter would also constrain the
    # $run loop variable below and silently coerce every run object to a string,
    # which printed an all "-" table.
    [Parameter(Mandatory = $true, Position = 0)]
    [Alias('Run')]
    [string]$RunSpec,
    [string]$Json
)

# Compare metrics of two or more Result Vault runs side by side.
#
# The vault stores every tool metric per run, but the UI shows them per run, so
# comparing an 8 GiB round against a 16 GiB round by hand means clicking back and
# forth. This prints one row per tool with one column per run.
$ErrorActionPreference = 'Stop'

$runs = (curl.exe -s "$($ResultVaultUrl.TrimEnd('/'))/api/runs" | ConvertFrom-Json)

$needles = $RunSpec -split '[,\s]+' | Where-Object { $_ }
$selected = foreach ($needle in $needles) {
    $match = @($runs | Where-Object { $_.id -like "*$needle*" -or $_.name -like "*$needle*" })
    if ($match.Count -eq 0) { throw "未找到匹配 '$needle' 的 run。" }
    if ($match.Count -gt 1) {
        throw "匹配 '$needle' 的 run 有 $($match.Count) 条，请给出更精确的名字：$($match.name -join ', ')"
    }
    $match[0]
}

if ($Json) {
    $selected | Select-Object id, name, status, fileCount, totalBytes, metrics, environment |
        ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $Json
    Write-Host "已写出原始指标: $Json"
}

$tools = $selected | ForEach-Object { $_.metrics.tool } | Sort-Object -Unique
$columns = @($selected | ForEach-Object { $_.name })

Write-Host ''
Write-Host ("{0,-14} {1}" -f 'tool', (($columns | ForEach-Object { "{0,26}" -f $_ }) -join ' '))
foreach ($tool in $tools) {
    $cells = foreach ($run in $selected) {
        $metric = @($run.metrics | Where-Object { $_.tool -eq $tool })[0]
        if (-not $metric) { '{0,26}' -f '-'; continue }
        $read = [double]$metric.readMiBps
        $write = [double]$metric.writeMiBps
        $value = if ($read -gt 0 -and $write -gt 0) {
            "$([Math]::Round($read,1))r/$([Math]::Round($write,1))w"
        } elseif ($read -gt 0) {
            [string][Math]::Round($read, 1)
        } elseif ($write -gt 0) {
            [string][Math]::Round($write, 1)
        } else {
            [string]$metric.status
        }
        '{0,26}' -f $value
    }
    Write-Host ("{0,-14} {1}" -f $tool, ($cells -join ' '))
}

$p99 = foreach ($run in $selected) {
    $path = @($run.metrics | Where-Object { $_.tool -eq 'fio-randrw' })[0]
    if ($path) { "{0,26}" -f "@$([Math]::Round([double]$path.readP99Ms,1))ms" } else { '{0,26}' -f '-' }
}
Write-Host ("{0,-14} {1}" -f 'randrw-p99', ($p99 -join ' '))

Write-Host ''
foreach ($run in $selected) {
    Write-Host ("{0,-30} {1}  status={2}  files={3}" -f $run.name, $run.id, $run.status, $run.fileCount)
}
