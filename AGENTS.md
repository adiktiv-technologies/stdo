# AGENTS.md — Architectural Guardrails for `stdo`

You are working on **`stdo`** (_Standard I/O Orchestrator_), an ultra-compact, 100% offline Unix DAG workflow engine in Rust with an embedded Studio UI, developed by **Adiktiv Technologies Inc.**

## 1. Non-Negotiable Architectural Laws

1. **ZERO Outbound Network Dependencies:**
   - NEVER add `reqwest`, `hyper-tls`, `rustls`, `openssl`, `ureq`, or any HTTP client crate to `Cargo.toml`.
   - `stdo` only binds a local TCP listener on `127.0.0.1` for `stdo serve`. All external network calls in user workflows must be delegated to system binaries (e.g., `curl`).
2. **ZERO External Frontend Assets (100% Air-Gapped UI):**
   - The `stdo serve` Web UI (`src/studio/studio.html`) must work on an air-gapped machine.
   - NEVER include external scripts, fonts, CDNs, or npm runtime dependencies. Use Vanilla HTML/CSS/JS and inline SVG embedded with `include_str!("studio.html")`.
3. **Strict `stdin` / `stdout` Process Isolation:**
   - Always spawn subprocesses directly via `tokio::process::Command::new(program).args(args)` without invoking a shell (`/bin/sh -c`) unless explicitly requested by the user's command array.
   - Never pass large step outputs via environment variables or CLI arguments (`ARG_MAX` hazard); always route payloads through `Stdio::piped()`.
4. **Binary Footprint Discipline (< 3.5 MiB):**
   - Keep crate dependencies minimal and disable default features where possible (`default-features = false`).
   - Preserve the release profile (`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`).

## 2. Project Layout

- `src/main.rs` — CLI entrypoint (`run`, `serve`, `doctor`, `bench`).
- `src/dag/` — Workflow schema, graph validation, topological layers, and runtime execution groups.
- `src/engine/` — Subprocess runner, MiniJinja rendering, monitored pipes, JSONL validation, and telemetry.
- `src/engine/cache.rs` — SHA-256 content-addressable step cache.
- `src/studio/mod.rs` — Loopback HTTP/SSE server using `tokio::net::TcpListener` (default `127.0.0.1:8080`).
- `src/studio/studio.html` — Embedded zero-dependency Studio UI.
- `src/bench.rs` and `src/report.rs` — Benchmark comparisons and atomic offline audit exports.
- `examples/` — Runnable POSIX workflows, repository audits, and the JSONL sensor pipeline.

## 3. Verification Commands

Before completing any task, always run:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## Git & Protected `main` Policy

- NEVER commit or push directly to `main`. Create a short-lived `feat/<name>`, `fix/<name>`, `chore/<name>`, or `docs/<name>` branch per task. The `release-plz-` prefix is reserved for automated release PRs.
- Before pushing or opening a PR, always run and pass `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Merge through a PR only. Keep the branch up to date with `main`, wait for the required `check-and-test` status to pass, and use Squash and merge to preserve linear history. The ruleset requires zero approvals.
- Use the PR title as the squash commit subject. Configure GitHub's default squash commit message to use the pull request title and verify the subject in the merge dialog.

## Conventional Commits & SemVer `0.x` Bumping Strategy

Release-plz reads the squash commit on `main`, so the PR title must follow Conventional Commits.

| PR title | Release effect |
| --- | --- |
| `fix(scope): ...` or `perf(scope): ...` | Patch, such as `0.0.1` → `0.0.2` or `0.1.0` → `0.1.1` |
| `feat(scope): ...` | Patch during `0.0.x` pre-alpha and for ordinary additive features in `0.x` |
| `feat(scope)!: ...` | Minor for a breaking change or milestone completion, such as `0.0.x` → `0.1.0` or `0.1.x` → `0.2.0` |
| `chore: ...`, `docs: ...`, `ci: ...`, `test: ...`, or `refactor: ...` | No version bump or crates.io release on their own |

Use the `!` marker on a feature PR that completes a milestone so release-plz can distinguish it from an ordinary feature. A maintainer must squash-merge the generated release PR after CI passes to publish to crates.io and create the GitHub release.

Configure repository Actions secrets `CARGO_REGISTRY_TOKEN` (with publish access to `stdo`) and `RELEASE_PLZ_TOKEN` (a repository-scoped GitHub token with Contents and Pull requests read/write access). The workflow passes the latter as `GITHUB_TOKEN` so GitHub runs CI on generated release PRs and builds binaries on release tags.

## Workflow Commands

- `/ship` follows [the shipping procedure](.claude/commands/ship.md) to validate, commit, push, and open a PR against `main`.
