#!/usr/bin/env bash

set -Eeuo pipefail

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

: "${BREWFS_BIN:?BREWFS_BIN is required}"
: "${PACKED_FIXTURE_BIN:?PACKED_FIXTURE_BIN is required}"
: "${BREWFS_S3_BUCKET:?BREWFS_S3_BUCKET is required}"
: "${BREWFS_S3_ENDPOINT:?BREWFS_S3_ENDPOINT is required}"
: "${BREWFS_S3_REGION:?BREWFS_S3_REGION is required}"
: "${PACKED_SMALLFILE_COUNT:?PACKED_SMALLFILE_COUNT is required}"
: "${PACKED_SMALLFILE_SIZE:?PACKED_SMALLFILE_SIZE is required}"
: "${PACKED_DIR_LEVELS:?PACKED_DIR_LEVELS is required}"
: "${PACKED_DIRS_PER_LEVEL:?PACKED_DIRS_PER_LEVEL is required}"
: "${PACKED_FILES_PER_DIR:?PACKED_FILES_PER_DIR is required}"

WORK="${BREWFS_NATIVE_WORK:-/opt/brewfs-native}"
ARTIFACT_DIR="${BREWFS_NATIVE_ARTIFACT_DIR:-$WORK/artifacts}"
MOUNT_DIR="${BREWFS_MOUNT_POINT:-/mnt/brewfs-packed}"
CONFIG_PATH="$WORK/mount.yaml"
CACHE_ROOT="${BREWFS_CACHE_ROOT:-$WORK/cache}"
FIXTURE_PREFIX="${PACKED_FIXTURE_PREFIX:-brewfs-packed-native-$(date +%s)}"
FIXTURE_MANIFEST="$WORK/manifest-key.txt"
FIO_FILE_SIZE="${PERF_PACKED_FIO_FILE_SIZE:-67108864}"
READ_BYTES="${PERF_PACKED_SMALLFILE_READ_BYTES:-0}"
TOOLS="${PERF_TOOLS:-packed-smallfiles packed-posix fio-seqread fio-randread}"
FIO_RUNTIME="${PERF_FIO_RUNTIME:-20}"
FORCE_PATH_STYLE="${BREWFS_S3_FORCE_PATH_STYLE:-false}"

mkdir -p "$WORK" "$ARTIFACT_DIR/tools" "$MOUNT_DIR"
chmod 0755 "$BREWFS_BIN" "$PACKED_FIXTURE_BIN"

BREWFS_PID=""
stop_mount() {
    if [[ -n "$BREWFS_PID" ]] && kill -0 "$BREWFS_PID" 2>/dev/null; then
        kill "$BREWFS_PID" 2>/dev/null || true
        wait "$BREWFS_PID" 2>/dev/null || true
    fi
    BREWFS_PID=""
    while findmnt -rn --target "$MOUNT_DIR" --output FSTYPE 2>/dev/null | grep -Eq '^fuse(\.|$)'; do
        fusermount3 -u "$MOUNT_DIR" >/dev/null 2>&1 \
            || umount -l "$MOUNT_DIR" >/dev/null 2>&1 \
            || sleep 1
    done
}

cleanup() {
    stop_mount
    rm -rf -- "$CACHE_ROOT"
}
trap cleanup EXIT INT TERM

drop_caches() {
    sync
    if [[ ! -w /proc/sys/vm/drop_caches ]]; then
        return 1
    fi
    echo 3 >/proc/sys/vm/drop_caches
}

write_config() {
    cat >"$CONFIG_PATH" <<EOF
mount_point: $MOUNT_DIR
volume_format: packed-metadata-v1
packed_manifest_key: $(cat "$FIXTURE_MANIFEST")

data:
  backend: s3
  s3:
    bucket: $BREWFS_S3_BUCKET
    endpoint: $BREWFS_S3_ENDPOINT
    region: $BREWFS_S3_REGION
    part_size: 16777216
    max_concurrency: 32
    force_path_style: $FORCE_PATH_STYLE
    disable_payload_checksum: true

layout:
  chunk_size: 67108864
  block_size: 4194304

fuse:
  workers: 16
  max_background: 512
  privileged: true

cache:
  root: $CACHE_ROOT
  read_memory_bytes: 0
  read_ssd_bytes: 0
  prefetch_enabled: false
  range_background_prefetch: false
  compression: none
EOF
}

start_mount() {
    write_config
    mkdir -p "$MOUNT_DIR"
    "$BREWFS_BIN" mount --privileged --config "$CONFIG_PATH" "$MOUNT_DIR" \
        >"$ARTIFACT_DIR/brewfs.log" 2>&1 &
    BREWFS_PID=$!
    local deadline=$((SECONDS + 90))
    while (( SECONDS < deadline )); do
        if findmnt -rn --target "$MOUNT_DIR" --output FSTYPE 2>/dev/null | grep -Eq '^fuse(\.|$)'; then
            return 0
        fi
        if ! kill -0 "$BREWFS_PID" 2>/dev/null; then
            wait "$BREWFS_PID" || true
            tail -n 80 "$ARTIFACT_DIR/brewfs.log" >&2 || true
            return 1
        fi
        sleep 1
    done
    return 1
}

run_tool() {
    local name="$1"
    shift
    local log_path="$ARTIFACT_DIR/tools/$name.log"
    local start_ns end_ns elapsed_ns status=0
    stop_mount
    rm -rf -- "$CACHE_ROOT"
    drop_caches || die "drop_caches failed before $name; refusing to report a cached read"
    start_mount || die "BrewFS mount failed before $name"
    start_ns="$(date +%s%N)"
    "$@" >"$log_path" 2>&1 || status=$?
    end_ns="$(date +%s%N)"
    elapsed_ns=$((end_ns - start_ns))
    stop_mount
    printf '%s\t%s\t%.6f\t%s\n' "$name" "$([[ "$status" -eq 0 ]] && echo pass || echo "fail($status)")" \
        "$(awk -v ns="$elapsed_ns" 'BEGIN { print ns / 1000000000 }')" "$log_path" \
        >>"$ARTIFACT_DIR/perf-summary.tsv"
    if [[ "$status" -ne 0 ]]; then
        tail -n 30 "$log_path" >&2 || true
        return "$status"
    fi
}

publish_fixture() {
    log "publishing immutable packed metadata fixture to OSS"
    "$PACKED_FIXTURE_BIN" \
        --bucket "$BREWFS_S3_BUCKET" \
        --endpoint "$BREWFS_S3_ENDPOINT" \
        --region "$BREWFS_S3_REGION" \
        --prefix "$FIXTURE_PREFIX" \
        --dir-levels "$PACKED_DIR_LEVELS" \
        --dirs-per-level "$PACKED_DIRS_PER_LEVEL" \
        --files-per-dir "$PACKED_FILES_PER_DIR" \
        --small-file-size "$PACKED_SMALLFILE_SIZE" \
        --fio-file-size "$FIO_FILE_SIZE" \
        --force-path-style "$FORCE_PATH_STYLE" \
        --manifest-output "$FIXTURE_MANIFEST" \
        >"$ARTIFACT_DIR/packed-fixture.log" 2>&1
    [[ -s "$FIXTURE_MANIFEST" ]] || die "fixture did not produce a manifest key"
    log "packed manifest: $(cat "$FIXTURE_MANIFEST")"
}

packed_smallfiles_scan() {
    python3 - "$MOUNT_DIR" "$PACKED_SMALLFILE_COUNT" "$PACKED_SMALLFILE_SIZE" "$READ_BYTES" "$PACKED_DIR_LEVELS" "$PACKED_DIRS_PER_LEVEL" "$PACKED_FILES_PER_DIR" <<'PY'
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
expected = int(sys.argv[2])
file_size = int(sys.argv[3])
read_bytes = int(sys.argv[4])
levels = int(sys.argv[5])
fanout = int(sys.argv[6])
files_per_leaf = int(sys.argv[7])
expected_leaf_dirs = fanout ** levels
expected_tree_dirs = sum(fanout ** level for level in range(1, levels + 1))
started = time.monotonic()
files = directories = leaf_dirs = logical = payload = errors = checksum = 0
walk_errors = []

def on_walk_error(error):
    walk_errors.append(error)
    print(f"walk error path={error.filename} error={error}")

import os
for directory, dirs, names in os.walk(root, onerror=on_walk_error):
    dirs[:] = sorted(name for name in dirs if name.startswith("d"))
    relative = pathlib.Path(directory).relative_to(root)
    depth = len(relative.parts)
    if depth:
        directories += 1
    if len(dirs) != (fanout if depth < levels else 0):
        raise SystemExit(f"directory fanout mismatch path={directory} depth={depth} actual={len(dirs)}")
    if depth < levels:
        if names:
            raise SystemExit(f"unexpected files above leaf level path={directory}")
        continue
    if depth != levels or len(names) != files_per_leaf:
        raise SystemExit(f"leaf shape mismatch path={directory} depth={depth} files={len(names)}")
    leaf_dirs += 1
    for name in sorted(names):
        path = pathlib.Path(directory) / name
        try:
            size = path.stat().st_size
            if size != file_size:
                raise OSError(f"size={size} expected={file_size}")
            with path.open("rb") as stream:
                data = stream.read() if read_bytes <= 0 else stream.read(read_bytes)
            if read_bytes <= 0 and len(data) != file_size:
                raise OSError(f"short read={len(data)} expected={file_size}")
            files += 1
            logical += size
            payload += len(data)
            checksum = (checksum + (data[0] if data else 0)) & 0xffffffff
        except OSError as error:
            errors += 1
            print(f"read error path={path} error={error}")
elapsed = time.monotonic() - started
mode = "full" if read_bytes <= 0 else f"prefix:{read_bytes}"
print(f"packed_smallfiles_summary files={files} expected={expected} directories={directories} expected_directories={expected_tree_dirs} leaf_directories={leaf_dirs} expected_leaf_directories={expected_leaf_dirs} file_size={file_size} read_mode={mode} logical_bytes={logical} payload_bytes={payload} errors={errors} walk_errors={len(walk_errors)} checksum={checksum} seconds={elapsed:.6f} files_per_sec={files / elapsed if elapsed else 0:.2f}")
if files != expected or directories != expected_tree_dirs or leaf_dirs != expected_leaf_dirs or errors or walk_errors:
    raise SystemExit(1)
PY
}

packed_tree_scan() {
    python3 - "$MOUNT_DIR" "$PACKED_SMALLFILE_COUNT" "$PACKED_DIR_LEVELS" "$PACKED_DIRS_PER_LEVEL" "$PACKED_FILES_PER_DIR" <<'PY'
import os
import pathlib
import sys
import time

root = pathlib.Path(sys.argv[1])
expected_files = int(sys.argv[2])
levels = int(sys.argv[3])
fanout = int(sys.argv[4])
files_per_leaf = int(sys.argv[5])
expected_leaf_dirs = fanout ** levels
expected_tree_dirs = sum(fanout ** level for level in range(1, levels + 1))
started = time.monotonic()
files = directories = leaf_dirs = 0
walk_errors = []

def on_walk_error(error):
    walk_errors.append(error)
    print(f"walk error path={error.filename} error={error}")

for directory, dirs, names in os.walk(root, onerror=on_walk_error):
    dirs[:] = sorted(name for name in dirs if name.startswith("d"))
    relative = pathlib.Path(directory).relative_to(root)
    depth = len(relative.parts)
    if depth:
        directories += 1
    if depth < levels:
        if names:
            raise SystemExit(f"unexpected files above leaf level path={directory}")
        if len(dirs) != fanout:
            raise SystemExit(f"directory fanout mismatch path={directory} depth={depth} actual={len(dirs)}")
        continue
    if depth != levels or len(names) != files_per_leaf:
        raise SystemExit(f"leaf shape mismatch path={directory} depth={depth} files={len(names)}")
    leaf_dirs += 1
    files += len(names)
elapsed = time.monotonic() - started
print(f"packed_tree_summary files={files} expected={expected_files} directories={directories} expected_directories={expected_tree_dirs} leaf_directories={leaf_dirs} expected_leaf_directories={expected_leaf_dirs} walk_errors={len(walk_errors)} seconds={elapsed:.6f}")
if files != expected_files or directories != expected_tree_dirs or leaf_dirs != expected_leaf_dirs or walk_errors:
    raise SystemExit(1)
PY
}

packed_posix_scan() {
    python3 - "$MOUNT_DIR" "$PACKED_SMALLFILE_SIZE" "$FIO_FILE_SIZE" "$PACKED_FILES_PER_DIR" "$PACKED_DIR_LEVELS" <<'PY'
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
small_size = int(sys.argv[2])
fio_size = int(sys.argv[3])
files_per_dir = int(sys.argv[4])
levels = int(sys.argv[5])
leaf = root.joinpath(*(["d000"] * levels))
read_path = leaf / "f00000"
bench_path = root / "bench" / "read.bin"
verify = root / "verify"
data = read_path.read_bytes()
if len(data) != small_size or bench_path.stat().st_size != fio_size:
    raise SystemExit("fixture sizes do not match")
if len(list(leaf.iterdir())) != files_per_dir:
    raise SystemExit("readdir count mismatch")
if read_path.stat().st_ino != (verify / "hardlink").stat().st_ino:
    raise SystemExit("hardlink inode mismatch")
if os.readlink(verify / "symlink") != "../" + "/".join(["d000"] * levels) + "/f00000":
    raise SystemExit("symlink target mismatch")
for name, predicate in (("fifo", stat.S_ISFIFO), ("socket", stat.S_ISSOCK), ("char", stat.S_ISCHR), ("block", stat.S_ISBLK)):
    if not predicate((verify / name).lstat().st_mode):
        raise SystemExit(f"special inode mismatch: {name}")
rejected = 0
for operation in (
    lambda: os.open(root / "write-attempt", os.O_WRONLY | os.O_CREAT, 0o644),
    lambda: os.mkdir(root / "mkdir-attempt"),
    lambda: os.unlink(read_path),
    lambda: os.rename(read_path, root / "rename-attempt"),
    lambda: os.truncate(bench_path, 0),
    lambda: os.chmod(read_path, 0o600),
    lambda: os.link(read_path, root / "link-attempt"),
    lambda: os.symlink("d000/f00000", root / "symlink-attempt"),
    lambda: os.rmdir(root / "d000"),
    lambda: os.setxattr(read_path, "user.packed", b"deny"),
):
    try:
        result = operation()
        if isinstance(result, int):
            os.close(result)
    except OSError:
        rejected += 1
if rejected != 10:
    raise SystemExit(f"mutation rejection mismatch: {rejected}")
print(f"packed_posix_summary read_bytes={len(data)} bench_bytes={bench_path.stat().st_size} mutation_checks={rejected}")
PY
}

fio_read() {
    local rw="$1"
    fio --name="packed-$rw" --filename="$MOUNT_DIR/bench/read.bin" \
        --rw="$rw" --ioengine=sync --direct=1 --iodepth=1 --numjobs=4 \
        --size="$FIO_FILE_SIZE" --runtime="$FIO_RUNTIME" --time_based=1 \
        --group_reporting=1 --output-format=normal
}

printf 'tool\tstatus\tseconds\tlog\n' >"$ARTIFACT_DIR/perf-summary.tsv"
if [[ "${PACKED_SKIP_FIXTURE:-false}" == "true" ]]; then
    : "${PACKED_EXISTING_MANIFEST_KEY:?PACKED_EXISTING_MANIFEST_KEY is required when PACKED_SKIP_FIXTURE=true}"
    printf '%s\n' "$PACKED_EXISTING_MANIFEST_KEY" >"$FIXTURE_MANIFEST"
    log "reusing packed manifest: $PACKED_EXISTING_MANIFEST_KEY"
else
    publish_fixture
fi

status=0
for tool in $TOOLS; do
    case "$tool" in
        packed-smallfiles) run_tool "$tool" packed_smallfiles_scan || status=1 ;;
        packed-tree) run_tool "$tool" packed_tree_scan || status=1 ;;
        packed-posix) run_tool "$tool" packed_posix_scan || status=1 ;;
        fio-seqread) run_tool "$tool" fio_read read || status=1 ;;
        fio-randread) run_tool "$tool" fio_read randread || status=1 ;;
        *) die "unsupported native packed tool: $tool" ;;
    esac
done

printf 'files=%s file_size=%s levels=%s fanout=%s files_per_leaf=%s read_bytes=%s\n' \
    "$PACKED_SMALLFILE_COUNT" "$PACKED_SMALLFILE_SIZE" "$PACKED_DIR_LEVELS" \
    "$PACKED_DIRS_PER_LEVEL" "$PACKED_FILES_PER_DIR" "$READ_BYTES"
cat "$ARTIFACT_DIR/perf-summary.tsv"
exit "$status"
