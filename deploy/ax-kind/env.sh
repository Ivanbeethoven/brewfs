#!/usr/bin/env bash
# Shared environment for the AX-on-kind deployment.
set -euo pipefail

export GO_VERSION="${GO_VERSION:-1.27.3}"
export KUBECTL_VERSION="${KUBECTL_VERSION:-v1.34.1}"
export KIND_VERSION="${KIND_VERSION:-v0.30.0}"
export KO_VERSION="${KO_VERSION:-v0.19.1}"

export KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-ax-kind}"
export KUBECTL_CONTEXT="${KUBECTL_CONTEXT:-kind-${KIND_CLUSTER_NAME}}"
export SUBSTRATE_VERSION="${SUBSTRATE_VERSION:-v0.5.0}"

export AX_REPO="${AX_REPO:-https://github.com/Ivanbeethoven/ax.git}"
export AX_BRANCH="${AX_BRANCH:-brewfs-workspace}"

export SRC_ROOT="${SRC_ROOT:-$HOME/src}"
export LOCAL_BIN="${LOCAL_BIN:-$HOME/.local/bin}"
export AX_IMAGE_REPO="${AX_IMAGE_REPO:-localhost:5001}"
export TASK_RUNNER_REPO="${TASK_RUNNER_REPO:-${AX_IMAGE_REPO}/ax-task-runner}"

# Optional HTTP(S) proxy for downloads and Go module fetches, e.g.
#   AX_DEPLOY_PROXY=http://127.0.0.1:7897 ./install.sh
export AX_DEPLOY_PROXY="${AX_DEPLOY_PROXY:-}"

# Go modules: proxy.golang.org is not reachable from some networks.
export GOPROXY="${GOPROXY:-https://goproxy.cn,direct}"

# Fallbacks used when GitHub is unreachable or resets large transfers.
export GO_DOWNLOAD_BASE="${GO_DOWNLOAD_BASE:-https://mirrors.aliyun.com/golang}"
export GITHUB_PROXY_BASE="${GITHUB_PROXY_BASE:-https://ghproxy.net/}"

export PATH="${LOCAL_BIN}:${HOME}/.local/go/bin:${PATH}"
export KIND_EXPERIMENTAL_PROVIDER="${KIND_EXPERIMENTAL_PROVIDER:-docker}"

mkdir -p "${SRC_ROOT}" "${LOCAL_BIN}"

# fetch_github <github-url> <dest>: proxy first, then a GitHub mirror, then direct.
fetch_github() {
  local url="$1" dest="$2"
  if [ -n "${AX_DEPLOY_PROXY}" ]; then
    curl -4 -fsSL -x "${AX_DEPLOY_PROXY}" "$url" -o "$dest" && return 0
  fi
  curl -4 -fsSL "${GITHUB_PROXY_BASE}${url}" -o "$dest" && return 0
  curl -4 -fsSL "$url" -o "$dest"
}
