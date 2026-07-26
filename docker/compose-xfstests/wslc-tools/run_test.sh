#!/bin/sh
set -eu

DEFAULT_FIO_TOOLS="fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw"
MOUNT_DIR=/mnt/brewfs
BREWFS_PID=""
BREWFS_LOG=/artifacts/brewfs.log
BREWFS_LOG_INITIALIZED=false
BREWFS_CONFIG=/tmp/brewfs-wslc-perf.yaml

echo "=== brewfs wslc-compose perf test ==="
echo "Installing dependencies..."
if [ -n "${BREWFS_APT_MIRROR:-}" ]; then
    mirror="${BREWFS_APT_MIRROR%/}"
    case "$mirror" in
        http://*|https://*) ;;
        *) mirror="https://${mirror}" ;;
    esac
    sed -i -E \
        -e "s|https?://[^ /]+/debian-security|${mirror}/debian-security|g" \
        -e "s|https?://[^ /]+/debian|${mirror}/debian|g" \
        /etc/apt/sources.list /etc/apt/sources.list.d/*.sources 2>/dev/null || true
fi
apt_options="-o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o Acquire::Retries=2"
apt-get $apt_options update -qq
if ! apt-get $apt_options install -y -qq awscli fuse3 fio procps util-linux >/tmp/apt-install.log 2>&1; then
    tail -20 /tmp/apt-install.log >&2
    exit 1
fi
tail -3 /tmp/apt-install.log
command -v fio >/dev/null
echo "Dependencies installed."

echo "Setting up fuse..."
modprobe fuse 2>/dev/null || true
mkdir -p "$MOUNT_DIR" /var/lib/brewfs/data /artifacts
test -c /dev/fuse

data_backend="${BREWFS_DATA_BACKEND:-s3}"
# The WSLC guest currently has about 2 GiB of RAM. BrewFS's normal cache
# defaults target larger Linux hosts and can let the mixed fio workload OOM the
# mount process. Keep this runner bounded while allowing callers to override
# each value for a larger WSLC guest.
BREWFS_READ_MEMORY_BYTES="${BREWFS_READ_MEMORY_BYTES:-268435456}"
BREWFS_WRITE_MEMORY_BYTES="${BREWFS_WRITE_MEMORY_BYTES:-134217728}"
BREWFS_MEMORY_BUDGET_BYTES="${BREWFS_MEMORY_BUDGET_BYTES:-536870912}"

write_brewfs_config() {
    cat >"$BREWFS_CONFIG" <<EOF
cache:
  read_memory_bytes: ${BREWFS_READ_MEMORY_BYTES}
  write_memory_bytes: ${BREWFS_WRITE_MEMORY_BYTES}
  memory_budget_bytes: ${BREWFS_MEMORY_BUDGET_BYTES}
EOF
    cp "$BREWFS_CONFIG" /artifacts/brewfs-config.yaml
}

write_brewfs_config
if [ "$data_backend" = "s3" ]; then
    : "${BREWFS_S3_BUCKET:?BREWFS_S3_BUCKET is required for the s3 backend}"
    : "${BREWFS_S3_ENDPOINT:?BREWFS_S3_ENDPOINT is required for the s3 backend}"
    echo "Waiting for RustFS endpoint..."
    for _ in $(seq 1 60); do
        if aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-buckets >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-buckets >/dev/null
    if ! aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api head-bucket --bucket "$BREWFS_S3_BUCKET" >/dev/null 2>&1; then
        aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api create-bucket --bucket "$BREWFS_S3_BUCKET" >/dev/null
    fi
elif [ "$data_backend" != "local-fs" ]; then
    echo "unsupported BREWFS_DATA_BACKEND: $data_backend" >&2
    exit 1
fi

stop_brewfs() {
    fusermount3 -u "$MOUNT_DIR" 2>/dev/null || fusermount -u "$MOUNT_DIR" 2>/dev/null || umount "$MOUNT_DIR" 2>/dev/null || true
    if [ -n "$BREWFS_PID" ]; then
        kill "$BREWFS_PID" 2>/dev/null || true
        wait "$BREWFS_PID" 2>/dev/null || true
        BREWFS_PID=""
    fi
}

cleanup() {
    echo "Cleanup..."
    stop_brewfs
}
trap cleanup EXIT INT TERM

start_brewfs() {
    echo "Starting brewfs..."
    if [ "$BREWFS_LOG_INITIALIZED" = false ]; then
        : >"$BREWFS_LOG"
        BREWFS_LOG_INITIALIZED=true
    else
        printf '\n=== BrewFS remount ===\n' >>"$BREWFS_LOG"
    fi
    if [ "$data_backend" = "s3" ]; then
        /brewfs-bin/brewfs mount --privileged --config "$BREWFS_CONFIG" \
            --meta-backend "${BREWFS_META_BACKEND:-redis}" \
            --meta-url "${BREWFS_META_URL:-redis://redis:6379/0}" \
            --data-backend s3 \
            --s3-bucket "$BREWFS_S3_BUCKET" \
            --s3-endpoint "$BREWFS_S3_ENDPOINT" \
            --s3-region "${BREWFS_S3_REGION:-us-east-1}" \
            --s3-force-path-style="${BREWFS_S3_FORCE_PATH_STYLE:-true}" \
            "$MOUNT_DIR" >>"$BREWFS_LOG" 2>&1 &
    else
        /brewfs-bin/brewfs mount --privileged --config "$BREWFS_CONFIG" \
            --meta-backend "${BREWFS_META_BACKEND:-redis}" \
            --meta-url "${BREWFS_META_URL:-redis://redis:6379/0}" \
            --data-backend local-fs \
            --data-dir "${BREWFS_DATA_DIR:-/var/lib/brewfs/data}" \
            "$MOUNT_DIR" >>"$BREWFS_LOG" 2>&1 &
    fi
    BREWFS_PID=$!

    mounted=0
    for _ in $(seq 1 30); do
        if findmnt -rn --target "$MOUNT_DIR" --output FSTYPE 2>/dev/null | grep -Eq '^fuse(\.|$)'; then
            mounted=1
            break
        fi
        if ! kill -0 "$BREWFS_PID" 2>/dev/null; then
            break
        fi
        sleep 1
    done
    if [ "$mounted" -ne 1 ]; then
        echo "brewfs did not establish a FUSE mount" >&2
        wait "$BREWFS_PID" || true
        exit 1
    fi
    echo "Mounted filesystem: $(findmnt -rn --target "$MOUNT_DIR" --output FSTYPE,SOURCE)"
}

is_true() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

get_env() {
    eval "printf '%s' \"\${$1-}\""
}

profile_value() {
    profile_key="$1"
    suffix="$2"
    default_value="$3"
    value="$(get_env "PERF_FIO_${profile_key}_${suffix}")"
    if [ -z "$value" ]; then
        value="$(get_env "PERF_FIO_${suffix}")"
    fi
    if [ -z "$value" ]; then
        value="$default_value"
    fi
    printf '%s' "$value"
}

prepare_fio_dataset() {
    tool="$1"
    work_dir="$2"
    name="$3"
    size="$4"
    direct="$5"
    numjobs="$6"
    bs="$7"
    ioengine="$8"
    iodepth="$9"

    echo "Preparing fio dataset for $tool..."
    fio \
        --name="$name" \
        --directory="$work_dir" \
        --rw=write \
        --bs="${PERF_FIO_PREP_BS:-$bs}" \
        --size="$size" \
        --numjobs="${PERF_FIO_PREP_NUMJOBS:-$numjobs}" \
        --ioengine="${PERF_FIO_PREP_IOENGINE:-$ioengine}" \
        --iodepth="${PERF_FIO_PREP_IODEPTH:-$iodepth}" \
        --direct="$direct" \
        --end_fsync=1 \
        --group_reporting \
        --eta=never \
        >"/artifacts/${tool}-prepare.log" 2>&1
}

remount_brewfs() {
    sync
    stop_brewfs
    start_brewfs
}

remount_for_cold_read() {
    echo "Remounting BrewFS before read profile..."
    remount_brewfs
}

run_fio_profile() {
    tool="$1"
    profile="${tool#fio-}"
    profile_key="$(printf '%s' "$profile" | tr '[:lower:]-' '[:upper:]_')"
    work_dir="$MOUNT_DIR/.perf-${tool}"
    json_path="/artifacts/${tool}.json"
    log_path="/artifacts/${tool}.log"
    needs_prefill=false
    use_time_based=true
    use_end_fsync=false
    use_refill_buffers=false
    rwmixread=""

    case "$profile" in
        seqread)
            name="$(profile_value "$profile_key" NAME brewfs-seqread)"
            rw="$(profile_value "$profile_key" RW read)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 1g)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 1)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime="$(profile_value "$profile_key" RUNTIME 60)"
            needs_prefill=true
            ;;
        seqwrite)
            name="$(profile_value "$profile_key" NAME brewfs-seqwrite)"
            rw="$(profile_value "$profile_key" RW write)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 1g)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 1)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime="$(profile_value "$profile_key" RUNTIME 60)"
            ;;
        randread)
            name="$(profile_value "$profile_key" NAME brewfs-randread)"
            rw="$(profile_value "$profile_key" RW randread)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 512m)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 4)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime="$(profile_value "$profile_key" RUNTIME 60)"
            needs_prefill=true
            ;;
        randwrite)
            name="$(profile_value "$profile_key" NAME brewfs-randwrite)"
            rw="$(profile_value "$profile_key" RW randwrite)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 512m)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 4)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime="$(profile_value "$profile_key" RUNTIME 60)"
            ;;
        randrw)
            name="$(profile_value "$profile_key" NAME brewfs-randrw)"
            rw="$(profile_value "$profile_key" RW randrw)"
            rwmixread="$(profile_value "$profile_key" RWMIXREAD 70)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 512m)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 4)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime="$(profile_value "$profile_key" RUNTIME 60)"
            needs_prefill=true
            ;;
        bigwrite)
            name="$(profile_value "$profile_key" NAME brewfs-bigwrite)"
            rw="$(profile_value "$profile_key" RW write)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 128m)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 8)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime=0
            use_time_based=false
            use_end_fsync=true
            use_refill_buffers=true
            ;;
        bigread)
            name="$(profile_value "$profile_key" NAME brewfs-bigread)"
            rw="$(profile_value "$profile_key" RW read)"
            bs="$(profile_value "$profile_key" BS 4m)"
            size="$(profile_value "$profile_key" SIZE 128m)"
            numjobs="$(profile_value "$profile_key" NUMJOBS 8)"
            ioengine="$(profile_value "$profile_key" IOENGINE io_uring)"
            iodepth="$(profile_value "$profile_key" IODEPTH 1)"
            direct="$(profile_value "$profile_key" DIRECT 0)"
            runtime=0
            use_time_based=false
            use_refill_buffers=true
            needs_prefill=true
            ;;
        *)
            echo "unsupported PERF_TOOLS entry: $tool" >&2
            return 1
            ;;
    esac

    rm -rf "$work_dir"
    mkdir -p "$work_dir"
    if [ "$needs_prefill" = true ]; then
        prepare_fio_dataset "$tool" "$work_dir" "$name" "$size" "$direct" "$numjobs" "$bs" "$ioengine" "$iodepth"
        if is_true "${PERF_FIO_COLD_READ:-false}" || is_true "${PERF_FIO_PREFILL_REMOUNT:-false}"; then
            remount_for_cold_read
        fi
    fi

    echo "Running $tool: rw=$rw bs=$bs size=$size numjobs=$numjobs ioengine=$ioengine iodepth=$iodepth direct=$direct"
    set -- \
        --name="$name" \
        --directory="$work_dir" \
        --rw="$rw" \
        --bs="$bs" \
        --size="$size" \
        --numjobs="$numjobs" \
        --ioengine="$ioengine" \
        --iodepth="$iodepth" \
        --direct="$direct" \
        --group_reporting \
        --eta=never \
        --output-format=json \
        --output="$json_path"
    if [ "$use_time_based" = true ]; then
        set -- "$@" --runtime="$runtime" --time_based
    fi
    if [ "$use_end_fsync" = true ]; then
        set -- "$@" --end_fsync=1
    fi
    if [ "$use_refill_buffers" = true ]; then
        set -- "$@" --refill_buffers
    fi
    if [ -n "$rwmixread" ]; then
        set -- "$@" --rwmixread="$rwmixread"
    fi
    fio "$@" >"$log_path" 2>&1
}

PERF_TOOLS="${PERF_TOOLS:-$DEFAULT_FIO_TOOLS}"
printf '%s\n' "$PERF_TOOLS" > /artifacts/fio-profiles.txt
{
    printf 'PERF_FIO_REMOUNT_BETWEEN_PROFILES=%s\n' "${PERF_FIO_REMOUNT_BETWEEN_PROFILES:-true}"
    env | sort | grep '^PERF_FIO_' || true
} > /artifacts/fio-environment.txt

start_brewfs
first_profile=true
for tool in $PERF_TOOLS; do
    if [ "$first_profile" = false ] && is_true "${PERF_FIO_REMOUNT_BETWEEN_PROFILES:-true}"; then
        echo "Remounting BrewFS between fio profiles..."
        remount_brewfs
    fi
    run_fio_profile "$tool"
    first_profile=false
done

if [ "$data_backend" = "s3" ]; then
    echo "Verifying fio data reached RustFS..."
    aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-objects-v2 \
        --bucket "$BREWFS_S3_BUCKET" \
        --no-paginate \
        --output json >/artifacts/rustfs-objects.json
    object_count=$(aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-objects-v2 \
        --bucket "$BREWFS_S3_BUCKET" \
        --no-paginate \
        --query 'length(Contents)' \
        --output text)
    test "$object_count" -gt 0
    echo "RustFS object count: $object_count"
fi

echo "=== perf test completed ==="
