# Phase 5 streaming qualification

Run from the repository root with Bash, `jq`, and standard Unix utilities.

```sh
cargo build --locked --release
benchmarks/run_qualification.sh
```

The script uses 15 measured trials and two warmups per mode by default. Set `RUNS`, `WARMUP`, or `STDO_BIN` to override the defaults. Each invocation creates its own workspace and cache.

Three workers each wait 40 ms and emit one line. The fan-in streams those lines to the final stage, waits another 40 ms, then writes a fourth line. The final stage reports the first line and counts the rest. The Bash baselines write each worker's output to a temporary file and read those files in order. Sequential Bash runs the workers one at a time. Parallel Bash starts them with `&` and checks each job with `wait` before starting fan-in. Both `stdo` modes parallelize the workers. The warm mode reuses three worker results for a 60% cache hit ratio across five steps.

The suite measures Bash directly and checks the standalone recipe separately. Run it from the repository root.

```sh
stdo run -f benchmarks/parallel-bash-baseline.yaml
```

## Reference run

Darwin arm64, release-profile build of `stdo 0.5.0` from release commit `d6d7e7d`, 15 measured trials and two warmups per mode.

| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | Cache hits |
| --- | ---: | ---: | ---: | ---: | ---: |
| Sequential Bash | 237.00 | 259.70 | 62.04 | n/a | n/a |
| Parallel Bash | 116.00 | 119.50 | 61.21 | n/a | n/a |
| Cold stdo (`--no-cache`) | 118.74 | 130.69 | 56.41 | 393,233 | 0% |
| Warm stdo | 61.66 | 63.00 | 3.85 | 262,178 | 60% |

Cold `stdo` was close to Parallel Bash at p50 and had a higher p95 in this run. Warm `stdo` showed median speedups of **1.88×** over Parallel Bash and **3.84×** over Sequential Bash.

Bash wall time comes from the shell's `time` keyword. Separate executions estimate Bash TTFB from the earliest completed one-line worker, including `jq` timestamp overhead. `stdo bench` measures the first stdout chunk from any step with its internal clock. Both calculate percentiles by linear interpolation. Peak monitored bytes count `stdo` buffers only and are separate from process RSS. This workload measures scheduling overlap and cache reuse with deliberately delayed workers. JSONL validation and process-group cleanup are covered by qualification tests.

Run the adversarial qualification tests separately.

```sh
cargo test --test qualification_test
```

They cover 12 MiB pass-through with a monitored-buffer ceiling of 512 KiB, backpressure from a delayed reader, side-effect protection after upstream failure, descendant cleanup on timeout, streaming JSONL validation, and executable cache invalidation when contents change without changing file size or modification time.
