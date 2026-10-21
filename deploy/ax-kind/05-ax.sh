#!/usr/bin/env bash
# Build and deploy AX (fork branch with pre-mounted workspace support).
set -euo pipefail
source "$(dirname "$0")/env.sh"

AX_DIR="${SRC_ROOT}/ax"
if [ ! -d "${AX_DIR}/.git" ]; then
  git clone "${AX_REPO}" "${AX_DIR}"
fi
cd "${AX_DIR}"
git fetch origin
git checkout "${AX_BRANCH}"
git pull --ff-only origin "${AX_BRANCH}" || true

make build-binaries
install -m 0755 bin/ax "${LOCAL_BIN}/ax"

make build-task-runner TASK_RUNNER_REPO="${TASK_RUNNER_REPO}"
docker push "${TASK_RUNNER_REPO}:latest"

make deploy AX_IMAGE_REPO="${AX_IMAGE_REPO}"
kubectl -n ax-system rollout status deploy/ax-server --timeout=300s
kubectl -n ax-system get pods
