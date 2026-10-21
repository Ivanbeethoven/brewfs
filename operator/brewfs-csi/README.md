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
  mount is removed before a new process is started. A state file without a PID
  (from an older adapter) is reused conservatively and should be retired before
  production use.

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
per actor. The current AX API does not carry an arbitrary existing workspace
UUID into that template. A production AX bridge therefore still needs a
reviewed workspace-to-StorageClass broker or an API field for a pre-provisioned
volume identity. Until that exists, use the operator's one-mount-pod path.

The controller endpoint in `csidriverconfig.yaml` is intentionally plaintext
inside the cluster. Enable Substrate pod-identity TLS and configure a matching
server name before exposing it outside a trusted cluster network.

## Build and local tests

```bash
cargo fmt --manifest-path operator/brewfs-csi/Cargo.toml --all --check
cargo check --manifest-path operator/brewfs-csi/Cargo.toml --locked
cargo test --manifest-path operator/brewfs-csi/Cargo.toml --locked
```

The image build expects a BrewFS image containing `/usr/local/bin/brewfs`:

```bash
docker build \
  --build-arg BREWFS_IMAGE=ghcr.io/ivanbeethoven/brewfs:<pinned-tag> \
  -t ghcr.io/ivanbeethoven/brewfs-csi:<pinned-tag> \
  operator/brewfs-csi
```

No FUSE mount, Kubernetes cluster, Substrate actor, or `csi-sanity` run was
available in this change. The unit tests use a fake mount-operations boundary;
they validate identity parsing, status mapping, idempotent reference handling,
and stale-target protection without pretending to validate host mount
propagation.

For a kind smoke after installing the base resources and replacing the config
Secret/StorageClass workspace values:

```bash
kubectl apply -k operator/brewfs-csi/manifests
kubectl apply -f operator/brewfs-csi/manifests/examples/kind-smoke.yaml
kubectl -n brewfs-csi-smoke wait --for=condition=Ready pod/workspace-smoke --timeout=180s
kubectl -n brewfs-csi-smoke logs pod/workspace-smoke
```

The smoke command is a recipe only; it was not run here.
