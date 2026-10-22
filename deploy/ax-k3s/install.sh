#!/usr/bin/env bash
# End-to-end AX-on-k3s deployment.
#
# k3s runs its own containerd and does not touch Docker. Docker is used only to
# build the AX task-runner image in 05-ax.sh.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

"${here}/00-prereqs.sh"
"${here}/01-k3s.sh"
"${here}/02-registry.sh"
"${here}/03-substrate.sh"
"${here}/04-workerpool.sh"
"${here}/05-ax.sh"
"${here}/06-smoke.sh"

echo "AX-on-k3s smoke completed. Use 'source ${here}/env.sh' then 'ax get tasks'."
