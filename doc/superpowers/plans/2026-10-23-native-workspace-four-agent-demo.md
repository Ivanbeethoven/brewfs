# Four concurrent agents on native workspace forks

## Result and scope

**PASS, 2026-10-23:** four AX tasks ran concurrently on the existing parent and
three fresh native forks through BrewFS CSI. Every task reported Running,
Ready=True and WorkspaceReady=True. All four wrote/fsynced distinct files,
read the inherited seed bytes, and passed SSH byte/hash/isolation assertions.
After deleting the writers and proving their FUSE mounts/processes had gone,
four fresh tasks remounted the same workspace UUIDs. All four passed the same
checks without rewriting data. Both groups of tasks were subsequently deleted.

These are deterministic Python agents, not LLM agents or a performance test.
This verifies separate workspace writers, not multiple independent writable
mounts of one workspace. The parent was suspended and unmounted while the three
forks were created; online fork and packed-lower fork remain outside this test.

The deployment and repairs from the
[two-agent demo](2026-10-23-native-workspace-fork-demo.md) were reused without
new builds, source changes, worker replacements or protected configuration
changes. BrewFS code was `f802748` on `codex/workspace-fork-20261022`; AX was
`4ef115f` on `brewfs-workspace`. Kubernetes namespace: `brewfs-demo`; AX/Substrate
atespace: `fork-demo`; cluster: `fork-demo`. The two existing gVisor workers
were `sandbox-workerpool-7996f86485-dtm59` (`10.42.0.132`) and
`sandbox-workerpool-7996f86485-v8dpp` (`10.42.0.133`).

## Workspace identities and fork boundary

The existing `fork-parent` AX Workspace was reused. Three successful AX
ForkWorkspace RPCs created `fork-multi-b`, `fork-multi-c`, and `fork-multi-d`,
each with its own BrewFSWorkspaceFork, workspace UUID and StorageClass. The
earlier `fork-child` was preserved and is not part of this four-agent run.

| Agent | AX/BrewFS Workspace | Workspace UUID | StorageClass |
| --- | --- | --- | --- |
| ma | `fork-parent` | `075fbcc3-76bf-556c-ab32-8bb9b1107f16` | `brewfs-fork-parent` |
| mb | `fork-multi-b` | `2112dbc1-d165-55d1-b7be-f521758394ce` | `brewfs-brewfs-demo-fork-multi-b` |
| mc | `fork-multi-c` | `776e25fc-48a0-55cc-8861-4743a6eeed57` | `brewfs-brewfs-demo-fork-multi-c` |
| md | `fork-multi-d` | `f30cf94e-7bc9-59f0-970e-9611aca55bb3` | `brewfs-brewfs-demo-fork-multi-d` |

| Fork | Snapshot ID | Final fork phase |
| --- | --- | --- |
| `fork-multi-b` | `7b631232-04e0-5cb1-b486-79261b69c0d4` | Ready |
| `fork-multi-c` | `879fe385-10af-5e1b-8101-9c7fd8ad9ce3` | Ready |
| `fork-multi-d` | `43461cd9-5b58-5e15-b439-154d77e38b45` | Ready |

All actor external-volume records matched `brewfs://fork-demo/<workspace UUID>`
and `volumeType: csi.brewfs.io`. The parent was reactivated after all three
forks returned. All four workspaces were Active after cleanup, with the three
fork CRs still Ready.

Both existing parent files, `agent-a.txt` and `parent-only.txt`, predate these
three forks and were inherited by all three. New files use `ma/mb/mc/md` to
avoid overwriting the previous demo's `agent-a.txt`.

## Concurrent write and SSH evidence

All writers reached both readiness conditions at `04:56:21.602518Z` UTC before
their `/tmp/multiagent-go` barriers were released through four concurrent
`ax ssh` calls. Each command wrote only its own file, flushed and fsynced it,
verified its bytes and the seeds, then stayed running. A second readiness
collection confirmed all four remained Ready concurrently. SSH independently
checked contents, hashes, and sibling-file absence after barrier release.

| Task | AX task ID | Actor UID | Worker IP |
| --- | --- | --- | --- |
| `fork-agent-ma` | `task-fork-agent-ma-1792731379` | `3709147b-d999-4c1b-bd75-b7b46b57df77` | `10.42.0.133` |
| `fork-agent-mb` | `task-fork-agent-mb-1792731379` | `66438b28-d8af-46cd-a2ce-7b871df2a484` | `10.42.0.132` |
| `fork-agent-mc` | `task-fork-agent-mc-1792731380` | `bdc71a3e-1eb9-4a85-a7b0-f41af00ba482` | `10.42.0.132` |
| `fork-agent-md` | `task-fork-agent-md-1792731380` | `36e906dd-b21b-4a5f-ad6a-28b6714f79dd` | `10.42.0.133` |

The guest logs contain these actual output messages, distinct from the runner
log that prints the command text:

```text
2026-10-23T04:56:22.455698478Z WRITE_PASS ma
2026-10-23T04:56:22.382214237Z WRITE_PASS mb
2026-10-23T04:56:22.455772592Z WRITE_PASS mc
2026-10-23T04:56:22.417403650Z WRITE_PASS md
```

Every string below ends with one newline. Task stdout and independent SSH
readback agreed on every byte and hash, before and after remount.

| File | Contents without final newline | SHA-256 |
| --- | --- | --- |
| `agent-a.txt` (all four) | `agent A: parent initial state` | `6ceeb2da0f21d978ba0f7930a78b3b8a068317c0f4f7a8013791a285d219c9cc` |
| `parent-only.txt` (all four) | `agent A: written after fork` | `e58b1728fe9bbc653232384b053a94b53c2b15986c7db9b2f8f573ff1857c616` |
| `agent-ma.txt` (parent only) | `multiagent native fork 2026-10-23: ma` | `ab001fe82d15f30e0239f61c9946f92af115431583224b38fafd66b14f1c06f7` |
| `agent-mb.txt` (b only) | `multiagent native fork 2026-10-23: mb` | `3d8820fc2b47e9f265bd415e6cc51ad920a0e0194ba7611e18abc450d1f7b02f` |
| `agent-mc.txt` (c only) | `multiagent native fork 2026-10-23: mc` | `a1137e46cbd2e58f24ffa8a6c3fc5e92fe2f81afa1b395ef4cdc5cf2fa8a19d5` |
| `agent-md.txt` (d only) | `multiagent native fork 2026-10-23: md` | `e64b9d3b1bca8c45a156988dfd935ef4bea79debac822624c0ff7195ebd95e99` |

For each agent, all three other new file names were asserted absent. Thus each
phase passed 12 cross-workspace absence assertions, as well as exact-byte
checks of all eight seed copies and all four new files. Child writes left the
parent's seed bytes intact and introduced no child files into the parent.
All eight SSH readiness probes returned `ok` and `HTTP 200`.

## Unmount, fresh CSI mounts and persistence

At `04:57:08.325007Z`, after deleting the four writer tasks, checks inside the
CSI node container proved all four state JSON files absent, all four previous
FUSE processes gone, and all recorded shared/actor paths absent from mountinfo.
The output was `FOUR_WORKSPACE_MOUNTS_RELEASED_PASS`.

Only then were four verification tasks created. Their commands opened files
for reading and asserted the table above; they contained no data writes.
All four were Running, Ready=True and WorkspaceReady=True by
`04:57:22.698750Z`, and independent `ax ssh` checks passed for each.

| Verification task | AX task ID | Fresh actor UID |
| --- | --- | --- |
| `fork-agent-ma-verify` | `task-fork-agent-ma-verify-1792731440` | `d278e9bd-a1f7-4f6d-b67c-6437134325c9` |
| `fork-agent-mb-verify` | `task-fork-agent-mb-verify-1792731440` | `9ec28432-152f-488c-8137-0751a3a2cd7a` |
| `fork-agent-mc-verify` | `task-fork-agent-mc-verify-1792731441` | `a88eee0c-3c80-44f4-9306-e67e574fb3f6` |
| `fork-agent-md-verify` | `task-fork-agent-md-verify-1792731442` | `1dade19d-a170-407e-b208-ca1de92a3ea7` |

| Workspace | Writer FUSE PID | Fresh verifier FUSE PID |
| --- | --- | --- |
| parent | 282 | 432 |
| b | 301 | 451 |
| c | 319 | 469 |
| d | 337 | 488 |

PIDs are in the CSI container's PID namespace. New CSI records also had distinct
actor target paths. The comparison printed `FRESH_CSI_MOUNTS_PASS`. This proves
readback after releasing the original mounts/processes, rather than readback
from the writer's live FUSE/page cache. It is not a backend restart or
power-loss test.

```text
2026-10-23T04:57:21.152347738Z VERIFY_PASS ma
2026-10-23T04:57:21.745350636Z VERIFY_PASS mb
2026-10-23T04:57:22.267229451Z VERIFY_PASS mc
2026-10-23T04:57:22.843631870Z VERIFY_PASS md
```

Final cleanup at `04:57:48.956457Z` again produced
`FOUR_WORKSPACE_MOUNTS_RELEASED_PASS`. The `fork-demo` task list was empty;
the set of pre-existing actor UIDs was unchanged. No manual golden-actor
deletion was necessary in this run. Workspaces, fork CRs, StorageClasses and
their data remain for inspection. Operator/CSI, AX, Substrate, registry and
both existing worker pods remain deployed; none were rolled or reconfigured.

## Commands and saved artifacts

The CLI used was `/mnt/brewfs-data/src/fork-demo-20261023/ax`, with
`KUBECONFIG=/etc/rancher/k3s/k3s.yaml`. The complete bounded Python harness and
generated manifests are saved with the raw evidence at
`/mnt/brewfs-data/src/fork-multiagent-20261023/`.
`/tmp/fork-multiagent-progress.md` points to that directory's `progress.md`.
The following are the command shapes actually executed:

```bash
kubectl -n brewfs-demo patch brewfsworkspace fork-parent --type=merge -p '{"spec":{"desiredState":"Suspended"}}'
kubectl -n brewfs-demo wait --for=jsonpath='{.status.phase}'=Suspended brewfsworkspace/fork-parent --timeout=60s
ax -a fork-demo workspace fork fork-parent fork-multi-b
ax -a fork-demo workspace fork fork-parent fork-multi-c
ax -a fork-demo workspace fork fork-parent fork-multi-d
kubectl -n brewfs-demo patch brewfsworkspace fork-parent --type=merge -p '{"spec":{"desiredState":"Active"}}'
python3 /mnt/brewfs-data/src/fork-multiagent-20261023/run.py start write
python3 /mnt/brewfs-data/src/fork-multiagent-20261023/cleanup.py write
python3 /mnt/brewfs-data/src/fork-multiagent-20261023/run.py start verify
python3 /mnt/brewfs-data/src/fork-multiagent-20261023/cleanup.py verify
```

`run.py` uses `ax apply`, `ax resume task`, bounded readiness polling,
`ax ssh <task> -- python3 -c <byte/hash/absence assertions>`,
`ax ssh <task> -- sh -ec 'curl -fsS -w "\nHTTP %{http_code}\n" http://127.0.0.1/readyz?check=workspace'`,
`kubectl ate get actor -o json`, and
`kubectl ate logs actor <task> -a fork-demo -c guest --source containers`.
`cleanup.py` deletes only this run's tasks and checks recorded mount paths and
PIDs. The saved writer harness is run-specific: do not rerun it against these
same seeded workspaces as a fresh-fork test.

Key artifacts: `fork-multi-{b,c,d}-{fork-result.yaml,fork.json,storageclass.json}`;
`fork-agent-m{a,b,c,d}[-verify]-{task.yaml,actor.json,guest.log,ssh-proof.txt,readyz.txt}`;
`{write,verify}-{concurrent-tasks.txt,csi-state.txt,mountinfo.txt,cleanup-proof.txt,actors-after-cleanup.json}`;
`fresh-mount-proof.txt`, `workspaces-after.json`, `pods-after.txt`, `csi-run.log`.
Full per-task guest logs are retained, including the output lines quoted above.

## Limits and evidence validation

No requested check failed. The captured CSI log does contain 56 occurrences of
`brewfs::fuse: statfs failed: unsupported`; these did not prevent the tested
operations, but this run is not a claim of an error-free CSI log or complete
POSIX support. The earlier golden-actor lifecycle limitation remains unqualified.

Artifact validation checked actual guest output records (not just command-text
matches), recomputed every SSH-reported SHA-256, matched logs against readback,
checked readiness conditions and exact volume UUIDs, and proved fresh process
and actor target identities. The documentation diff passed `git diff --check`.
No Rust/Go source changed, so build and unit-test gates were not rerun.
