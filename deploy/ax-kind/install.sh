#!/usr/bin/env bash
# End-to-end AX-on-kind deployment (no KVM required; gVisor).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

"${here}/01-toolchain.sh"
"${here}/02-cluster.sh"
"${here}/03-substrate.sh"
"${here}/04-workerpool.sh"
"${here}/05-ax.sh"
"${here}/06-smoke.sh"

echo "AX smoke test completed. Use 'source ${here}/env.sh' then 'ax get tasks'."
