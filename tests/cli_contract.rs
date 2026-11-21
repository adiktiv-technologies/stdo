use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::{tempdir, TempDir};

struct Fixture(TempDir);
impl Fixture {
    fn new() -> Self {
        let f = Self(tempdir().unwrap());
        f.script("sample",r#"case "$*" in
--help) printf 'Usage: sample [COMMAND]\nCommands:\n  alpha  List records\n  beta  Save records\nOptions:\n  -v, --verbose  Details\n  --other  Extra\nExamples:\n  sample alpha\n' ;;
'alpha --help'|'beta --help') printf 'Usage: sample %s\nOptions:\n  --all  All records\n' "$1" ;;
*) exit 1;;
esac"#);
        f
    }
    fn dir(&self) -> &Path {
        self.0.path()
    }
    fn script(&self, name: &str, body: &str) {
        let path = self.dir().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stdo"));
        c.arg("skill");
        c.env_clear()
            .env("PATH", self.dir())
            .env("HOME", self.dir())
            .env("XDG_DATA_DIRS", self.dir())
            .current_dir(self.dir())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn run(&self, args: &[&str], input: &[u8], code: i32) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
}
fn text(o: Output) -> String {
    String::from_utf8(o.stdout).unwrap()
}
#[test]
fn circuit_breaker() {
    let f = Fixture::new();
    for (status, help) in [(0, "Commands without signature"), (1, "Usage: danger")] {
        f.script(
            "danger",
            &format!("echo \"$*\" >> calls; echo '{help}'; exit {status}"),
        );
        f.run(&["-l", "--", "danger"], b"", 65);
        assert!(fs::read_to_string(f.dir().join("calls"))
            .unwrap()
            .lines()
            .all(|s| matches!(s, "--help" | "-h")));
    }
}
#[test]
fn timeout() {
    let f = Fixture::new();
    f.script("waiter", "/bin/sleep 5");
    let start = Instant::now();
    f.run(&["--timeout", "200", "--", "waiter"], b"", 75);
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn absent() {
    Fixture::new().run(&["--", "non_existent_bin_xyz"], b"", 69);
}
#[test]
fn filters_and_eof() {
    let f = Fixture::new();
    assert_eq!(
        text(f.run(&["-l", "--", "sample"], b"", 0)),
        "alpha\nbeta\n"
    );
    let doc = text(f.run(&["-o", "-", "--", "sample"], b"beta\n", 0));
    assert!(doc.contains("- `beta`"));
    assert!(!doc.contains("- `alpha`"));
    f.run(&["--", "sample"], b"bogus\n", 65);
    f.run(&["--", "sample"], b"\n", 65);
    assert_eq!(text(f.run(&["-l", "-d", "0", "--", "sample"], b"", 0)), "");
}
#[test]
fn protection_and_update() {
    let f = Fixture::new();
    let path = f.dir().join("doc");
    fs::write(&path, "text\nx-generated-by: skill\n").unwrap();
    let args = ["-o", "doc", "--", "sample"];
    f.run(&args, b"", 73);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "text\nx-generated-by: skill\n"
    );
    assert!(f
        .run(&["-f", "-o", "doc", "--", "sample"], b"", 0)
        .stdout
        .is_empty());
    let doc = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{doc}extra\n")).unwrap();
    f.run(&args, b"", 0);
    assert_eq!(doc, fs::read_to_string(path).unwrap());
}
#[test]
fn dry_run() {
    let f = Fixture::new();
    let args = ["-n", "-o", "doc", "--", "sample"];
    let preview = text(f.run(&args, b"", 3));
    assert!(preview.starts_with("---\n"));
    let path = f.dir().join("doc");
    assert!(!path.exists());
    fs::write(&path, "old\n").unwrap();
    let diff = text(f.run(&args, b"", 3));
    assert!(diff.starts_with("--- "));
    assert!(diff.contains("@@"));
    assert!(!diff.contains('\x1b'));
    fs::write(&path, preview).unwrap();
    let out = f.run(&args, b"", 0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}
#[test]
fn line_budget() {
    let f = Fixture::new();
    for limit in 9..20 {
        let doc = text(f.run(
            &["-L", &limit.to_string(), "-o", "-", "--", "sample"],
            b"",
            0,
        ));
        assert!(doc.lines().count() <= limit);
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(doc.split("---\n").nth(1).unwrap()).unwrap();
        assert_eq!(yaml["x-generated-by"], "skill");
        for h in ["## Synopsis", "## Subcommands", "## Flags"] {
            assert!(doc.lines().any(|l| l == h));
        }
    }
}
#[test]
fn syntax() {
    let f = Fixture::new();
    for args in [
        vec!["sample"],
        vec!["-R", "-g", "--", "sample"],
        vec!["-R", "some-dir", "--", "sample"],
        vec!["--name", "Bad!", "--", "sample"],
        vec!["--init", "unknown"],
        vec![],
    ] {
        f.run(&args, b"", 2);
    }
}

#[test]
fn passive_completion_precedes_active_help() {
    let f = Fixture::new();
    let dir = f.dir().join(".local/share/bash-completion/completions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("sample"), "complete -W 'alpha beta' sample\n").unwrap();
    f.script("sample", "echo unexpected > invoked; exit 1");
    f.run(&["-d", "0", "-o", "-", "--", "sample"], b"", 0);
    assert!(!f.dir().join("invoked").exists());
    f.run(&["-l", "--", "sample"], b"", 65);
}

#[test]
fn short_help_with_status_two_and_isolated_environment() {
    let f = Fixture::new();
    f.script(
        "probe",
        r#"[ "$CI:$TERM:$NO_COLOR" = 1:dumb:1 ] || exit 1
if read -r input; then exit 1; fi
case "$*" in
  --help) exit 1;;
  -h) printf 'Usage: probe\nOptions:\n  --number  Choose number\n' >&2; exit 2;;
esac"#,
    );
    let doc = text(f.run(&["-o", "-", "--", "probe"], b"", 0));
    assert!(doc.contains("--number"));
}

#[test]
fn shell_wrapper_preserves_arguments_and_plugin_matches_canonical() {
    let f = Fixture::new();
    f.script(
        "sample",
        r#"if [ "$1" = --help ]; then printf 'Usage: sample\n'; else printf '<%s>\n' "$@"; fi"#,
    );
    let bin = env!("CARGO_BIN_EXE_stdo");
    std::os::unix::fs::symlink(bin, f.dir().join("stdo")).unwrap();
    std::os::unix::fs::symlink(bin, f.dir().join("sample-skill")).unwrap();
    for shell in ["/bin/bash", "/bin/zsh"] {
        if !Path::new(shell).is_file() {
            continue;
        }
        let name = Path::new(shell).file_name().unwrap().to_str().unwrap();
        let script=format!("eval \"$(stdo skill --init {name})\" && sample ordinary 'two words' && sample skill -L 9 -o -");
        let out = Command::new(shell)
            .args(["-c", &script])
            .env_clear()
            .env("PATH", f.dir())
            .env("HOME", f.dir())
            .env("XDG_DATA_DIRS", f.dir())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text(out).starts_with("<ordinary>\n<two words>\n---\n"));
    }
    let out = Command::new(f.dir().join("sample-skill"))
        .args(["-o", "-"])
        .env_clear()
        .env("PATH", f.dir())
        .env("HOME", f.dir())
        .env("XDG_DATA_DIRS", f.dir())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(text(out), text(f.run(&["-o", "-", "--", "sample"], b"", 0)));
}

#[test]
fn scoped_document_records_target_version_only_when_advertised() {
    let f = Fixture::new();
    f.script("sample", r#"case "$*" in
--help) printf 'Usage: sample COMMAND\nCommands:\n  alpha  List records\nOptions:\n  -V, --version  Print version\n' ;;
--version) echo 'sample v2.7.3-rc.1' ;;
'alpha --help') echo 'Usage: sample alpha' ;;
*) exit 1 ;;
esac"#);
    let doc = text(f.run(&["-o", "-", "--", "sample", "alpha"], b"", 0));
    assert!(doc.contains("x-cli-version: \"v2.7.3-rc.1\""));
    let unversioned = Fixture::new();
    assert!(!text(unversioned.run(&["-o", "-", "--", "sample"], b"", 0)).contains("x-cli-version:"));
}

#[test]
fn native_stdo_guide_teaches_a_runnable_workflow() {
    let f = Fixture::new();
    let bin = env!("CARGO_BIN_EXE_stdo");
    let doc = text(f.run(&["-o", "-", "--", bin], b"", 0));
    assert!(doc.lines().count() <= 200);
    let yaml = doc
        .split_once("```yaml\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    assert_eq!(
        yaml,
        include_str!("../examples/12-count-words.yaml").trim_end()
    );
    fs::write(f.dir().join("stdo.yaml"), yaml).unwrap();
    for (cmd, args) in [("doctor", vec![]), ("run", vec!["-i", "message=one two"])] {
        let out = Command::new(bin)
            .current_dir(f.dir())
            .arg(cmd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if cmd == "run" {
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "2");
        }
    }
    for command in ["doctor", "run", "top", "serve", "bench", "skill"] {
        assert!(doc.contains(&format!("**{command}**")));
    }
    let scoped = text(f.run(&["-o", "-", "--", bin, "skill"], b"", 0));
    assert!(!scoped.contains("count-words"));
    assert!(scoped.contains("fzf"));
    assert!(!text(f.run(&["-o", "-", "--", "sample"], b"", 0)).contains("count-words"));
}
