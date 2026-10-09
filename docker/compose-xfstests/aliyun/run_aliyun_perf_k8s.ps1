[CmdletBinding()]
param(
    [ValidateSet('test', 'status', 'export', 'destroy')]
    [string]$Action = 'test',
    [string]$KubeconfigPath,
    [string]$Namespace = 'brewfs-perf',
    [string]$RegistryImage = 'ghcr.io/ivanbeethoven/brewfs-perf',
    [string]$ImageTag = ('aliyun-{0}' -f (Get-Date -Format 'yyyyMMdd-HHmmss')),
    [string]$GhcrUsername = 'Ivanbeethoven',
    [string]$GhcrToken,
    [ValidateSet('brewfs', 'juicefs')]
    [string]$Workload = 'brewfs',
    [ValidateSet('redis', 'tikv')]
    [string]$Backend = 'redis',
    [ValidateSet('managed', 'embedded')]
    [string]$ServiceMode = 'managed',
    [ValidateSet('s3', 'local-fs')]
    [string]$DataBackend = 's3',
    [string]$MetaUrl,
    [string]$S3Endpoint,
    [string]$S3Bucket = 'brewfs-data',
    [string]$S3Region = 'us-east-1',
    [bool]$S3ForcePathStyle = $false,
    [string]$S3AccessKey = $env:AWS_ACCESS_KEY_ID,
    [string]$S3SecretKey = $env:AWS_SECRET_ACCESS_KEY,
    [string]$PerfTools = 'fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw dirstress dirperf metaperf looptest',
    [switch]$SkipImageBuild,
    [switch]$KeepJob,
    [Alias('JobName')]
    [string]$ExistingJobName,
    [string]$ArtifactDirectory,
    [string]$ArchivePath,
    [string]$ResultVaultUrl = $env:BREWFS_RESULTS_URL,
    [ValidateRange(30, 86400)]
    [int]$ArtifactHoldSeconds = 900,
    [ValidateRange(1, 1440)]
    [int]$ExportTimeoutMinutes = 10
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$Image = "$RegistryImage`:$ImageTag"
$Dockerfile = if ($Workload -eq 'juicefs') { 'docker/compose-xfstests/Dockerfile.juicefs-perf' } else { 'docker/compose-xfstests/aliyun/Dockerfile.perf-local' }
$JobName = if ($ExistingJobName) { $ExistingJobName } else { "$Workload-perf-$ImageTag".ToLowerInvariant() }
$ManagedByLabel = 'app.kubernetes.io/managed-by=brewfs-perf-runner'
$script:TestResourcesApplied = $false

function Resolve-Tool([string]$Name, [string[]]$Candidates = @()) {
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    throw "找不到 $Name，请先安装并加入 PATH。"
}
function Invoke-Checked([string]$File, [string[]]$Arguments) {
    $out = & $File @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "$File 失败:`n$($out -join [Environment]::NewLine)" }
    return $out
}

function Invoke-KubectlYaml([object[]]$Yaml) {
    $output = $Yaml | & $Kubectl @('apply', '-f', '-') 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "kubectl apply 失败:`n$($output -join [Environment]::NewLine)"
    }
    return $output
}

$Kubectl = Resolve-Tool 'kubectl'
if ($KubeconfigPath) { $env:KUBECONFIG = (Resolve-Path $KubeconfigPath).Path }

function Get-ArtifactRoot {
    if ($ArtifactDirectory) {
        if ([IO.Path]::IsPathRooted($ArtifactDirectory)) {
            return [IO.Path]::GetFullPath($ArtifactDirectory)
        }
        return [IO.Path]::GetFullPath((Join-Path $RepoRoot $ArtifactDirectory))
    }
    return (Join-Path $RepoRoot 'docker\compose-xfstests\artifacts')
}

function Get-PerfPod {
    $pod = ((Invoke-Checked $Kubectl @('get', 'pod', '-n', $Namespace, '-l', "job-name=$JobName", '-o', 'jsonpath={.items[0].metadata.name}') -join '')).Trim()
    if (-not $pod) { throw "找不到 Job $JobName 对应的 Pod。" }
    return $pod
}

function Export-Artifacts {
    param([Parameter(Mandatory = $true)][string]$Pod)

    $tar = Resolve-Tool 'tar' @('C:\Windows\System32\tar.exe')
    $root = Get-ArtifactRoot
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $target = Join-Path $root $JobName
    if (Test-Path -LiteralPath $target) {
        $target = Join-Path $root ("{0}-export-{1}" -f $JobName, (Get-Date -Format 'yyyyMMdd-HHmmss'))
    }
    $stage = Join-Path $root ('.staging-{0}-{1}' -f $JobName, [guid]::NewGuid().ToString('N'))
    $localArchive = Join-Path $root ('.{0}-{1}.tar.gz' -f $JobName, [guid]::NewGuid().ToString('N'))
    $remoteArchive = "/tmp/$JobName-artifacts.tar.gz"
    $remoteSnapshot = "/tmp/$JobName-artifacts-snapshot"
    try {
        New-Item -ItemType Directory -Force -Path $stage | Out-Null
        # Snapshot first so an actively appended brewfs.log cannot make tar exit
        # with "file changed as we read it" during the artifact hold window.
        Invoke-Checked $Kubectl @('exec', '-n', $Namespace, $Pod, '-c', 'perf', '--', 'sh', '-ec', "rm -rf '$remoteSnapshot'; cp -a '/artifacts/$JobName' '$remoteSnapshot'; tar -czf '$remoteArchive' -C /tmp '$(Split-Path -Leaf $remoteSnapshot)'") | Out-Host
        # kubectl treats an absolute Windows destination (for example C:\...) as
        # another remote specification because of the drive-letter colon. Run
        # the copy from the artifact root and pass a relative local path.
        Push-Location -LiteralPath $root
        try {
            $relativeArchive = Join-Path '.' (Split-Path -Leaf $localArchive)
            Invoke-Checked $Kubectl @('cp', '--retries=5', "$Namespace/$Pod`:$remoteArchive", $relativeArchive) | Out-Host
        } finally {
            Pop-Location
        }
        Invoke-Checked $tar @('-xzf', $localArchive, '-C', $stage) | Out-Host
        $stagedTarget = Join-Path $stage (Split-Path -Leaf $remoteSnapshot)
        if (-not (Test-Path -LiteralPath $stagedTarget)) { throw "导出的归档中缺少 $JobName。" }
        Move-Item -LiteralPath $stagedTarget -Destination $target
        $podInfo = ((Invoke-Checked $Kubectl @('get', 'pod', $Pod, '-n', $Namespace, '-o', 'json')) -join [Environment]::NewLine) | ConvertFrom-Json
        $nodeInfo = ((Invoke-Checked $Kubectl @('get', 'node', $podInfo.spec.nodeName, '-o', 'json')) -join [Environment]::NewLine) | ConvertFrom-Json
        $runMetadata = [ordered]@{
            jobName = $JobName
            namespace = $Namespace
            image = $Image
            nodeName = $podInfo.spec.nodeName
            instanceType = $nodeInfo.metadata.labels.'node.kubernetes.io/instance-type'
            cpuCapacity = $nodeInfo.status.capacity.cpu
            memoryCapacity = $nodeInfo.status.capacity.memory
            ephemeralStorageCapacity = $nodeInfo.status.capacity.'ephemeral-storage'
            osImage = $nodeInfo.status.nodeInfo.osImage
            kernelVersion = $nodeInfo.status.nodeInfo.kernelVersion
            kubeletVersion = $nodeInfo.status.nodeInfo.kubeletVersion
            startedAt = $podInfo.status.startTime
            completedAt = [DateTime]::UtcNow.ToString('o')
            perfTools = @($PerfTools -split '\s+' | Where-Object { $_ })
        }
        $runMetadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $target 'run-metadata.json') -Encoding utf8
        $archive = if ($ArchivePath) {
            if ([IO.Path]::IsPathRooted($ArchivePath)) { [IO.Path]::GetFullPath($ArchivePath) }
            else { [IO.Path]::GetFullPath((Join-Path $RepoRoot $ArchivePath)) }
        } else { "$target.zip" }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $archive) | Out-Null
        $archiveInputs = Get-ChildItem -LiteralPath $target -Force | Select-Object -ExpandProperty FullName
        Compress-Archive -Path $archiveInputs -DestinationPath $archive -Force
        return [pscustomobject]@{ Directory = $target; Archive = $archive }
    } finally {
        Remove-Item -LiteralPath $stage, $localArchive -Force -Recurse -ErrorAction SilentlyContinue
        # Best effort: the pod may already have exited when this cleanup runs.
        & $Kubectl @('exec', '-n', $Namespace, $Pod, '-c', 'perf', '--', 'sh', '-ec', "rm -rf '$remoteArchive' '$remoteSnapshot'") 2>$null | Out-Null
    }
}

function Upload-ResultVault {
    param([Parameter(Mandatory = $true)][string]$Archive)
    if (-not $ResultVaultUrl) { throw '必须设置 -ResultVaultUrl 或 BREWFS_RESULTS_URL，才能保证结果上传。' }
    $baseUrl = $ResultVaultUrl.Trim().TrimEnd('/')
    $parsedUrl = $null
    if (-not [Uri]::TryCreate($baseUrl, [UriKind]::Absolute, [ref]$parsedUrl) -or
        $parsedUrl.Scheme -notin @('http', 'https')) {
        throw "无效的 BREWFS_RESULTS_URL/ResultVaultUrl: $ResultVaultUrl"
    }
    $endpoint = "$baseUrl/api/runs"
    try {
        $response = Invoke-RestMethod -Uri $endpoint -Method Post -Form @{ archive = Get-Item -LiteralPath $Archive }
        Write-Host "Result Vault 跑次: $($response.id)"
        Write-Host "Result Vault URL: $baseUrl"
    } catch {
        throw "上传 Result Vault 失败（本地归档仍已保留）：$($_.Exception.Message)"
    }
}

function Ensure-Image {
    $docker = Resolve-Tool 'docker' @('C:\Program Files\Docker\Docker\resources\bin\docker.exe')
    Push-Location $RepoRoot
    try {
        Invoke-Checked $docker @('build', '-f', $Dockerfile, '-t', $Image, '.') | Out-Host
        if ($GhcrToken) {
            $GhcrToken | & $docker login ghcr.io --username $GhcrUsername --password-stdin 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) { throw 'docker login ghcr.io 失败。' }
        }
        Invoke-Checked $docker @('push', $Image) | Out-Host
    } finally { Pop-Location }
}

function Render-Manifests {
    $dataEnv = if ($DataBackend -eq 's3') { 's3' } else { 'local-fs' }
    $initContainers = if ($DataBackend -eq 's3') {
        @"
      initContainers:
      - name: rustfs-init
        image: docker.m.daocloud.io/amazon/aws-cli:latest
        command: ["sh", "-ec"]
        args:
        - |
          mkdir -p /root/.aws
          printf '[default]\nregion = us-east-1\ns3 =\n  addressing_style = path\n' > /root/.aws/config
          until aws --endpoint-url http://rustfs:9000 s3api create-bucket --bucket brewfs-data >/dev/null 2>&1 || aws --endpoint-url http://rustfs:9000 s3api head-bucket --bucket brewfs-data >/dev/null 2>&1; do
            sleep 2
          done
        env:
        - { name: AWS_ACCESS_KEY_ID, value: rustfsadmin }
        - { name: AWS_SECRET_ACCESS_KEY, value: rustfsadmin }
        - { name: AWS_DEFAULT_REGION, value: us-east-1 }
        - { name: AWS_EC2_METADATA_DISABLED, value: "true" }
"@
    } else { '' }
    $storage = if ($DataBackend -eq 's3') {
        @"
apiVersion: apps/v1
kind: Deployment
metadata: { name: rustfs, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  selector: { matchLabels: { app: rustfs } }
  template:
    metadata: { labels: { app: rustfs } }
    spec:
      containers:
      - name: rustfs
        image: docker.m.daocloud.io/rustfs/rustfs:latest
        args: ["--address", ":9000", "--console-enable", "--server-domains", "rustfs", "--access-key", "rustfsadmin", "--secret-key", "rustfsadmin", "/data"]
        readinessProbe: { tcpSocket: { port: 9000 }, initialDelaySeconds: 5 }
---
apiVersion: v1
kind: Service
metadata: { name: rustfs, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec: { selector: { app: rustfs }, ports: [{ port: 9000 }] }
"@
    } else { '' }
    $meta = if ($Backend -eq 'redis') {
        @"
apiVersion: apps/v1
kind: Deployment
metadata: { name: redis, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  selector: { matchLabels: { app: redis } }
  template:
    metadata: { labels: { app: redis } }
    spec:
      containers:
      - name: redis
        image: docker.m.daocloud.io/library/redis:7.2-alpine
        args: ["redis-server", "--save", "", "--appendonly", "yes", "--appendfsync", "everysec"]
        readinessProbe: { exec: { command: ["redis-cli", "ping"] } }
---
apiVersion: v1
kind: Service
metadata: { name: redis, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec: { selector: { app: redis }, ports: [{ port: 6379 }] }
"@
    } else {
        @"
apiVersion: apps/v1
kind: Deployment
metadata: { name: pd, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  selector: { matchLabels: { app: pd } }
  template:
    metadata: { labels: { app: pd } }
    spec:
      containers: [{ name: pd, image: pingcap/pd:v8.5.0, args: ["--name=pd", "--data-dir=/data", "--client-urls=http://0.0.0.0:2379", "--advertise-client-urls=http://pd:2379"] }]
---
apiVersion: v1
kind: Service
metadata: { name: pd, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec: { selector: { app: pd }, ports: [{ port: 2379 }] }
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: tikv, namespace: $Namespace, labels: { app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  selector: { matchLabels: { app: tikv } }
  template:
    metadata: { labels: { app: tikv } }
    spec:
      containers: [{ name: tikv, image: pingcap/tikv:v8.5.0, args: ["--addr=0.0.0.0:20160", "--advertise-addr=tikv:20160", "--pd=pd:2379"] }]
"@
    }
    $imagePullSecrets = if ($GhcrToken) { "      imagePullSecrets:`n      - name: brewfs-ghcr" } else { '' }
    $job = @"
apiVersion: batch/v1
kind: Job
metadata: { name: $JobName, namespace: $Namespace, labels: { app: brewfs-perf, app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 86400
  template:
    metadata: { labels: { app: brewfs-perf, job: $JobName } }
    spec:
      restartPolicy: Never
$imagePullSecrets
$initContainers
      containers:
      - name: perf
        image: $Image
        imagePullPolicy: Always
        env:
        - { name: PERF_TOOLS, value: '$PerfTools' }
        - { name: BREWFS_DATA_BACKEND, value: '$dataEnv' }
        - { name: BREWFS_META_BACKEND, value: '$Backend' }
        - { name: BREWFS_META_URL, value: 'redis://redis:6379/0' }
        - { name: BREWFS_META_TIKV_PD_ENDPOINTS, value: 'pd:2379' }
        - { name: BREWFS_ARTIFACT_ROOT, value: /artifacts }
        - { name: BREWFS_ARTIFACT_DIR, value: /artifacts/$JobName }
        - { name: BREWFS_S3_ENDPOINT, value: http://rustfs:9000 }
        - { name: BREWFS_S3_BUCKET, value: brewfs-data }
        - { name: BREWFS_S3_FORCE_PATH_STYLE, value: "true" }
        - { name: BREWFS_S3_REGION, value: us-east-1 }
        - { name: BREWFS_ARTIFACT_HOLD_SECONDS, value: "$ArtifactHoldSeconds" }
        - { name: AWS_ACCESS_KEY_ID, value: rustfsadmin }
        - { name: AWS_SECRET_ACCESS_KEY, value: rustfsadmin }
        securityContext:
          privileged: true
          capabilities: { add: [SYS_ADMIN] }
          appArmorProfile: { type: Unconfined }
        volumeMounts:
        - { name: fuse, mountPath: /dev/fuse }
        - { name: artifacts, mountPath: /artifacts }
        - { name: state, mountPath: /var/lib/brewfs }
        - { name: perf-runner, mountPath: /usr/local/bin/run_perf_in_container.sh, subPath: run_perf_in_container.sh, readOnly: true }
      volumes:
      - { name: fuse, hostPath: { path: /dev/fuse, type: CharDevice } }
      - { name: artifacts, emptyDir: {} }
      - { name: state, emptyDir: {} }
      - name: perf-runner
        configMap:
          name: brewfs-perf-runner
          defaultMode: 0755
"@
    $parts = @($meta.TrimEnd())
    if ($storage) { $parts += $storage.TrimEnd() }
    $parts += $job.TrimStart()
    return ($parts -join "`n---`n")
}

function Render-ManagedManifests {
    $dataEnv = if ($DataBackend -eq 's3') { 's3' } else { 'local-fs' }
    $runnerMount = if ($Workload -eq 'brewfs') {
        '        - { name: perf-runner, mountPath: /usr/local/bin/run_perf_in_container.sh, subPath: run_perf_in_container.sh, readOnly: true }'
    } else { '' }
    $runnerVolume = if ($Workload -eq 'brewfs') {
        @"
      - name: perf-runner
        configMap:
          name: brewfs-perf-runner
          defaultMode: 0755
"@
    } else { '' }
    $juicefsCacheMount = if ($Workload -eq 'juicefs') {
        '        - { name: state, mountPath: /var/lib/juicefs }'
    } else { '' }
    $workloadEnv = if ($Workload -eq 'brewfs') {
        @"
        - { name: BREWFS_DATA_BACKEND, value: '$dataEnv' }
        - { name: BREWFS_META_BACKEND, value: '$Backend' }
        - { name: BREWFS_META_URL, value: '$MetaUrl' }
        - { name: BREWFS_META_TIKV_PD_ENDPOINTS, value: '$MetaUrl' }
        - { name: BREWFS_S3_ENDPOINT, value: '$S3Endpoint' }
        - { name: BREWFS_S3_BUCKET, value: '$S3Bucket' }
        - { name: BREWFS_S3_FORCE_PATH_STYLE, value: '$($S3ForcePathStyle.ToString().ToLowerInvariant())' }
        - { name: BREWFS_S3_REGION, value: '$S3Region' }
"@
    } else {
        @"
        - { name: JUICEFS_META_BACKEND, value: '$Backend' }
        - { name: JFS_META_URL, value: '$MetaUrl' }
        - { name: JFS_S3_ENDPOINT, value: '$S3Endpoint' }
        - { name: JFS_S3_BUCKET, value: '$S3Bucket' }
        - { name: JFS_S3_REGION, value: '$S3Region' }
        - { name: JFS_COMPRESS, value: none }
        - { name: JFS_WRITEBACK, value: "true" }
        - { name: JFS_BUFFER_SIZE_MIB, value: "4096" }
        - { name: JFS_CACHE_SIZE_MIB, value: "8192" }
        - { name: JFS_CACHE_LARGE_WRITE, value: "true" }
        - { name: JFS_MAX_UPLOADS, value: "4" }
        - { name: JFS_MAX_STAGE_WRITE, value: "4" }
        - { name: JFS_MAX_DOWNLOADS, value: "8" }
        - { name: JFS_OPEN_CACHE, value: "1s" }
        - { name: JFS_OPEN_CACHE_LIMIT, value: "65536" }
        - { name: JFS_BACKUP_META, value: "0" }
        - { name: JFS_NO_USAGE_REPORT, value: "true" }
        - { name: JFS_CACHE_DIR, value: /var/lib/juicefs/cache }
"@
    }
    $imagePullSecrets = if ($GhcrToken) { "      imagePullSecrets:`n      - name: brewfs-ghcr" } else { '' }
    $credentialEnv = if ($Workload -eq 'juicefs' -or $DataBackend -eq 's3') {
        @"
        - name: AWS_ACCESS_KEY_ID
          valueFrom: { secretKeyRef: { name: brewfs-perf-backend, key: s3-access-key } }
        - name: AWS_SECRET_ACCESS_KEY
          valueFrom: { secretKeyRef: { name: brewfs-perf-backend, key: s3-secret-key } }
        - { name: AWS_DEFAULT_REGION, value: '$S3Region' }
        - { name: AWS_EC2_METADATA_DISABLED, value: "true" }
"@
    } else { '' }
    @"
apiVersion: batch/v1
kind: Job
metadata: { name: $JobName, namespace: $Namespace, labels: { app: brewfs-perf, app.kubernetes.io/managed-by: brewfs-perf-runner } }
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 86400
  template:
    metadata: { labels: { app: brewfs-perf, job: $JobName } }
    spec:
      restartPolicy: Never
$imagePullSecrets
      containers:
      - name: perf
        image: $Image
        imagePullPolicy: Always
        env:
        - { name: PERF_TOOLS, value: '$PerfTools' }
        - { name: PERF_FIO_DIRECT, value: "0" }
        - { name: PERF_FIO_IOENGINE, value: io_uring }
        - { name: PERF_FIO_IODEPTH, value: "1" }
        - { name: PERF_FIO_PREFILL_DRAIN, value: "true" }
        - { name: PERF_FIO_PREFILL_REMOUNT, value: "true" }
        - { name: PERF_FIO_POST_WRITE_DRAIN, value: "true" }
        - { name: BREWFS_ARTIFACT_ROOT, value: /artifacts }
        - { name: BREWFS_ARTIFACT_DIR, value: /artifacts/$JobName }
        - { name: BREWFS_ARTIFACT_HOLD_SECONDS, value: "$ArtifactHoldSeconds" }
$workloadEnv
$credentialEnv
        securityContext:
          privileged: true
          capabilities: { add: [SYS_ADMIN] }
          appArmorProfile: { type: Unconfined }
        volumeMounts:
        - { name: fuse, mountPath: /dev/fuse }
        - { name: artifacts, mountPath: /artifacts }
        - { name: state, mountPath: /var/lib/brewfs }
$juicefsCacheMount
$runnerMount
      volumes:
      - { name: fuse, hostPath: { path: /dev/fuse, type: CharDevice } }
      - { name: artifacts, emptyDir: {} }
      - { name: state, emptyDir: {} }
$runnerVolume
"@
}

function Assert-TestInputs {
    if (-not $ResultVaultUrl) { throw '必须设置 -ResultVaultUrl 或 BREWFS_RESULTS_URL。' }
    $parsed = $null
    if (-not [Uri]::TryCreate($ResultVaultUrl.Trim().TrimEnd('/'), [UriKind]::Absolute, [ref]$parsed) -or $parsed.Scheme -notin @('http', 'https')) {
        throw "无效的 Result Vault URL: $ResultVaultUrl"
    }
    if ($ServiceMode -eq 'managed' -and -not $MetaUrl) { throw 'managed 模式必须提供 -MetaUrl（托管 Redis URL 或 TiKV PD endpoint）。' }
    if ($ServiceMode -eq 'embedded' -and $Workload -eq 'juicefs') { throw 'JuiceFS ACK runner 需要 managed 外部元数据服务；请使用 -ServiceMode managed。' }
    if (($Workload -eq 'juicefs' -or $DataBackend -eq 's3') -and -not $S3Endpoint) { throw 'S3/OSS 测试必须提供 -S3Endpoint。' }
    if (($Workload -eq 'juicefs' -or $DataBackend -eq 's3') -and (-not $S3AccessKey -or -not $S3SecretKey)) { throw 'S3/OSS 测试必须提供 -S3AccessKey 和 -S3SecretKey。' }
}

function Apply-Test {
    Assert-TestInputs
    $namespaceYaml = Invoke-Checked $Kubectl @('create', 'namespace', $Namespace, '--dry-run=client', '-o', 'yaml')
    Invoke-KubectlYaml $namespaceYaml | Out-Host
    $existingJobs = & $Kubectl @(
        'get', 'job', '--namespace', $Namespace, '--selector', $ManagedByLabel,
        '--output', 'name', '--ignore-not-found'
    ) 2>$null
    if ($LASTEXITCODE -eq 0 -and $existingJobs) {
        throw "命名空间 $Namespace 已有 BrewFS 性能任务：$($existingJobs -join ', ')。请等待其结束、执行 -Action destroy，或使用不同的 -Namespace。"
    }
    $script:TestResourcesApplied = $true
    if ($Workload -eq 'brewfs') {
        $runnerSource = Join-Path $RepoRoot 'docker\compose-xfstests\run_perf_in_container.sh'
        $runnerPath = Join-Path $env:TEMP "brewfs-perf-runner-$([guid]::NewGuid().ToString('N')).sh"
        try {
            $runnerContent = (Get-Content -LiteralPath $runnerSource -Raw) -replace "`r`n", "`n"
            [IO.File]::WriteAllText($runnerPath, $runnerContent, [Text.UTF8Encoding]::new($false))
            $configMapYaml = Invoke-Checked $Kubectl @('create', 'configmap', 'brewfs-perf-runner', '--namespace', $Namespace,
                "--from-file=run_perf_in_container.sh=$runnerPath", '--dry-run=client', '-o', 'yaml')
            Invoke-KubectlYaml $configMapYaml | Out-Host
            Invoke-Checked $Kubectl @('label', 'configmap', 'brewfs-perf-runner', '--namespace', $Namespace,
                $ManagedByLabel, '--overwrite') | Out-Null
        } finally {
            Remove-Item -LiteralPath $runnerPath -Force -ErrorAction SilentlyContinue
        }
    }
    if ($Workload -eq 'juicefs' -or $DataBackend -eq 's3') {
        $backendSecret = [ordered]@{
            apiVersion = 'v1'
            kind = 'Secret'
            metadata = [ordered]@{ name = 'brewfs-perf-backend'; namespace = $Namespace; labels = [ordered]@{ 'app.kubernetes.io/managed-by' = 'brewfs-perf-runner' } }
            type = 'Opaque'
            stringData = [ordered]@{ 's3-access-key' = $S3AccessKey; 's3-secret-key' = $S3SecretKey }
        } | ConvertTo-Json -Depth 8 -Compress
        Invoke-KubectlYaml $backendSecret | Out-Host
    }
    if ($GhcrToken) {
        $auth = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("${GhcrUsername}:${GhcrToken}"))
        $dockerConfig = [ordered]@{
            auths = [ordered]@{
                'ghcr.io' = [ordered]@{
                    username = $GhcrUsername
                    password = $GhcrToken
                    auth = $auth
                }
            }
        } | ConvertTo-Json -Depth 8 -Compress
        $dockerConfigBase64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($dockerConfig))
        $secretYaml = [ordered]@{
            apiVersion = 'v1'
            kind = 'Secret'
            metadata = [ordered]@{ name = 'brewfs-ghcr'; namespace = $Namespace }
            type = 'kubernetes.io/dockerconfigjson'
            data = [ordered]@{ '.dockerconfigjson' = $dockerConfigBase64 }
        } | ConvertTo-Json -Depth 8 -Compress
        Invoke-KubectlYaml $secretYaml | Out-Host
        Invoke-Checked $Kubectl @('label', 'secret', 'brewfs-ghcr', '--namespace', $Namespace,
            $ManagedByLabel, '--overwrite') | Out-Null
    }
    $yaml = if ($ServiceMode -eq 'managed') { Render-ManagedManifests } else { Render-Manifests }
    $yaml | & $Kubectl apply -f - 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) {
        $debugManifest = Join-Path $env:TEMP "$JobName.yaml"
        Set-Content -LiteralPath $debugManifest -Value $yaml -Encoding utf8
        throw "Kubernetes manifest apply 失败，manifest 已保存到 $debugManifest。"
    }
    if ($ServiceMode -eq 'embedded') {
        Invoke-Checked $Kubectl @('wait', '--for=condition=available', "deployment/$($Backend -eq 'redis' ? 'redis' : 'pd')", '-n', $Namespace, '--timeout=10m') | Out-Host
        if ($DataBackend -eq 's3') { Invoke-Checked $Kubectl @('wait', '--for=condition=available', 'deployment/rustfs', '-n', $Namespace, '--timeout=10m') | Out-Host }
    }
    $exported = $null
    $deadline = [DateTime]::UtcNow.AddHours(48)
    while ([DateTime]::UtcNow -lt $deadline -and -not $exported) {
        $pod = Get-PerfPod
        $phase = ((Invoke-Checked $Kubectl @('get', 'pod', $pod, '-n', $Namespace, '-o', 'jsonpath={.status.phase}') -join '')).Trim()
        if ($phase -eq 'Running') {
            $probe = & $Kubectl @('exec', '-n', $Namespace, $pod, '-c', 'perf', '--', 'test', '-f', "/artifacts/$JobName/perf.complete") 2>&1
            if ($LASTEXITCODE -eq 0) {
                try {
                    $exported = Export-Artifacts -Pod $pod
                    Write-Host "测试结果目录: $($exported.Directory)"
                    Write-Host "测试结果归档: $($exported.Archive)"
                } catch {
                    Write-Warning "导出 artifacts 失败，将在下一轮重试：$($_.Exception.Message)"
                }
                if ($exported) { Upload-ResultVault -Archive $exported.Archive }
            }
        } elseif ($phase -in @('Succeeded', 'Failed')) { break }
        if (-not $exported) { Start-Sleep -Seconds 10 }
    }
    if (-not $exported) { throw 'Job 完成前未能导出 artifacts；如需稍后导出，请使用 -KeepJob 并在 hold 窗口内执行 -Action export。' }
    # perf.complete means the report is finalized. Cleanup is handled by the
    # caller so failures and successful exports follow the same policy.
    if (-not $KeepJob) { return }
    $jobStatus = ((Invoke-Checked $Kubectl @('get', 'job', $JobName, '-n', $Namespace, '-o', 'jsonpath={.status.failed}:{.status.succeeded}') -join '')).Trim()
    if ($jobStatus -match '^([1-9][0-9]*|[1-9]):') {
        Write-Warning "Job 已记录失败项（status=$jobStatus），但 artifacts 已成功导出。"
    } else {
        Invoke-Checked $Kubectl @('wait', '--for=condition=complete', "job/$JobName", '-n', $Namespace, '--timeout=48h') | Out-Host
    }
}

function Export-ExistingJob {
    $deadline = [DateTime]::UtcNow.AddMinutes($ExportTimeoutMinutes)
    while ([DateTime]::UtcNow -lt $deadline) {
        $pod = Get-PerfPod
        $phase = ((Invoke-Checked $Kubectl @('get', 'pod', $pod, '-n', $Namespace, '-o', 'jsonpath={.status.phase}') -join '')).Trim()
        if ($phase -eq 'Running') {
            $probe = & $Kubectl @('exec', '-n', $Namespace, $pod, '-c', 'perf', '--', 'test', '-f', "/artifacts/$JobName/perf.complete") 2>&1
            if ($LASTEXITCODE -eq 0) {
                $exported = Export-Artifacts -Pod $pod
                Write-Host "测试结果目录: $($exported.Directory)"
                Write-Host "测试结果归档: $($exported.Archive)"
                Upload-ResultVault -Archive $exported.Archive
                return
            }
        } elseif ($phase -in @('Succeeded', 'Failed')) {
            throw "Pod 已进入 $phase，emptyDir 中的结果无法再通过 kubectl exec 导出；请在测试运行期间导出，或下次使用持久化卷。"
        }
        Start-Sleep -Seconds 5
    }
    throw "在 $ExportTimeoutMinutes 分钟内未发现可导出的 perf.complete。"
}

function Remove-TestResources {
    $output = & $Kubectl @(
        'delete', 'job,deployment,service,configmap,secret', '--namespace', $Namespace,
        '--selector', $ManagedByLabel, '--ignore-not-found=true', '--wait=false'
    ) 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "清理 BrewFS 性能测试资源失败：$($output -join [Environment]::NewLine)"
    } elseif ($output) {
        $output | Out-Host
    }
    # Also remove jobs created by an older runner that did not carry the
    # managed-by label. The exact job name keeps this fallback scoped.
    & $Kubectl @('delete', 'job', $JobName, '--namespace', $Namespace, '--ignore-not-found=true', '--wait=false') 2>$null | Out-Null
}

switch ($Action) {
    'test' {
        try {
            Assert-TestInputs
            if (-not $SkipImageBuild) { Ensure-Image }
            Apply-Test
        } finally {
            if ($script:TestResourcesApplied -and -not $KeepJob) { Remove-TestResources }
        }
    }
    'status' { Invoke-Checked $Kubectl @('get', 'job', $JobName, '-n', $Namespace, '-o', 'wide') }
    'export' { Export-ExistingJob }
    'destroy' { Remove-TestResources }
}
