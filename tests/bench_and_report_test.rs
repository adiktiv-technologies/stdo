use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("stdo-report-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn report(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn assert_no_temporary_files(&self) {
        assert!(fs::read_dir(&self.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".stdo-save-")
        }));
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invoke(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stdo"))
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn run_exports_exact_provenance_and_first_byte_timings_as_json_and_html() {
    let workspace = Workspace::new();
    let yaml = r#"
version: '1.0'
name: 'audit <check>'
inputs:
  value: {type: string, default: initial}
steps:
  - id: emit
    cmd: [sh, -c, 'printf "%s" "$1"; sleep 1', stdo, '{{ inputs.value }}']
  - id: empty
    cmd: ['true']
output: '{{ steps.emit.stdout }}'
"#;
    fs::write(workspace.report("stdo.yaml"), yaml).unwrap();

    let output = invoke(
        &workspace.0,
        &["run", "-i", "value=hello", "--report", "audit.json", "-t"],
    );
    assert_success(&output);
    assert_eq!(output.stdout, b"hello");
    assert!(String::from_utf8_lossy(&output.stderr).contains("ttfb="));

    let audit = read_json(&workspace.report("audit.json"));
    assert_eq!(audit["status"], "completed");
    assert_eq!(audit["workflow_name"], "audit <check>");
    assert_eq!(audit["workflow_version"], "1.0");
    assert_eq!(audit["workflow_path"], "stdo.yaml");
    assert_eq!(audit["workflow_sha256"], sha256(yaml.as_bytes()));
    assert_eq!(audit["inputs"], json!({"value": "hello"}));
    assert_eq!(audit["inputs_sha256"], sha256(br#"{"value":"hello"}"#));
    assert_eq!(audit["stdout_bytes"], 5);
    assert_eq!(audit["stdout_sha256"], sha256(b"hello"));
    assert_eq!(audit["layers"][0]["layer"], 1);
    let emit = &audit["steps"][0];
    let ttfb = emit["ttfb_ms"].as_f64().unwrap();
    assert!(ttfb >= 0.0 && ttfb < emit["duration_ms"].as_f64().unwrap());
    assert_eq!(emit["stdout_sha256"], sha256(b"hello"));
    assert!(audit["steps"][1]["ttfb_ms"].is_null());
    assert!(audit["ttfb_ms"].as_f64().unwrap() < audit["duration_ms"].as_f64().unwrap());

    let output = invoke(&workspace.0, &["run", "--report", "audit.htm"]);
    assert_success(&output);
    assert_eq!(output.stdout, b"initial");
    assert!(output.stderr.is_empty());
    let html = fs::read_to_string(workspace.report("audit.htm")).unwrap();
    for text in [
        "<!doctype html>",
        "<style>",
        "audit &lt;check&gt;",
        "workflow_sha256",
        "stdout_sha256",
        "ttfb_ms",
    ] {
        assert!(html.contains(text), "{text}");
    }
    assert!(!html.contains("<script") && !html.contains("href=\"http"));
    workspace.assert_no_temporary_files();
}

#[test]
fn failed_run_writes_a_partial_audit_and_uses_duration_for_empty_stdout() {
    let workspace = Workspace::new();
    fs::write(
        workspace.report("stdo.yaml"),
        r#"
version: '1.0'
name: failed
steps:
  - id: fail
    cmd: [sh, -c, 'printf diagnostic >&2; exit 7']
"#,
    )
    .unwrap();
    let output = invoke(&workspace.0, &["run", "--report", "audit.data"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Spawned Process Report"));
    let report = read_json(&workspace.report("audit.data"));
    assert_eq!(report["status"], "failed");
    assert_eq!(report["ttfb_ms"], report["duration_ms"]);
    assert_eq!(report["stdout_sha256"], sha256(b""));
    assert_eq!(report["steps"][0]["status"], "exit 7");
    assert_eq!(report["steps"][0]["stderr_bytes"], 10);
    workspace.assert_no_temporary_files();
}

#[test]
fn benchmark_compares_examples_and_exports_measured_samples_and_charts() {
    let workspace = Workspace::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = workspace.report("bench.json");
    let args = [
        "bench",
        "examples/01-parallel-repo-audit.yaml",
        "examples/02-monitored-pipe-etl.yaml",
        "--runs",
        "2",
        "--warmup",
        "1",
        "--no-cache",
        "--report",
        path.to_str().unwrap(),
    ];
    let output = invoke(root, &args);
    assert_success(&output);
    let table = String::from_utf8(output.stdout).unwrap();
    for text in [
        "cache: bypassed",
        "p50",
        "p95",
        "ttfb",
        "peak_bytes",
        "A  examples/",
        "B  examples/",
        "baseline",
        "per-step timings",
    ] {
        assert!(table.contains(text), "{table}");
    }
    let data = read_json(&path);
    assert_eq!(data["runs"], 2);
    assert_eq!(data["warmup"], 1);
    assert_eq!(data["cache"], "bypassed");
    let workflows = data["workflows"].as_array().unwrap();
    assert_eq!(workflows.len(), 2);
    for workflow in workflows {
        assert_eq!(workflow["samples"].as_array().unwrap().len(), 2);
        assert_eq!(workflow["cached_steps"], 0);
        assert!(workflow["ttfb_mean_ms"].is_number());
        assert!(workflow["peak_bytes_max"].as_u64().unwrap() > 0);
        let stats = &workflow["latency"];
        assert!(stats["min_ms"].as_f64().unwrap() <= stats["p50_ms"].as_f64().unwrap());
        assert!(stats["p50_ms"].as_f64().unwrap() <= stats["p95_ms"].as_f64().unwrap());
        assert!(stats["p95_ms"].as_f64().unwrap() <= stats["max_ms"].as_f64().unwrap());
    }

    let html = workspace.report("bench.html");
    let mut args = args;
    args[9] = html.to_str().unwrap();
    assert_success(&invoke(root, &args));
    let html = fs::read_to_string(html).unwrap();
    assert_eq!(html.matches("<svg ").count(), 4);
    assert!(html.contains("Mean TTFB") && html.contains("Peak monitored bytes"));
    workspace.assert_no_temporary_files();
}

#[test]
fn benchmark_uses_warm_cache_and_rejects_zero_runs() {
    let workspace = Workspace::new();
    fs::write(
        workspace.report("stdo.yaml"),
        r#"
version: '1.0'
name: warm-cache
steps:
  - id: emit
    cmd: [sh, -c, 'printf x >> counter; printf hello']
    cache: true
"#,
    )
    .unwrap();
    let output = invoke(&workspace.0, &["bench", "stdo.yaml", "--runs", "0"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--runs >= 1"));
    assert!(!workspace.report("counter").exists());

    let output = invoke(
        &workspace.0,
        &[
            "bench",
            "stdo.yaml",
            "--runs",
            "2",
            "--report",
            "bench.json",
        ],
    );
    assert_success(&output);
    assert_eq!(fs::read(workspace.report("counter")).unwrap(), b"x");
    let bench = read_json(&workspace.report("bench.json"));
    assert_eq!(bench["cache"], "enabled");
    assert_eq!(bench["workflows"][0]["cache_hit_ratio"], 1.0);
    let output = invoke(&workspace.0, &["run", "--report", "audit.json"]);
    assert_success(&output);
    let audit = read_json(&workspace.report("audit.json"));
    assert_eq!(audit["steps"][0]["status"], "cache hit (exit 0)");
    assert!(audit["steps"][0]["ttfb_ms"].is_number());
}
