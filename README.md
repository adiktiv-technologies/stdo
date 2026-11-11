<div align="center">

# ⚡ `stdo` <sup><a href="https://cdn.jsdelivr.net/gh/adiktiv-technologies/stdo@main/assets/stdo-pronunciation.mp4" title="Hear how stdo is pronounced">🔊</a></sup>

**Standard I/O Orchestrator — Workflow-as-Code meets the Unix Philosophy.**

A compact, zero-network Rust binary that orchestrates local CLI tools (`curl`, `ollama`, `rg`, `jq`, `sqlite3`, `dscr`) over `stdin` and `stdout` pipes.

[![Crates.io](https://img.shields.io/crates/v/stdo.svg)](https://crates.io/crates/stdo)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/adiktiv-technologies/stdo/blob/main/LICENSE)
[![Network: 100% Offline](https://img.shields.io/badge/network-zero_outbound_calls-success.svg)](#architecture--core-concepts)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://github.com/adiktiv-technologies/stdo/blob/main/Cargo.toml)

[**Quickstart**](#quickstart) • [**What is a Unix DAG?**](#what-is-a-unix-dag) • [**Embedded Studio (`stdo serve`)**](#embedded-studio-stdo-serve) • [**Roadmap**](#roadmap)

</div>

![stdo Studio showing a LOW-severity release gate with three DAG layers and run telemetry](https://raw.githubusercontent.com/adiktiv-technologies/stdo/main/assets/studio-preview.svg)

> [!NOTE]
> Phases 1 through 5 are complete. See the Crates.io badge above for the current version and the [changelog](CHANGELOG.md) for release details.

## Why `stdo`?

Cron and job tools such as Dagu, Airflow, and Task organize when jobs run and which jobs depend on one another. `stdo` focuses on what flows between running commands. It is a **bounded-memory Unix process dataflow engine** that connects process stdin (`fd 0`) and stdout (`fd 1`) and monitors the bytes in transit.

**stdo** *(pronounced "studio" or "S-T-D-O" [🔊](https://cdn.jsdelivr.net/gh/adiktiv-technologies/stdo@main/assets/stdo-pronunciation.mp4))* takes the radical Unix path:

1. **Zero built-in outbound networking:** The orchestrator binary has no outbound network stack. Commands it starts, such as `curl`, can access the network directly.
2. **True `stdin` / `stdout` Piping (No `ARG_MAX` Crashes):** Passing megabytes of JSON or RAG context through environment variables (`${OUTPUT}`) crashes the OS kernel (`ARG_MAX` limit) and invites shell-injection bugs. `stdo` streams data directly through process file descriptors (`fd 0` and `fd 1`).
3. **Content-Addressable Cache:** Deterministic steps can reuse captured output from a previous run.
4. **Embedded Offline Studio:** Edit the execution graph, configure inputs, and inspect completed runs from a local browser.

## What is a Unix DAG?

At the heart of `stdo` is a **DAG (_Directed Acyclic Graph_)**:

- **Directed:** Data flows in a clear, deterministic direction from upstream producer processes to downstream consumer processes.
- **Acyclic:** The graph contains no infinite loops that could lock up unattended pipelines or CI/CD runners; every execution path is guaranteed to terminate.
- **Graph:** Unlike a linear Bash pipe (`cmd1 | cmd2 | cmd3`), a DAG supports **parallel fan-out** (running 4 CLI tools simultaneously), **fan-in aggregation** (combining outputs from multiple tools into a single LLM prompt), and **conditional branching** (`switch` routing based on structured JSON output or exit codes).

```text
                  ┌──► [ Step B: rg (Static Scan) ] ──────┐
[ Inputs / CLI ] ─┤                                       ├──► [ Step D: ollama / dscr ] ──► [ Switch ]
                  └──► [ Step C: sqlite3 (Fetch Rules) ] ─┘                                    ├─► PASS
                                                                                               └─► BLOCK
```

Before starting subprocesses, `stdo run` sorts and validates the DAG. Use `stdo doctor` to check the graph and confirm executable availability without starting any workflow commands.

## Quickstart

### 1. Install

```sh
cargo install stdo --locked
```

Download a prebuilt binary for your platform from [GitHub Releases](https://github.com/adiktiv-technologies/stdo/releases). Linux builds use static musl binaries.

To build from source:

```sh
git clone https://github.com/adiktiv-technologies/stdo.git
cd stdo
cargo install --path . --locked
```

### 2. Define your workflow (`stdo.yaml`)

`stdo` supports two complementary data-passing modes between nodes:

- **Buffered Templating (`stdin: "{{ steps.x.stdout }}"`):** Waits for step `x` to complete, then injects its output into a template (ideal for assembling multi-source prompts).
- **Monitored Pipe (`pipe_from: step_x`):** Forwards stdout in chunks of up to 64 KiB while capturing bounded output. Eligible producers and consumers run concurrently with backpressure. Downstream EOF follows successful producer completion. Buffered template references and unresolved switch branches still wait for their dependencies. A cache-enabled consumer waits for complete stdin to compute its cache key.

Set `capture_stdout: false` on a command to stream stdout to live consumers or discard it when unpiped. The stream can exceed 8 MiB while the queue stays bounded by backpressure. Telemetry still counts bytes and measures TTFB, and audit reports hash the stream incrementally. Uncaptured stdout cannot be replayed, exposed as `steps.<id>.stdout` or `steps.<id>.json`, cached, or parsed as a single JSON document. Streaming JSONL still validates and normalizes records, with a 64 KiB limit per record.

Set `pipe_mode: atomic` on a `pipe_from` consumer to start it only after the captured producer output has passed validation and the producer has exited with status zero. This is useful for steps that write to databases or external systems. With the default `pipe_mode: concurrent`, a consumer may act on partial output before a later producer failure.

#### Execution mode trade-offs

| Setting | When data moves | What composes |
| --- | --- | --- |
| `pipe_mode: concurrent` | The consumer can start while the producer is running. | Accepts captured or uncaptured producers. Cache lookup on the consumer, retries on either end, or other dependencies can require a buffered edge. |
| `pipe_mode: atomic` | The consumer starts after the producer exits zero and its output passes validation. | Requires capture on the producer. The consumer can disable its own capture. Any external writes it makes remain in place if it later fails. |
| `capture_stdout: false` | Stdout streams to live consumers, or is discarded when unpiped. | Supports streaming JSONL. Rules out replay, `steps.<id>.stdout` and `.json`, whole-document JSON parsing, and caching on that step. |
| `cache: true` | A piped consumer buffers all stdin for its cache key. A cached producer can replay its captured output into a live pipe. | Requires capture on the cached step and a captured upstream for piped input. Works with retries on cache misses. |
| `retry: { attempts: 2 }` | A retrying consumer replays buffered stdin. Downstream steps wait for the retrying producer to succeed. | With multiple attempts, piped input requires a captured upstream, and piped output requires capture on the retrying step. |

Set `parse_stdout: jsonl` to validate each non-empty JSON Lines record as stdout arrives. The runner compacts each record, ignores blank lines, and forwards complete records to live `pipe_from` consumers. An invalid record fails the producer with its line number. Templates and switches can use `steps.<id>.json | length` or index a record with `steps.<id>.json[0]`. Try [the JSONL sensor example](examples/06-jsonl-sensor-stream.yaml).

By default, `run` and `doctor` allow up to 64 steps, 16 layers, and 16 steps per layer. Use `--force` to bypass these graph limits. Captured streams have an 8 MiB limit each, with a shared 32 MiB budget for captured data, pipe buffers, and rendered I/O buffers. On failure, `stdo run` prints the PID and last known status of each direct child it started.

Use `{{ inputs.key }}` in command arguments and pass step output through `stdin` templates or `pipe_from`. The workflow's `output` template determines what `stdo run` writes to stdout.

Pass `--telemetry` (`-t`) to print step and layer timings, time to first byte (TTFB), captured byte counts, and peak monitored buffer usage to stderr. Failed runs include a partial report. Failed commands also surface captured stderr above the PID ledger.

Set `timeout_ms: 5000` to give each command attempt a five-second deadline, including pipe I/O. On timeout, the runner kills the Unix process group, reaps the direct child, and marks it `TimedOut` in the PID ledger. Set `retry: { attempts: 3, backoff_ms: 250 }` to allow up to three total attempts, with a fixed 250 ms delay between failures. Attempts must be between 1 and 10, and the delay defaults to 0 ms with a maximum of 60,000 ms. Non-ignored exit failures, timeouts, and invalid JSON or JSONL can trigger retries. Retrying steps buffer piped stdin for replay and publish output only from a successful attempt. Telemetry and audit reports show the number of attempts. Step duration includes backoff.

Set `cache: true` on a deterministic command step to reuse its output from `.stdo/cache/` under the working directory. The SHA-256 key covers rendered arguments, stdin bytes, exit-code and output-parsing settings, and the executable's content digest and metadata. Executables that cannot be read because of file permissions use metadata only. Declare additional dependencies with `cache_files: ["{{ inputs.config_path }}"]` and `cache_env: ["LANG", "TZ"]`. File paths are relative to the working directory and must resolve to regular files within it. The key includes their contents and distinguishes unset environment variables from empty values. Pass `--no-cache` to bypass cache reads, writes, and dependency hashing. Telemetry reports cache hits separately from executed commands.

```yaml
version: "1.0"
name: "offline-code-triage"
description: "100% local security audit orchestrating ripgrep, SQLite, and Ollama"

inputs:
  target_dir:
    type: path
    default: "./src"
    description: "Directory to scan"
  severity:
    type: enum
    values: ["LOW", "HIGH", "CRITICAL"]
    default: "HIGH"

steps:
  # Runs in parallel with fetch_policy (no dependencies between them)
  - id: scan_code
    name: "Static secret & unsafe pattern scan"
    cmd:
      [
        "rg",
        "--json",
        "-e",
        "password|secret|api_key|unsafe",
        "{{ inputs.target_dir }}",
      ]
    ignore_exit_code: true

  # Runs in parallel with scan_code
  - id: fetch_policy
    name: "Load internal security rules from SQLite"
    cmd: ["sqlite3", "-json", "./security_rules.db"]
    stdin: |
      SELECT rule_id, description FROM rules WHERE severity = '{{ inputs.severity }}';

  # Fan-in: waits for both scan_code and fetch_policy
  - id: local_llm_audit
    name: "Triage findings with local LLM"
    cmd: ["ollama", "run", "qwen2.5-coder:7b", "--format", "json"]
    cache: true
    stdin: |
      You are an offline security auditor.
      Rules: {{ steps.fetch_policy.stdout }}
      Findings: {{ steps.scan_code.stdout }}
      Respond ONLY with JSON: {"block_release": boolean, "summary": string}
    parse_stdout: json

  # Deterministic conditional routing
  - id: release_gate
    type: switch
    condition: "{{ steps.local_llm_audit.json.block_release }}"
    cases:
      "true": record_incident
      "false": approve_release

  - id: record_incident
    pipe_from: local_llm_audit
    pipe_mode: atomic
    cmd:
      - sqlite3
      - "-batch"
      - "-bail"
      - "./security_rules.db"
      - "CREATE TEMP TABLE audit_input(payload TEXT);"
      - ".mode ascii"
      - ".import /dev/stdin audit_input"
      - |
        INSERT INTO incidents (summary, ts)
        SELECT json_extract(payload, '$.summary'), datetime('now')
        FROM audit_input;

  - id: approve_release
    cmd: ["echo", "Release approved."]

output: "{{ steps.local_llm_audit.json.summary }}"
```

SQLite imports the validated JSON from stdin into a temporary table. ASCII mode uses control-character separators that cannot appear literally in valid JSON. The SQL stays fixed while `json_extract` reads the summary as data.

### 3. Execute or Inspect Visually

```bash
# Validate a bundled workflow and check its required executables
stdo doctor -f examples/01-parallel-repo-audit.yaml

# Run it headlessly from the repository root
stdo run -f examples/01-parallel-repo-audit.yaml

# Keep the workflow output on stdout and write telemetry to stderr
stdo run -t -f examples/01-parallel-repo-audit.yaml
```

See the [workflow gallery](examples/README.md) for the ETL and release gate examples.

- Turn a Gemma 4 answer into a WAV file with [Ollama and Phonon](examples/07-ollama-gemma4-phonon-speech.yaml).
- Route a Rust source triage report from a [local Ollama model](examples/08-ollama-local-pr-triage.yaml).
- Fetch a REST endpoint with retries and write validated JSONL through an [atomic pipe](examples/09-generic-http-api-pipeline.yaml).
- Turn a local voiceover into a waveform or recording MP4 with the [YouTube capsule generator](examples/10-youtube-capsule-generator.yaml).

### Stream local speech with Ollama and Phonon

The pronunciation clip linked next to the title ([MP4](https://cdn.jsdelivr.net/gh/adiktiv-technologies/stdo@main/assets/stdo-pronunciation.mp4)) is generated end-to-end by running [examples/10-youtube-capsule-generator.yaml](examples/10-youtube-capsule-generator.yaml) via `scripts/generate-pronunciation-asset.sh` (local Gemma 4 → Phonon TTS → FFmpeg).

Gradium’s Phonon family streams speech as it generates it. Ollama’s `phonon:latest` uses the 31M English checkpoint and selects the `default` voice when you omit `--voice`. These commands use the Phonon package for Apple Silicon and Ollama 0.40.9 or later. [Gradium models](https://huggingface.co/gradium) · [Phonon 31M English](https://huggingface.co/gradium/phonon-31m-en) · [Ollama speech guide](https://docs.ollama.com/capabilities/speech)

```sh
ollama run phonon:latest "Good morning. Here is a short update before your first meeting."
ollama run gemma4 --hidethinking "Tell me a fun fact about owls in two sentences." | ollama run phonon:latest
```

Save this workflow as `speech.yaml` and run `stdo run -f speech.yaml`. Phonon streams WAV bytes into a sibling temporary file. The final step replaces `speech.wav` after Phonon and the staged write succeed. [Runnable gallery example](examples/07-ollama-gemma4-phonon-speech.yaml) · [Ollama WAV output](https://docs.ollama.com/capabilities/speech#save-audio-to-a-file)

```yaml
version: "1.0"
name: spoken-owl-fact
steps:
  - id: answer
    cmd: [ollama, run, gemma4, --hidethinking, "Tell me a fun fact about owls in two sentences."]
  - id: speak
    cmd: [ollama, run, "phonon:latest"]
    pipe_from: answer
    capture_stdout: false
  - id: stage_wav
    cmd:
      - sh
      - -c
      - |
        set -eu
        case "$1" in /*) target=$1 ;; *) target=./$1 ;; esac
        tmp="$target.tmp.$$"
        umask 077
        (set -C; : > "$tmp")
        trap 'rm -f "$tmp"' 0
        trap 'exit 1' HUP INT TERM
        cat > "$tmp"
        printf '%s\n' "$$"
        trap - 0 HUP INT TERM
      - stage-wav
      - "speech.wav"
    pipe_from: speak

  - id: save_wav
    cmd:
      - sh
      - -c
      - |
        set -eu
        case "$1" in /*) target=$1 ;; *) target=./$1 ;; esac
        IFS= read -r pid
        case "$pid" in ''|*[!0-9]*) exit 1 ;; esac
        tmp="$target.tmp.$pid"
        trap 'rm -f "$tmp"' 0
        trap 'exit 1' HUP INT TERM
        bytes=$(wc -c < "$tmp")
        [ ! -d "$target" ]
        mv -f "$tmp" "$target"
        printf '%s\n' "$bytes"
      - write-wav
      - "speech.wav"
    stdin: |
      {{ steps.stage_wav.stdout | trim }}
      {# The dependency below waits for Phonon's successful exit as well. #}
      {{ steps.speak.exit_code }}

output: |
  Wrote {{ steps.save_wav.stdout | trim }} bytes to speech.wav
```

### Benchmark workflows and export audit reports

```sh
stdo run -f examples/03-release-gate-switch.yaml -i severity=LOW --report audit.json
stdo bench examples/01-parallel-repo-audit.yaml examples/02-monitored-pipe-etl.yaml \
  --runs 5 --warmup 1 --no-cache --report comparison.html
```

`run --report` keeps workflow stdout available for piping and writes the audit to a separate file, including partial results when a readable workflow fails. It records the resolved inputs, SHA-256 hashes of the original YAML and captured stdout, and per-step timings and byte counts. Reports contain resolved input values, so treat them as sensitive if the workflow takes secrets. Use `.html` or `.htm` for a self-contained report, or another extension for JSON.

`bench` accepts one or more workflows and compares them against the first workflow's median latency. It runs one warmup and five measured trials per workflow by default. The table reports min, mean, p50, p95, max, mean TTFB, peak monitored bytes, and cache hit ratio, plus per-step mean and p95. Percentiles use linear interpolation, and the cache ratio is the number of hits divided by all recorded steps. Pass `-i KEY=VALUE` for an override shared by the workflows. Cached steps can reuse results during warmup and measurement. Pass `--no-cache` to measure subprocess execution on every trial. HTML benchmark reports include four inline charts.

Step TTFB measures from spawn to the first stdout chunk, or the read duration on a cache hit. Workflow TTFB is elapsed time to the first stdout from any step, with total duration as the fallback for a silent run. Benchmarks load each workflow once before its trials.

The [qualification benchmark](benchmarks/README.md) used `stdo 0.5.2` on Darwin arm64 with 15 measured trials and two warmups per mode.

On macOS, `/usr/bin/time -l` reports the maximum resident set size from `wait4` (the `ru_maxrss` field). The benchmark reports that peak in KiB separately from monitored buffers.

| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | OS max RSS (KiB) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Sequential Bash | 237.00 | 243.80 | 59.78 | n/a | 2,176 |
| Parallel Bash | 111.00 | 120.00 | 58.43 | n/a | 2,176 |
| Cold `stdo --no-cache` | 126.39 | 128.80 | 58.35 | 393,233 | 4,320 |
| Warm `stdo` | 63.78 | 65.14 | 5.46 | 262,178 | 4,064 |

With three of five steps served from cache, warm `stdo` took **57%** of Parallel Bash's median time and **27%** of Sequential Bash's on this workload.

In a separate live Apple Silicon run, Gemma 4 answered the prompt in 15.8 s and Phonon produced an 814,124-byte WAV in 942 ms with 402 KB of peak monitored buffers for the workflow. Local PR triage took 19.5 s cold and 0.41 ms on a warm cache hit. [Live qualification results](benchmarks/README.md#live-ollama-qualification)

## Embedded Studio (`stdo serve`)

Studio is an offline workflow editor for wiring DAG steps, editing YAML, and inspecting runs in your browser.

```sh
stdo serve -f examples/03-release-gate-switch.yaml --port 8080
```

Open [Studio](http://127.0.0.1:8080) in your browser. With no file argument, Studio selects `stdo.yaml` if present, then the first YAML file from the working directory or `examples/`. Studio binds to IPv4 loopback and serves the editor from the binary.

- Drag nodes to arrange them. Connect output and input ports to add pipe, template, or switch links. Click an edge to remove it.
- The inspector has four tabs: **Requirements** lists executable availability, local model hints, and workflow inputs; **Edit** configures inputs and steps; **YAML** edits the same draft as text; **Run inspector** shows live output and telemetry.
- **Validate (Doctor)** checks the draft. **Save** validates and atomically updates the current file; **Save As…** chooses a new workspace path.
- Use **Run Workflow** to execute the draft with optional input overrides and cache bypass. Studio streams node states and a bounded stdout preview over SSE, then shows workflow output, timings, cache hits, and the PID ledger on failure. Inline `<audio>` and `<video>` artifact players let you listen to WAV output and watch MP4 output, with download links alongside them.
- Studio reloads external file edits when the draft is clean and flags conflicts when you have unsaved changes.

## Roadmap

### Phase 1 — Foundation (`v0.0.x` → `v0.1.0`)

- [x] Crate reservation & baseline CLI (`stdo`).
- [x] Topological DAG parser (`stdo.yaml`), cycle detector, and `stdo doctor` binary verifier.
- [x] Buffered `minijinja` stdin templates and monitored `pipe_from` transfers.
- [x] Parallel execution within each topological layer via `tokio::process`.
- [x] Self-contained POSIX workflow gallery in `examples/` (fan-out, `pipe_from`, JSON switch routing).

### Phase 2 — Caching & Embedded Studio (`v0.2.0`)

- [x] SHA-256 Content-Addressable Storage (CAS) for command steps in `.stdo/cache/`.
- [x] `stdo serve` loopback HTTP server with a single embedded HTML/CSS/JS editor and SVG canvas.
- [x] Workflow file editing, validation, execution, and per-step telemetry in Studio.

### Phase 3 — Industrial Benchmarking & Streaming (`v0.3.0`)

- [x] Live Studio output streaming over server-sent events (SSE) and workflow file reload.
- [x] `stdo bench` A/B/C comparisons with latency percentiles, TTFB, cache hit ratios, and peak monitored buffer usage.
- [x] Continuous JSONL line-by-line validation and routing for sensor and log pipelines.
- [x] Offline HTML/JSON execution audits and benchmark reports.

### Phase 4 — Concurrent Streaming & Production Resilience (`v0.4.0`)

- [x] True concurrent producer-consumer streaming across `pipe_from` chains with overlapping upstream execution and downstream TTFB.
- [x] Step-level `timeout_ms`, retry with backoff, and hermetic cache dependencies (`cache_files` / `cache_env`).

### Phase 5 — Unbounded Streaming, Atomic Pipes & Qualification (`v0.5.0`)

- [x] Bounded-memory pass-through for arbitrarily long stdout streams.
- [x] Atomic pipe gating and Unix process-group cleanup.
- [x] SHA-256 executable content hashing for cache invalidation.
- [x] Adversarial stream tests and a reproducible [benchmark suite](benchmarks/README.md).

## License

Licensed under the **Apache License, Version 2.0** ([LICENSE](https://github.com/adiktiv-technologies/stdo/blob/main/LICENSE) or http://www.apache.org/licenses/LICENSE-2.0).
