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

# kind propagates the caller environment into node containers; a proxy here
# would also be used for local registry pulls and break them.
KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME}" \
  env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
  ./hack/create-kind-cluster.sh

# Registry mirrors for the node's containerd. The mirror host names do not
# follow a single suffix rule (registry.k8s.io -> k8s.m.daocloud.io).
declare -A MIRRORS=(
  [docker.io]=docker.m.daocloud.io
  [gcr.io]=gcr.m.daocloud.io
  [ghcr.io]=ghcr.m.daocloud.io
  [quay.io]=quay.m.daocloud.io
  [registry.k8s.io]=k8s.m.daocloud.io
)
for node in $(kind get nodes --name "${KIND_CLUSTER_NAME}"); do
  for reg in "${!MIRRORS[@]}"; do
    mirror="${MIRRORS[$reg]}"
    docker exec "${node}" mkdir -p "/etc/containerd/certs.d/${reg}"
    docker exec -i "${node}" tee "/etc/containerd/certs.d/${reg}/hosts.toml" >/dev/null <<EOF
server = "https://${reg}"
[host."https://${mirror}"]
  capabilities = ["pull", "resolve"]
EOF
  done
  docker exec "${node}" systemctl restart containerd || true
done

kubectl config use-context "${KUBECTL_CONTEXT}"
kubectl get nodes
