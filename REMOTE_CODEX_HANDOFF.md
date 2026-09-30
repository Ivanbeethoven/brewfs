# BrewFS Remote Codex Handoff

This document is the operational handoff for the remote read-only packed
metadata optimization task. It intentionally contains no access token, AK/SK,
password, or bearer credential. Credentials are installed only in the user
profiles on the target machine.

## Target and workspace

- SSH alias: `brewfs-frp-sea`
- Target: Windows 11 host `laptop-fjfpi4lk`, WSL2 distro `Ubuntu-24.04`
- WSL user: `hxy`
- Repository: `/home/hxy/brewfs`
- Branch: `codex/packed-metadata-aliyun-20260930`
- Initial migrated commit: `8f4fa1f` (`ops: document secure remote Codex migration`)
- Git remotes: `origin=https://github.com/Ivanbeethoven/brewfs.git`,
  `upstream=https://github.com/brewfs/brewfs.git`

The branch was migrated from a verified local Git bundle because the target
Windows host could not reach `github.com:443`. The bundle was cloned and then
the branch was checked out with its remote-tracking ref. Verify the starting
state with:

```bash
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- \
  /usr/bin/git -C /home/hxy/brewfs status --short --branch
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- \
  /usr/bin/git -C /home/hxy/brewfs rev-parse HEAD
```

The migration also copied the local, untracked `.claude/` scratch scripts and
the task files `REMOTE_CODEX_GOAL.md` and this handoff file. Treat those as
operator inputs, not as credentials or authoritative source code.

## Windows tools

The target Windows user has:

- Git for Windows 2.55.0.windows.5 on `PATH`.
- GitHub CLI authenticated as the configured personal account; `gh auth
  setup-git` installed the Git credential helper for GitHub HTTPS operations.
- Alibaba Cloud CLI installed and its `default` profile valid in
  `cn-hangzhou`.
- Global Git identity set to `Xiaoyang Han <lux1an@qq.com>`.

Safe verification commands (they mask credentials):

```text
ssh brewfs-frp-sea powershell.exe -NoProfile -Command git.exe --version
ssh brewfs-frp-sea powershell.exe -NoProfile -Command gh.exe auth status
ssh brewfs-frp-sea powershell.exe -NoProfile -Command aliyun.exe configure list
```

Do not print `gh auth token`, Aliyun credential files, or Codex bearer values.

## WSL command wrappers

WSL uses `/usr/bin/git`. Windows executables are exposed without duplicating
their credentials:

- `/home/hxy/.local/bin/gh.exe` links to the Windows GitHub CLI.
- `/home/hxy/.local/bin/aliyun.exe` links to the Windows Aliyun CLI.
- `/home/hxy/.local/bin/codex` is a small wrapper that adds the installed
  Node.js directory to `PATH` and execs the existing npm Codex package.

The WSL Git credential helper is:

```text
!/home/hxy/.local/bin/gh.exe auth git-credential
```

This permits `git fetch/push` without copying a GitHub token into WSL.

## Codex configuration and session

`/home/hxy/.codex/config.toml` is a minimal CLI-compatible configuration. It
uses the existing custom Responses provider and model settings from the local
Codex installation. The provider URL and bearer value are deliberately absent
from this document and must never be committed or echoed.

The remote session was started in the repository with workspace-write and
noninteractive approval policy. Its CLI session id was:

```text
01a0f085-e108-7283-996f-7981e0573602
```

The initial prompt was `REMOTE_CODEX_GOAL.md`; this session has now exited after
reviewing the path, applying one focused candidate, and recording its validation
status. Do not start a second Codex process until the working tree below has
been reviewed. The terminal output is intentionally not a source of truth;
inspect the repository and the final commit instead.

## Engineering objective

The goal is to improve the packed-v3 read-only path for large namespaces of
100 KiB to 1 MiB files under strict cold-data and metadata-warm/cold-data
profiles. Preserve the three integrated design points:

1. immutable, authenticated packed metadata with pageable indexes;
2. dynamic size-class data frames and bounded cross-file reads;
3. overlay workspace semantics with a packed lower layer.

The active hypothesis is that avoidable per-read scheduling and metadata/data
read-path work remains after the existing byte-budget caches, locator admission,
bounded descriptor reads, and group-window coordinator. Any optimization must
remain correct for malformed/truncated frames and must not share unrelated file
data merely to improve a benchmark.

## Required validation

Before accepting a change, run the repository gates from `AGENTS.md`, including:

```bash
cargo fmt --all --check
bash -n docker/compose-xfstests/run_perf_in_container.sh
bash -n docker/compose-xfstests/run_redis_perf.sh
bash -n docker/compose-xfstests/run_juicefs_perf_in_container.sh
bash -n docker/compose-xfstests/run_juicefs_perf.sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --workspace
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --workspace
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --workspace
git diff --check
```

Focused packed-v3 tests may be used during iteration, but do not replace the
workspace test gate. Performance evidence must distinguish:

- strict-cold data with both payload cache budgets zero;
- metadata-warm/cold-data with bounded warm-up time included in setup;
- warm payload-cache runs, which are not comparable to strict-cold results.

Record object GET/range counts, metadata bytes, frame overscan, active plus
drain bandwidth, and the exact cache budgets. Compare JuiceFS only with the
same dataset, OSS endpoint, concurrency, compression, and cache state.

## Cloud-resource hygiene

Start cloud validation at 10k files. Scale only after a local/focused result
justifies it. Every run must delete temporary OSS objects, Redis/TiKV keys,
mounts, containers, and any temporary server. Do not leave a background cloud
job or an unbounded upload running. Aliyun CLI uses the preconfigured Windows
profile through `/home/hxy/.local/bin/aliyun.exe`.

## Known environmental issues

- Direct target access to `github.com:443` failed during clone. Use the
  configured GitHub helper only after verifying connectivity; otherwise migrate
  changes through the operator and a bundle.
- The target currently cannot resolve `static.crates.io`; a first focused Cargo
  test attempted by Codex failed while downloading `asyncfuse`. Reuse the
  existing Cargo cache or configure the approved proxy before retrying. Do not
  weaken tests because of this failure.
- The initial branch has untracked `.claude/` and handoff files by design. They
  must not be mistaken for code regressions or credentials.

## Current candidate status (2026-09-30)

The remote session changed only the packed read coordinator and its performance
note: `COALESCE_DELAY` is now 250 microseconds instead of 1 millisecond, with a
unit assertion that the delay remains below 1 millisecond. Shell/report checks,
format checking, and `git diff --check` passed. Focused Rust tests and the
workspace gate were attempted but could not download `asyncfuse 0.1.122` because
the target could not resolve `static.crates.io`. No end-to-end performance run
was performed, so this is an unaccepted candidate until the dependency/network
issue is resolved and matched cold-read measurements pass.

Codex could not create a commit because its sandbox denied `.git/index.lock`.
The operator must review the two source/doc diffs, run the available gates, then
stage only the intended files and commit/push from the SSH session. Leave the
untracked `.claude/` scratch directory out of the commit.

## Recovery and closeout

To inspect progress without starting another agent:

```bash
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- /usr/bin/pgrep -af codex
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- \
  /usr/bin/git -C /home/hxy/brewfs log --oneline --decorate -8
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- \
  /usr/bin/git -C /home/hxy/brewfs status --short --branch
```

At closeout, the remote agent must leave a coherent commit on the personal
branch, push it to `origin`, and report tests/artifacts and any remaining gap.
Only after the final verification should the operator remove migration bundles
and Windows staging files. Keep `/home/hxy/brewfs`, its `.codex` configuration,
and the GitHub/Aliyun credential profiles intact for follow-up work.
