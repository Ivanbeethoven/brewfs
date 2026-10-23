# Completed validation commands

Broader gates are still pending. See README.md for scope.

## 219-220-242-before

Exit: **101**. Worktree: `/mnt/brewfs-data/src/brewfs-review-baseline`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-performance/target cargo test -p brewfs --features workspace-overlay --test compactor_test -- --test-threads=1 --nocapture
```

```text
test result: FAILED. 8 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.30s
```

## 219-220-after-1

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-compaction-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-compaction-review/target GIT_OPTIONAL_LOCKS=0 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test compactor_test -- --test-threads=1 --nocapture
```

```text
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.44s
```

## 219-220-after-2

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-compaction-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-compaction-review/target GIT_OPTIONAL_LOCKS=0 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test compactor_test -- --test-threads=1 --nocapture
```

```text
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.62s
```

## 219-220-after-3

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-compaction-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-compaction-review/target GIT_OPTIONAL_LOCKS=0 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo test -p brewfs --features workspace-overlay --test compactor_test -- --test-threads=1 --nocapture
```

```text
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.57s
```

## 219-220-fmt

Exit: **0**. Worktree: `/mnt/brewfs-data/src/brewfs-fix-compaction-review`.

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/src/brewfs-fix-compaction-review/target GIT_OPTIONAL_LOCKS=0 PATH=/home/hxy/.local/bin:/home/hxy/.local/bin:/home/hxy/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games:/usr/local/games:/snap/bin cargo fmt --all --check
```
