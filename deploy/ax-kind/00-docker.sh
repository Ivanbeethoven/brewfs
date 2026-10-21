#!/usr/bin/env bash
# Configure the host Docker daemon to use AX_DEPLOY_PROXY and a docker.io mirror.
# Restarts Docker, so run this before creating the kind cluster.
set -euo pipefail
source "$(dirname "$0")/env.sh"

if [ -z "${AX_DEPLOY_PROXY}" ]; then
  echo "AX_DEPLOY_PROXY is empty; leaving Docker network settings unchanged"
  exit 0
fi

mkdir -p /etc/systemd/system/docker.service.d
cat >/etc/systemd/system/docker.service.d/proxy.conf <<EOF
[Service]
Environment="HTTP_PROXY=${AX_DEPLOY_PROXY}"
Environment="HTTPS_PROXY=${AX_DEPLOY_PROXY}"
Environment="NO_PROXY=localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16"
EOF

if [ ! -f /etc/docker/daemon.json ]; then
  cat >/etc/docker/daemon.json <<'EOF'
{"registry-mirrors":["https://docker.m.daocloud.io"]}
EOF
fi

systemctl daemon-reload
systemctl restart docker
for _ in $(seq 1 30); do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info --format 'docker root={{.DockerRootDir}} proxy-ok'
