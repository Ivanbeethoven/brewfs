# Readahead profile experiment

## R9: 1 GiB readahead closes most of the JuiceFS bigread gap

The multithreading hypothesis was ruled out first. The accepted read profile
already sets `BREWFS_FUSE_WORKERS=16` and `BREWFS_FUSE_MAX_BACKGROUND=512`,
while `fio-bigread` uses `bs=4m`, `numjobs=8`, and `iodepth=1`. The remaining
hypotheses were checksum work, FUSE direct mode, and readahead depth.

Same-binary `nozero-final` A/B, one warmup plus three measured bigread runs:

| candidate | median bigread | artifact |
| --- | ---: | --- |
| checksum full (baseline) | 4145.7 MiB/s | `perf-run-1789825174-22012` |
| checksum none | 4289.0 MiB/s (+3.5%) | `perf-run-1789825010-3450` |
| FUSE read direct off | 3338.2 MiB/s (-19.5%) | `perf-run-1789825067-19979` |
| readahead 512 MiB | 4096.0 MiB/s | `perf-run-1789825328-25968` |
| readahead 1 GiB | 4432.9 MiB/s (+6.9%) | `perf-run-1789825249-31616` |

The 512 MiB window was ineffective and had 11.2% spread. The 1 GiB window
matches JuiceFS's `--max-readahead 1024` comparison setting. A back-to-back
three-scene pass compared default readahead against 1 GiB:

| tool | default (`perf-run-1789825540-21433`) | 1 GiB (`perf-run-1789825399-16380`) | delta |
| --- | ---: | ---: | ---: |
| seqread | 1.875 GiB/s | 1.906 GiB/s | +1.6% |
| bigread median | 3.707 GiB/s | 4.396 GiB/s | +18.6% |
| randread | 4.236 GiB/s | 4.282 GiB/s | +1.1% |

The `compare_artifacts.py` bigread `tool_wall` field is not comparable when
the candidate has three repeats and the baseline is canonicalized to one run;
use `fio-bigread-repeat-summary.json` for the median. Direct read remains
required: turning it off regressed the isolated bigread pass by 19.5%.
Disabling checksum is not worth the correctness trade for +3.5%.

The mixed read/write gate also passed. Explicit 64 MiB versus 1 GiB
single-run A/B:

| metric | 64 MiB (`perf-run-1789828021-31672`) | 1 GiB (`perf-run-1789828085-3470`) | delta |
| --- | ---: | ---: | ---: |
| `fio-randrw` read | 917.6 MiB/s | 958.4 MiB/s | +4.4% |
| `fio-randrw` write | 410.9 MiB/s | 429.0 MiB/s | +4.4% |
| read p99 | 33.161 ms | 29.753 ms | -10.3% |
| write p99 | 14.090 ms | 11.469 ms | -18.6% |

Both legs drained in 2 s with zero dirty/pending/buffer bytes and zero runner
warnings. A first randrw baseline attempt (`perf-run-1789827949-19231`) was
discarded as a second candidate sample because the harness default had already
been changed to 1 GiB.

CI gate: harness syntax and profile-budget/report tests passed, and
`cargo test --workspace --lib --bins` passed (842 + 741 tests).
