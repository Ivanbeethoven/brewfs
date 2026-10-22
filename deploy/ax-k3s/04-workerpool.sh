#!/usr/bin/env bash
# Create a gVisor WorkerPool (Substrate ships none by default).
set -euo pipefail
source "$(dirname "$0")/env.sh"

cd "${SRC_ROOT}/substrate"

KUBECONFIG="${KUBECONFIG}" KUBECTL_CONTEXT="${KUBECTL_CONTEXT}" \
KO_DOCKER_REPO="${KO_DOCKER_REPO}" BUCKET_NAME="${BUCKET_NAME}" \
  ./hack/install-ate-kind.sh --deploy-demo-sandbox --rollout-timeout 10m

for _ in $(seq 1 120); do
  ready="$(${SUDO} k3s kubectl -n ate-demo-sandbox get workerpools.ate.dev \
    -o jsonpath='{.items[0].status.readyReplicas}' 2>/dev/null || true)"
  if [ "${ready:-0}" -ge 1 ] 2>/dev/null; then
    ${SUDO} k3s kubectl -n ate-demo-sandbox get workerpools.ate.dev
    exit 0
  fi
  sleep 5
done
echo "worker pool did not become ready" >&2
${SUDO} k3s kubectl -n ate-demo-sandbox get pods
exit 1
