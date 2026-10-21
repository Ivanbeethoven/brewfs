#!/usr/bin/env bash
# Install Agent Substrate v0.5.0 into ate-system.
set -euo pipefail
source "$(dirname "$0")/env.sh"

SUBSTRATE_DIR="${SRC_ROOT}/substrate"
cd "${SUBSTRATE_DIR}"

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME}" \
KUBECTL_CONTEXT="${KUBECTL_CONTEXT}" \
  ./hack/install-ate-kind.sh --deploy-ate-system \
    --credential-provider='{"name":"k8s.io"}'

kubectl -n ate-system rollout status deploy/ate-api-server --timeout=300s
kubectl -n ate-system get pods
