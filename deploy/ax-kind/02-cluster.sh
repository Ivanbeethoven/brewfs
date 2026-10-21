#!/usr/bin/env bash
# Create the kind cluster and local registry used by Substrate and AX.
# No /dev/kvm is required: Substrate falls back to gVisor.
set -euo pipefail
source "$(dirname "$0")/env.sh"

SUBSTRATE_DIR="${SRC_ROOT}/substrate"
if [ ! -d "${SUBSTRATE_DIR}/.git" ]; then
  git clone https://github.com/agent-substrate/substrate.git "${SUBSTRATE_DIR}"
fi
cd "${SUBSTRATE_DIR}"
git fetch --tags origin
git checkout "${SUBSTRATE_VERSION}"

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME}" \
  ./hack/create-kind-cluster.sh

kubectl config use-context "${KUBECTL_CONTEXT}"
kubectl get nodes
