#!/usr/bin/env python3
"""Bounded real partial-read validation using the shared independent fixture pattern."""
import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import random
import resource
import time

from smallfiles_scan import FileSpec, discover_files, expected_pattern, expected_payload_chunk, percentile_ns


def requests(spec: FileSpec, sizes: list[int], seed: int):
    rng = random.Random(seed ^ spec.file_number)
    for size in sizes:
        positions = {0, rng.randrange(spec.expected_size + 1), max(0, spec.expected_size - size // 2), spec.expected_size}
        for boundary in (256 * 1024, 1024 * 1024, 4 * 1024 * 1024):
            if boundary < spec.expected_size:
                positions.add(max(0, boundary - min(2048, size // 2)))
        for offset in sorted(positions):
            yield offset, size


def scan_one(spec: FileSpec, sizes: list[int], seed: int):
    count = received = checksum = 0
    latency = []
    with spec.path.open('rb', buffering=0) as stream:
        if os.fstat(stream.fileno()).st_size != spec.expected_size:
            raise ValueError(f'file {spec.file_number}: size mismatch')
        pattern = expected_pattern(spec.file_number)
        for offset, size in requests(spec, sizes, seed):
            started = time.perf_counter_ns()
            expected = min(size, max(0, spec.expected_size - offset))
            data = bytearray()
            while len(data) < expected:
                chunk = os.pread(stream.fileno(), expected - len(data), offset + len(data))
                if not chunk:
                    raise ValueError(f'file {spec.file_number}: short partial read')
                data.extend(chunk)
            if expected == 0 and os.pread(stream.fileno(), size, offset):
                raise ValueError(f'file {spec.file_number}: nonempty EOF read')
            if data != expected_payload_chunk(pattern, offset, expected):
                raise ValueError(f'file {spec.file_number}: content mismatch at {offset}')
            latency.append(time.perf_counter_ns() - started)
            count += 1
            received += len(data)
            checksum += sum(data)
    return count, received, checksum, latency


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=pathlib.Path, required=True)
    parser.add_argument('--expected-files', type=int, required=True)
    parser.add_argument('--min-size', type=int, required=True)
    parser.add_argument('--max-size', type=int, required=True)
    parser.add_argument('--dir-levels', type=int, default=2)
    parser.add_argument('--dirs-per-level', type=int, default=10)
    parser.add_argument('--files-per-leaf', type=int, default=100)
    parser.add_argument('--workers', type=int, default=16)
    parser.add_argument('--max-files', type=int, default=1024)
    parser.add_argument('--read-sizes', default='4096,65536,204800')
    parser.add_argument('--seed', type=int, default=20261003)
    parser.add_argument('--json-output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    sizes = [int(x) for x in args.read_sizes.split(',')]
    if not 1 <= args.workers <= 64 or not 1 <= args.max_files <= 10000 or not sizes or any(x <= 0 or x > 1024 * 1024 for x in sizes):
        parser.error('partial workload exceeds bounded controls')
    discovery = time.monotonic()
    specs, dirs, leaves = discover_files(args.root, args.dir_levels, args.dirs_per_level, args.files_per_leaf, args.min_size, args.max_size, frozenset())
    if len(specs) != args.expected_files:
        raise ValueError('namespace file count mismatch')
    selected = random.Random(args.seed).sample(specs, min(args.max_files, len(specs)))
    trace = hashlib.sha256()
    for spec in selected:
        trace.update(json.dumps([spec.file_number, spec.expected_size, list(requests(spec, sizes, args.seed))], separators=(',', ':')).encode())
    discovery_seconds = time.monotonic() - discovery
    started = time.monotonic()
    reads = received = checksum = successful = 0
    latency = []
    errors = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        # At most two bounded batches of files per worker, not all namespace futures.
        iterator = iter(selected)
        pending = {}
        while True:
            while len(pending) < args.workers * 2:
                try:
                    spec = next(iterator)
                except StopIteration:
                    break
                pending[pool.submit(scan_one, spec, sizes, args.seed)] = spec.file_number
            if not pending:
                break
            done, _ = concurrent.futures.wait(pending, return_when=concurrent.futures.FIRST_COMPLETED)
            for future in done:
                number = pending.pop(future)
                try:
                    count, data_bytes, value, times = future.result()
                    reads += count
                    received += data_bytes
                    checksum += value
                    latency.extend(times)
                    successful += 1
                except Exception as error:
                    errors.append(dict(file_number=number, error=type(error).__name__))
    seconds = time.monotonic() - started
    summary = dict(mode='partial', files=successful, selected_files=len(selected), namespace_files=len(specs), directories=dirs, leaf_directories=leaves, reads=reads, payload_bytes=received, checksum=checksum, errors=len(errors), error_samples=errors[:10], seconds=seconds, discovery_seconds=discovery_seconds, workers=args.workers, read_sizes=sizes, seed=args.seed, trace_sha256=trace.hexdigest(), reads_per_second=reads / seconds if seconds else 0, logical_mib_per_second=received / 1048576 / seconds if seconds else 0, latency_p50_ms=percentile_ns(latency,.5)/1e6, latency_p95_ms=percentile_ns(latency,.95)/1e6, latency_p99_ms=percentile_ns(latency,.99)/1e6, peak_rss_kib=resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    args.json_output.parent.mkdir(parents=True, exist_ok=True)
    args.json_output.write_text(json.dumps(summary, sort_keys=True, indent=2) + '\n')
    print(json.dumps(summary, sort_keys=True))
    return int(bool(errors) or successful != len(selected))


if __name__ == '__main__':
    raise SystemExit(main())
