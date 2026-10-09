# Packed-v3 Redis/TiKV metadata continuation

Latest 2026-10-08 checkpoint: `operator-closeout-stores13` **failed**, with
418 passed / 2 failed / 103 ignored, unchanged source/Git, in 653.01 seconds.
All ten bounded raw-path tests and the final FUSE consumer owner tests passed.
The failures were VFS rmdir still calling unsupported full readdir, and a new
xattr fixture constructing a conditional write without its required inode.
Log SHA256: `e4e8b2f1910cbe1fe80a649d9a0d7bf0557e61578ae58111c1610fdcc8f32c41`.
Root subsequently integrated the VFS owned-page emptiness precheck and a real
rename replacement regression, replaced the invalid fixture with the actual
raw set_xattr API, removed its unused trait import, and integrated the two direct
readonly adapters' OwnedPaths owners plus three tests. Fmt/diff checks passed;
this later batch has not passed Cargo yet.

The actual RustFS single-case `rustfs-native-kernel-read-diagnostic03` did not
reach the earlier EIO read. Its first 57-byte native WRITE remained pending;
the captured kernel stack is `fuse_perform_write -> request_wait_answer` with
one waiting request. Root recorded the exact runner/test/mount identities and
used pidfd SIGTERM. The phase receipt records exit 143, 0 passed, interruption
and unchanged source/config/Git; it is not a completed test result. Log SHA256:
`562188a80ba9048d7c1775cc0e94d073d62f3ddb929f72a9c3546af71a702c21`.
After normal service/credential cleanup, root verified the exact private mount
and connection 99 against the captured mountinfo, aborted that connection and
unmounted it. `post-retirement.json` independently confirms process, connection,
mount and RustFS-container absence plus metadata/credential cleanup. The original
failed receipt and interruption/abort evidence remain unchanged. No root cause
is asserted. Full RustFS run03 and strict metadata10 have not started.

Earlier passing checkpoint: `operator-closeout-stores12` passed **383 / 0 /
103 ignored**, with unchanged source/Git, in 621.10 seconds. Log SHA256:
`dc1707cc5c692c259b578266ab9db9b503b89445028ed9eb8f3eabf9796b9e5a`.
This snapshot includes the explicit fixture key-lock release.

The fresh actual RustFS `rustfs-metadata-lifecycle-run02` **failed**:
17 original mounted shutdown cases passed, one failed, and the remaining
headless/route four cases were not executed. TiKV's reader-error scenario
failed during the initial genuine kernel read of a just-written native file
with EIO; the failure-only direct native plan returned the correct payload.
Log SHA256: `b682865285ab1bdf6107a109be83b82e2a25b7b424625162706e0a4ef1196f40`.
Owned metadata, proxy, RustFS and credentials were removed and independent
absence checks passed. No passing combination result is claimed.

The complementary strict `runtime-closeout-validation09` **failed** at fork:
1 passed / 1 failed / 23 unexecuted, unchanged source/config/Git and successful
owned cleanup/independent absence. TiKV rejected the final publication packet
because its complete fixed authority keyset exceeded the ordinary 32-key
bounded point-read cap. Its caller permits 64 keys; that mismatch was absent
from the mock regressions. Log SHA256:
`d98c8afcf1c4fadc758725e39d1acdca9e600a2f21e6ffbc48f71b249fa65dad`.
The combined 22+25 verifier cannot accept these inputs; prior partial runs
must not supply missing cases.

After terminal cleanup, root added two temporary test-only read-error
diagnostics and integrated raw lookup/symlink/xattr overrides plus six
regressions. `operator-closeout-style14` passed strict all-target Clippy on that
snapshot, with unchanged source/Git; log SHA256:
`4ab1804e9142e23ff33c8ce9cf4ae464945d62f8ce4a8443a14c58a469097722`.
Later changes integrate bounded packed directory/listxattr merge, its FUSE
consumer owners and memory delegation, the dedicated TiKV publication packet
API, and the 255-byte xattr-name limit. The TiKV API keeps one transaction/start
timestamp and a shared deadline, byte limit and 64-attempt budget; ordinary
point reads retain the 32-key cap. Directory streams and cloned xattr Bytes
retain their reader and output owners until the final consumer drops.
`operator-closeout-style15` failed on three style lints; those are repaired.
`operator-closeout-style16` reached the new tests and failed on one publication
packet test holding a MutexGuard across an await. An explicit lexical scope now
releases the guard before that await. Style16 kept source/Git unchanged; log
SHA256 `8197f330bd5b877346bcc9de830f7dbea3adf296854c038b4148fcbb291db4e8`.
A third test-only diagnostic records FUSE open errors, without paths or
credentials. The newer batch and its tests have not passed Cargo yet. Bounded raw reverse
paths and returned-result ownership are still external candidates. Their
4096-native-row census cap remains a full-scale SPEC gap even if focused tests
pass. Full Rust gate, real CLI/TLS, Kubernetes lifecycle and large
CONTROL/scoped GC remain required. RustFS run03 and strict metadata10 have not
started; run02 and metadata09 remain failed evidence.

The root-overlap repair and explicit RustFS fixture are now integrated.
`operator-closeout-style13` passed strict all-target overlay Clippy, and
`operator-closeout-stores11` passed **383 / 0 / 103 ignored**, including eight
new proof-rebuild regressions, with unchanged source/Git. Its log SHA256 is
`a785bab4db64b5fb43ca0dd598d66167a828fdb54a4f9d619daaa82f3b8b93ce`.
These remain development checks rather than the full repository gate.

The actual `rustfs-metadata-lifecycle-run01` passed all **18 original mounted
shutdown cases** on Redis/TiKV with RustFS, explicit runtime IAM credentials,
and real S3 reauthentication of the initial snapshot. Root then interrupted
the headless phase: a test-fixture key-inventory MutexGuard remained alive
across `consume(...).await`, blocking the publisher's next PUT observation on
the same mutex. The original headless log and owned-pidfd interruption receipt
are preserved. The overall 22-case run is **not passed**; metadata, proxy,
RustFS and credential cleanup and independent absence checks all passed.
The fixture now collects its keys in a separate scope before consumption.
`operator-closeout-stores12` subsequently passed; the newer failed real runs
and remaining work are recorded above.

The latest real-backend diagnostic, `runtime-closeout-remaining-diagnostic08`,
terminated with **44 passed / 1 failed / 0 unexecuted**. Its nine phases kept
source/config/Git unchanged. Exact owned Redis/PD/TiKV/proxy cleanup and the
independent absence checks passed. Initial bootstrap (2), initial composer
(2), clean-source negatives (4), original mounted shutdown (18), headless
route controls (2), native partial-upload resume (12), TiKV postsubmission
authentication (2) and Redis distinct-admin ACL (1) all passed. Headless
snapshot was 1 passed / 1 failed: Redis passed, while TiKV rejected a
root-generation overlap during native owner preparation **before commit**
(`committed=false`). The earlier Redis borrowed-fork inspection failure was
not reproduced; that is not proof that it cannot recur.

The preceding strict `runtime-closeout-validation07` stopped at fork/history:
**1 passed / 1 failed / 45 unexecuted**. Redis passed; TiKV reached publication
assembly and later returned Busy during clean child publication. This remains
an acceptance failure. Diagnostic08 deliberately excluded those two fork
cases and cannot replace a complete strict rerun. Both original result/log
artifacts are retained under `carrier-fork-real-validation01/`.

Diagnostic08 used LocalFS object bytes with actual FUSE and Redis/TiKV
control-plane operations. RustFS IAM09/10 below start no metadata services.
The explicit RustFS combination fixture and owned-service runner are now
integrated and exercised, with the failed run02 result recorded above.

Latest 2026-10-08 closeout evidence: the independent operator passed strict
all-target Clippy (`operator-independent-style03`) and all 50 tests
(`operator-independent-test01`), each with unchanged source/Git. CRD generation
completed; the checked-in manifest now matches its actual output, including
credential-field descriptions. A later NativeHold entry repair and its five
tests passed in `operator-closeout-stores10`: **375 passed / 0 failed /
103 ignored**, source/Git unchanged. Log SHA256:
`8ef5e1a29c25f4bb5e8528eeaf442b5883354accbd9c4834cedb6c839e5b4590`.
This includes the eight repaired stores08 failures and all five Active NativeHold
entry tests. Stores09 failed compilation because the added test backend lacked
the required `server_time_ns` method; its failure receipt is preserved. Delegation
to the inner backend corrected that fixture. The prior operator results do not
certify these subsequent source changes, and the full repository gate remains
required.

The owned RustFS IAM run `owned-object-permission-run-20261008-09` passed both
object-probe labels. Runtime create/read/range succeeded, conditional overwrite
was refused, runtime DELETE returned HTTP 403 with bytes unchanged, and admin
DELETE plus authorized absence checks succeeded. The exact owned container and
temporary credentials were removed. No metadata services were started by this
run. Run09 was not versioned. The subsequent
`owned-object-permission-run-20261008-10` enabled versioning and independently
verified Enabled using its separate admin identity. Both labels passed actual
VersionId deletion denial with HTTP 403, admin version-specific deletion and
independent absence of all versions/delete markers. Exact container absence and
temporary credential destruction also passed. Redis/TiKV metadata, FUSE and
Kubernetes gates remain separate.

The cloud campaign now rejects older wire encodings before any fixture path
access or publication. Its current packed-v3 raw/zstd arms retain the raw
special rows after removal of the 004 arm. Python syntax and the three existing
campaign contract tests passed; the external mocked-dispatch evidence covers
nine fixtures and 57 rows without cloud, services or FUSE. This is runner
validation, not a performance result.

Product/API/configuration names remain packed-v3/V3/v3. `wire005` identifies
the internal encoding; it does not introduce another packed product version.
Old packed-format compatibility is outside the current user scope.

The latest ordinary metadata checkpoint, `mounted-followup-stores05`, completed
328 passed / 0 failed / 101 ignored with unchanged source/Git. Its log SHA256 is
`2a86c3a88e8ec8d403a4cded2032b7cad38053ce8e63b7babc9e9ff4bb442a26`.
Ignored tests require explicit real-backend execution.

The subsequent same-source `runtime-integrated-diagnostic04` completed all 46
real cases: **38 passed / 8 failed / 0 unexecuted**. Source/config/Git remained
unchanged throughout; owned Redis/PD/TiKV and proxy retirement and independent
absence checks passed. Results supersede the diagnostic03 checkpoint below:

| Phase | Passed | Failed |
| --- | ---: | ---: |
| Fork and borrowed history | 0 | 2 |
| Initial bootstrap | 2 | 0 |
| Initial composer | 1 | 1 |
| Clean-source negative controls | 4 | 0 |
| Original mounted shutdown | 18 | 0 |
| Headless snapshot | 0 | 2 |
| Headless route controls | 2 | 0 |
| Native partial upload resume | 11 | 1 |
| TiKV specialized authentication after submission | 0 | 2 |

The fork fixtures fail before mount identity installation because their
writeback directory does not exist. Headless claim rereads stale historical
CONTROL leases despite retaining their actual Released hot rows in its source
ticket. Redis initial composer returns an unlocalized Fenced error. Redis
readback-cancelled recovery rejects a journal timestamp later than its same
authority packet's backend clock; BuildingEmpty now passes. Subsequent
read-only sampling observed no backwards TIME/realtime across 256 samples,
which cannot explain that earlier failure. TiKV postsubmission tests expose
an outdated typed-prewrite-error expectation and a pending secondary lock
after a successfully committed primary reply is lost. These remain failures,
not accepted lifecycle evidence.

After diagnostic04 terminal cleanup, root applied `metadata-followup06`: the
fork directory repair, Original PCR/Recovered PMR full historical hot-lease
authentication and CONTROL hydration, fixed-stage initial/native-clock
diagnostics, and specialized-authentication fault-contract corrections.
Formatting/diff checks passed; exactly eight build inputs changed and the
other 769 frozen inputs remained unchanged. The eight-case failed-path rerun,
`runtime-closeout-diagnostic05`, is running on this frozen source. These
changes were tested by diagnostic05 below.

`runtime-closeout-diagnostic05` has now terminated: **5 passed / 3 failed /
0 unexecuted**. Every phase preserved source/config/Git; owned Redis/PD/TiKV
and proxy cleanup and independent absence checks passed. Redis fork, initial
composer, readback-cancelled recovery and both TiKV postsubmission fault cases
passed. TiKV fork reaches clean publication but returns Busy before Quiesced;
both headless cases finish publication and then reject the first snapshot of
a new borrowed fork at `packed_headless_snapshot_tests.rs:236`. These later
failures remain open. Passing the clock case does not establish why the prior
backend-clock observation failed; its guard remains unchanged.

After terminal cleanup, the twelve-file independent object-credential package
and its absence-probe correction were applied with all predecessor/successor
hashes verified. Operator/runtime object Secrets are now separate in source;
compilation, generated CRD comparison and real server-policy checks are pending.

The same-source `runtime-integrated-diagnostic03` executed all 44 planned real
Redis/TiKV cases: 28 passed / 16 failed / 0 unexecuted. Phase results are:

| Phase | Passed | Failed |
| --- | ---: | ---: |
| Fork and borrowed history | 0 | 2 |
| Initial bootstrap | 2 | 0 |
| Initial composer | 2 | 0 |
| Clean-source negative controls | 4 | 0 |
| Original mounted shutdown | 8 | 10 |
| Headless snapshot | 0 | 2 |
| Headless route controls | 1 | 1 |
| Native partial upload resume | 11 | 1 |

Every phase preserved source/config/Git; owned Redis/PD/TiKV and proxy cleanup
and independent absence checks passed. The Redis BuildingEmpty resume case
failed at the frozen source's after-page authority check. Earlier 12/0 native
results remain historical evidence and do not replace this latest 11/1 result.

The explicit prebuilt-library probe `integrated-auth-reason04` completed
0 passed / 1 failed, preserving source/config/Git and executable bytes. Its
seven capped response observations include three nonempty reason-2
PessimisticRetry WriteConflicts. All three match the original request's key,
primary and six checked timestamp relationships. Their ExecDetailsV2 contains
TimeDetail, ScanDetailV2, WriteDetail and TimeDetailV2; observed nested scalar
tags include TimeDetailV2 field 6 and WriteDetail field 17. The current Get-only
timing validator cannot validate this Lock response shape. This is a failed
test plus diagnostic evidence, not a successful authentication or a complete
fault-injection gate. Log SHA256:
`6ec6ee96b278b23104aec2ba9e79ff3ef8a24f4c99b9db39aa97963ea496bc58`.
Owned service/proxy cleanup and independent absence checks passed.

The earlier reason02 failed probe and the runner-stage03 manifest-schema
preflight failure are retained. Stage03 launched no service or test. Stage04
corrected the manifest consumer contract and bound the actual executable to
all 765 build-reference input hashes; eleven additional CI/SPEC inputs were
frozen separately and were not represented as build provenance.

Root integrated `candidate-root-integrations/metadata-followup05/` with all six
exact predecessor and successor guards passing. The change corrects the
headless operation-owner body length from 203 to its actual 202 bytes, adds
roundtrip/corruption controls, allows exact closed-open deletion only during
Deleting borrowed-alias retirement with idle PWA and Released native leases,
and corrects the later-grant fixture's stale-PCR expectation. Two explicit
TiKV specialized-authentication postsubmission fault tests are also included.
Formatting and diff checks passed. This integration compiled and passed
stores05; its actual backend results are diagnostic04 above. Prepared or
integrated code is not proof that every related lifecycle has passed.

The Lock-only execution-details repair is now integrated. The server's pinned
kvproto schema confirms TimeDetailV2 fields 6/7 are uint64 gRPC timings; the
validator checks all four fixed scalar schemas within the original 128-byte
detail cap. Get classification and request certification are unchanged.
`mounted-followup-sdk03` completed 122 passed / 0 failed / 1 ignored with
unchanged source/Git, including actual-shape and malformed-encoding controls.
Its log SHA256 is
`4b57243b8271c3385fc0472dee5250a49020283f3cd546a16d8f42b82a90fae1`.
Integrated BrewFS stores05 passed; actual Redis/TiKV lifecycle acceptance
remains incomplete as diagnostic04 shows.

Evidence is retained below the external `v3-plans-repair01` root in
`kv-sdk-development/mounted-followup-stores05/`,
`carrier-fork-real-validation01/runtime-integrated-diagnostic04/` and
`operator-local-packed-v3-candidate01/mount-gate-audit01/`.

Redis/TiKV lifecycle acceptance, production CLI/FUSE, enforced runtime/admin
credentials, central leader-elected operator GC and the full repository gate
remain open. Complete these exits before freezing the three-innovation
experiments; all-SPEC completion is not claimed.

## 2026-10-08 testing closeout checkpoint

The current product remains packed-v3 only. Redis and TiKV are the distributed
metadata targets; RustFS is the object server for the owned IAM probe. MinIO
`mc` is used solely as its S3 administration client.

Root integrated the runtime metadata/object authority boundary, one CLI mount
dispatch type, the bounded small-catalog native GC adapter, and the actual
dual-client TiKV TLS test entry point. GC now refuses a missing or uninitialized
CONTROL before mutation or physical deletion. The earlier stores08 run was
361 passed / 8 failed / 102 ignored; its five missing-pin-budget fixtures and
three missing-CONTROL proof failures have been repaired, with rerun pending.
The original failure receipt remains in
`kv-sdk-development/operator-closeout-stores08/`.

The same-source strict check `operator-closeout-style12` passed:
`cargo clippy --locked --workspace --all-targets --features workspace-overlay -- -D warnings`.
Source and Git identity remained unchanged; log SHA256:
`f86a6ecf2a950b1666a954cee57d7729ddc1bd50e9d2446ec74b7ea411bd081d`.
The misleading phase-name prefix does not make this an independent operator
crate check. Earlier style09/style10/style11 failures remain preserved.

Large CONTROL and bounded, recoverable native GC finalization remain an
implementation gap. The current adapter refuses oversized catalogs; it does
not provide eventual collection for them. See the external
`native-gc-control-authority-audit-20261008.md`. Independent operator gates,
generated CRD comparison, complete Rust regression, real Redis/TiKV lifecycle,
production CLI/FUSE, dual TLS, object IAM and Kubernetes lifecycle results must
be recorded separately. This checkpoint does not accept performance numbers
or claim completion of all SPEC requirements.

## RustFS WRITE budget repair and actual mounted verification

RustFS is the actual object server; Redis/TiKV remain metadata authorities.
The MinIO `mc` executable is only an S3 administration client. Packed-v3 remains
the sole packed product format, with no older packed compatibility added.

The first mounted 57-byte WRITE exposed an input-buffer admission error before
the FUSE handler. A default-budget lower bound is already 16,909,432 Roots
bytes: an 8 MiB observer, 128 KiB readonly context, 4,194,896-byte main input
and 4,194,856-byte replacement input, against a 16,777,216-byte capacity.
The old replacement acquisition propagated the error out of dispatch without
replying to that WRITE. Diagnostic04 preserved a pending kernel WRITE and no
handler milestone, then stopped at its 180-second diagnostic deadline; this
missing milestone alone did not identify the exact error.

Root integrated the reviewed `write-input-admission-fallback-formatted02`
and `request-payload-metadata-budget-formatted02` candidates. Replacement
buffer refusal now copies only the actual admitted request bytes, retains the
request permit inside the Bytes owner and reuses the original input buffer.
Copy allocation failure explicitly replies ENOMEM. The pooled success path
is preserved. Request contents now charge Metadata at 2 MiB plus actual bytes,
while Control reserves 8192 bytes for control state. Pool capacities are
unchanged. This also removes the former admission refusal of a maximum 4 MiB
WRITE solely because Control has a 1 MiB limit. Maximum-request admission is
tested; a real 4 MiB kernel write and matched performance are separate gates.

Completed checks on this repaired source, with unchanged source/Git:

| Receipt under external `kv-sdk-development/` | Result |
| --- | --- |
| `operator-closeout-style18` | Workspace all-target overlay Clippy passed |
| `operator-closeout-stores15` | 422 passed, 0 failed, 103 ignored |
| `vendor-write-fallback-focused01` | 1 passed; copied payload survives input reuse and retains its final owner |
| `operator-closeout-request-payload01` | 1 passed; both adapters admit 4 MiB plus header and reject exhaustion/overflow atomically |
| `operator-closeout-control-stats01` | 3 passed |

`rustfs-native-kernel-read-diagnostic05` then passed the exact original TiKV
mounted reader-error case naturally: 1 passed, 0 failed, 44.66 seconds. Its
log records `[packed-v3-input-diag] bytes=4194856 errno=12`, followed by the
complete successful WRITE handler sequence. Original native and packed kernel
readback, unmount and reader-error pin/cleanup assertions all passed. These
observations establish the allocation refusal and successful fallback on the
actual TiKV + RustFS path. Log SHA256:
`95869c7d6ce8ed18e40c698639fd2d6ad6a84553cc8cdf3f5dfbdbb31989ee1d`.
All 797 stores15 inputs match this runtime receipt. Owned services and private
credentials retired, with no forced FUSE cleanup or residual native process.

Diagnostic04 remains failed (exit 143, zero passed). Its exact connection was
aborted; ordinary unmount failed without retained stderr. Root subsequently
verified mount ID 566, device 0:99 and exact mountinfo, unmounted that private
mount with sudo, and saved independent complete retirement evidence. The old
failed aggregate was not rewritten. Both diagnostics and supplemental receipts
are preserved under the external `v3-plans-repair01` root.

The complete RustFS lifecycle run03 passed as one fresh run: 22 passed,
0 failed, with phases of 18 + 2 + 2 and exactly 22 RustFS object-backend
markers. All phases retained unchanged source inputs. The aggregate records
RustFS removal and independent absence, metadata-service absence and private
credential destruction. The original-shutdown, headless-snapshot and
headless-route logs have SHA256 values respectively:
`45b9469c14d1d11e5d9094e5b0106a7cedc664909110afbc562971a42d5178fa`,
`6235de647ef993b965723ff0568515b269d8fc440b9560385b979ce84ae2d78f`,
`749f85ef28914dc3b6917c28c6a8a6c1627b5cbd8976dbb1764b779c685208c6`.
Evidence is preserved under external `rustfs-metadata-lifecycle-run03/`.

The 25-case strict metadata validation10 batch finished on the same frozen
source with 21 passed, 1 failed and 3 unexecuted. Its LocalFS/test object hosts
are separate from the actual RustFS run; the two batches must not be presented
as 47 RustFS cases. Source/config/Git remained unchanged and owned metadata
services/proxy were independently confirmed absent after cleanup.

The exact failure is `real_redis_native_resume_building_empty`: the recovery
authority rejected a journal 957,649,000 ns ahead of the backend clock. All
other reported authority predicates passed; `journal_not_future=false`
correctly fenced continuation. The 12-case resume phase was 11 passed / 1
failed, log SHA256
`6c60046e6f8094b6d9e486a3b4a9d1e8763e1627367f44a7dcec6a103499704a`.
The failure receipt is preserved under
`carrier-fork-real-validation01/runtime-closeout-validation10/`.

WSL logs around this batch show repeated clock-change notifications about
every 31 seconds and a backwards-time journal rotation. The timesync service
reported a roughly -1.6-second offset at a 32-second poll interval. This is
evidence of an unstable test clock, not yet proof of the exact individual
957-ms regression. Do not relax the future-journal check or rewrite this failed
receipt. A fresh run must record realtime/monotonic clock behavior and restore
any temporary test-environment changes before acceptance.

Two new 75-second clock probes now measure the environmental failure directly.
`clock-active-probe01` recorded three negative realtime deltas of approximately
1.63–1.65 seconds, while the monotonic clock advanced. Its three realtime minus
monotonic adjustments were approximately -1.69 seconds. In
`clock-paused-probe01`, temporarily stopping systemd-timesyncd produced no
negative delta or adjustment above 50 ms in 1467 samples. An independent
three-minute systemd restoration timer was installed before the pause; the
probe restored the original active service and retired that timer successfully.
The raw sample streams and before-state diagnostics are preserved externally.
This isolates a repeatable NTP/environment clock issue in the current WSL
session. It does not retrospectively authenticate the failed test's exact
timestamp or replace a fresh successful metadata batch.
Full same-source CI, production CLI on RustFS, dual TLS,
Kubernetes lifecycle, large reverse-path authority, large CONTROL/scoped GC
and publication-versus-delete fencing remain separate completion exits.
No performance or all-SPEC acceptance is claimed.

## Native publication/delete closeout integration

The actual-catalog race regression first ran on the predecessor source and
failed as intended: a second store published existing SID 1 into a live head
after the collector retained its upper bound, then the real WorkspaceGc path
deleted block `(1, 0)`. The live publication remained in metadata. This is an
executed data-loss race, not a timeout or a simulated alternative collector.
`kv-sdk-development/sid-publication-race-red01/` records 0 passed / 1 failed,
unchanged source/Git and log SHA256
`740bb6f9275c555e67f08845e4da6d224cb474af44e16fdd77b37d730ec28aaf`.

Root then integrated the reviewed reverse index and missing direct birth hooks,
native old/new journal-head ownership and Deleting ancestor fences, common
extent-generation/SID reservation preparation, stable per-target incarnation
recovery, and bounded durable small-catalog metadata finalization. The explicit
authenticated maintenance CLI can build/resume reverse indexes or initialize a
missing Deleting GC identity with `--initialize-gc-incarnation`; this action
creates Building identity only, and never claims reverse readiness. Runtime
reservation and metadata finalization refuse a missing identity. SID fences
survive metadata cleanup. Current inventory participates in every resumed CAS;
historical inventory remains evidence while exact target/build bytes reject
target reincarnation. The production packed mutation envelope remains 32768
items / 8 MiB; the bounded GC packet remains 256 items / 256 KiB.

The RustFS production CLI test adapter is also integrated for its six existing
Redis/TiKV cases, including actual bucket IAM PUT denial and restoration. Those
cases have not run on this integration. No new packed product format or old
packed compatibility is introduced.

The first integrated stores gate stopped at a test-fixture E0502 borrow error,
with zero tests executed. Root corrected it. Strict Clippy style01/style02
subsequently found three style errors, which were corrected without weakening
checks. `native-authority-style03` then passed workspace/all-target Clippy with
workspace-overlay, unchanged full compiler inputs/Git, log SHA256
`fab45d930993ca92b227361775647da5a1566c7bc654553ddc9755a7b9422349`.
The original failing receipts are preserved. `native-authority-binary01` passed
on that integration, with executable SHA256
`e077369700f2085f7db0bdd4a9f0bd44a754fa286de202202335c3c6c7f1e68e`.
Later test-source edits require a fresh build receipt for the strict CLI gate.

`native-authority-stores02` then executed 444 passed / 29 failed / 105 ignored,
with unchanged source/Git. The publication/delete race passed, while stale
fixtures failed the newly required migration marker, sealed-source ancestry,
retained block-range capability and exact CAS keyset contracts. Its log SHA256
is `90c7a8a87c018593180bfe59a096db1aaac4a8eb517aa738cf1e41e618096071`.
Root corrected these fixtures, preserving production refusal and authorization
checks. The retained-range wrapper is test-only and persists across collector
instances for the fixture lifetime; it does not claim process persistence.

The fresh `native-authority-stores03` passed **473 / 0 / 111 ignored** on the
corrected source, including all previous 29 failures and the actual-catalog
publication/delete race. Source/Git remained unchanged; log SHA256
`8ee073ed64427fff78eaf7f38a076632ed9084aeb6cb1c8d4de505813b6c52c0`.
Strict workspace/all-target overlay Clippy `native-authority-style05` passed,
log SHA256 `252cbb6e9e4e65122cc5ff619bd30fe17b35d3f1be1340d1e4fae973d13e6277`.
The three maintenance CLI unit tests passed in `native-authority-maintenance01`,
log SHA256 `7653055aeb5d2bbb1902fabffe7bb9b35debc512392d44c1fdf2576ae743a251`.
These three gates share unchanged compiler inputs. Style04's new-test helper
name error and its failed receipt remain preserved.

Six ignored Redis/TiKV + actual RustFS SID tests are now integrated: publication
wins, reservation wins with an unknown response after actual successful DELETE,
and 65-block recovery across three fresh collector ticks for each backend.
They check structured IAM denial, exact object presence/absence, independent
metadata clients, no deletion replay, unrelated layer birth/death and stable
target identity. Compilation has passed; real execution remains pending.
They test collector tick recovery, not metadata service/process restart. The
earlier RustFS22 run03 remains historical evidence until fresh runtime validation.

The first actual SID attempt `sid-real-rustfs-run01` failed in its first phase:
the exact selector omitted the real `admin_gc::facade` module. Cargo exited
successfully with **zero tests**; the harness correctly rejected the missing
real evidence (exit 96). No SID behavior pass is claimed. Source/config/Git and
toolchain stayed unchanged; owned metadata/RustFS independent absence and
credential destruction passed. The clock wrapper recorded no regression or
large adjustment and restored active NTP, retiring its independent restore timer.
The failed phase log SHA256 is
`f0f8e2005eed2a50f6024a8357990a7525c57e1bff43b82c69f5653ff6e2bb76`.
Root obtained all six exact names directly from the compiled test executable,
SHA256 `010e25ac5bdfb044af8668ece64f185d7abb8027fff9293b10b5f3c7e4b85459`.
The external execution tool is being corrected without changing Rust source.

This integration still uses the explicitly bounded small-catalog collector.
It does not finish large CONTROL/scoped authority or its segmented-source
migration, complete durable large root/shared-extent proofs, Kubernetes, full
same-source CI or performance experiments. Real Redis/TiKV + RustFS race and
recovery, CLI six-case lifecycle and dual TLS transport gates remain separate.
