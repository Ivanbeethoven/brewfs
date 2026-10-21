#!/usr/bin/env bash
# Build and deploy AX (fork branch with pre-mounted workspace support).
set -euo pipefail
source "$(dirname "$0")/env.sh"

AX_DIR="${SRC_ROOT}/ax"
if [ ! -f "${AX_DIR}/Makefile" ]; then
  if ! git clone --depth 1 --branch "${AX_BRANCH}" "${AX_REPO}" "${AX_DIR}"; then
    echo "==> git clone failed; downloading the fork tarball"
    tmp="$(mktemp -d)"
    fetch_github "https://github.com/Ivanbeethoven/ax/archive/refs/heads/${AX_BRANCH}.tar.gz" "${tmp}/ax.tgz"
    tar -C "${tmp}" -xzf "${tmp}/ax.tgz"
    rm -rf "${AX_DIR}"
    mv "${tmp}/ax-${AX_BRANCH}" "${AX_DIR}"
    rm -rf "${tmp}"
  fi
fi
cd "${AX_DIR}"

make build-binaries
install -m 0755 bin/ax "${LOCAL_BIN}/ax"

make build-task-runner TASK_RUNNER_REPO="${TASK_RUNNER_REPO}"
docker push "${TASK_RUNNER_REPO}:latest"

make deploy AX_IMAGE_REPO="${AX_IMAGE_REPO}"
kubectl -n ax-system rollout status deploy/ax-server --timeout=300s
kubectl -n ax-system get pods
