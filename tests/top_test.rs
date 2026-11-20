use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[test]
fn repeated_passes_route_output_and_quit_during_interval() {
    let dir = std::env::temp_dir().join(format!("stdo-top-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.yaml"), "version: '1.0'\nname: monitor\ninputs:\n  message: {type: string, default: no}\nsteps:\n  - id: echo\n    cmd: [cat]\n    stdin: '{{ inputs.message }}'\noutput: '{{ steps.echo.stdout }}'\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
        .current_dir(&dir)
        .args([
            "top",
            "-f",
            "workflow.yaml",
            "-i",
            "message=hello",
            "--watch",
            "--interval",
            "20",
            "--no-cache",
            "--force",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut log = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log.push_str(&line);
            log.push('\n');
            if line.contains("DAG PASS | Pass 2 ") {
                let _ = tx.send(log.clone());
                break;
            }
        }
    });
    let log = rx.recv_timeout(Duration::from_secs(10));
    if log.is_err() {
        let _ = child.kill();
    } else {
        child.stdin.as_mut().unwrap().write_all(b"q").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(dir).unwrap();
    let log = log.expect("two completed passes");
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"hellohello"));
    assert!(log.contains("● PASS"));
    assert!(log.contains("32 MiB"));
}

#[test]
fn interrupt_stops_an_active_process_group() {
    let dir = std::env::temp_dir().join(format!("stdo-top-interrupt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.yaml"),
        r#"version: '1.0'
name: interrupt
steps:
  - id: worker
    cmd: [sh, -c, 'sleep 1 && touch survived & echo ready > ready; wait']
"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
        .current_dir(&dir)
        .args(["top", "workflow.yaml"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !dir.join("ready").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ready = dir.join("ready").exists();
    assert!(Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let survived = dir.join("survived").exists();
    fs::remove_dir_all(dir).unwrap();
    assert!(ready && status.success());
    assert!(!survived, "background process outlived the supervisor");
}

#[test]
fn completed_pass_holds_until_reload_and_reads_updated_workflow() {
    let path = std::env::temp_dir().join(format!("stdo-top-reload-{}.yaml", std::process::id()));
    let workflow = |word: &str| {
        format!("version: '1.0'\nname: hold\nsteps:\n  - id: say\n    cmd: [echo, {word}]\noutput: '{{{{ steps.say.stdout }}}}'\n")
    };
    fs::write(&path, workflow("first")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
        .arg("top")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let wait = Duration::from_secs(5);
    assert_eq!(rx.recv_timeout(wait).unwrap(), "first");
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(1300)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    fs::write(&path, workflow("second")).unwrap();
    child.stdin.as_mut().unwrap().write_all(b"r").unwrap();
    assert_eq!(rx.recv_timeout(wait).unwrap(), "second");
    child.stdin.as_mut().unwrap().write_all(b"q").unwrap();
    assert!(child.wait().unwrap().success());
    fs::remove_file(path).unwrap();
}
