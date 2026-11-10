use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use stdo::{
    dag::graph::{MAX_DEPTH, MAX_LAYER_WIDTH, MAX_STEPS},
    engine::runner::{MAX_STEP_OUTPUT_BYTES, MAX_WORKFLOW_MEMORY_BYTES},
};

struct WorkflowFile(PathBuf);

impl WorkflowFile {
    fn new(yaml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "stdo-stress-{}-{}.yaml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, yaml).unwrap();
        Self(path)
    }

    fn execute(&self, subcommand: &str, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
            .arg(subcommand)
            .arg(&self.0)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("stdo {subcommand} did not finish within ten seconds");
            }
            thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }
}

impl Drop for WorkflowFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_process(report: &str, step: &str, state: &str) {
    assert!(
        report.lines().any(|line| {
            let Some(row) = line.strip_prefix("  PID ") else {
                return false;
            };
            let Some((pid, _)) = row.split_once(" | ") else {
                return false;
            };
            pid.parse::<u32>().is_ok_and(|pid| pid != 0)
                && row.contains(&format!(" | step {step:?} | "))
                && row.ends_with(&format!(" | {state}"))
        }),
        "{report}"
    );
}

#[test]
fn graph_limits_require_force_in_run_and_doctor() {
    let mut yaml = String::from("version: '1.0'\nname: graph-limits\nsteps:\n");
    for i in 0..=MAX_DEPTH {
        writeln!(yaml, "  - id: deep_{i}\n    cmd: ['true']").unwrap();
        if i != 0 {
            writeln!(yaml, "    pipe_from: deep_{}", i - 1).unwrap();
        }
    }
    for i in 0..=MAX_STEPS {
        writeln!(yaml, "  - id: wide_{i}\n    cmd: ['true']").unwrap();
    }
    let workflow = WorkflowFile::new(&yaml);
    for subcommand in ["run", "doctor"] {
        let rejected = workflow.execute(subcommand, &[]);
        assert!(!rejected.status.success());
        let report = stderr(&rejected);
        assert!(report.contains("graph safety limits exceeded"), "{report}");
        assert!(report.contains("pass --force"), "{report}");
        assert!(
            report.contains(&format!("max_width: {}/{}", MAX_STEPS + 2, MAX_LAYER_WIDTH)),
            "{report}"
        );

        let accepted = workflow.execute(subcommand, &["--force"]);
        let report = stderr(&accepted);
        assert!(accepted.status.success(), "{report}");
        assert!(report.contains(&format!(
            "WARNING: Graph safety limits bypassed with --force (steps: {}, depth: {}, max_width: {})",
            MAX_STEPS + MAX_DEPTH + 2, MAX_DEPTH + 1, MAX_STEPS + 2,
        )), "{report}");
    }
}

#[test]
fn watchdog_kills_a_process_exceeding_either_stream_limit() {
    for cmd in ["[yes]", "[sh, -c, 'exec yes >&2']"] {
        let workflow = WorkflowFile::new(&format!(
            "version: '1.0'\nname: stream-limit\nsteps:\n  - id: flood\n    cmd: {cmd}\n"
        ));
        let output = workflow.execute("run", &[]);
        let report = stderr(&output);
        assert!(!output.status.success());
        assert!(
            report.contains(&format!("memory limit of {MAX_STEP_OUTPUT_BYTES} bytes")),
            "{report}"
        );
        assert!(
            report.contains("Spawned Process Report (PID Ledger)"),
            "{report}"
        );
        assert_process(&report, "flood", "Killed");
    }
}

#[test]
fn retained_parallel_output_counts_against_the_workflow_budget() {
    let mut yaml = String::from("version: '1.0'\nname: global-budget\nsteps:\n");
    for i in 0..5 {
        writeln!(
            yaml,
            "  - id: flood_{i}\n    cmd: [head, -c, '7340032', /dev/zero]"
        )
        .unwrap();
    }
    let output = WorkflowFile::new(&yaml).execute("run", &[]);
    let report = stderr(&output);
    assert!(!output.status.success());
    assert!(
        report.contains(&format!(
            "memory limit of {MAX_WORKFLOW_MEMORY_BYTES} bytes"
        )),
        "{report}"
    );
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("  PID "))
            .count(),
        5,
        "{report}"
    );
    assert!(!report.contains("StillRunningCheckManually"), "{report}");
}

#[test]
fn failing_process_cancels_and_reaps_its_parallel_peer() {
    let workflow = WorkflowFile::new(
        r#"
version: '1.0'
name: parallel-failure
steps:
  - id: fail
    cmd: [sh, -c, 'exit 7']
  - id: slow
    cmd: [sleep, '30']
"#,
    );
    let output = workflow.execute("run", &[]);
    let report = stderr(&output);
    assert!(!output.status.success());
    assert!(
        report.contains("Spawned Process Report (PID Ledger)"),
        "{report}"
    );
    assert_process(&report, "fail", "Exited(7)");
    assert_process(&report, "slow", "Killed");
}

#[test]
fn runs_templates_pipes_json_and_switches_end_to_end() {
    let workflow = WorkflowFile::new(
        r#"
version: '1.0'
name: end-to-end
inputs:
  label:
    type: string
    default: default
steps:
  - id: seed
    cmd: [echo, '{"ok":true,"label":"{{ inputs.label }}"}']
  - id: json
    cmd: [grep, ok]
    pipe_from: seed
    parse_stdout: json
  - id: count
    cmd: [wc, -l]
    stdin: |
      {{ steps.json.json.label }}
      {{ steps.json.json.ok }}
  - id: route
    type: switch
    condition: '{{ steps.json.json.ok }}'
    cases:
      'true': approved
      'false': rejected
  - id: approved
    cmd: [sh, -c, 'cat']
    stdin: '{{ steps.json.json.label }}:{{ steps.count.stdout | trim }}'
  - id: rejected
    cmd: [sh, -c, 'exit 99']
  - id: rejected_child
    cmd: [sh, -c, 'exit 98']
    pipe_from: rejected
output: '{{ steps.approved.stdout }}'
"#,
    );
    let output = workflow.execute("run", &["-i", "label=ready", "-t"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, b"ready:2");
    let report = stderr(&output);
    assert!(report.contains("switch -> approved"), "{report}");
    assert_eq!(
        report
            .matches("| skipped | stdout_bytes=0 stderr_bytes=0")
            .count(),
        2
    );
    assert!(
        report.contains("executed=4 switches=1 skipped=2"),
        "{report}"
    );
}

#[test]
fn template_failure_after_an_earlier_layer_reports_the_exited_pid() {
    let workflow = WorkflowFile::new(
        r#"
version: '1.0'
name: template-error
steps:
  - id: seed
    cmd: [echo, hello]
  - id: failure
    cmd: [cat]
    stdin: '{{ steps.seed.stdout }} {{ inputs.missing }}'
"#,
    );
    let output = workflow.execute("run", &[]);
    let report = stderr(&output);
    assert!(!output.status.success());
    assert!(report.contains("invalid template"), "{report}");
    assert_process(&report, "seed", "Exited(0)");
}

#[test]
fn telemetry_reports_timings_and_bytes_without_changing_stdout() {
    let workflow = WorkflowFile::new(
        r#"
version: '1.0'
name: telemetry
steps:
  - id: emit
    cmd: [sh, -c, 'printf hello; printf note >&2']
output: '{{ steps.emit.stdout }}'
"#,
    );
    let default = workflow.execute("run", &[]);
    assert!(default.status.success());
    assert!(default.stderr.is_empty());

    let output = workflow.execute("run", &["--telemetry"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, default.stdout);
    assert_eq!(output.stdout, b"hello");
    let report = stderr(&output);
    assert!(
        report.contains("Execution Telemetry (completed)"),
        "{report}"
    );
    for prefix in ["  Layer 1: ", "    Step \"emit\": ", "  Total: "] {
        let line = report
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap();
        let (duration, _) = line.split_once(" ms").unwrap();
        let (whole, fraction) = duration.split_once('.').unwrap();
        assert!(
            whole.parse::<u64>().is_ok() && fraction.len() == 2 && fraction.parse::<u8>().is_ok()
        );
    }
    assert!(
        report.contains("exit 0 | stdout_bytes=5 stderr_bytes=4"),
        "{report}"
    );
    assert!(
        report.contains("executed=1 switches=0 skipped=0"),
        "{report}"
    );
    let (_, usage) = report.split_once("peak_bytes=").unwrap();
    let (peak, limit) = usage.split_once(" limit_bytes=").unwrap();
    let peak = peak.parse::<usize>().unwrap();
    assert!((9..=MAX_WORKFLOW_MEMORY_BYTES).contains(&peak), "{report}");
    assert_eq!(
        limit.trim().parse::<usize>().unwrap(),
        MAX_WORKFLOW_MEMORY_BYTES
    );
}

#[test]
fn failed_step_prints_trimmed_stderr_before_the_pid_ledger() {
    let workflow = WorkflowFile::new(
        r#"
version: '1.0'
name: telemetry-failure
steps:
  - id: fail
    cmd: [sh, -c, 'printf "  missing manifest\n\n" >&2; exit 2']
"#,
    );
    for args in [&[][..], &["-t"][..]] {
        let output = workflow.execute("run", args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let report = stderr(&output);
        assert!(
            report.contains("failed with exit status: 2\nmissing manifest\nSpawned Process Report"),
            "{report}"
        );
        assert_process(&report, "fail", "Exited(2)");
        if !args.is_empty() {
            assert!(
                report.contains("Execution Telemetry (partial; failed)"),
                "{report}"
            );
            assert!(
                report.contains("exit 2 | stdout_bytes=0 stderr_bytes=20"),
                "{report}"
            );
        }
    }
}
