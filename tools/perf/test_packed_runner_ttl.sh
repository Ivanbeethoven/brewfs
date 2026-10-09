#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -r -- "$WORK"' EXIT
python3 - "$ROOT/docker/compose-xfstests/aliyun/run_aliyun_packed_native.sh" "$WORK/start-mount.sh" <<'PY'
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text()
start = text.index('start_mount() {')
end = text.index('\nrun_tool() {', start)
Path(sys.argv[2]).write_text(text[start:end])
PY
printf '#!/usr/bin/env bash\nprintf "effective_ttl_ms=%%s\\n" "${BREWFS_CACHE_TTL_MS-unset}"\n' >"$WORK/brewfs"
chmod +x "$WORK/brewfs"
source "$WORK/start-mount.sh"
write_config() { :; }
findmnt() { printf 'fuse.brewfs\n'; }
BREWFS_BIN="$WORK/brewfs"
MOUNT_DIR="$WORK/mnt"
CONFIG_PATH="$WORK/config"
ARTIFACT_DIR="$WORK/artifacts"
mkdir -p "$ARTIFACT_DIR/tools" "$MOUNT_DIR"
for ttl in 0 1000 60000; do
    CURRENT_TOOL="ttl-$ttl"
    METADATA_CACHE_TTL_MS="$ttl"
    export BREWFS_CACHE_TTL_MS=777
    start_mount
    wait "$BREWFS_PID"
    grep -qx "effective_ttl_ms=$ttl" "$ARTIFACT_DIR/tools/${CURRENT_TOOL}-brewfs.log"
done
printf 'packed FUSE TTL forwarding: passed\n'
