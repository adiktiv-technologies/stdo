//! Sequential benchmark trials using the regular workflow engine.
use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    dag::schema::Workflow,
    engine::{self, telemetry::ExecutionTelemetry, ProcessLedger, RunOptions, Telemetry},
    error::Error,
    report,
};

#[derive(Serialize)]
struct Stats {
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

impl Stats {
    fn new(mut samples: Vec<f64>) -> Self {
        samples.sort_by(f64::total_cmp);
        let quantile = |fraction: f64| {
            let index = (samples.len() - 1) as f64 * fraction;
            let lower = index.floor() as usize;
            let upper = index.ceil() as usize;
            samples[lower] + (samples[upper] - samples[lower]) * index.fract()
        };
        Self {
            min_ms: samples[0],
            mean_ms: samples.iter().sum::<f64>() / samples.len() as f64,
            p50_ms: quantile(0.50),
            p95_ms: quantile(0.95),
            max_ms: samples[samples.len() - 1],
        }
    }
}

#[derive(Serialize)]
struct StepSummary {
    id: String,
    samples: usize,
    mean_ms: f64,
    p95_ms: f64,
}

#[derive(Serialize)]
struct Sample {
    duration_ms: f64,
    ttfb_ms: f64,
    peak_bytes: usize,
    cached_steps: usize,
    total_steps: usize,
}

#[derive(Serialize)]
struct WorkflowSummary {
    label: String,
    path: String,
    name: String,
    latency: Stats,
    ttfb_mean_ms: f64,
    peak_bytes_max: usize,
    cached_steps: usize,
    total_steps: usize,
    cache_hit_ratio: f64,
    speedup_vs_baseline: Option<f64>,
    steps: Vec<StepSummary>,
    samples: Vec<Sample>,
}

#[derive(Serialize)]
struct Comparison {
    cache: &'static str,
    runs: usize,
    warmup: usize,
    workflows: Vec<WorkflowSummary>,
}

pub async fn run(
    files: &[PathBuf],
    runs: usize,
    warmup: usize,
    overrides: &[String],
    no_cache: bool,
    destination: Option<&Path>,
) -> Result<(), Error> {
    if runs == 0 || files.is_empty() {
        return Err(Error::InvalidWorkflow(
            "bench requires at least one workflow and --runs >= 1".into(),
        ));
    }
    if let Some(destination) = destination {
        for file in files {
            report::reject_workflow_destination(destination, file)?;
        }
    }

    let mut comparison = Comparison {
        cache: if no_cache { "bypassed" } else { "enabled" },
        runs,
        warmup,
        workflows: Vec::new(),
    };
    for (index, file) in files.iter().enumerate() {
        let workflow = Workflow::read(file)?;
        for trial in 0..warmup {
            run_trial(&workflow, file, overrides, no_cache, None)
                .await
                .map_err(|error| trial_error(file, "warmup", trial, error))?;
        }
        let mut samples = Vec::new();
        let mut by_step = BTreeMap::<String, Vec<f64>>::new();
        for trial in 0..runs {
            let telemetry = Telemetry::default();
            run_trial(&workflow, file, overrides, no_cache, Some(&telemetry))
                .await
                .map_err(|error| trial_error(file, "measured", trial, error))?;
            let ExecutionTelemetry {
                duration_ms,
                ttfb_ms,
                peak_bytes,
                steps,
                ..
            } = telemetry.snapshot();
            samples.push(Sample {
                duration_ms,
                ttfb_ms,
                peak_bytes,
                cached_steps: steps.iter().filter(|step| step.cache_hit).count(),
                total_steps: steps.len(),
            });
            for step in steps.into_iter().filter(|step| step.kind != "skipped") {
                by_step.entry(step.id).or_default().push(step.duration_ms);
            }
        }
        let latency = Stats::new(samples.iter().map(|sample| sample.duration_ms).collect());
        let cached_steps = samples
            .iter()
            .map(|sample| sample.cached_steps)
            .sum::<usize>();
        let total_steps = samples
            .iter()
            .map(|sample| sample.total_steps)
            .sum::<usize>();
        let baseline = comparison
            .workflows
            .first()
            .map_or(latency.p50_ms, |workflow| workflow.latency.p50_ms);
        let speedup_vs_baseline =
            (latency.p50_ms > 0.0 && baseline > 0.0).then(|| baseline / latency.p50_ms);
        let steps = by_step
            .into_iter()
            .map(|(id, values)| {
                let samples = values.len();
                let stats = Stats::new(values);
                StepSummary {
                    id,
                    samples,
                    mean_ms: stats.mean_ms,
                    p95_ms: stats.p95_ms,
                }
            })
            .collect();
        comparison.workflows.push(WorkflowSummary {
            label: label(index),
            path: file.to_string_lossy().into_owned(),
            name: workflow.name,
            ttfb_mean_ms: samples.iter().map(|sample| sample.ttfb_ms).sum::<f64>() / runs as f64,
            peak_bytes_max: samples
                .iter()
                .map(|sample| sample.peak_bytes)
                .max()
                .unwrap_or(0),
            cache_hit_ratio: if total_steps == 0 {
                0.0
            } else {
                cached_steps as f64 / total_steps as f64
            },
            cached_steps,
            total_steps,
            speedup_vs_baseline,
            latency,
            steps,
            samples,
        });
    }
    print!("{comparison}");
    if let Some(path) = destination {
        report::write(path, "Pipeline benchmark", &comparison, || {
            comparison.html()
        })
        .await?;
    }
    Ok(())
}

async fn run_trial(
    workflow: &Workflow,
    source: &Path,
    overrides: &[String],
    no_cache: bool,
    telemetry: Option<&Telemetry>,
) -> Result<(), Error> {
    engine::run_with_source(
        workflow,
        Some(source),
        overrides,
        &ProcessLedger::default(),
        &mut io::sink(),
        RunOptions {
            no_cache,
            telemetry,
            ..RunOptions::default()
        },
    )
    .await
}

fn trial_error(file: &Path, phase: &str, trial: usize, error: Error) -> Error {
    Error::InvalidWorkflow(format!(
        "benchmark {file:?}, {phase} run {}: {error}",
        trial + 1
    ))
}

fn label(mut index: usize) -> String {
    let mut label = Vec::new();
    loop {
        label.push(char::from(b'A' + (index % 26) as u8));
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    label.into_iter().rev().collect()
}

fn relative(index: usize, speedup: Option<f64>) -> String {
    if index == 0 {
        "1.00x (baseline)".into()
    } else {
        match speedup {
            Some(value) if value >= 1.0 => format!("{value:.2}x faster"),
            Some(value) => format!("{:.2}x slower", 1.0 / value),
            None => "n/a".into(),
        }
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            output,
            "stdo bench | runs: {} | warmup: {} | cache: {}",
            self.runs, self.warmup, self.cache
        )?;
        writeln!(
            output,
            "Latencies and ttfb in ms; relative to workflow A (p50)."
        )?;
        for workflow in &self.workflows {
            writeln!(output, "  {}  {}", workflow.label, workflow.path)?;
        }
        writeln!(
            output,
            "\n{:<8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>12} {:>8}  Relative",
            "Workflow", "min", "mean", "p50", "p95", "max", "ttfb", "peak_bytes", "Cache"
        )?;
        for (index, workflow) in self.workflows.iter().enumerate() {
            let stats = &workflow.latency;
            writeln!(
                output,
                "{:<8} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>12} {:>7.1}%  {}",
                workflow.label,
                stats.min_ms,
                stats.mean_ms,
                stats.p50_ms,
                stats.p95_ms,
                stats.max_ms,
                workflow.ttfb_mean_ms,
                workflow.peak_bytes_max,
                workflow.cache_hit_ratio * 100.0,
                relative(index, workflow.speedup_vs_baseline)
            )?;
        }
        for workflow in &self.workflows {
            writeln!(output, "\n{} per-step timings (ms)", workflow.label)?;
            writeln!(
                output,
                "  {:<24} {:>7} {:>9} {:>9}",
                "Step", "Samples", "Mean", "p95"
            )?;
            for step in &workflow.steps {
                writeln!(
                    output,
                    "  {:<24} {:>7} {:>9.2} {:>9.2}",
                    step.id, step.samples, step.mean_ms, step.p95_ms
                )?;
            }
        }
        Ok(())
    }
}

impl Comparison {
    fn html(&self) -> String {
        let mut body = format!(
            "<p>{} measured runs · {} warmup · cache: {}</p><dl>",
            self.runs, self.warmup, self.cache
        );
        for workflow in &self.workflows {
            write!(
                body,
                "<dt>{} · {}</dt><dd>{}</dd>",
                workflow.label,
                report::escape(&workflow.name),
                report::escape(&workflow.path)
            )
            .unwrap();
        }
        body.push_str("</dl><h2>Measured results</h2>");
        self.chart(&mut body, "p50 latency (ms)", |workflow| {
            workflow.latency.p50_ms
        });
        self.chart(&mut body, "p95 latency (ms)", |workflow| {
            workflow.latency.p95_ms
        });
        self.chart(&mut body, "Mean TTFB (ms)", |workflow| {
            workflow.ttfb_mean_ms
        });
        self.chart(&mut body, "Peak monitored bytes", |workflow| {
            workflow.peak_bytes_max as f64
        });
        write!(
            body,
            "<h2>Comparison and per-step timings</h2><pre>{}</pre>",
            report::escape(&self.to_string())
        )
        .unwrap();
        body
    }

    fn chart(&self, body: &mut String, title: &str, measure: impl Fn(&WorkflowSummary) -> f64) {
        let max = self.workflows.iter().map(&measure).fold(0.0, f64::max);
        let height = self.workflows.len() * 28 + 8;
        write!(body, "<figure><figcaption>{title}</figcaption><svg role=\"img\" aria-label=\"{title}\" viewBox=\"0 0 600 {height}\" xmlns=\"http://www.w3.org/2000/svg\">").unwrap();
        for (index, workflow) in self.workflows.iter().enumerate() {
            let value = measure(workflow);
            let width = if max > 0.0 { value / max * 420.0 } else { 0.0 };
            let y = index * 28 + 4;
            write!(body, "<text x=\"0\" y=\"{}\" fill=\"#e8eef8\" font-size=\"12\">{}</text><rect x=\"42\" y=\"{y}\" width=\"{width:.2}\" height=\"17\" rx=\"3\" fill=\"#6ee7c8\"/><text x=\"{}\" y=\"{}\" fill=\"#e8eef8\" font-size=\"12\">{value:.2}</text>",
                y + 13, workflow.label, 49.0 + width, y + 13).unwrap();
        }
        body.push_str("</svg></figure>");
    }
}
