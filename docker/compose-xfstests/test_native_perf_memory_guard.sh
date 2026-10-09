#!/usr/bin/env bash
# Regression for the Aliyun native runner memory pre-check.
#
# On 2026-09-17 the writeback throughput profile was launched on an 8 GiB ECS
# (ecs.u1-c1m2.xlarge) instead of the usual 16 GiB box. BrewFS never finished:
# the 80 GiB system disk stayed pinned at 170 MB/s reads with 55-70 ms latency,
# the data disk sat idle, CPU stayed under 10%, and Cloud Assistant stopped
# answering. The guard added for that has to refuse such a host from the real
# argument parser, before the runner touches any data or artifact root, while
# still letting a 16 GiB host through.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$SCRIPT_DIR/aliyun/run_native_perf.sh"
[[ -f "$SRC" ]] || { echo "FAIL: missing $SRC"; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

tr -d '\r' < "$SRC" > "$tmp/run.sh"
bash -n "$tmp/run.sh" && echo "SYNTAX-OK : bash -n"

sed -n '/^require_memory_headroom()/,/^}/p' "$tmp/run.sh" > "$tmp/helper.sh"
grep -q 'require_memory_headroom()' "$tmp/helper.sh" || { echo "FAIL: helper not extracted"; exit 1; }

printf 'MemTotal:        8388608 kB\n' > "$tmp/mem-8g"
printf 'MemTotal:       16311000 kB\n' > "$tmp/mem-16g"

unit_case() {
    local meminfo="$1" required="$2" label="$3" expect="$4"
    local out status
    set +e
    out="$(BREWFS_PERF_MEMINFO="$meminfo" bash -c "
        source '$tmp/helper.sh'
        log() { echo \"[native-perf] \$*\"; }
        die() { log \"ERROR \$*\" >&2; exit 1; }
        is_truthy() { case \"\${1:-}\" in 1|true|TRUE|yes|YES|on|ON) return 0 ;; *) return 1 ;; esac; }
        require_memory_headroom $required '$label'
    " 2>&1)"
    status=$?
    set -e
    echo "--- unit $(basename "$meminfo") required=$required expect=$expect exit=$status ---"
    [[ -n "$out" ]] && echo "$out"
    if [[ "$expect" == fail ]]; then
        [[ $status -ne 0 ]] || { echo "FAIL: expected a refusal"; exit 1; }
    else
        [[ $status -eq 0 ]] || { echo "FAIL: expected the check to pass"; echo "$out"; exit 1; }
    fi
    echo "OK ($expect)"
}

# BrewFS writeback profile: 4 GiB read cache + 4 GiB write cache + 4 GiB prefill.
unit_case "$tmp/mem-8g" 12884901888 "BrewFS 4GiB read + 4GiB write + 4GiB prefill" fail
unit_case "$tmp/mem-16g" 12884901888 "BrewFS 4GiB read + 4GiB write + 4GiB prefill" pass

# Integration: the guard has to fire from the real argument parser. The pass
# case is proven the same way -- the guard stays silent and the runner only
# stops later, at a deliberately blocked data root.
integration_case() {
    local meminfo="$1" expect="$2"
    local out status
    set +e
    out="$(BREWFS_PERF_MEMINFO="$meminfo" BREWFS_PERF_SOURCE_ROOT="$tmp/src" \
        BREWFS_PERF_DATA_ROOT=/dev/null/blocked-data-root \
        bash "$tmp/run.sh" brewfs --writeback-throughput-profile --s3 2>&1)"
    status=$?
    set -e
    echo "--- integration $(basename "$meminfo") expect=$expect exit=$status ---"
    if [[ "$expect" == fail ]]; then
        [[ $status -ne 0 ]] || { echo "FAIL: expected nonzero exit"; exit 1; }
        grep -q '拒绝启动' <<<"$out" || { echo "FAIL: missing refusal message"; echo "$out"; exit 1; }
    else
        if grep -q '拒绝启动' <<<"$out"; then
            echo "FAIL: guard refused on a 16 GiB host"
            echo "$out"
            exit 1
        fi
    fi
    echo "$out" | tail -3
    echo "OK ($expect)"
}

integration_case "$tmp/mem-8g" fail
integration_case "$tmp/mem-16g" pass
echo "ALL-OK"
