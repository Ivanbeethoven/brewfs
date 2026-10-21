#!/usr/bin/env python3
"""Small-file write / read-back / mixed workload for a mounted BrewFS tree.

Modes (one per invocation, all against a dedicated ``--subdir`` of the mount):

  write    create ``--files`` new files of ``--file-bytes`` each (O_CREAT|O_EXCL,
           write, fsync, close). Latency is one complete file.
  readback open+read+close every file written by ``write``; verifies content.
  mixed    each op is a read of an existing ``write`` file or a create+write of a
           new file, chosen by a seeded RNG (``--write-ratio``). Reads verify
           content; latency is per op.

Payload bytes are a deterministic function of the file index, so read-back is
verified byte-for-byte without keeping the corpus in memory. Output is one JSON
summary compatible with run_overlay_vs_native.sh rows.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import random
import statistics
import sys
import threading
import time

DIRS = 20


def payload(index: int, size: int) -> bytes:
    seed = hashlib.sha256(index.to_bytes(8, "little")).digest()
    return (seed * (size // len(seed) + 1))[:size]


def file_path(root: pathlib.Path, index: int) -> pathlib.Path:
    return root / f"d{index % DIRS:03d}" / f"f{index:07d}"


def write_one(root: pathlib.Path, index: int, size: int, fsync: bool, syscalls=os) -> None:
    data = payload(index, size)
    fd = syscalls.open(file_path(root, index), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    try:
        view = memoryview(data)
        while view:
            view = view[syscalls.write(fd, view):]
        if fsync:
            syscalls.fsync(fd)
    finally:
        syscalls.close(fd)


def read_one(root: pathlib.Path, index: int, size: int, syscalls=os) -> None:
    fd = syscalls.open(file_path(root, index), os.O_RDONLY)
    try:
        chunks = []
        remaining = size + 1
        while remaining > 0:
            chunk = syscalls.read(fd, min(remaining, 1 << 20))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
    finally:
        syscalls.close(fd)
    if b"".join(chunks) != payload(index, size):
        raise ValueError(f"content mismatch for file {index}")


class TimedSyscalls:
    """Observe the existing syscall sequence without adding filesystem calls."""

    def __init__(self):
        self.stages = []

    def __getattr__(self, name):
        if name not in {"open", "write", "fsync", "read", "close"}:
            raise AttributeError(name)
        function = getattr(os, name)

        def call(*args):
            start = time.monotonic_ns()
            cpu_start = time.thread_time_ns()
            result, success = None, False
            try:
                result = function(*args)
                success = True
                return result
            finally:
                cpu_end = time.thread_time_ns()
                end = time.monotonic_ns()
                stage = {"name": name, "start_monotonic_ns": start,
                         "end_monotonic_ns": end, "elapsed_ns": end - start,
                         "thread_cpu_ns": cpu_end - cpu_start, "success": success}
                if success and name in {"read", "write"}:
                    stage["bytes"] = len(result) if name == "read" else result
                self.stages.append(stage)

        return call


def percentile(values, q):
    if not values:
        return None
    ordered = sorted(values)
    rank = max(0, min(len(ordered) - 1, int(round(q / 100 * (len(ordered) - 1)))))
    return ordered[rank]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--subdir", default="rw")
    parser.add_argument("--mode", choices=("write", "readback", "mixed"), required=True)
    parser.add_argument("--files", type=int, required=True)
    parser.add_argument("--file-bytes", type=int, required=True)
    parser.add_argument("--workers", type=int, default=16)
    parser.add_argument("--mixed-ops", type=int, default=0, help="mixed: op count (default --files)")
    parser.add_argument("--write-ratio", type=float, default=0.5)
    parser.add_argument("--seed", type=int, default=20261014)
    parser.add_argument("--no-fsync", action="store_true")
    parser.add_argument("--label", default="rw")
    parser.add_argument("--json-output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    critical_path = os.environ.get("BREWFS_CRITICAL_PATH_DIAGNOSTICS") == "1"

    root = args.root / args.subdir
    fsync = not args.no_fsync
    if args.mode == "write":
        root.mkdir(exist_ok=True)
        if any(root.iterdir()):
            raise SystemExit(f"{root} is not empty")
        for d in range(DIRS):
            (root / f"d{d:03d}").mkdir()
        ops = [("w", i) for i in range(args.files)]
    elif args.mode == "readback":
        ops = [("r", i) for i in range(args.files)]
        random.Random(args.seed).shuffle(ops)
    else:
        rng = random.Random(args.seed)
        count = args.mixed_ops or args.files
        ops, next_new = [], args.files
        for _ in range(count):
            if rng.random() < args.write_ratio:
                ops.append(("w", next_new))
                next_new += 1
            else:
                ops.append(("r", rng.randrange(args.files)))

    def run(op):
        kind, index = op
        start = time.perf_counter_ns()
        try:
            if kind == "w":
                write_one(root, index, args.file_bytes, fsync)
            else:
                read_one(root, index, args.file_bytes)
            return kind, (time.perf_counter_ns() - start) / 1e6, None
        except Exception as error:  # recorded; any error rejects the row
            return kind, None, f"{kind}{index}: {error}"

    def run_timed(op):
        kind, index = op
        syscalls = TimedSyscalls()
        thread_id = threading.get_native_id()
        start = time.monotonic_ns()
        cpu_start = time.thread_time_ns()
        error = None
        try:
            if kind == "w":
                write_one(root, index, args.file_bytes, fsync, syscalls)
            else:
                read_one(root, index, args.file_bytes, syscalls)
        except Exception as caught:
            error = f"{kind}{index}: {caught}"
        cpu_end = time.thread_time_ns()
        end = time.monotonic_ns()
        timing = {"kind": kind, "file_index": index, "thread_native_id": thread_id,
                  "start_monotonic_ns": start, "end_monotonic_ns": end,
                  "elapsed_ns": end - start, "thread_cpu_ns": cpu_end - cpu_start,
                  "success": error is None, "stages": syscalls.stages}
        return kind, (end - start) / 1e6 if error is None else None, error, timing

    if critical_path:
        phase_start = time.monotonic_ns()
        process_cpu_start = time.process_time_ns()
    begin = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(args.workers) as pool:
        results = list(pool.map(run_timed if critical_path else run, ops, chunksize=8))
    seconds = time.perf_counter() - begin
    if critical_path:
        process_cpu_end = time.process_time_ns()
        phase_end = time.monotonic_ns()
        timings = [row[3] for row in results]
        results = [row[:3] for row in results]

    latencies = [lat for _, lat, err in results if err is None]
    errors = [err for _, _, err in results if err is not None]
    by_kind = {}
    for kind in ("w", "r"):
        values = [lat for k, lat, err in results if k == kind and err is None]
        if values:
            by_kind[kind] = {
                "ops": len(values),
                "p50_ms": percentile(values, 50),
                "p95_ms": percentile(values, 95),
                "p99_ms": percentile(values, 99),
            }
    summary = {
        "label": args.label,
        "mode": args.mode,
        "ops": len(ops),
        "files": len(latencies),
        "expected_files": len(ops),
        "errors": len(errors),
        "error_samples": errors[:10],
        "file_bytes": args.file_bytes,
        "workers": args.workers,
        "fsync": fsync,
        "seconds": seconds,
        "files_per_sec": len(latencies) / seconds if seconds > 0 else None,
        "mib_per_sec": len(latencies) * args.file_bytes / seconds / (1 << 20) if seconds > 0 else None,
        "latency_p50_ms": percentile(latencies, 50),
        "latency_p95_ms": percentile(latencies, 95),
        "latency_p99_ms": percentile(latencies, 99),
        "latency_mean_ms": statistics.fmean(latencies) if latencies else None,
        "by_kind": by_kind,
        # Same op list and payload function in both arms -> identical trace.
        "trace_sha256": hashlib.sha256(json.dumps(ops).encode()).hexdigest(),
        "checksum": len(ops) - len(errors),
    }
    if critical_path:
        summary["critical_path"] = {
            "schema": "brewfs-smallfiles-critical-path-v1", "clock": "monotonic_ns",
            "phase_start_monotonic_ns": phase_start, "phase_end_monotonic_ns": phase_end,
            "phase_elapsed_ns": phase_end - phase_start,
            "process_cpu_start_ns": process_cpu_start, "process_cpu_end_ns": process_cpu_end,
            "process_cpu_ns": process_cpu_end - process_cpu_start,
            "notes": ["Per-file elapsed_ns exactly supplies latency_mean_ms and excludes executor queue residence.",
                      "File timers include payload/path work and readback verification; syscall stages are nested.",
                      "Thread/process CPU overlaps elapsed time and is not an additive latency category.",
                      "WRITE fsync behavior is unchanged; READBACK does not issue explicit fsync.",
                      "Phase CPU/window brackets the original executor wall timer and excludes output serialization."]}
        summary["timings"] = timings
    args.json_output.write_text(json.dumps(summary, indent=2) + "\n")
    print(
        f"smallfiles_rw label={args.label} mode={args.mode} ops={len(ops)} errors={len(errors)} "
        f"seconds={seconds:.3f} files_per_sec={summary['files_per_sec']:.1f} "
        f"p50={summary['latency_p50_ms']} p95={summary['latency_p95_ms']} p99={summary['latency_p99_ms']}"
    )
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
