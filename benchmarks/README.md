# Phase 5 streaming qualification

Run from the repository root with Bash, `jq`, and standard Unix utilities.

```sh
cargo build --locked --release
benchmarks/run_qualification.sh
```

The script uses 15 measured trials and two warmups per mode by default. Set `RUNS`, `WARMUP`, or `STDO_BIN` to override the defaults. Each invocation creates its own workspace and cache.

Three workers each wait 40 ms and emit one line. The fan-in forwards those lines, waits another 40 ms, then writes a fourth line. The final stage reports the first line and counts the rest. The sequential Bash baseline runs the workers one at a time and saves the fan-in to a file before starting the reader. Both `stdo` modes parallelize the workers and stream the fan-in to the reader. The warm mode reuses the three worker results, for a 60% cache hit ratio across five steps.

## Reference run

Darwin arm64, Phase 5 release-profile build reporting `stdo 0.4.0`, 15 measured trials and two warmups per mode.

| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | Cache hits |
| --- | ---: | ---: | ---: | ---: | ---: |
| Sequential Bash | 251.17 | 259.96 | 58.58 | n/a | n/a |
| stdo cold | 120.76 | 124.84 | 56.71 | 393,228 | 0% |
| stdo warm | 64.15 | 66.88 | 5.14 | 262,178 | 60% |

The Bash baseline timestamps its first completed one-line worker and includes the overhead of the `jq` clock calls. The runner measures the first stdout chunk from any step and uses its internal clock. Both use linear interpolation for percentiles. Peak monitored bytes count `stdo` buffers only. Bash has no equivalent counter, and these values do not represent process RSS. This workload measures overlap and cache reuse with deliberately delayed workers.

Run the adversarial qualification tests separately.

```sh
cargo test --test qualification_test
```

They cover 12 MiB pass-through with a monitored-buffer ceiling of 512 KiB, backpressure from a delayed reader, side-effect protection after upstream failure, descendant cleanup on timeout, streaming JSONL validation, and executable cache invalidation when contents change without changing file size or modification time.
