#!/usr/bin/env bash
# Reviewable G02 harness; not executed by the draft author. No FUSE or network.
# Usage (after building as the normal user): sudo bash script FIXTURE LIB_TEST ARTIFACT_DIR
set -euo pipefail

fixture=$(readlink -f -- "${1:?compiled fixture binary}")
lib_test=$(readlink -f -- "${2:?compiled brewfs library test executable}")
artifact=$(readlink -m -- "${3:?artifact directory outside the owned snapshot}")
[[ $EUID -eq 0 ]] || { echo 'Requires an isolated Linux root mount namespace.' >&2; exit 2; }
if [[ ${BREWFS_G02_MOUNT_NAMESPACE:-0} != 1 ]]; then
    exec unshare --mount --propagation private env BREWFS_G02_MOUNT_NAMESPACE=1 \
        bash "$0" "$fixture" "$lib_test" "$artifact"
fi
[[ -x $fixture && -x $lib_test ]] || { echo 'Build both executables before running.' >&2; exit 2; }
for tool in btrfs mkfs.btrfs losetup truncate mount umount findmnt python3; do
    command -v "$tool" >/dev/null || { echo "Missing dependency: $tool" >&2; exit 2; }
done
mkdir -p -- "$artifact"
work=$(mktemp -d /tmp/brewfs-source-btrfs-XXXXXXXX)
image="$work/owned-192m.img"
mountpoint="$work/filesystem"
bindpoint="$work/readonly-bind"
ordinary="$work/ordinary"
loop_device=''
mounted=false
nested_mounted=false
bind_mounted=false

cleanup() {
    local result=$?
    trap - EXIT INT TERM
    if $nested_mounted; then
        umount -- "$mountpoint/snapshot/nested" || { echo 'Owned nested mount cleanup failed; preserved image.' >&2; exit 1; }
    fi
    if $bind_mounted; then
        umount -- "$bindpoint" || { echo 'Owned bind mount cleanup failed; preserved source.' >&2; exit 1; }
    fi
    if $mounted; then
        if ! umount -- "$mountpoint"; then
            echo "Owned filesystem remains mounted at $mountpoint; preserved image." >&2
            exit 1
        fi
    fi
    if [[ -n $loop_device ]]; then
        local backing
        backing=$(losetup -n -O BACK-FILE -- "$loop_device")
        [[ $backing == "$image" ]] || { echo 'Loop backing identity changed; preserved image.' >&2; exit 1; }
        losetup -d -- "$loop_device" || { echo 'Loop detach failed; preserved image.' >&2; exit 1; }
    fi
    rm -f -- "$image"
    rm -f -- "$ordinary/file"
    rmdir -- "$mountpoint" "$bindpoint" "$ordinary" "$work"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -- "$mountpoint" "$bindpoint" "$ordinary"
printf 'ordinary' > "$ordinary/file"
truncate -s 192M -- "$image"
loop_device=$(losetup --find --show -- "$image")
[[ $loop_device == /dev/loop* ]] || { echo 'Unexpected loop identity.' >&2; exit 1; }
mkfs.btrfs -f -q -m single -d single -- "$loop_device" > "$artifact/mkfs.log" 2>&1
mount -t btrfs -o nosuid,nodev,noexec -- "$loop_device" "$mountpoint"
mounted=true
btrfs subvolume create "$mountpoint/original" > "$artifact/create.log"
printf 'before-source-mutation' > "$mountpoint/original/frozen"
mkdir -- "$mountpoint/original/nested"
btrfs subvolume snapshot -r "$mountpoint/original" "$mountpoint/snapshot" >> "$artifact/create.log"
btrfs subvolume create "$mountpoint/plain-readonly" >> "$artifact/create.log"
btrfs property set -ts "$mountpoint/plain-readonly" ro true
findmnt -n -o SOURCE,FSTYPE,OPTIONS --target "$mountpoint" > "$artifact/filesystem.txt"
btrfs subvolume show "$mountpoint/snapshot" > "$artifact/snapshot-identity.txt"

expect_reject() {
    local name=$1 source=$2 expected=$3
    if "$fixture" --source-directory "$source" --source-consistency snapshot-backed \
        --source-hardlink-policy visible-links --output-dir "$artifact/objects-$name" \
        --manifest-output "$artifact/$name.manifest" > "$artifact/$name.log" 2>&1; then
        echo "Unexpected SnapshotBacked acceptance: $name" >&2; return 1
    fi
    [[ ! -e "$artifact/$name.manifest" ]] || { echo 'Rejected source emitted a trusted manifest.' >&2; return 1; }
    grep -E -- "$expected" "$artifact/$name.log" >/dev/null
}
expect_reject ordinary "$ordinary" 'SnapshotBacked requires'
expect_reject mutable "$mountpoint/original" 'readonly snapshot'
expect_reject non-snapshot "$mountpoint/plain-readonly" 'readonly snapshot'
mount --bind -- "$ordinary" "$bindpoint"
bind_mounted=true
mount -o remount,bind,ro -- "$bindpoint"
expect_reject readonly-bind "$bindpoint" 'SnapshotBacked requires'
umount -- "$bindpoint"
bind_mounted=false

BREWFS_TEST_BTRFS_ORIGINAL="$mountpoint/original" BREWFS_TEST_BTRFS_SNAPSHOT="$mountpoint/snapshot" \
    "$lib_test" namespace_real_btrfs_snapshot_freezes_mutable_source_and_preserves_provenance \
    --ignored --nocapture > "$artifact/library-positive.log" 2>&1
# Require one executed test so a mismatched filter cannot silently pass.
grep -E 'test result: ok\. 1 passed;' "$artifact/library-positive.log" >/dev/null

"$fixture" --source-directory "$mountpoint/snapshot" --source-consistency snapshot-backed \
    --source-hardlink-policy visible-links --output-dir "$artifact/objects-positive" \
    --manifest-output "$artifact/positive.manifest" > "$artifact/fixture-positive.log" 2>&1
python3 - "$artifact/positive.source.json" <<'PY'
import json,sys
data=json.load(open(sys.argv[1]))
view=data['source_view']
assert data['source_consistency']=='snapshot-backed'
assert view['provider']=='linux-btrfs-readonly-snapshot'
assert all(view[k] for k in ('filesystem_uuid','snapshot_uuid','parent_snapshot_uuid','subvolume_id','root_stat_token'))
assert view['generation'] is not None and view['change_transaction'] is not None
PY

mount --bind -- "$ordinary" "$mountpoint/snapshot/nested"
nested_mounted=true
expect_reject nested-mount "$mountpoint/snapshot" 'nested mount/subvolume'
umount -- "$mountpoint/snapshot/nested"
nested_mounted=false

BREWFS_TEST_BTRFS_SNAPSHOT="$mountpoint/snapshot" \
    "$lib_test" source_btrfs_lease_rejects_revoked_readonly_guard \
    --ignored --nocapture > "$artifact/library-revoked.log" 2>&1
grep -E 'test result: ok\. 1 passed;' "$artifact/library-revoked.log" >/dev/null
python3 - "$artifact/summary.json" <<'PY'
import json,sys
json.dump({'image_bytes':192*1024*1024,'fuse':False,'network':False,
           'positive':'real readonly Btrfs snapshot with mutable-original change',
           'rejections':['ordinary','mutable','non-snapshot','readonly-bind','nested-mount','revoked-readonly'],
           'status':'passed-before-cleanup'},open(sys.argv[1],'w'),indent=2)
PY
