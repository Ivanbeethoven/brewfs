# Packed v3 physical dependency validation — 2026-10-07

Scope is v3/005 only. The physical-dependency audit primitive passed the
complete fixed-source local gate and an isolated real RustFS check. Full G12
publication acceptance remains open. `VerifiedPackedLower` still authenticates the
manifest/maximum-inode route; this new result cannot construct that authority.

## Implemented primitive

- Bounded typed object metadata: unsupported backends refuse without a whole
  GET; LocalFS uses metadata and S3 uses observed SDK HEAD. Missing, invalid
  length and authorization failures remain distinct. HEAD transfers zero
  payload bytes and conserves HTTP/typed success, failure and cancellation.
- Shared buffered/streaming envelope rules; GC05/LD05 authentication uses
  sequential ranges at most 1 MiB, complete object/body SHA, exact physical
  length checks before/after reading, and owned bounded summaries. An unchanged
  ref with a corrupted payload tail fails whole-object authentication first.
- A private SQLite inventory binds key/kind/u64 length/full digest. Same-key
  conflicts fail; identical digests under different keys are separately
  authenticated. Indexed single-row traversal avoids a whole-graph collection.
  Database page quota, object/byte/edge/leaf/readback quotas and owned memory
  reservations bound the operation. SQLite cache size is a target; complete
  native RSS has not been measured as a hard bound.
- Physical traversal covers all seven manifest roots, source allocations,
  every IP06 child, Container/Frame/Cold object refs and PS09 External LE09
  roots/children. Metadata pages and payloads require bounded size capability.
- Successful return waits for real SQLite shutdown, then rechecks cancellation
  and shared-budget closure. Cancelled verification does not close the shared
  budget. Drop retains directory/cache owners through independent cleanup.
  Only the exclusively created private directory is removed.
- An interrupted SQLx query keeps its 32 KiB Metadata owner with the connection
  until shutdown acknowledgment. Dropping a caller future prevents subsequent
  queries from replacing the pending owner. Completed queries release or
  transfer ownership; recoverable identity/quota errors remain usable.

## Actual development evidence

Artifacts: `C:\Codex-Recovery\brewfs-root-20261005\v3-plans-repair01\g12-graph-validation-20261007`.
Only root runs Cargo, using toolchain 1.98.1, one job, incremental off and
dev/test debug off, with the existing target directory.

- Bounded HEAD API compile RED exited 1 with 11 missing-method errors. After
  implementation, `bounded_` exited 0: 35 passed, 1 ignored, including all five
  new adapter tests. This is an API-development RED, not a behavioral regression
  reproduced through a previously existing method.
- Inventory API compile RED exited 1: 13 missing-inventory API errors and one
  independent missing payload generic bound, subsequently corrected. The
  tests-only source was saved separately. Lifecycle tests were strengthened
  during implementation; whole-file RED/GREEN byte identity is not claimed.
- First combined development run exited 1: 33 passed, 2 failed. The inline
  fixture lacked its required inline flag; corrupted GC tail authentication
  reached footer schema rejection before full ref SHA comparison. Both were
  corrected without weakening the final hash-mismatch assertion.
- Independent review identified cancellation during the final close await.
  The controlled close-acknowledgment test uses a real producer graph result;
  its RED exited 1 with the explicit escaped-success assertion. The fix checks
  liveness after cleanup and before returning a successful result.
- Corrected combined run exited 0: 39 passed, 0 failed, 0 ignored. New coverage
  includes 13 inventory, 10 payload and 11 physical audit tests, plus five
  existing publication tests. Raw/Zstd, inline/all-hole/External, off-maximum
  child absence, same-key identity conflict, actual quota exhaustion, suffix,
  equal-length tail corruption and termination/release controls passed.
- Final tests compilation, including the explicitly ignored real S3 test,
  exited 0.
- Gate13 actually ended with exit 1: 48/49 commands passed, all 564 frozen
  source/configuration inputs unchanged. Only strict overlay Clippy failed,
  on an implicitly dropped cleanup receiver and a redundant return binding.
  Both have been corrected. Default library 1,169, overlay library 1,827 plus
  eight fixture tests, all-features library 1,842 and operator 28 passed.
  These results do not accept the subsequently discovered query-owner defect.
- Independent SQLx review identified a query ownership gap after foreground
  cancellation or future drop. Three real progress-handler barrier RED tests
  actually failed: 0 passed, 3 failed, with sources unchanged. Each stopped
  SQLite while it still held a 4 KiB bound argument; Metadata fell to its
  24 KiB baseline instead of retaining the additional 32 KiB query owner.
  Barrier release, real connection close and zero-owner/private-directory
  cleanup happened before each failing assertion.
- The query owner now resides in InventoryStorage and moves into Cleanup.
  The corrected publication-module GREEN actually passed 37 tests, 0 failed,
  1 ignored, against unchanged sources. It includes all three interruption
  cases and the caller-drop immediate-rejection/unchanged-charge assertions.
  RED and pre-format GREEN test modules are byte-identical. This filter does
  not include the five producer publication controls in the earlier 39-test run.
- Gate14 actually ended with exit 1: 48/49 commands passed and all 564 frozen
  source/configuration inputs and Git identity stayed unchanged. All behavior
  suites passed; strict overlay Clippy alone rejected two late-initialized
  booleans in the new caller-drop test. The test now returns these booleans as
  an `if` expression tuple, preserving the query, observation and cleanup order.
  The same complete 49-command gate15 then passed after final formatting.
- Independent review confirmed the gate14-to-gate15 production prefix is
  byte-identical and the caller-drop observations, timeout and assertions are
  unchanged. The live runner's six in-memory cleanup fault scenarios passed,
  including diagnostic ENOSPC, diagnostic-timeout ENOSPC, process survivors,
  removal errors and absence-check errors. These are runner-control tests,
  not themselves evidence of a live S3 or Rust gate result.
  Receipt: `gate14-style-diff-runner-gate15-readonly-20261007-065807.json`.
- Gate15 actually ended exit 0 in session 5923, terminal chunk `2dbd49`:
  49/49 commands passed with 564 frozen source files, configuration and Git
  identity unchanged. Stores passed 108/12 ignored, default library
  1,169/225 ignored, overlay library 1,830/241 ignored plus eight fixture tests,
  all-features library 1,845/241 ignored, and operator 28/0 ignored. All other
  all-features integration/doc tests, strict Clippy configurations, build,
  runtime checks and CRD generation/diff passed. Independent review confirmed
  frozen copies/current source set, configuration, Cargo environment and
  each of the 49 unique status commands match the plan.
- The owned real RustFS run actually ended exit 0, terminal chunk `2a8a87`:
  one explicitly ignored S3 test was executed and passed against the same
  source/configuration/Git inventory. Its 12-object Zstd/source/cold graph
  authenticated 55,228 bytes, using 48 HTTP attempts and 24 range requests,
  with maximum range 4,096 bytes. The two HEADs per object transferred zero
  payload; HTTP/backend/validated-fetch ledger totals conserved actual bytes.
  Remote and local inventory digests/counts matched. Missing cold and appended
  GC objects were rejected, each private scratch and memory owner was released,
  and all owned objects and the bucket were removed. The runner separately
  confirmed its container absent, process groups empty and logs redacted.

## Fixed-source evidence

Gate: `full-rust-gate15-sql-query-owner-style-20261007/` below the parent
`v3-plans-repair01` artifact directory. Its canonical manifest SHA256 is
`93a8daae4e74966073c82f33db06315d2726fb6b74a7604c4613bfac74434f53`;
verification SHA256 is
`650ceddd700038a048a9119e43ccb292ee57e56faf574de14de145b499f3f1c2`.
Live evidence is `g12-graph-validation-20261007/real-s3/`; verification SHA256
is `1e1054ff33e6b8e2c5b48ecc6f9eba58b129fc217f5b5e83798db3d400252429`,
cleanup SHA256 is
`64d15c51f83a2c3391037c5132be9fcd3af67a7058fee487c08175db52c8d274`.
Gate13/gate14 failures and query-owner REDs remain preserved separately.
Independent final review:
`g12-graph-validation-20261007/gate15-real-s3-final-readonly-20261007-073838.json`,
SHA256 `ed1ad76c8925893d7147675c1e19cf7e93d639053071f38a152e2bd6ae4bb50c`.
It separately checked the owned UUID/name and full container ID are absent,
the live test source/environment matches the complete gate, and all recorded
counts, commands and referenced hashes agree.

## Remaining SPEC scope

Full G12 still requires parent height/fence/weight checks in every context,
GroupMeta/namespace/cold/source joins, FD coverage and frame codec validation,
External EOF/count/raw/logical-digest closure and a mandatory opaque complete
proof at all public install/publication routes. Physical authentication is not
a durable candidate pin or an atomic source-view proof. Durable journal/reopen,
atomic seal/head/binding rotation, reader/history retention and packed graph GC
remain open, as do the relevant real lifecycle, crash/recovery and S/X exits.
The external `semantic-parent-context-red01/` candidate and
`semantic-facts-parent-context-design01/` schema/API design prepare the next
batch; neither has been applied as Rust code or dynamically validated as a
full graph proof. The design uses an explicitly quota-accounted second pass,
per-incoming-edge/context checks and indexed semantic facts, retaining query
owners until actual shutdown. It cannot authorize catalog writes yet.
Remote object immutability remains required; two HEADs cannot make arbitrary
backend replacements atomic. The formal performance campaign has not started.
