# Workspace statfs synthetic-capacity fallback

## Cause and change

The [four-agent fork run](2026-10-23-native-workspace-four-agent-demo.md)
captured 56 `brewfs::fuse: statfs failed: unsupported` messages. They came from
the BrewFS FUSE callback, not CSI `NodeGetVolumeStats`:

```text
FUSE statfs
  -> VFS::stat_fs_cached_for_fuse
  -> WorkspaceMetaLayer::stat_fs
  -> MetaError::NotSupported
```

The FUSE callback logged that metadata error and returned zero block counts.
A fresh baseline AX task reproduced the problem on the existing deployment:

```text
Block size: 4096       Fundamental block size: 4096
Blocks: Total: 0          Free: 0          Available: 0
Inodes: Total: 0          Free: -1
total_bytes= 0 free_bytes= 0
```

`WorkspaceMetaLayer::stat_fs` now returns the existing
`stat_fs_snapshot_from_usage(0, 0)` fallback. It reports **synthetic** 1 PiB
total and 1 PiB free/available, zero placeholder used inodes, and a synthetic
10,485,760 free-inode budget. It performs no layer/namespace scan. These values
are not measured backend capacity, a quota, or a guarantee that allocation or
writes will succeed. The default uses the existing shared constant; no new
runtime configuration or persistent counter is introduced.

The change applies to the established `workspace-v1` metadata adapter. The
dedicated entity FUSE adapter is separate. CSI `NodeGetVolumeStats` remains
unimplemented and unadvertised; this change does not add that capability or
hide unrelated metadata errors in FUSE.

## Focused test and live verification

The obsolete unit test expecting `NotSupported` was replaced with
`tests/workspace_statfs.rs`. This independent integration target uses the public
workspace metadata API and in-memory SQLite, without any object-store mock.
It checks the fallback for both empty and populated workspaces.

```bash
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/target-operator \
cargo test -p brewfs --test workspace_statfs --no-default-features \
  --features fuse-tokio-runtime,workspace-overlay -- --nocapture
```

Observed result: **1 passed, 0 failed, 0 ignored**, in 0.04 seconds after
compilation. This invocation also built the `brewfs` executable. A copy was
stripped and packaged on top of the existing CSI image, replacing only
`/usr/local/bin/brewfs`:

```dockerfile
FROM localhost:5001/brewfs-csi@sha256:91d446adb46e48e80b36bd8715440fc3fafb7e168d4de9a062adf2829e32ec96
COPY brewfs /usr/local/bin/brewfs
```

The pushed image is
`localhost:5001/brewfs-csi@sha256:39aa27cf4f8e0ca35de2112e6d01efb910768830eab988f976444c3a82641333`.
After confirming there were no demo tasks or CSI state files, only the
`brewfs-demo/brewfs-csi-node` DaemonSet's mount-client image was rolled:

```bash
docker build -t localhost:5001/brewfs-csi:statfs-a881b9d /mnt/brewfs-data/src/fork-statfs-20261023/image
docker push localhost:5001/brewfs-csi:statfs-a881b9d
kubectl -n brewfs-demo set image ds/brewfs-csi-node brewfs-csi=localhost:5001/brewfs-csi@sha256:39aa27cf4f8e0ca35de2112e6d01efb910768830eab988f976444c3a82641333
kubectl -n brewfs-demo rollout status ds/brewfs-csi-node --timeout=180s
```

The DaemonSet successfully rolled out; pod `brewfs-csi-node-8wdg2` was 2/2
Running with zero restarts. The new verification task mounted the existing
fork through CSI and reached Running, Ready=True and WorkspaceReady=True.
The deployed `/usr/local/bin/brewfs` SHA-256 matched the packaged binary:
`de36d2dc114611cc174525d881b30b6a184fb667cfa28214cbf5ba0e40ffb204`.

| Identity | Value |
| --- | --- |
| Task | `fork-statfs-verify` |
| Task ID | `task-fork-statfs-verify-1792736327` |
| Actor UID | `ab54e219-54a1-4643-87d8-393c71717e05` |
| AX/BrewFS workspace | `fork-multi-b` |
| StorageClass | `brewfs-brewfs-demo-fork-multi-b` |
| CSI volume | `brewfs://fork-demo/2112dbc1-d165-55d1-b7be-f521758394ce` |

The saved `verify.py` applies/resumes the task and invokes
`ax -a fork-demo ssh fork-statfs-verify -- python3 -c <probe.py contents>`.
The probe executes `stat -f /workspace`, asserts `os.statvfs` byte counts,
and reads existing files without rewriting them. Actual SSH output:

```text
  File: "/workspace"
    ID: 0        Namelen: 255     Type: v9fs
Block size: 4096       Fundamental block size: 4096
Blocks: Total: 274877906944 Free: 274877906944 Available: 274877906944
Inodes: Total: 10485760   Free: 10485760
{"frsize": 4096, "blocks": 274877906944, "bfree": 274877906944, "bavail": 274877906944, "total_bytes": 1125899906842624, "free_bytes": 1125899906842624, "available_bytes": 1125899906842624}
STATFS_ONE_PIB_AND_DATA_PASS
```

`stat -f` inside the CSI node container on the recorded shared mount also
reported these exact block/inode counts with `Type: fuse`, corroborating the
guest's gVisor `v9fs` view. Guest stdout contained the actual PASS message,
independently of the runner's command-text log.

The three existing files retained their previously recorded contents/hashes:

| File | SHA-256 |
| --- | --- |
| `agent-a.txt` | `6ceeb2da0f21d978ba0f7930a78b3b8a068317c0f4f7a8013791a285d219c9cc` |
| `parent-only.txt` | `e58b1728fe9bbc653232384b053a94b53c2b15986c7db9b2f8f573ff1857c616` |
| `agent-mb.txt` | `3d8820fc2b47e9f265bd415e6cc51ad920a0e0194ba7611e18abc450d1f7b02f` |

The probe also asserted absence of `agent-ma.txt`, `agent-mc.txt`, and
`agent-md.txt`. The CSI log collected from the fresh probe's start through
cleanup contained **zero `statfs failed` messages**. This is a targeted fresh
mount check, not a rerun of the full four-agent workload.

Both temporary baseline and verification tasks were deleted. The verification
cleanup checked that the state JSON file, FUSE process and all recorded
shared/actor mount paths were gone: `STATFS_MOUNT_RELEASED_PASS`. Pre-existing
actor UIDs and worker pod UIDs matched the before-state. No protected
Substrate/WorkerPool configuration changed; workspace data remains intact.

## Gates and build limits

The first unit-test build was intentionally stopped during dependency
compilation after the live baseline reproduced the failure. It ran no tests.
The subsequent monolithic overlay lib-test build hit an enforced 18 GiB limit
and systemd recorded `Result=oom-kill`; its wrapper exited 143 before any test
ran. Neither attempt is a passing or failing test result. Moving the regression
to the integration target above allowed it to compile and pass.

A redundant standalone overlay build was cancelled with exit 130 after the
successful integration invocation had already produced the tested executable.
The preserved binary, rather than an incomplete build output, was packaged.

All required local gates completed successfully:

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| Four required perf-runner `bash -n` checks and three report/matrix shell tests | Passed |
| `cargo build --workspace` | Passed |
| `cargo check --workspace` | Passed |
| `cargo check -p brewfs --no-default-features --features fuse-tokio-runtime` | Passed |
| `cargo check -p brewfs --no-default-features --features fuse-io-uring-runtime` | Passed |
| `cargo test --workspace --lib --bins -- --test-threads=1` | 1,196 passed, 0 failed, 225 ignored; two binary targets had zero tests |
| Focused `workspace_statfs` integration test above | 1 passed, 0 failed, 0 ignored |
| `cargo clippy --workspace` | Passed with dependency warnings |
| `cargo check --workspace --no-default-features --features fuse-tokio-runtime,workspace-overlay` | Passed |
| `cargo clippy --workspace --no-default-features --features fuse-tokio-runtime,workspace-overlay` | Passed with dependency warnings |
| `git diff --check` | Passed |

Rust commands used `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and the target directory
shown above. Clippy ran without `-D warnings`. The conditional `rfuse3`
workspace-member checks did not apply: `cargo metadata` confirmed it is not a
member. This is not a passing full overlay lib-test/all-features suite claim.

The shell checks were syntax checks of `run_perf_in_container.sh`,
`run_redis_perf.sh`, `run_juicefs_perf_in_container.sh`, and `run_juicefs_perf.sh`,
plus execution of `test_perf_report_delta.sh`, `test_juicefs_direct_matrix.sh`,
and `test_juicefs_perf_report.sh`, all under `docker/compose-xfstests/`.
The gate wrapper retains exit 1 because it records the intentionally cancelled
duplicate overlay build as exit 130; every required command in the table above
passed individually. The failed 18 GiB compile is recorded separately, without
counting tests that never ran.

Artifacts and exact gate commands are saved in
`/mnt/brewfs-data/src/fork-statfs-20261023/`; `/tmp/statfs-progress.md` points to
its `progress.md`. Key files: `baseline-statfs.txt`, `baseline-csi.log`,
`focused-build-oom.log`, `focused-integration.log`, `gates.sh`,
`gates-summary.txt`, `image-build.log`, `image-push.log`, `binary.sha256`,
`task.yaml`, `actor.json`, `guest.log`, `ssh-proof.txt`,
`direct-fuse-statfs.txt`, `csi-after.log`, `cleanup-proof.txt`, and
`verification-result.txt`. The product changes are on
`codex/workspace-fork-20261022`; no merge is part of this task.
