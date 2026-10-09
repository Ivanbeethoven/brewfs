# Packed v3 G12/G13 catalog safety — 2026-10-07

Scope is v3/005 only. This checkpoint covers catalog publication and native
metadata deletion. Full G12/G13, system S, experiment X, packed object graph GC,
durable reader pins, history retirement and formal performance acceptance remain
open.

## G10 rollback review

The successor02 temporary tests-only candidate was correctly reversed. Its
original report records SIGTERM (`exit_code=-15`), zero passed/failed test lines,
successful reverse application, `source_restored=true` and
`production_source_changed=false`. It does not establish a completed test run
or production rollback. The active tree
retains bounded v3 open, catalog-backed packed attach, G11 generation checks and
later G13 protection. No restoration of the rejected candidate is required.
The current `g10` focused run passed all three tests. This result applies to the
active implementation, not the rejected candidate.

Original rollback report:
`C:\Codex-Recovery\brewfs-root-20261005\v3-plans-repair01\g10-control-corrected-tests-only-run02\report.json`.

## Publication retry and inode reservation

SQLite and KV recognize an already committed publication only when the complete
target binding, predecessor history, base and head records match. The head must
have exactly the publication's one sequence advance; the allocator must remain
above the target namespace floor. A live lease, holder and target epoch remain
required. KV performs a read-only timed CAS before returning an existing result;
the retry does not change root generation, epoch, sequence or allocator.

Independent review found that first publication could expand a lower namespace
into already issued native inode IDs. Five tests use real authenticated
producers: the initial lower has inode 400, and its replacement actually contains
401 and 450. Before the production fix, the two issued-ID rejection assertions
failed; the unissued-range SQLite/KV controls and the KV concurrent allocation
control passed (3 passed, 2 failed). The RED log is retained.

The shared first-publication predicate rejects an allocator below the old
reserved floor as corrupt. Namespace expansion is allowed only while the
allocator still equals that old floor; otherwise it returns
`UnsupportedCapability`. SQLite rolls back its complete transaction and KV
writes nothing. Committed retries use the target-state predicate instead, so a
legitimate allocation after successful publication does not invalidate a retry.
This proves reservation safety for issued IDs; source namespace provenance and
complete dependency durability remain separate open contracts.

The first retry test run had 8 passes and one test-adapter failure: the scheduled
head mutation attempted to decode a KV envelope as a bare bincode payload. Only
the adapter was corrected to preserve `BWSKV001`; no production codec was
changed to accommodate the test.

## Binding roots and destructive revalidation

KV captures `packed/v3/root-generation` before scanning PWB3 history/current.
Install and new publication atomically advance that key together with the
binding records. Native layer marking and finalization check the captured
generation and every scanned binding value in their final CAS. This fences a
first binding whose newly created keys were absent from the earlier root scan.
Malformed binding identity, malformed generation, zero generation and overflow
fail closed. Bound base/head layers and their parent closure remain protected.

SQLite finalization rechecks all durable PWB3 history roots and their native
parent closure in the same `BEGIN IMMEDIATE` transaction as deletion. Any bound
candidate rejects the whole batch before rows are removed.

The first G13 run passed all six `g13d_` tests, including public first-binding
installation after a root scan, direct finalization of a bound Deleting head,
SQLite mixed-batch atomicity and persisted-history ancestry.

An additional public-API review found that KV finalization scanned delta keys
before verifying the candidate state. A last writable mutation followed by
workspace deletion could therefore leave newly written rows behind. A layer
deleted and recreated under the same UUID also requires the scan's original
authority to remain unchanged. The corrective slice requires Deleting before
scanning and retains those original exact layer values through the final CAS;
absent candidates also require an exact absence check.

Exact layer bytes alone do not detect every deletion/recreation: the public
orphan API can recreate the same UUID and byte-identical LayerRecord with a
different slice. An additional durable layer-inventory generation must advance
atomically whenever the layer ID set changes. Finalization captures it before
scanning and verifies it in the destructive CAS. The raw-only candidate is
retained as a rejected design, not accepted as a complete race fix.

The original first-binding test's marking lane retains its strict generation
fence assertion. With the new pre-scan guard, its Writable finalization lane may
return Busy before scanning; that result proves the state precondition rather
than a generation CAS. Separate finalization race tests cover scanned authority
changes and staged cleanup.

## Validation status and artifacts

The initial `g13d_`, `g10` and `binding_same_head` runs passed 6, 3 and 2 tests
respectively. Formatting and the AGENTS perf shell syntax/report checks passed.

The stores RED then compiled and ran all 117 selected tests: 105 passed, exactly
the three new finalization cases failed, and 9 live-service tests were ignored.
Each failure explicitly returned `Ok(())`. The inode reservation and publication
retry tests passed in that run. After applying the inventory-generation fix,
the full gate's stores stage passed **108 tests, 0 failed, 9 ignored**. Binding
tests are byte-identical to the inode RED snapshot, and G13 KV tests are
byte-identical to the finalization RED snapshot. The obsolete raw-layer-only
candidate was never applied.

The first current-source complete local CI gate finished with 45/49 commands
passing and all 557 frozen inputs unchanged. Default workspace tests passed
1,164, overlay library tests passed 1,787 plus 8 fixture tests, and all-features
library tests passed 1,802; all had zero failures. Overlay Clippy rejected a
collapsible conditional and a same-type integer conversion. Operator Clippy
rejected a hand-written default implementation. Real operator CRD generation
panicked while flattening the internally tagged binding-status schema, so its
CRD diff was a dependent failure. These failures remain recorded in gate11;
they are not a successful full gate. The corrected final source passed the
new complete gate described below. Ignored Redis/TiKV tests in the offline
gate do not count as live backend evidence.

The Clippy corrections preserve existing shutdown and allocator behavior.
Operator binding status now has one structural object schema rather than
conflicting tagged-union branches. The existing four JSON states and
`manifest_digest` wire field remain; version must be positive and the digest
must contain 64 lowercase hex characters. A CEL field-presence rule requires
both fields only for `present`. The new test actually calls
`BrewFSWorkspace::crd()`: before the fix it reproduced the original kube panic,
and after the fix it and the tagged-wire roundtrip control both passed. CRD
generation exited 0 and its output was inspected and regenerated in-tree. This
does not establish Kubernetes API-server acceptance or lifecycle E2E.

Three ignored Redis G12 tests now reuse the existing generic public contracts:
same-head exact retry, expansion of unissued IDs followed by retry, and issued-ID
collision rejection. They use unique namespaces and clean only their own keys.
All three then passed against a real, isolated Redis 7.2 service; no
transport-response-loss or G13 race claim is attached to these tests.

The final full gate actually exited 0 in session 55436. All **49/49 commands**
passed against **558 frozen source inputs**, with unchanged configuration and
Git identity, on the same
Rust/Cargo 1.98.1, single-job, no-incremental/no-debug profiles as gate11. Its
stores stage passed **108, 0 failed, 12 ignored**; default workspace passed
**1,164/225 ignored**, overlay library passed **1,787/240 ignored** plus **8
fixture tests**, all-features library passed **1,802/240 ignored**, and operator
passed **28 tests**. Strict default/overlay/operator Clippy, all runtime checks,
CRD generation and byte comparison, formatting and diff checks all passed.
The all-features summaries include ordinary offline integration controls;
they do not certify live Redis/RustFS Docker, TiKV or Kubernetes testing.

Final gate artifacts:
`C:\Codex-Recovery\brewfs-root-20261005\v3-plans-repair01\full-rust-gate12-g12-g13-20261007\`.
Its `source/` inventory is authoritative for the accepted source; earlier
six-file `final/` snapshots retain the pre-Clippy checkpoint. Manifest SHA256:
`90017f0bb81779d34c917ca6ef9625f5b84b394c2e01229babf03f7cb78f3a35`.
Verification SHA256:
`f354cd7ff7ce4eff4e1e1dd5a8bebd4e60eb3a5ed7a0fa382ad2bd2d6a85784c`.

The subsequent owned Redis run actually exited 0: **4 filters, 8 tests, zero
failures or ignored selections**, with the exact same source inventory,
configuration and Git identity. It covered initial binding, v3 open, the three
G12 publication/allocator contracts, real-clock deadline CAS, distributed
catalog and corrupted-key-index atomicity. Logs and verification are in the
batch's `real-redis/` directory. No G13 race, response-loss transport injection,
complete packed dependency graph, durable journal or TiKV result is inferred.
The service used a cached image, a loopback dynamic port, 96 MiB memory and
16 MiB tmpfs with persistence disabled. Cleanup returned 0 and a successful
container-list query confirmed its absence; all owned process groups were
empty. No other containers, volumes or data were removed.
Redis verification SHA256:
`f2735fb4f73488d6ceb719328b740cbf27455fba507b0b44ee9a150f8b8707b4`.
The gate11 failure logs and schema RED source/log remain preserved separately.

Raw logs, RED sources, candidate patches and six-file snapshots are retained at:

`C:\Codex-Recovery\brewfs-root-20261005\v3-plans-repair01\g12-g13-validation-20261007\`

The complete gate froze its source inventory and stored every command's exit
status separately. No commit, push, production deployment or formal performance
campaign is claimed by this checkpoint.

## Next required lifecycle work

The producer already uploads dependencies before the manifest and reads them
back; the index builder writes children before parents. Those steps prove the
producer's generated objects individually, not a complete authenticated graph
at catalog publication. `VerifiedPackedLower::from_authenticated_snapshot`
checks only the manifest and the maximum-inode route. The next closure proof
must traverse all IP06 children, allocation/source roots, container/FD/cold
references and nested external extent/chunk references within explicit memory,
depth and object limits. A missing dependency away from the maximum-inode route
must prevent SQLite/KV catalog side effects. Durable staging roots are still
needed to protect candidates between verification and publication.

The existing native seal API cannot directly compose with same-head packed
publication. Native `begin_seal` changes the workspace/head to Sealing; packed
publication requires Active/Writable. Native `commit_seal` rotates head/base,
while the current PWB3 request fixes them. Native recovery stores no candidate
manifest, and the producer's private temporary spool has no reopen API. A packed
journal sidecar and atomic head/binding/journal commit require their own crash,
reopen, stale-holder and late-abort tests.

Persistent reader pins, protected noncurrent-history retirement and packed
object graph mark/sweep remain unimplemented. Current same-head publication
keeps native base/head unchanged; retiring an old binding version cannot be
used as evidence that distinct native layers became reclaimable. Version 1 is
still the current implementation's mandatory open/publication anchor.
