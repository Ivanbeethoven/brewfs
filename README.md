<div align="center">
  <img src="doc/icon.png" alt="BrewFS icon" width="96" height="96" />
</div>

<h1 align="center">BrewFS</h1>
<p align="center"><strong>High-performance Rust and layer-aware distributed filesystem</strong></p>
<p align="center"><a href="README.md"><b>English</b></a> | <a href="README_CN.md">中文</a></p>

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)

BrewFS is a Rust filesystem for container, AI, and object-storage-heavy workloads. It exposes a POSIX-like FUSE interface, stores file data as chunked objects, and keeps namespace and slice metadata in a pluggable transactional metadata backend.

The core design goal is to decouple compute from storage: applications read and write normal paths, while BrewFS handles chunk layout, object IO, caching, metadata transactions, compaction, and garbage collection.

## Architecture

Main layers:

- `fuse` and `vfs`: inode-based FUSE integration and POSIX-facing behavior.
- `meta`: metadata client, transaction-capable backends, sessions, control plane, compaction hooks, and GC metadata.
- `chunk`: chunk/block layout, read/write path, cache, compaction, delayed deletion, and block-store GC.
- `cadapter`: object backend abstraction with LocalFS and S3-compatible implementations.
- `fs` and SDK examples: path-based API and local examples that can run without FUSE.

Default data layout:

- Chunk size: 64 MiB
- Block size: 4 MiB
- Object granularity: blocks addressed under slice IDs

## Current Capabilities

Data backends:

- `local-fs`: stores object data in a local directory for development and tests.
- `s3`: supports AWS S3 and S3-compatible services such as RustFS, MinIO, and Ceph RGW.

Metadata backends:

- `sqlx`: SQLite for local/dev and PostgreSQL for server deployments.
- `redis`: low-latency metadata operations with Lua/CAS based chunk updates.
- `etcd`: distributed KV metadata with transaction and watch-oriented semantics.
- `tikv`: transactional TiKV metadata backend with namespace, file data, hardlinks, symlinks, rename exchange, compaction hooks, and delayed/uncommitted slice GC support.

Filesystem and runtime features:

- FUSE mount via `brewfs mount`
- Path/inode operations for create, mkdir, readdir, stat, read, write, truncate, unlink, rmdir, rename, hardlink, and symlink
- Chunked sparse IO with zero-fill for holes
- Read/write cache with memory and SSD budgets
- Optional S3 writeback mode (`commit_before_upload`) with orphan cleanup support
- Slice compaction and two-phase block deletion
- Runtime control plane for `brewfs info` and `brewfs gc`

## Performance Snapshot

Current Redis + S3-compatible RustFS perf runs use `fio` with `io_uring`, `iodepth=1`, buffered IO, 4 MiB blocks, BrewFS writeback throughput profile, no compression, 6 FUSE workers, 4 GiB read/write SSD cache budgets, full cache checksum verification, and a 1s/65k open metadata cache. JuiceFS is v1.3.1 with writeback, 8192 MiB buffer, 4096 MiB cache, 4 uploads, and the same open-cache limit.

Artifacts:

- BrewFS accepted full run: `docker/compose-xfstests/artifacts/perf-run-1781737544-9539`
- BrewFS latest full gate, rejected and reverted: `docker/compose-xfstests/artifacts/perf-run-1781762455-1310`
- JuiceFS latest same-profile full run: `docker/compose-xfstests/artifacts/juicefs-perf-run-1781760859-15737`
- JuiceFS clean planning target: `docker/compose-xfstests/artifacts/juicefs-perf-run-1781732616-8549`

These artifacts include `perf-profile.env`, `runner-console.log`, and `runner-warning-summary.tsv`, which record the effective fio/filesystem tuning and flag noisy runs. The latest JuiceFS run completed with the aligned fio profile, but its writeback/cache path produced 3801 `WARNING` lines, 3772 timeout matches, 16 slow requests, and 5 slow operations, so the clean JuiceFS artifact remains the stable planning target for write-sensitive decisions. The tables below keep the accepted BrewFS snapshot visible while also recording the latest rejected full gate.

| fio tool | BrewFS accepted MiB/s | BrewFS latest gate MiB/s | JuiceFS latest MiB/s | Latest / JuiceFS |
| --- | ---: | ---: | ---: | ---: |
| `fio-bigread` | R 628.2 / W 0.0 | R 691.0 / W 0.0 | R 2438.1 / W 0.0 | 28.3% |
| `fio-bigwrite` | R 0.0 / W 1149.3 | R 0.0 / W 1224.9 | R 0.0 / W 3170.3 | 38.6% |
| `fio-seqread` | R 1754.0 / W 0.0 | R 1749.5 / W 0.0 | R 2487.1 / W 0.0 | 70.3% |
| `fio-seqwrite` | R 0.0 / W 69.2 | R 0.0 / W 68.4 | R 0.0 / W 230.8 | 29.6% |
| `fio-randread` | R 774.0 / W 0.0 | R 741.6 / W 0.0 | R 3292.9 / W 0.0 | 22.5% |
| `fio-randwrite` | R 0.0 / W 73.3 | R 0.0 / W 88.7 | R 0.0 / W 277.2 | 32.0% |
| `fio-randrw` | R 253.4 / W 113.8 | R 177.6 / W 79.6 | R 169.9 / W 77.4 | R 104.5% / W 102.8% |

| metadata op | BrewFS accepted ops/s | BrewFS latest gate ops/s | JuiceFS latest ops/s | Latest / JuiceFS |
| --- | ---: | ---: | ---: | ---: |
| create | 629.9 | 612.9 | 1344.2 | 45.6% |
| open | 9271.0 | 9702.0 | 23511.7 | 41.3% |
| stat | 1022440.1 | 1019757.7 | 1015739.5 | 100.4% |
| readdir | 64070.5 | 63883.9 | 66787.2 | 95.7% |
| rename | 1903.7 | 1890.8 | 2727.0 | 69.3% |

Current interpretation: BrewFS is near parity for `stat` and `readdir`, competitive on noisy `randrw`, but still trails JuiceFS heavily on random/cold reads, pure writes, `create`, `open`, and `rename`. The latest rejected gate tested a hot open-file cache write-skip candidate; it did not improve the target `open` path versus the same-round BrewFS comparison (`9847.8 -> 9702.0` ops/s), so the runtime code was reverted. The next tuning rounds focus on metadata-only commandstats gates and writeback partial-tail aggregation while preserving the full scenario regression budget.

Latest rejected full gate: `docker/compose-xfstests/artifacts/perf-run-1781762455-1310` tested skipping unchanged open-file cache attr rewrites. The focused TDD test passed, and the full suite improved some fio write-heavy numbers versus the immediately prior same-profile BrewFS run (`randwrite` `68.9 -> 88.7` MiB/s, `randrw` `R 157.2 / W 70.5 -> R 177.6 / W 79.6` MiB/s), but the target `open` metric regressed by 1.5% (`9847.8 -> 9702.0` ops/s). Because the target did not improve by at least 5%, the candidate was rejected and reverted.

Latest rejected runtime candidate: `docker/compose-xfstests/artifacts/perf-run-1781752756-6134` tested the current dirty writer/overlay and metadata correctness worktree against the full profile. It improved `open` (`9271.0 -> 9737.8` ops/s) and `randwrite` (`73.3 -> 90.5` MiB/s), but regressed `randrw` to `R 202.2 / W 91.0` MiB/s, about 80% of the kept BrewFS baseline, and `randread` fell to 94.8%. The runtime candidate was therefore not committed as a performance optimization. Earlier rejected A/B `docker/compose-xfstests/artifacts/perf-run-1781750066-14640` added an additive lower metadata create fast path that returned fresh attrs from Redis/SQLite create. The full run showed only a small `create` gain (`629.9 -> 671.5` ops/s) and lower `randrw` (`R 195.2 / W 87.7` MiB/s). Follow-up on/off A/B runs (`perf-run-1781751169-26848` with the candidate, `perf-run-1781751855-18732` reverted) showed the same metadata numbers without the candidate, so the code change was not a reliable cause of improvement and was reverted. Earlier rejected A/B `docker/compose-xfstests/artifacts/perf-run-1781748154-9951` changed FUSE `create` to use an attrs-at-create metadata fast path for new regular files. It fixed a setgid inheritance test locally, but it did not improve the target `create` path (`create` 629.9 -> 625.6 ops/s) and regressed `randrw` to R 177.2 / W 79.5 MiB/s, so the code change was reverted. Earlier rejected A/B `docker/compose-xfstests/artifacts/perf-run-1781745250-11404` seeded the open-file metadata cache from `open_with_cached_attr`; it regressed `create`, `open`, `randread`, and `randrw`. Earlier rejected A/B `docker/compose-xfstests/artifacts/perf-run-1781741772-12024` removed the post-rename eager preload in `MetaClient::rename`; it improved `rename` only by 0.6% and regressed read/mixed workloads. Earlier rejected A/B `docker/compose-xfstests/artifacts/perf-run-1781739942-2326` disabled BrewFS cache checksum verification; it improved `bigread` but regressed `randrw` and `create`, so the main snapshot keeps full checksum verification enabled.

## Quick Start

Requirements:

- Rust 1.85+ (the crate uses Rust 2024 edition)
- Linux for FUSE mounting
- `fuse3` / `fusermount3` for unprivileged mounts

Run the SDK demo without FUSE:

```bash
cargo run -p brewfs --example sdk_demo -- /tmp/brewfs-sdk-data
```

Build the CLI:

```bash
cargo build -p brewfs --release
```

BrewFS defaults to the `io_uring` FUSE runtime. Build the Tokio FUSE runtime with:

```bash
cargo build -p brewfs --release --no-default-features --features fuse-tokio-runtime
```

Mount with local object storage and SQLite metadata:

```bash
mkdir -p /tmp/brewfs-mnt /tmp/brewfs-data

cargo run -p brewfs -- mount /tmp/brewfs-mnt \
  --data-backend local-fs \
  --data-dir /tmp/brewfs-data \
  --meta-backend sqlx \
  --meta-url sqlite:///tmp/brewfs-meta.db
```

Unmount with `Ctrl+C` in the mount process, or use `fusermount3 -u /tmp/brewfs-mnt` if needed.

## Configuration

BrewFS can be configured with CLI flags, a YAML file, or both. CLI flags override YAML values.

Minimal YAML:

```yaml
mount_point: /tmp/brewfs

data:
  backend: local-fs
  localfs:
    data_dir: ./data

meta:
  backend: sqlx
  sqlx:
    url: "sqlite::memory:"

layout:
  chunk_size: 67108864
  block_size: 4194304
```

S3 plus Redis example:

```yaml
mount_point: /mnt/brewfs

data:
  backend: s3
  s3:
    bucket: brewfs-data
    endpoint: http://127.0.0.1:9000
    region: us-east-1
    force_path_style: true
    disable_payload_checksum: true
    part_size: 16777216
    max_concurrency: 16

meta:
  backend: redis
  redis:
    url: "redis://127.0.0.1:6379/0"

cache:
  root: /var/cache/brewfs
  writeback_mode: upload_before_commit
```

TiKV metadata example:

```yaml
mount_point: /mnt/brewfs

meta:
  backend: tikv
  tikv:
    pd_endpoints:
      - 127.0.0.1:2379
    namespace: tenant-a
```

See [doc/configuration.md](doc/configuration.md) and the files under [examples/](examples/) for the full configuration surface.

## CLI

```bash
brewfs mount [OPTIONS] [MOUNT_POINT]
brewfs info [MOUNT_POINT]
brewfs gc [MOUNT_POINT] [--dry-run]
```

Useful mount options:

- `--config <FILE>`: YAML configuration file.
- `--data-backend <local-fs|s3>`: object data backend.
- `--meta-backend <sqlx|redis|etcd|tikv>`: metadata backend.
- `--chunk-size <BYTES>` and `--block-size <BYTES>`: data layout tuning.
- `--fuse-workers <N>`: `0` or `1` uses low-overhead asyncfuse session dispatch; values above `1` enable the worker pool.
- `--fuse-max-background <N>`: maximum queued and running FUSE requests.
- `--privileged`: use `/dev/fuse` directly instead of `fusermount3`.

## Testing

Fast local checks:

```bash
cargo check -p brewfs
cargo test -p brewfs
```

Focused checks used often during backend work:

```bash
cargo test -p brewfs meta::stores::tikv --lib
cargo test -p brewfs mount_config --bin brewfs
```

Docker-based filesystem tests:

```bash
cd docker
bash compose-xfstests/run_redis_xfstests.sh --cases "generic/001"
bash compose-xfstests/run_redis_xfstests.sh --s3 --cases "generic/001"
```

More test and benchmark entry points:

- [docker/README.md](docker/README.md)
- [doc/docker-compose-test-guide.md](doc/docker-compose-test-guide.md)
- [doc/bench.md](doc/bench.md)
- [doc/fuzz_testing_guide.md](doc/fuzz_testing_guide.md)

## Feature Flags

```bash
cargo build -p brewfs --release --features jemalloc
cargo build -p brewfs --release --features profiling
cargo build -p brewfs --release --features rkyv-serialization
```

Available features:

- `jemalloc`: use jemalloc as the global allocator on Linux.
- `jemalloc-profiling`: enable jemalloc heap profiling support.
- `profiling`: enable tracing flamegraph, Chrome trace, and tokio-console integrations.
- `rkyv-serialization`: enable rkyv-based metadata serialization support.

## Documentation

Start here:

- [Configuration](doc/configuration.md)
- [Architecture](doc/arch.md)
- [Metadata](doc/meta.md)
- [Chunk layout](doc/chunk.md)
- [Read path](doc/read-path.md)
- [Write path](doc/write-path.md)
- [Caching](doc/caching.md)
- [Compaction and GC](doc/compaction-gc.md)
- [Observability](doc/observability.md)
- [SDK](doc/sdk.md)
- [Control plane](docs/control-plane.md)
- [BrewFS vs JuiceFS analysis](doc/brewfs-vs-juicefs-analysis.md)

## Repository Map

- `src/`: core filesystem, metadata, chunk, object backend, FUSE, and CLI code.
- `examples/`: SDK, S3, persistence, and local mount examples.
- `doc/` and `docs/`: design notes, operations guides, audits, and debugging notes.
- `docker/`: compose stacks, xfstests/LTP/perf runners, and runtime image tooling.
- `tests/`: integration and native stress tests.
- `operator/`: Kubernetes operator prototype and CRD documentation.
- `tools/`: performance and stats helpers.

## Contributing

Issues and PRs are welcome. For larger changes, prefer keeping implementation, tests, and documentation updates together so backend capabilities and operational guidance stay in sync.
