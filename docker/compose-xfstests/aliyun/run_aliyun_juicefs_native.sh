#!/usr/bin/env bash

set -Eeuo pipefail

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

: "${JUICEFS_BIN:?JUICEFS_BIN is required}"
: "${JFS_S3_BUCKET:?JFS_S3_BUCKET is required}"
: "${JFS_S3_REGION:?JFS_S3_REGION is required}"
: "${JFS_S3_ENDPOINT:?JFS_S3_ENDPOINT is required}"
: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID is required}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY is required}"
: "${JFS_RAW_FIXTURE_BIN:?JFS_RAW_FIXTURE_BIN is required}"
: "${JFS_RAW_OBJECT_PREFIX:?JFS_RAW_OBJECT_PREFIX is required}"
: "${JFS_SMALLFILE_COUNT:?JFS_SMALLFILE_COUNT is required}"
: "${JFS_SMALLFILE_SIZE:?JFS_SMALLFILE_SIZE is required}"
: "${JFS_DIR_LEVELS:?JFS_DIR_LEVELS is required}"
: "${JFS_DIRS_PER_LEVEL:?JFS_DIRS_PER_LEVEL is required}"
: "${JFS_FILES_PER_DIR:?JFS_FILES_PER_DIR is required}"

WORK="${JFS_NATIVE_WORK:-/opt/juicefs-native}"
ARTIFACT_DIR="${JFS_NATIVE_ARTIFACT_DIR:-$WORK/artifacts}"
MOUNT_DIR="${JFS_MOUNT_POINT:-/mnt/juicefs-compare}"
CACHE_DIR="${JFS_CACHE_DIR:-$WORK/jfs-cache}"
VOLUME_NAME="${JFS_VOLUME_NAME:-brewfs-jfs-compare}"
META_BACKEND="${JFS_META_BACKEND:-redis}"
TIKV_VERSION="${JFS_TIKV_VERSION:-v6.5.3}"
TIKV_TAG="${JFS_TIKV_TAG:-brewfs-juicefs-tikv}"
TIKV_HOME="${JFS_TIKV_HOME:-$WORK/tiup-home}"
TIKV_PID_FILE="$WORK/tiup-playground.pid"
META_URL="${JFS_META_URL:-}"
S3_ENDPOINT_HOST="${JFS_S3_ENDPOINT#https://}"
S3_ENDPOINT_HOST="${S3_ENDPOINT_HOST#http://}"
DATA_PREFIX="${JFS_DATA_PREFIX:-juicefs-data-${VOLUME_NAME}}"
S3_BUCKET_URL="${JFS_BUCKET_URL:-https://${JFS_S3_BUCKET}.${S3_ENDPOINT_HOST}/${DATA_PREFIX#/}}"
RAW_SOURCE_URL="oss://${AWS_ACCESS_KEY_ID}:${AWS_SECRET_ACCESS_KEY}@${JFS_S3_BUCKET}.${S3_ENDPOINT_HOST}/${JFS_RAW_OBJECT_PREFIX#/}/"
PREFETCH_CACHE_SIZE_MIB="${JFS_PREFETCH_CACHE_SIZE_MIB:-4096}"
PREFETCH_BLOCKS="${JFS_PREFETCH_BLOCKS:-16}"
PERF_TOOLS="${JFS_PERF_TOOLS:-juicefs-smallfiles}"
METADATA_LATENCY_MS="${JFS_METADATA_LATENCY_MS:-0}"
SMALLFILE_MIN_SIZE="${JFS_SMALLFILE_MIN_SIZE:-$JFS_SMALLFILE_SIZE}"
SMALLFILE_MAX_SIZE="${JFS_SMALLFILE_MAX_SIZE:-$JFS_SMALLFILE_SIZE}"
SMALLFILE_WORKERS="${JFS_SMALLFILE_WORKERS:-16}"
TOOL_TIMEOUT_SECONDS="${JFS_TOOL_TIMEOUT_SECONDS:-7200}"
# Match BrewFS' FUSE request and kernel readahead contract for A/B tests.
JFS_MAX_FUSE_IO="${JFS_MAX_FUSE_IO:-4M}"
JFS_MAX_READAHEAD="${JFS_MAX_READAHEAD:-16M}"

case "$META_BACKEND" in
    redis)
        META_URL="${META_URL:-redis://127.0.0.1:6379/0}"
        ;;
    tikv)
        META_URL="${META_URL:-tikv://127.0.0.1:2379}"
        ;;
    *)
        die "unsupported JFS_META_BACKEND: $META_BACKEND (expected redis or tikv)"
        ;;
esac

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
    if [[ "$METADATA_LATENCY_MS" -gt 0 ]]; then
        tc qdisc del dev lo root >/dev/null 2>&1 || true
    fi
    if [[ "$META_BACKEND" == redis ]]; then
        redis-cli -h 127.0.0.1 shutdown nosave >/dev/null 2>&1 || true
    elif [[ "$META_BACKEND" == tikv ]]; then
        if [[ -f "$TIKV_PID_FILE" ]]; then
            kill "$(cat "$TIKV_PID_FILE")" >/dev/null 2>&1 || true
            rm -f "$TIKV_PID_FILE"
        fi
        pkill -TERM -f "$TIKV_HOME" >/dev/null 2>&1 || true
        for _ in $(seq 1 30); do
            pgrep -f "$TIKV_HOME" >/dev/null 2>&1 || break
            sleep 1
        done
        pkill -KILL -f "$TIKV_HOME" >/dev/null 2>&1 || true
        rm -rf -- "$TIKV_HOME"
    fi
}
trap cleanup EXIT INT TERM

apply_metadata_latency() {
    if [[ "$METADATA_LATENCY_MS" -gt 0 ]]; then
        tc qdisc replace dev lo root netem delay "${METADATA_LATENCY_MS}ms"
        printf 'metadata_latency_ms=%s\n' "$METADATA_LATENCY_MS" >>"$ARTIFACT_DIR/resource-proof.env"
    fi
}

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

start_tikv() {
    mkdir -p "$TIKV_HOME"
    export TIUP_HOME="$TIKV_HOME"
    local tiup_bin="$TIKV_HOME/bin/tiup"
    if [[ ! -x "$tiup_bin" ]]; then
        curl --fail --location --retry 5 --connect-timeout 20 \
            --output "$WORK/tiup-install.sh" \
            https://tiup-mirrors.pingcap.com/install.sh
        sh "$WORK/tiup-install.sh" >/"$WORK/tiup-install.log" 2>&1
    fi
    [[ -x "$tiup_bin" ]] || die "TiUP was not installed at $tiup_bin"
    if [[ -f "$TIKV_PID_FILE" ]]; then
        kill "$(cat "$TIKV_PID_FILE")" >/dev/null 2>&1 || true
        rm -f "$TIKV_PID_FILE"
    fi
    "$tiup_bin" playground "$TIKV_VERSION" --mode tikv-slim --tag "$TIKV_TAG" \
        --host 127.0.0.1 --without-monitor \
        >"$WORK/tiup-playground.log" 2>&1 &
    echo $! >"$TIKV_PID_FILE"
    for _ in $(seq 1 240); do
        if curl -fsS --max-time 2 http://127.0.0.1:2379/pd/api/v1/cluster/status \
            >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    curl -fsS --max-time 5 http://127.0.0.1:2379/pd/api/v1/cluster/status \
        >/dev/null 2>&1 || {
        tail -n 120 "$WORK/tiup-playground.log" >&2 || true
        die 'TiKV PD did not become ready'
    }
    for _ in $(seq 1 60); do
        if python3 - <<'PY'
import json
import urllib.request

try:
    with urllib.request.urlopen('http://127.0.0.1:2379/pd/api/v1/stores', timeout=2) as response:
        payload = json.load(response)
    stores = payload.get('stores', [])
    if any(str(store.get('store', {}).get('state_name', '')).lower() in {'up', 'serving'} for store in stores):
        raise SystemExit(0)
except Exception:
    pass
raise SystemExit(1)
PY
        then
            return 0
        fi
        sleep 1
    done
    tail -n 120 "$WORK/tiup-playground.log" >&2 || true
    die 'TiKV store did not become ready'
}

start_metadata() {
    case "$META_BACKEND" in
        redis) start_redis ;;
        tikv) start_tikv ;;
    esac
    printf 'metadata_backend=%s\nmetadata_url=%s\ntikv_version=%s\ndata_prefix=%s\n' \
        "$META_BACKEND" "$META_URL" "$TIKV_VERSION" "$DATA_PREFIX" \
        >"$ARTIFACT_DIR/metadata-proof.env"
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
    local marker_suffix="${VOLUME_NAME//[^A-Za-z0-9_.-]/_}"
    local marker="$WORK/dataset-ready-$marker_suffix"
    [[ -f "$marker" ]] && return 0
    stop_mount
    rm -rf -- "$CACHE_DIR" "$WORK/raw-manifest-key.txt"
    if [[ "${JFS_SKIP_RAW_UPLOAD:-false}" == true ]]; then
        log "reusing deterministic raw fixture prefix: $JFS_RAW_OBJECT_PREFIX"
    else
        log "uploading deterministic raw files with SDK"
        "$JFS_RAW_FIXTURE_BIN" \
            --bucket "$JFS_S3_BUCKET" \
            --endpoint "$JFS_S3_ENDPOINT" \
            --region "$JFS_S3_REGION" \
            --prefix "$JFS_RAW_OBJECT_PREFIX" \
            --dir-levels "$JFS_DIR_LEVELS" \
            --dirs-per-level "$JFS_DIRS_PER_LEVEL" \
            --files-per-dir "$JFS_FILES_PER_DIR" \
            --small-file-size "$JFS_SMALLFILE_SIZE" \
            --small-file-min-size "$SMALLFILE_MIN_SIZE" \
            --small-file-max-size "$SMALLFILE_MAX_SIZE" \
            --raw-only true \
            --manifest-output "$WORK/raw-manifest-key.txt" \
            >"$ARTIFACT_DIR/raw-upload.log" 2>&1
        [[ -s "$WORK/raw-manifest-key.txt" ]] || die 'raw SDK uploader did not produce a manifest key'
    fi
    log "importing raw OSS objects through JuiceFS sync without FUSE"
    myfs="$META_URL" "$JUICEFS_BIN" sync \
        --threads 32 --list-threads 4 \
        --check-new --exclude='raw-manifest.tsv' \
        "$RAW_SOURCE_URL" "jfs://myfs/" \
        >"$ARTIFACT_DIR/juicefs-sync.log" 2>&1
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
    printf 'cache_size_mib=%s\nprefetch_blocks=%s\nmax_fuse_io=%s\nmax_readahead=%s\n' \
        "$cache_size" "$prefetch" "$JFS_MAX_FUSE_IO" "$JFS_MAX_READAHEAD" \
        >"$ARTIFACT_DIR/mount-${profile}.env"
    "$JUICEFS_BIN" mount "$META_URL" "$MOUNT_DIR" \
        --storage oss --bucket "$S3_BUCKET_URL" \
        --buffer-size "$buffer_size" --cache-size "$cache_size" --prefetch "$prefetch" \
        --cache-dir "$CACHE_DIR" --max-fuse-io "$JFS_MAX_FUSE_IO" --max-readahead "$JFS_MAX_READAHEAD" \
        --attr-cache 0s --entry-cache 0s --dir-entry-cache 0s --open-cache 0s \
        --no-usage-report --read-only -d --metrics 127.0.0.1:9567 \
        --log "$WORK/juicefs-${profile}.log" >/dev/null 2>&1
    for _ in $(seq 1 60); do mountpoint -q "$MOUNT_DIR" && break; sleep 1; done
    mountpoint -q "$MOUNT_DIR" || die "JuiceFS $profile mount failed"
}

scan_smallfiles() {
    local profile="$1"
    local log_suffix="${2:-smallfiles}"
    python3 - "$MOUNT_DIR" "$JFS_SMALLFILE_COUNT" "$SMALLFILE_MIN_SIZE" "$SMALLFILE_MAX_SIZE" \
        "$JFS_DIR_LEVELS" "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR" "$SMALLFILE_WORKERS" \
        >"$ARTIFACT_DIR/scan-${profile}-${log_suffix}.log" 2>&1 <<'PY'
import os
import pathlib
import sys
import time
from concurrent.futures import ThreadPoolExecutor

root = pathlib.Path(sys.argv[1])
expected = int(sys.argv[2])
min_size = int(sys.argv[3])
max_size = int(sys.argv[4])
levels = int(sys.argv[5])
fanout = int(sys.argv[6])
per_leaf = int(sys.argv[7])
workers = max(1, int(sys.argv[8]))
started = time.monotonic()
files = directories = errors = logical = payload = checksum = 0
file_specs = []
for directory, dirs, names in os.walk(root):
    dirs[:] = sorted(name for name in dirs if name.startswith('d'))
    names = sorted(name for name in names if not name.startswith('.') and name != 'lost+found')
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
        leaf_index = 0
        for component in pathlib.Path(directory).relative_to(root).parts:
            leaf_index = leaf_index * fanout + int(component[1:])
        file_index = int(name[1:])
        global_file_index = leaf_index * per_leaf + file_index
        span = max_size - min_size + 1
        mixed = (global_file_index * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        file_specs.append((path, global_file_index, min_size + (mixed % span if span else 0)))

def read_one(spec):
    path, global_file_index, expected_size = spec
    try:
        with path.open('rb') as handle:
            data = handle.read()
        if len(data) != expected_size:
            raise OSError(f'size={len(data)} expected={expected_size}')
        seed = (global_file_index + 1).to_bytes(8, 'little')
        expected_header = bytes(
            seed[offset % len(seed)] ^ (((offset << 7) | (offset >> 57)) & 0xff)
            for offset in range(min(8, expected_size))
        )
        if data[:len(expected_header)] != expected_header:
            raise OSError('file content pattern mismatch')
        return ('ok', expected_size, len(data), (data[0] if data else 0))
    except OSError as error:
        return ('error', str(error))

with ThreadPoolExecutor(max_workers=workers) as executor:
    for spec, result in zip(file_specs, executor.map(read_one, file_specs)):
        if result[0] == 'ok':
            _, expected_size, data_len, first_byte = result
            files += 1
            logical += expected_size
            payload += data_len
            checksum = (checksum + first_byte) & 0xffffffff
        else:
            errors += 1
            print(f'read error path={spec[0]} error={result[1]}')
elapsed = time.monotonic() - started
print(f'juicefs_smallfiles_summary files={files} expected={expected} directories={directories} min_file_size={min_size} max_file_size={max_size} read_mode=full workers={workers} logical_bytes={logical} payload_bytes={payload} errors={errors} checksum={checksum} seconds={elapsed:.6f} files_per_sec={files / elapsed if elapsed else 0:.2f}')
if files != expected or errors:
    raise SystemExit(1)
PY
}

scan_tree() {
    local profile="$1"
    local log_suffix="${2:-tree}"
    python3 - "$MOUNT_DIR" "$JFS_SMALLFILE_COUNT" \
        "$JFS_DIR_LEVELS" "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR" \
        >"$ARTIFACT_DIR/scan-${profile}-${log_suffix}.log" 2>&1 <<'PY'
import os
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
expected_files = int(sys.argv[2])
levels = int(sys.argv[3])
fanout = int(sys.argv[4])
per_leaf = int(sys.argv[5])
expected_leaf_dirs = fanout ** levels
expected_tree_dirs = sum(fanout ** level for level in range(1, levels + 1))
started = time.monotonic()
files = directories = leaf_dirs = 0
for directory, dirs, names in os.walk(root):
    raw_dirs = list(dirs)
    raw_names = list(names)
    dirs[:] = sorted(name for name in dirs if name.startswith('d'))
    names = sorted(name for name in names if not name.startswith('.') and name != 'lost+found')
    relative = pathlib.Path(directory).relative_to(root)
    depth = len(relative.parts)
    if depth == 0:
        if len(dirs) != fanout or names:
            raise SystemExit(f'root shape mismatch path={directory} raw_dirs={raw_dirs!r} raw_names={raw_names!r} filtered_dirs={dirs!r} filtered_names={names!r}')
        continue
    directories += 1
    if depth < levels:
        if len(dirs) != fanout or names:
            raise SystemExit(f'internal shape mismatch path={directory}')
        continue
    if depth != levels or len(dirs) != 0 or len(names) != per_leaf:
        raise SystemExit(f'leaf shape mismatch path={directory}')
    leaf_dirs += 1
    files += len(names)
elapsed = time.monotonic() - started
print(f'juicefs_tree_summary files={files} expected={expected_files} directories={directories} expected_directories={expected_tree_dirs} leaf_directories={leaf_dirs} expected_leaf_directories={expected_leaf_dirs} seconds={elapsed:.6f} files_per_sec={files / elapsed if elapsed else 0:.2f}')
if files != expected_files or directories != expected_tree_dirs or leaf_dirs != expected_leaf_dirs:
    raise SystemExit(1)
PY
}

scan_stat() {
    local profile="$1"
    local log_suffix="${2:-stat}"
    local scanner="${JFS_SMALLFILES_SCANNER:-}"
    [[ -n "$scanner" && -x "$scanner" ]] || die "JFS_SMALLFILES_SCANNER must point to tools/perf/smallfiles_scan.py"
    "$scanner" \
        --root "$MOUNT_DIR" \
        --label juicefs-stat \
        --mode stat \
        --expected-files "$JFS_SMALLFILE_COUNT" \
        --min-size "$SMALLFILE_MIN_SIZE" \
        --max-size "$SMALLFILE_MAX_SIZE" \
        --dir-levels "$JFS_DIR_LEVELS" \
        --dirs-per-level "$JFS_DIRS_PER_LEVEL" \
        --files-per-leaf "$JFS_FILES_PER_DIR" \
        --workers "$SMALLFILE_WORKERS" \
        --json-output "$ARTIFACT_DIR/scan-${profile}-${log_suffix}.json" \
        >"$ARTIFACT_DIR/scan-${profile}-${log_suffix}.log" 2>&1
}

scan_shared() {
    local profile="$1"
    local log_suffix="$2"
    local mode="$3"
    shift 3
    local scanner="${JFS_SMALLFILES_SCANNER:-}"
    [[ -n "$scanner" && -x "$scanner" ]] || die "JFS_SMALLFILES_SCANNER must point to tools/perf/smallfiles_scan.py"
    "$scanner" \
        --root "$MOUNT_DIR" \
        --label "juicefs-${META_BACKEND}-${log_suffix}" \
        --mode "$mode" \
        --expected-files "$JFS_SMALLFILE_COUNT" \
        --min-size "$SMALLFILE_MIN_SIZE" \
        --max-size "$SMALLFILE_MAX_SIZE" \
        --dir-levels "$JFS_DIR_LEVELS" \
        --dirs-per-level "$JFS_DIRS_PER_LEVEL" \
        --files-per-leaf "$JFS_FILES_PER_DIR" \
        --workers "$SMALLFILE_WORKERS" \
        "$@" \
        --json-output "$ARTIFACT_DIR/scan-${profile}-${log_suffix}.json" \
        >"$ARTIFACT_DIR/scan-${profile}-${log_suffix}.log" 2>&1
}

scan_gpu() {
    local profile="$1"
    scan_shared "$profile" gpu-smallfiles full \
        --order shuffle \
        --shuffle-seed "${PERF_GPU_SHUFFLE_SEED:-20261001}" \
        --epochs "${PERF_GPU_EPOCHS:-2}" \
        --batch-size "${PERF_GPU_BATCH_SIZE:-256}" \
        --max-inflight-batches "${PERF_GPU_MAX_INFLIGHT_BATCHES:-2}"
}

snapshot_metadata_backend() {
    local label="$1"
    local output="$ARTIFACT_DIR/metadata-${label}.env"
    case "$META_BACKEND" in
        redis)
            {
                printf 'backend=redis\n'
                redis-cli -h 127.0.0.1 INFO commandstats | awk -F'[:,=]' '/^cmdstat_/ { calls += $3 } END { printf "command_calls_total=%d\n", calls }'
                redis-cli -h 127.0.0.1 INFO memory | awk -F: '/^used_memory:/ { gsub("\\r", "", $2); print "used_memory_bytes=" $2 }'
                redis-cli -h 127.0.0.1 DBSIZE | awk '{ print "keys=" $1 }'
            } >"$output"
            ;;
        tikv)
            {
                printf 'backend=tikv\n'
                curl -fsS --max-time 5 http://127.0.0.1:2379/pd/api/v1/cluster/status \
                    | python3 -c 'import json,sys; d=json.load(sys.stdin); print("pd_status=" + json.dumps(d, sort_keys=True, separators=(",", ":")))' \
                    || printf 'pd_status=unavailable\n'
                curl -fsS --max-time 5 http://127.0.0.1:2379/pd/api/v1/stores \
                    | python3 -c 'import json,sys; d=json.load(sys.stdin); print("store_count=" + str(len(d.get("stores", []))))' \
                    || printf 'store_count=unavailable\n'
                curl -fsS --max-time 5 http://127.0.0.1:20180/metrics \
                    | awk '/^tikv_storage_command_total[{ ]/ {sum += $NF} END {printf "storage_command_total=%.0f\n", sum}' \
                    || printf 'storage_command_total=unavailable\n'
            } >"$output"
            ;;
    esac
}

run_with_timeout() {
    local log_path="$1"
    shift
    "$@" >"$log_path" 2>&1 &
    local command_pid=$!
    local deadline=$((SECONDS + TOOL_TIMEOUT_SECONDS))
    while kill -0 "$command_pid" 2>/dev/null; do
        if (( SECONDS >= deadline )); then
            kill -TERM "$command_pid" 2>/dev/null || true
            sleep 10
            kill -KILL "$command_pid" 2>/dev/null || true
            wait "$command_pid" 2>/dev/null || true
            return 124
        fi
        sleep 1
    done
    wait "$command_pid"
}

run_tool_profile() {
    local profile="$1"
    local tool="$2"
    stop_mount
    rm -rf -- "$CACHE_DIR"
    drop_caches || die "drop_caches failed before JuiceFS $profile"
    mount_profile "$profile"
    local cache_bytes_before
    cache_bytes_before="$(du -sb "$CACHE_DIR" 2>/dev/null | awk '{print $1}')"
    cache_bytes_before="${cache_bytes_before:-0}"
    local cache_files_before
    cache_files_before="$(find "$CACHE_DIR" -type f -printf '%p\n' 2>/dev/null | wc -l)"
    cache_files_before="${cache_files_before:-0}"
    scrape_metrics "${profile}-${tool}-before"
    snapshot_metadata_backend "${profile}-${tool}-before"
    local start_ns end_ns drain_start_ns drain_end_ns drain_ns status=0
    start_ns="$(date +%s%N)"
    case "$tool" in
        juicefs-tree) run_with_timeout "$ARTIFACT_DIR/scan-${profile}-tree.log" scan_shared "$profile" tree tree || status=$? ;;
        juicefs-stat) run_with_timeout "$ARTIFACT_DIR/scan-${profile}-stat.log" scan_shared "$profile" stat stat || status=$? ;;
        juicefs-smallfiles) run_with_timeout "$ARTIFACT_DIR/scan-${profile}-smallfiles.log" scan_shared "$profile" smallfiles full || status=$? ;;
        juicefs-gpu-smallfiles) run_with_timeout "$ARTIFACT_DIR/scan-${profile}-gpu-smallfiles.log" scan_gpu "$profile" || status=$? ;;
        *) die "unsupported JuiceFS tool: $tool" ;;
    esac
    end_ns="$(date +%s%N)"
    snapshot_metadata_backend "${profile}-${tool}-after"
    scrape_metrics "${profile}-${tool}-after"
    local cache_bytes_after
    cache_bytes_after="$(du -sb "$CACHE_DIR" 2>/dev/null | awk '{print $1}')"
    cache_bytes_after="${cache_bytes_after:-0}"
    local cache_files_after
    cache_files_after="$(find "$CACHE_DIR" -type f -printf '%p\n' 2>/dev/null | wc -l)"
    cache_files_after="${cache_files_after:-0}"
    printf 'profile=%s tool=%s cache_dir=%s cache_bytes_before=%s cache_bytes_after=%s cache_files_before=%s cache_files_after=%s\n' \
        "$profile" "$tool" "$CACHE_DIR" "$cache_bytes_before" "$cache_bytes_after" \
        "$cache_files_before" "$cache_files_after" \
        >>"$ARTIFACT_DIR/cache-proof.env"
    local elapsed_ns=$((end_ns - start_ns))
    drain_start_ns="$(date +%s%N)"
    stop_mount
    drain_end_ns="$(date +%s%N)"
    drain_ns=$((drain_end_ns - drain_start_ns))
    printf '%s\t%s\t%.6f\t%.6f\t%.6f\t%s\n' "${profile}-${tool}" \
        "$([[ "$status" -eq 0 ]] && echo pass || echo "fail($status)")" \
        "$(awk -v ns="$elapsed_ns" 'BEGIN { print ns / 1000000000 }')" \
        "$(awk -v ns="$drain_ns" 'BEGIN { print ns / 1000000000 }')" \
        "$(awk -v a="$elapsed_ns" -v d="$drain_ns" 'BEGIN { print (a + d) / 1000000000 }')" \
        "$ARTIFACT_DIR/scan-${profile}-${tool#juicefs-}.log" >>"$ARTIFACT_DIR/perf-summary.tsv"
    [[ "$status" -eq 0 ]] || return "$status"
}

run_profile() {
    local profile="$1"
    local tool status=0
    for tool in $PERF_TOOLS; do
        run_tool_profile "$profile" "$tool" || status=1
    done
    return "$status"
}

start_metadata
format_volume
prepare_dataset
apply_metadata_latency
printf 'profile\tstatus\tactive_seconds\tdrain_seconds\tactive_plus_drain_seconds\tlog\n' >"$ARTIFACT_DIR/perf-summary.tsv"
status=0
run_profile strict || status=1
printf 'files=%s file_size=%s-%s levels=%s fanout=%s files_per_leaf=%s cache=0 prefetch=0\n' \
    "$JFS_SMALLFILE_COUNT" "$SMALLFILE_MIN_SIZE" "$SMALLFILE_MAX_SIZE" "$JFS_DIR_LEVELS" \
    "$JFS_DIRS_PER_LEVEL" "$JFS_FILES_PER_DIR"
printf '%s\n' '--- JuiceFS mount proof ---'
cat "$ARTIFACT_DIR/mount-strict.env" "$ARTIFACT_DIR/cache-proof.env" 2>/dev/null || true
cat "$ARTIFACT_DIR/perf-summary.tsv"
for log_path in "$ARTIFACT_DIR"/scan-*.log; do
    echo "### $log_path"
    tail -n 4 "$log_path" || true
done
exit "$status"
