# Completed validation commands

Only completed commands are listed. See README.md for pending gates and scope.

Full output, exact revisions, environments and source patches are in
`validation.tar.gz`. Ignored tests did not run.

## 222-before

Exit: **101**. Worktree: `/mnt/brewfs-data/src/brewfs-review-baseline`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-performance/target GIT_OPTIONAL_LOCKS=0 cargo test -p brewfs --features workspace-overlay --test workspace_session_drop -- --test-threads=1 --nocapture
```

```text
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s
```

## 222-after-1

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-lease-drop-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-lease-drop-review/target CARGO_PROFILE_DEV_CODEGEN_UNITS=16 CARGO_PROFILE_TEST_CODEGEN_UNITS=16 GIT_OPTIONAL_LOCKS=0 REVIEW_EXCLUSIVE_HOST=1 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test workspace_session_drop -- --test-threads=1 --nocapture
```

```text
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s
```

## 222-after-2

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-lease-drop-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-lease-drop-review/target CARGO_PROFILE_DEV_CODEGEN_UNITS=16 CARGO_PROFILE_TEST_CODEGEN_UNITS=16 GIT_OPTIONAL_LOCKS=0 REVIEW_EXCLUSIVE_HOST=1 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test workspace_session_drop -- --test-threads=1 --nocapture
```

```text
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s
```

## 222-after-3

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-lease-drop-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-lease-drop-review/target CARGO_PROFILE_DEV_CODEGEN_UNITS=16 CARGO_PROFILE_TEST_CODEGEN_UNITS=16 GIT_OPTIONAL_LOCKS=0 REVIEW_EXCLUSIVE_HOST=1 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test workspace_session_drop -- --test-threads=1 --nocapture
```

```text
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s
```

## 222-fmt-publication

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-lease-drop-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_CODEGEN_UNITS=16 CARGO_PROFILE_TEST_CODEGEN_UNITS=16 GIT_OPTIONAL_LOCKS=0 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo fmt --all --check
```
