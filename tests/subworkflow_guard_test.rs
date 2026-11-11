use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{json, Value};

struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = env::temp_dir().join(format!("stdo-guard-{name}-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, file: &str, steps: Value, output: Option<&str>) {
        let workflow = json!({
            "version": "1.0", "name": file, "steps": steps, "output": output
        });
        fs::write(self.0.join(file), workflow.to_string()).unwrap();
    }

    fn command(&self) -> Command {
        let executable = Path::new(env!("CARGO_BIN_EXE_stdo"));
        let path = env::join_paths(
            std::iter::once(executable.parent().unwrap().to_owned())
                .chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut command = Command::new(executable);
        command
            .current_dir(&self.0)
            .env("PATH", path)
            .env_remove("STDO_CALL_STACK");
        command
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn failure(output: Output) -> String {
    assert!(!output.status.success());
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn static_cycle_fails_before_any_steps_for_run_report_and_bench() {
    let workspace = Workspace::new("static-cycle");
    workspace.write(
        "a.yaml",
        json!([
            {"id":"canary", "cmd":["sh","-c","set -C; : > spawned"]},
            {"id":"child", "cmd":["stdo","run","-f","b.yaml"], "stdin":"{{ steps.canary.stdout }}"}
        ]),
        None,
    );
    workspace.write(
        "b.yaml",
        json!([
            {"id":"reenter", "cmd":["stdo","run","--file=./a.yaml"]}
        ]),
        None,
    );

    for args in [
        &["run", "-f", "a.yaml"][..],
        &["run", "-f", "a.yaml", "--report", "report.json"][..],
        &["bench", "a.yaml", "--warmup", "0", "--runs", "1"][..],
    ] {
        let stderr = failure(workspace.command().args(args).output().unwrap());
        assert!(
            stderr.contains("sub-workflow cycle detected: a.yaml -> b.yaml -> a.yaml"),
            "{stderr}"
        );
        assert!(!workspace.0.join("spawned").exists());
        if args[0] == "run" {
            assert!(stderr.contains("No processes spawned."), "{stderr}");
        }
    }
}

#[test]
fn shell_wrapped_cycle_stops_before_reentering_the_first_workflow() {
    let workspace = Workspace::new("shell-cycle");
    for (file, child) in [("a.yaml", "b.yaml"), ("b.yaml", "./a.yaml")] {
        workspace.write(file, json!([{
            "id":"call",
            "cmd":["sh","-c",
                "set -eC; : > \"$2.started\"; printf '%s\\n' \"$2\" >> calls; exec stdo run -f \"$1\"",
                "stdo",child,file]
        }]), None);
    }
    let stderr = failure(
        workspace
            .command()
            .args(["run", "-f", "a.yaml"])
            .output()
            .unwrap(),
    );
    assert!(stderr.contains("sub-workflow cycle detected:"), "{stderr}");
    assert!(stderr.contains("No processes spawned."), "{stderr}");
    assert_eq!(
        fs::read_to_string(workspace.0.join("calls")).unwrap(),
        "a.yaml\nb.yaml\n"
    );
}

#[test]
fn parallel_diamond_can_reuse_a_leaf_with_a_dynamic_file_argument() {
    let workspace = Workspace::new("diamond");
    workspace.write(
        "a.yaml",
        json!([
            {"id":"left","cmd":["stdo","run","-f","b.yaml"]},
            {"id":"right","cmd":["stdo","run","-f","c.yaml"]}
        ]),
        Some("{{ steps.left.stdout }}{{ steps.right.stdout }}"),
    );
    workspace.write(
        "b.yaml",
        json!([
            {"id":"leaf","cmd":["stdo","run","-f","d.yaml"]}
        ]),
        Some("{{ steps.leaf.stdout }}"),
    );
    workspace.write(
        "c.yaml",
        json!([
            {"id":"choose","cmd":["echo","d.yaml"]},
            {"id":"leaf","cmd":["sh","-c","IFS= read -r child; exec stdo run -f \"$child\""],
                "stdin":"{{ steps.choose.stdout }}"}
        ]),
        Some("{{ steps.leaf.stdout }}"),
    );
    workspace.write(
        "d.yaml",
        json!([
            {"id":"say","cmd":["echo","leaf"]}
        ]),
        Some("{{ steps.say.stdout }}"),
    );

    let output = workspace
        .command()
        .args(["run", "-f", "a.yaml"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"leaf\nleaf\n");
}

#[test]
fn inherited_depth_limit_rejects_before_spawn() {
    let workspace = Workspace::new("depth");
    workspace.write(
        "a.yaml",
        json!([
            {"id":"canary","cmd":["touch","spawned"]}
        ]),
        None,
    );
    let stack = vec!["1".repeat(64); 16].join(":");
    let stderr = failure(
        workspace
            .command()
            .env("STDO_CALL_STACK", stack)
            .args(["run", "-f", "a.yaml"])
            .output()
            .unwrap(),
    );
    assert!(
        stderr.contains("maximum sub-workflow nesting depth (16) exceeded"),
        "{stderr}"
    );
    assert!(stderr.contains("No processes spawned."), "{stderr}");
    assert!(!workspace.0.join("spawned").exists());
}

#[test]
fn invalid_child_yaml_dag_and_workspace_escape_fail_before_spawn() {
    let workspace = Workspace::new("invalid-child");
    workspace.write(
        "a.yaml",
        json!([
            {"id":"canary","cmd":["touch","spawned"]},
            {"id":"child","cmd":["stdo","run","-f","b.yaml"]}
        ]),
        None,
    );
    let reject = |expected: &str| {
        let stderr = failure(
            workspace
                .command()
                .args(["run", "-f", "a.yaml"])
                .output()
                .unwrap(),
        );
        assert!(stderr.contains(expected), "{stderr}");
        assert!(stderr.contains("No processes spawned."), "{stderr}");
        assert!(!workspace.0.join("spawned").exists());
    };

    let child = workspace.0.join("b.yaml");
    fs::write(&child, "steps: [").unwrap();
    reject("invalid workflow YAML");
    workspace.write(
        "b.yaml",
        json!([
            {"id":"self","cmd":["cat"],"pipe_from":"self"}
        ]),
        None,
    );
    reject("cycle");
    fs::remove_file(&child).unwrap();
    let outside = Workspace::new("outside");
    outside.write(
        "external.yaml",
        json!([{"id":"say","cmd":["echo","outside"]}]),
        None,
    );
    std::os::unix::fs::symlink(outside.0.join("external.yaml"), child).unwrap();
    reject("inside the workspace");
}
