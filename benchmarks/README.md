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

Darwin arm64, release-profile build of `stdo 0.5.2` from release commit `9f3f2cf`, 15 measured trials and two warmups per mode. The maintainer ran the benchmark on the Apple Silicon host.

| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | OS max RSS (KiB) | Cache hits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sequential Bash | 237.00 | 243.80 | 59.78 | n/a | 2,176 | n/a |
| Parallel Bash | 111.00 | 120.00 | 58.43 | n/a | 2,176 | n/a |
| Cold stdo (`--no-cache`) | 126.39 | 128.80 | 58.35 | 393,233 | 4,320 | 0% |
| Warm stdo | 63.78 | 65.14 | 5.46 | 262,178 | 4,064 | 60% |

Cold `stdo` was 15.39 ms behind Parallel Bash at p50 in this run. Warm `stdo` showed median speedups of **1.74×** over Parallel Bash and **3.72×** over Sequential Bash.

Bash wall time comes from the shell's `time` keyword. Separate executions estimate Bash TTFB from the earliest completed one-line worker, including `jq` timestamp overhead. `stdo bench` measures the first stdout chunk from any step with its internal clock. Both calculate percentiles by linear interpolation. Peak monitored bytes count `stdo` buffers only and are separate from process RSS. This workload measures scheduling overlap and cache reuse with deliberately delayed workers. JSONL validation and process-group cleanup are covered by qualification tests.

The suite measures RSS in separate runs using `/usr/bin/time -l` on macOS or GNU `/usr/bin/time -v` on Linux. On macOS, this is the maximum resident set size from `wait4` (the `ru_maxrss` field). The suite reports the largest OS-provided peak among the runs for each mode in KiB. This value follows the platform tool's accounting and represents neither the sum of concurrent process RSS nor the monitored-buffer budget. If the tool cannot report RSS, the suite prints `n/a`.

## Live Ollama qualification

These are observed step timings from a live Apple Silicon run reported by the maintainer. They are separate from the synthetic Bash comparison and its OS RSS samples.

| Workflow | Observed step timings | Output or cache result | Peak monitored buffers |
| --- | --- | --- | --- |
| [07 · Gemma 4 to Phonon speech](../examples/07-ollama-gemma4-phonon-speech.yaml) | `ask_gemma` 15.8 s, `stream_phonon` 942 ms | 814,124-byte WAV | 402 KB |
| [08 · Local PR triage](../examples/08-ollama-local-pr-triage.yaml) | Cold Gemma 4 19.5 s, warm cache hit 0.41 ms | SHA-256 cache reused the triage result | n/a |

Run the adversarial qualification tests separately.

```sh
cargo test --test qualification_test
```

They cover 12 MiB pass-through with a monitored-buffer ceiling of 512 KiB, backpressure from a delayed reader, side-effect protection after upstream failure, descendant cleanup on timeout, streaming JSONL validation, and executable cache invalidation when contents change without changing file size or modification time.
