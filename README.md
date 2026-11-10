<div align="center">

# ⚡ `stdo`

**Standard I/O Orchestrator — Workflow-as-Code meets the Unix Philosophy.**

A compact, zero-network Rust binary that orchestrates local CLI tools (`curl`, `ollama`, `rg`, `jq`, `sqlite3`, `dscr`) over `stdin` and `stdout` pipes.

[![Crates.io](https://img.shields.io/crates/v/stdo.svg)](https://crates.io/crates/stdo)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/adiktiv-technologies/stdo/blob/main/LICENSE)
[![Network: 100% Offline](https://img.shields.io/badge/network-zero_outbound_calls-success.svg)](#architecture--core-concepts)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://github.com/adiktiv-technologies/stdo/blob/main/Cargo.toml)

[**Quickstart**](#quickstart) • [**What is a Unix DAG?**](#what-is-a-unix-dag) • [**Embedded Studio (`stdo serve`)**](#embedded-studio-stdo-serve) • [**Roadmap**](#roadmap)

</div>

> [!NOTE]
> **Phase 1 (`v0.1.0`) is complete.** `stdo doctor` and `stdo run` are ready, with a topological DAG engine, parallel layers, monitored pipes, switch routing, and a PID ledger. The Embedded Studio UI (`stdo serve`) and SHA-256 content-addressable caching are scheduled for Phase 2 (`v0.2.0`).

## Why `stdo`?

Modern AI, data, and automation orchestrators (like Dify, Airflow, or Langflow) suffer from severe infrastructure bloat: they demand multi-container Docker stacks, PostgreSQL, Redis, and lock your logic into fragile Python SDKs that break every month.

**`stdo` (pronounced _"studio"_ or _"S-T-D-O"_) takes the radical Unix path:**

1. **Zero HTTP Client Inside:** `stdo` contains no outbound network stack (`no reqwest`, `no openssl`). Need a REST API? Call `curl`. Need a local LLM? Call `ollama` or `llama-cli`. Need vector or relational queries? Call `sqlite3`.
2. **True `stdin` / `stdout` Piping (No `ARG_MAX` Crashes):** Passing megabytes of JSON or RAG context through environment variables (`${OUTPUT}`) crashes the OS kernel (`ARG_MAX` limit) and invites shell-injection bugs. `stdo` streams data directly through process file descriptors (`fd 0` and `fd 1`).
3. **Content-Addressable Cache (Phase 2):** Step results will be reusable when `SHA-256(cmd + args + stdin)` matches a cached result.
4. **Embedded Offline Studio (Phase 2):** A local Web UI will visualize the execution graph, generate input forms, and stream logs over WebSockets.

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

```bash
# From a checkout of this repository
cargo install --path . --locked
```

### 2. Define your workflow (`stdo.yaml`)

`stdo` supports two complementary data-passing modes between nodes:

- **Buffered Templating (`stdin: "{{ steps.x.stdout }}"`):** Waits for step `x` to complete, then injects its output into a template (ideal for assembling multi-source prompts).
- **Monitored Pipe (`pipe_from: step_x`):** Captures `step_x`'s stdout in 64 KiB reads and writes those bytes to the downstream command's stdin after the upstream layer finishes. Both stdout and stderr are drained while stdin is being written.

By default, `run` and `doctor` allow up to 64 steps, 16 layers, and 16 steps per layer. Use `--force` to bypass these graph limits. Subprocess streams still have an 8 MiB limit each, with a shared 32 MiB budget for captured data, pipe read buffers, and rendered I/O buffers. On failure, `stdo run` prints the PID and last known status of each direct child it started.

Use `{{ inputs.key }}` in command arguments and pass step output through `stdin` templates or `pipe_from`. The workflow's `output` template determines what `stdo run` writes to stdout.

Pass `--telemetry` (`-t`) to print step and layer timings, captured byte counts, and peak monitored buffer usage to stderr. Failed runs include a partial report. Failed commands also surface captured stderr above the PID ledger.

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
    cmd: ["sqlite3", "./security_rules.db"]
    stdin: |
      INSERT INTO incidents (summary, ts)
      VALUES ('{{ steps.local_llm_audit.json.summary }}', datetime('now'));

  - id: approve_release
    cmd: ["echo", "Release approved."]

output: "{{ steps.local_llm_audit.json.summary }}"
```

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

## Embedded Studio (`stdo serve`)

Phase 2 will add a local-only HTTP/WebSocket server bound to `127.0.0.1:8080` with **zero external CDN calls** and these panels:

- **Left Panel (Auto-Generated Controls):** Renders typed inputs (`string`, `path`, `enum`, `number`, `boolean`) directly from your `stdo.yaml`.
- **Center Panel (Live SVG DAG):** Visualizes topological layers, parallel branches, and conditional switches. Active nodes pulse in real time as processes execute.
- **Right Panel (Stream Inspector):** Click any node to inspect resolved `stdin`, live `stdout` / `stderr` lines, execution latency (`ms`), exit code, and SHA-256 cache hit status.

## Roadmap

### Phase 1 — Foundation (`v0.0.x` → `v0.1.0`)

- [x] Crate reservation & baseline CLI (`stdo`).
- [x] Topological DAG parser (`stdo.yaml`), cycle detector, and `stdo doctor` binary verifier.
- [x] Buffered `minijinja` stdin templates and monitored `pipe_from` transfers.
- [x] Parallel execution within each topological layer via `tokio::process`.
- [x] Self-contained POSIX workflow gallery in `examples/` (fan-out, `pipe_from`, JSON switch routing).

### Phase 2 — Caching & Embedded Studio (`v0.2.0`)

- [ ] Content-Addressable Storage (CAS) in `.stdo/cache/` keyed by `SHA-256(cmd + args + stdin)`.
- [ ] `stdo serve` local server (`127.0.0.1:8080`) with embedded single-file Vanilla JS + SVG DAG UI (`rust-embed`).
- [ ] Live WebSocket streaming of `stdout` / `stderr` and hot-reload on `stdo.yaml` changes (`--watch`).

### Phase 3 — Industrial Benchmarking & Streaming (`v0.3.0`)

- [ ] `stdo bench`: Built-in A/B/C pipeline comparison harness measuring end-to-end latency, Time-To-First-Byte (TTFB/TTFT), and peak RSS memory per node.
- [ ] Continuous JSONL line-by-line stream routing for high-frequency sensor and log pipelines.
- [ ] Exportable offline HTML/JSON execution audit reports for air-gapped compliance.

## License

Licensed under the **Apache License, Version 2.0** ([LICENSE](https://github.com/adiktiv-technologies/stdo/blob/main/LICENSE) or http://www.apache.org/licenses/LICENSE-2.0).
