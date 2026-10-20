# BrewFS Workspace Overlay implementation specification

Updated 2026-10-20. **Status: entity experiment with opt-in benchmark mount
wiring; production mount admission remains disabled.**
This specification replaces the previous mutable-layer log design as the target
for staged implementation. The
[stage 2 checkpoint](../plans/2026-10-19-overlay-entity-stage2.md) and
[file-I/O continuation](../plans/2026-10-20-overlay-entity-data.md) distinguish
implemented APIs from the remaining production behavior below. The
[mount experiment checkpoint](../plans/2026-10-20-overlay-entity-mount.md) records
the subsequent fresh-fixture bootstrap and dedicated FUSE adapter, including
their separate validation status. No completed schema migration or qualified
production mount is claimed.

The shipped baseline remains r31, PR #41 (`0d4e36c`), with its delivery documents
in `6b54270` on `codex/packed-metadata-aliyun-20260930`. That implementation uses
an immutable packed lower and a non-rotating writable overlay, versioned extent
rows, `head.next_sequence`, an expected-head CAS, a mount-wide mutation gate,
data group commit, per-layer extent generation, and lease-bound caches. A
nonempty mutation advances its head sequence; an empty authorization need not.
This is not a full replay log: scalar inode/dentry/xattr rows are overwritten,
while data extents carry sequence-ordered history. The
[delivery handoff](../plans/2026-10-17-overlay-no-seal-handoff.md) records its
measurements, known failures and reproduction commands. The previous text of
this specification remains available at `6b54270` for interpreting that format.

The r32 prefetch/concurrent-commit experiments are **dropped, unvalidated**.
Their unfinished source is not part of this target. The prefetch full gate was
deliberately interrupted; no optimized release/probe/acceptance comparison
established a result. This is not a measured negative performance result.
Baseline diagnostic artifacts are retained in the
[WRITE investigation](../plans/2026-10-19-overlay-write-r32.md).

The maintained [architecture](../../architecture/workspace-overlay.md) describes
the same target model. The
[packed-v3 specification](2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md)
continues to define immutable packed objects, checksums, authenticated manifests
and bounded reads. Removing a mutable overlay digest does not weaken those
integrity contracts.

## 1. Target and scope

Each workspace has exactly one immutable packed lower and one ordinary mutable
metadata namespace. The upper stores the **current state** of changed entities,
as a conventional filesystem does: inode attributes, directory entries,
xattrs/ACLs and chunk-to-slice mappings. Reads check the upper first and fall
through to the pinned lower when the upper has no overriding state.

The target has no mutable layer log, global operation order, per-layer
`next_sequence`, per-commit head-version CAS, or canonical mutable-layer digest.
There is no mount-wide mutation mutex in the ordinary data/namespace path.
There is still atomicity for each POSIX operation, local ordering for conflicting
entities, lease fencing, durable object ownership and crash recovery.

There is no seal transaction, head rotation, flatten/compaction workflow or
strict-lower mode. Fork accepts an exact existing immutable packed lower;
creating a new immutable lower remains an explicit packed publication operation.
Ordinary fsync/close does not pack a workspace.

This is a staged implementation contract. The experiment defines
`workspace-entity-v1` and a bounded reference schema; it does not establish a
production wire-format promise, mounted-write admission or a throughput gain.

## 2. Required semantics and removable machinery

| Mechanism or requirement | Classification | Target treatment |
| --- | --- | --- |
| Single writable lease, backend-time expiry, stale-holder fencing | Required | Retain; every mutation validates current authority |
| Atomic create/rename/link/unlink and inode/data updates | Required | Entity transactions; lock only the affected entities |
| O_EXCL visibility before CREATE returns | Required | Commit the new inode/dentry immediately; never defer create until fsync |
| Whiteouts, holes, lower masking after truncate | Required | Explicit current-state records and a lower visibility boundary |
| Immutable packed manifest/object integrity | Required | Retain authenticated identities, checksums and reader bounds |
| Shared-object ownership, deletion exclusion and reader safety | Required | Reference counts, lower/publication owners, retirement holds and proven namespace quiescence |
| Sequence-ordered extent history and head-coupled deltas | Artifact of the old representation | Replace with the current normalized map for each chunk |
| Shared head counter and expected-head CAS on every commit | Artifact of the old representation | Remove; unrelated files have no shared mutable commit record |
| Deterministic mutable-layer digest and global ordering/replay scaffold | Artifact of the old representation | Remove; explicit stable exports are a separate operation |
| Per-layer extent generation and complete extent census before deletion | Old GC proof | Replace only after all owners use the new refcount/deletion protocol |
| Mount-wide mutation gate and generic exact-view commit packet | Old implementation machinery | Remove from the target hot path; retain entity ordering and authority checks |
| Shipped data group commit/commit-view cache, proposed create batching | Optimizations compensating for serialized commits | Not target requirements; drop during the first implementation; create batching never shipped |
| Sequence blocks, backend template stamping, r32 shared-admission adapter | Unvalidated optimization of the old representation | Drop; do not carry into the new schema |
| Lease-bound upper row/permission caches | Optional optimization | Require entity-scoped invalidation and a correct uncached path |
| Immutable lower metadata memos/prefetch | Required entity hot-path policy | Bound memory, asynchronous authenticated fills and session-retained lower ownership |
| Derived reverse aliases | Optional lookup index | Retain only when needed by operations; update atomically with their dentry |
| Recovery intent for upload, truncate, publication or deletion | Required where an operation crosses transaction/object boundaries | Bounded, idempotent lifecycle state; not a total filesystem mutation log |

The old log is unnecessary for lower sharing: the immutable lower binding is
the sharing unit. It is unnecessary for write ordering: operations affecting the
same inode/chunk serialize at their entity transaction. It is unnecessary for
lease safety: authority is a live lease record and holder identity. GC needs
proof of live owners and exclusion of new owners during deletion, not a total
sequence over every write. Diff can compare current upper state with the lower;
it does not promise historical replay or a cheap arbitrary-workspace diff. The
old scalar-row layout also did not preserve a complete operation history.

## 3. Compatibility and format boundary

`workspace-overlay` stays off by default. Ordinary flat volumes retain their
existing `MetaStore -> MetaClient -> VFS` path, schema, caches, object keys and
background jobs. Workspace volumes use the workspace adapter and explicitly
selected workspace backend. An overlay error must not fall back to flat
semantics, and a binary lacking the required feature/format must fail closed.

The existing `workspace-v1`, catalog format 2, `BaseRevision` and bincode bytes
remain definitions of the shipped format. Do not silently reinterpret those
bytes, change a hash field's meaning, or make an old binary read the new layout.
The target requires an explicit layout/capability marker and a separate upper
namespace, selected at mount. Its final wire identifier and encoding belong in
the schema change, not an undocumented reuse of an old marker.

Existing `SliceDesc` and `chunks/{slice_id}/{block_index}` object addressing need
not change. Upper records may refer to those immutable objects through a neutral
read-plan interface. The packed-v3 wire format remains authoritative for lower
object references; an old layer root hash must not be treated as a manifest
object digest.

## 4. Metadata placement and current records

Use **workspace prefixes in one volume transaction domain**, not an independent
Redis database per workspace. For Redis Cluster, all records needed by one
transaction, including shared ownership/refcount records, use the same volume
hash tag. Independent Redis DBs or cross-slot keys cannot provide the required
atomic map/refcount updates and are not part of the initial target.

The following are logical key identities, not a finalized wire encoding:

| Record | Identity | Current contents |
| --- | --- | --- |
| Workspace descriptor | workspace | Upper namespace/layout identity, immutable lower manifest binding, state, active lease identity |
| Inode | namespace, inode | Current attributes, nlink/state, lower visibility boundary, optional recovery intent |
| Dentry | namespace, parent inode, raw name | `Present(inode, kind)` or `Whiteout` |
| Chunk map | namespace, inode, chunk index | Complete normalized map of that chunk's current contents |
| Xattr/ACL | namespace, inode, raw attribute name/type | Current value or whiteout |
| Reverse alias, if required | namespace, inode, parent, raw name | Current alias, atomically maintained with its dentry |
| Slice owner membership | namespace, inode, chunk index, slice ID | One durable ownership edge for a distinct slice present in that chunk map |
| Object/slice ownership | volume, immutable object identity | Refcount, lifecycle state and durable staging/publication/retirement holds |
| Lower owner | volume, immutable lower identity, owner identity | Workspace, snapshot, reader/session or publication reference to the lower |
| Lease/open authority | workspace, holder/session | Sole writer, backend deadline, recovery/takeover fencing |
| Allocators | volume or namespace as appropriate | Plain monotonic inode/slice allocation counters |

The descriptor changes on administrative lifecycle events, lease acquisition or
release, and format/binding migration. Ordinary writes do not increment a
descriptor version, write a head record, or maintain a workspace-wide dirty
counter as a substitute serialization point. Lease renewal updates authority,
not an ordering sequence.

Names remain raw bytes with existing length, NUL and separator validation; key
encoding must be unambiguous and order-preserving where directory enumeration
needs it. Inode allocation begins above the lower's maximum inode number and
must respect the signed inode range. Slice IDs are globally unique and never
reused. Existing allocators may use ordinary `INCR`/transactional increments;
overflow fails before exposing an invalid ID. Allocation gaps are harmless.
Redis allocations respect its signed 64-bit counter limit. IDs above `2^53`
must remain exact strings/bytes through Lua and protocol handling; they must
not round through a Lua floating-point number or silently wrap/narrow.

There is no hidden claim that Redis Lua executes concurrently: Redis still
serializes scripts. Concurrent clients can overlap preparation and round trips,
and unrelated operations no longer optimistically CAS the same changing head.

## 5. Resolution rules

### 5.1 Namespace and attributes

For `(parent, name)`, an upper `Present` wins, an upper `Whiteout` returns
not-found, and absence consults the immutable lower. Removing an override that
masks a lower name would resurrect that name; whiteouts therefore survive until
the workspace is discarded or explicitly migrated.

An upper inode contains the current effective attributes after copy-up. If it
is absent, use the lower inode. A deleted/orphan inode state is not absence and
must not expose the lower inode again. All hardlink aliases resolve to the same
workspace inode; copy-up changes the inode once and never modifies a sibling
workspace. Xattrs and ACLs use the same value/whiteout/absence distinction.

Readdir merges upper entries and lower entries by raw name, honoring whiteouts.
It uses bounded directory enumeration, not a scan of all workspace metadata.
An optional reverse-alias index is a lookup aid; forward namespace rows remain
authoritative and the index must not authorize removal of a live reference.

### 5.2 Chunk maps

A present chunk row is the **complete current mapping** of the fixed logical
chunk range. It contains sorted, disjoint spans covering that range:

- `Data`: immutable upper slice ID, slice offset and length;
- `LowerRange`: authenticated locator into the workspace's pinned lower;
- `Hole`: explicit zero bytes, with no object fetch.

A present chunk never falls through through a missing span. A gap, overlap,
out-of-range locator or malformed map is an error, not implicit lower data.
The format may encode adjacent equal spans compactly but cannot omit their
semantic coverage. Reads are clipped to inode EOF before executing the map.

An absent chunk falls through to the lower only below the inode's
`lower_visible_size`; the rest is zero. For a lower inode this boundary starts
at the lower file size; for a newly created inode it is zero. It can only
decrease during that inode's lifetime. Extending a file never raises it.

The first write to a chunk materializes its **mapping metadata**, including
lower locators and holes, then replaces the written interval with new Data
spans. It does not copy the lower payload or the whole file. Later writes
replace intervals in the current map, removing obsolete spans; no sequence
history or newest-wins merge is needed on reads. Punch-hole/zero-range writes
Hole spans. Dropping an all-hole chunk is legal only if absence would also
resolve to zeros.

Map preparation must have explicit memory/record limits. Excessive fragmentation
cannot trigger an unbounded metadata response or silently lose spans. A future
map-page layout or payload rewrite is a separate design decision; it is not an
implicit workspace flatten/compaction operation in this target.

## 6. Transaction and concurrency contract

An operation atomically validates its authority and relevant current entities,
then changes exactly those entities and their ownership edges. Redis scripts
read/check/update the selected keys at the server; TiKV/SQLite use the equivalent
transaction. There is no generic expected-head comparison or retry based on an
unrelated file's write.

Mount-local locks cover conflicting inode/chunk/parent operations. Acquire
multiple locks in a stable order; rename may involve both parents and both
inodes. Same-parent creates legitimately share a parent update, and same-file
writes legitimately share size/map state. A local lock is not backend authority:
the transaction still fences expired leases, stale holders and deletion state.
Backend conflict handling may use current-row predicates or entity-local
versions where necessary; it must not introduce a new global commit version.

Concurrent whole-row updates must not overwrite each other's size, mode, ACL or
timestamps. A cached permission decision is valid only under the current lease
and matching entity/invalidation state; otherwise re-read and decide again.
Do not transparently retry a stale prepared inode image against newer metadata.
This includes mutable ancestor permissions on which the operation relies:
protect their decision inputs with the relevant locks or transaction checks,
not merely the final inode's cached row. Concurrent ancestor chmod/ACL changes
must not leave a stale path authorization valid.

Each operation has one visibility point. No lock is held around unrelated file
uploads or reads. Temporary admission stops for an administrative stable export,
format switch or exclusive recovery are allowed; these are not per-operation
hot-path serialization.

Initial implementation does not include group commit, speculative commit-view
caches, generic sequence templates, or reserved sequence blocks. Establish the
plain transaction behavior and measure it before adding an optimization.

Redis Lua atomic execution is not rollback on a runtime error. Validate all
input encodings, key/value types, bounds, counter overflow, reference-count
underflow and computed writes
before the first mutation. An error after an earlier SET must not leave a
partial inode/map/refcount operation. Backend-specific failure behavior must
be tested rather than inferred from the word transaction.

## 7. POSIX mutation behavior

| Operation | Required atomic result |
| --- | --- |
| Create/mkdir/symlink/mknod | Reserve a unique inode ID; atomically validate parent/type/permissions/ACL inheritance and effective name absence, install inode/dentry and update parent |
| O_EXCL create | Exactly one winner for the effective name; visibility is committed before CREATE succeeds |
| Link | New dentry and effective nlink change together; aliases retain one inode identity |
| Unlink | Whiteout/removal as required by lower presence, nlink/parent update and orphan status together |
| Rmdir | Effective upper-plus-lower emptiness and namespace mutation are ordered against concurrent children |
| Rename/replace/exchange | Source/destination, affected parents, link counts and directory-parent semantics change atomically; no partial exchange |
| Chmod/chown/xattr/ACL | Current inode/ACL relationship and privilege rules remain consistent in one transaction |
| Write/punch-hole | Current chunk map(s), inode size/times and slice-owner deltas publish together for the admitted write unit |

Create allocates with INCR inside its namespace publication transaction. It
previews the exact decimal successor to validate generated keys and collisions
before the first mutation; IDs and timestamps never round through Lua numbers.
The allocator is not a caller-side CAS predicate. An O_EXCL conflict must not
advance the counter. Unknown transport outcomes are returned without replay.
The same transaction exposes inode, name, inherited ACLs and parent updates.
All mutable validation inputs, including effective absence for O_EXCL, remain
exact predicates. Upper name absence or a canonical tombstone is required;
a present name cannot be overwritten by supplying its matching current bytes.

Create batches the five authority rows with parent/ancestor inode and ACL rows,
the destination dentry and the parent's truncate marker. A bounded advisory
parent-hint cache selects this batch; it never caches permission decisions.
The expected warm path is one read and one commit. Cold or stale topology can
require additional reads, and conflicts recalculate permissions from fresh rows.
Successful create-open retains the parent lock until the open handle is installed.

Sticky-bit, setgid/default ACL, symlink, raw-name, special-inode/rdev and existing
lock semantics remain required. Rename cycle detection must use a stable view
of the affected ancestry; simplifying storage does not remove that POSIX check.
File locks are scoped to the workspace and inode, so siblings do not interfere.

The target's initial atime policy is explicitly **noatime**. Regular open checks
lease/permissions with zero metadata writes and retains a local handle. Linked
close releases the handle locally, including after lease expiry; neither
operation updates atime. `O_TRUNC` is still a data/metadata
mutation. Last close of an unlinked inode and orphan recovery retain durable
cleanup behavior. This policy is confined to the proposed workspace layout;
it does not alter flat volumes or claim uniform native relatime behavior.

The measured r31 WRITE path already uses `record_open`/`record_close`, not the
separate legacy atime-mutating `MetaLayer::open`. Dropping that legacy atime
path alone would not explain or fix the recorded throughput gap.

## 8. Truncate without data resurrection

A shrink must hide old upper **and** lower bytes after later re-extension.
Changing only inode size or deleting only lower extents is insufficient.

Use an inode lock and a durable `Truncating` intent:

Before publishing the cut, drain earlier buffered writes/uploads and their
metadata publications for this inode. If later implementations permit overlap,
they require an inode-scoped truncate-generation fence that rejects old upload
completions/recovery publication. Otherwise an earlier write landing after
cleanup could resurrect data. This is not a workspace-wide operation sequence.

1. Commit the new size and `lower_visible_size = min(old_boundary, new_size)`
   with the intent. This is the authoritative visible cut. Reads cannot expose
   bytes beyond the new EOF.
2. Block writes, extension and conflicting mutations of this inode until cleanup
   finishes. Other inodes continue normally. Safe reads below the cut may use
   the old maps, clipped to the current inode size.
3. In bounded transactions, remove chunks beyond the cut and replace the
   boundary chunk's suffix with Hole, updating slice-owner/refcount deltas in
   those same transactions. Advance the durable cleanup cursor atomically with
   each map/owner/refcount change; replay cannot skip rows or release twice.
4. Clear the intent only after every stale upper mapping beyond the cut is
   removed/masked. Re-extension can then expose only zeros or subsequent writes.

Recovery finishes a pending truncate before admitting mutations of that inode.
It must not clear the intent merely because the process restarted. An error
after the visible cut is an incomplete cleanup with known persistent state,
not permission to restore an old size or report success for an unperformed
extension. Fsync must durably cover the cut and its recoverable cleanup state.
The intent is bounded operation recovery, not an append-only mutation history.

## 9. Object ownership and reference counts

Object lifetime is independent of inode nlink. The ownership unit for upper
data is one distinct `(namespace, inode, chunk, slice ID)` membership. Multiple
spans of the same slice in one chunk contribute one owner; the same slice used
in two chunks contributes two. Adding/removing a hardlink does not duplicate
the chunk's data owners. Packed object/block allocation granularity must be
respected; a slice counter cannot authorize deleting a pack shared by other
live owners.

Direct sharing/reflinking of mutable upper data between two live workspace
namespaces is outside the initial scope. Sharing through immutable publication
and pinned lowers remains supported. Membership/owner accounting still handles
multiple chunks within a namespace and an upper-to-immutable ownership transfer.

When replacing a chunk map, compute old/new distinct upper-slice sets. The
metadata transaction installs the new map, adds newly required owner edges,
increments their counts, removes obsolete edges, and decrements their counts.
Edges make retries/recovery idempotent: reprocessing the same replacement must
not double-decrement or double-increment. Negative counts, missing required
owners or conflicting ownership state fail closed and require repair.

Newly uploaded data has a durable staging/orphan owner until its chunk mapping
and normal ownership commit. Failed/fenced publication leaves recoverable
orphan work, not a live mapping to missing data. No writer can reference an
object already reserved for deletion; the owner acquisition and deletion-state
check share the metadata transaction.

### 9.1 Lower ownership

A workspace pins its whole immutable lower manifest/ownership graph. Fork adds
one lower owner and an empty upper namespace; it does not enumerate or increment
every lower inode, chunk or slice. A lower pin protects all packed objects and
borrowed immutable objects reachable through that lower's validated ownership
inventory. `LowerRange` spans rely on this pin and cannot outlive it.

Snapshots, active readers and incomplete publications have their own durable
lower owners. Removing 99 of 100 workspace owners cannot free the shared lower.
Namespace destruction releases its lower owner only after its upper references,
open handles, pending reads and recovery obligations are resolved.

The experimental SDK's trusted constructors require the packed owner's volume
and namespace IDs to match `EntityKeys`, with the same lower binding and
authoritative entity/packed backend pairing. Terminal deletion proof must come
from that same entity namespace backend. The generic lower trait checks the
manifest binding but does not enforce owner namespace identity. Enforced
production factory pairing is absent from the experiment. Standalone packed
adapter reads outside an entity session must be quiesced by their caller before
releasing the owner. The crate-private benchmark bootstrap passes one supplied
volume/namespace/binding to the existing constructors and uses the supplied
Redis URL for both adapters. This is trusted fixture wiring; it does not add
enforced backend pairing, an owner-identity hook or an external-writer check.

If publication includes upper data in a new lower, establish the new immutable
owner **before** dropping any old upper owner. Record and recover that transfer
with a bounded durable publication intent. Never briefly drive the object's
count to zero between two representations of a live reference.

## 10. GC and in-flight reads

Reference counts select candidates; deletion requires an atomic lifecycle
transition. For a zero-reference object, GC must establish that there are no
live lower/upper owners, staging obligations, publication pins or retirement holds,
then reserve `Live -> Deleting`. New owner acquisition must reject `Deleting`.
Physical deletion is idempotent; failure leaves the deletion record for retry.
Object IDs are never reused, so an old delete cannot target a later allocation.

A read can hold an old plan after another transaction replaces its chunk map.
The initial target therefore performs **physical reclamation only during proven
namespace quiescence or fenced discard**. Live writes update current refcounts
and enqueue durable retirement holds atomically with removing the last relevant
map membership. They do not delete objects merely because the current count is
zero. This protects old read plans without a distributed per-object pin request
on every read.

To release a namespace's retirement holds, stop admission, drain writes and all
active read plans/handles, and fence recoverable sessions so no old request or
recovery task can resume. Deletion still requires zero current owners and no
lower/publication/staging/retirement holds from **any** namespace. Quiescing one
namespace does not discharge another namespace's reader obligations. If an old
session cannot be proved drained/stopped, defer reclamation; lease expiry or a
fixed grace interval alone is insufficient.

The explicit tradeoff is that an active namespace retains retired bytes until
maintenance or discard. Online per-object reader pins or finer retirement
epochs may be evaluated later as a separate optimization; they are not part of
the initial target or a prerequisite extra RPC on every read.

Workspace deletion first fences new mutation, marks the namespace deleting and
resolves active leases/handles/readers. Bounded sweeps remove current entities
and release their owners transactionally. A crash resumes from recorded state;
revisiting a row cannot release an owner twice. The lower owner is released last.
GC/recovery has explicit lifecycle authority for a quiescent/deleting namespace;
it does not impersonate an expired runtime writer lease. Its authority and
checks must exclude a new mount or recovery publication during reclamation.

Namespace mark/sweep also reconciles owner edges and detects unreachable staging
or orphan records. It is a repair and cleanup mechanism, not a mandatory full
catalog scan on each write. A concurrent incomplete `SCAN` is not evidence that
an object has no owners: deletion relies on the object lifecycle fence and the
authoritative ownership protocol. Offline repair may quiesce the namespace.

The r31 per-layer extent-generation/census proof remains necessary for old-format
owners throughout coexistence. Do not remove those checks while any old writer
or old GC path can touch the same objects. Retirement occurs only after format
fencing and migration prove every remaining owner participates in the new
protocol. New refcount fields alone do not make an old census collector safe.

## 11. Lease, caches and session authority

There is one active writable lease per workspace, identified by holder/session
generation. Acquisition rejects a competing live writer. Mutation validation
checks the workspace state, namespace identity, active lease, holder generation,
required open/writer authority and backend-time expiry at the transaction's
visibility point. No host-wall-clock deadline authorizes a stale writer.

Heartbeat renews only its own authority. Local `LeaseValidity` extends only
after the authoritative renewal succeeds; failure revokes it. Lease loss blocks
new commits and clears/bypasses mutable caches. Takeover/recovery must honor
existing packed open records and reader/writeback obligations; it cannot infer
that a disconnected process has stopped using its objects.

The immutable lower is authenticated at attach and memoized by binding identity.
The entity packed adapter uses a shared 64 MiB byte-weighted LRU budget for inode,
dentry/negative-dentry, cold xattr/ACL, chunk-map and directory metadata, alongside
the packed reader's existing 4 MiB index cache. Concurrent asynchronous fills of
one key coalesce; errors are not memoized as absence. Mount warmup prefetches root
metadata and up to 32 children with four concurrent child loads, including cold
attributes and first-file-chunk maps. Cache hits use memory without metadata
network/object requests. Cold and evicted entries require authenticated fills;
neither this budget nor prefetch promises complete residency of an unbounded lower.

Binding validates the durable packed owner and exact volume, namespace, manifest,
root and maximum-inode identity against the acquired writer session. Bound hits
and fills retain local admission until completion. The same session must close
and drain before terminal lower-owner release, so per-key owner RPCs are redundant
within this bound lifetime. Unbound SDK calls retain remote owner checks. Upper
rows/tombstones take precedence at each entity key; a copied-up inode alone does
not suppress lower inheritance for an absent xattr or chunk.

Writer sessions retain the complete immutable lower binding. Runtime authority
fetches descriptor, lease, lower pin, namespace lower owner and lower registry in
one snapshot and verifies that binding has not changed. Every transaction still
checks their exact bytes and the backend-time lease deadline.

Upper inode/dentry/permission caches are separately bounded and authoritative
only under the current lease. Entity invalidation occurs before/after mutation;
an in-flight stale fetch must not repopulate a cache after invalidation.
Namespace changes invalidate the affected names, aliases and path dependencies.
Data-only writes need not clear unrelated namespace caches.

Read-plan consistency uses the affected inode/chunk lock or an equivalent local
generation validation, with lifetime protected by lower ownership and deferred
reclamation until namespace quiescence. A lower binding identity
or a lease token alone cannot detect a same-session concurrent write. No cache
may reintroduce a persisted workspace-wide version requirement.

## 12. Durability, failures and fsync

Use the existing durable data path with explicit workspace recovery identity.
Before publishing a mapping, its data must be remote durable or protected by a
proven recoverable staging protocol that can complete the upload after a crash.
Until that protocol is proven for this layout, require remote durability before
metadata publication. Local memory is never sufficient object durability.

Fsync waits for the file's required data and metadata commits and the documented
remote/durable-staging boundary; it must propagate upload, commit and lease
errors. Close must not silently discard dirty state or convert a failed commit
into success. Existing writeback recovery keys must distinguish volume,
namespace, inode, chunk and writer incarnation without altering flat-volume
keys. Packed publication always drains referenced data to remote durability.
The fsync barrier is scoped to the file, or to an explicitly documented
directory durability boundary. It is not a flush/order barrier across unrelated
workspace files and must not recreate the removed global serialization point.

The experiment uses synchronous upload-before-publication, one current chunk
per transaction. A multi-chunk write returns only its acknowledged prefix;
upload/publication uncertainty remains a session-shared file error for fsync,
including after caller cancellation or constructing another adapter. A rejected
pre-effect argument does not poison fsync. File operations retain the same
inode lock through I/O and use owned tasks once effects can begin. Ordinary
close releases a handle; it is not a durability acknowledgment for a canceled
operation, whose task and admission remain alive independently.

Strict entity PUT is a separate backend capability. S3 must support conditional creation;
the legacy OSS unconditional overwrite fallback is forbidden here. The configured
S3 durability and Redis persistence/replication policy remain external contracts.
Standalone Redis tests with persistence disabled prove transactions/replay, not
host-power-loss durability.

| Failure point | Required result |
| --- | --- |
| Upload succeeds; metadata explicitly aborts/fences | No live map references it; durable staging/orphan owner enables eventual cleanup |
| Map/refcount transaction aborts | Both old map and old ownership remain authoritative |
| Commit result is uncertain | The map may already reference the durable object; resolve using operation/entity identity or return uncertainty, never blindly replay refcount deltas |
| Crash after map/refcount commit | New mapping and its owners recover together |
| Crash during truncate | Visible cut remains authoritative; cleanup resumes before extension/write |
| Crash after GC reservation | New owners remain excluded; physical deletion resumes idempotently |
| Crash during publication ownership transfer | At least one durable owner remains until recovery completes |

Per-operation identity/recovery records are allowed where an external upload or
multi-batch cleanup needs them. They do not assign a total order to normal
filesystem mutations. The metadata backend's own crash-durability configuration
and object-store guarantees must be stated in deployment and test evidence.

## 13. Lifecycle, export and diff

Mount selects the explicit upper layout, validates its descriptor and immutable
lower, acquires the sole-writer authority, recovers pending obligations, then
admits operations. Partial mount failure releases only resources it owns.

The minimal mounted experiment is narrower than that production contract.
`entity/experiment.rs` exposes crate-private `RedisEntityExperiment` and
`ExperimentalEntityMount` wiring for an unused packed catalog and fresh entity
namespace. The CLI selects `--volume-format workspace-entity-v1` with
`--experimental-entity-mount`, a packed manifest key, a non-nil
`--entity-volume-id` and a non-nil workspace UUID. Manifest/workspace inputs
may use their existing configuration sources. `--workspace-namespace` is
optional and defaults to `brewfs`; the selected scope must be unused, and the
isolated harness supplies a fresh scope explicitly. Without opt-in, entity
mount dispatch fails closed.
The caller supplies a standalone packed graph in a dedicated experimental
object bucket and unused metadata scopes. These are fixture preconditions, not
evidence that another catalog's writers/readers have stopped.

Bootstrap uses the existing authenticated manifest/max-inode reader, creates a
private packed catalog, takes its setup lease, installs the packed lower and
acquires a source reader pin. `PackedLower::open` promotes that pin to the
existing durable lower owner for the selected volume and namespace. The source
pin is released, `Lifecycle` provisions/acquires the entity writer, and the
setup lease is released before exposing namespace/data handles. The retained
source catalog is an administrative lower root, not a shared head on the entity
write path. Uncertain failures retain protection conservatively; this wrapper
does not implement general rollback/cleanup reconciliation.

The mount renews the writer lease through the existing lifecycle API. Orderly
unmount first stops FUSE request admission and drops the adapter's handle
registry, then calls the existing session drain/maintenance transition. This
wrapper retains upper data, lower ownership and source catalog records and
provides no reopen, resume or discard operation. The isolated benchmark harness
owns teardown of its services, volumes and bucket. No crash takeover, combined
stack fault qualification, offline conversion, cutover, pairing enforcement or
production pagination is added by this wiring. Ordinary production admission
remains disabled.

Fork from an existing immutable lower creates a descriptor, empty upper and
lower owner in a bounded metadata operation. Fork from an active mutable
workspace is not introduced. Discard uses the fenced namespace-deletion process
above, never recursively deletes a shared lower, and never removes shared
object ownership solely because the originating workspace was deleted.

An explicit export/publication must quiesce the source, drain writes and walk a
stable effective namespace/map view. Ordinary Redis `SCAN` alone is not a
snapshot. Build/upload/validate packed objects using packed-v3 integrity rules,
establish the new immutable owner, then publish its descriptor/binding through
the administrative recovery protocol. No mutable-layer digest or runtime total
order is required. A checksum over the exported immutable objects remains valid
and required; it is not the removed hot-overlay root digest.

Diff compares the current namespace, attributes and normalized maps with the
pinned lower or another explicit stable export. The upper key set can identify
candidates, but a changed-then-restored record may be semantically unchanged.
Rename recognition is best effort from current aliases/inode identity; when
ambiguous, report delete+add. A consistent full diff may require a stable walk.

The target removes the shared sequence-based representation and deterministic
mutable-layer digest identity. It offers no cross-entity total order, total-order
replay or cheap arbitrary-workspace incremental diff. Upper-only changed-key
enumeration remains useful for diff candidates; a stable semantic comparison is
the general contract. The shipped scalar rows already overwrite old values, so
this is not a claim that r31 implemented a complete historical replay service.

Cheap chronological/incremental diff, total-order replay/auditing and a live
mutable-layer digest can be separate future products only with explicit cost
and semantics. They must not be recreated as a hidden global counter or log in
the write path.

## 14. Backend and VFS interfaces

Expose operations in terms of current entities and ownership, not
`VersionedMutation` templates tied to an expected layer pair. The implementation
should have bounded point reads and operation-specific atomic mutations for
namespace, inode/ACL, chunk replacement, ownership and lifecycle state.
The stage 2 `EntityBackend` provides bounded row transactions for the namespace.
`EntityData` implements current-map file operations and the existing VFS file
APIs dispatch explicitly through `EntityFileIo`. Mount admission remains a
separate qualification.

Redis is the primary implementation target, using bounded binary-safe scripts
in one volume hash slot and backend `TIME` for lease checks. TiKV must implement
the same transaction/authority contract with its transaction clock. The stage 2
local reference/fault-injection backend is `MemoryEntityBackend`; SQLite is
unimplemented and unqualified. Unsupported capabilities must fail
explicitly rather than run an unsafe partial protocol. Live Redis validation
does not imply TiKV or SQLite migration support is complete.

The current implementation uses the existing authenticated packed reader for
LowerRange and a separate current-map executor for upper Data and Hole spans.
The legacy unified provider's generation type contains head/sequence fields;
entity handles do not manufacture those fields. The explicit handle dispatch
preserves logical and object offsets, hardlinks, raw names and bounded memory.
Entity handles stay outside native inode-number indexes; unsupported fallocate
and copy-file-range handle operations fail closed. FUSE callbacks accepting live
entity handles reject them before native inode/cache/lock effects; this SDK seam
does not admit entity mounts. A separate experimental `EntityFuse` adapter
accepts the explicitly supplied entity namespace/data/root for the benchmark
mount; it does not reinterpret the SDK/native FUSE callbacks as entity-safe or
enforce production admission. Fully covered upper reads need
no lower payload fetch. Zero spans perform no object request. The ordinary flat
slice cache/compactor must not reinterpret these workspace plans.

`statfs` must use consistent persisted usage information when provided. Do not
scan an entire namespace for each call or substitute fabricated zero usage.
Until supported, retain an explicit unsupported result. Usage accounting is
not justification for reinstating a workspace-wide CAS on every write.

## 15. Experiment stages and deferred migration

Each stage has its own reviewable result. Stage 2 is now an isolated experiment
on `codex/overlay-entity-namespace-20261019` from `b6f297e`; see the
[implementation checkpoint](../plans/2026-10-19-overlay-entity-stage2.md).
The namespace, Redis transactions, packed adapter and experimental VFS file
dispatch do not admit production mounts. Stage 3 now has a minimal fresh-fixture
mount experiment, tracked in its own checkpoint. This stage is fresh-only:
importer, ownership migration and cutover are explicitly deferred. The remaining
production-admission limits below are not tasks for this benchmark stage.

1. **Preserved baseline and format boundary.** R31 code/format and accepted
   artifacts remain the baseline. Unvalidated r32 source was dropped; its
   diagnostics and incomplete gate evidence remain archived.
2. **Reference model and schema.** Current entity/map resolution and ownership
   transitions are implemented behind separate format dispatch. The stage 2
   and file-I/O checkpoints record their reference-model, metadata and service
   test coverage; those API-stage results do not qualify a production mount.
3. **Fresh-only backend/FUSE experiment: current scope.** The dedicated adapter
   uses operation-specific namespace/data APIs without old group-commit/head-CAS
   adapters. Its normal mounted probe, local gate and matched performance runs
   have their own checkpoint. This stage adds no fault-injection or broader
   production-admission work.
4. **Offline importer: DEFERRED.** No importer or adoption of existing mutable
   workspaces is implemented. The experiment bootstraps only fresh fixtures.
5. **Ownership migration/validation: DEFERRED.** No transfer from an old catalog,
   migration checkpoint or old/new semantic comparison is implemented.
6. **Atomic cutover/rollback: DEFERRED.** There is no descriptor format switch,
   migration recovery or reverse export. **After new-format writes, pointer
   rewind would lose data.** This experiment provides no rollback to r31.
7. **Production qualification and retirement: DEFERRED.** Enforced pairing,
   external crash-takeover authority, paginated production cleanup, full
   mounted/POSIX qualification and retirement of old owners remain limitations.
   The experiment leaves existing ownership and legacy code in place.

The schema/namespace selector is administrative identity, not a new hot head
version. Migration journals/checkpoints and exported-object checksums do not
restore a global runtime mutation log or digest.

## 16. Validation and performance evidence

New object-I/O tests use the production `S3Backend` against an isolated RustFS
service through `entity/test_objects.rs`. The helper requires an explicit
loopback `BREWFS_PACKED_GRAPH_TEST_S3_ENDPOINT`, a UUID-named isolated
`BREWFS_PACKED_GRAPH_TEST_S3_BUCKET`, and runner-provided AWS credentials;
Redis integration also requires `BREWFS_TEST_REDIS_URL`. It disables mutation
replay and gives fixtures unique object keys in the runner-owned bucket.
Memory metadata backends provide deterministic transaction/fault injection;
local temporary directories are limited to producer metadata spools. LocalFS
does not supply the new entity durability capability or its object-I/O evidence.
Service tests are ignored by default and must run with `--include-ignored` in
the real-service gate. Selecting them without configuration fails explicitly.
The RustFS/Redis gate passed six packed-lower, 78 entity and two VFS tests at its
recorded API-stage source snapshot, with zero failed or ignored in those
selected suites. The
[file-I/O continuation](../plans/2026-10-20-overlay-entity-data.md) records the
source snapshot, exact commands, artifacts and remaining qualification gaps.
Those results do not validate the subsequent bootstrap or dedicated FUSE
adapter; their checks and mounted probes are tracked in the mount checkpoint.

The recovery fault oracles exercise metadata and simulated byte/deletion
state; separate RustFS fixtures exercise object requests. Packed-lower tests
use memory entity metadata even when their packed owner/census is in Redis;
Redis entity data/VFS tests use a synthetic lower. This is not qualification of
the combined Redis entity, real packed lower and VFS stack, physical packed
graph retirement, or process/host crash recovery. Local maintenance tokens
prove a drain in the surviving process only. External process-takeover
authority remains unimplemented, and lease expiry alone cannot supply it.

The following target requirements remain unqualified as a complete production
stack. They describe known limits, not additional tasks for the current
fresh-only benchmark stage:

- two siblings share lower bytes but isolate create/write/chmod/rename/unlink;
- concurrent O_EXCL has one winner, same-file size/maps retain all valid writes,
  and unrelated creates/writes do not wait on one mount-wide metadata gate;
- holes and shrink/re-extend never expose old lower or old upper bytes;
- a crash at every truncate stage completes cleanup before mutation resumes;
- lease expiry/loss, stale generation, takeover and admin changes fail closed;
- cached ACL/permission decisions cannot survive a conflicting mutation;
- map/owner/refcount updates abort and recover atomically, including uncertain
  replies and retries without duplicate increments/decrements;
- a new owner racing `Live -> Deleting` wins before reservation or fails after
  it; active readers and delayed read plans retain objects until all relevant
  namespace retirement holds can be released under proven quiescence;
- 100 children keep a shared lower alive after 99 deletions, and publication
  never leaves an ownership gap for borrowed upper objects;
- namespace/migration cleanup resumes without double-release, and old collectors
  cannot delete objects visible through the new layout;
- fsync/restart and a real mount verify durable payloads and namespace state;
- default flat behavior, feature isolation, backend contracts, POSIX and the
  repository's required fmt/clippy/build/test gates remain valid.

Future performance acceptance uses matched compose runners and preserved executables.
Measure 16 and 32 workers with arm order swapped; report overlay and native
files/s, sample counts/spread, raw and normalized deltas, errors and teardown.
Retain readback/mixed correctness and latency, object amplification, fsync/close
and writeback-debt checks. Broader randrw/metadata and flat regressions remain
gates. Diagnostic logging/profiling is separate from acceptance measurements.
Do not interpret overlapping concurrent hold timers as exclusive occupancy.
Those broader acceptance requirements are deferred for the current experiment.
Its scope is one minimal smoke per available arm, then WRITE/READBACK at
1/16/32 workers in opposite arm orders using Redis and RustFS. The requested
arms are native, entity over frozen native, and entity over packed-v3; a missing
arm is explicitly skipped with no substitute.

The [mount checkpoint](../plans/2026-10-20-overlay-entity-mount.md) records all
24 completed available phase rows, zero workload errors, two samples per cell
and clean smoke/teardown evidence. Preliminary packed-overlay WRITE medians
at 1/16/32 workers were 20.08/111.90/135.00 files/s, versus native
51.63/351.03/419.83. READBACK medians were 32.04/173.05/199.05, versus native
113.40/602.50/616.47. `overlay-native` was skipped because its entity-compatible
native lower adapter, binding and mount bootstrap are missing. These results
measure newly created upper bytes; they are not lower-read/copy-up or
acceptance results and do not establish a win or removal of the write plateau.

Historical **r31**, not target-design, overlay/native ratios are:

| Workers | Stat | Full read | Write | Readback | Mixed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 1.00 | 0.71 | 0.86 | 1.06 | 1.02 |
| 16 | Unverified | Unverified | 0.63 | 0.91 | 0.85 |
| 32 | Unverified | Unverified | 0.57 | 0.88 | 0.87 |

The completed 16/32-worker WRITE means were 216.892/236.892 overlay versus
344.005/414.744 native files/s (six runs per arm/worker). The final all-mode
sweep was interrupted; its partial stat/full rows are not completed acceptance.
Known baseline feature-test failures and flaky/source/operator checks remain
documented in the handoff. Neither those historical gates nor the r32 partial
gate validate this target design. **The entity measurements above are preliminary;
there is no target-design performance acceptance result.**

## 17. Remaining tradeoffs and delivery boundary

Current chunk maps trade sequence-history read merging for map replacement cost and
potential fragmentation limits. Same-inode and same-parent contention remains
legitimate; plain allocators and Redis's single-threaded script execution still
have finite capacity. Shared-object refcounts introduce object-local contention
when many entities share the same object. Lower pins retain whole lower
ownership until the last owner is gone, which can delay reclamation of bytes
unused by a particular workspace.

Offline migration is deferred and has no implementation in this stage. Live
namespaces retain retired bytes until quiescent
maintenance/discard, which deliberately trades reclamation latency for a simple
read path. Complete quiescence and crash-safe owner accounting are not qualified
for production.
The target deliberately gives no cheap arbitrary-workspace incremental-diff or
total-order-replay contract and removes live mutable-layer digest identity.
Upper-only diff candidates remain available. These are explicit tradeoffs;
they do not assert that the shipped layout was a complete replay log.

The stage 2 experiment adds separately dispatched `workspace-entity-v1`
namespace and file APIs. Packed ownership joins the existing collector census;
entity writes use current maps and strict durable uploads, and truncate has a
replayable inode-local intent. The experimental VFS handle path keeps entity
handles out of native inode indexes and rejects unsupported handle operations.
Only the explicit fresh-fixture benchmark opt-in can select the dedicated
entity mount adapter. Production mounts remain disabled. External
crash-takeover authority, enforced backend/owner pairing, production pagination,
full mounted/POSIX qualification and conversion remain known limitations.
These correctness gates are not performance acceptance.
