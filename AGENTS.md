# AGENTS.md — Architectural Guardrails for `stdo`

You are working on **`stdo`** (_Standard I/O Orchestrator_), an ultra-compact, 100% offline Unix DAG workflow engine in Rust with an embedded Studio UI, developed by **Adiktiv Technologies Inc.**

## 1. Non-Negotiable Architectural Laws

1. **ZERO Outbound Network Dependencies:**
   - NEVER add `reqwest`, `hyper-tls`, `rustls`, `openssl`, `ureq`, or any HTTP client crate to `Cargo.toml`.
   - `stdo` only binds a local TCP listener on `127.0.0.1` for `stdo serve`. All external network calls in user workflows must be delegated to system binaries (e.g., `curl`).
2. **ZERO External Frontend Assets (100% Air-Gapped UI):**
   - The `stdo serve` Web UI (`src/ui/`) must work on an air-gapped machine with no internet connection.
   - NEVER include external `<script src="https://...">`, Google Fonts, Tailwind CDNs, or npm runtime dependencies. Everything must be Vanilla HTML/CSS/JS + inline SVG embedded via `rust-embed`.
3. **Strict `stdin` / `stdout` Process Isolation:**
   - Always spawn subprocesses directly via `tokio::process::Command::new(program).args(args)` without invoking a shell (`/bin/sh -c`) unless explicitly requested by the user's command array.
   - Never pass large step outputs via environment variables or CLI arguments (`ARG_MAX` hazard); always route payloads through `Stdio::piped()`.
4. **Binary Footprint Discipline (< 3.5 MiB):**
   - Keep crate dependencies minimal and disable default features where possible (`default-features = false`).
   - Preserve the release profile (`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`).

## 2. Project Layout

- `src/main.rs` — CLI entrypoint (`run`, `serve`, `doctor`, `bench`).
- `src/dag.rs` — `stdo.yaml` schema, topological sorting, and cycle detection.
- `src/engine.rs` — Subprocess spawner, `minijinja` stdin renderer, `pipe_from` OS descriptor wiring, and `SHA-256` CAS cache.
- `src/server.rs` — Local `axum` HTTP/WebSocket server (`127.0.0.1:8080`).
- `src/ui/index.html` — Embedded zero-dependency Studio UI.
- `examples/` — Reference `.stdo.yaml` workflows (including `dscr` + `.nex` air-gapped pipelines).

## 3. Verification Commands

Before completing any task, always run:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
