#!/usr/bin/env bash

set -Eeuo pipefail

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

: "${JUICEFS_BIN:?JUICEFS_BIN is required}"
: "${JFS_S3_BUCKET:?JFS_S3_BUCKET is required}"
: "${JFS_S3_REGION:?JFS_S3_REGION is required}"
: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID is required}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY is required}"
: "${JFS_SMALLFILE_COUNT:?JFS_SMALLFILE_COUNT is required}"
: "${JFS_SMALLFILE_SIZE:?JFS_SMALLFILE_SIZE is required}"
: "${JFS_DIR_LEVELS:?JFS_DIR_LEVELS is required}"
: "${JFS_DIRS_PER_LEVEL:?JFS_DIRS_PER_LEVEL is required}"
: "${JFS_FILES_PER_DIR:?JFS_FILES_PER_DIR is required}"

WORK="${JFS_NATIVE_WORK:-/opt/juicefs-native}"
ARTIFACT_DIR="${JFS_NATIVE_ARTIFACT_DIR:-$WORK/artifacts}"
MOUNT_DIR="${JFS_MOUNT_POINT:-/mnt/juicefs-compare}"
CACHE_DIR="${JFS_CACHE_DIR:-$WORK/jfs-cache}"
META_URL="${JFS_META_URL:-redis://127.0.0.1:6379/0}"
VOLUME_NAME="${JFS_VOLUME_NAME:-brewfs-jfs-compare}"
S3_BUCKET_URL="${JFS_BUCKET_URL:-https://${JFS_S3_BUCKET}.oss-${JFS_S3_REGION}.aliyuncs.com}"
PREFETCH_CACHE_SIZE_MIB="${JFS_PREFETCH_CACHE_SIZE_MIB:-4096}"
PREFETCH_BLOCKS="${JFS_PREFETCH_BLOCKS:-16}"

mkdir -p "$WORK" "$ARTIFACT_DIR" "$MOUNT_DIR"
chmod 0755 "$JUICEFS_BIN"

stop_mount() {
    if mountpoint -q "$MOUNT_DIR" 2>/dev/null; then
        "$JUICEFS_BIN" umount "$MOUNT_DIR" >/dev/null 2>&1 \
            || fusermount3 -u "$MOUNT_DIR" >/dev/null 2>&1 \
            || umount -l "$MOUNT_DIR" >/dev/null 2>&1 \
            || true
    fi
}

cleanup() {
    stop_mount
    redis-cli -h 127.0.0.1 shutdown nosave >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

drop_caches() {
    sync
    [[ -w /proc/sys/vm/drop_caches ]] || return 1
    echo 3 >/proc/sys/vm/drop_caches
}

start_redis() {
    if ! redis-cli -h 127.0.0.1 ping >/dev/null 2>&1; then
        redis-server --daemonize yes --save '' --appendonly no --bind 127.0.0.1
    fi
    for _ in $(seq 1 30); do
        redis-cli -h 127.0.0.1 ping 2>/dev/null | grep -q '^PONG$' && return 0
        sleep 1
    done
    die 'Redis did not become ready'
}

format_volume() {
    if "$JUICEFS_BIN" status "$META_URL" >/dev/null 2>&1; then
        log "JuiceFS volume already formatted: $VOLUME_NAME"
        return 0
    fi
    "$JUICEFS_BIN" format "$META_URL" "$VOLUME_NAME" \
        --storage oss \
        --bucket "$S3_BUCKET_URL" \
        --access-key "$AWS_ACCESS_KEY_ID" \
        --secret-key "$AWS_SECRET_ACCESS_KEY" \
        --block-size 4M \
        --compress none \
        --trash-days 0 \
        --no-update
}

prepare_dataset() {
    local marker="$WORK/dataset-ready"
    [[ -f "$marker" ]] && return 0
    stop_mount
    rm -rf -- "$CACHE_DIR"
    local mount_log="$WORK/juicefs-prepare.log"
    set +e
    "$JUICEFS_BIN" mount "$META_URL" "$MOUNT_DIR" \
        --storage oss --bucket "$S3_BUCKET_URL" \
        --buffer-size 1024 --cache-size 0 --prefetch 0 \
        --no-usage-report -d --log "$mount_log" \
        >"$mount_log.command" 2>&1
    local mount_status=$?
    set -e
    if [[ "$mount_status" -ne 0 ]]; then
        cat "$mount_log.command" >&2 || true
        cat "$mount_log" >&2 || true
        die 'JuiceFS prepare mount command failed'
    fi
    for _ in $(seq 1 60); do mountpoint -q "$MOUNT_DIR" && break; sleep 1; done
    mountpoint -q "$MOUNT_DIR" || die 'JuiceFS prepare mount failed'
    python3 - "$MOUNT_DIR" "$JFS_SMALLFILE_COUNT" "$JFS_SMALLFILE_SIZE" \
        "$JFS_DIR_LEVELS" "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR" \
        >"$ARTIFACT_DIR/prepare.log" 2>&1 <<'PY'
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
expected = int(sys.argv[2])
size = int(sys.argv[3])
levels = int(sys.argv[4])
fanout = int(sys.argv[5])
per_leaf = int(sys.argv[6])
if fanout ** levels * per_leaf != expected:
    raise SystemExit('fixture shape mismatch')
data = bytes([0x5A]) * size
started = time.monotonic()
for leaf_index in range(fanout ** levels):
    components = []
    value = leaf_index
    for _ in range(levels):
        components.append(f'd{value % fanout:03d}')
        value //= fanout
    directory = root.joinpath(*reversed(components))
    directory.mkdir(parents=True, exist_ok=True)
    for file_index in range(per_leaf):
        (directory / f'f{file_index:05d}').write_bytes(data)
elapsed = time.monotonic() - started
print(f'juicefs_prepare_summary files={expected} bytes={expected * size} seconds={elapsed:.6f}')
PY
    sync
    stop_mount
    touch "$marker"
}

scrape_metrics() {
    local label="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsS --max-time 3 http://127.0.0.1:9567/metrics \
            >"$ARTIFACT_DIR/metrics-${label}.txt" 2>/dev/null || : >"$ARTIFACT_DIR/metrics-${label}.txt"
    else
        : >"$ARTIFACT_DIR/metrics-${label}.txt"
    fi
}

mount_profile() {
    local profile="$1"
    local cache_size=0
    local prefetch=0
    local buffer_size=0
    if [[ "$profile" == prefetch ]]; then
        cache_size="$PREFETCH_CACHE_SIZE_MIB"
        prefetch="$PREFETCH_BLOCKS"
        buffer_size="$PREFETCH_CACHE_SIZE_MIB"
    fi
    rm -rf -- "$CACHE_DIR"
    mkdir -p "$CACHE_DIR"
    "$JUICEFS_BIN" mount "$META_URL" "$MOUNT_DIR" \
        --storage oss --bucket "$S3_BUCKET_URL" \
        --buffer-size "$buffer_size" --cache-size "$cache_size" --prefetch "$prefetch" \
        --cache-dir "$CACHE_DIR" --max-fuse-io 128K --max-readahead 128M \
        --attr-cache 0s --entry-cache 0s --dir-entry-cache 0s --open-cache 0s \
        --no-usage-report --read-only -d --metrics 127.0.0.1:9567 \
        --log "$WORK/juicefs-${profile}.log" >/dev/null 2>&1
    for _ in $(seq 1 60); do mountpoint -q "$MOUNT_DIR" && break; sleep 1; done
    mountpoint -q "$MOUNT_DIR" || die "JuiceFS $profile mount failed"
}

scan_smallfiles() {
    local profile="$1"
    python3 - "$MOUNT_DIR" "$JFS_SMALLFILE_COUNT" "$JFS_SMALLFILE_SIZE" \
        "$JFS_DIR_LEVELS" "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR" \
        >"$ARTIFACT_DIR/scan-${profile}.log" 2>&1 <<'PY'
import os
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
expected = int(sys.argv[2])
size = int(sys.argv[3])
levels = int(sys.argv[4])
fanout = int(sys.argv[5])
per_leaf = int(sys.argv[6])
started = time.monotonic()
files = directories = errors = logical = payload = checksum = 0
for directory, dirs, names in os.walk(root):
    dirs[:] = sorted(name for name in dirs if name.startswith('d'))
    relative = pathlib.Path(directory).relative_to(root)
    depth = len(relative.parts)
    if depth == 0:
        continue
    directories += 1
    if depth < levels:
        if len(dirs) != fanout or names:
            raise SystemExit(f'internal shape mismatch path={directory}')
        continue
    if depth != levels or len(dirs) != 0 or len(names) != per_leaf:
        raise SystemExit(f'leaf shape mismatch path={directory}')
    for name in sorted(names):
        path = pathlib.Path(directory) / name
        try:
            with path.open('rb') as handle:
                data = handle.read()
            if len(data) != size:
                raise OSError(f'size={len(data)} expected={size}')
            files += 1
            logical += size
            payload += len(data)
            checksum = (checksum + (data[0] if data else 0)) & 0xffffffff
        except OSError as error:
            errors += 1
            print(f'read error path={path} error={error}')
elapsed = time.monotonic() - started
print(f'juicefs_smallfiles_summary files={files} expected={expected} directories={directories} file_size={size} read_mode=full logical_bytes={logical} payload_bytes={payload} errors={errors} checksum={checksum} seconds={elapsed:.6f} files_per_sec={files / elapsed if elapsed else 0:.2f}')
if files != expected or errors:
    raise SystemExit(1)
PY
}

run_profile() {
    local profile="$1"
    stop_mount
    rm -rf -- "$CACHE_DIR"
    drop_caches || die "drop_caches failed before JuiceFS $profile"
    mount_profile "$profile"
    scrape_metrics "${profile}-before"
    local start_ns end_ns status=0
    start_ns="$(date +%s%N)"
    scan_smallfiles "$profile" || status=$?
    end_ns="$(date +%s%N)"
    scrape_metrics "${profile}-after"
    local elapsed_ns=$((end_ns - start_ns))
    printf '%s\t%s\t%.6f\t%s\n' "$profile" \
        "$([[ "$status" -eq 0 ]] && echo pass || echo "fail($status)")" \
        "$(awk -v ns="$elapsed_ns" 'BEGIN { print ns / 1000000000 }')" \
        "$ARTIFACT_DIR/scan-${profile}.log" >>"$ARTIFACT_DIR/perf-summary.tsv"
    stop_mount
    [[ "$status" -eq 0 ]] || return "$status"
}

start_redis
format_volume
prepare_dataset
printf 'profile\tstatus\tseconds\tlog\n' >"$ARTIFACT_DIR/perf-summary.tsv"
status=0
run_profile strict || status=1
run_profile prefetch || status=1
printf 'files=%s file_size=%s levels=%s fanout=%s files_per_leaf=%s\n' \
    "$JFS_SMALLFILE_COUNT" "$JFS_SMALLFILE_SIZE" "$JFS_DIR_LEVELS" \
    "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR"
cat "$ARTIFACT_DIR/perf-summary.tsv"
for log_path in "$ARTIFACT_DIR"/scan-*.log; do
    echo "### $log_path"
    tail -n 4 "$log_path" || true
done
exit "$status"
