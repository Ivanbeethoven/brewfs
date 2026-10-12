#!/usr/bin/env python3
"""Record contemporaneous resources for the workspace-control compose runner."""

from __future__ import annotations

import argparse
import datetime as _datetime
import json
import pathlib
import subprocess
import tempfile
from typing import Any


SCHEMA = "workspace-control-resource-journal-v1"


class JournalError(ValueError):
    """Raised when the journal cannot be updated or validated."""


def _now() -> str:
    return _datetime.datetime.now(_datetime.timezone.utc).isoformat()


def _atomic_write(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write("\n")
        temporary = pathlib.Path(stream.name)
    temporary.replace(path)


def _read(path: pathlib.Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise JournalError(f"invalid resource journal: {error}") from error
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise JournalError("unsupported resource journal schema")
    if not isinstance(value.get("events"), list) or not value["events"]:
        raise JournalError("resource journal has no events")
    return value


def _token(name: str, value: str) -> str:
    if not isinstance(value, str) or not value.strip() or any(c.isspace() for c in value):
        raise JournalError(f"{name} must be a non-empty token")
    return value


def init_journal(
    path: pathlib.Path,
    *,
    run_id: str,
    artifact: str,
    project: str,
    backend: str,
    namespace: str,
) -> None:
    if path.exists():
        raise JournalError("resource journal already exists")
    for name, value in (
        ("run_id", run_id),
        ("artifact", artifact),
        ("compose_project", project),
        ("backend", backend),
        ("namespace", namespace),
    ):
        _token(name, value)
    _atomic_write(
        path,
        {
            "schema": SCHEMA,
            "run_id": run_id,
            "owned": {
                "artifact": artifact,
                "compose_project": project,
                "backend": backend,
                "namespace": namespace,
            },
            "events": [{"name": "init", "at_utc": _now()}],
            "status": "running",
        },
    )


def append_stage(path: pathlib.Path, stage: str, phase: str, status: int | None) -> None:
    value = _read(path)
    if value.get("status") != "running":
        raise JournalError("cannot append after journal finalization")
    _token("stage", stage)
    if phase not in {"start", "end"}:
        raise JournalError("stage phase must be start or end")
    event: dict[str, Any] = {
        "name": "stage",
        "stage": stage,
        "phase": phase,
        "at_utc": _now(),
        "status": "running" if phase == "start" else None,
    }
    if phase == "end":
        if status is None:
            raise JournalError("stage end requires a status")
        event["status"] = int(status)
        event["result"] = "passed" if status == 0 else "failed"
    value["events"].append(event)
    _atomic_write(path, value)


def _command(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, capture_output=True, text=True, check=False)


def _names(args: list[str]) -> tuple[list[str], str | None]:
    result = _command(args)
    if result.returncode:
        return [], (result.stderr or result.stdout or f"command exited {result.returncode}").strip()
    return [line.strip() for line in result.stdout.splitlines() if line.strip()], None


def _inspect(resource: str, identifier: str) -> tuple[dict[str, Any] | None, str | None]:
    result = _command(["docker", "inspect", identifier])
    if result.returncode:
        return None, (result.stderr or result.stdout or f"inspect exited {result.returncode}").strip()
    try:
        decoded = json.loads(result.stdout)
        if not isinstance(decoded, list) or not decoded or not isinstance(decoded[0], dict):
            raise ValueError("inspect response is not an object list")
        raw = decoded[0]
        if resource == "container":
            config = raw.get("Config") or {}
            return {
                "id": raw.get("Id", identifier),
                "name": str(raw.get("Name", "")).lstrip("/"),
                "image": config.get("Image"),
                "image_id": raw.get("Image"),
            }, None
        if resource == "volume":
            return {"name": raw.get("Name", identifier), "driver": raw.get("Driver")}, None
        return {
            "id": raw.get("Id", identifier),
            "name": raw.get("Name", identifier),
            "driver": raw.get("Driver"),
        }, None
    except (ValueError, json.JSONDecodeError) as error:
        return None, f"cannot decode docker inspect for {resource} {identifier}: {error}"


def snapshot_resources(project: str) -> dict[str, Any]:
    resources: dict[str, Any] = {
        "captured_at_utc": _now(),
        "compose_project": project,
        "containers": [],
        "volumes": [],
        "networks": [],
        "errors": [],
    }
    filters = f"label=com.docker.compose.project={project}"
    for resource, list_args, key in (
        ("container", ["docker", "ps", "-aq", "--filter", filters], "containers"),
        ("volume", ["docker", "volume", "ls", "-q", "--filter", filters], "volumes"),
        ("network", ["docker", "network", "ls", "-q", "--filter", filters], "networks"),
    ):
        identifiers, error = _names(list_args)
        if error:
            resources["errors"].append(f"{resource} list: {error}")
            continue
        for identifier in identifiers:
            inspected, inspect_error = _inspect(resource, identifier)
            if inspected is not None:
                resources[key].append(inspected)
            if inspect_error:
                resources["errors"].append(inspect_error)
    return resources


def append_snapshot(path: pathlib.Path, stage: str, phase: str, project: str) -> dict[str, Any]:
    value = _read(path)
    if value.get("status") != "running":
        raise JournalError("cannot append after journal finalization")
    resources = snapshot_resources(_token("compose_project", project))
    value["events"].append(
        {
            "name": "resource-snapshot",
            "stage": stage,
            "phase": phase,
            "at_utc": resources["captured_at_utc"],
            "resources": resources,
        }
    )
    _atomic_write(path, value)
    return resources


def latest_resources(value: dict[str, Any]) -> dict[str, Any] | None:
    for event in reversed(value.get("events", [])):
        if isinstance(event, dict) and event.get("name") == "resource-snapshot":
            resources = event.get("resources")
            if isinstance(resources, dict):
                return resources
    return None


def resources_empty(resources: dict[str, Any] | None) -> bool:
    return bool(
        resources
        and not resources.get("containers")
        and not resources.get("volumes")
        and not resources.get("networks")
        and not resources.get("errors")
    )


def finalize_journal(
    path: pathlib.Path,
    *,
    status: int,
    down_status: int,
    kept: bool,
) -> bool:
    value = _read(path)
    if value.get("status") != "running":
        raise JournalError("resource journal is already finalized")
    resources = latest_resources(value)
    verified_empty = resources_empty(resources)
    value["events"].append({"name": "finalize", "at_utc": _now()})
    value["cleanup"] = {
        "compose_down_exit_status": int(down_status),
        "kept": bool(kept),
        "verified_empty": verified_empty,
        "remaining": resources,
    }
    value["exit_status"] = int(status)
    value["status"] = "passed" if status == 0 else "failed"
    value["finalized_at_utc"] = _now()
    _atomic_write(path, value)
    return verified_empty


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    init = sub.add_parser("init")
    init.add_argument("--path", type=pathlib.Path, required=True)
    init.add_argument("--run-id", required=True)
    init.add_argument("--artifact", required=True)
    init.add_argument("--project", required=True)
    init.add_argument("--backend", required=True)
    init.add_argument("--namespace", required=True)
    stage = sub.add_parser("stage")
    stage.add_argument("--path", type=pathlib.Path, required=True)
    stage.add_argument("--stage", required=True)
    stage.add_argument("--phase", choices=("start", "end"), required=True)
    stage.add_argument("--status", type=int)
    snap = sub.add_parser("snapshot")
    snap.add_argument("--path", type=pathlib.Path, required=True)
    snap.add_argument("--stage", required=True)
    snap.add_argument("--phase", choices=("start", "end", "cleanup"), required=True)
    snap.add_argument("--project", required=True)
    check = sub.add_parser("check-empty")
    check.add_argument("--path", type=pathlib.Path, required=True)
    finish = sub.add_parser("finalize")
    finish.add_argument("--path", type=pathlib.Path, required=True)
    finish.add_argument("--status", type=int, required=True)
    finish.add_argument("--down-status", type=int, required=True)
    finish.add_argument("--kept", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "init":
            init_journal(
                args.path,
                run_id=args.run_id,
                artifact=args.artifact,
                project=args.project,
                backend=args.backend,
                namespace=args.namespace,
            )
        elif args.command == "stage":
            append_stage(args.path, args.stage, args.phase, args.status)
        elif args.command == "snapshot":
            append_snapshot(args.path, args.stage, args.phase, args.project)
        elif args.command == "check-empty":
            if not resources_empty(latest_resources(_read(args.path))):
                return 1
        else:
            finalize_journal(
                args.path,
                status=args.status,
                down_status=args.down_status,
                kept=args.kept,
            )
    except JournalError as error:
        print(f"resource journal error: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
