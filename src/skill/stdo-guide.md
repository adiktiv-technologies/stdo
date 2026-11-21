### Write and run a workflow

A workflow is a YAML document with `version: "1.0"`, a `name`, and a list of `steps` with unique IDs. Optional fields are `description`, `inputs` and `output`.

<!-- workflow-example -->

### Design the DAG

- Inputs support `string`, `path`, `enum`, `number` and `boolean`. Declare enum choices in `values`, an optional `default`, and an optional `description`. Supply required values at execution with repeated `-i key=value`.
- A command step uses `cmd`, an array of executable and argv strings. Each entry is one argument. Shell operators take effect inside an explicitly invoked shell. For shell scripts, pass input values as positional arguments, e.g. `[sh, -c, 'printf "%s" "$1"', sh, "{{ inputs.message }}"]`.
- Step IDs are dependency references. Independent steps can run in parallel. Use MiniJinja in `stdin` for buffered fan-in, e.g. `stdin: "{{ steps.a.stdout }}{{ steps.b.stdout }}"`. Command argv templates see workflow inputs. The final `output` template selects what reaches workflow stdout.
- Use `pipe_from: source_id` to route a single producer into stdin. Concurrent pipes stream with backpressure when eligible. Cache lookup, retries or additional dependencies can require buffering. Choose either `stdin` or `pipe_from` on a consumer.
- Use `pipe_mode: atomic` when the consumer must wait for a successful, validated producer. This requires captured upstream output. For atomic file replacement, have the writing step stage a sibling file and rename it on success.
- Capture is enabled by default. For large streams use `capture_stdout: false` and a live consumer. Uncaptured output cannot be cached, replayed, referenced as stdout/JSON, or parsed as a whole JSON document. Unpiped, uncaptured stdout is discarded.
- Use `parse_stdout: json` for an entire document or `jsonl` for line-delimited records. JSONL ignores blank lines, validates and compacts each record (64 KiB per record), and reports the failing line. Access captured values as `{{ steps.scan.json.field }}` or `{{ steps.scan.json[0] }}`.
- To branch, use a step with `type: switch`, a templated `condition`, and `cases` mapping rendered strings to step IDs, e.g. `cases: {"true": approve, "false": reject}`. Guard references to skipped outputs in the final template.
- To compose workflows, invoke a child explicitly, e.g. `cmd: [stdo, run, -f, child.yaml]`. Relative child paths use the process working directory. Doctor follows static calls and checks cycles; runtime also guards recursion. Static inspection is bounded to 16 nested files and 64 calls.
- Graph defaults allow 64 steps, 16 layers and 16 steps per layer. `--force` on run, doctor or top overrides graph size limits. Streams captured in memory are limited to 8 MiB each, with a shared 32 MiB monitored-buffer budget. For large data, stream into a consumer or write a file.

### Execution, caching and failures

- A nonzero command exit normally fails the run. Use `ignore_exit_code: true` only for expected nonzero outcomes, then inspect `steps.<id>.exit_code`. On failure, read the error, captured diagnostics and PID ledger on stderr. Keep stdout for the rendered result.
- Set `timeout_ms` to a positive per-attempt deadline including pipe I/O. A timeout kills the process group and reaps the direct child. `retry: {attempts: 3, backoff_ms: 250}` allows three total attempts. Valid ranges are 1–10 attempts and 0–60000 ms fixed backoff. Retried failures include non-ignored exits, timeouts and invalid JSON/JSONL. Make external writes safe to repeat.
- Use `cache: true` for deterministic steps. Cache keys under `.stdo/cache/` cover argv, stdin, executable identity and output/exit settings. Declare file contents via `cache_files` (regular files inside the working directory) and environment dependencies via `cache_env`. Use `--no-cache` to force execution and bypass writes. A cache hit reuses captured output and skips the command's side effects.
- The orchestrator runs locally. Child commands inherit the host's capabilities and can use network services or modify files. Run workflows from a directory containing their referenced files and review commands before executing unfamiliar YAML.
