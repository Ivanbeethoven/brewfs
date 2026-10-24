# BrewFS CSI adapter

This directory contains an experimental CSI v1.8 adapter for an existing
BrewFS workspace. It is deliberately standalone (`operator/brewfs-csi` has its
own Cargo workspace) so building the adapter does not change the BrewFS
filesystem binary or its on-disk formats.

## Design

* Driver name: `csi.brewfs.io`.
* Volume identity: `brewfs://<cluster>/<workspace-uuid>`. The cluster name
  prevents an accidental cross-cluster identity collision; the UUID identifies
  the operator-owned workspace. `CreateVolume` is deterministic and
  idempotent. It returns StorageClass parameters as `volume_context`.
* The controller does not create, clone, or delete workspace data. `DeleteVolume`
  is a no-op and the example StorageClass uses `Retain`; workspace lifecycle
  remains with `BrewFSWorkspace` and its operator.
* `NodeStageVolume` starts the existing `brewfs mount` command with the
  configured file, workspace UUID, and privileged mount mode. One node plugin
  reuses one hash-derived mount directory for a volume. `NodePublishVolume`
  bind-mounts that directory into the actor target. The bind is remounted
  read-only when requested. The plugin needs `SYS_ADMIN`, `/dev/fuse`, and
  bidirectional propagation for the host paths it serves.
* State is an atomically replaced JSON file under `--state-dir`. It records the
  mount context, published targets, and the PID of a mount process started by
  this plugin. Repeated stage/publish and unpublish/unstage calls are safe.
  Unstage refuses to remove a shared mount while a target is still published.
  If a recorded mount process is gone but its mountpoint remains, the stale
  mount is removed before a new process is started. An existing mount without
  a recorded PID (including state from an older adapter) is rejected; unmount
  it before retrying stage. A mount visibility timeout kills and reaps the
  child, then removes any mount that appeared during the final poll.

The BrewFS process remains the lease authority. CSI reference tracking only
controls when the node plugin may unbind and unstage; it cannot grant a second
writer. A writable workspace therefore has exactly one lease-owning BrewFS
mount. Multiple AX actors must either use one mount pod with cooperating
containers (`BrewFSWorkspaceMount.spec.agent`) or use a single CSI-backed AX
Task. Independent writable Tasks for one workspace are rejected by the
workspace lease and are unsupported. The adapter advertises only
`SINGLE_NODE_WRITER` and `SINGLE_NODE_READER_ONLY`; it does not claim
multi-node reader access because each node would otherwise start a distinct
FUSE process.

The lease is acquired by normal BrewFS mount startup and released by its FUSE
shutdown when `NodeUnstageVolume` unmounts the shared path. If the CSI process
restarts, persisted PID/reference state lets it reuse a live mount or remove a
stale mount before retrying lease acquisition; a failed BrewFS lease
acquisition is returned as a failed stage request.

## Kubernetes resources

`manifests/` contains:

* `CSIDriver` (`attachRequired: false`, `podInfoOnMount: true`), the
  Substrate `ate.dev/v1alpha1/CSIDriverConfig`, and a workspace-aware
  `StorageClass`;
* a controller Deployment and Service. The controller exposes TCP gRPC for
  Substrate and an in-pod Unix socket for the external provisioner sidecar;
* a privileged node DaemonSet with the CSI node registrar, `/dev/fuse`,
  `/var/lib/kubelet/pods`, `/var/lib/ate`, and `/var/lib/brewfs-csi` mounted
  with bidirectional propagation;
* an example config Secret. Replace its Redis/S3 endpoints and credentials
  before use; it is a template, not a working credential;
* separate PVC/Pod examples under `manifests/examples/`.

Substrate v0.5.0 does not consume a PVC. Its `externalVolumeTemplate` names a
StorageClass and asks the registered `CSIDriverConfig` to provision one volume
per actor. The AX fork used for the integration adds
`WorkspaceSpec.externalStorageClassName`; when it is set, AX emits that
`externalVolumeTemplate` and leaves `durableDir` empty. The StorageClass's
`workspace` parameter is the pre-provisioned BrewFS workspace UUID. This keeps
workspace ownership with BrewFS while giving AX a deterministic CSI volume
identity. The AX change is in `Ivanbeethoven/ax` branch `brewfs-workspace`,
commit `a0a660a`.

The controller endpoint in `csidriverconfig.yaml` is intentionally plaintext
inside the cluster. Enable Substrate pod-identity TLS and configure a matching
server name before exposing it outside a trusted cluster network.

## Build and local tests

```bash
cargo fmt --manifest-path operator/brewfs-csi/Cargo.toml --all --check
cargo check --manifest-path operator/brewfs-csi/Cargo.toml --locked
cargo test --manifest-path operator/brewfs-csi/Cargo.toml --locked
```

The image build expects a BrewFS image containing `/usr/local/bin/brewfs`.
The runtime stage uses Debian Trixie so it can run the current BrewFS images
built with glibc 2.39; the builder installs `protobuf-compiler` before running
the locked Rust build:

```bash
docker build \
  --build-arg BREWFS_IMAGE=ghcr.io/ivanbeethoven/brewfs:<pinned-tag> \
  -t ghcr.io/ivanbeethoven/brewfs-csi:<pinned-tag> \
  operator/brewfs-csi
```

## Verification matrix

The following evidence was collected on the single-node k3s cluster
`brewfs-singapore-v2` on 2026-10-22. The existing `ax-system`, `ate-system`,
WorkerPool, and registry workloads were left running. The temporary resources
were in `brewfs-csi-ax-test` and `ax-csi-test`.

| Area | Result | Evidence |
| --- | --- | --- |
| Rust unit tests | **Verified** | `cargo fmt --manifest-path operator/brewfs-csi/Cargo.toml --all --check`; `cargo test --manifest-path operator/brewfs-csi/Cargo.toml --locked`: 6 tests passed. |
| CSI image | **Verified** | Image `localhost:5001/brewfs-csi@sha256:91d446adb46e48e80b36bd8715440fc3fafb7e168d4de9a062adf2829e32ec96`, built from the committed Dockerfile with `protobuf-compiler` and pushed to the in-cluster registry. |
| Identity and node CSI calls | **Verified** | csi-sanity v5.3.0 full run: no identity or node failures after the target-removal fix. `NodeStageVolume`, `NodePublishVolume`, `NodeUnpublishVolume`, `NodeUnstageVolume`, and idempotent stage/publish paths exercised against the live Unix socket. |
| csi-sanity overall | **Partial** | `Ran 33 of 92 Specs`: **30 passed, 3 failed, 1 pending, 58 skipped**. The first failure was `CreateVolume ... same name and different capacity` (expected an error, got nil). The other failures were invalid-volume `DeleteVolume` and nonexistent-volume `ValidateVolumeCapabilities`. The controller is intentionally stateless and maps the configured workspace deterministically, so it does not retain capacity or a volume inventory. Snapshot, expansion, stats, and group-controller cases were skipped as unsupported. |
| AX external-volume selection | **Verified** | AX fork `a0a660a`; `WorkspaceSpec.externalStorageClassName: brewfs-ax-workspace` produced a Substrate `externalVolumeTemplate`. The patched runner image was `localhost:5001/ax-task-runner@sha256:1b89768c157514a8e83571abeea5de71828107186a4d776aa4e050571f99ab5e`. |
| AX task data path | **Verified** | `brewfs-csi-task-3` was resumed to `Running` with `Ready=True`, `WorkspaceReady=True`, worker `10.42.0.23`. `ax ssh ... -- sh -c 'cat /workspace/ax-proof; sha256sum /workspace/ax-proof; wc -c < /workspace/ax-proof'` returned `ax-csi-proof:workspace`, SHA-256 `4f6f4a5fcfde254f79f67bf571401928044d4f54b071f5e0a211ec210aecff38`, and `23` bytes. |
| RustFS publication | **Verified** | `aws s3 ls s3://brewfs-data/chunks-v2/ --recursive` showed `chunks-v2/129/0` (27 bytes). Reading that object through RustFS and hashing its 23-byte payload after the four-byte slice header returned the same SHA and `ax-csi-proof:workspace`. |
| Separate remount | **Unverified** | A prior separate remount could list the directory but returned `EIO` while reading the object. The successful AX read plus direct RustFS object check are the current data-path evidence; a clean independent remount remains to be repeated. |
| AX `ssh` guest tunnel | **Verified for this task** | `ax ssh` returned the exact file contents and hash above. The earlier EOF issue was resolved in the tested task path; no claim is made for unrelated guest-service configurations. |

The focused csi-sanity command used unique paths that did not exist before the
run, and cleanup helpers for paths left by the test harness:

```bash
kubectl -n brewfs-csi-ax-test exec <node-pod> -c brewfs-csi -- \
  /tmp/csi-sanity \
  -csi.endpoint unix:///csi/csi.sock \
  -csi.testvolumeparameters /tmp/csi-params.yaml \
  -csi.stagingdir /tmp/csi-full-stage-20261022b \
  -csi.mountdir /tmp/csi-full-target-20261022b \
  -csi.removemountpathcmd /tmp/remove-csi-path.sh \
  -csi.removestagingpathcmd /tmp/remove-csi-path.sh
```

The unit tests use a fake mount-operations boundary; they validate identity
parsing, status mapping, idempotent reference handling, and stale-target
protection without pretending to validate host mount propagation. The k3s
verification above is the only cluster evidence currently recorded.

For a kind smoke after installing the base resources and replacing the config
Secret/StorageClass workspace values:

```bash
kubectl apply -k operator/brewfs-csi/manifests
kubectl apply -f operator/brewfs-csi/manifests/examples/kind-smoke.yaml
kubectl -n brewfs-csi-smoke wait --for=condition=Ready pod/workspace-smoke --timeout=180s
kubectl -n brewfs-csi-smoke logs pod/workspace-smoke
```

The smoke command is a recipe only; it was not run here.
