# Packed v3 G15/G17 runner manifest validation — 2026-10-06

This checkpoint closes one small control-plane contract in the bounded local
runner. Before this change, `run_packed_local.sh` passed a fixed shuffle seed
only on the scanner command line. The seed, request trace identity, fixture
prefix, and measurement phases were not tied together in an artifact-level
record, so a missing or incomplete summary could be mistaken for a reproducible
run by a downstream consumer.

The runner now creates `run-manifest.json` before any fixture or mount work and
finalizes it from the cleanup trap. The manifest records the schema, run id,
wire version, owned fixture prefix, bounded controls, source and binary hash
inventories, scanner seed, and final status. A successful run is accepted only
when the fixture key is inside the owned prefix, every scanner epoch records the
same seed and a request-trace SHA-256, all files completed with zero errors, and
mount/active/drain/total timing fields are present and finite. A failed or
cancelled run is recorded as `failed` with its exit status and no measurement
section, so it cannot be consumed as a performance result.

The scanner now emits a deterministic `trace_sha256` for each epoch. The local
runner exposes the seed as `PACKED_LOCAL_SCANNER_SEED`, writes it to
`profile.env`, and uses a unique run-scoped fixture prefix by default. The
validator is dependency-free and can be run independently on a saved artifact.

Validation performed:

- RED: `test_packed_run_manifest.py` initially failed because the manifest
  module and contract did not exist (`ModuleNotFoundError`).
- GREEN: `test_packed_run_manifest.py` — 3 passed, including missing seed/timing
  fail-closed behavior and nonzero-exit recording.
- GREEN: `test_smallfiles_scan.py` — 4 passed.
- GREEN: `test_packed_local_runner.py` — 4 passed.
- `python3 -m py_compile` for the helper, scanner, and focused tests passed.
- `bash -n tools/perf/run_packed_local.sh` and `git diff --check` passed.

No FUSE, ECS, OSS, or performance campaign was started. This evidence proves
artifact metadata and fail-closed validation only; it does not close the
workspace lifecycle, 005 cross-request pipeline, full G15 build-policy
controls, G16 remote execution, or G17 paired performance acceptance/S/X.