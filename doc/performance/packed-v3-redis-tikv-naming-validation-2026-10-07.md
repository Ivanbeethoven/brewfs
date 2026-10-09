# Packed-v3 naming and real Redis/TiKV metadata validation

The supported product is packed-v3. `wire005` names its encoding module;
005 is not a separate product version. Rust types and APIs now use V3/v3,
metrics use `brewfs_packed_v3_*`, and mount budget variables use
`BREWFS_PACKED_V3_*_BUDGET_BYTES`. Former module/type names have no aliases.
The mount continues to reject 004/v1/v2 and old manifest payloads.

2026-10-08 follow-up: old Windows observer helper scripts and two candidate
test snippets now use V3/v3 names and the existing internal wire005 module.
Both scripts parse without execution. A fresh source/docs/tools and workspace
helper search found no obsolete packed product/module/API identifiers.
Previously captured evidence remains intact; no compatibility alias was added.

The mechanical rename checked identifier collisions and preserved every Rust
byte-string literal. This includes object magic and the frozen inode identity
hash domain. Product naming changes do not change that domain or the encoded
layout. Fixture/temp paths and human-facing labels use V3. Recovery receipts
and historical artifact sources remain preserved outside the checkout.

## Actual metadata backend gates

Root ran the production Redis and TiKV backends against isolated Redis/PD/TiKV
services. Every phase froze source inputs before Cargo and verified them after
completion. No ignored test was counted as a pass without execution.

| Phase | Executed test entry points | Result |
| --- | ---: | --- |
| Public binding/open/publication/inode allocation | 10 | 10 passed |
| Distributed catalog with independent connections | 2 | 2 passed |
| Backend-clock expired-deadline CAS | 2 | 2 passed |
| Redis key-index corruption, no partial inode/ACL write | 1 | 1 passed |
| Destructive revalidation | 2 | 2 passed; seven cases per backend |

The destructive cases cover first packed install after root scan, history-only
root protection, native fork after scan, deletion before actual fork CAS,
missing orphan creation, orphan recreation and native-layer recreation. All
metadata methods delegate to the actual backend. Scheduling wrappers pause
only after real scans or before real CAS calls. Cleanup retires owned tasks,
deletes exact values within fresh UUID namespaces, and verifies logical
emptiness. The root service runner then removes only its exact owned container
IDs and verifies absence. A distinct timeout flag rejects expired cases even
if an abort races with successful task completion.

Artifact:
`C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/redis-tikv-closeout-20261007-attempt02/`.
Each phase has `result.json`, `cargo.log` and before/after source hashes.
`cleanup.json` confirms Redis, PD and TiKV container absence.

Images were pinned by local image SHA. Services were bounded to Redis 96 MiB,
PD 512 MiB and TiKV 2 GiB, with 656 MiB total tmpfs data and bounded logs.
The initial failed TiKV startup and its cleanup are retained separately; the
successful second attempt uses nofile 1048576. No cloud services were created.

## Repository gate and remaining boundary

Gate16 completed against the frozen V3/wire005 source inventory:
`C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/full-rust-gate16-v3-wire005-real-metadata-20261007/`.
All 571 inputs remained unchanged. It ran 49 checks: 48 passed, while strict
overlay Clippy rejected one complex type in the new GC test helper. The fix
extracts a transparent `JobEntries` alias. Supplement01 verifies mechanically
that this is the sole input delta and preserves the exact underlying field
type; production inputs remain identical to Gate16. Supplementary fmt, strict
overlay Clippy and diff checks passed 3/3 against unchanged inputs:
`C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/gate16-v3-clippy-supplement01/`.
The failed lint and its original log remain retained. Gate15 does not certify
these later changes, and Gate16 alone is not described as 49/49 successful.

Default workspace tests passed 1169, overlay library tests passed 1860 plus
eight fixture tests, and all-feature library tests passed 1875. Integration
and doc tests also passed. Operator tests passed 28 and CRD generation matched.

These are small metadata correctness gates. Shared native payload uses the
existing bounded in-memory helper; packed fixtures use the real authenticated
local producer. They do not certify FUSE, object-store/server crash durability,
performance, byte-identical ABA, or unknown remote-commit recovery.

Full graph proof and enforced publication authority, durable journal recovery,
native seal/head rotation, persistent reader/history pins and packed-object
graph GC remain incomplete. SQLite remains a disposable local authentication
ledger, while Redis/TiKV own distributed metadata. The repository-external
journal candidate is uncompiled and unapplied and does not close those gaps.
TiKV commit-error retry/cleanup requires a separate proven fix: pessimistic
rollback does not itself remove already-prewritten 2PC locks, and transport
errors can hide a successful commit. No blind rollback was introduced.
