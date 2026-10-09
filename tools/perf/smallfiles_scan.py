#!/usr/bin/env python3
"""Matched recursive small-file scanner for BrewFS and reference filesystems."""

from __future__ import annotations

import argparse
from array import array
import hashlib
import json
import math
import os
import pathlib
import random
import resource
import stat
import sys
import time
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from dataclasses import dataclass


@dataclass(frozen=True)
class FileSpec:
    path: pathlib.Path
    file_number: int
    expected_size: int


@dataclass
class FileResult:
    ok: bool
    logical_bytes: int = 0
    payload_bytes: int = 0
    checksum: int = 0
    latency_ns: int = 0
    error: str = ""


def expected_size(file_number: int, minimum: int, maximum: int) -> int:
    span = maximum - minimum + 1
    mixed = (
        file_number * 6_364_136_223_846_793_005 + 1_442_695_040_888_963_407
    ) & ((1 << 64) - 1)
    return minimum + (mixed % span if span else 0)


def expected_pattern(file_number: int) -> bytes:
    seed = (file_number + 1).to_bytes(8, "little")
    return bytes(byte ^ (0x80 if index & 1 else 0) for index, byte in enumerate(seed))


def expected_payload_chunk(pattern: bytes, offset: int, length: int) -> bytes:
    start = offset % len(pattern)
    repetitions = math.ceil((start + length) / len(pattern))
    return (pattern * repetitions)[start : start + length]


def trace_sha256(specs: list[FileSpec]) -> str:
    hasher = hashlib.sha256()
    for spec in specs:
        hasher.update(f"{spec.file_number}:{spec.expected_size}\n".encode())
    return hasher.hexdigest()


def percentile_ns(values: list[int], percentile: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    rank = max(0, min(len(ordered) - 1, math.ceil(percentile * len(ordered)) - 1))
    return ordered[rank]


def discover_files(
    root: pathlib.Path,
    levels: int,
    fanout: int,
    files_per_leaf: int,
    minimum: int,
    maximum: int,
    ignored_root_files: frozenset[str],
) -> tuple[list[FileSpec], int, int]:
    files: list[FileSpec] = []
    directories = 0
    leaf_directories = 0
    for directory, dirs, names in os.walk(root):
        dirs[:] = sorted(name for name in dirs if name.startswith("d"))
        names = sorted(name for name in names if not name.startswith("."))
        relative = pathlib.Path(directory).relative_to(root)
        depth = len(relative.parts)
        if depth == 0:
            names = [name for name in names if name not in ignored_root_files]
        if depth:
            directories += 1
        if depth < levels:
            if names or len(dirs) != fanout:
                raise ValueError(
                    f"internal shape mismatch path={directory} depth={depth} "
                    f"dirs={len(dirs)} files={len(names)}"
                )
            continue
        if depth != levels or dirs or len(names) != files_per_leaf:
            raise ValueError(
                f"leaf shape mismatch path={directory} depth={depth} "
                f"dirs={len(dirs)} files={len(names)}"
            )
        leaf_directories += 1
        leaf_index = 0
        for component in relative.parts:
            if len(component) < 2 or component[0] != "d":
                raise ValueError(f"invalid directory component {component!r}")
            leaf_index = leaf_index * fanout + int(component[1:])
        for name in names:
            if len(name) < 2 or name[0] != "f":
                raise ValueError(f"invalid file name {name!r}")
            file_index = int(name[1:])
            file_number = leaf_index * files_per_leaf + file_index
            files.append(
                FileSpec(
                    path=pathlib.Path(directory) / name,
                    file_number=file_number,
                    expected_size=expected_size(file_number, minimum, maximum),
                )
            )
    return files, directories, leaf_directories


def scan_file(spec: FileSpec, mode: str, chunk_bytes: int) -> FileResult:
    started = time.perf_counter_ns()
    try:
        metadata = spec.path.stat()
        if not stat.S_ISREG(metadata.st_mode):
            raise OSError("not a regular file")
        if metadata.st_size != spec.expected_size:
            raise OSError(f"size={metadata.st_size} expected={spec.expected_size}")
        if mode == "stat":
            return FileResult(
                ok=True,
                logical_bytes=spec.expected_size,
                latency_ns=time.perf_counter_ns() - started,
            )

        pattern = expected_pattern(spec.file_number)
        payload_bytes = 0
        first_byte = 0
        with spec.path.open("rb", buffering=0) as stream:
            while True:
                data = stream.read(chunk_bytes)
                if not data:
                    break
                if payload_bytes == 0:
                    first_byte = data[0]
                expected = expected_payload_chunk(pattern, payload_bytes, len(data))
                if data != expected:
                    raise OSError(f"content mismatch at offset {payload_bytes}")
                payload_bytes += len(data)
        if payload_bytes != spec.expected_size:
            raise OSError(f"short read={payload_bytes} expected={spec.expected_size}")
        return FileResult(
            ok=True,
            logical_bytes=spec.expected_size,
            payload_bytes=payload_bytes,
            checksum=first_byte,
            latency_ns=time.perf_counter_ns() - started,
        )
    except (OSError, ValueError) as error:
        return FileResult(
            ok=False,
            latency_ns=time.perf_counter_ns() - started,
            error=f"path={spec.path} error={error}",
        )


def scan_batch(batch: list[FileSpec], mode: str, chunk_bytes: int) -> list[FileResult]:
    return [scan_file(spec, mode, chunk_bytes) for spec in batch]


def scan_specs_bounded(
    specs: list[FileSpec],
    mode: str,
    chunk_bytes: int,
    workers: int,
    batch_size: int,
    max_inflight_batches: int,
) -> tuple[int, int, int, int, list[str], array]:
    successful = logical_bytes = payload_bytes = checksum = 0
    errors: list[str] = []
    latencies = array("Q")
    batches = (
        specs[offset : offset + batch_size]
        for offset in range(0, len(specs), batch_size)
    )
    max_pending = workers * max_inflight_batches
    with ThreadPoolExecutor(max_workers=workers) as executor:
        pending = set()
        for _ in range(max_pending):
            try:
                batch = next(batches)
            except StopIteration:
                break
            pending.add(executor.submit(scan_batch, batch, mode, chunk_bytes))
        while pending:
            completed, pending = wait(pending, return_when=FIRST_COMPLETED)
            for future in completed:
                for result in future.result():
                    latencies.append(result.latency_ns)
                    if result.ok:
                        successful += 1
                        logical_bytes += result.logical_bytes
                        payload_bytes += result.payload_bytes
                        checksum = (checksum + result.checksum) & 0xFFFFFFFF
                    else:
                        errors.append(result.error)
                try:
                    batch = next(batches)
                except StopIteration:
                    continue
                pending.add(executor.submit(scan_batch, batch, mode, chunk_bytes))
    return successful, logical_bytes, payload_bytes, checksum, errors, latencies


def emit_summary(summary: dict[str, object]) -> None:
    rendered = " ".join(
        f"{key}={value:.6f}" if isinstance(value, float) else f"{key}={value}"
        for key, value in summary.items()
    )
    print(f"smallfiles_scan_summary {rendered}", flush=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--label", default="smallfiles")
    parser.add_argument("--mode", choices=("tree", "stat", "full"), default="full")
    parser.add_argument("--expected-files", type=int, required=True)
    parser.add_argument("--min-size", type=int, required=True)
    parser.add_argument("--max-size", type=int, required=True)
    parser.add_argument("--dir-levels", type=int, required=True)
    parser.add_argument("--dirs-per-level", type=int, required=True)
    parser.add_argument("--files-per-leaf", type=int, required=True)
    parser.add_argument("--workers", type=int, default=16)
    parser.add_argument("--chunk-bytes", type=int, default=1024 * 1024)
    parser.add_argument("--order", choices=("lexicographic", "shuffle"), default="lexicographic")
    parser.add_argument("--shuffle-seed", type=int, default=20261001)
    parser.add_argument("--epochs", type=int, default=1)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--max-inflight-batches", type=int, default=2)
    parser.add_argument("--ignore-root-file", action="append", default=[])
    parser.add_argument("--json-output", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if (
        args.expected_files < 0
        or args.min_size <= 0
        or args.min_size > args.max_size
        or args.dir_levels < 0
        or args.dirs_per_level <= 0
        or args.files_per_leaf <= 0
        or args.workers <= 0
        or args.chunk_bytes <= 0
        or args.epochs <= 0
        or args.batch_size <= 0
        or args.max_inflight_batches <= 0
    ):
        print("invalid scanner bounds", file=sys.stderr)
        return 2

    expected_directories = sum(
        args.dirs_per_level**level for level in range(1, args.dir_levels + 1)
    )
    expected_leaf_directories = args.dirs_per_level**args.dir_levels
    started = time.monotonic()
    try:
        specs, directories, leaf_directories = discover_files(
            args.root,
            args.dir_levels,
            args.dirs_per_level,
            args.files_per_leaf,
            args.min_size,
            args.max_size,
            frozenset(args.ignore_root_file),
        )
    except (OSError, ValueError) as error:
        print(f"smallfiles discovery failed: {error}", file=sys.stderr)
        return 1

    discovery_finished = time.monotonic()
    if args.mode == "tree":
        summary = {
            "label": args.label,
            "mode": args.mode,
            "order": args.order,
            "shuffle_seed": args.shuffle_seed if args.order == "shuffle" else 0,
            "epoch": 1,
            "epochs": 1,
            "files": len(specs),
            "expected_files": args.expected_files,
            "stat_calls": 0,
            "directories": directories,
            "expected_directories": expected_directories,
            "leaf_directories": leaf_directories,
            "expected_leaf_directories": expected_leaf_directories,
            "workers": args.workers,
            "batch_size": args.batch_size,
            "logical_bytes": 0,
            "payload_bytes": 0,
            "errors": 0,
            "checksum": 0,
            "trace_sha256": trace_sha256(specs),
            "discovery_seconds": discovery_finished - started,
            "seconds": discovery_finished - started,
            "files_per_sec": len(specs) / (discovery_finished - started),
            "mib_per_sec": 0.0,
            "latency_p50_ms": 0.0,
            "latency_p95_ms": 0.0,
            "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        }
        summaries = [summary]
        emit_summary(summary)
    else:
        summaries = []
        base_specs = list(specs)
        for epoch in range(args.epochs):
            epoch_specs = list(base_specs)
            if args.order == "shuffle":
                random.Random(args.shuffle_seed + epoch).shuffle(epoch_specs)
            epoch_started = started if epoch == 0 else time.monotonic()
            (
                successful,
                logical_bytes,
                payload_bytes,
                checksum,
                errors,
                latencies,
            ) = scan_specs_bounded(
                epoch_specs,
                args.mode,
                args.chunk_bytes,
                args.workers,
                args.batch_size,
                args.max_inflight_batches,
            )
            elapsed = time.monotonic() - epoch_started
            for error in errors:
                print(error)
            summary = {
                "label": args.label,
                "mode": args.mode,
                "order": args.order,
                "shuffle_seed": args.shuffle_seed if args.order == "shuffle" else 0,
                "epoch": epoch + 1,
                "epochs": args.epochs,
                "files": successful,
                "expected_files": args.expected_files,
                "stat_calls": len(epoch_specs),
                "directories": directories,
                "expected_directories": expected_directories,
                "leaf_directories": leaf_directories,
                "expected_leaf_directories": expected_leaf_directories,
                "workers": args.workers,
                "batch_size": args.batch_size,
                "max_inflight_batches": args.max_inflight_batches,
                "logical_bytes": logical_bytes,
                "payload_bytes": payload_bytes,
                "errors": len(errors),
                "checksum": checksum,
                "trace_sha256": trace_sha256(epoch_specs),
                "discovery_seconds": discovery_finished - started,
                "seconds": elapsed,
                "files_per_sec": successful / elapsed if elapsed else 0.0,
                "mib_per_sec": payload_bytes / elapsed / (1024 * 1024) if elapsed else 0.0,
                "latency_p50_ms": percentile_ns(latencies, 0.50) / 1_000_000,
                "latency_p95_ms": percentile_ns(latencies, 0.95) / 1_000_000,
                "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
            }
            summaries.append(summary)
            emit_summary(summary)
            if (
                successful != args.expected_files
                or len(epoch_specs) != args.expected_files
                or errors
                or (args.mode == "full" and payload_bytes != logical_bytes)
            ):
                return 1

    if args.json_output is not None:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        payload = summaries[0] if len(summaries) == 1 else {"epochs": summaries}
        args.json_output.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n")

    valid = (
        len(specs) == args.expected_files
        and directories == expected_directories
        and leaf_directories == expected_leaf_directories
        and all(summary["files"] == args.expected_files for summary in summaries)
        and all(summary["errors"] == 0 for summary in summaries)
    )
    return 0 if valid else 1


if __name__ == "__main__":
    raise SystemExit(main())
