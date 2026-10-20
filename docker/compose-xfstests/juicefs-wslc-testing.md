# JuiceFS performance testing with wslc-compose

This test runs JuiceFS and fio inside a privileged WSLC container. It mirrors
the BrewFS performance topology so that both filesystems use the same metadata,
object storage, image, and fio profiles.

## Topology

| Service | Purpose |
| --- | --- |
| `redis` | JuiceFS metadata backend |
| `rustfs` | S3-compatible data backend |
| `perf` | Privileged Debian container that mounts JuiceFS and runs fio |

The `perf` container installs JuiceFS from its official CDN, formats a fresh
volume, creates `/dev/fuse` when WSLC does not populate the node, mounts the
filesystem, runs fio, and verifies that RustFS contains data objects.

The implementation is split across:

- `wslc-juicefs-perf.yml`: service and volume definitions.
- `juicefs-tools/run_test.sh`: dependency installation, FUSE mount, fio, and
  RustFS verification.
- `run_juicefs_perf_wslc.ps1`: per-profile project lifecycle, artifact
  validation, reporting, and cleanup.

## Prerequisites

- A WSLC build with privileged container support.
- The matching custom `wslcsdk.dll` beside `wslc-compose.exe`. Windows loads
  the DLL in the executable directory before the system copy; an older DLL
  silently drops the privileged flag.
- `wslc.exe` on `PATH` and `wslservice` running.
- A Docker Hub registry mirror in `WSLC_REGISTRY_MIRROR` when direct pulls are
  unavailable. Store only the host name, without `https://`.
- Sufficient space on the drive containing `WSLC_COMPOSE_STATE_ROOT` and the
  artifact directory. The default big profiles transfer 8 GiB each.

## Smoke test

Use a small dataset before the full performance run:

```powershell
Set-Location D:\--------code----------\brewfs

$env:WSLC_COMPOSE = "D:\--------code----------\wslc-compose\target\release\wslc-compose.exe"
$env:WSLC_COMPOSE_STATE_ROOT = "D:\wslc-compose-tests\juicefs-smoke"
$env:PERF_FIO_BIGWRITE_SIZE = "64m"
$env:PERF_FIO_BIGWRITE_NUMJOBS = "2"
$env:PERF_FIO_BIGREAD_SIZE = "64m"
$env:PERF_FIO_BIGREAD_NUMJOBS = "2"

.\docker\compose-xfstests\run_juicefs_perf_wslc.ps1 `
  -ArtifactsDir "D:\juicefs-artifacts\smoke" `
  -AptMirror "http://<apt-mirror>" `
  -Tools fio-bigwrite,fio-bigread
```

The driver fails unless every fio report contains jobs, every job reports zero
errors, transferred bytes are nonzero, and the RustFS object listing is
nonempty.

## Full profiles

Remove the `PERF_FIO_*` overrides to use the normal profile sizes:

```powershell
Get-ChildItem Env:PERF_FIO_* | Remove-Item

.\docker\compose-xfstests\run_juicefs_perf_wslc.ps1 `
  -ArtifactsDir "D:\juicefs-artifacts\full" `
  -Tools fio-bigwrite,fio-bigread,fio-seqread,fio-seqwrite,fio-randread,fio-randwrite,fio-randrw
```

Read profiles create their datasets first and remount JuiceFS before fio, so
they exercise the filesystem read path instead of reading an empty directory or
only process-local cache.

## Artifacts

For each profile the artifact directory contains:

- `fio-<profile>.json` and `fio-<profile>.log`.
- `fio-<profile>-rustfs-objects.json`.
- `juicefs.log` and `juicefs-version.txt`.
- `rustfs-objects.json`, the latest raw S3 listing.

The PowerShell driver prints read/write throughput and transferred MiB after
validating these files.

## Configuration

| Variable | Purpose |
| --- | --- |
| `WSLC_COMPOSE` | Path to `wslc-compose.exe` |
| `WSLC_COMPOSE_STATE_ROOT` | Session VHD and SDK volume location |
| `WSLC_COMPOSE_SDK_TIMEOUT_SECS` | SDK timeout; `0` waits indefinitely |
| `WSLC_REGISTRY_MIRROR` | Docker Hub mirror host |
| `JUICEFS_APT_MIRROR` | Optional Debian mirror |
| `JUICEFS_INSTALL_URL` | JuiceFS installation script URL |
| `PERF_FIO_<PROFILE>_SIZE` | Dataset size override for one profile |
| `PERF_FIO_<PROFILE>_NUMJOBS` | Job count override for one profile |
| `PERF_FIO_<PROFILE>_RUNTIME` | Runtime override for time-based profiles |

## Cleanup

Without `-Keep`, each per-profile project is removed with `down --volumes`.
The SDK daemon can remain alive for its state root. Stop only the daemon whose
command line contains the exact state root before deleting that directory.
