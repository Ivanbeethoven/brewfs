# AX on kind (BrewFS workspace integration)

Deploys Google's AX agent executor and Agent Substrate on a single kind cluster,
including a patched AX task runner that accepts an already-mounted workspace
(`AX_PREMOUNTED_WORKSPACE=1`). This is the environment used to validate running
agents against a BrewFS-backed workspace.

## Prerequisites

- Ubuntu 24.04 (or similar), root or passwordless sudo
- Docker with its data-root on a large disk (the scripts assume Docker is already
  installed; `kind` and registry images land there)
- Outbound network access to GitHub, go.dev, dl.k8s.io and kind.sigs.k8s.io
- **No `/dev/kvm` is required**; Substrate uses gVisor (`runsc`).

## Layout

| Script | Purpose |
| --- | --- |
| `env.sh` | pinned versions and paths |
| `01-toolchain.sh` | Go, kubectl, kind, ko under `~/.local` |
| `02-cluster.sh` | clone Substrate v0.5.0, create kind cluster `ax-kind` and local registry |
| `03-substrate.sh` | install Agent Substrate into `ate-system` |
| `04-workerpool.sh` | deploy a gVisor WorkerPool (Substrate ships none by default) |
| `05-ax.sh` | clone the AX fork, build the patched runner, deploy the control plane |
| `06-smoke.sh` | create an AX Task on a pre-mounted workspace and resume it |
| `install.sh` | run all of the above |

## Usage

```bash
cd deploy/ax-kind
sudo -v   # only if your user needs sudo for apt
./install.sh
```

After it finishes:

```bash
source deploy/ax-kind/env.sh
ax get tasks
ax describe task brewfs-task
```

## Notes

- The AX fork branch is `brewfs-workspace` in `Ivanbeethoven/ax`; the runner
  change is opt-in and does not alter upstream behavior.
- `06-smoke.sh` uses a `durableDir` workspace at `/workspace`. A real BrewFS
  workspace must be supplied by the runtime (CSI volume or a sidecar mount);
  the runner then skips git/bootstrap and runs the command in place.
- AX tasks are suspended on creation by design; the smoke script resumes the
  task before checking for `Running`.
- `ax ssh` requires the guest-services path; if it fails with a server-preface
  error, the task itself can still be Running (this is a known open item).
