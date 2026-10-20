#!/bin/sh
set -eu

DEFAULT_FIO_TOOLS="fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw"
MOUNT_DIR=/mnt/juicefs
JUICEFS_BIN=/usr/local/bin/juicefs
JUICEFS_PID=""
JUICEFS_LOG=/artifacts/juicefs.log

configure_apt_mirror() {
    [ -n "${JUICEFS_APT_MIRROR:-}" ] || return 0
    mirror="${JUICEFS_APT_MIRROR%/}"
    case "$mirror" in
        http://*|https://*) ;;
        *) mirror="http://${mirror}" ;;
    esac
    sed -i -E \
        -e "s|https?://[^ /]+/debian-security|${mirror}/debian-security|g" \
        -e "s|https?://[^ /]+/debian|${mirror}/debian|g" \
        /etc/apt/sources.list /etc/apt/sources.list.d/*.sources 2>/dev/null || true
}

install_dependencies() {
    echo "Installing dependencies..."
    configure_apt_mirror
    apt_options="-o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o Acquire::Retries=2"
    apt-get $apt_options update -qq
    if ! apt-get $apt_options install -y -qq awscli curl fuse3 fio procps util-linux >/tmp/apt-install.log 2>&1; then
        tail -20 /tmp/apt-install.log >&2
        return 1
    fi
    tail -3 /tmp/apt-install.log
}

install_juicefs() {
    if [ ! -x "$JUICEFS_BIN" ]; then
        echo "Installing JuiceFS..."
        curl -fsSL "${JUICEFS_INSTALL_URL:-https://d.juicefs.com/install}" | sh -
    fi
    "$JUICEFS_BIN" version | tee /artifacts/juicefs-version.txt
}

ensure_fuse_device() {
    modprobe fuse 2>/dev/null || true
    if [ ! -c /dev/fuse ]; then
        mknod /dev/fuse c 10 229
        chmod 666 /dev/fuse
    fi
    test -c /dev/fuse
}

stop_juicefs() {
    fusermount3 -u "$MOUNT_DIR" 2>/dev/null || fusermount -u "$MOUNT_DIR" 2>/dev/null || umount "$MOUNT_DIR" 2>/dev/null || true
    if [ -n "$JUICEFS_PID" ]; then
        kill "$JUICEFS_PID" 2>/dev/null || true
        wait "$JUICEFS_PID" 2>/dev/null || true
        JUICEFS_PID=""
    fi
}

cleanup() {
    echo "Cleanup..."
    stop_juicefs
}
trap cleanup EXIT INT TERM

start_juicefs() {
    echo "Mounting JuiceFS..."
    "$JUICEFS_BIN" mount --foreground "$meta_url" "$MOUNT_DIR" >>"$JUICEFS_LOG" 2>&1 &
    JUICEFS_PID=$!
    for _ in $(seq 1 30); do
        if findmnt -rn --target "$MOUNT_DIR" --output FSTYPE 2>/dev/null | grep -Eq '^fuse'; then
            echo "Mounted filesystem: $(findmnt -rn --target "$MOUNT_DIR" --output FSTYPE,SOURCE)"
            return 0
        fi
        kill -0 "$JUICEFS_PID" 2>/dev/null || break
        sleep 1
    done
    echo "JuiceFS did not establish a FUSE mount" >&2
    cat "$JUICEFS_LOG" >&2
    return 1
}

remount_juicefs() {
    sync
    stop_juicefs
    start_juicefs
}

get_env() {
    eval "printf '%s' \"\${$1-}\""
}

profile_value() {
    profile_key="$1"
    suffix="$2"
    default_value="$3"
    value="$(get_env "PERF_FIO_${profile_key}_${suffix}")"
    [ -n "$value" ] || value="$(get_env "PERF_FIO_${suffix}")"
    [ -n "$value" ] || value="$default_value"
    printf '%s' "$value"
}

prepare_fio_dataset() {
    tool="$1"
    work_dir="$2"
    size="$3"
    numjobs="$4"
    bs="$5"
    echo "Preparing dataset for $tool..."
    fio --name="${tool}-prepare" --directory="$work_dir" --rw=write \
        --bs="$bs" --size="$size" --numjobs="$numjobs" \
        --ioengine=io_uring --iodepth=1 --direct=0 --end_fsync=1 \
        --group_reporting --eta=never >"/artifacts/${tool}-prepare.log" 2>&1
    remount_juicefs
}

run_fio_profile() {
    tool="$1"
    profile="${tool#fio-}"
    profile_key="$(printf '%s' "$profile" | tr '[:lower:]-' '[:upper:]_')"
    work_dir="$MOUNT_DIR/.perf-${tool}"
    json_path="/artifacts/${tool}.json"
    needs_prefill=false
    time_based=true
    end_fsync=false
    rwmixread=""

    case "$profile" in
        bigwrite) rw=write; bs=4m; size=1g; numjobs=8; time_based=false; end_fsync=true ;;
        bigread) rw=read; bs=4m; size=1g; numjobs=8; time_based=false; needs_prefill=true ;;
        seqread) rw=read; bs=4m; size=1g; numjobs=1; needs_prefill=true ;;
        seqwrite) rw=write; bs=4m; size=1g; numjobs=1 ;;
        randread) rw=randread; bs=4m; size=512m; numjobs=4; needs_prefill=true ;;
        randwrite) rw=randwrite; bs=4m; size=512m; numjobs=4 ;;
        randrw) rw=randrw; bs=4m; size=512m; numjobs=4; rwmixread=70; needs_prefill=true ;;
        *) echo "unsupported PERF_TOOLS entry: $tool" >&2; return 1 ;;
    esac

    bs="$(profile_value "$profile_key" BS "$bs")"
    size="$(profile_value "$profile_key" SIZE "$size")"
    numjobs="$(profile_value "$profile_key" NUMJOBS "$numjobs")"
    runtime="$(profile_value "$profile_key" RUNTIME 60)"
    rm -rf "$work_dir"
    mkdir -p "$work_dir"
    if [ "$needs_prefill" = true ]; then
        prepare_fio_dataset "$tool" "$work_dir" "$size" "$numjobs" "$bs"
    fi

    echo "Running $tool: rw=$rw bs=$bs size=$size numjobs=$numjobs"
    set -- --name="$tool" --directory="$work_dir" --rw="$rw" --bs="$bs" \
        --size="$size" --numjobs="$numjobs" --ioengine=io_uring --iodepth=1 \
        --direct=0 --group_reporting --eta=never --output-format=json --output="$json_path"
    [ "$time_based" = false ] || set -- "$@" --runtime="$runtime" --time_based
    [ "$end_fsync" = false ] || set -- "$@" --end_fsync=1
    [ -z "$rwmixread" ] || set -- "$@" --rwmixread="$rwmixread"
    fio "$@" >"/artifacts/${tool}.log" 2>&1
}

echo "=== JuiceFS wslc-compose perf test ==="
mkdir -p "$MOUNT_DIR" /artifacts
install_dependencies
install_juicefs
ensure_fuse_device

meta_url="${JUICEFS_META_URL:-redis://redis:6379/1}"
s3_bucket="${JUICEFS_S3_BUCKET:-juicefs-data}"
s3_endpoint="${JUICEFS_S3_ENDPOINT:-http://rustfs:9000}"
echo "Waiting for RustFS..."
for _ in $(seq 1 60); do
    aws --endpoint-url "$s3_endpoint" s3api list-buckets >/dev/null 2>&1 && break
    sleep 1
done
aws --endpoint-url "$s3_endpoint" s3api list-buckets >/dev/null
if ! aws --endpoint-url "$s3_endpoint" s3api head-bucket --bucket "$s3_bucket" >/dev/null 2>&1; then
    aws --endpoint-url "$s3_endpoint" s3api create-bucket --bucket "$s3_bucket" >/dev/null
fi

"$JUICEFS_BIN" format --storage s3 --bucket "${s3_endpoint}/${s3_bucket}" \
    --access-key "${AWS_ACCESS_KEY_ID:-rustfsadmin}" \
    --secret-key "${AWS_SECRET_ACCESS_KEY:-rustfsadmin}" \
    "$meta_url" juicefs-perf
start_juicefs

PERF_TOOLS="${PERF_TOOLS:-$DEFAULT_FIO_TOOLS}"
printf '%s\n' "$PERF_TOOLS" > /artifacts/fio-profiles.txt
for tool in $PERF_TOOLS; do
    run_fio_profile "$tool"
done

echo "Verifying data reached RustFS..."
aws --endpoint-url "$s3_endpoint" s3api list-objects-v2 \
    --bucket "$s3_bucket" --no-paginate --output json >/artifacts/rustfs-objects.json
object_count=$(aws --endpoint-url "$s3_endpoint" s3api list-objects-v2 \
    --bucket "$s3_bucket" --no-paginate --query 'length(Contents)' --output text)
test "$object_count" -gt 0
echo "RustFS object count: $object_count"
