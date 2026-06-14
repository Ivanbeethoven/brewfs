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

BrewFS is not a JuiceFS fork, but JuiceFS is the main production-grade reference point used for performance comparison and gap analysis. Current tuning work compares metadata caching, read/write cache behavior, writeback semantics, object-store amplification, compaction, and test coverage against JuiceFS so regressions and improvements can be measured against a mature baseline.

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

See [doc/operations/configuration.md](doc/operations/configuration.md) and the files under [examples/](examples/) for the full configuration surface.

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
- [doc/testing/docker-compose-test-guide.md](doc/testing/docker-compose-test-guide.md)
- [doc/testing/bench.md](doc/testing/bench.md)
- [doc/testing/fuzz_testing_guide.md](doc/testing/fuzz_testing_guide.md)

## JuiceFS Comparison

BrewFS tracks JuiceFS as a practical benchmark for distributed filesystem semantics and object-storage performance. The comparison is organized around three document sets:

- [JuiceFS internals notes](doc/juicefs/README.md): architecture, read/write paths, cache system, transactions, and slice compaction.
- [BrewFS/JuiceFS gap analysis](doc/gap/README.md): module-by-module gaps and iteration roadmap.
- [Performance roadmap](doc/performance/perf-optimization-roadmap.md): current tuning targets and validation expectations.

Use these notes to understand where BrewFS intentionally differs from JuiceFS, where it is still catching up, and which benchmark scenarios are used to guard against regressions.

### Latest Local Perf Snapshot

The following snapshot was collected on 2026-06-14 with the Docker perf runners under `docker/compose-xfstests/`:

```bash
PERF_FIO_DIRECT=1 PERF_FIO_RUNTIME=20 PERF_FIO_SIZE=512m \
  bash docker/compose-xfstests/run_redis_perf.sh --s3 --writeback-throughput-profile \
  --tools "fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw"

PERF_FIO_DIRECT=1 PERF_FIO_RUNTIME=20 PERF_FIO_SIZE=512m \
  bash docker/compose-xfstests/run_juicefs_perf.sh --writeback-throughput-profile \
  --tools "fio-bigwrite fio-bigread fio-seqread fio-seqwrite fio-randread fio-randwrite fio-randrw"
```

Artifacts:

- BrewFS: `docker/compose-xfstests/artifacts/perf-run-1781450598-12940`
- JuiceFS: `docker/compose-xfstests/artifacts/juicefs-perf-run-1781451380-31093`

| Workload | BrewFS | JuiceFS | Comparison |
| --- | --- | --- | --- |
| `fio-bigwrite` | pass; W 183.04 MiB/s; W p99 18.743 ms; wall 23s | pass; W 3.59 GiB/s; W p99 0.010 ms; wall 1s | BrewFS is about 0.05x JuiceFS foreground write bandwidth. |
| `fio-bigread` | pass; R 1.00 GiB/s; R p99 0.114 ms; wall 5s | pass; R 2.77 GiB/s; R p99 0.070 ms; wall 2s | BrewFS is about 0.36x JuiceFS big-read bandwidth. |
| `fio-seqread` | pass; R 3.37 GiB/s; R p99 0.005 ms; wall 20s | pass; R 3.47 GiB/s; R p99 0.002 ms; wall 21s | Sequential read is near parity, about 0.97x JuiceFS. |
| `fio-seqwrite` | pass; W 89.28 MiB/s; W p99 27.132 ms; wall 52s | pass; W 591.70 MiB/s; W p99 0.006 ms; wall 74s | BrewFS is about 0.15x JuiceFS sequential-write bandwidth. |
| `fio-randread` | pass; R 3.56 GiB/s; R p99 0.012 ms; wall 20s | fail(4); FIO read `Input/output error`; wall 13s | Not a fair bandwidth comparison for this run. |
| `fio-randwrite` | pass; W 67.16 MiB/s; W p99 87.556 ms; wall 130s | pass; W 668.70 MiB/s; W p99 0.007 ms; wall 78s | BrewFS is about 0.10x JuiceFS random-write bandwidth. |
| `fio-randrw` | pass; R 125.44 MiB/s, W 55.32 MiB/s; R/W p99 0.008/87.556 ms; wall 144s | fail(4); FIO read `Input/output error`; wall 14s | Not a fair bandwidth comparison for this run. |

Interpretation:

- `PERF_FIO_DIRECT=1` reduces Linux page-cache effects, but BrewFS read rows still show high internal cache hit rates in the generated report. Treat read bandwidth as a cache-aware regression signal, not a cold-object-store maximum.
- JuiceFS writeback completes foreground writes much faster in this profile. BrewFS still pays a large writeback/upload tail in random-write and mixed random-read/write workloads, which is the main remaining gap shown by this snapshot.
- The JuiceFS `fio-randread` and `fio-randrw` rows failed with FIO read `Input/output error` after prefill/remount, so those rows are recorded as runner results rather than used as successful comparative throughput numbers.

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

Start with the [documentation index](doc/README.md).

Common entry points:

- [Configuration](doc/operations/configuration.md)
- [Architecture](doc/architecture/arch.md)
- [VFS internals](doc/vfs/README.md)
- [Testing and CI guides](doc/README.md#testing-and-ci)
- [Performance and JuiceFS comparison](doc/README.md#performance-and-juicefs-comparison)
- [JuiceFS internals notes](doc/juicefs/README.md)
- [BrewFS/JuiceFS gap analysis](doc/gap/README.md)
- [Control plane](doc/operations/control-plane.md)

## Repository Map

- `src/`: core filesystem, metadata, chunk, object backend, FUSE, and CLI code.
- `examples/`: SDK, S3, persistence, and local mount examples.
- `doc/`: canonical design notes, operations guides, performance plans, tests, and debugging notes.
- `docker/`: compose stacks, xfstests/LTP/perf runners, and runtime image tooling.
- `tests/`: integration and native stress tests.
- `operator/`: Kubernetes operator prototype and CRD documentation.
- `tools/`: performance and stats helpers.

## Contributing

Issues and PRs are welcome. For larger changes, prefer keeping implementation, tests, and documentation updates together so backend capabilities and operational guidance stay in sync.
