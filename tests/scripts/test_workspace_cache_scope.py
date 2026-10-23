#!/usr/bin/env python3
"""Real FUSE + RustFS regression; requires an overlay-enabled brewfs binary.

Run as root (fresh workspace roots are owned by uid 0). Uses two fresh SQLite
catalogs, three workspaces and UUID-named S3 buckets.
The endpoint must be an isolated loopback RustFS, with AWS_ACCESS_KEY_ID and
AWS_SECRET_ACCESS_KEY set. Logs and catalogs remain in --artifacts. Only this
test's mounts and buckets are cleaned up, including on assertion failure.
"""

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import time
import urllib.parse
import uuid
import xml.etree.ElementTree as ET


def run(argv, **kwargs):
    return subprocess.run(argv, check=True, timeout=60, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    assert os.geteuid() == 0, "Run as root: fresh workspace root inodes are owned by uid 0"
    endpoint = urllib.parse.urlparse(args.endpoint)
    assert endpoint.scheme == "http" and endpoint.hostname == "127.0.0.1" and endpoint.port
    args.artifacts.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, RUST_LOG="warn", AWS_EC2_METADATA_DISABLED="true",
               AWS_DEFAULT_REGION="us-east-1", TOKIO_WORKER_THREADS="2")
    buckets = []
    cases = []
    poisoned = {}

    def s3(method, bucket, key=""):
        # Do not print credentials or include them in case logs.
        return run(["curl", "--silent", "--show-error", "--fail", "--max-time", "30",
                    "--aws-sigv4", "aws:amz:us-east-1:s3", "--user",
                    f"{env['AWS_ACCESS_KEY_ID']}:{env['AWS_SECRET_ACCESS_KEY']}",
                    "-X", method, f"{args.endpoint}/{bucket}/{urllib.parse.quote(key)}"],
                   capture_output=True).stdout

    def cli(label, db, *command):
        out = run([str(args.binary), "workspace", *command, "--meta-backend", "sqlx",
                   "--meta-url", f"sqlite://{db}"], env=env, capture_output=True)
        (args.artifacts / f"{label}.log").write_bytes(out.stdout + out.stderr)
        raw = re.sub(r"\x1b\[[0-9;]*m", "", out.stdout.decode())
        return json.loads(raw[raw.index("{"):] if not raw.lstrip().startswith("[") else raw)

    @contextlib.contextmanager
    def mount(case, cache, label, promoted=False):
        mp = args.artifacts / label
        mp.mkdir()
        config = args.artifacts / f"{label}.json"
        # JSON is also valid YAML. Tiny budgets keep concurrent smoke mounts cheap.
        config.write_text(json.dumps({
            "mount_point": str(mp), "volume_format": "workspace-v1",
            "workspace": case["workspace"],
            "meta": {"backend": "sqlx", "sqlx": {"url": f"sqlite://{case['db']}"}},
            "data": {"backend": "s3", "s3": {
                "bucket": case["bucket"], "endpoint": args.endpoint,
                "region": "us-east-1", "force_path_style": True}},
            "cache": {"cache_root": str(cache), "read_memory_bytes": 1048576,
                      "read_ssd_bytes": 1048576, "prefetch_enabled": False,
                      "range_background_prefetch": False,
                      "persist_write_cache_after_upload": promoted,
                      "writeback_mode": "commit_before_upload" if promoted else "upload_before_commit"}
        }))
        with (args.artifacts / f"{label}.log").open("wb") as log:
            process = subprocess.Popen([str(args.binary), "mount", "--config", str(config)],
                                       env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 30
                while not os.path.ismount(mp):
                    assert process.poll() is None, f"mount exited: {label}; see its log"
                    assert time.monotonic() < deadline, f"mount timed out: {label}"
                    time.sleep(0.1)
                yield mp
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                try:
                    status = process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    if os.path.ismount(mp):
                        run(["fusermount3", "-u", str(mp)])
                    process.kill()
                    process.wait()
                    raise AssertionError(f"mount teardown timed out: {label}")
                if os.path.ismount(mp):
                    run(["fusermount3", "-u", str(mp)])
                    raise AssertionError(f"process exited without unmounting: {label}")
                assert not os.path.ismount(mp), f"mount leaked: {label}"
                assert status == 0, f"mount failed: {label}, exit={status}"

    def poison(root, slice_id, promoted):
        chunks = root / "chunks"
        chunks.mkdir(parents=True, exist_ok=True)
        name = (f".writeback-slice-{slice_id}" if promoted else
                hashlib.sha256(f"chunks/{slice_id}/0".encode()).hexdigest())
        path = chunks / name
        # Legacy raw blocks are valid cache entries, not checksum corruption.
        path.write_bytes(b"WRONG!!!")
        poisoned[path] = path.read_bytes()

    try:
        for volume_index in range(2):
            bucket = f"brewfs-cache-test-{uuid.uuid4()}"
            s3("PUT", bucket)
            buckets.append(bucket)
            db = args.artifacts / f"volume-{volume_index}.db"
            parent = cli(f"init-{volume_index}", db, "init-volume")
            workspace_ids = [parent["workspace_id"]]
            if volume_index == 1:
                revision = parent["fork_base"]
                encoded = (f"{revision['layer_id']}:{revision['sealed_version']}:"
                           + bytes(revision["root_hash"]).hex())
                forked = cli("fork", db, "fork", encoded, "--count", "1")
                workspace_ids.append(forked[0]["workspace_id"])
            with sqlite3.connect(db) as sql:
                volume = str(uuid.UUID(bytes=sql.execute(
                    "SELECT volume_id FROM ws_v1_volume_header").fetchone()[0]))
            for workspace in workspace_ids:
                index = len(cases) + 1
                case = dict(db=db, bucket=bucket, volume=volume, workspace=workspace,
                            data=f"agent-{index}\n".encode(), filename=f"agent-{index}.txt")
                with mount(case, args.artifacts / f"seed-cache-{index}", f"seed-{index}") as mp:
                    with (mp / case["filename"]).open("wb") as f:
                        f.write(case["data"])
                        f.flush()
                        os.fsync(f.fileno())
                    assert (mp / case["filename"]).read_bytes() == case["data"]
                with sqlite3.connect(db) as sql:
                    case["slice"] = sql.execute(
                        "SELECT MAX(slice_id) FROM ws_v1_data_extent_delta").fetchone()[0]
                cases.append(case)
        assert cases[0]["slice"] == cases[1]["slice"], "fixture must collide across volumes"
        print("COLLIDING_SLICE", cases[0]["slice"], flush=True)
        for promoted in [False, True]:
            cache = args.artifacts / ("promoted-cache" if promoted else "block-cache")
            for case in cases:
                poison(cache, case["slice"], promoted)
            # A sibling and another volume hold valid-but-wrong cache entries
            # with the target's exact block/slice key. Neither may be reused.
            target = cases[2]
            poison(cache / "workspace-read-v1" / target["volume"] / cases[1]["workspace"],
                   target["slice"], promoted)
            poison(cache / "workspace-read-v1" / cases[0]["volume"] / target["workspace"],
                   target["slice"], promoted)
            for remount in range(2):
                for index, case in enumerate(cases):
                    label = f"verify-{int(promoted)}-{remount}-{index}"
                    with mount(case, cache, label, promoted) as mp:
                        actual = (mp / case["filename"]).read_bytes()
                        assert actual == case["data"], f"{label}: wrong bytes {actual!r}"
                        for other in cases:
                            if other is not case:
                                assert not (mp / other["filename"]).exists(), "workspace leak"
                        print(label, case["workspace"], repr(actual),
                              hashlib.sha256(actual).hexdigest(), "NO_LEAK", flush=True)
            for path, original in poisoned.items():
                assert path.read_bytes() == original, f"poisoned cache was modified: {path}"
        print("PASS: blocks, promoted slices, volume/workspace isolation, remount, stale cache preserved")
    finally:
        for bucket in buckets:
            listing = ET.fromstring(s3("GET", bucket))
            for key in listing.findall(".//{*}Contents/{*}Key"):
                s3("DELETE", bucket, key.text)
            s3("DELETE", bucket)
        print("CLEANUP: test mounts stopped and owned RustFS buckets deleted", flush=True)


if __name__ == "__main__":
    main()
