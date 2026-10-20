# BrewFS × RustFS benchmark scripts

This directory brings the workload and analysis code behind the
[innovation results](../../../innovations/brewfs-rustfs/agent-mixed-fuse-results.md)
into the repository. The scripts previously lived in separate `work/brewfs-*`
experiment directories. `import-manifest.json` identifies every imported source
and its checksum **before** portability edits. Git identifies the published
version. No JuiceFS feature code is included.

## What to run

| Directory | Purpose | Entry points |
| --- | --- | --- |
| `agent-mixed/` | Primary end-to-end runtime ablation: whole files, mixed sizes, FUSE, no-dynamic-block versus auto | `freeze_workloads.py`, `campaign.py`, `analyze.py`, `report.py` |
| `io-joint/` | Earlier whole-file write, cold read, hot read and fixed/auto policy experiments | `run_joint_io.py`, `run_hot_io.py`, `run_policy_io.py`, `analyze_closed.py` |
| `writeback-budget/` | Diagnostic workloads for upload budgets, tiny files, first-file cost, resources, network and compose fio guards | `run_budget_io.py`, `run_size_budget_io.py`, `run_first_file_diagnostic.py`, `run_fio_guard.py`, `analyze*.py` |
| `recovery-scan/` | Old/new auto regression guards, mixed replay and path attribution | `run_guard_io.py`, `profile_paths.py`, `run_fio_guard.py`, `analyze*.py`, `audit*.py` |

Use `agent-mixed/` for a new benchmark. The other directories retain the actual
historical workload/analysis modules and their distinct job schemas, including
rejected experiments. They are not alternative current defaults. In particular,
the early standalone `run_core.py` main program expects the old 4 MiB auto policy;
the current mixed runner overrides that path and checks 1 MiB layouts.
Repeated helper files intentionally preserve campaign-specific behavior.

Historical leaf runners take a frozen job JSON as their first argument inside
the matching disposable client Pod. Supply the original plan and binaries
identified by that campaign's artifacts. Offline analyzers consume that
campaign's `BASE/<stage>/raw/` records. Do not use a new binary with an old
fingerprint or combine screen/formal samples. The io-joint report additionally
expects exported `*-results.json` from its original evidence bundle; it does not
recreate a historical deployment. Fio guard runners require the repository's
`docker/compose-xfstests/` runner files under
`/work/perf-script-gate-canonical/docker/compose-xfstests/`, plus fio and their
normal system dependencies; the job pins the runner SHA256.
For recovery guard audits of old data, set `BENCH_RECORDED_TOOLS` to the matching
archived `guard-tools/` or `fio-guard-tools/`; those bytes must match the plan.

The import excludes one-off code-patching/build-resume scripts, automation
supervisors, credentials, cloud creation/deletion commands tied to old resource
IDs, generated traces, binaries, raw data and redundant copies from superseded
campaigns. The primary suite includes a portable ACS manifest/deployer and
export-gated cleanup for a **dedicated namespace in an existing cluster**.
It does not create or delete the cluster or shared network.

## Workload and interpretation

- Three scenes: browse (90% read), edit/verify (70% read), generate (30% read).
- Four workers; 8,000 operations and 3,842.5 MiB logical I/O per run. Each run
  starts from 3,000 files. Six paired rounds give 36 runs / 288,000 operations.
- Whole-file sizes: 4 KiB, 64 KiB, 256 KiB, 1 MiB, 4 MiB + 64 KiB and 16 MiB;
  operation weights 40/30/15/10/4/1%. Half the writes create files and half replace
  them through temporary file → fsync → close → rename. Application requests
  are at most 128 KiB; BrewFS data blocks remain 4 MiB, chunks 64 MiB.
- Same feature-enabled binary in both arms. Runtime hint absent versus auto;
  RustFS hint acceptance is disabled/enabled with the arm. This measures the
  complete runtime collaboration, not an isolated change in EC block size.
  Both actual EC layouts were 1 MiB in the published experiment.
- Fresh metadata, object directories and client cache per run; prefill, drain,
  remount, full trace, drain, fresh-cache remount and all-file SHA256 checks.
  Debt must be zero for three successive samples. Timed I/O latency excludes
  read hashing; workflow wall time includes it. The final total includes upload
  drain. Logical throughput includes cache hits and is not S3 bandwidth.
- 20,000 paired-round bootstrap draws, seed 20261020, percentile 95% intervals.
  No regression: throughput ratio lower bound ≥ 0.95 and read/save p95 upper
  bound ≤ 1.05. Clear throughput gain: point ≥ 1.05 and lower bound > 1.
  There are 51 reported comparisons and 45 required checks, with edit as the
  primary scene. Intervals are per-comparison, not a familywise guarantee.

Published results passed **44/45** required checks: browse save-p95 remained
inconclusive. Do not turn this into a claim that every performance gate passed.
See the innovation documents for the absolute values and all adverse outcomes.

## Dependencies and local verification

Use Linux or WSL with Python 3.12+, the packages in `requirements.txt`, kubectl,
and an ACS kubeconfig. Reports need a CJK font, e.g. `fonts-noto-cjk`, or set
`BENCH_FONT` to an installed font file. Never use `python -O`: benchmark
invariants use assertions. No cloud access is needed for the offline checks.

```sh
python3 -m venv /tmp/brewfs-bench-venv
. /tmp/brewfs-bench-venv/bin/activate
pip install -r tools/perf/rustfs-hint/requirements.txt
python -m compileall -q tools/perf/rustfs-hint
python -m unittest discover -s tools/perf/rustfs-hint/agent-mixed -p 'test_*.py' -v
```

`reference-traces.json` records all 18 measured input hashes. The tests regenerate
every trace, audit file ownership/content dependencies and compare exact bytes.
Wrong read versions and writes into another worker's namespace must fail.
Product acceptance still requires the repository's normal CI and compose gates;
syntax checks of this import are not new performance evidence.

## Reproduce the mixed suite on ACS

Use a dedicated cluster with FUSE support enabled, or a dedicated namespace in
your experimental cluster. The deployed Redis instance is disposable: the
runner calls `FLUSHDB` between jobs. All four server disks are disposable and
each job's data is reclaimed only after its raw records are exported and checked.

Configure paths explicitly, outside the repository. The binary manifest example
contains **historical** hashes; replace its paths, full revisions and SHA256
values with those of your actual builds. Build BrewFS with the
`rustfs-ec-block-size-hint` feature. The RustFS build must support the named
hint/cache flags and provide the `compare_layout` utility that decodes `xl.meta`.
The recorded RustFS revision is in the example manifest and innovation documents.

```sh
export BENCH_BASE=/absolute/path/new-campaign/records
export BENCH_OUT=/absolute/path/new-campaign/results
export BENCH_BINARIES=/absolute/path/binaries.local.json
export BENCH_KUBECONFIG=/absolute/path/acs.config
export BENCH_NAMESPACE=brewfs-agent-my-run
# WSL may use KUBECTL=kubectl.exe and a Windows BENCH_KUBECONFIG path.
cd tools/perf/rustfs-hint/agent-mixed

python freeze_workloads.py
python prepare_inputs.py
# First renders JSON without creating resources; inspect it before --execute.
python deploy.py --zone cn-hangzhou-h > /tmp/acs-plan.json
python deploy.py --zone cn-hangzhou-h --execute
```

The manifest fixes the client to general-purpose 4 CPU / 8 GiB, server to
performance 16 CPU / 32 GiB, Redis to 1 CPU / 2 GiB, and four 40 GiB ESSD PL1
disks. `--registry` can select an accessible image registry. Its Ubuntu image
is pinned; package installation and Redis tag resolution are recorded by the
actual Pod topology. The client opens `/dev/fuse` and performs a bindfs write
probe before creating `/work/ready`. Wait for readiness and inspect Pod logs if
setup fails; never count a failed setup as a benchmark sample.

```sh
kubectl --kubeconfig "$BENCH_KUBECONFIG" -n "$BENCH_NAMESPACE" exec agent-client -- test -f /work/ready
kubectl --kubeconfig "$BENCH_KUBECONFIG" -n "$BENCH_NAMESPACE" exec agent-store -- test -f /work/ready
python stage.py
python capacity_check.py             # separate TCP diagnostic, not acceptance
python prepare_jobs.py smoke
python verify_staged.py smoke
python campaign.py smoke
python prepare_jobs.py formal
python verify_staged.py formal
python campaign.py formal
```

Staging verifies local and remote binary hashes, Pod resource shapes and four
distinct disks. The formal plan freezes the tools and deployment hashes before
measurement. Do not edit it or rerun a failed/slow sample to improve a verdict.
Controller observation timeouts do not stop the remote job. Inspect its saved
PID/starttime and original outputs. `resume_formal.py --recovery-name
formal-recovery-01` only supports the documented state where the original client
has completed, its server is still active, and layout/export have not begun; it
refuses other states. Never start `campaign.py` twice for the same output folder.

```sh
python analyze.py
python analyze_components.py
python analyze_bottlenecks.py
python audit_completed.py
python report.py
python export_results.py
python cleanup.py                    # lists recorded resources
python cleanup.py --execute          # requires successful export and matching UIDs
```

The Chinese HTML includes all comparisons, per-round values, latency-by-size
plots, complete debt timelines, component timings and adverse results. Export
scans the generated credentials and verifies every archive member. Keep private
auth, kubeconfig and binary manifests local. Cleanup removes the recorded
namespace resources; verify all four CSI disk IDs from `topology.json` have
actually disappeared in ACS before declaring resources released.

## Reanalyze existing records without running I/O

Extract a trusted evidence archive to a separate directory. Set `BENCH_BASE` to
its `records/` and `BENCH_OUT` to a new report directory containing the original
`traces/` and byte-identical `experiment-plan.json`. Run the four analysis/audit
commands above. For previous source versions, pass
`audit_completed.py --recorded-tools "$BENCH_BASE/formal-tools"`; it verifies
those archived tools against the immutable original plan. It does not compare
old fingerprints to the relocated source. Then run `report.py`. Preserve the
original archive and its checksum. Statistical reanalysis needs neither
kubeconfig nor cloud credentials.

Changes here only package the tests. They do not establish a new performance
result or implement another storage format, and they do not include the separate
JuiceFS experiment.
