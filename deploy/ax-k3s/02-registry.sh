#!/usr/bin/env bash
# Run an in-cluster registry that is reachable as:
#   - localhost:<port> from the node/host (ko pushes, k3s containerd pulls)
#   - kind-registry:5000 from pods (Substrate atelet pulls the guest image)
#
# The Substrate kind profile rewrites localhost:<port> to kind-registry:5000,
# so the Service must be named kind-registry and live in ate-system.
set -euo pipefail
source "$(dirname "$0")/env.sh"

${SUDO} k3s kubectl apply -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: kind-registry
  namespace: ate-system
spec:
  replicas: 1
  selector:
    matchLabels:
      app: kind-registry
  template:
    metadata:
      labels:
        app: kind-registry
    spec:
      hostNetwork: true
      containers:
        - name: registry
          image: registry:3
          env:
            - name: REGISTRY_HTTP_ADDR
              value: "0.0.0.0:${REGISTRY_PORT}"
            - name: REGISTRY_HTTP_DEBUG_ADDR
              value: "0.0.0.0:$((REGISTRY_PORT + 1))"
          ports:
            - containerPort: ${REGISTRY_PORT}
---
apiVersion: v1
kind: Service
metadata:
  name: kind-registry
  namespace: ate-system
spec:
  selector:
    app: kind-registry
  ports:
    - port: 5000
      targetPort: ${REGISTRY_PORT}
EOF

for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null 2>&1; then
    echo "registry ready on ${REGISTRY_PORT}"
    exit 0
  fi
  sleep 2
done
echo "registry did not become ready" >&2
${SUDO} k3s kubectl -n ate-system get pods -l app=kind-registry
exit 1
