use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

struct Workspace(PathBuf);

impl Workspace {
    fn new(yaml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "stdo-live-pipe-{}-{}",
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
            .args(["run", "--report", "audit.json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("streaming workflow did not complete");
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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn consumer_receives_a_normalized_line_while_producer_is_running() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: live-pipe
steps:
  - id: producer
    cmd:
      - sh
      - -c
      - |
        printf '%s\n' ' { "value" : 42 }'
        sleep 0.25
        for attempt in 1 2 3 4 5; do
          [ ! -f consumed ] || exit 0
          sleep 0.1
        done
        exit 9
    parse_stdout: jsonl
  - id: consumer
    cmd: [sh, -c, 'IFS= read -r line || exit 3; printf "%s\n" "$line"; touch consumed; cat']
    pipe_from: producer
output: '{{ steps.consumer.stdout }}'
"#,
    );
    let output = workspace.run();
    assert_success(&output);
    assert_eq!(output.stdout, b"{\"value\":42}\n");
    let audit = workspace.audit();
    let producer = &audit["steps"][0];
    let consumer = &audit["steps"][1];
    assert_eq!(producer["id"], "producer");
    assert_eq!(consumer["id"], "consumer");
    assert_eq!(consumer["layer"], 2);
    assert!(consumer["ttfb_ms"].as_f64().unwrap() < producer["duration_ms"].as_f64().unwrap());
}

#[test]
fn cached_producers_replay_to_live_consumers_and_cached_consumers() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cached-pipes
steps:
  - id: producer
    cmd: [sh, -c, 'printf x >> producer-count; printf "one\ntwo\n"']
    cache: true
  - id: live
    cmd: [cat]
    pipe_from: producer
  - id: cached
    cmd: [sh, -c, 'printf x >> consumer-count; cat']
    pipe_from: producer
    cache: true
output: '{{ steps.live.stdout }}{{ steps.cached.stdout }}'
"#,
    );
    for _ in 0..2 {
        let output = workspace.run();
        assert_success(&output);
        assert_eq!(output.stdout, b"one\ntwo\none\ntwo\n");
    }
    assert_eq!(fs::read(workspace.0.join("producer-count")).unwrap(), b"x");
    assert_eq!(fs::read(workspace.0.join("consumer-count")).unwrap(), b"x");
    let audit = workspace.audit();
    assert_eq!(
        audit["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["cache_hit"] == true)
            .count(),
        2
    );
}

#[test]
fn upstream_failure_wins_over_a_consumer_that_rejects_short_input() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: failed-pipe
steps:
  - id: producer
    cmd: [sh, -c, 'printf "one\n"; exit 7']
  - id: consumer
    cmd: [sh, -c, 'read -r first; read -r second || exit 8']
    pipe_from: producer
"#,
    );
    let result = workspace.run();
    assert!(!result.status.success());
    let report = String::from_utf8_lossy(&result.stderr);
    assert!(
        report.contains("step \"producer\" failed with exit status: 7"),
        "{report}"
    );
    assert_eq!(workspace.audit()["steps"][1]["status"], "cancelled");
}

#[test]
fn an_early_consumer_exit_does_not_starve_other_consumers() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: fan-out
steps:
  - id: producer
    cmd: [head, -c, '262144', /dev/zero]
  - id: early
    cmd: [head, -c, '1']
    pipe_from: producer
  - id: counter
    cmd: [wc, -c]
    pipe_from: producer
output: '{{ steps.counter.stdout | trim }}'
"#,
    );
    let result = workspace.run();
    assert_success(&result);
    assert_eq!(result.stdout, b"262144");
}
