# Canonical spec and evidence consolidation

Date: 2026-10-20. Documentation-only branch:
`codex/docs-spec-consolidation-20261020`, based on
`cfa000af39414e14f0e0149181b92da497c1a8fb`. Worktree:
`/mnt/brewfs-data/src/brewfs-docs-spec`. The active performance worktree and its
uncommitted implementation/spec changes were not copied, edited, or merged.
The consolidation task performs no merge into another branch; any later
integration is a separate owner-directed action.

## Canonical specifications

- [packed-meta](../specs/packed-meta.md): current v3 format/read contract and
  dated evidence ledger, plus concise historical v1/v2/large-directory rationale.
- [overlay-workspace](../specs/overlay-workspace.md): current entity implementation
  contract and the old-format operator lifecycle reference. The latter does not
  impose head/CAS machinery or admission work on the entity experiment.

No requirements or acceptance results were added. Dynamic-block work remains
deferred and has no separate spec. The three unrelated web-console,
pjdfstest-compose and Aliyun native managed-backend specs are byte-identical to
the branch base. The mapping below lists all eight removed source paths; exact
originals remain in the pinned Git history linked by the canonical specs.

| Removed path under `doc/superpowers/specs/` | Canonical destination | Preserved material |
| --- | --- | --- |
| `2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md` | [packed-meta.md](../specs/packed-meta.md) | V3 contract and historical ledger |
| `2026-09-22-brewfs-packed-metadata-large-directory.md` | [packed-meta.md](../specs/packed-meta.md) | Historical large-directory design |
| `2026-08-24-brewfs-frozen-metadata-image-v1.md` | [packed-meta.md](../specs/packed-meta.md) | Historical frozen image v1 |
| `2026-08-24-brewfs-compact-layer-pack-format-v1.md` | [packed-meta.md](../specs/packed-meta.md) | Historical compact delta pack v1 |
| `2026-08-24-brewfs-memory-s3-readonly-layer-metadata-spec.md` | [packed-meta.md](../specs/packed-meta.md) | Historical memory/S3 architecture |
| `2026-08-24-brewfs-clustered-frozen-metadata-v2.md` | [packed-meta.md](../specs/packed-meta.md) | Historical clustered v2 |
| `2026-08-23-brewfs-workspace-overlay-implementation-spec.md` | [overlay-workspace.md](../specs/overlay-workspace.md) | Current entity contract |
| `2026-09-03-brewfs-workspace-operator-lifecycle-spec.md` | [overlay-workspace.md](../specs/overlay-workspace.md) | Old-format operator reference |

## Performance evidence retention

The audit matched all performance filenames against the canonical specs and
architecture documents before deleting anything. All ten directly cited
correctness reports are retained, including historically limited evidence.
Standalone measurements, analysis, test design and a rejected optimization
report are also retained. Per-round/per-gate reports absent from those acceptance
citations were removed from the working tree; their historical references use
immutable links to files verified to exist in the branch base. Raw artifacts
and benchmark outputs were not deleted.

### Kept files

All paths below are relative to `doc/performance/` (20 files, including README).

- [README.md](../../performance/README.md)
- [aliyun-packed-v3-100k-cold-read-test-plan-2026-09-28.md](../../performance/aliyun-packed-v3-100k-cold-read-test-plan-2026-09-28.md)
- [aliyun-packed-v3-cold-read-2026-09-28.md](../../performance/aliyun-packed-v3-cold-read-2026-09-28.md)
- [aliyun-packed-v3-vs-juicefs-triple-1m-2026-10-01.md](../../performance/aliyun-packed-v3-vs-juicefs-triple-1m-2026-10-01.md)
- [aliyun-packed-vs-juicefs-smallfiles-2026-09-26.md](../../performance/aliyun-packed-vs-juicefs-smallfiles-2026-09-26.md)
- [native-packed-base-large-scale-validation-2026-09-24.md](../../performance/native-packed-base-large-scale-validation-2026-09-24.md)
- [packed-v3-cold-hardlink-validation-2026-10-03.md](../../performance/packed-v3-cold-hardlink-validation-2026-10-03.md)
- [packed-v3-completion-correctness-2026-10-02.md](../../performance/packed-v3-completion-correctness-2026-10-02.md)
- [packed-v3-decoded-frame-cache-local-2026-10-01.md](../../performance/packed-v3-decoded-frame-cache-local-2026-10-01.md)
- [packed-v3-external-validation-2026-10-04.md](../../performance/packed-v3-external-validation-2026-10-04.md)
- [packed-v3-frozen-source-seek-validation-2026-10-04.md](../../performance/packed-v3-frozen-source-seek-validation-2026-10-04.md)
- [packed-v3-index-budget-candidate-2026-10-02.md](../../performance/packed-v3-index-budget-candidate-2026-10-02.md)
- [packed-v3-metadata-cache-analysis-2026-09-29.md](../../performance/packed-v3-metadata-cache-analysis-2026-09-29.md)
- [packed-v3-metadata-closeout-validation-2026-10-07.md](../../performance/packed-v3-metadata-closeout-validation-2026-10-07.md)
- [packed-v3-namespace-posix-validation-2026-10-04.md](../../performance/packed-v3-namespace-posix-validation-2026-10-04.md)
- [packed-v3-namespace-validation-2026-10-04.md](../../performance/packed-v3-namespace-validation-2026-10-04.md)
- [packed-v3-pagination-errno-validation-2026-10-04.md](../../performance/packed-v3-pagination-errno-validation-2026-10-04.md)
- [packed-v3-source-batch-validation-2026-10-04.md](../../performance/packed-v3-source-batch-validation-2026-10-04.md)
- [packed-v3-source-validation-2026-10-03.md](../../performance/packed-v3-source-validation-2026-10-03.md)
- [packed-v3-vs-juicefs-tikv-metadata-stat-2026-09-30.md](../../performance/packed-v3-vs-juicefs-tikv-metadata-stat-2026-09-30.md)

### Deleted files

All paths below are relative to `doc/performance/` (27 files). Each link opens
the exact removed file at the consolidation base. These historical checkpoints
are not current acceptance evidence.

- [packed-v3-acl-validation-2026-10-04.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-acl-validation-2026-10-04.md)
- [packed-v3-carrier-fork-bootstrap-validation-2026-10-08.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-carrier-fork-bootstrap-validation-2026-10-08.md)
- [packed-v3-g06-http-status-terminal-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g06-http-status-terminal-validation-2026-10-06.md)
- [packed-v3-g07-budget-shutdown-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g07-budget-shutdown-validation-2026-10-06.md)
- [packed-v3-g07-g10-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g07-g10-validation-2026-10-06.md)
- [packed-v3-g08-unified-executor-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g08-unified-executor-validation-2026-10-06.md)
- [packed-v3-g09-pipeline-shutdown-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g09-pipeline-shutdown-validation-2026-10-06.md)
- [packed-v3-g09-pipeline-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g09-pipeline-validation-2026-10-06.md)
- [packed-v3-g12-g13-catalog-safety-validation-2026-10-07.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g12-g13-catalog-safety-validation-2026-10-07.md)
- [packed-v3-g12-history-anchor-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g12-history-anchor-validation-2026-10-06.md)
- [packed-v3-g12-publication-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g12-publication-validation-2026-10-06.md)
- [packed-v3-g13-binding-root-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g13-binding-root-validation-2026-10-06.md)
- [packed-v3-g14-capability-status-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g14-capability-status-validation-2026-10-06.md)
- [packed-v3-g15-g17-runner-manifest-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g15-g17-runner-manifest-validation-2026-10-06.md)
- [packed-v3-g16-resource-journal-validation-2026-10-10.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g16-resource-journal-validation-2026-10-10.md)
- [packed-v3-g16-runner-policy-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-g16-runner-policy-validation-2026-10-06.md)
- [packed-v3-gc-grace-fork-validation-2026-10-06.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-gc-grace-fork-validation-2026-10-06.md)
- [packed-v3-metadata-followup-validation-2026-10-08.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-metadata-followup-validation-2026-10-08.md)
- [packed-v3-observer-cancel-progress-2026-10-04.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-observer-cancel-progress-2026-10-04.md)
- [packed-v3-physical-dependency-validation-2026-10-07.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-physical-dependency-validation-2026-10-07.md)
- [packed-v3-plans-admission-validation-2026-10-05.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-plans-admission-validation-2026-10-05.md)
- [packed-v3-pr141-integration-2026-10-08.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-pr141-integration-2026-10-08.md)
- [packed-v3-redis-tikv-naming-validation-2026-10-07.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-redis-tikv-naming-validation-2026-10-07.md)
- [packed-v3-reply-retirement-validation-2026-10-05.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-reply-retirement-validation-2026-10-05.md)
- [packed-v3-sidecar-candidate-validation-2026-10-05.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-sidecar-candidate-validation-2026-10-05.md)
- [packed-v3-source-stat-validation-2026-10-04.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-source-stat-validation-2026-10-04.md)
- [packed-v3-spool-readonly-validation-2026-10-04.md](https://github.com/Ivanbeethoven/brewfs/blob/cfa000af39414e14f0e0149181b92da497c1a8fb/doc/performance/packed-v3-spool-readonly-validation-2026-10-04.md)

## Relative-link and Markdown validation

The repository-wide checker parses CommonMark plus GFM tables. It checks local
inline/reference/image links, HTML href/src targets, repository-root paths,
percent-encoded paths, explicit HTML IDs and GitHub-style heading anchors,
including duplicate anchors. Code blocks and code spans are excluded. External
URLs are classified but not network-validated. Each newly introduced historical
GitHub target was checked with `git cat-file -e` at the pinned base commit.

The initial scan found 91 pre-existing broken local links. Missing native-base
logs and an unavailable cloud artifact are now explicitly labeled historical
paths, not fabricated links or fabricated evidence. The copied JuiceFS README
references use upstream URLs; the native-base testdata traversal and obsolete
documentation-index entries were corrected. A missing shell-fence opener in
the benchmark guide was restored.

Reproducible checker and JSON results are preserved outside the worktree:
`/mnt/brewfs-data/artifacts/docs-spec-consolidation-20261020/`.
The checker requires Python 3 and `markdown-it-py` (the exact version is recorded
in `validation.json`). Run from the docs worktree:

```bash
python3 /mnt/brewfs-data/artifacts/docs-spec-consolidation-20261020/check_relative_links.py --json .
git diff cfa000af39414e14f0e0149181b92da497c1a8fb --check
git diff cfa000af39414e14f0e0149181b92da497c1a8fb --name-only
```

The final local-link check has zero broken targets or fragments. Canonical
specifications and this inventory have valid heading hierarchy and blank-line
separation; fenced blocks and lists parse correctly. Existing optional style
advisories in unrelated historical/vendor documents are not malformed links or
new formatting changes. `git diff --check` is clean. Every changed tracked file
is Markdown, so no Rust build, service, mount or benchmark was run for this
docs-only branch.
