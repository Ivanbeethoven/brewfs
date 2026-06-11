#!/usr/bin/env python3
"""Compare BrewFS compose-xfstests perf artifacts.

The script is intentionally local and dependency-free so controllers can run it
after collecting artifacts from multiple candidate branches. It reads the
artifact layout produced by docker/compose-xfstests/run_redis_perf.sh:

  perf-summary.tsv
  post-write-drain.tsv
  results/fio*.json
  diagnostics/stats-*-after.txt
"""

from __future__ import annotations

import argparse
import csv
import json
import pathlib
import sys
from dataclasses import dataclass
from typing import Iterable


BYTES_PER_MIB = 1024.0 * 1024.0


@dataclass(frozen=True)
class Metric:
    kind: str
    item: str
    name: str
    value: float | str
    unit: str = ""


@dataclass(frozen=True)
class Row:
    kind: str
    item: str
    metric: str
    baseline: float | str
    candidate: float | str
    delta_pct: str
    unit: str


def die(message: str, code: int = 2) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(code)


def warn(message: str) -> None:
    print(f"warning: {message}", file=sys.stderr)


def as_float(value: object, default: float = 0.0) -> float:
    try:
        if value is None:
            return default
        return float(value)
    except (TypeError, ValueError):
        return default


def parse_seconds(value: str | None) -> tuple[float, bool] | None:
    if value is None or value == "":
        return None
    if value.startswith("timeout:"):
        seconds = as_float(value.split(":", 1)[1], -1.0)
        return (seconds, True) if seconds >= 0 else None
    seconds = as_float(value, -1.0)
    return (seconds, False) if seconds >= 0 else None


def format_value(value: float | str) -> str:
    if isinstance(value, float):
        return f"{value:.3f}"
    return value


def format_delta(baseline: float | str, candidate: float | str) -> str:
    if not isinstance(baseline, float) or not isinstance(candidate, float):
        return ""
    if baseline == 0:
        if candidate == 0:
            return "+0.0"
        return ""
    delta = (candidate - baseline) / baseline * 100.0
    sign = "+" if delta >= 0 else ""
    return f"{sign}{delta:.1f}"


def percentile(op: dict, pct: int) -> float:
    values = op.get("clat_ns", {}).get("percentile", {})
    return as_float(values.get(f"{pct:.6f}") or values.get(str(pct)))


def op_totals(jobs: list[dict], op_name: str) -> dict[str, float]:
    ops = [job.get(op_name, {}) for job in jobs]
    samples: list[tuple[float, float]] = []
    for op in ops:
        n = as_float(op.get("clat_ns", {}).get("N"))
        if n > 0:
            samples.append((as_float(op.get("clat_ns", {}).get("mean")), n))
    total_n = sum(n for _, n in samples)
    mean_ns = sum(mean * n for mean, n in samples) / total_n if total_n else 0.0
    runtimes = [as_float(op.get("runtime")) for op in ops if as_float(op.get("runtime")) > 0]
    return {
        "bw_bytes": sum(as_float(op.get("bw_bytes")) for op in ops),
        "iops": sum(as_float(op.get("iops")) for op in ops),
        "mean_ns": mean_ns,
        "p95_ns": max((percentile(op, 95) for op in ops), default=0.0),
        "p99_ns": max((percentile(op, 99) for op in ops), default=0.0),
        "p999_ns": max((percentile(op, 99.9) for op in ops), default=0.0),
        "runtime_ms": max(runtimes) if runtimes else 0.0,
    }


def first_options(jobs: list[dict]) -> dict:
    for job in jobs:
        options = job.get("job options", {})
        if options:
            return options
    return {}


def iter_fio_paths(artifact_dir: pathlib.Path) -> list[pathlib.Path]:
    paths: list[pathlib.Path] = []
    for subdir in ("results", "fio"):
        root = artifact_dir / subdir
        if root.exists():
            paths.extend(root.glob("fio*.json"))
    paths.extend(artifact_dir.glob("fio*.json"))
    return sorted({path.resolve(): path for path in paths}.values())


def load_fio_metrics(artifact_dir: pathlib.Path) -> list[Metric]:
    metrics: list[Metric] = []
    for path in iter_fio_paths(artifact_dir):
        try:
            data = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as exc:
            warn(f"{artifact_dir}: skipping unreadable fio JSON {path.name}: {exc}")
            continue
        jobs = data.get("jobs", [])
        if not jobs:
            continue

        item = path.stem
        options = first_options(jobs)
        read = op_totals(jobs, "read")
        write = op_totals(jobs, "write")
        runtime_ms = max(read["runtime_ms"], write["runtime_ms"])

        for prefix, op in (("read", read), ("write", write)):
            if op["bw_bytes"] <= 0:
                continue
            metrics.extend(
                [
                    Metric("fio", item, f"{prefix}_bw_mib_s", op["bw_bytes"] / BYTES_PER_MIB, "MiB/s"),
                    Metric("fio", item, f"{prefix}_iops", op["iops"], "iops"),
                    Metric("fio", item, f"{prefix}_mean_ms", op["mean_ns"] / 1_000_000.0, "ms"),
                    Metric("fio", item, f"{prefix}_p95_ms", op["p95_ns"] / 1_000_000.0, "ms"),
                    Metric("fio", item, f"{prefix}_p99_ms", op["p99_ns"] / 1_000_000.0, "ms"),
                    Metric("fio", item, f"{prefix}_p999_ms", op["p999_ns"] / 1_000_000.0, "ms"),
                ]
            )

        if runtime_ms > 0:
            metrics.append(Metric("fio", item, "active_io_runtime_s", runtime_ms / 1000.0, "s"))
        for opt_name in ("rw", "bs", "numjobs", "direct"):
            if opt_name in options:
                metrics.append(Metric("fio_config", item, opt_name, str(options[opt_name]), ""))
    return metrics


def load_summary_metrics(artifact_dir: pathlib.Path) -> list[Metric]:
    path = artifact_dir / "perf-summary.tsv"
    if not path.exists():
        warn(f"{artifact_dir}: missing perf-summary.tsv")
        return []

    metrics: list[Metric] = []
    try:
        with path.open(newline="") as f:
            rows = list(csv.DictReader(f, delimiter="\t"))
    except OSError as exc:
        warn(f"{artifact_dir}: failed reading perf-summary.tsv: {exc}")
        return []

    for row in rows:
        tool = row.get("tool", "")
        if not tool:
            continue
        if row.get("status"):
            metrics.append(Metric("summary", tool, "status", row["status"], ""))
        seconds = as_float(row.get("seconds"), -1.0)
        if seconds >= 0:
            metrics.append(Metric("summary", tool, "tool_wall_s", seconds, "s"))
    return metrics


def load_drain_metrics(artifact_dir: pathlib.Path) -> list[Metric]:
    path = artifact_dir / "post-write-drain.tsv"
    if not path.exists():
        warn(f"{artifact_dir}: missing post-write-drain.tsv")
        return []

    metrics: list[Metric] = []
    try:
        with path.open(newline="") as f:
            rows = list(csv.DictReader(f, delimiter="\t"))
    except OSError as exc:
        warn(f"{artifact_dir}: failed reading post-write-drain.tsv: {exc}")
        return []

    for row in rows:
        tool = row.get("tool", "")
        if not tool:
            continue
        parsed = parse_seconds(row.get("post_fio_drain_s"))
        if parsed:
            seconds, timed_out = parsed
            metrics.append(Metric("drain", tool, "post_write_drain_s", seconds, "s"))
            metrics.append(Metric("drain", tool, "post_write_drain_timeout", 1.0 if timed_out else 0.0, "bool"))
        for source, name in (
            ("pending_bytes", "drain_pending_mib"),
            ("dirty_bytes", "drain_dirty_mib"),
            ("buffer_dirty_bytes", "drain_buffer_dirty_mib"),
        ):
            value = as_float(row.get(source), -1.0)
            if value >= 0:
                metrics.append(Metric("drain", tool, name, value / BYTES_PER_MIB, "MiB"))
    return metrics


STAT_METRICS = {
    "brewfs_writeback_recent_pending_upload_bytes": ("backpressure_pending_mib", "MiB", BYTES_PER_MIB),
    "brewfs_writeback_dirty_bytes": ("writeback_dirty_mib", "MiB", BYTES_PER_MIB),
    "brewfs_writeback_live_dirty_bytes": ("writeback_live_dirty_mib", "MiB", BYTES_PER_MIB),
    "brewfs_buffer_dirty_bytes": ("buffer_dirty_mib", "MiB", BYTES_PER_MIB),
    "brewfs_writeback_recent_uploaded_bytes": ("writeback_recent_uploaded_mib", "MiB", BYTES_PER_MIB),
    "brewfs_fuse_write_bytes_total": ("fuse_write_mib", "MiB", BYTES_PER_MIB),
    "brewfs_fuse_read_bytes_total": ("fuse_read_mib", "MiB", BYTES_PER_MIB),
    "brewfs_s3_put_ops_total": ("s3_put_ops", "ops", 1.0),
    "brewfs_s3_get_ops_total": ("s3_get_ops", "ops", 1.0),
    "brewfs_s3_put_avg_lat_us": ("s3_put_avg_ms", "ms", 1000.0),
    "brewfs_s3_get_avg_lat_us": ("s3_get_avg_ms", "ms", 1000.0),
    "brewfs_writeback_backpressure_soft_sleep_ops": ("writeback_soft_sleep_ops", "ops", 1.0),
    "brewfs_writeback_backpressure_soft_sleep_us": ("writeback_soft_sleep_ms", "ms", 1000.0),
    "brewfs_writeback_backpressure_hard_wait_ops": ("writeback_hard_wait_ops", "ops", 1.0),
    "brewfs_writeback_backpressure_hard_wait_us": ("writeback_hard_wait_ms", "ms", 1000.0),
}


def parse_stats_file(path: pathlib.Path) -> dict[str, float]:
    parsed: dict[str, float] = {}
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError as exc:
        warn(f"skipping unreadable stats file {path}: {exc}")
        return parsed

    for raw in lines:
        raw = raw.strip()
        if not raw.startswith("brewfs_"):
            continue
        parts = raw.split()
        if len(parts) < 2:
            continue
        try:
            parsed[parts[0]] = float(parts[1])
        except ValueError:
            continue
    return parsed


def stats_item_name(path: pathlib.Path) -> str:
    name = path.name
    if name.startswith("stats-"):
        name = name[len("stats-") :]
    if name.endswith("-after.txt"):
        name = name[: -len("-after.txt")]
    elif name.endswith(".txt"):
        name = name[:-4]
    return name


def load_stats_metrics(artifact_dir: pathlib.Path) -> list[Metric]:
    diag_dir = artifact_dir / "diagnostics"
    if not diag_dir.exists():
        warn(f"{artifact_dir}: missing diagnostics directory")
        return []

    metrics: list[Metric] = []
    for path in sorted(diag_dir.glob("stats-*-after.txt")):
        item = stats_item_name(path)
        raw_metrics = parse_stats_file(path)
        for raw_name, (metric_name, unit, divisor) in STAT_METRICS.items():
            if raw_name in raw_metrics:
                metrics.append(Metric("stats", item, metric_name, raw_metrics[raw_name] / divisor, unit))
    return metrics


def load_artifact(artifact_dir: pathlib.Path) -> dict[tuple[str, str, str], Metric]:
    if not artifact_dir.exists():
        die(f"missing artifact directory: {artifact_dir}")
    if not artifact_dir.is_dir():
        die(f"artifact path is not a directory: {artifact_dir}")

    metrics: list[Metric] = []
    metrics.extend(load_fio_metrics(artifact_dir))
    metrics.extend(load_summary_metrics(artifact_dir))
    metrics.extend(load_drain_metrics(artifact_dir))
    metrics.extend(load_stats_metrics(artifact_dir))

    if not metrics:
        die(
            f"artifact has no comparable perf data: {artifact_dir} "
            "(expected results/fio*.json, perf-summary.tsv, post-write-drain.tsv, "
            "or diagnostics/stats-*-after.txt)"
        )

    return {(metric.kind, metric.item, metric.name): metric for metric in metrics}


def compare_metrics(
    baseline: dict[tuple[str, str, str], Metric],
    candidate: dict[tuple[str, str, str], Metric],
) -> list[Row]:
    rows: list[Row] = []
    for key in sorted(set(baseline) & set(candidate)):
        base = baseline[key]
        cand = candidate[key]
        unit = cand.unit or base.unit
        rows.append(
            Row(
                kind=key[0],
                item=key[1],
                metric=key[2],
                baseline=base.value,
                candidate=cand.value,
                delta_pct=format_delta(base.value, cand.value),
                unit=unit,
            )
        )
    return rows


def emit_tsv(rows: Iterable[Row]) -> str:
    lines = ["kind\titem\tmetric\tbaseline\tcandidate\tdelta_pct\tunit"]
    for row in rows:
        lines.append(
            "\t".join(
                [
                    row.kind,
                    row.item,
                    row.metric,
                    format_value(row.baseline),
                    format_value(row.candidate),
                    row.delta_pct,
                    row.unit,
                ]
            )
        )
    return "\n".join(lines) + "\n"


def markdown_table(rows: list[Row]) -> list[str]:
    lines = [
        "| Item | Metric | Baseline | Candidate | Delta | Unit |",
        "| --- | --- | ---: | ---: | ---: | --- |",
    ]
    for row in rows:
        lines.append(
            f"| {row.item} | {row.metric} | {format_value(row.baseline)} | "
            f"{format_value(row.candidate)} | {row.delta_pct} | {row.unit} |"
        )
    return lines


def emit_markdown(
    rows: list[Row],
    baseline_label: str,
    candidate_label: str,
    baseline_path: pathlib.Path,
    candidate_path: pathlib.Path,
) -> str:
    lines = [
        "# BrewFS Perf Artifact Comparison",
        "",
        f"Baseline: `{baseline_label}`",
        f"Candidate: `{candidate_label}`",
        "",
        f"- Baseline artifact: `{baseline_path}`",
        f"- Candidate artifact: `{candidate_path}`",
    ]

    groups = [
        ("summary", "## Summary"),
        ("fio_config", "## Fio Config"),
        ("fio", "## Fio"),
        ("drain", "## Drain And Backpressure"),
        ("stats", "## BrewFS Stats"),
    ]
    for kind, heading in groups:
        group_rows = [row for row in rows if row.kind == kind]
        if not group_rows:
            continue
        lines.extend(["", heading, ""])
        lines.extend(markdown_table(group_rows))
    return "\n".join(lines) + "\n"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compare BrewFS perf-run artifacts, including fio, post-write drain, and BrewFS stats."
    )
    parser.add_argument("baseline", type=pathlib.Path, help="Baseline/current perf-run artifact directory")
    parser.add_argument("candidate", type=pathlib.Path, help="Candidate perf-run artifact directory")
    parser.add_argument(
        "--format",
        choices=("markdown", "tsv"),
        default="markdown",
        help="Output format (default: markdown)",
    )
    parser.add_argument("--baseline-label", default=None, help="Label to show for the baseline artifact")
    parser.add_argument("--candidate-label", default=None, help="Label to show for the candidate artifact")
    parser.add_argument("-o", "--output", type=pathlib.Path, help="Write output to a file instead of stdout")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    baseline_path = args.baseline.resolve()
    candidate_path = args.candidate.resolve()

    baseline = load_artifact(baseline_path)
    candidate = load_artifact(candidate_path)
    rows = compare_metrics(baseline, candidate)
    if not rows:
        die(f"no comparable metrics found between {baseline_path} and {candidate_path}")

    baseline_label = args.baseline_label or baseline_path.name
    candidate_label = args.candidate_label or candidate_path.name
    if args.format == "tsv":
        output = emit_tsv(rows)
    else:
        output = emit_markdown(rows, baseline_label, candidate_label, baseline_path, candidate_path)

    if args.output:
        args.output.write_text(output)
    else:
        sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
