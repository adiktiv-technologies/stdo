use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

struct Workspace(PathBuf);

impl Workspace {
    fn new(yaml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "stdo-qualification-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("stdo.yaml"), yaml).unwrap();
        Self(path)
    }

    fn run(&self) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
            .current_dir(&self.0)
            .args(["run", "-t", "--report", "audit.json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("qualification workflow exceeded its test deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }

    fn audit(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("audit.json")).unwrap()).unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_success(result: &Output, stdout: &[u8]) {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, stdout);
}

#[test]
fn passes_through_twelve_mib_and_hashes_without_retaining_stdout() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: large-pass-through
steps:
  - id: producer
    cmd: [dd, if=/dev/zero, bs=65536, count=192]
    capture_stdout: false
  - id: consumer
    cmd: [wc, -c]
    pipe_from: producer
output: '{{ steps.consumer.stdout | trim }}'
"#,
    );
    assert_success(&workspace.run(), b"12582912");
    let audit = workspace.audit();
    assert!(audit["peak_bytes"].as_u64().unwrap() < 512 * 1024);
    let producer = &audit["steps"][0];
    assert_eq!(producer["stdout_bytes"], 12 * 1024 * 1024);
    assert!(producer["ttfb_ms"].is_number());
    let mut digest = Sha256::new();
    for _ in 0..192 {
        digest.update([0; 65536]);
    }
    assert_eq!(
        producer["stdout_sha256"],
        format!("{:x}", digest.finalize())
    );
}

#[test]
fn atomic_pipe_prevents_side_effects_after_partial_failure() {
    let yaml = r#"
version: '1.0'
name: atomic-pipe
steps:
  - id: producer
    cmd:
      - sh
      - -c
      - |
        printf 'partial\n'
        for attempt in 1 2 3 4 5; do
          [ ! -f side-effect ] || break
          sleep 0.05
        done
        exit 7
  - id: consumer
    cmd: [sh, -c, 'IFS= read -r line && printf "%s" "$line" > side-effect; cat']
    pipe_from: producer
    pipe_mode: MODE
"#;
    for (mode, ignore, expected_effect) in [
        ("concurrent", false, true),
        ("atomic", false, false),
        ("atomic", true, false),
    ] {
        let workflow = yaml.replace("MODE", mode).replace(
            "  - id: consumer",
            &format!("    ignore_exit_code: {ignore}\n  - id: consumer"),
        );
        let workspace = Workspace::new(&workflow);
        let result = workspace.run();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{stderr}");
        assert!(stderr.contains("exit status: 7"), "{stderr}");
        assert_eq!(
            workspace.0.join("side-effect").exists(),
            expected_effect,
            "{stderr}"
        );
        if ignore {
            assert!(stderr.contains("cannot start its atomic pipe"), "{stderr}");
        }
        if mode == "atomic" {
            assert!(!stderr.contains(" | step \"consumer\" | "), "{stderr}");
        }
    }
}

#[test]
fn a_slow_reader_applies_backpressure_to_a_fast_producer() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: slow-reader
steps:
  - id: producer
    cmd: [dd, if=/dev/zero, bs=65536, count=192]
    capture_stdout: false
  - id: consumer
    cmd: [sh, -c, 'sleep 0.25; exec wc -c']
    pipe_from: producer
output: '{{ steps.consumer.stdout | trim }}'
"#,
    );
    assert_success(&workspace.run(), b"12582912");
    let audit = workspace.audit();
    assert!(audit["peak_bytes"].as_u64().unwrap() < 512 * 1024);
    assert!(audit["steps"][0]["duration_ms"].as_f64().unwrap() >= 200.0);
}

#[test]
fn timeout_terminates_a_background_descendant() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: timeout-group
steps:
  - id: shell
    cmd:
      - sh
      - -c
      - |
        sleep 30 &
        (
          for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
            printf . >> descendant.heartbeat
            sleep 0.1
          done
        ) &
        wait
    timeout_ms: 150
"#,
    );
    let started = Instant::now();
    let result = workspace.run();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(stderr.contains("timed out after 150 ms"), "{stderr}");
    assert!(stderr.contains("TimedOut"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let heartbeat = workspace.0.join("descendant.heartbeat");
    let before = fs::read(&heartbeat).unwrap();
    assert!(!before.is_empty());
    thread::sleep(Duration::from_millis(350));
    assert_eq!(
        fs::read(&heartbeat).unwrap(),
        before,
        "descendant kept running"
    );
}

#[test]
fn uncaptured_jsonl_still_validates_and_normalizes_records() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: streamed-jsonl
steps:
  - id: producer
    cmd: [sh, -c, 'printf "%s\n" " { \"sample\" : 42 } "']
    capture_stdout: false
    parse_stdout: jsonl
  - id: consumer
    cmd: [cat]
    pipe_from: producer
output: '{{ steps.consumer.stdout }}'
"#,
    );
    assert_success(&workspace.run(), b"{\"sample\":42}\n");
    assert_eq!(workspace.audit()["steps"][0]["stdout_bytes"], 14);
}

#[test]
fn uncaptured_output_cannot_be_replayed_or_referenced() {
    for consumer in [
        "output: '{{ steps.emit.stdout }}'\n",
        "output: '{{ steps.emit.json }}'\n",
        "  - id: consumer\n    cmd: [cat]\n    pipe_from: emit\n    pipe_mode: atomic\n",
        "  - id: consumer\n    cmd: [cat]\n    pipe_from: emit\n    cache: true\n",
        "  - id: consumer\n    cmd: [cat]\n    pipe_from: emit\n    retry: { attempts: 2 }\n",
    ] {
        let yaml = format!("version: '1.0'\nname: invalid\nsteps:\n  - id: emit\n    cmd: [echo]\n    capture_stdout: false\n{consumer}");
        let workspace = Workspace::new(&yaml);
        let result = workspace.run();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success());
        assert!(stderr.contains("capture_stdout: false"), "{stderr}");
        assert!(stderr.contains("No processes spawned"), "{stderr}");
    }
}

#[test]
fn executable_changes_invalidate_cache_with_identical_size_and_mtime() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: executable-content
steps:
  - id: run
    cmd: ['./script']
    cache: true
output: '{{ steps.run.stdout }}'
"#,
    );
    let executable = workspace.0.join("script");
    fs::write(&executable, "#!/bin/sh\nprintf old\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mtime = fs::metadata(&executable).unwrap().modified().unwrap();
    assert_success(&workspace.run(), b"old");
    assert_success(&workspace.run(), b"old");
    assert_eq!(workspace.audit()["steps"][0]["cache_hit"], true);
    fs::write(&executable, "#!/bin/sh\nprintf new\n").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_success(&workspace.run(), b"new");
    assert_eq!(workspace.audit()["steps"][0]["cache_hit"], false);
}
