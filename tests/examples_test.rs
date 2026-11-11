use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const AUDIT: &str = "examples/01-parallel-repo-audit.yaml";
const ETL: &str = "examples/02-monitored-pipe-etl.yaml";
const GATE: &str = "examples/03-release-gate-switch.yaml";
const ARCHITECTURE: &str = "examples/04-stdo-zero-network-guard.yaml";
const READINESS: &str = "examples/05-stdo-release-readiness.yaml";
const SENSOR: &str = "examples/06-jsonl-sensor-stream.yaml";
const SPEECH: &str = "examples/07-ollama-gemma4-phonon-speech.yaml";
const TRIAGE: &str = "examples/08-ollama-local-pr-triage.yaml";
const HTTP: &str = "examples/09-generic-http-api-pipeline.yaml";
const CAPSULE: &str = "examples/10-youtube-capsule-generator.yaml";
const COMPOSABLE: &str = "examples/11-composable-subworkflows.yaml";
const GALLERY: [&str; 11] = [
    AUDIT,
    ETL,
    GATE,
    ARCHITECTURE,
    READINESS,
    SENSOR,
    SPEECH,
    TRIAGE,
    HTTP,
    CAPSULE,
    COMPOSABLE,
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn invoke(subcommand: &str, file: &str, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stdo"))
        .current_dir(root())
        .args([subcommand, "-f", file])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdo {subcommand} -f {file} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn parallel_audit_counts_files_lines_and_matches() {
    assert_eq!(
        invoke("doctor", AUDIT, &[]),
        "parallel-repo-audit: 4 steps in 2 layers, required executables available\n"
    );

    let yaml = GALLERY.map(|file| fs::read_to_string(root().join(file)).unwrap());
    let files = yaml.len();
    let lines: usize = yaml.iter().map(|text| text.lines().count()).sum();
    let matches = yaml
        .iter()
        .flat_map(|text| text.lines())
        .filter(|line| line.contains("name:"))
        .count();
    assert_eq!(
        invoke("run", AUDIT, &[]),
        format!("YAML files: {files}\nYAML lines: {lines}\nMatching lines: {matches}\n")
    );
    assert_eq!(
        invoke(
            "run",
            AUDIT,
            &[
                "-i",
                "target_dir=./examples",
                "-i",
                "keyword=__stdo_no_match__"
            ]
        ),
        format!("YAML files: {files}\nYAML lines: {lines}\nMatching lines: 0\n")
    );
}

#[test]
fn monitored_etl_renders_structured_json() {
    assert_eq!(
        invoke("doctor", ETL, &[]),
        "monitored-pipe-etl: 5 steps in 5 layers, required executables available\n"
    );
    assert_eq!(
        invoke("run", ETL, &["--no-cache"]),
        "Kept 3 records\nFirst: apple\nLast: pear\n"
    );
}

#[test]
fn release_gate_runs_both_severity_branches() {
    assert_eq!(
        invoke("doctor", GATE, &[]),
        "release-gate-switch: 4 steps in 3 layers, required executables available\n"
    );
    assert_eq!(
        invoke("run", GATE, &["-i", "severity=LOW"]),
        "Severity: LOW\nPASS: release approved\n"
    );
    assert_eq!(
        invoke("run", GATE, &["-i", "severity=CRITICAL"]),
        "Severity: CRITICAL\nBLOCK: incident recorded\n"
    );
}

fn rust_source_totals(dir: &Path) -> (usize, usize) {
    let mut modules = 0;
    let mut lines = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            let (nested_modules, nested_lines) = rust_source_totals(&path);
            modules += nested_modules;
            lines += nested_lines;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            modules += 1;
            lines += fs::read_to_string(path).unwrap().lines().count();
        }
    }
    (modules, lines)
}

#[test]
fn architecture_audit_checks_the_checkout_and_counts_rust_source() {
    assert_eq!(
        invoke("doctor", ARCHITECTURE, &[]),
        "stdo-zero-network-guard: 7 steps in 3 layers, required executables available\n"
    );
    let (modules, lines) = rust_source_totals(&root().join("src"));
    assert_eq!(
        invoke("run", ARCHITECTURE, &[]),
        format!(
            "# Architecture & Safety Self-Audit\nUnsafe-code guard: PASS\nNetwork dependency guard: PASS\nRust source: {modules} modules, {lines} lines\n"
        )
    );
}

#[test]
fn release_readiness_renders_markdown_and_valid_json() {
    assert_eq!(
        invoke("doctor", READINESS, &[]),
        "stdo-release-readiness: 2 steps in 1 layers, required executables available\n"
    );
    let card = invoke("run", READINESS, &[]);
    assert!(card.contains("# stdo release readiness\n"));
    assert!(card.contains(&format!("Crate version: `{}`", env!("CARGO_PKG_VERSION"))));
    assert!(card.contains(&format!("Documented workflows: {0}/{0}\n", GALLERY.len())));
    assert!(card.contains("Status: READY\n"));
    let (_, fenced) = card.split_once("```json\n").unwrap();
    let (json, _) = fenced.split_once("\n```").unwrap();
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["manifest"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        json["manifest"]["exclude"],
        serde_json::json!([
            "AGENTS.md",
            ".github/",
            ".claude/",
            "release-plz.toml",
            "assets/",
            "benchmarks/"
        ])
    );
    assert_eq!(
        json["documentation"],
        serde_json::json!({
            "workflows": GALLERY.len(),
            "documented": GALLERY.len(),
            "gallery_linked": true,
            "integration_tests_present": true,
            "ready": true
        })
    );
}

struct StubWorkspace(PathBuf);

impl Drop for StubWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl StubWorkspace {
    fn new(label: &str) -> Self {
        let path = env::temp_dir().join(format!("stdo-example-{label}-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let workspace = Self(path);
        fs::create_dir(workspace.0.join("bin")).unwrap();
        workspace
    }

    fn stub(&self, name: &str, script: &str) {
        let path = self.0.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{script}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn invoke(&self, subcommand: &str, file: &str, args: &[&str]) -> Output {
        let path = env::join_paths(
            std::iter::once(self.0.join("bin"))
                .chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_stdo"))
            .current_dir(&self.0)
            .env("PATH", path)
            .args([subcommand, "-f"])
            .arg(root().join(file))
            .args(args)
            .output()
            .unwrap()
    }

    fn success(&self, subcommand: &str, file: &str, args: &[&str]) -> String {
        let result = self.invoke(subcommand, file, args);
        assert!(
            result.status.success(),
            "{file}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
}

#[test]
fn external_workflows_run_offline_with_stub_ollama_and_curl() {
    let workspace = StubWorkspace::new("stubs");
    fs::create_dir(workspace.0.join("src")).unwrap();
    workspace.stub(
        "ollama",
        r#"
[ "$1" = run ]
case "$2" in
  phonon)
    [ "$#" -eq 2 ]
    [ "$(cat)" = 'Owls can turn their heads far.' ]
    printf 'RIFF\000\377WAVE'
    ;;
  gemma4)
    [ "$3" = --hidethinking ]
    if [ "$#" -eq 4 ]; then
      [ "$4" = 'Tell me a fun fact about owls in two sentences.' ]
      printf 'Owls can turn their heads far.\n'
    else
      [ "$#" -eq 3 ]
      prompt=$(cat)
      case "$prompt" in *fixture.rs*) ;; *) exit 7 ;; esac
      printf x >> triage-calls
      case "$prompt" in
        *'TODO fixture marker'*) printf '{"risk":"HIGH","summary":"Inspect the fixture"}\n' ;;
        *) printf '{"risk":"LOW","summary":"No scan findings"}\n' ;;
      esac
    fi
    ;;
  *) exit 8 ;;
esac
"#,
    );
    workspace.stub(
        "curl",
        r#"
[ "$#" -eq 5 ] && [ "$1" = -fsSL ] && [ "$2" = -H ] &&
  [ "$3" = 'Accept: application/json' ] && [ "$4" = -- ] || exit 8
case "$5" in
  'https://api.github.com/repos/adiktiv-technologies/stdo/releases?per_page=3')
    printf '[{"id":1,"tag_name":"v0.5.0"}]\n'
    ;;
  https://fixture.invalid/malformed) printf 'not JSON\n' ;;
  https://fixture.invalid/items)
    printf x >> curl-calls
    [ "$(cat curl-calls)" != x ] || exit 22
    printf '{"items":[{"id":1,"name":"one"},{"id":3,"name":"three"}]}\n'
    ;;
  *) exit 9 ;;
esac
"#,
    );

    for file in [SPEECH, TRIAGE, HTTP] {
        assert!(workspace
            .success("doctor", file, &[])
            .contains("required executables available"));
    }

    assert_eq!(
        workspace.success("run", SPEECH, &["-i", "output_wav=owl fact.wav"]),
        "Wrote 10 bytes to owl fact.wav\n"
    );
    assert_eq!(
        fs::read(workspace.0.join("owl fact.wav")).unwrap(),
        b"RIFF\0\xffWAVE"
    );

    let source = workspace.0.join("src/fixture.rs");
    fs::write(&source, "// TODO fixture marker\n").unwrap();
    for _ in 0..2 {
        assert_eq!(
            workspace.success("run", TRIAGE, &[]),
            "HIGH: Inspect the fixture\nReview requested\n"
        );
    }
    assert_eq!(fs::read(workspace.0.join("triage-calls")).unwrap(), b"x");
    fs::write(source, "// completed fixture\n").unwrap();
    assert_eq!(
        workspace.success("run", TRIAGE, &[]),
        "LOW: No scan findings\nLow risk\n"
    );

    workspace.success("run", HTTP, &["-i", "output_jsonl=releases.jsonl"]);
    assert_eq!(
        fs::read_to_string(workspace.0.join("releases.jsonl")).unwrap(),
        "{\"id\":1,\"tag_name\":\"v0.5.0\"}\n"
    );

    let result = workspace.success(
        "run",
        HTTP,
        &[
            "-i",
            "api_url=https://fixture.invalid/items",
            "-i",
            "min_id=2",
            "-i",
            "output_jsonl=records.jsonl",
        ],
    );
    assert_eq!(result, "Wrote 1 JSONL record(s) to records.jsonl\n");
    assert_eq!(fs::read(workspace.0.join("curl-calls")).unwrap(), b"xx");
    let records = workspace.0.join("records.jsonl");
    let expected = "{\"id\":3,\"name\":\"three\"}\n";
    assert_eq!(fs::read_to_string(&records).unwrap(), expected);
    assert_eq!(
        workspace.success(
            "run",
            HTTP,
            &[
                "-i",
                "api_url=https://fixture.invalid/items",
                "-i",
                "min_id=10",
                "-i",
                "output_jsonl=records.jsonl",
            ]
        ),
        "No matching records\n"
    );
    let malformed = workspace.invoke(
        "run",
        HTTP,
        &[
            "-i",
            "api_url=https://fixture.invalid/malformed",
            "-i",
            "output_jsonl=records.jsonl",
        ],
    );
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("step \"normalize_jsonl\" failed"));
    assert_eq!(fs::read_to_string(&records).unwrap(), expected);
}

#[test]
fn failed_sink_writes_preserve_existing_audio_and_jsonl() {
    let workspace = StubWorkspace::new("sink-failure");
    workspace.stub(
        "ollama",
        r#"
case "$2" in
  gemma4) printf '%s\n' "$4" ;;
  phonon) IFS= read -r answer; printf 'RIFF\000\377WAVE'; [ "$answer" != fail ] ;;
  *) exit 1 ;;
esac
"#,
    );
    workspace.stub("curl", "printf '[{\"id\":1}]\\n'\n");
    let wav = workspace.0.join("upstream-failure.wav");
    fs::write(&wav, b"original").unwrap();
    let upstream_failure = workspace.invoke(
        "run",
        SPEECH,
        &["-i", "output_wav=upstream-failure.wav", "-i", "prompt=fail"],
    );
    assert!(!upstream_failure.status.success());
    assert!(
        String::from_utf8_lossy(&upstream_failure.stderr).contains("step \"stream_phonon\" failed")
    );
    assert_eq!(fs::read(wav).unwrap(), b"original");

    workspace.stub("cat", "printf 'partial output'\nexit 1\n");
    for (file, key, target, failing_step) in [
        (SPEECH, "output_wav", "owl fact.wav", "stage_wav"),
        (HTTP, "output_jsonl", "records.jsonl", "atomic_commit"),
    ] {
        let path = workspace.0.join(target);
        fs::write(&path, b"original").unwrap();
        let result = workspace.invoke("run", file, &["-i", &format!("{key}={target}")]);
        assert!(!result.status.success(), "{file}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(&format!("step \"{failing_step}\" failed")),
            "{stderr}"
        );
        assert_eq!(fs::read(path).unwrap(), b"original", "{file}");
        let temporary_prefix = format!("{target}.tmp.");
        assert!(fs::read_dir(&workspace.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&temporary_prefix)
        }));
    }
}

#[test]
fn concurrent_atomic_sinks_publish_one_complete_payload() {
    let workspace = StubWorkspace::new("concurrent-sink");
    workspace.stub(
        "curl",
        r#"
case "$5" in
  https://fixture.invalid/left) printf '[{"id":1},{"id":2}]\n' ;;
  https://fixture.invalid/right) printf '[{"id":3},{"id":4},{"id":5}]\n' ;;
  *) exit 1 ;;
esac
"#,
    );
    workspace.stub(
        "cat",
        r#"
# Each sink has already reserved its sibling file when it invokes cat.
# Rendezvous before either writer can finish.
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  set -- ./shared.jsonl.tmp.*
  if [ "$#" -eq 2 ]; then
    : > both-staged
  fi
  if [ -f both-staged ]; then
    exec /bin/cat
  fi
  sleep 0.2
done
exit 1
"#,
    );
    let run = |url| workspace.success("run", HTTP, &["-i", url, "-i", "output_jsonl=shared.jsonl"]);
    std::thread::scope(|scope| {
        let left = scope.spawn(|| run("api_url=https://fixture.invalid/left"));
        let right = scope.spawn(|| run("api_url=https://fixture.invalid/right"));
        assert!(left.join().unwrap().contains("2 JSONL record(s)"));
        assert!(right.join().unwrap().contains("3 JSONL record(s)"));
    });
    let output = fs::read_to_string(workspace.0.join("shared.jsonl")).unwrap();
    assert!(
        [
            "{\"id\":1}\n{\"id\":2}\n",
            "{\"id\":3}\n{\"id\":4}\n{\"id\":5}\n"
        ]
        .contains(&output.as_str()),
        "{output}"
    );
    assert!(fs::read_dir(&workspace.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("shared.jsonl.tmp.")
    }));
}

#[test]
fn capsule_renders_waveform_and_recording_offline() {
    let workspace = StubWorkspace::new("capsule");
    workspace.stub(
        "ollama",
        r#"
[ "$1" = run ]
case "$2" in
  gemma4) [ "$3" = --hidethinking ]; printf 'Unix pipes connect tools.\n' ;;
  phonon:latest) [ "$(cat)" = 'Unix pipes connect tools.' ]; printf 'RIFF\000\000\000\000WAVE' ;;
  *) exit 1 ;;
esac
"#,
    );
    workspace.stub(
        "ffmpeg",
        r#"
[ "$(wc -c < './voice track.wav' | tr -d ' ')" = 12 ]
case " $* " in
  *' -stream_loop -1 -i ./screen recording.mov '*)
    case " $* " in
      *' -af aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo '*) ;;
      *) exit 1 ;;
    esac
    case " $* " in
      *' -c:a aac -b:a 192k '*) ;;
      *) exit 1 ;;
    esac
    printf 'recording\n' >> render-modes ;;
  *' color=c=0x0d1117:s=640x360:r=24 '*)
    case " $* " in
      *' -filter_complex [1:a]asplit=2[a_vis][a_raw];[a_vis]aformat=channel_layouts=mono,compand,showwaves=s=640x360:mode=cline:rate=24:scale=sqrt:colors=0x38bdf8[fg];[0:v][fg]overlay=format=auto,format=yuv420p[v];[a_raw]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[a] -map [v] -map [a] '*)
        case " $* " in
          *' -tune animation -crf 28 -preset veryslow -maxrate 120k -bufsize 240k '*) ;;
          *) exit 1 ;;
        esac
        case " $* " in
          *' -c:a aac -b:a 64k '*) ;;
          *) exit 1 ;;
        esac
        printf 'waveform\n' >> render-modes ;;
      *) exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac
for output do :; done
case "$output" in './finished video.mp4.tmp.'*.mp4) ;; *) exit 1 ;; esac
printf '\000\000\000\014ftypisom' > "$output"
"#,
    );
    assert!(workspace
        .success("doctor", CAPSULE, &[])
        .contains("required executables available"));
    let args = [
        "-i",
        "output_wav=voice track.wav",
        "-i",
        "output_mp4=finished video.mp4",
    ];
    let expected = "Unix pipes connect tools.\nWAV: voice track.wav (12 bytes)\nMP4: finished video.mp4 (12 bytes)\n";
    assert_eq!(workspace.success("run", CAPSULE, &args), expected);
    fs::write(workspace.0.join("screen recording.mov"), b"fixture").unwrap();
    let recording = [
        args.as_slice(),
        &["-i", "background_video=screen recording.mov"],
    ]
    .concat();
    assert_eq!(workspace.success("run", CAPSULE, &recording), expected);
    assert_eq!(
        fs::read(workspace.0.join("voice track.wav")).unwrap(),
        b"RIFF\0\0\0\0WAVE"
    );
    assert_eq!(
        fs::read(workspace.0.join("finished video.mp4")).unwrap(),
        b"\0\0\0\x0cftypisom"
    );
    assert_eq!(
        fs::read_to_string(workspace.0.join("render-modes")).unwrap(),
        "waveform\nrecording\n"
    );
}

#[test]
fn jsonl_sensor_stream_filters_and_routes_records() {
    assert_eq!(
        invoke("doctor", SENSOR, &[]),
        "jsonl-sensor-stream: 5 steps in 4 layers, required executables available\n"
    );
    assert_eq!(
        invoke("run", SENSOR, &[]),
        "ALERT: 2 hot readings\nFirst: west at 32 C\n"
    );
}

#[test]
fn repo_examples_render_the_same_output_from_the_gallery_directory() {
    for file in [AUDIT, ARCHITECTURE, READINESS] {
        let local_path = format!("./{}", file.strip_prefix("examples/").unwrap());
        let output = Command::new(env!("CARGO_BIN_EXE_stdo"))
            .current_dir(root().join("examples"))
            .args(["run", &local_path])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{local_path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, invoke("run", file, &[]).as_bytes());
    }
}

#[test]
fn composable_gallery_checks_and_runs_both_children() {
    let executable = Path::new(env!("CARGO_BIN_EXE_stdo"));
    let path = env::join_paths(
        std::iter::once(executable.parent().unwrap().to_owned())
            .chain(env::split_paths(&env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for action in ["doctor", "run"] {
        let output = Command::new(executable)
            .current_dir(root())
            .env("PATH", &path)
            .args([action, "-f", COMPOSABLE])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        if action == "doctor" {
            assert!(text.contains("3 steps in 2 layers, required executables available"));
        } else {
            assert!(text.contains("## Repository audit\nYAML files: 11"));
            assert!(text.contains("## Sensor stream\nALERT: 2 hot readings\nFirst: west at 32 C"));
        }
    }
}

#[test]
fn doctor_recurses_with_child_overrides_without_spawning_commands() {
    let workspace = StubWorkspace::new("nested-doctor");
    workspace.stub("stdo", "exit 98");
    workspace.stub("ollama", "exit 99");
    let parent = workspace.0.join("a.yaml");
    fs::write(&parent,"version: '1.0'\nname: a\nsteps:\n  - id: invoke\n    cmd: [stdo, run, --file, b.yaml, -i, llm_model=custom-model]\n").unwrap();
    fs::write(workspace.0.join("b.yaml"),"version: '1.0'\nname: b\ninputs:\n  llm_model: {type: string, default: gemma4}\nsteps:\n  - id: prompt\n    cmd: [ollama, run, '{{ inputs.llm_model }}', hello]\n  - id: nested\n    cmd: [stdo, run, --file=c.yaml]\n").unwrap();
    let missing = workspace.invoke("doctor", parent.to_str().unwrap(), &[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("sub-workflow \"c.yaml\""));
    let child = workspace.0.join("c.yaml");
    fs::write(&child,"version: '1.0'\nname: c\nsteps:\n  - id: leaf\n    cmd: [stdo-example-missing-executable]\n").unwrap();
    let missing = workspace.invoke("doctor", parent.to_str().unwrap(), &[]);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("stdo-example-missing-executable") && error.contains("c.yaml"));
    fs::write(
        &child,
        "version: '1.0'\nname: c\nsteps:\n  - id: leaf\n    cmd: [echo, ok]\n",
    )
    .unwrap();
    let summary = workspace.success("doctor", parent.to_str().unwrap(), &[]);
    assert!(summary.contains("Local Ollama models (availability not probed): custom-model"));
    assert!(!summary.contains("gemma4"));
}

#[test]
fn doctor_rejects_canonical_cycles_and_workspace_escapes() {
    let workspace = StubWorkspace::new("nested-cycle");
    let parent = workspace.0.join("a.yaml");
    fs::write(
        &parent,
        "version: '1.0'\nname: a\nsteps:\n  - id: child\n    cmd: [stdo, run, -f, b.yaml]\n",
    )
    .unwrap();
    let child = workspace.0.join("b.yaml");
    fs::write(
        &child,
        "version: '1.0'\nname: b\nsteps:\n  - id: child\n    cmd: [stdo, run, --file, ./a.yaml]\n",
    )
    .unwrap();
    let result = workspace.invoke("doctor", parent.to_str().unwrap(), &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("a.yaml -> b.yaml -> a.yaml"));
    fs::remove_file(&child).unwrap();
    std::os::unix::fs::symlink(root().join(AUDIT), child).unwrap();
    let result = workspace.invoke("doctor", parent.to_str().unwrap(), &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("must be a YAML file inside the workspace"));
}
