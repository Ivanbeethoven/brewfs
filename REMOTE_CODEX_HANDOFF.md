# BrewFS Remote Codex Handoff

This is the operator handoff for the remote packed-v3 read-only optimization
work. It contains no password, access token, Aliyun AK/SK, GitHub token, or
Codex bearer value. Credentials remain only in the target user profiles.

## Target and repository

- SSH alias: `brewfs-frp-sea`
- Windows target: `laptop-fjfpi4lk`
- WSL distribution: `Ubuntu-24.04`
- WSL user: `hxy`
- Repository: `/home/hxy/brewfs`
- Branch: `codex/packed-metadata-aliyun-20260930`
- Current remote HEAD before the candidate: `2e10947` (`docs-remote-codex-handoff`)
- Git remotes: `origin=https://github.com/Ivanbeethoven/brewfs.git`,
  `upstream=https://github.com/brewfs/brewfs.git`

The branch was migrated from a local Git bundle because the target Windows
host could not reach `github.com:443` during the first clone. Confirm the
current state before doing more work:

```text
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- /usr/bin/git -C /home/hxy/brewfs status --short --branch
ssh brewfs-frp-sea wsl.exe -d Ubuntu-24.04 -- /usr/bin/git -C /home/hxy/brewfs rev-parse HEAD
```

The untracked `.claude/` directory is migration scratch state and must not be
committed. `REMOTE_CODEX_GOAL.md` and this file are operator inputs, not source
code or credential stores.

## Access and command transport

Windows OpenSSH is configured with the `brewfs-frp-sea` alias. A direct
Windows command is the most reliable form:

```text
ssh.exe brewfs-frp-sea "wsl.exe -d Ubuntu-24.04 -- /usr/bin/git -C /home/hxy/brewfs status --short --branch"
```

The generic `remote-ssh-dev` helper scripts assume that the local `ssh`
binary has the same alias configuration. When invoked from local WSL, Linux
`ssh` does not know the Windows alias and reports `Could not resolve hostname
brewfs-frp-sea`. Use `ssh.exe` from Windows, or export a small shell function
that forwards `ssh` to `ssh.exe` before running the helper scripts. This is a
local transport issue, not a remote repository failure.

## Installed tools

The target Windows user has Git for Windows 2.55.0.windows.5, authenticated
GitHub CLI with `gh auth setup-git`, and an authenticated Alibaba Cloud CLI
profile in `cn-hangzhou`. Git identity is `Xiaoyang Han <lux1an@qq.com>`.

The WSL helpers are:

- `/home/hxy/.local/bin/gh.exe` -> Windows GitHub CLI
- `/home/hxy/.local/bin/aliyun.exe` -> Windows Aliyun CLI
- `/home/hxy/.local/bin/codex` -> the installed npm Codex package with Node.js
  on `PATH`

The WSL Git credential helper delegates to `gh.exe auth git-credential`. Do
not print `gh auth token`, Aliyun credential files, or Codex configuration
secrets in logs or commits.

## Objective and design invariants

The active objective is a measurable read-only improvement for large
namespaces of 100 KiB to 1 MiB files under strict cold-data and
metadata-warm/cold-data profiles, with the same OSS endpoint and dataset as
the JuiceFS reference. Keep these three ideas integrated:

1. Immutable authenticated packed metadata with pageable group/inode indexes.
2. Dynamic size-class data frames with bounded descriptor and payload windows.
3. Overlay workspace semantics with packed metadata as the lower layer.

Correctness is more important than a benchmark-only shortcut. Preserve digest,
CRC, length, ordering, and frame-boundary checks. Never share unrelated file
payload merely to inflate a small-file benchmark. A large directory must be
pageable and must not require materializing every entry in memory.

## Candidate currently in the remote worktree

The only source candidate is in
`src/workspace_overlay/packed_v3/coordinator.rs`:

- the shared group-read batching delay changed from 1 ms to 250 us;
- a unit assertion requires the delay to remain below 1 ms.

The corresponding hypothesis and limits are recorded in
`doc/performance/packed-v3-metadata-cache-analysis-2026-09-29.md`. The change
does not alter the range planner, size-class separation, byte/range budgets,
window cache, or integrity validation. It is still a performance candidate:
no new end-to-end cold-read number has been accepted, and it must not be
described as faster than JuiceFS without a matched run.

## Verification completed on 2026-09-30

All commands below ran in `/home/hxy/brewfs` on the target, with
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` for Cargo
commands where shown.

Passed:

- `cargo fmt --all --check`
- all required shell syntax/report checks from `AGENTS.md`
- `git diff --check`
- `cargo check --workspace`
- `cargo build --workspace`
- `cargo clippy --workspace` (existing warnings only; exit status 0)
- `cargo test --workspace --lib --bins`: 1097 passed, 225 ignored, 0 failed
- packed-v3 focused suite:

  ```text
  cargo test -p brewfs --features workspace-overlay packed_v3 --lib -- --nocapture
  ```

  Result: 59 passed, 0 failed.

The feature flag matters. `workspace_overlay` is gated by the
`workspace-overlay` Cargo feature, so a filter run without
`--features workspace-overlay` legitimately reports zero packed tests; that
is not evidence that the suite passed.

The target has `asyncfuse 0.1.12` in its Cargo cache. The earlier failure to
resolve `static.crates.io` is therefore no longer blocking these local gates.

## Performance status

No new remote OSS/FUSE benchmark was run for this 250-us candidate. The
existing matched reference artifacts remain the source of truth until a new
run is completed. The artifact directory is in the operator's local checkout;
it was not copied into the remote WSL repository during migration:

```text
docker/compose-xfstests/artifacts/aliyun-packed-v3-vs-juicefs-100k-20260928-r3/
```

Verify that path locally before using it as a comparison input. Do not infer
that a missing path on the remote host means the reference run never existed.

Any follow-up benchmark must report, for both packed and JuiceFS, the exact
metadata/payload memory and SSD cache budgets, page-cache state, active and
active-plus-drain bandwidth, object GET/range counts, metadata bytes, frame
overscan, and dataset/file-size distribution. Strict cold means both payload
cache budgets are zero and the host page cache is dropped before each matched
run. Metadata-warm/cold-data must include the warm-up procedure and duration.
Do not update README comparison tables from this candidate alone.

## Remaining work

1. Stage only the coordinator source, its performance note, and this handoff;
   leave `.claude/` untracked.
2. Commit the candidate with a message that says it is a measured-pending
   optimization, then push `codex/packed-metadata-aliyun-20260930` to `origin`.
3. Run a bounded 10k-file matched cold-read test before considering a larger
   100k test. Use the existing compose runners and comparison tool; do not
   start an unbounded upload or cloud server.
4. If the candidate regresses request count, overscan, active-plus-drain
   bandwidth, or correctness, revert only the candidate patch and document the
   rejection in `doc/performance/`.
5. After every cloud run, remove temporary OSS objects, Redis/TiKV keys,
   mounts, containers, and servers. Preserve accepted artifacts and record
   their paths.

## Commit and closeout commands

Review before staging:

```text
ssh brewfs-frp-sea "wsl.exe -d Ubuntu-24.04 -- /usr/bin/git -C /home/hxy/brewfs diff -- src/workspace_overlay/packed_v3/coordinator.rs doc/performance/packed-v3-metadata-cache-analysis-2026-09-29.md REMOTE_CODEX_HANDOFF.md"
ssh brewfs-frp-sea "wsl.exe -d Ubuntu-24.04 -- /usr/bin/git -C /home/hxy/brewfs status --short --branch"
```

After review and any matched benchmark:

```text
ssh brewfs-frp-sea "wsl.exe -d Ubuntu-24.04 -- bash -lc 'cd /home/hxy/brewfs && git add src/workspace_overlay/packed_v3/coordinator.rs doc/performance/packed-v3-metadata-cache-analysis-2026-09-29.md REMOTE_CODEX_HANDOFF.md && git commit -m \"perf: shorten packed read coalescing window\" && git push origin codex/packed-metadata-aliyun-20260930'"
```

Do not use destructive reset/checkout commands. At closeout, leave a coherent
commit on the personal branch, report the commit ID and all test/artifact
paths, and keep the repository and credential profiles available for follow-up
work. Remove only migration bundles and temporary staging files after the
final verification; never remove the working repository or credentials as
part of normal handoff.
