#!/usr/bin/env bash
# Install Agent Substrate v0.5.0 into ate-system on k3s.
#
# The kind profile is reused: it deploys in-cluster PostgreSQL, RustFS, and
# OpenTelemetry, which is what a single-node k3s host needs. The registry name
# is provided by 02-registry.sh.
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
    # hack/kind.sh expects a git repository root; give the tarball one.
    (cd "${SUBSTRATE_DIR}" && git init -q && git add -A &&
      git -c user.email=deploy@local -c user.name=deploy commit -qm snapshot)
  fi
fi
cd "${SUBSTRATE_DIR}"

KUBECONFIG="${KUBECONFIG}" KUBECTL_CONTEXT="${KUBECTL_CONTEXT}" \
KO_DOCKER_REPO="${KO_DOCKER_REPO}" BUCKET_NAME="${BUCKET_NAME}" \
  ./hack/install-ate-kind.sh --deploy-ate-system --rollout-timeout 10m \
    --credential-provider='{"name":"k8s.io"}'

${SUDO} k3s kubectl -n ate-system get pods
