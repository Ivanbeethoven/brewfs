#!/usr/bin/env bash
# Create a gVisor WorkerPool (Substrate ships no worker pool by default).
set -euo pipefail
source "$(dirname "$0")/env.sh"

SUBSTRATE_DIR="${SRC_ROOT}/substrate"
cd "${SUBSTRATE_DIR}"

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME}" \
KUBECTL_CONTEXT="${KUBECTL_CONTEXT}" \
  ./hack/install-ate-kind.sh --deploy-demo-sandbox \
    --rollout-timeout 10m

for _ in $(seq 1 120); do
  ready="$(kubectl -n ate-demo-sandbox get workerpools.ate.dev -o jsonpath='{.items[0].status.readyReplicas}' 2>/dev/null || true)"
  if [ "${ready:-0}" -ge 1 ] 2>/dev/null; then
    kubectl -n ate-demo-sandbox get workerpools.ate.dev
    exit 0
  fi
  sleep 5
done
echo "worker pool did not become ready" >&2
kubectl -n ate-demo-sandbox get pods
exit 1
