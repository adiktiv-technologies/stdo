//! Offline audit output and the shared HTML/JSON report writer.
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{self, Write},
    path::Path,
};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    atomic_file,
    dag::schema::Workflow,
    engine::{self, telemetry::ExecutionTelemetry, ProcessLedger, RunOptions, Telemetry},
    error::Error,
};

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Serialize)]
struct Audit<'a> {
    workflow_name: Option<&'a str>,
    workflow_version: Option<&'a str>,
    workflow_path: &'a str,
    workflow_sha256: String,
    inputs: Option<BTreeMap<String, Value>>,
    inputs_sha256: Option<String>,
    status: &'static str,
    error: Option<String>,
    #[serde(flatten)]
    telemetry: ExecutionTelemetry,
    stdout_bytes: usize,
    stdout_sha256: String,
}

struct AuditOutput<'a, W> {
    output: &'a mut W,
    hash: Sha256,
    bytes: usize,
}

impl<W: Write> Write for AuditOutput<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.bytes += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

/// Runs a workflow and writes an audit even when validation or execution fails.
pub async fn run(
    workflow_path: &Path,
    report_path: &Path,
    overrides: &[String],
    ledger: &ProcessLedger,
    output: &mut impl Write,
    options: RunOptions<'_>,
) -> Result<(), Error> {
    reject_workflow_destination(report_path, workflow_path)?;
    let yaml = Workflow::read_yaml(workflow_path)?;
    let parsed = Workflow::from_yaml(&yaml);
    let workflow_name = parsed.as_ref().ok().map(|workflow| workflow.name.clone());
    let workflow_version = parsed
        .as_ref()
        .ok()
        .map(|workflow| workflow.version.clone());
    let inputs = parsed
        .as_ref()
        .ok()
        .and_then(|workflow| engine::resolve_inputs(workflow, overrides).ok());
    let inputs_sha256 = inputs
        .as_ref()
        .map(|inputs| sha256(&serde_json::to_vec(inputs).expect("resolved inputs serialize")));
    let telemetry = options
        .telemetry
        .map_or_else(Telemetry::for_audit, Telemetry::with_stdout_hashes);
    let mut output = AuditOutput {
        output,
        hash: Sha256::new(),
        bytes: 0,
    };
    let result = match parsed {
        Ok(workflow) => engine::run_with_options(
            &workflow,
            overrides,
            ledger,
            &mut output,
            RunOptions {
                telemetry: Some(&telemetry),
                ..options
            },
        )
        .await
        .and_then(|()| output.flush().map_err(Error::WriteOutput)),
        Err(error) => Err(error),
    };
    telemetry.finish();
    let snapshot = telemetry.snapshot();
    let workflow_path = workflow_path.to_string_lossy();
    let audit = Audit {
        workflow_name: workflow_name.as_deref(),
        workflow_version: workflow_version.as_deref(),
        workflow_path: &workflow_path,
        workflow_sha256: sha256(yaml.as_bytes()),
        inputs,
        inputs_sha256,
        status: if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
        error: result.as_ref().err().map(ToString::to_string),
        telemetry: snapshot,
        stdout_bytes: output.bytes,
        stdout_sha256: format!("{:x}", output.hash.finalize()),
    };
    let written = write(report_path, "Execution audit", &audit, || audit.html()).await;
    match (result, written) {
        (Err(error), Err(write_error)) => {
            eprintln!("stdo: {write_error}");
            Err(error)
        }
        (result, written) => result.and(written),
    }
}

impl Audit<'_> {
    fn html(&self) -> String {
        let mut body = format!(
            "<p><strong>{}</strong> · {} · {:.2} ms · TTFB {:.2} ms · {} peak bytes</p>\
             <dl><dt>Workflow SHA-256</dt><dd><code>{}</code></dd>\
             <dt>Stdout SHA-256</dt><dd><code>{}</code></dd></dl>",
            escape(self.workflow_name.unwrap_or(self.workflow_path)),
            self.status,
            self.telemetry.duration_ms,
            self.telemetry.ttfb_ms,
            self.telemetry.peak_bytes,
            self.workflow_sha256,
            self.stdout_sha256
        );
        if let Some(error) = &self.error {
            write!(body, "<pre class=\"error\">{}</pre>", escape(error)).unwrap();
        }
        body.push_str("<h2>Steps</h2><table><thead><tr><th>Step</th><th>Layer</th><th>Status</th><th>Duration (ms)</th><th>TTFB (ms)</th><th>Stdout bytes</th><th>Stderr bytes</th></tr></thead><tbody>");
        for step in &self.telemetry.steps {
            write!(body,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape(&step.id), step.layer, escape(&step.status), step.duration_ms,
                step.ttfb_ms.map_or_else(|| "—".into(), |value| format!("{value:.2}")),
                step.stdout_bytes, step.stderr_bytes,
            ).unwrap();
        }
        body.push_str("</tbody></table>");
        body
    }
}

pub(crate) fn reject_workflow_destination(report: &Path, workflow: &Path) -> Result<(), Error> {
    if std::fs::canonicalize(report).is_ok_and(|report| {
        std::fs::canonicalize(workflow).is_ok_and(|workflow| workflow == report)
    }) {
        return Err(Error::InvalidWorkflow(format!(
            "report path {report:?} would overwrite the workflow"
        )));
    }
    Ok(())
}

pub(crate) async fn write(
    path: &Path,
    title: &str,
    data: &impl Serialize,
    body: impl FnOnce() -> String,
) -> Result<(), Error> {
    let json = serde_json::to_string_pretty(data).map_err(|source| Error::Report {
        path: path.to_owned(),
        source: io::Error::other(source),
    })?;
    let html = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
        });
    let document = if html {
        let title = escape(title);
        format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{title}</title><style>{CSS}</style></head>\
             <body><main><p class=\"label\">stdo offline report</p><h1>{title}</h1>{}\
             <details><summary>Full report data (JSON)</summary><pre>{}</pre></details>\
             </main></body></html>\n",
            body(),
            escape(&json)
        )
    } else {
        json + "\n"
    };
    atomic_file::write(path, document.as_bytes(), 0o600)
        .await
        .map_err(|source| Error::Report {
            path: path.to_owned(),
            source,
        })
}

pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const CSS: &str = "
:root{color-scheme:dark;font:15px system-ui,sans-serif;background:#101725;color:#e8eef8}
body{margin:0}main{max-width:1060px;margin:auto;padding:30px 20px}
h1,h2{line-height:1.15}.label,dt{color:#a8b5ca}.label{font-family:monospace}
dd{margin:0 0 10px}code,pre{font-family:ui-monospace,monospace;overflow-wrap:anywhere}
pre{white-space:pre-wrap;background:#0b101b;padding:14px;border-radius:6px}.error{color:#ffb4b4}
table{border-collapse:collapse;width:100%;margin:12px 0 20px}th,td{text-align:left;padding:8px;border-bottom:1px solid #34445f}
details{margin-top:22px}summary{cursor:pointer}figure{margin:16px 0;padding:12px;background:#192437;border-radius:7px}
figcaption{font-weight:650;margin-bottom:6px}svg{width:100%;height:auto;display:block}
";
