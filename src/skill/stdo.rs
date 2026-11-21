//! Guidance for the running binary, paired with the command tree from its help.
use super::introspect::executable;
use std::{fs, os::unix::fs::MetadataExt};

pub(super) fn is_current(target: &str) -> bool {
    let current = std::env::current_exe().and_then(fs::metadata);
    let target = executable(target).and_then(|p| fs::metadata(p).ok());
    matches!((current,target),(Ok(a),Some(b)) if a.dev()==b.dev() && a.ino()==b.ino())
}

pub(super) const DESCRIPTION: &str = "Use stdo to author, validate, execute, supervise and benchmark local Unix DAG workflows, inspect them in Studio, and generate CLI skills.";
pub(super) const GUIDE: &str = include_str!("stdo-guide.md");
pub(super) const WORKFLOW_EXAMPLE: &str = concat!(
    "Save this as `stdo.yaml` in the working directory.\n\n```yaml\n",
    include_str!("../../examples/12-count-words.yaml"),
    "```\n\nRun `stdo doctor -f stdo.yaml` to check the graph and executables. ",
    "Then run `stdo run -f stdo.yaml -i 'message=one two'`. Expected stdout: `2`."
);

pub(super) fn command(name: &str) -> Option<&'static str> {
    Some(match name {
        "doctor" => "Use before running a new or edited workflow. Inspect missing programs, bad references, cycles and graph limits. Listed Ollama models still need a local availability check.",
        "run" => "Choose run when another program needs the rendered output. Telemetry goes to stderr. The report includes timings, hashes, resolved inputs and partial results on failure. Treat reports containing sensitive inputs accordingly. Use .html or .htm for a self-contained HTML audit.",
        "top" => "Choose top for live per-step state, PID, time to first byte, duration, bytes, attempts and cache hits on stderr. Workflow output goes to stdout. The last pass stays on screen. Press r to reread YAML, q or Ctrl+C to quit. Use one-pass mode in scripts. Automatic repetition can replay external writes.",
        "bench" => "Put the baseline workflow first and give the candidates equivalent inputs. Compare p50/p95 latency, mean time to first stdout, peak monitored bytes and cache hit ratio. Warmups can populate the cache; use --no-cache for execution cost. Monitored bytes measure engine buffers, distinct from OS RSS.",
        "serve" => "Visit the loopback URL printed at startup. Use the graph editor or YAML view, inspect Requirements, and run to see step output and telemetry. The offline UI serves files in the workspace and can preview supported artifacts.",
        "skill" => "Discover with `stdo skill -l -- gh pr`, preview with `stdo skill -o - -- gh pr`, and install in the current Git repository with `stdo skill -R -- gh pr`. For a selection: `sel=$(stdo skill -l -- gh pr | fzf -m) && printf '%s\\n' \"$sel\" | stdo skill -R -- gh pr`. Empty stdin explores normally; nonempty stdin selects listed paths. File output is silent. Global scope uses SKILL_GLOBAL_DIR or the XDG data skills directory; repo scope defaults to .agents/skills. Dry-run returns 3 for drift, 0 when current. Unmanaged files require confirmation or --force. Other codes: 1 runtime/I/O, 2 usage, 65 invalid data or repo, 69 missing CLI, 73 unmanaged conflict, 75 timeout.",
        _=>return None,
    })
}
