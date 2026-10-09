[CmdletBinding()]
param(
    [string]$SshHost = "brewfs-aliyun-dev",
    [string]$RemoteRepo = "/opt/brewfs",
    [string]$Repository = "https://github.com/Ivanbeethoven/brewfs.git",
    [string]$Branch = "codex/packed-metadata-aliyun-20260930",
    [switch]$SkipGitHub,
    [switch]$SkipAliyun,
    [switch]$SkipCodex,
    [switch]$SkipClone
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-Remote {
    param([Parameter(Mandatory)][string]$Command)

    & ssh -o BatchMode=yes $SshHost $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Remote command failed with exit code $LASTEXITCODE."
    }
}

function Send-RemoteText {
    param(
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$Command
    )

    $Text | & ssh -o BatchMode=yes $SshHost $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Remote transfer failed with exit code $LASTEXITCODE."
    }
}

function Assert-SafeValue {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Value,
        [Parameter(Mandatory)][string]$Pattern
    )

    if ($Value -notmatch $Pattern) {
        throw "$Name contains characters that are not allowed by this helper."
    }
}

Assert-SafeValue "RemoteRepo" $RemoteRepo "^[A-Za-z0-9._/-]+$"
Assert-SafeValue "Branch" $Branch "^[A-Za-z0-9._/-]+$"
Assert-SafeValue "Repository" $Repository "^https://github\.com/[A-Za-z0-9._/-]+\.git$"

if (-not (Get-Command ssh -ErrorAction SilentlyContinue)) {
    throw "ssh is required on PATH."
}

Invoke-Remote "umask 077; command -v git >/dev/null; command -v gh >/dev/null 2>/dev/null || true; command -v aliyun >/dev/null 2>/dev/null || true"

if (-not $SkipClone) {
    $cloneCommand = "set -eu; if [ -d '$RemoteRepo/.git' ]; then git -C '$RemoteRepo' status --short --branch; elif [ -e '$RemoteRepo' ]; then echo 'Refusing to reuse a non-Git path.' >&2; exit 2; else git clone --branch '$Branch' --single-branch '$Repository' '$RemoteRepo'; fi"
    Invoke-Remote $cloneCommand
}

if (-not $SkipGitHub) {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        throw "gh is required for GitHub credential transfer."
    }

    $githubToken = (& gh auth token).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($githubToken)) {
        throw "gh auth token did not return a token."
    }

    Send-RemoteText $githubToken "umask 077; gh auth login --with-token"
}

if (-not $SkipAliyun) {
    $aliyunConfigPath = Join-Path $env:USERPROFILE ".aliyun\config.json"
    if (-not (Test-Path -LiteralPath $aliyunConfigPath -PathType Leaf)) {
        throw "Aliyun CLI config was not found at $aliyunConfigPath."
    }

    $aliyunConfig = Get-Content -LiteralPath $aliyunConfigPath -Raw
    Send-RemoteText $aliyunConfig "umask 077; mkdir -p ~/.aliyun; cat > ~/.aliyun/config.json; chmod 600 ~/.aliyun/config.json"
}

if (-not $SkipCodex) {
    $codexConfigPath = Join-Path $env:USERPROFILE ".codex\config.toml"
    if (-not (Test-Path -LiteralPath $codexConfigPath -PathType Leaf)) {
        throw "Codex config was not found at $codexConfigPath."
    }

    $localCodexConfig = Get-Content -LiteralPath $codexConfigPath -Raw
    $baseUrlMatch = [regex]::Match($localCodexConfig, '(?m)^\s*base_url\s*=\s*"([^"]+)"')
    $tokenMatch = [regex]::Match($localCodexConfig, '(?m)^\s*experimental_bearer_token\s*=\s*"([^"]+)"')
    if (-not $baseUrlMatch.Success -or -not $tokenMatch.Success) {
        throw "The local Codex config does not contain the expected custom provider fields."
    }

    $baseUrl = $baseUrlMatch.Groups[1].Value
    $bearerToken = $tokenMatch.Groups[1].Value
    $tomlBaseUrl = $baseUrl.Replace('\', '\\').Replace('"', '\"')
    $tomlToken = $bearerToken.Replace('\', '\\').Replace('"', '\"')
    $remoteCodexConfig = @"
model_provider = "custom"
model = "gpt-6-sol"
model_reasoning_effort = "high"
disable_response_storage = true

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = false
base_url = "$tomlBaseUrl"
experimental_bearer_token = "$tomlToken"

[projects.'$RemoteRepo']
trust_level = "trusted"
"@

    Send-RemoteText $remoteCodexConfig "umask 077; mkdir -p ~/.codex; cat > ~/.codex/config.toml; chmod 600 ~/.codex/config.toml"
}

$finalCommand = 'printf ''migration_ready repo=%s\n'' ''' + $RemoteRepo + ''''
Invoke-Remote $finalCommand
