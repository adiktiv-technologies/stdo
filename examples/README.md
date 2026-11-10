# Phase 1 workflow gallery

Run these commands from the repository root with the Phase 1 `stdo` binary on your PATH. The examples use POSIX tools and local inputs.

## 01 · Parallel repository audit

Three independent scans count YAML files, lines, and lines matching a literal keyword in `target_dir`. A MiniJinja stdin template combines the results. The grep step accepts exit code 1 so a keyword with no matches yields a count of zero.

```sh
stdo doctor -f examples/01-parallel-repo-audit.yaml
stdo run -f examples/01-parallel-repo-audit.yaml
stdo run -f examples/01-parallel-repo-audit.yaml -i target_dir=./examples -i keyword=does-not-match
```

## 02 · Monitored pipe ETL

An inline dataset flows through successive `pipe_from` links. The last step emits JSON with the record count and the first and last sorted values. The workflow renders those fields as text.

```sh
stdo doctor -f examples/02-monitored-pipe-etl.yaml
stdo run -f examples/02-monitored-pipe-etl.yaml
```

## 03 · Release gate

A JSON boolean drives a switch that selects either `approve_release` or `record_incident`. The selected step prints the simulated decision.

```sh
stdo doctor -f examples/03-release-gate-switch.yaml
stdo run -f examples/03-release-gate-switch.yaml -i severity=LOW
stdo run -f examples/03-release-gate-switch.yaml -i severity=CRITICAL
```

## 04 · Architecture & Safety Self-Audit

Three parallel checks inspect this checkout for unsafe Rust, forbidden network dependency declarations, and Rust source size. Per-file line counts feed the module and line totals through `pipe_from`. A JSON boolean routes the dependency result through a switch. The source and manifest checks are textual and report findings in the summary.

```sh
stdo doctor -f examples/04-stdo-zero-network-guard.yaml
stdo run -f examples/04-stdo-zero-network-guard.yaml
```

## 05 · Repo & Documentation Sync Check

Reads the crate version and Cargo exclusions, checks that each workflow is listed in this guide, and looks for the gallery link and integration test file. The release-readiness card includes a Markdown summary and a JSON block.

```sh
stdo doctor -f examples/05-stdo-release-readiness.yaml
stdo run -f examples/05-stdo-release-readiness.yaml
```
