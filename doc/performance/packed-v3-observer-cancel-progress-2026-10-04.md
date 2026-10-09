# Packed v3 observer / cancellation validation progress

This is implementation and correctness evidence, not a performance result or
all-SPEC acceptance. Only v3/005 is in scope. Workspace, operator, large-directory
and three-innovation requirements remain required; no old-format compatibility
work is added here.

## Integrated checkpoint, 2026-10-05

The latest `integration-focused02` passed all seven stages on 376 unchanged
source inputs, manifest SHA256
`b743bfa6bdc5e599cf913200396e5fdbd4b445394af235df6db83fe84d0ca5a1`.
Counts are existing API lib 3/bin 4, G15 lib 6/fixture 1, adapter 10, registry 5,
packed wire005 132 (3 ignored), writable ACL 23, vendor lifecycle 10. The entire
adapter cancellation/minimum-Control test file is byte-identical to focused01.
The protected verification recomputes source, backup, command and log identities.
Separate production vendor io-uring 49 (15 ignored)/Tokio 26 passed; these do not
replace the full gate. The Native matched plan/data/path test passed in green04.

Two new existing-API tests compiled and failed before implementation: the 32 KiB
capability threshold and a cancelled ticket releasing its fixed slot too early.
The repair prepays fixed registry and bounded collection structures in Roots,
shares one admitted key Arc, moves independently admitted recipes into jobs,
and retains source/execution scratch in Plans. Waiter 2048, contribution 256,
final frame 512 and receipt 2080 Control bounds remain. At most eight collection
futures belong to one worker and shutdown joins it; collection Vecs grow with
actual jobs. No budget is increased to make the strong 32 KiB fixture pass.

Full gate05 is now running against 378 frozen inputs (the focused set plus
AGENTS and vendor Cargo.toml.orig), manifest SHA256
`229d1e14e329e0e542bbaca7c0a49f225cc9017046206c14b0c084b1dcb85beb`.
Gate04 preflight refused an omitted cbindgen.toml input before execution;
gate05 includes it and matches all 52 exact v7 commands. Runtime builds and
fresh real FUSE acceptance are still pending. The paragraphs below retain
intermediate failures and their original source scope.

The active integration has progressed beyond source03 and green03 below. It
contains ACL/G08/G09/G15 plus readonly worker shutdown and bounded control
response ownership. Current formats are exclusively PM11/BP11/FD06, wire 005
and IP06. Twelve further distinct real behavior failures were preserved across
tests-first stages; duplicate lib/bin tests are counted once. Dual-body timeouts
occurred before destroy and are not destroy behavior failures.

The completed seven-step `integration-focused01` checkpoint passed existing API
lib 3/bin 4, G15 lib 6/fixture 1, registry 5 and writable ACL 23. Adapter tests
passed 8/10. Packed wire005 passed 116 with 9 failures and 3 ignored. Vendor tests
did not compile because two extended fixtures lack observer fields. The passing
old vendor lifecycle stage (10/10) is separate source evidence.

Remaining focused work covers 32 KiB Control ownership, pending independent
collections, PM11 assertions with legacy rejection, explicit lazy-reader
shutdown/join and zero ownership, and Native atomic create-only test forwarding.
The v7 harness passed 30 static/synthetic checks, including negative receipts;
it has not run real FUSE. New focused success, a same-source 52-check gate,
explicit runtime builds and fresh mounted validation are required before any
acceptance. Evidence is retained under
`packed-v3-completion-20261004-integrated-contracts/root-validation/` and a
verified D source/log backup. System S, experiment X and all-SPEC remain open.

The historical checkpoints below retain their original source scope.

The next standalone coordinator test compiled and proved that a pending actual
body blocked a later independent collection (`integration-coordinator-red02`).
Its repair keeps up to eight collections in a worker-owned FuturesUnordered;
mount Roots prepay their state and shutdown joins that worker. In
`integration-coordinator-green02`, all 14 pipeline tests and the real dual-body
destroy test passed. Packed wire005 progressed to 125 passed/1 failed/3 ignored;
current PM11 and explicit reader-close zero-owner tests passed. The remaining
Native failure is `get_paths_bytes`: an 8 MiB path arena nests another 1 MiB
names reservation under the default 8 MiB Workspace capacity. The data plans
and reads passed before this failure. The next fix borrows the existing path
arena for reverse-name scratch; the original 8 MiB capacity and path bounds
remain enforced. Vendor green02 compiled neither behavior test: root placed
the fixture fields into an adjacent existing constructor, then corrected the
precise constructor and checked byte equality with the frozen reviewed patch.
Both intermediate failures and sources remain retained. New validation is
required for these corrections and the separate 32 KiB ownership repair.

The observer-active source03 manifest identifies 363 inputs, SHA256
`6faee2f71dc35fe474db5f289720b89fe7026accc405d3fdfab5f67c951a1938`.
All 49 same-source local gate03 stages passed. Actual passed counts are default
lib/bin 1,051/1,135, overlay lib 1,408, fixture 7, native packed 202, readonly
compile-fail 3, and typed-native/observer lib 1,366. The runtime-only vendor gate
ran 31 tests. Separate production buffer-pool/file-lock/unprivileged checks ran
33 io-uring and 19 Tokio tests; all 15 explicit io-uring kernel tests passed.
These scopes are recorded separately rather than inflating the runtime-only
test count. Both explicit runtime binary/fixture pairs were built and frozen
with actual compiler-artifact features, Cargo JSON, toolchain and gate receipts.

Two fresh production io-uring/raw mounts used the actual S3 adapter against an
owned read-only HTTP oracle. Each imported 10,066 source entries, 10,067 inodes,
21 groups and 64 frames. Both failed runs retain complete workspaces, source and
object hashes, physical HTTP logs and diagnostics. Both normally unmounted with
daemon exit 0 and no owned process, mount or thread survivors. The verified D
copy retains 10,172 / 10,312 files, 296,899,271 / 312,957,868 bytes respectively.

The first run stopped at a harness assumption that fstat size equals the open
stats snapshot length. The exact running WSL 6.18.33.2 kernel's fuse_getattr
passes NULL file into fuse_update_get_attr; it does not forward the open FUSE
handle on this inode-attribute path. Fresh observer inode attributes and retained
open snapshots therefore have different lengths. Draft-wire005 records bounded inode
sizes and proves complete same-FD content stability by reading to EOF twice with
different chunk sizes, including the first partial-read prefix. Production stats
source is unchanged by this harness repair. The original worker and failure are
preserved. Kernel evidence is the official
[matching source](https://raw.githubusercontent.com/microsoft/WSL2-Linux-Kernel/linux-msft-wsl-6.18.33.2/fs/fuse/dir.c),
line 2198, with source hash in live-gate03-first-two-runs.json.

The second run passed manifest-only startup, inline and payload reads, cold
xattr, external and hole reads, physical warm/no-index-GET followed by pressure
and identical-object/range refetch, actual 503 retry, corrupt/short EIO with clean
recovery, and retained stats output release. It then failed 16-concurrent-read
validation because a budget rejection reached FUSE as EIO. This is an actual
implementation defect, not a reason to accept unexpected EIO in the worker.

The subsequent source03 cancellation/errno batch has 366 pinned inputs, SHA256
`3ed951c111affab5e0c2081dbdd8543222f1fc6e55ddd93cf4c58be7193e6557`.
Its 14-file patch SHA256 is
`69b3243a5318d728af649cf3d1d3233a7aebfc61a0148b7ecc56c9464eab5876`.
Real tests-first compilation succeeded before five behavior failures: unknown
interrupt returned success, active body reads with existing/temporary handles
did not cancel, EIO leaked a temporary handle, and preparation admission returned
EIO instead of ENOMEM. Two additional real LoggingFileSystem worker/control tests
failed independently because cancellation capability and interrupt were not
forwarded. After errno repair, a stronger test exposed admission recorded as
Backend: zero Admission failures versus one expected. These eight distinct red
cases and their corresponding frozen source snapshots are retained.

The repair registers actual read uniques, aborts the entire opted-in readonly
read future, preserves caller-owned handles, and closes temporary handles before
the response. The wrapper forwards capability and interrupts. The common reader
preserves admission errors through nested metadata/IO/packed error chains for
both FUSE ENOMEM and the logical Admission terminal reason. Recovery is tested
with the same handle after the held Plans budget is released. Actual green03
results are six adapter tests, three registry tests, and four real worker/control
tests, all passed. Formatting and diff checks passed. This is a focused green
checkpoint: it has not yet passed its complete AGENTS gate or fresh real FUSE
acceptance, and gate03's old binaries do not validate these new sources.

Artifacts are under docker/compose-xfstests/artifacts:

- packed-v3-completion-20261004-observer-active/local-gate03-verification.json,
  production-vendor-gate03/verification.json, runtime-builds-gate03/ and
  live-gate03-first-two-runs.json.
- packed-v3-completion-20261004-fuse-interruption/root-validation/red01/,
  green01-wrapper-red/, green02-admission-class-red/, green03/,
  green03-on-source03.patch and green03-checkpoint.json.
- packed-v3-completion-20261004-live-observer-budget-harness/draft-v6/ preserves
  the original same-unique cancellation requirement and handles actual pretty
  tracing field syntax. --op-log explicitly controls wrapper verification. This
  revision is still awaiting real execution.

Readonly worker shutdown, bounded refusal/control replies, the smallest valid
Control profile, fresh four runtime/codec mounts and real same-unique kernel
interrupt correlation remain open. Larger RSS, full lifecycle G04/G08–G17 and
system S / experiment X remain open. Kernel memory release still requires the
existing original CQE or conditional quiescence proof; a permanently wedged
kernel with neither remains unproven. No campaign, commit or push is performed.

## 2026-10-05 complete-input gate checkpoint

Gate05 reached 35 passing checks, then overlay lib had 1485 passes and four
failures. The preserved failures were three stale shared PM10 assertions and
one now-rejected unauthenticated p90 hint. Two test-only corrections retain every
normal size/profile read assertion and require the documented unproven-hint
rejection. Five readonly fixtures, the size-class corpus, fmt and diff passed in
`integration-format-corrections01`; no trusted histogram support is accepted.

Gate06 was intentionally interrupted after 33 passes when static coverage found
two missing stats-package inputs and five native include_bytes fixtures. Its
next runtime check exited -15 and its controller/Cargo groups fully terminated.
The frozen source and D logs matched; this interruption is not a behavior red.
Neither incomplete gate can validate the new runtime binaries.

Gate07 pins 412 inputs, SHA256
`64f809eb7d27f8c882c563bf76a6f8831412c2f1474ca0da4b43f820132ad8ef`.
The actual v7 builder coverage check and the independently audited 381 build
inputs / 27 gate scripts and helpers passed before execution. All 52 exact gate
commands remain. The gate exited zero with all 52 checks passed. Independent
`full-gate07/verification.json` confirms every active/protected source and log
hash, exact command coverage, and zero Cargo/target process survivors. Overlay
lib passed 1489 tests (231 ignored), typed native/observer 1447 (231 ignored),
and default bin 1147 (225 ignored). Both explicit runtimes subsequently built
and froze on D: with matching protected copies. The first fresh
`io-uring-raw-normal-01` completed all 17 recorded operation stages but failed
the final sampler assertion at runner line 465. Six RSS/PSS samples exist;
the original runner did not persist the specific sample_error, so no cause or
memory/normal acceptance is inferred. The matrix stopped. Full work/logs are
protected on D:, and mount, daemon, owned groups, sampler and oracle terminated
without cleanup errors. A separate diagnostic wrapper will preserve the exact
error while retaining v7's assertions. The remaining normal/cancel matrix,
logged wrapper, minimum Control and fault teardown remain pending.
Private G10/G13/G14 candidates are not behavior acceptance.
System S, experiment X and final campaign remain open.

## 2026-10-05 strict sampling and cancellation parser checkpoint

The diagnostic `io-uring-raw-normal-diag02` captured worker smaps_rollup ESRCH
on the wrapper's exited/zombie child. The replacement sampler retains the actual
Popen owner and only omits ENOENT/ESRCH when that child's poll proves exit;
daemon/live worker errors remain fatal. It writes omissions/errors and joins
the sampler before checking frozen RSS/PSS samples. Strict sampler03 produced
135+ samples without a sampling error, but failed cache eviction/refetch.
Coldpressure04 failed that same assertion. Complete failed work, object/source
hashes and cleanup are retained under live-gate07 on WSL and D; no later finding
backfills normal01's missing original exception.

The authenticated IP06 page inspected in cache-preconditions-gate07.json has
254704 bytes of owned payload before structural overhead, greater than the
original 131072-byte cache. The original stable-warm premise cannot be accepted.
At 1 MiB warm succeeded, but the frequently accessed target was not proved
evicted. A new independent target/pressure probe is being built. A separate
private retention-lease implementation must prove existing API red/green and
real small-cache behavior before its owner metrics can be accepted.

Cancellation sampler01 terminated and was protected with no owned survivors.
It delivered zero logical bytes, recorded one failed/cancelled logical operation,
and captured real READ unique512 → INTERRUPT target512 → cancelled unique512.
The fixed five-second delayed 524288-byte HTTP body disconnected after 65536
bytes. The run nevertheless remains FAILED: the old regex consumed the tracing
namespace's request_cancel: event pair and lost the event field.

New draft-v9-owned-sampler-cancel-parser preserves all 85 assertion ASTs and
parses fields only after the exact namespace, handling actual ANSI/colon and
equals formats with duplicate rejection. Six regression tests gave five legacy
failures and six new passes. Receipt and logs are in
D:/Codex-Recovery/brewfs-root-20261004/cancel-parser-v9-verification/;
runner SHA256 f6852b1da659be690443ccb67aac6b114721645ecc53363896a118ca095a166f.
The fresh six-case matrix is terminal: four runtime/codec cases and the logging
wrapper passed; Control=32768 FAILED because opening complete `.stats` during
the physically inflight body returned ENOMEM. All cleanup finished without mount/
group survivors or cleanup errors. This is an actual production admission
defect; do not increase the minimum capacity or ignore the failed stats read.
The later v10 strict parser rejects missing/invalid u64 IDs, false future/handle
release flags and embedded namespaces. Its 11 tests pass, with all 85 original
assertion ASTs retained. Independent verification of all five successful raw
logs, protected work, statistics and cleanup is running under handle 8436;
the failed minimum-Control record remains unchanged and open. That verifier
has now finished exit 0: all five standard/logged successes independently
passed; no minimum-Control or normal acceptance was inferred.
This does not accept normal mounts, hard cache ownership, fault teardown,
post-unmount owners or system S. No performance claim or campaign is made.

Fresh geometry normal01 still failed same-identity eviction/refetch after a
full-body prime and zero-index warm. Authenticated unrelated pressure exceeded
the 2 MiB cache without revisiting the target. Failure work is retained and the
matrix stopped. Do not declare every refetch an eviction or remove the target
requirement. Separately, real body-hold→daemon SIGTERM failed ordinary unmount:
daemon exit1, worker still live, mount still present, fusermount EBUSY. Only
failure cleanup subsequently released the HTTP hold and externally unmounted.
All groups/mount/sampler/oracle then ended without cleanup errors. These are
two additional real failed records, not passing cleanup substitutes.

The body-hold candidate's nested 93 source files are protected and verified in
live-gate07/io-uring-raw-inflight-teardown-strict01/sealed-probe-inputs on D,
with nested-probe-protection.json. Read cancellation before ordinary unmount,
full task/ring ownership and minimum-Control stats coexistence now require
production repair. Root is compiling the retention existing-reader stage-01
tests on active; no production candidate or later gate has been accepted.
