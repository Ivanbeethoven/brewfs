# Workspace Overlay Architecture

Status: **stage 2 experiment; production mount remains disabled** (2026-10-20).
This document and the [implementation spec](../superpowers/specs/2026-08-23-brewfs-workspace-overlay-implementation-spec.md)
now specify an ordinary per-entity writable namespace over one immutable packed
lower. They supersede the previous mutation/version architecture as the target,
not as a claim about the current binary.

The shipped code remains r31, `0d4e36c` / PR #41; `6b54270` delivered its
documentation on `codex/packed-metadata-aliyun-20260930`.
Its actual design, validation gaps and reproduction commands remain in the
[r31 delivery handoff](../superpowers/plans/2026-10-17-overlay-no-seal-handoff.md).
The unvalidated r32 candidate has been dropped in favor of this direction.
The isolated `workspace-entity-v1` implementation lives on
`codex/overlay-entity-namespace-20261019`, branched from `b6f297e`.
The integration branch remains unchanged. See the
[stage 2 checkpoint](../superpowers/plans/2026-10-19-overlay-entity-stage2.md)
and [file-I/O continuation](../superpowers/plans/2026-10-20-overlay-entity-data.md)
for implemented APIs, exact gates and remaining admission requirements.
There is no conversion, production mount or replacement performance result.

## Contract and boundaries

A workspace has one immutable packed lower and one writable upper namespace.
Missing upper entries fall through to the lower; explicit tombstones suppress
that fallback. Metadata changes and new data slices belong to the upper.
Creating a workspace pins its lower and creates an empty namespace; it does
not copy the lower's metadata or data.

The experimental packed adapter authenticates the manifest key/digest, root
inode and maximum inode before exposing metadata or payloads. First attach
promotes a live packed reader pin into a durable owner for the volume and
namespace; reopening reconstructs that owner from persistent metadata. These
owners participate in the existing packed/native collector census and its root
generation CAS. They survive reader-lease expiry and are released only with a
terminal deletion proof after upper cleanup. The current bridge admits at most
256 active owners per store and retains terminal owner tombstones; creating a
new owner still requires a live source reader pin.

The experimental SDK uses trusted adapter composition. Callers must pair the
packed owner with the same volume, namespace, lower binding and authoritative
metadata backends used by the entity lifecycle. A production factory enforcing
that pairing and reader quiescence remains an admission requirement.

Keep sole-writer lease safety, atomic namespace operations, crash consistency,
correct object reclamation and fsync durability. Preserve ordinary permissions,
ACLs, hardlinks, sparse-file behavior and O_EXCL. Those filesystem semantics
do not require a workspace-wide mutation order.

The target has no writable head/layer record, per-layer sequence allocator,
expected-head CAS, row sequence stamps, append-only extent history, or mutable
workspace digest. There is no live-layer seal, overlay-stack flattening or
strict-lower mode. Publishing an immutable packed export remains a separate
administrative operation. Packed manifests, payload checksums and immutable
object authentication remain; removing a workspace digest does not remove
data integrity checks.

Default flat volumes retain their existing path. A new versioned namespace
format and explicit dispatch separate this target from existing workspace-v1
records. Old binaries must reject the new format. The format identifier and
new APIs are experimental. Selecting `workspace-entity-v1` explicitly reaches a
fail-closed mount dispatch; it never falls back to the old workspace path.

## Audit of the current design

The current overlay is not uniformly an append-only log: dentry, inode, xattr
and ACL delta keys already hold their current value. Extent keys include a
sequence and accumulate overlapping records. Every nonempty row mutation advances the
same head sequence, which provides a coarse version for the entire workspace.

| Current component | Classification | Target disposition |
| --- | --- | --- |
| Authenticated immutable lower and fallthrough reads | Required contract | Keep the packed reader and a durable lower pin |
| Dentry/inode/xattr whiteouts | Required contract | Keep tombstones; absence must remain distinct |
| Lease, sole-writer authority, backend-time expiry | Required contract | Check in every metadata-changing transaction |
| Atomic create/rename/link/unlink and permission decisions | Required contract | Transactions over affected entities; O_EXCL absence check inside create |
| Upload-before-publication and fsync completion | Required contract | Keep; independent of workspace ordering |
| Reachability protection, orphan recovery and deletion reservation | Required contract | Replace census fencing with object ownership and safe retirement |
| Head record, next_sequence, expected_layers and global mutation CAS | Artifact of chosen version model | Remove from the runtime format |
| Sequence-stamped extent keys and newest-first overlay folding | Artifact of extent history | Replace with current normalized chunk maps |
| Layer delta digest/root hash for mutable-overlay identity | Artifact of revision model | Remove; immutable packed integrity remains |
| Per-layer extent generation and whole-layer census fencing | Artifact of current GC proof | Remove only after per-object deletion fencing is implemented |
| Global mutation mutex | Artifact of coarse version/cache coordination | Replace with affected-inode/parent coordination and backend atomicity |
| Commit-view cache and cached head/derived-counter bytes | Optimization of that model | Drop |
| Data group commit | Optional optimization | Drop from the initial target; reconsider only with measurements |
| Lease-bound row/path/permission caches and created-path seeding | Optional optimization with coherence obligations | Not required for the initial target; add only if measured and correct |
| Packed-lower memos, payload caching and prefetch | Optional performance policy | Existing immutable-reader facilities may remain; not mutation authority |
| Per-inode data version / directory revision | Local coherence mechanism | Keep only where reads, caches or directory cookies need it |
| Publication/discard recovery intent | Required crash protocol when work spans transactions | Keep bounded operation state; no general mutation replay log |

The shared head was a convenient replacement for precise read sets: a changed
head forced inode-copy, permission and namespace decisions to be recalculated.
It also supplied ordering for overlapping extent records, read-plan fences,
accounting and administrative empty-head checks. It was not required by the
packed file format, lower fallback, leases or fsync.

Diff/export used delta rows, sequence-aware resolution and canonical layer
digests. The target obtains the current effective state by walking upper keys
and merging with the pinned lower. Sequence-based comparison/identity is removed;
reading and exporting remain. Current scalar rows already overwrite history,
so r31 does not provide complete historical transaction replay either.

## Proposed data model

Use workspace prefixes in one volume metadata transaction domain, for example
`{volume}/workspace/<id>/...`. Prefer prefixes over separate Redis databases:
chunk references and shared-object ownership must participate in the same
transaction. Redis Cluster keys involved in such transactions share a hash slot.
This provides independent client operations, not parallel execution of Lua on
one Redis server. TiKV support requires equivalent transactions and separate
qualification; it is not assumed from Redis results.

| Entity | Current value |
| --- | --- |
| Workspace descriptor | Format, namespace identity, immutable lower manifest, lifecycle state and lease identity; changed only by administration |
| Lease | Sole writer, holder generation, state and backend deadline |
| Inode | Present/deleted, attributes, nlink, size, xattrs/ACLs, lower visibility boundary and any needed local data version |
| Dentry `(parent, name)` | Inode/type or tombstone; no upper key means consult lower |
| Xattr/ACL `(inode, name)` | Value or tombstone, so removal masks a lower value |
| Chunk `(inode, chunk_index)` | Current complete, disjoint spatial map of upper Data slices, immutable LowerRange references and Holes |
| Slice/object ownership | Pending-upload owner, upper reference count/edges, immutable-publication ownership, read protection and deletion state |
| Allocators | Plain INCR/INCRBY uniqueness counters, unrelated to mutation order |
| Recovery intent | In-progress truncate, publication or discard with an idempotent cleanup cursor |

The stage 2 schema uses `{brewfs-entity:<volume UUID>}/workspace/<namespace UUID>/`
for inode, dentry, xattr, chunk, orphan and lifecycle rows. Raw names are
hex-encoded injectively; slice records and the slice allocator use the shared
volume prefix. Rows use bounded typed JSON; allocator values are canonical
decimal integers. The experiment is not yet a production wire-format promise.
There is no per-commit workspace counter. A shared descriptor/lease may be read
and checked by every operation without being rewritten by unrelated file mutations.

Inode allocation starts above imported lower IDs. Object/slice IDs are unique
across sharing namespaces, never reused after deletion, restore or failover.
Redis's signed-64-bit range is explicit; counters fail on exhaustion rather than
rounding through Lua doubles. Allocation gaps are harmless.

### Reads, copy-up and tombstones

Lookup first checks the upper dentry. A tombstone returns absence; a missing key
consults the packed lower. Inodes and xattrs follow the same rule. Hardlinks in
either namespace resolve the same effective inode, so a copied-up inode changes
all its aliases. Readdir merges names and removes whiteouts, with per-directory
cookie/version semantics rather than a workspace generation.

A missing chunk inherits the immutable file only below the inode's
`lower_visible_size` (initially lower EOF, or zero for a new file); other bytes
are holes or beyond EOF. On first modification,
materialize that chunk's mapping metadata, then replace the written range with
new slices. A present chunk map covers the entire chunk domain: gaps are explicit holes, never an
implicit invitation to reveal lower bytes. LowerRange entries retain the
workspace's immutable lower identity. Spatial interval order is not mutation
history; overwritten intervals are removed from the current map.

Same-inode writes serialize their map/size changes, preserving overlap and
append semantics. Unrelated inodes need no common ordering. Multi-chunk writes
retain the filesystem's existing partial-write semantics; no new file-wide
atomic-write guarantee is implied.

### Namespace transactions and open/close

Create reserves a unique inode ID and atomically checks parent permissions and
effective name absence while installing inode/dentry/parent changes before
returning. The reference implementation reserves the ID with a separate INCR;
failed attempts may consume IDs but cannot expose partial namespace changes.
The immutable lower lookup may be prepared outside Lua, but its binding must be
unchanged and the final transaction must recheck upper absence and mutable policy inputs.
Never merge create visibility with the first data fsync.

Rename, link and unlink transact over the affected parents, names, inodes and
ACL inputs. Same-entity local locks are acquired in a fixed order; stale
client-prepared decisions use per-entity validation and full recalculation.
Redis Lua can instead calculate from current entity rows inside the transaction.
Neither approach requires a shared-head CAS. Every mutating transaction checks
sole-writer authority, lifecycle admission and backend-time lease expiry.
Checks include mutable ancestor search-permission inputs used by the operation.
Lua errors do not roll back earlier writes: types, bounds, overflow, refcount
underflow and key collisions must be validated before the first entity write.

Ordinary open checks current permissions/lease with zero metadata writes and
retains a local handle. Linked close releases that handle locally, including
after lease expiry. O_TRUNC still performs a data mutation. Unlink of an open
inode records a recoverable orphan; closing its final handle allows proven
quiescent recovery to complete retirement. The initial target uses explicit
noatime semantics; adding a different timestamp policy is a separate decision.

### Truncate, object lifetime and fsync

Shrinking must hide both lower bytes and old upper chunks permanently across
later extension. Decrease `lower_visible_size` monotonically. For cleanup that
does not fit one transaction, set the new size/mask and a durable Truncating
intent atomically, hold same-inode mutation admission, prune the boundary and
later chunk mappings with their reference changes in bounded transactions,
then clear the intent. Boundary bytes beyond EOF become explicit holes. Advance
each cleanup cursor atomically with its map/reference changes, so a crash cannot
skip work or apply a decrement twice. Drain earlier buffered writes/uploads or
fence their publications with a per-inode truncate generation; they cannot land
after pruning and resurrect data. Block writes/re-extension until cleanup finishes,
including after restart. An interrupted shrink cannot expose old data merely
because its cleanup has not completed. Hole punching needs the same atomic
mapping/lifetime discipline.

Publish a chunk replacement and its slice-reference deltas in one transaction.
Count distinct current chunk-to-slice memberships, not file names or nlink.
LowerRange references are protected by the immutable lower pin; zero upper
references never authorizes deleting lower-owned data. Fork takes one manifest
pin, not an eager increment on every lower slice.

Fresh upper slices initially belong to one workspace; direct sharing between
live uppers is outside the initial target. Sharing through immutable publication
and lower pins remains supported. Publication that reuses upper objects establishes durable immutable ownership
before upper ownership is removed. Staged publication keeps its source protected
until this transfer is complete.

The initial implementation defers physical reclamation of retired upper slices
until proven namespace quiescence or fenced discard. Stop new I/O admission,
drain writes and active read plans (including handles retaining object pins),
invalidate obsolete plans and exclude live/recoverable reader sessions before
deletion. Refcounts and retirement queues update during normal operation, but
zero references alone do not delete an object a reader may still hold. This
avoids new per-read distributed pin traffic at the cost of retaining dead bytes
until maintenance. A future online collector would need an explicit read-pin
protocol; elapsed grace time alone is insufficient for stalled reads.

Reclaim an upper object only after atomically reserving Live-to-Deleting with
zero references, proven quiescence and no upload or immutable-publication protection.
Reference birth checks the same object state and cannot resurrect Deleting.
The durable reservation precedes object DELETE; retries are idempotent.
Pending uploads have durable owners and are reaped only when their authority
can no longer publish. Namespace sweeps enumerate ownership and reconcile leaks;
they do not infer safe deletion from an unfenced SCAN.
Collectors and crash recovery use explicitly fenced lifecycle authority, not
an expired runtime writer lease.

Complete durable object uploads before publishing their metadata references.
File fsync waits for that file's preceding uploads and metadata publications;
directory sync covers its documented namespace boundary. Both honor the
configured metadata/object-store persistence policy. A successful Redis reply
alone is not a universal power-loss guarantee. Lost commit replies require
reconciliation or a proven idempotent operation, never blind replay.

Strict entity uploads use the production S3 adapter's conditional-create
capability. The new packed-lower, upper-data and VFS object-I/O tests use an
isolated RustFS service through the shared `entity/test_objects.rs` helper.
Memory backends remain suitable for metadata transactions and fault injection;
temporary local files hold producer metadata spools only. These service tests
are ignored in the default suite and must be selected with `--include-ignored`;
missing explicit service configuration fails the test. The real-service gate
passes six packed-lower, 78 entity and two VFS tests; exact evidence and remaining
qualification gaps are in the file-I/O continuation.
Metadata recovery oracles and actual object-I/O tests have separate coverage;
they do not yet qualify a complete Redis entity + real packed lower + VFS
stack, physical packed-graph retirement, or recovery after process/host failure.

## Export, diff and lifecycle

Pin one immutable lower when creating a workspace. Discard first fences the
writer, then walks upper ownership with resumable idempotent cleanup, releases
the lower pin last and retires the namespace. Reader admission and outstanding
read plans must also be drained/fenced before their protection is removed.
Never FLUSHDB a shared backend.

A consistent dump/export stops mutation admission, drains writes/fsync and
walks a stable namespace while retaining its lease and lower pin. Redis SCAN
over a live changing namespace is not a consistent snapshot. This administrative
pause is not a lock on every ordinary mutation. Build and authenticate the new
packed lower before publishing it; never seal or rotate a live upper.

Diff becomes a semantic comparison of effective namespaces/content. An
upper-only walk against the same fixed lower can still identify changed
entities, but a cheap arbitrary-workspace/history diff and total-order replay
are not provided. Export can sort records for reproducible output if desired;
a canonical writable-layer digest is no longer an identity contract.

## Migration from the current branch

1. **Design and format boundary.** Review this proposal; name a new format and
   error/compatibility contract. Preserve r31 binaries, schema and benchmark
   artifacts. R32's partial implementation stays archived, not deployed.
2. **Isolated implementation.** Build current-entity storage/resolution and
   atomic namespace operations behind explicit new-format dispatch. Establish
   lower pins, pending uploads, object ownership, deletion reservations and
   orphan recovery before admitting writes. Keep old format operational.
   The experiment includes a bounded namespace/Redis substrate, an authenticated
   packed adapter and durable lower owners in the existing collector census.
   Its file-I/O continuation adds write-through current maps, an explicit VFS
   handle dispatch and bounded truncate/discard/orphan recovery. Production
   mount admission stays closed: external crash-takeover authority, paginated
   production cleanup and full mounted qualification remain outstanding.
   Operations require their namespace lower owner and active writer authority.
3. **Correctness before conversion.** Validate O_EXCL, rename/hardlinks,
   permissions, same-inode overlap, shrink/extend, fsync, cancellation, lease
   expiry and GC/reference races with crash injection. Qualify every supported
   backend; do not inherit r31 tests as proof of a new model.
4. **Offline conversion.** Refuse live mounts; obtain migration authority,
   drain the old writer and hold its roots. Walk the effective old view, use
   old sequences only in the importer to resolve extents, and write normalized
   current rows into a fresh namespace. Seed object ownership before making it
   visible. Verify names, attributes, ACLs, holes, link identity and data bytes.
5. **Atomic cutover and recovery.** Switch the descriptor/format once, after
   conversion validation, with a durable migration intent. A crash chooses
   either complete old or complete new state. Keep old roots until acceptance;
   after new writes, rollback requires a reverse export, not rewinding a pointer.
6. **Qualification and retirement.** Run the full local gate, real-mount/POSIX
   and GC/crash tests, then matched 16/32-worker order-swapped native/r31/new
   comparisons including readback/mixed and drain. Only then remove old runtime
   log/head/cache machinery and retire old ownership safely.

## Evidence, decision and trade-offs

The r31 stage-only diagnostic measured 91.2% gate occupancy at 32 workers,
3.703 ms of held service per file: create 60.2%, open+close 29.9%, data 9.9%.
At 16 workers occupancy was 82.0%, with 3.785 ms/file. These are one-repetition
attribution runs, not acceptance results for this proposal.

Completed r31 acceptance means (six samples per arm at 16/32 workers):

| Workers | Overlay WRITE files/s | Native WRITE files/s | Overlay/native |
| --- | ---: | ---: | ---: |
| 16 | 216.89 | 344.01 | 0.63 |
| 32 | 236.89 | 414.74 | 0.57 |

Completed r31 ratios for stat/full/write/readback/mixed are respectively
1.00/0.71/0.86/1.06/1.02 at one worker,
—/—/0.63/0.91/0.85 at 16, and —/—/0.57/0.88/0.87 at 32.
Missing stat/full results are not filled from the interrupted final sweep.

Native CREATE_ENTRY_LUA executes one atomic script without a client mount-wide
mutex; the server still serializes Lua and updates a shared allocator/parent.
Native open/close bookkeeping does not commit metadata. The r31 measured
CREATE/record_open/record_close path also did not update atime: the separate
legacy MetaLayer::open does. Atime is not an explanation of the measured gap.

**Drop r32.** Its backend-assigned global sequences and per-key checks could
reduce client serialization, but preserve the very sequence/head model being
retired. Neither it nor the prefetch-only precursor has a candidate performance
comparison. The prefetch gate was interrupted; the later atomic-sequence draft
was never built or validated. This is a design supersession, not a measured
performance rejection. Draft sources and evidence are preserved in the
[r32 investigation](../superpowers/plans/2026-10-19-overlay-write-r32.md).

Trade-offs are explicit: more expensive diff/export and an export pause;
no total-order replay or mutable-layer digest; current chunk-map rewriting and
possible fragmentation costs; per-object reference bookkeeping and crash
recovery; retired upper-byte retention until maintenance/discard and lower-object
retention until the last immutable pin;
and a format migration with no instant rollback after new writes. Hot inode or
parent transactions and Redis server execution still serialize. No throughput
win is claimed before the replacement's correctness and matched performance
gates pass.
