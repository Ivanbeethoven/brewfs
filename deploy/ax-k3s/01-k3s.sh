#!/usr/bin/env bash
# Install k3s and configure it for Substrate/AX.
#
# k3s uses its own containerd, so it does not touch Docker or containers that
# other workloads (benchmarks, evaluations) run on the host.
set -euo pipefail
source "$(dirname "$0")/env.sh"

${SUDO} mkdir -p /etc/rancher/k3s
cat >"${TMPDIR:-/tmp}/k3s-config.yaml" <<EOF
write-kubeconfig-mode: "0644"
data-dir: ${K3S_DATA_DIR}
disable:
  - traefik
  - servicelb
  - metrics-server
kube-apiserver-arg:
  - feature-gates=ClusterTrustBundle=true,ClusterTrustBundleProjection=true,PodCertificateRequest=true
  - runtime-config=certificates.k8s.io/v1beta1=true
kubelet-arg:
  - eviction-hard=imagefs.available<5%,nodefs.available<5%
EOF
${SUDO} cp "${TMPDIR:-/tmp}/k3s-config.yaml" /etc/rancher/k3s/config.yaml

# A registry reachable from the host and from pods (see 02-registry.sh).
${SUDO} tee /etc/rancher/k3s/registries.yaml >/dev/null <<EOF
mirrors:
  "localhost:${REGISTRY_PORT}":
    endpoint:
      - "http://localhost:${REGISTRY_PORT}"
  "kind-registry:5000":
    endpoint:
      - "http://kind-registry:5000"
EOF

if ! command -v k3s >/dev/null 2>&1; then
  K3S_INSTALL_URL="${K3S_INSTALL_URL:-https://get.k3s.io}"
  echo "==> installing k3s from ${K3S_INSTALL_URL}"
  curl -sfL "${K3S_INSTALL_URL}" | ${SUDO} env INSTALL_K3S_BIN_DIR=/usr/local/bin sh -
else
  ${SUDO} systemctl restart k3s
fi

for _ in $(seq 1 90); do
  ${SUDO} k3s kubectl get nodes >/dev/null 2>&1 && break
  sleep 2
done
${SUDO} k3s kubectl get nodes
