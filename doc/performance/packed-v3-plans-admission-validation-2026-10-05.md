# packed v3 Plans admission checkpoint — 2026-10-05

Scope remains v3/005 only: PM11/BP11/FD06/IP06. Historical v2/004 compatibility
is not a delivery requirement. The overall SPEC goal, system S and experiment X
remain open; no final campaign or performance win is accepted here.

## Complete CI and frozen programs before this repair

`v3-lifecycle-request02/full-rust-gate04/` passed 49 checks and
`full-rust-supplement03/` passed 17, together preserving all original 52 precise
commands. They used 532 matching frozen inputs; source-manifest file SHA256 is
`5002ae11b838e136413038952e4c7ee7ef2fb07399ee66815ec9c965625cab49`, canonical SHA256
`a4c9ee7f6dc8a2d8fd8749a046af185247d3dac8dc207c014e3ada3446c803a1`.
Both explicit runtime builds and their backups passed identity verification.
This establishes CI/build evidence for that source, not for the repair below.

Actual counts included default lib 1163/225 ignored, overlay lib 1705/237 ignored,
all-features lib 1720/237 ignored, typed-native/observer 1665/237 ignored, stores
69/9 ignored, request 57, operator 24, and migrated CLI lib 8/0 ignored.
The historical packed bin filter ran zero tests and is command coverage only.
The G08 parent checks its exact child success; a separate child stdout artifact
was not saved and is not claimed.

Evidence root is `D:/Codex-Recovery/brewfs-root-20261004/v3-lifecycle-request02/`.
`ci04-preflight-independent01/fullgate-pair-terminal.json` and
`freeze-and-live-independent-review01/freeze-review-terminal.json` record the
independent terminal checks. Failed/aborted earlier gates remain preserved.

## Fresh real FUSE failures

`live-fuse-gate04-supp03-01/io-uring-raw-normal-01/` failed before mounting.
The output-derived mount path was on `/mnt/d` (WSL 9p/Windows mapping), which
unprivileged fusermount3 rejects with magic `0x01021997`. This is an environment
failure, not cache eviction/refetch evidence. Cleanup has daemon exit 1, no
mount/group survivors and no cleanup errors.

The unchanged strict harness and frozen io-uring/raw binary were then run with
fresh ext4 output under `/home/hxy/brewfs-live-fuse-gate04-supp03-01/`.
`io-uring-raw-normal-02` mounted and passed manifest-only startup and the first
stat, but its first inline p00000 pread returned ENOMEM. Kernel READ was unique
390, inode 4, offset 0, size 8192. Session 96175 exited 1 before pressure/refetch,
payload fault recovery, concurrent reads and normal teardown acceptance.

Its failure cleanup exited the daemon with 0, removed the mount, stopped the
sampler/oracle, and left no owned survivors or cleanup errors. A sampler ESRCH
during the failed run is retained; neither memory nor normal FUSE acceptance is
claimed. `normal02-protection.json` verifies the complete failed run and protected
D copy: 10,191 files, 298,300,912 bytes; protection session 89973 exited 0.
The independent failed-run review is
`freeze-and-live-independent-review01/failed-normal01-normal02-review.json`.

## Tests-first repair, currently awaiting complete acceptance

Both manifest preparation entrances reserved min(allocation limit, Plans
capacity, 4 MiB). With strict Plans=4 MiB this occupied the entire pool before
the next index request admitted its separate `2 * key.len() + 512` recipe.
That recipe is required even on a cache hit. The runtime internal stack and
after-failure stats were not captured; the existing API tests below establish
the deterministic behavior without claiming that missing diagnostic evidence.

`constrained-plans-red01/` records four actual exact tests, each running one
test and failing one with Cargo exit 101. Inline read failed with ENOMEM; the
exhaustion/recovery test still failed after its external holder was released;
the shared-placement A-B-A test failed its independent budget admission. The
same-frame test's leader never reached the real body and timed out; that is a
leader preparation failure, not a demonstrated follower-sharing failure.
The controller session 62310 exited 0 after preserving all four failures.

The smallest production change adds one common preparation limit and uses it
in both entrances: leave 512 KiB for independent index/flight recipes, cap a
preparation arena at 2 MiB, and keep the original 1 MiB scratch and final shrink
to actual source/segment/recipe ownership. A minimum supported 2 MiB Plans pool
therefore admits a 1.5 MiB arena. No owner charge or immediate overload rejection
is removed, no wait is added, and strict harness capacities/assertions remain
unchanged. The headroom is not a guarantee for 16 simultaneous preparations.

Tests now cover strict 4 MiB and minimum 2 MiB inline reads and two readers held
on one real payload body, plus exhaustion/recovery and the common placement
entrance. They retain complete data, conservation, actual capacity and zero
unexpected rejection checks. This batch changes one production file and three
test files relative to gate04. Production manifest SHA256 is
`55e0dc2cb281d173b5ded8ba047aeef9ad36bad84f82359f632132397f6bbbcd`.
The six-test `constrained-plans-green01/` controller, session 59317, exited 0.
Each exact command ran one test and passed one, with no ignored tests. Frozen
iteration inputs remained identical. This is focused acceptance only.
The fresh 49-check gate05 controller, session 49068, exited 0. Its terminal
records 49 passed / 0 failed, 532 inputs unchanged, canonical SHA256
`0c81f094632b1573770f207e697a6d8126578bcf46ce0c65ebb4ab6a7454e9f3`;
source-manifest file SHA256 is
`d60d16dd86a619e97bd3f7aa981c54a10038133ef40081d3fa10a44fcf7bb76a`.
Default lib passed 1163 / 225 ignored, overlay lib 1708 / 237 ignored,
all-features lib 1723 / 237 ignored, and operator passed 24;
strict default/overlay lint and CRD comparison passed.
`gate05-supp04-independent01/gate05-terminal01.json` independently checks the
exact 49 commands, actual summaries/named tests, all inputs/configs and Git.
Supplement04 controller session 88026 exited 0 with 17 passed / 0 failed,
the same 532 source inputs/configuration/environment/toolchain, and no drift.
Typed native/observer passed 1668 / 237 ignored and migrated CLI lib passed
8 / 0 ignored. Its historical bin filter remains zero-test command coverage.
`gate05-supp04-independent01/gate05-supp04-terminal01.json` verifies the pair.
The new binding SHA256 is
`3dc839205ba65617ef7ff0b07f978b6298a085fd4a729bb104b73f33fd7f2059`.
The explicit runtime freezer session 49797 exited 0, saving both programs and
their complete protected copy under
`C:/Codex-Recovery/brewfs-root-20261005/v3-plans-repair01/`.
`freeze-and-normal-independent01/freeze-review-terminal01.json` independently
checks both 33-file inventories, compiler features, programs and frozen inputs.

## New real FUSE normal pass and pending-body shutdown failure

Session 48562 ran the unchanged candidate02 io-uring/raw normal runner in fresh
ext4 `/home/hxy/brewfs-live-fuse-gate05-supp04-01/io-uring-raw-normal-01`
and exited 0. All 17 stages completed, including the first inline read, same
physical inode-index object/full-range eviction-refetch, fault recovery,
held-body statistics and normal unmount. RSS grew from 102215680 to 127774720
bytes within the unchanged declared capacity/allowance; scanner memory is
separate. The concurrent stage uses 16 workers over 64 reads and permits explicit
ENOMEM admission refusals: this run completed one read and rejected 63. It is
not evidence that 16 reads can all succeed together. Protection session 58318
exited 0 with 10333 files / 314415939 bytes matching before/copy/after.
Independent normal behavior review remains pending.

The separate strict pending-body SIGTERM wrapper session 55988 exited 1.
It completed the original normal stages again, proved the held HTTP body and
zero delivery, then sent only the daemon SIGTERM. Raw logs record READ unique
12432 cancellation and temporary-handle closure, followed by ordinary
fusermount3 -u EBUSY and daemon exit 1 with the mount still present. The first
poll observed no worker exit yet; its final report/log shows zero bytes and
ENODEV (19), failing its allowed-errno assertion. Do not claim it stayed live
throughout, or infer actual reply-write ordering from the cancellation event.
There is no successful shutdown receipt.

Failure cleanup later released the HTTP hold and externally performed ordinary
unmount. These are failure cleanup only. No mount/process/thread survives and
no cleanup error was recorded. Protection session 33191 exited 0: evidence
440 files / 44557511 bytes plus separate ext4 work 10156 files / 296014368 bytes
all match their protected C copies. The remaining live matrix is pending;
reply delivery, client close, physical unmount, session/reply/ring join and owner
retirement still require a production repair and fresh verification.

## Remaining system work and disk state

G13 grace/fork candidate has a self-contained external v02 with mechanical
application verification; G14 has an independent review requiring stronger
fake transport/domain-error assertions before its RED run. Neither candidate
has Rust compile or behavior acceptance. Packed publication/recovery, graph GC,
operator durable drain/reclamation, remaining G10 contracts and system exits
stay open. `supports_packed_workspace_mount()` remains false.

The user-confirmed deletion was already executed. The old recovery ext4.vhdx
was absent/zero length, so no former image-sized host recovery is claimed.
The active D WSL image, target, source and evidence are preserved. At this
checkpoint host C had about 251 GiB free and D about 26 GiB; Linux virtual free
space is not host free space. Future large artifacts can use C to preserve D
headroom. No further disk deletion is implied by this report.
