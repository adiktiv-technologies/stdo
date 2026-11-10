use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Workspace(PathBuf);

impl Workspace {
    fn new(yaml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "stdo-cas-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("stdo.yaml"), yaml).unwrap();
        Self(root)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_stdo"))
            .current_dir(&self.0)
            .arg("run")
            .args(args)
            .output()
            .unwrap()
    }

    fn entries(&self) -> Vec<PathBuf> {
        fs::read_dir(self.0.join(".stdo/cache")).map_or_else(
            |error| {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                Vec::new()
            },
            |entries| entries.map(|entry| entry.unwrap().path()).collect(),
        )
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_run(result: Output, stdout: &[u8], status: &str) {
    let report = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{report}");
    assert_eq!(result.stdout, stdout);
    assert!(report.contains(status), "{report}");
}

#[test]
fn cached_steps_skip_the_subprocess_and_key_on_resolved_stdin() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-test
inputs:
  value: { type: string, default: alpha }
steps:
  - id: save
    cmd: [sh, -c, 'printf x >> counter; cat; printf diagnostic >&2']
    stdin: '{{ inputs.value }}'
    cache: true
output: '{{ steps.save.stdout }}'
"#,
    );
    let counter = workspace.0.join("counter");

    assert_run(workspace.run(&["-t", "--no-cache"]), b"alpha", "cached=0");
    assert!(workspace.entries().is_empty());
    assert_eq!(fs::read(&counter).unwrap(), b"x");

    assert_run(workspace.run(&["-t"]), b"alpha", "cached=0");
    let first_entry = workspace.entries().pop().unwrap();
    let key = first_entry.file_name().unwrap().to_str().unwrap();
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let original = fs::read(&first_entry).unwrap();
    assert_eq!(fs::read(&counter).unwrap(), b"xx");

    let hit = workspace.run(&["-t"]);
    let report = String::from_utf8_lossy(&hit.stderr);
    assert!(report.contains("executed=0"), "{report}");
    assert!(report.contains("cached=1"), "{report}");
    assert_run(
        hit,
        b"alpha",
        "cache hit (exit 0) | stdout_bytes=5 stderr_bytes=10",
    );
    assert_eq!(fs::read(&counter).unwrap(), b"xx");

    assert_run(
        workspace.run(&["-t", "-i", "value=beta"]),
        b"beta",
        "cached=0",
    );
    assert_eq!(workspace.entries().len(), 2);
    assert_eq!(fs::read(&counter).unwrap(), b"xxx");

    assert_run(workspace.run(&["-t", "--no-cache"]), b"alpha", "cached=0");
    assert_eq!(workspace.entries().len(), 2);
    assert_eq!(fs::read(&first_entry).unwrap(), original);
    assert_eq!(fs::read(&counter).unwrap(), b"xxxx");
}

#[test]
fn cache_stores_ignored_exit_codes_only_after_json_validation() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-json
inputs:
  payload: { type: string, default: '{"answer":42}' }
steps:
  - id: emit
    cmd: [sh, -c, 'cat; exit 7']
    stdin: '{{ inputs.payload }}'
    parse_stdout: json
    ignore_exit_code: true
    cache: true
output: '{{ steps.emit.json.answer }}:{{ steps.emit.exit_code }}'
"#,
    );
    let invalid = workspace.run(&["-i", "payload=invalid"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("produced invalid JSON"));
    assert!(workspace.entries().is_empty());

    assert_run(workspace.run(&["-t"]), b"42:7", "cached=0");
    assert_run(workspace.run(&["-t"]), b"42:7", "cache hit (exit 7)");
    assert_eq!(workspace.entries().len(), 1);
}

#[test]
fn oversized_cache_entry_is_rejected_by_the_stream_budget() {
    use stdo::engine::runner::MAX_STEP_OUTPUT_BYTES;

    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-budget
steps:
  - id: emit
    cmd: [echo, small]
    cache: true
"#,
    );
    assert_run(workspace.run(&["-t"]), b"", "cached=0");
    let entry = workspace.entries().pop().unwrap();
    let mut data = fs::read(&entry).unwrap();
    let size = MAX_STEP_OUTPUT_BYTES + 1;
    data[9..17].copy_from_slice(&(size as u64).to_be_bytes());
    fs::write(&entry, &data[..25]).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(entry)
        .unwrap()
        .set_len((25 + size) as u64)
        .unwrap();

    let output = workspace.run(&["-t"]);
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        report.contains(&format!("memory limit of {MAX_STEP_OUTPUT_BYTES} bytes")),
        "{report}"
    );
    assert!(report.contains("No processes spawned."), "{report}");
}

#[test]
fn concurrent_writers_leave_a_complete_entry() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-concurrent
steps:
  - id: emit
    cmd: [sh, -c, 'sleep 1; printf stdout; printf stderr >&2']
    cache: true
output: '{{ steps.emit.stdout }} {{ steps.emit.stderr }}'
"#,
    );
    std::thread::scope(|scope| {
        let first = scope.spawn(|| workspace.run(&[]));
        let second = scope.spawn(|| workspace.run(&[]));
        for result in [first.join().unwrap(), second.join().unwrap()] {
            assert_run(result, b"stdout stderr", "");
        }
    });
    assert_eq!(workspace.entries().len(), 1);
    assert_run(
        workspace.run(&["-t"]),
        b"stdout stderr",
        "cache hit (exit 0)",
    );
}

#[test]
fn cached_jsonl_replays_normalized_bytes_and_exposes_records() {
    let workspace = Workspace::new(
        r#"
version: '1.0'
name: cache-jsonl
steps:
  - id: emit
    cmd: [sh, -c, 'printf " { \"value\" : 1 }\r\n\n{\"value\":2}"']
    parse_stdout: jsonl
    cache: true
  - id: copy
    cmd: [cat]
    pipe_from: emit
output: '{{ steps.emit.json | length }}:{{ steps.emit.json[0].value }}:{{ steps.copy.stdout }}'
"#,
    );
    let expected = b"2:1:{\"value\":1}\n{\"value\":2}\n";
    assert_run(workspace.run(&["-t"]), expected, "cached=0");
    assert_run(workspace.run(&["-t"]), expected, "cache hit (exit 0)");
}
