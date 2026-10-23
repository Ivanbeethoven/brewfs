# Issue #228: bounded entity slice census

Verified statically at `a8459dc`: all three maintenance paths scan the shared
volume `slice/` prefix with a 4,096-row limit before filtering the namespace.
Both Memory and Redis reject the 4,097th matching row. Deleted tombstones remain
in that prefix. Resume also accumulates all owned slices in its final CAS.

The fix enumerates existing slice keys in 128-ID pages up to a captured allocator
watermark. Each page retains only the target namespace's rows and validates the
row against the requested SID. Foreign-only pages do not end the census. The
watermark is captured after maintenance admission, when the exact namespace
authority fences further local allocation; concurrent foreign allocations do
not extend the census. Existing global IDs, reservations, and tombstones need no
migration.

Resume checks one namespace `slice-delete-fence` row in its final transaction.
Reserving deletion writes its fresh reservation UUID to that fence atomically
with the `Available -> Deleting` transition. This is the maintenance transition
that can introduce a resume blocker after an earlier page was checked. Upload
abandonment and deletion completion only remove blockers. The exact descriptor,
lease, and maintenance predicates still fence every mutation. Discard protects
the fence during sweeping and removes it only at terminal namespace deletion.

This is a bounded-memory census, not a namespace index: total work remains
proportional to the allocator watermark, and discard steps restart the census.
It removes the row-count/transaction cliff without changing allocation or adding
an index migration. The separate namespace-prefix scan limit from #186 remains.

Regression coverage includes the three empty-namespace operations with 4,097
foreign reservations, a target Pending slice after those foreign pages, resume
with 4,097 Available/Deleted rows, tombstone retention and SID non-reuse, and
deletion starting after resume has observed an Available row. A test backend
asserts reads of at most 128 keys and final transactions of fewer than 32 rows.

Validation on ax-srv is static only for Rust: cargo, rustc, and rustfmt are absent.
The initial regression command failed to start (`cargo: command not found`), so
neither a runtime red result nor a passing Rust result is claimed. `git diff
--check`, the four perf-runner shell syntax checks, and the three perf-report
shell regression scripts passed.

On brewfs, run the full local gate in `AGENTS.md` and the Rust jobs in
`.github/workflows/ci.yml`, including formatting, build/check, FUSE feature
checks, workspace/default and all-feature tests, and clippy. The new tests
require `workspace-overlay`; run them explicitly as well:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test -p brewfs --features workspace-overlay --lib workspace_overlay::entity::
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --lib --bins
```

Also run the ignored entity Redis backend/ownership/recovery contracts against
an isolated Redis using `BREWFS_TEST_REDIS_URL`. No object or cluster test was
performed on ax-srv, and no performance measurements are claimed.
