use std::{fs, path::Path, process::Command};

const AUDIT: &str = "examples/01-parallel-repo-audit.yaml";
const ETL: &str = "examples/02-monitored-pipe-etl.yaml";
const GATE: &str = "examples/03-release-gate-switch.yaml";
const ARCHITECTURE: &str = "examples/04-stdo-zero-network-guard.yaml";
const READINESS: &str = "examples/05-stdo-release-readiness.yaml";
const GALLERY: [&str; 5] = [AUDIT, ETL, GATE, ARCHITECTURE, READINESS];

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
        invoke("run", ETL, &[]),
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
    assert!(card.contains("Documented workflows: 5/5\n"));
    assert!(card.contains("Status: READY\n"));
    let (_, fenced) = card.split_once("```json\n").unwrap();
    let (json, _) = fenced.split_once("\n```").unwrap();
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["manifest"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        json["manifest"]["exclude"],
        serde_json::json!(["AGENTS.md", ".github/", ".claude/", "release-plz.toml"])
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
