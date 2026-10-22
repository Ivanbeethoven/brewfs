#!/usr/bin/env bash
# Smoke: create an AX Task on a pre-mounted workspace and verify it runs.
set -euo pipefail
source "$(dirname "$0")/env.sh"

export KUBECONFIG="${KUBECONFIG}"

digest="$(docker push "${TASK_RUNNER_REPO}:latest" 2>&1 |
  sed -n 's/.*digest: \(sha256:[0-9a-f]*\).*/\1/p' | tail -1)"
if [ -z "${digest}" ]; then
  echo "could not resolve the task-runner image digest" >&2
  exit 1
fi
image="${TASK_RUNNER_REPO}@${digest}"

tmp="$(mktemp -d)"
cat >"${tmp}/smoke.yaml" <<EOF
apiVersion: ax.io/v1alpha1
kind: Workspace
metadata:
  name: brewfs-mounted
spec: {}
---
apiVersion: ax.io/v1alpha1
kind: Task
metadata:
  name: brewfs-task
spec:
  image: ${image}
  command:
    - /bin/sh
    - -ec
    - echo task-ready > /workspace/task-ready; sleep 3600
  env:
    - name: AX_PREMOUNTED_WORKSPACE
      value: "1"
  workspace:
    name: brewfs-mounted
    path: /workspace
  debug: true
EOF

ax delete task brewfs-task >/dev/null 2>&1 || true
ax apply -f "${tmp}/smoke.yaml"
sleep 5
ax resume task brewfs-task

for _ in $(seq 1 60); do
  phase="$(ax get task brewfs-task -o yaml 2>/dev/null | awk '/^    phase:/{print $2; exit}')"
  [ "${phase:-}" = "Running" ] && break
  sleep 3
done

ax describe task brewfs-task | sed -n '1,20p'
ax ssh brewfs-task -- cat /workspace/task-ready
ax ssh brewfs-task -- ls -la /workspace
