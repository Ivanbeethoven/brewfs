#!/usr/bin/env bash
# Smoke test: one AX Task on a pre-mounted (durableDir) workspace.
set -euo pipefail
source "$(dirname "$0")/env.sh"

OUT="$(mktemp -d)"
cat >"${OUT}/ax-kind-smoke.yaml" <<EOF
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
  image: ${TASK_RUNNER_REPO}:latest
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

ax apply -f "${OUT}/ax-kind-smoke.yaml"
for _ in $(seq 1 60); do
  phase="$(ax get task brewfs-task -o yaml 2>/dev/null | awk '/^    phase:/{print $2; exit}')"
  [ "${phase:-}" = "Suspended" ] && break
  sleep 2
done
ax resume task brewfs-task || true
for _ in $(seq 1 60); do
  phase="$(ax get task brewfs-task -o yaml 2>/dev/null | awk '/^    phase:/{print $2; exit}')"
  [ "${phase:-}" = "Running" ] && break
  sleep 3
done
ax get tasks
ax describe task brewfs-task | sed -n '1,20p'
