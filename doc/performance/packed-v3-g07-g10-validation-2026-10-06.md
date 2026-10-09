# Packed v3 G07/G10 validation — 2026-10-06

This checkpoint records the post-format active source on
codex/packed-metadata-aliyun-20260930 at a429b0e1bc1c158af06ecf738e552062123d6e00.
It is evidence for focused development gates only; it does not close G07, G10,
S/X, or the complete SPEC contract.

## G07 focused lifecycle checks

Using Rust/Cargo 1.98.1 with CARGO_BUILD_JOBS=1, CARGO_INCREMENTAL=0,
CARGO_PROFILE_DEV_DEBUG=0, and CARGO_PROFILE_TEST_DEBUG=0, one serial run
passed all three exact cases:

- provider cancellation retains the actual coordinator JoinHandle until the resumed join;
- worker cancellation retains every actual readonly worker handle until resumed join;
- two concurrent reader closes join and retire the idle coordinator.

The post-format report and logs are frozen under
/mnt/c/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/g07-current-api-shutdown-cancel-tests04/.
The current source hashes are coordinator bd84e4f0..., coordinator tests
fe223e41..., and asyncfuse worker 1ad14113.... `cargo fmt --all --check` and
git diff --check pass. The focused checks do not prove owner-drop accounting,
JoinError propagation, ordinary unmount, native/provider/reply collection, or
the nine strict SIGTERM retirement items.

## G10 bounded-open candidate

The earlier existing-API tests-only candidate was correctly rejected and reversed: its
G10a/G10b observations grew with unrelated forks and terminal journals. The follow-up
bounded-open v3 candidate is now applied to the active worktree. It changes the open
consistent read to use the immutable volume/header and workspace-scoped
open/v3/recovery/<workspace> marker, updates the marker by atomic CAS, and fails
closed when a Sealing workspace lacks recovery state. Legacy catalogs materialize the
header during schema initialization. The open path no longer transfers the growing
global CONTROL document.

The isolated candidate evidence is under
/mnt/c/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/g10-bounded-open-fix01/:
G10a/G10b/G10c passed, the invalid-head/workspace-state topology case passed, and the
malformed-header/oversized-owner case passed. The G10 candidate source hashes at that checkpoint were
src/workspace_overlay/stores/kv_store.rs =
70f98eaa8514b1016a0a1e86d7c802899344fee2bd6c1c9644a3c195e6b107a9 and
src/workspace_overlay/stores/g10_control_open_growth_tests.rs =
7ceef9b884f078ef8dfdfdf56cb1137464d3253a0328a6ff09f0244c7ac94db4.
The active tree subsequently added the G13 PWB3 native-root retention contract; its current kv_store.rs hash is 1e551e6fb22294c60a752e1a8137470c29ada3afe4141c1beb87b9b1717afbbc.
The active focused rebuild used the same serial, explicit C: target configuration.
The three growth cases passed, and the topology and malformed-header cases each
passed. The workspace lib/bin gate then passed 1163 tests, with 225 ignored and
0 failures.

This is a bounded-read and topology proof only. Real Redis/TiKV admission, the full
fork→mutate→seal→remount→GC lifecycle, and the remaining G10–G14 binding/fence/
publication/recovery contracts remain open.

## Resource hygiene

Only rebuildable Rust incremental data and the temporary G10 target were removed;
source files, vendor code, .claude, credentials, and prior evidence remain.
After cleanup the BrewFS target is about 88 GiB, with approximately 98 GiB free
on D: and 237 GiB free on C:. Future full gates should use a single explicit C:
target and -j 1.

## Still open

The active v3 goal still requires G06/G07 mount-wide accounting, G08/G09 native
and 005 pipeline integration, G10–G14 binding/fence/publication/recovery/GC and
operator capability lifecycle, G15/G16 controls and reproducible runners, G17
matched acceptance, and the real S/X lifecycle/failure-recovery exit. Historical
49/49 and 17/17 gate results remain pre-production-source evidence until rerun on
this source map.

## G07 ordinary worker ownership and JoinError ledger

The ordinary-worker successor was applied to the active vendored asyncfuse source after git apply --check passed against the recorded source hashes. Ordinary non-READ handler JoinHandles are retained in a worker-local FuturesUnordered and drained before the worker exits; Session::dispatch now shuts down workers for ordinary and readonly sessions. The follow-up ledger records child and outer worker JoinError values, preserves a primary dispatch result, and returns a worker error only when no primary result exists.

Active source hashes after formatting and ledger integration are:

- vendor/asyncfuse/src/raw/session/worker.rs: 7eae7454317cb6c975576f399defe97d7c716ddfa2443a7fe9675f0a9ad0a675
- vendor/asyncfuse/src/raw/session/mod.rs: 0412fae9db222ced3ffcd1c373e2d706755adf28f380c0d6e455bec1c4cea58e

Validation on the active source used Rust/Cargo 1.98.1, serial jobs, and an explicit C target. The asyncfuse crate passed 47/47 lib tests, including the ordinary handler join, controlled JoinError ledger, and readonly shutdown-cancellation regression. The io-uring ordinary join/ledger pair passed 2/2 tests; async-io no-default compilation passed; and the full workspace lib/bin gate passed 1163 tests, with 225 ignored and 0 failures. cargo fmt --all -- --check and git diff --check also passed.

This closes only the ordinary worker ownership and JoinError subcontracts. Workers::Drop cannot await, provider coordinator JoinError/Drop handling, physical ordinary-unmount error ordering, reply/native ring/kernel delivery, direct-control packets, post-unmount budget accounting, and strict SIGTERM retirement remain open.

## Provider collector JoinError ledger verification

The provider collector ledger candidate is verified on the active v3 source with
Rust/Cargo 1.98.1 and serial build settings. The exact test
`workspace_overlay::packed_v3::wire005::pipeline::coordinator::tests::current_shutdown_records_provider_collector_join_error_before_handle_retirement`
passed 1/1 (`--exact --nocapture`); the controlled collector panic was observed
and the test completed successfully. The active source hashes are
`coordinator.rs` 279aed01d438f0b7ff95fd4ab318a6a65fb4bc0955afd654fedfac992be96dd5
and `coordinator/tests.rs`
c9a32b954456535cd5439228af73685170c3acc593a3b762de06264e0f34916d. Expected
`JoinError::is_cancelled()` from coordinator shutdown abort is filtered, while
panic and other JoinError values remain recorded before the original handle is
retired. This evidence does not close physical ordinary-unmount error ordering.

## Ordinary physical-unmount error ordering candidate

The ordinary physical-unmount candidate now notifies and joins the Session
cleanup task before returning a physical unmount error, while preserving the
physical errno as the primary result. The exact strengthened test
`raw::session::unmount_order_tests::ordinary_unmount_error_is_preserved_after_read_preparation`
was RED before the patch (`destroy_at_return` was 0) and GREEN after it (1/1).
The asyncfuse io-uring library gate passed 89 tests with 15 ignored, and the
no-default async-io configuration compiled. Candidate evidence and the patch
are frozen under
`/mnt/c/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/g07-ordinary-unmount-cleanup-candidate01/`.
The candidate covers direct and unprivileged Linux paths, runtime join errors,
and the test seam; native kernel unmount acceptance remains a separate gate.


## G10 catalog-backed binding attach contract

The active source now exposes `WorkspaceMetaLayer::with_packed_v3_lower_from_store`. It
loads the current `PackedLowerBinding` under the workspace guard and installs the
catalog-backed authority; when the binding is absent it returns `ESTALE` before attaching
any packed lower. This closes only the attach fail-closed boundary. It does not publish or
replace PWB3 records and does not provide seal/recovery ownership, mutation-generation
fencing, reachability GC, or operator lifecycle capability.

TDD evidence is frozen under
`/mnt/c/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/g10-bounded-open-fix01/`:

- RED compile: `binding-attach-red.log` (`16f9e8f3c034f703fefcaec57092cbb46e93375a6d5bdc857c8b0ef80db935d`), missing API and the test fixture ownership move were exposed.
- GREEN exact binary: `binding-attach-cargo-exact.log` (`5ddfaf9f1d4a3208febe51a17f098e15f097e2fbe3d1d9caedb3def233311c7f`), 1 passed and 0 failed.
- `cargo +1.98.1 fmt --all --check` and `git diff --check` passed.
- Isolated patch: `binding-attach-contract.patch`, SHA256 `d036cdc2487c1d59b0815213cbed9708d31108317ba389e675177d0ec544d5b9`.

The exact Cargo command used was `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo +1.98.1 test -p brewfs --locked --lib --features workspace-overlay workspace_overlay::meta_layer::tests::packed_lower_tests::public_catalog_binding_attach_requires_persisted_binding -- --exact --nocapture --test-threads=1`. The focused source hashes are `meta_layer/mod.rs`
`a68f59d791e52275976942007e14eabaf9e259f0320a3da35a963fbc56f2797a` and
`meta_layer/tests/packed_lower_tests.rs`
`3a1ad79d265ef24cd11914bd32f19fe23c7f9c3d0e1e633e7b90536e927ee755`.

## G11 visible upper mutation generation candidate

`ReadGeneration` now carries `workspace_mutation_sequence` in addition to the head epoch
and immutable lower digest. The packed workspace preparer fills it from the captured
writable head's monotonic `LayerRecord.next_sequence`; readonly PM11 plans keep zero.
This makes two plans from the same head epoch distinguishable after an upper mutation,
while the existing `CompositeFetcher`/request fence still performs the authoritative
backend validation and typed whole-request retry.

The existing regression `workspace_overlay::meta_layer::tests::packed_lower_tests::public_prepared_rejects_same_epoch_mutation_and_binding_fence_before_io` asserts that the old and fresh plans have different generations after a same-epoch truncate; it also feeds a deliberately mismatched sequence into the retained fetcher and requires `ReadPlanError::StaleView`, so a stale-generation mismatch cannot be reported as an untyped backend error. The exact focused GREEN command was:

```text
cargo test --no-default-features --features workspace-overlay,fuse-io-uring-runtime workspace_overlay::meta_layer::tests::packed_lower_tests::public_prepared_rejects_same_epoch_mutation_and_binding_fence_before_io -- --exact --nocapture
```

Result: 1 passed, 0 failed, 1951 filtered. `cargo fmt --all --check` also passes. This closes only the visible generation and typed stale-plan contract; the full workspace gate and native/packed lifecycle/FUSE validation remain open.
