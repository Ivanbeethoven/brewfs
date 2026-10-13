#!/usr/bin/env python3
"""Summarize one paired native-vs-packed group from its run artifacts."""
import json, pathlib, statistics, sys
group = pathlib.Path(sys.argv[1]); status = int(sys.argv[2])
rows = []
for run in sorted((group / "runs").iterdir()):
    if not (run / "pair-entry.json").exists():
        # Interrupted before the run recorded its own entry: keep it as an
        # explicit failed row instead of dropping it from the evidence.
        rep, _, arm = run.name.partition("-")
        rows.append({"rep": int(rep.lstrip("r") or 0), "arm": arm, "exit": None, "artifact": "", "interrupted": True})
        continue
    entry = json.loads((run / "pair-entry.json").read_text())
    art = pathlib.Path(entry["artifact"]) if entry.get("artifact") else None
    if art and (art / "summary.json").exists() and (art / "timing.json").exists():
        s = json.loads((art / "summary.json").read_text())
        epochs = s["epochs"] if isinstance(s.get("epochs"), list) else [s]
        t = json.loads((art / "timing.json").read_text())
        entry.update(
            files_ok=all(e["files"] == e["expected_files"] and e["errors"] == 0 for e in epochs),
            checksum=[e.get("checksum") for e in epochs],
            trace_sha256=[e.get("trace_sha256") for e in epochs],
            p50_ms=[e.get("latency_p50_ms") for e in epochs],
            p95_ms=[e.get("latency_p95_ms") for e in epochs],
            p99_ms=[e.get("latency_p99_ms") for e in epochs],
            files_per_sec=[e.get("files_per_sec") for e in epochs],
            mount_s=t["mount_seconds"], active_s=t["active_seconds"],
            drain_s=t["drain_seconds"], total_s=t["total_seconds"],
            binary_sha256=(art / "binary-sha256.txt").read_text().split()[0])
        imp = art / "native-import.json"
        if imp.exists():
            entry["native_import_s"] = json.loads(imp.read_text())["native_import_seconds"]
    rows.append(entry)
ok = lambda r: r.get("exit") == 0 and r.get("files_ok")
arms = {}
for r in rows:
    arms.setdefault(r["arm"], []).append(r)
def stats(rs, key, idx=-1):
    vals = [r[key][idx] if isinstance(r[key], list) else r[key] for r in rs if ok(r)]
    return {"n": len(vals), "median": statistics.median(vals) if vals else None,
            "min": min(vals) if vals else None, "max": max(vals) if vals else None}
summary = {"schema": "brewfs-packed-paired-v1", "group": str(group), "arms": {}}
for arm, rs in arms.items():
    summary["arms"][arm] = {k: stats(rs, k) for k in ("active_s", "mount_s", "drain_s", "total_s", "p50_ms", "p95_ms", "p99_ms", "files_per_sec")}
    summary["arms"][arm]["runs"] = len(rs); summary["arms"][arm]["valid"] = sum(ok(r) for r in rs)
# Each epoch has its own seeded shuffle, so a run carries one trace hash per
# epoch. Identity means every run has the same per-epoch trace sequence.
traces = {tuple(r.get("trace_sha256", [])) for r in rows if ok(r)}
checks = {tuple(r.get("checksum", [])) for r in rows if ok(r)}
binaries = {r.get("binary_sha256") for r in rows if r.get("binary_sha256")}
meta = json.loads((group / "group.json").read_text())
all_valid = all(ok(r) for r in rows) and len(rows) == len(meta["arms"]) * int(meta["repeats"])
summary.update(trace_sha256=[list(t) for t in sorted(traces)], checksums_identical=len(checks) == 1,
               binary_sha256=sorted(binaries), rows=rows,
               status="passed" if status == 0 and all_valid and len(traces) == 1 and len(checks) == 1 and len(binaries) == 1 else "rejected")
(group / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(f"paired status={summary['status']} group={group}")
