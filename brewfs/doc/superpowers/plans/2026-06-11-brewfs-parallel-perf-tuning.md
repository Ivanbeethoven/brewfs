# BrewFS Parallel Performance Tuning Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use isolated parallel agents to produce small BrewFS performance candidates, then merge only changes that improve the same perf matrix without hiding cost in buffered writes, close, or post-write drain.

**Architecture:** Each agent works in a dedicated git worktree and owns a non-overlapping subsystem. The coordinator branch cherry-picks only reviewed commits, runs the same baseline/candidate matrix, and reverts any candidate that improves one metric while regressing `fio-randrw` direct mode, write tail, metadata tests, or post-write drain.

**Tech Stack:** Rust BrewFS VFS/cache/meta code, Docker compose Redis/RustFS perf runner, fio direct matrix, `post-write-drain.tsv`, BrewFS `.stats`, and optional `brewfs/tools/perf` artifact analyzers.

---

## Base And Coordination State

- Base branch: `codex/writeback-backpressure-drain`
- Base commit: `a28239a perf: expose writeback drain backpressure metrics`
- Coordinator worktree: `/mnt/slayerfs/brewfs/.worktrees/perf-tune-integration`
- Coordinator branch: `codex/perf-tune-integration`

The base contains:

- `.stats` counters for writeback backpressure soft sleeps and hard waits.
- `PERF_FIO_POST_WRITE_DRAIN` support in the compose perf runner.
- A rejected hysteresis experiment result: buffered `fio-randrw` improved, but direct `fio-randrw` regressed and post-write drain was mixed. Do not reintroduce hysteresis as a default.

## Agent Roster

| Agent | ID | Branch | Worktree | Ownership |
| --- | --- | --- | --- | --- |
| writer admission | `019eb62c-61fa-7923-8014-c8609ccfa533` | `codex/perf-tune-writer` | `/mnt/slayerfs/brewfs/.worktrees/perf-tune-writer` | `brewfs/src/vfs/io/writer.rs` admission/backpressure only |
| upload pipeline | `019eb62c-8df1-7bd1-8027-76473251f75a` | `codex/perf-tune-upload` | `/mnt/slayerfs/brewfs/.worktrees/perf-tune-upload` | upload dispatch/drain path, avoid admission policy |
| read/cache | `019eb62c-bb43-7722-bde3-c9250cf58dbc` | `codex/perf-tune-read` | `/mnt/slayerfs/brewfs/.worktrees/perf-tune-read` | reader/cache/prefetch path only |
| metadata cache | `019eb62c-e5ca-7443-a36e-0da60abc62aa` | `codex/perf-tune-meta` | `/mnt/slayerfs/brewfs/.worktrees/perf-tune-meta` | metadata cache analysis and plan |
| perf harness | `019eb62d-0f70-7f41-8d1b-dc0254098303` | `codex/perf-tune-harness` | `/mnt/slayerfs/brewfs/.worktrees/perf-tune-harness` | scripts/tools for A/B report only |

## Merge Rules

- Do not merge directly from an agent branch.
- Read the agent's final report and inspect its diff first:

```bash
git -C /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration diff --stat a28239a..<agent-branch>
git -C /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration log --oneline a28239a..<agent-branch>
```

- Reject candidates that:
  - edit outside their ownership scope without a clear reason,
  - introduce broad config/default changes without an A/B gate,
  - do not include targeted tests for code changes,
  - improve buffered `direct=0` while regressing `direct=1` by more than 5% throughput or 25% p99.9 latency,
  - increase post-write drain for write workloads by more than 10% unless the workload's fio runtime improves enough to reduce total wall time.
- Cherry-pick one candidate at a time onto `codex/perf-tune-integration`.
- After each cherry-pick, run targeted tests before trying the next candidate.

## Baseline Matrix

Run the baseline from `a28239a` and record artifact IDs before evaluating candidates:

```bash
cd /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration
PERF_FIO_DIRECT_MATRIX="0 1" \
PERF_FIO_RANDRW_RUNTIME=20 \
PERF_FIO_RANDWRITE_RUNTIME=20 \
PERF_FIO_SEQWRITE_RUNTIME=20 \
PERF_FIO_POST_WRITE_DRAIN=true \
PERF_FIO_PREFILL_DRAIN_TIMEOUT_SECS=600 \
BREWFS_COMPRESSION=none \
BREWFS_PREFETCH_ENABLED=true \
BREWFS_UPLOAD_CONCURRENCY=32 \
bash brewfs/docker/compose-xfstests/run_redis_perf.sh \
  --writeback-throughput-profile \
  --tools "fio-seqwrite fio-randwrite fio-randrw"
```

Required baseline artifacts:

- `perf-summary.tsv`
- `post-write-drain.tsv`
- `results/fio-*.json`
- `diagnostics/stats-*-after.txt`
- `report.md`

## Candidate Matrix

Run the same command on every candidate after targeted tests pass. For read/cache candidates, add read-heavy tools:

```bash
PERF_FIO_DIRECT_MATRIX="0 1" \
PERF_FIO_RANDREAD_RUNTIME=20 \
PERF_FIO_SEQREAD_RUNTIME=20 \
PERF_FIO_POST_WRITE_DRAIN=true \
PERF_FIO_PREFILL_DRAIN_TIMEOUT_SECS=600 \
BREWFS_COMPRESSION=none \
BREWFS_PREFETCH_ENABLED=true \
BREWFS_UPLOAD_CONCURRENCY=32 \
bash brewfs/docker/compose-xfstests/run_redis_perf.sh \
  --writeback-throughput-profile \
  --tools "fio-seqread fio-randread fio-randrw"
```

For final acceptance, run the full set:

```bash
PERF_FIO_DIRECT_MATRIX="0 1" \
PERF_FIO_POST_WRITE_DRAIN=true \
PERF_FIO_PREFILL_DRAIN_TIMEOUT_SECS=600 \
BREWFS_COMPRESSION=none \
BREWFS_PREFETCH_ENABLED=true \
BREWFS_UPLOAD_CONCURRENCY=32 \
bash brewfs/docker/compose-xfstests/run_redis_perf.sh \
  --writeback-throughput-profile \
  --tools "fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw metaperf dirstress dirperf"
```

## Task 1: Collect Agent Results

- [ ] Wait for all five agents to reach `DONE`, `DONE_WITH_CONCERNS`, or `BLOCKED`.
- [ ] Record each final report in this plan or in a sibling review document.
- [ ] For every code-changing branch, capture:

```bash
git -C /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration log --oneline a28239a..<agent-branch>
git -C /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration diff --stat a28239a..<agent-branch>
```

## Task 2: Review Candidate Diffs

- [ ] Check whether the changed files match the agent ownership table.
- [ ] Check whether tests exist for changed Rust behavior.
- [ ] Check whether scripts fail clearly when artifacts are missing.
- [ ] Reject or send back candidates with unbounded background work, hidden async tasks, or default config changes without perf evidence.

## Task 3: Integrate One Candidate At A Time

- [ ] Cherry-pick the smallest accepted candidate.
- [ ] Run its targeted tests.
- [ ] Run the baseline/candidate perf matrix against the current integration branch.
- [ ] If metrics fail acceptance, revert the cherry-pick immediately:

```bash
git revert <candidate-sha>
```

- [ ] If metrics pass, keep the commit and move to the next candidate.

## Task 4: Final Verification

- [ ] Run Rust targeted tests for touched modules.
- [ ] Run `cargo fmt --all --check`.
- [ ] Run `bash -n` on touched shell scripts.
- [ ] Run the final full perf set.
- [ ] Commit/push only accepted changes plus this management plan.

## Reporting Template

For every attempted candidate, record:

```text
Candidate:
Branch:
Commit:
Touched files:
Targeted tests:
Perf artifact baseline:
Perf artifact candidate:
fio-randrw direct0 delta:
fio-randrw direct1 delta:
post-write drain delta:
Decision: keep / revert
Reason:
```
