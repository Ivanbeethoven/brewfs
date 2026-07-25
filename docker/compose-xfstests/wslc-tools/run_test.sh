#!/bin/sh
set -e
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
mkdir -p /mnt/brewfs /var/lib/brewfs/data
test -c /dev/fuse

data_backend="${BREWFS_DATA_BACKEND:-s3}"
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
fi

echo "Starting brewfs..."
if [ "$data_backend" = "s3" ]; then
    /brewfs-bin/brewfs mount --privileged \
        --meta-backend "${BREWFS_META_BACKEND:-redis}" \
        --meta-url "${BREWFS_META_URL:-redis://redis:6379/0}" \
        --data-backend s3 \
        --s3-bucket "$BREWFS_S3_BUCKET" \
        --s3-endpoint "$BREWFS_S3_ENDPOINT" \
        --s3-region "${BREWFS_S3_REGION:-us-east-1}" \
        --s3-force-path-style="${BREWFS_S3_FORCE_PATH_STYLE:-true}" \
        /mnt/brewfs &
elif [ "$data_backend" = "local-fs" ]; then
    /brewfs-bin/brewfs mount --privileged \
        --meta-backend "${BREWFS_META_BACKEND:-redis}" \
        --meta-url "${BREWFS_META_URL:-redis://redis:6379/0}" \
        --data-backend local-fs \
        --data-dir "${BREWFS_DATA_DIR:-/var/lib/brewfs/data}" \
        /mnt/brewfs &
else
    echo "unsupported BREWFS_DATA_BACKEND: $data_backend" >&2
    exit 1
fi
BREWFS_PID=$!
mounted=0
for _ in $(seq 1 30); do
    if findmnt -rn --target /mnt/brewfs --output FSTYPE 2>/dev/null | grep -Eq '^fuse(\.|$)'; then
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
echo "Mounted filesystem: $(findmnt -rn --target /mnt/brewfs --output FSTYPE,SOURCE)"

echo "Running fio basic write test..."
fio --name=basic-write --size=10M --rw=write --bs=4k --directory=/mnt/brewfs --end_fsync=1 --output-format=json --output=/artifacts/fio-write.json 2>&1
echo "Running fio basic read test..."
fio --name=basic-read --size=10M --rw=read --bs=4k --directory=/mnt/brewfs --output-format=json --output=/artifacts/fio-read.json 2>&1

if [ "$data_backend" = "s3" ]; then
    echo "Verifying fio data reached RustFS..."
    aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-objects-v2 \
        --bucket "$BREWFS_S3_BUCKET" \
        --output json >/artifacts/rustfs-objects.json
    object_count=$(aws --endpoint-url "$BREWFS_S3_ENDPOINT" s3api list-objects-v2 \
        --bucket "$BREWFS_S3_BUCKET" \
        --query 'length(Contents)' \
        --output text)
    test "$object_count" -gt 0
    echo "RustFS object count: $object_count"
fi

echo "Cleanup..."
fusermount3 -u /mnt/brewfs 2>/dev/null || fusermount -u /mnt/brewfs 2>/dev/null || umount /mnt/brewfs 2>/dev/null || true
kill $BREWFS_PID 2>/dev/null || true
echo "=== perf test completed ==="
