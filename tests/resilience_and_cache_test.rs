use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant, UNIX_EPOCH},
};

struct Workspace(PathBuf);

impl Workspace {
    fn new(yaml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "stdo-resilience-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("stdo.yaml"), yaml).unwrap();
        Self(root)
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stdo"));
        command.current_dir(&self.0).arg(subcommand);
        command
    }

    fn audit(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.0.join("audit.json")).unwrap()).unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn diagnostic(result: &Output) -> String {
    String::from_utf8_lossy(&result.stderr).into_owned()
}

fn assert_success(result: &Output, stdout: &[u8]) {
    assert!(result.status.success(), "{}", diagnostic(result));
    assert_eq!(result.stdout, stdout);
}

#[test]
fn timeout_kills_and_reaps_the_child() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: timeout
steps:
  - id: slow
    cmd: [sleep, '8']
    timeout_ms: 50
"#,
    );
    let started = Instant::now();
    let result = workspace
        .command("run")
        .args(["--report", "audit.json"])
        .output()
        .unwrap();
    let stderr = diagnostic(&result);
    assert!(!result.status.success());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        stderr.contains("step \"slow\" timed out after 50 ms"),
        "{stderr}"
    );
    assert!(stderr.contains("| TimedOut"), "{stderr}");
    assert_eq!(workspace.audit()["steps"][0]["kind"], "timed_out");
}

#[test]
fn retries_nonzero_exit_with_backoff_and_reports_attempts() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: flaky
steps:
  - id: flaky
    cmd: [sh, -c, 'if [ ! -f marker ]; then touch marker; exit 7; fi; printf recovered']
    retry: { attempts: 3, backoff_ms: 75 }
output: '{{ steps.flaky.stdout }}'
"#,
    );
    let result = workspace
        .command("run")
        .args(["-t", "--report", "audit.json"])
        .output()
        .unwrap();
    assert_success(&result, b"recovered");
    assert!(diagnostic(&result).contains("attempts=2"));
    let audit = workspace.audit();
    assert_eq!(audit["steps"][0]["attempts"], 2);
    assert!(audit["steps"][0]["duration_ms"].as_f64().unwrap() >= 75.0);
}

#[test]
fn retry_replays_stdin_and_discards_invalid_jsonl_attempt_output() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: retry-pipe
steps:
  - id: upstream
    cmd: [sh, -c, 'printf "original\n"']
  - id: flaky
    pipe_from: upstream
    cmd:
      - sh
      - -c
      - |
        IFS= read -r line || exit 8
        [ "$line" = original ] || exit 9
        if [ ! -f marker ]; then
          touch marker
          printf '%s\n' '{"attempt":1}' invalid
        else
          printf '%s\n' '{"attempt":2}'
        fi
    retry: { attempts: 2 }
    parse_stdout: jsonl
  - id: downstream
    pipe_from: flaky
    cmd: [cat]
output: '{{ steps.downstream.stdout }}'
"#,
    );
    let result = workspace
        .command("run")
        .args(["--report", "audit.json"])
        .output()
        .unwrap();
    assert_success(&result, b"{\"attempt\":2}\n");
    assert_eq!(workspace.audit()["steps"][1]["attempts"], 2);
}

#[test]
fn retries_after_a_timeout() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: timeout-retry
steps:
  - id: flaky
    cmd: [sh, -c, 'if [ ! -f marker ]; then touch marker; exec sleep 8; fi; printf recovered']
    timeout_ms: 250
    retry: { attempts: 2 }
output: '{{ steps.flaky.stdout }}'
"#,
    );
    let result = workspace.command("run").arg("-t").output().unwrap();
    assert_success(&result, b"recovered");
    assert!(diagnostic(&result).contains("attempts=2"));
}

#[test]
fn cache_keys_track_templated_files_environment_and_executable_metadata() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-dependencies
inputs:
  file: { type: path, default: data.txt }
steps:
  - id: read
    cmd: ['./script', '{{ inputs.file }}']
    cache: true
    cache_files: ['{{ inputs.file }}']
    cache_env: [STDO_TEST_CACHE_ENV]
output: '{{ steps.read.stdout }}'
"#,
    );
    let data = workspace.0.join("data.txt");
    let executable = workspace.0.join("script");
    fs::write(&data, "first").unwrap();
    fs::write(&executable, "#!/bin/sh\nprintf x >> counter\ncat \"$1\"\nprintf ':%s' \"${STDO_TEST_CACHE_ENV-unset}\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let run = |value: Option<&str>, stdout, hit| {
        let mut command = workspace.command("run");
        command.arg("-t").env_remove("STDO_TEST_CACHE_ENV");
        if let Some(value) = value {
            command.env("STDO_TEST_CACHE_ENV", value);
        }
        let result = command.output().unwrap();
        assert_success(&result, stdout);
        assert_eq!(diagnostic(&result).contains("cache hit"), hit);
    };
    run(None, b"first:unset", false);
    run(None, b"first:unset", true);
    fs::write(data, "other").unwrap();
    run(None, b"other:unset", false);
    run(Some(""), b"other:", false);
    run(Some(""), b"other:", true);
    run(Some("value"), b"other:value", false);
    fs::OpenOptions::new()
        .write(true)
        .open(executable)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    run(Some("value"), b"other:value", false);
    run(Some("value"), b"other:value", true);
    assert_eq!(fs::read(workspace.0.join("counter")).unwrap(), b"xxxxx");
}

#[test]
fn rejects_missing_and_outside_cache_files_before_spawning() {
    let workflow = r#"
version: '1.0'
name: invalid-cache-path
steps:
  - id: emit
    cmd: [echo, ok]
    cache: true
    cache_files: [dependency]
output: '{{ steps.emit.stdout }}'
"#;
    let workspace = Workspace::new(workflow);
    let result = workspace.command("run").output().unwrap();
    assert!(!result.status.success());
    assert!(diagnostic(&result).contains("cache dependency \"dependency\""));
    assert!(diagnostic(&result).contains("No processes spawned"));
    symlink(
        workspace.0.parent().unwrap(),
        workspace.0.join("dependency"),
    )
    .unwrap();
    let result = workspace.command("run").output().unwrap();
    assert!(!result.status.success());
    assert!(diagnostic(&result).contains("path escapes the workspace"));
    let bypass = workspace.command("run").arg("--no-cache").output().unwrap();
    assert_success(&bypass, b"ok\n");
}

#[test]
fn doctor_rejects_invalid_resilience_options() {
    for (options, error) in [
        ("timeout_ms: 0", "timeout_ms must be greater than 0"),
        (
            "retry: { attempts: 0 }",
            "retry.attempts must be between 1 and 10",
        ),
        (
            "retry: { attempts: 11 }",
            "retry.attempts must be between 1 and 10",
        ),
        (
            "retry: { attempts: 1, backoff_ms: 60001 }",
            "retry.backoff_ms must be at most 60000",
        ),
    ] {
        let workspace = Workspace::new(&format!(
            "version: '1.0'\nname: invalid-options\nsteps:\n  - id: bad\n    cmd: [echo]\n    {options}\n"
        ));
        let result = workspace.command("doctor").output().unwrap();
        assert!(!result.status.success());
        let stderr = diagnostic(&result);
        assert!(stderr.contains(error), "{stderr}");
    }
}
