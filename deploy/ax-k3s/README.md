# AX on k3s

Deploys Google's AX agent executor and Agent Substrate on a single-node k3s
cluster, with a patched AX task runner that accepts an already-mounted
workspace (`AX_PREMOUNTED_WORKSPACE=1`).

This is an alternative to `deploy/ax-kind/` for hosts where:

- Docker must not be restarted or used for the cluster (other workloads run on
  the host), and/or
- the kind node image cannot be pulled, but k3s can.

## What it installs

| Component | Notes |
| --- | --- |
| k3s | own containerd, data-dir `<K3S_DATA_DIR>`, traefik/servicelb/metrics-server disabled |
| `ate-system` | Agent Substrate v0.5.0 (PostgreSQL, RustFS, OTel, ate API/controller/atelet/atenet) |
| WorkerPool | gVisor pool `ate-demo-sandbox/sandbox-workerpool` |
| `ax-system` | AX control plane (ax-server + ax-redis) |
| task runner | fork branch `brewfs-workspace`, pre-mounted workspace support |

## Prerequisites

- Ubuntu/Debian host with root or passwordless sudo
- Outbound access to `get.k3s.io`, GitHub, `dl.google.com`, `gcr.io`,
  `registry.k8s.io` (or a working `GITHUB_PROXY_BASE`/`K3S_INSTALL_URL`)
- Docker (only to build the task-runner image; k3s itself does not use Docker)
- Enough disk for the images; the k3s data-dir should be on a large volume

## Usage

```bash
cd deploy/ax-k3s
./install.sh
```

Useful environment variables: `K3S_DATA_DIR`, `KUBECONFIG`, `REGISTRY_PORT`,
`KO_DOCKER_REPO`, `SUBSTRATE_VERSION`, `AX_BRANCH`, `GITHUB_PROXY_BASE`.

After it finishes:

```bash
source deploy/ax-k3s/env.sh
ax get tasks
ax describe task brewfs-task
```

## Design notes

- **Registry.** Substrate's kind profile rewrites `localhost:<port>` image
  references to `kind-registry:5000` for guest pulls. A host Docker registry is
  not reachable from pods, so `02-registry.sh` runs the registry inside the
  cluster on the node network (`hostNetwork`, port `REGISTRY_PORT`) and exposes
  it as Service `kind-registry:5000` in `ate-system`. ko pushes to
  `localhost:REGISTRY_PORT`; k3s containerd pulls from there; atelet resolves
  `kind-registry`.
- **Digest pinning.** Substrate requires task images pinned by digest, so
  `06-smoke.sh` pushes the runner and builds the reference from the push
  digest.
- **Disk pressure.** k3s defaults to a 15% imagefs threshold. The config sets
  `eviction-hard=imagefs.available<5%` so a busy build disk does not evict
  system pods.
- **Single writer.** A BrewFS workspace has one writer lease. Multiple AX actors
  must not mount one workspace independently; use one mount pod with
  cooperating containers (`BrewFSWorkspaceMount.spec.agent`) or a single
  CSI-backed AX task.
- **CSI.** `operator/brewfs-csi` (separate work) exposes a workspace as a CSI
  volume. This deployment does not install it; the smoke tests the
  pre-mounted `durableDir` path.

## Cleanup

The cluster and all resources live in k3s and the k3s data-dir. To remove
everything, run `sudo k3s-uninstall.sh` (or `k3s-uninstall.sh` from the same
directory) and delete `<K3S_DATA_DIR>`. This does not touch Docker or other
host workloads.
