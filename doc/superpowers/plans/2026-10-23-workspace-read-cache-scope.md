# Workspace read-cache isolation

Branch: `codex/fix-cache-scope-20261023`, based on the separate statfs fix
`cdb5bbb`. No merge is part of this task.

## Cause and change

The CLI constructed `ObjectBlockStore` before loading the workspace volume
header. Its persistent block cache used `<cache_root>/chunks`, with keys such
as `chunks/2/0`. A slice ID is not a global volume/workspace identity. A valid
cache file from another volume could therefore satisfy a read without fetching
the correct object. CRC verification cannot detect this ownership mismatch.
Promoted writeback slices also used the shared directory, with names such as
`.writeback-slice-2`.

Workspace mounts now load and validate the existing volume header before
constructing the object store. Both clean blocks and promoted slices use:

```text
<cache_root>/workspace-read-v1/<volume UUID>/<workspace UUID>/chunks
```

The CLI supplies the same scoped clean-cache configuration to the object store
and VFS. This applies to workspace-v1, workspace-native-v2 and packed-workspace
recovery. The operator runtime wrapper and production S3 adapter remain in use.
The entity experiment has no shared persistent chunk-cache path to migrate;
the authenticated packed read-only path already disallows payload disk caches.
Existing flat-volume cache namespacing is unchanged.

Clean slices are immutable and their IDs are not recycled within a volume, so
the clean-cache scope remains stable across lease/epoch changes. Dirty staging
continues to use its original
`workspace-v1/<volume UUID>/<workspace UUID>/<head epoch>` path. This change
does not relocate pending uploads or alter the writeback recovery identity.
Legacy unscoped clean entries are ignored, never imported or deleted.

## Regression coverage

- [Rust cache tests](../../../tests/workspace_cache_scope.rs) seed a
  checksum-valid wrong block in the legacy directory, another workspace and
  another volume. The selected scope misses all three, then reuses its own
  correct block after reopening. A second test checks stable clean paths and
  unchanged epoch-scoped dirty paths. **2 passed, 0 failed.** The existing
  statfs integration test also passed.
- [FUSE/RustFS regression](../../../tests/scripts/test_workspace_cache_scope.py)
  uses the real CLI and S3 adapter with two fresh SQLite catalogs, three
  workspaces and disposable RustFS buckets. It seeds valid legacy raw blocks
  and promoted-slice files with wrong bytes, checks colliding slice IDs across
  volumes, sibling workspace isolation, reads after remount, and preservation
  of the poisoned entries. It cleans up only its mounts and buckets. This is
  an explicit external test, not a silently skipped unit test.
  **Not yet verified end to end:** the initial invocation failed while
  creating its first file because the harness ran as an ordinary user against
  a uid-0-owned root. Its mounts and bucket were cleaned up. The harness now
  requires root explicitly; the corrected run remained queued behind another
  worktree's Cargo job when the owner directed delivery after the remote pass.

The focused command is:

```bash
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/mnt/brewfs-data/target-operator \
cargo test -p brewfs --test workspace_cache_scope --test workspace_statfs \
  --no-default-features --features fuse-tokio-runtime,workspace-overlay -- --nocapture
```

For the FUSE test, start an isolated loopback RustFS, set its test
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, then run:

```bash
sudo --preserve-env=AWS_ACCESS_KEY_ID,AWS_SECRET_ACCESS_KEY \
  python3 tests/scripts/test_workspace_cache_scope.py \
  --binary /path/to/overlay-enabled/brewfs \
  --endpoint http://127.0.0.1:PORT --artifacts /path/to/new-artifact-directory
```

Run the FUSE test as root: a fresh workspace's root inode belongs to uid 0.
Prerequisites: Linux `/dev/fuse`, `fusermount3`, Python 3, curl with SigV4
support, and a running RustFS. The artifact directory must not already exist.

## Original remote reproduction

The original `/root/.cache/brewfs` on ax-srv is retained. The old binary
(`de36d2dc114611cc174525d881b30b6a184fb667cfa28214cbf5ba0e40ffb204`)
reproduced `agent-2.txt = "hello-fr"` with SHA-256
`29e21cde6e8e6a46f6647aa3e24dbc190b6d9997d91001e3e129b71cdefc481d`.
The other three files were correct and all four mounts reported `NO_LEAK`.
After cleanup, all four original clean-cache file hashes were unchanged.

The first baseline attempt hit a harness double-unmount race. The harness was
corrected to signal the owning process and wait for its unmount; the complete
baseline rerun is `remote-before-r2.log`. No product teardown change was made.

Candidate binary SHA-256:
`2b2b1b5b3b18ef12a13607b7e55fd983cc81b63463a8bd591812fd3267a9c4bb`.

The owner completed the corrected-binary proof on ax-srv and supplied the
result in `/tmp/cache-pass-commit.txt`, preserved as `remote-proof-owner.txt`
in the artifact directory: **two rounds of four concurrent mounts passed**,
including correct `agent-N.txt` contents, `NO_LEAK` in both rounds, remount
persistence, and `ORIGINAL_STALE_CACHE_PRESERVED`. The proof used the candidate
hash above. The retained remote directory is
`/data/brewfs/cache-scope-after-20261023`.

The byte-verified contents include a trailing newline; their SHA-256 values are:

| File | SHA-256 |
| --- | --- |
| `agent-1.txt` | `91421995bf91b3643a4bb9a802e0554cbeb3d85eb9c502583956ef5db0b2beb2` |
| `agent-2.txt` | `77c574151eae1d29393137cea4a6fe37ffcaeafb55c44bdd1df6e125e104e5f0` |
| `agent-3.txt` | `8eb3f22e68936190e06c0ba0bd1a8ca4a12241569c0b315bec7fa35e76afc7d7` |
| `agent-4.txt` | `8970dd97136e5229599227f038cdeca888045a3e556069c9094eba1d80e7d463` |

After receiving that result, the delivery agent independently rechecked the
remote executable's complete hash, all four original stale-cache hashes, and
the absence of FUSE mounts. It retrieved all eight mount/unmount logs and the
cache hash inventory (`remote-after-mount-logs.tar`). The stale files exactly
match the baseline inventory; no cache clearing was used to obtain the pass.

Local checks completed in this iteration: the three focused tests above,
`cargo fmt --all --check`, `git diff --check`, and Python syntax validation.
The focused command compiled the overlay-enabled library and mount binary.
Broad workspace check/build/test/clippy and native-packed-base compilation
were not rerun on this branch before the owner's explicit commit instruction;
successful gates from other worktrees are not counted as this branch's gates.
No new CSI deployment, real-S3 smoke pass, or packed-recovery end-to-end result
is claimed here.

Raw commands, scripts, logs and binary hashes are preserved in
`/mnt/brewfs-data/src/cache-scope-20261023`; current progress is linked from
`/tmp/cache-bug-progress.md`. Builds and tests are serialized and build
resources bounded.
