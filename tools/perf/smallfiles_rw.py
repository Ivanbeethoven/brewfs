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
import time

DIRS = 20


def payload(index: int, size: int) -> bytes:
    seed = hashlib.sha256(index.to_bytes(8, "little")).digest()
    return (seed * (size // len(seed) + 1))[:size]


def file_path(root: pathlib.Path, index: int) -> pathlib.Path:
    return root / f"d{index % DIRS:03d}" / f"f{index:07d}"


def write_one(root: pathlib.Path, index: int, size: int, fsync: bool) -> None:
    data = payload(index, size)
    fd = os.open(file_path(root, index), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    try:
        view = memoryview(data)
        while view:
            view = view[os.write(fd, view):]
        if fsync:
            os.fsync(fd)
    finally:
        os.close(fd)


def read_one(root: pathlib.Path, index: int, size: int) -> None:
    fd = os.open(file_path(root, index), os.O_RDONLY)
    try:
        chunks = []
        remaining = size + 1
        while remaining > 0:
            chunk = os.read(fd, min(remaining, 1 << 20))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
    finally:
        os.close(fd)
    if b"".join(chunks) != payload(index, size):
        raise ValueError(f"content mismatch for file {index}")


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

    begin = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(args.workers) as pool:
        results = list(pool.map(run, ops, chunksize=8))
    seconds = time.perf_counter() - begin

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
    args.json_output.write_text(json.dumps(summary, indent=2) + "\n")
    print(
        f"smallfiles_rw label={args.label} mode={args.mode} ops={len(ops)} errors={len(errors)} "
        f"seconds={seconds:.3f} files_per_sec={summary['files_per_sec']:.1f} "
        f"p50={summary['latency_p50_ms']} p95={summary['latency_p95_ms']} p99={summary['latency_p99_ms']}"
    )
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
