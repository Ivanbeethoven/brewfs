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

export PATH="${LOCAL_BIN}:${HOME}/.local/go/bin:${PATH}"
export KIND_EXPERIMENTAL_PROVIDER="${KIND_EXPERIMENTAL_PROVIDER:-docker}"

mkdir -p "${SRC_ROOT}" "${LOCAL_BIN}"
