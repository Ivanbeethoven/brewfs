#!/usr/bin/env bash
# Configure the host Docker daemon for the kind cluster.
#
# Do NOT put a proxy in the Docker daemon: kind nodes inherit it and then
# kubelet/containerd try to reach the local registry (kind-registry:5000)
# through 127.0.0.1, which is not reachable inside the node. Node pulls use the
# registry mirrors configured by 02-cluster.sh instead.
set -euo pipefail
source "$(dirname "$0")/env.sh"

changed=0
if [ -f /etc/systemd/system/docker.service.d/proxy.conf ]; then
  rm -f /etc/systemd/system/docker.service.d/proxy.conf
  changed=1
fi

if python3 - <<'PY'
import json, os
p = '/etc/docker/daemon.json'
d = {}
if os.path.exists(p):
    try:
        d = json.load(open(p))
    except Exception:
        d = {}
mirror = 'https://docker.m.daocloud.io'
mirrors = d.get('registry-mirrors') or []
if mirror not in mirrors:
    mirrors.append(mirror)
    d['registry-mirrors'] = mirrors
    json.dump(d, open(p, 'w'), indent=2)
    raise SystemExit(0)
raise SystemExit(1)
PY
then
  changed=1
fi

if [ "${changed}" = "1" ]; then
  systemctl daemon-reload
  systemctl restart docker
fi

for _ in $(seq 1 30); do
  docker info >/dev/null 2>&1 && break
  sleep 1
done
docker info --format 'root={{.DockerRootDir}} mirrors={{json .RegistryConfig.Mirrors}}'
