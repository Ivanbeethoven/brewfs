#!/usr/bin/env bash
# Shared environment for the AX-on-k3s deployment.
set -euo pipefail

export GO_VERSION="${GO_VERSION:-1.27.3}"
export KO_VERSION="${KO_VERSION:-v0.19.1}"
export K3S_VERSION="${K3S_VERSION:-v1.36.5+k3s1}"
export K3S_DATA_DIR="${K3S_DATA_DIR:-/mnt/brewfs-data/k3s}"
export KUBECONFIG="${KUBECONFIG:-/etc/rancher/k3s/k3s.yaml}"
export KUBECTL_CONTEXT="${KUBECTL_CONTEXT:-default}"

export SUBSTRATE_VERSION="${SUBSTRATE_VERSION:-v0.5.0}"
export AX_REPO="${AX_REPO:-https://github.com/Ivanbeethoven/ax.git}"
export AX_BRANCH="${AX_BRANCH:-brewfs-workspace}"

export SRC_ROOT="${SRC_ROOT:-$HOME/src}"
export LOCAL_BIN="${LOCAL_BIN:-$HOME/.local/bin}"

export REGISTRY_PORT="${REGISTRY_PORT:-5001}"
export KO_DOCKER_REPO="${KO_DOCKER_REPO:-localhost:${REGISTRY_PORT}}"
export TASK_RUNNER_REPO="${TASK_RUNNER_REPO:-${KO_DOCKER_REPO}/ax-task-runner}"
export BUCKET_NAME="${BUCKET_NAME:-ate-snapshots}"

export PATH="${LOCAL_BIN}:${HOME}/.local/go/bin:/usr/local/bin:${PATH}"
export GITHUB_PROXY_BASE="${GITHUB_PROXY_BASE:-https://ghfast.top/}"

mkdir -p "${SRC_ROOT}" "${LOCAL_BIN}"

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

# fetch_github <github-url> <dest>: proxy first, then a GitHub mirror, then direct.
fetch_github() {
  local url="$1" dest="$2"
  curl -4 -fsSL "${GITHUB_PROXY_BASE}${url}" -o "$dest" && return 0
  curl -4 -fsSL "$url" -o "$dest"
}
