#!/usr/bin/env bash
# Create the kind cluster and local registry used by Substrate and AX.
# No /dev/kvm is required: Substrate falls back to gVisor.
set -euo pipefail
source "$(dirname "$0")/env.sh"

SUBSTRATE_DIR="${SRC_ROOT}/substrate"
if [ ! -f "${SUBSTRATE_DIR}/hack/install-ate-kind.sh" ]; then
  if ! git clone --depth 1 --branch "${SUBSTRATE_VERSION}" \
      https://github.com/agent-substrate/substrate.git "${SUBSTRATE_DIR}"; then
    echo "==> git clone failed; downloading the ${SUBSTRATE_VERSION} tarball"
    tmp="$(mktemp -d)"
    fetch_github "https://github.com/agent-substrate/substrate/archive/refs/tags/${SUBSTRATE_VERSION}.tar.gz" "${tmp}/sub.tgz"
    tar -C "${tmp}" -xzf "${tmp}/sub.tgz"
    rm -rf "${SUBSTRATE_DIR}"
    mv "${tmp}/substrate-${SUBSTRATE_VERSION#v}" "${SUBSTRATE_DIR}"
    rm -rf "${tmp}"
  fi
fi
cd "${SUBSTRATE_DIR}"
if [ -d .git ]; then
  git fetch --tags origin || true
  git checkout "${SUBSTRATE_VERSION}" || true
fi

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME}" \
  ./hack/create-kind-cluster.sh

kubectl config use-context "${KUBECTL_CONTEXT}"
kubectl get nodes
