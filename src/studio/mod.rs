//! The offline Studio serves one bounded HTTP request per loopback connection.
mod artifact;
mod http;

use std::{
    io,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use serde::Deserialize;
use serde_json::json;
use tokio::{
    fs::{self, File},
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpListener,
    sync::Semaphore,
};

use crate::{
    atomic_file,
    dag::{graph::Graph, schema::Workflow},
    doctor,
    engine::{
        self,
        events::{EngineEvent, Events},
        ProcessLedger, RunOptions, Telemetry,
    },
    error::Error,
    report::sha256,
};

use http::{Problem, Request, Response, MAX_BODY};

const HTML: &str = include_str!("studio.html");

pub async fn serve(port: u16, initial: Option<&Path>) -> Result<(), Error> {
    let root = std::env::current_dir().map_err(Error::Studio)?;
    let root = fs::canonicalize(root).await.map_err(Error::Studio)?;
    let mut studio = Studio {
        root,
        initial: None,
        runs: Semaphore::new(1),
        artifacts: artifact::Store::default(),
    };
    if let Some(initial) = initial {
        let relative = initial.strip_prefix(&studio.root).unwrap_or(initial);
        let relative = relative
            .to_str()
            .ok_or_else(|| Error::Studio(io::Error::other("Workflow path must be UTF-8")))?;
        let path = studio.path(relative).await.map_err(|_| {
            Error::Studio(io::Error::other(
                "Choose a YAML workflow inside the working directory",
            ))
        })?;
        studio.initial = Some(
            path.strip_prefix(&studio.root)
                .expect("workspace path")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(Error::Studio)?;
    let address = listener.local_addr().map_err(Error::Studio)?;
    eprintln!("Studio listening on http://{address}");
    let studio = Arc::new(studio);
    let connections = Arc::new(Semaphore::new(16));
    loop {
        let permit = connections
            .clone()
            .acquire_owned()
            .await
            .expect("connection limit stays open");
        let (mut socket, _) = listener.accept().await.map_err(Error::Studio)?;
        let studio = studio.clone();
        tokio::spawn(async move {
            let _permit = permit;
            studio.connection(&mut socket, address.port()).await;
        });
    }
}

struct Studio {
    root: PathBuf,
    initial: Option<String>,
    runs: Semaphore,
    artifacts: artifact::Store,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Draft {
    path: Option<String>,
    workflow: Option<Workflow>,
    yaml: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Run {
    #[serde(flatten)]
    draft: Draft,
    inputs: Vec<String>,
    no_cache: bool,
}

impl Draft {
    fn parse(self) -> Result<(Workflow, String), Problem> {
        match (self.workflow, self.yaml) {
            (Some(workflow), None) => {
                let yaml = canonical(&workflow)?;
                Ok((workflow, yaml))
            }
            (None, Some(yaml)) => parse_yaml(yaml),
            _ => Err(Problem::bad(
                "Send either a workflow object or a YAML string",
            )),
        }
    }
}

impl Studio {
    async fn connection(&self, socket: &mut (impl AsyncRead + AsyncWrite + Unpin), port: u16) {
        let response = match Request::read(socket, port).await {
            Ok(request) if request.method == "POST" && request.target == "/api/run/stream" => {
                match self.run_stream(socket, &request.body).await {
                    Ok(()) => return,
                    Err(error) => Err(error),
                }
            }
            Ok(request)
                if matches!(request.method.as_str(), "GET" | "HEAD")
                    && request.target.split('?').next() == Some("/api/artifact") =>
            {
                match self.open_artifact(&request.target).await {
                    Ok((file, size, mime)) => {
                        let _ = http::send_file(socket, file, size, mime, &request).await;
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
            Ok(request) => self.route(request).await,
            Err(error) => Err(error),
        };
        response.unwrap_or_else(Response::from).send(socket).await;
    }

    async fn open_artifact(&self, target: &str) -> Result<(File, u64, &'static str), Problem> {
        let (_, query) = target.split_once('?').unwrap_or((target, ""));
        let path = http::query_path(query)?;
        self.artifacts.open(&self.root, &path).await
    }

    async fn route(&self, request: Request) -> Result<Response, Problem> {
        let (target, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        match (request.method.as_str(), target) {
            ("GET", "/") => Ok(Response::html(HTML)),
            ("GET", "/api/workflows") => self.list().await,
            ("GET", "/api/workflow") => {
                let relative = http::query_path(query)?;
                let (workflow, yaml) = self.load(&relative).await?;
                Ok(inspect(&workflow, yaml))
            }
            ("GET", "/api/workflow/meta") => {
                let relative = http::query_path(query)?;
                let bytes = self.workflow_bytes(&relative).await?;
                Ok(Response::json(json!({"sha256": sha256(&bytes)})))
            }
            ("POST", "/api/preview") => {
                let (workflow, yaml) = decode::<Draft>(&request.body)?.parse()?;
                Ok(inspect(&workflow, yaml))
            }
            ("POST", "/api/workflow") => {
                let mut draft = decode::<Draft>(&request.body)?;
                let relative = draft
                    .path
                    .take()
                    .ok_or_else(|| Problem::bad("A workflow path is required"))?;
                let path = self.path(&relative).await?;
                let (workflow, _) = draft.parse()?;
                Graph::build(&workflow).map_err(|error| Problem::bad(error.to_string()))?;
                let yaml = canonical(&workflow)?;
                atomic_file::write(&path, yaml.as_bytes(), 0o666)
                    .await
                    .map_err(Problem::io)?;
                Ok(inspect(&workflow, yaml))
            }
            ("DELETE", "/api/workflow") => {
                let path = self.path(&http::query_path(query)?).await?;
                fs::remove_file(path).await.map_err(Problem::io)?;
                Ok(Response::json(json!({"deleted": true})))
            }
            ("POST", "/api/run") => self.run(decode(&request.body)?).await,
            (
                _,
                "/" | "/api/workflows" | "/api/workflow" | "/api/workflow/meta" | "/api/preview"
                | "/api/run" | "/api/run/stream" | "/api/artifact",
            ) => Err(Problem::new(
                "405 Method Not Allowed",
                "Method not allowed for this endpoint",
            )),
            _ => Err(Problem::new("404 Not Found", "Unknown Studio endpoint")),
        }
    }

    async fn list(&self) -> Result<Response, Problem> {
        let mut paths = Vec::new();
        for directory in [self.root.clone(), self.root.join("examples")] {
            match fs::symlink_metadata(&directory).await {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(Problem::io(error)),
            }
            let mut entries = fs::read_dir(directory).await.map_err(Problem::io)?;
            while let Some(entry) = entries.next_entry().await.map_err(Problem::io)? {
                let path = entry.path();
                if yaml_path(&path) && entry.file_type().await.map_err(Problem::io)?.is_file() {
                    if let Some(relative) =
                        path.strip_prefix(&self.root).ok().and_then(Path::to_str)
                    {
                        paths.push(relative.to_owned());
                    }
                }
            }
        }
        if let Some(initial) = &self.initial {
            if !paths.contains(initial) {
                paths.push(initial.clone());
            }
        }
        paths.sort_unstable();
        let selected = self
            .initial
            .as_ref()
            .or_else(|| paths.iter().find(|path| *path == "stdo.yaml"))
            .or_else(|| paths.first());
        Ok(Response::json(
            json!({"workflows": paths, "selected": selected}),
        ))
    }

    async fn path(&self, relative: &str) -> Result<PathBuf, Problem> {
        let mut normalized = PathBuf::new();
        for component in Path::new(relative).components() {
            match component {
                Component::Normal(name) => normalized.push(name),
                Component::CurDir => {}
                _ => return Err(Problem::bad("Use a path inside the working directory")),
            }
        }
        if !yaml_path(&normalized) {
            return Err(Problem::bad("Use a .yaml or .yml workflow file"));
        }
        let path = self.root.join(normalized);
        let parent = path.parent().expect("workflow has a workspace parent");
        if fs::canonicalize(parent).await.map_err(Problem::io)? != parent {
            return Err(Problem::bad("Workflow directories must not be symlinks"));
        }
        match fs::symlink_metadata(&path).await {
            Ok(metadata) if !metadata.is_file() => {
                Err(Problem::bad("Workflow must be a regular file"))
            }
            Ok(_) => Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(error) => Err(Problem::io(error)),
        }
    }

    async fn load(&self, relative: &str) -> Result<(Workflow, String), Problem> {
        let bytes = self.workflow_bytes(relative).await?;
        let yaml = String::from_utf8(bytes).map_err(|_| Problem::bad("Workflow must be UTF-8"))?;
        parse_yaml(yaml)
    }

    async fn workflow_bytes(&self, relative: &str) -> Result<Vec<u8>, Problem> {
        let file = File::open(self.path(relative).await?)
            .await
            .map_err(Problem::io)?;
        let mut yaml = Vec::new();
        file.take((MAX_BODY + 1) as u64)
            .read_to_end(&mut yaml)
            .await
            .map_err(Problem::io)?;
        if yaml.len() > MAX_BODY {
            return Err(Problem::new(
                "413 Content Too Large",
                "Workflow file exceeds 1 MiB",
            ));
        }
        Ok(yaml)
    }

    async fn run_workflow(&self, draft: Draft) -> Result<Workflow, Problem> {
        if draft.workflow.is_some() || draft.yaml.is_some() {
            Ok(draft.parse()?.0)
        } else {
            let path = draft
                .path
                .as_deref()
                .ok_or_else(|| Problem::bad("A workflow path or draft is required"))?;
            Ok(self.load(path).await?.0)
        }
    }

    fn run_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, Problem> {
        self.runs
            .try_acquire()
            .map_err(|_| Problem::new("409 Conflict", "A workflow is already running"))
    }

    async fn run(&self, request: Run) -> Result<Response, Problem> {
        let _permit = self.run_permit()?;
        let workflow = self.run_workflow(request.draft).await?;
        let result = self
            .execute(
                &workflow,
                &request.inputs,
                request.no_cache,
                &Telemetry::for_studio(),
            )
            .await;
        Ok(Response::json(result))
    }

    async fn run_stream(
        &self,
        socket: &mut (impl AsyncWrite + Unpin),
        body: &[u8],
    ) -> Result<(), Problem> {
        let request: Run = decode(body)?;
        let _permit = self.run_permit()?;
        let workflow = self.run_workflow(request.draft).await?;
        // Request::read has already consumed the complete body before this response.
        if http::start_stream(socket).await.is_err() {
            return Ok(());
        }
        let (events, receiver) = Events::channel();
        let telemetry = Telemetry::for_studio().with_events(events.clone());
        let run = async move {
            let mut result = self
                .execute(&workflow, &request.inputs, request.no_cache, &telemetry)
                .await;
            result["live_stdout_truncated"] = json!(events.truncated());
            events.emit(EngineEvent::WorkflowFinished { result });
        };
        // Let the engine finish and reap its processes if the browser disconnects.
        tokio::join!(run, http::stream(socket, receiver));
        Ok(())
    }
    async fn execute(
        &self,
        workflow: &Workflow,
        inputs: &[String],
        no_cache: bool,
        telemetry: &Telemetry,
    ) -> serde_json::Value {
        let ledger = ProcessLedger::default();
        let mut stdout = Vec::new();
        let result = engine::run_with_options(
            workflow,
            inputs,
            &ledger,
            &mut stdout,
            RunOptions {
                no_cache,
                telemetry: Some(telemetry),
                ..RunOptions::default()
            },
        )
        .await;
        let (report, stderr, stderr_truncated) = telemetry.studio_report();
        let pid_ledger = if result.is_err() {
            ledger.snapshot()
        } else {
            Vec::new()
        };
        let artifacts = if result.is_ok() {
            self.artifacts.register(&self.root, workflow, inputs).await
        } else {
            Vec::new()
        };
        let error = result.err().map(|error| match error {
            Error::StepFailed { step, status, .. } => format!("step {step:?} failed with {status}"),
            other => other.to_string(),
        });
        json!({
            "ok": error.is_none(), "error": error, "stdout": String::from_utf8_lossy(&stdout),
            "stderr": stderr, "stderr_truncated": stderr_truncated, "telemetry": report, "pid_ledger": pid_ledger,
            "artifacts": artifacts
        })
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Problem> {
    serde_json::from_slice(body).map_err(|error| Problem::bad(error.to_string()))
}

fn yaml_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("yaml" | "yml")
    )
}

fn parse_yaml(yaml: String) -> Result<(Workflow, String), Problem> {
    let workflow = Workflow::from_yaml(&yaml).map_err(|error| Problem::bad(error.to_string()))?;
    Ok((workflow, yaml))
}

fn canonical(workflow: &Workflow) -> Result<String, Problem> {
    let yaml = serde_yaml::to_string(workflow).map_err(|error| Problem::bad(error.to_string()))?;
    if yaml.len() > MAX_BODY {
        return Err(Problem::new(
            "413 Content Too Large",
            "Serialized workflow exceeds 1 MiB",
        ));
    }
    Ok(yaml)
}

fn inspect(workflow: &Workflow, yaml: String) -> Response {
    let (layers, graph_error) = match Graph::build(workflow) {
        Ok(graph) => (graph.layers, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let doctor_error = graph_error
        .clone()
        .or_else(|| doctor::check(workflow).err().map(|error| error.to_string()));
    Response::json(json!({
        "workflow": workflow, "sha256": sha256(yaml.as_bytes()), "yaml": yaml, "layers": layers, "graph_error": graph_error,
        "doctor": {"ok": doctor_error.is_none(), "error": doctor_error}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn exchange(studio: &Studio, request: String) -> (String, String) {
        let (headers, body) = exchange_bytes(studio, request).await;
        (headers, String::from_utf8(body).unwrap())
    }

    async fn exchange_bytes(studio: &Studio, request: String) -> (String, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let response = async {
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            let end = response
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap();
            let headers = String::from_utf8(response[..end].to_vec()).unwrap();
            (headers + "\r\n", response.split_off(end + 4))
        };
        tokio::join!(
            async move { studio.connection(&mut server, 8080).await },
            response
        )
        .1
    }

    #[tokio::test]
    async fn streams_wav_metadata_and_serves_only_allowed_audio_with_ranges() {
        const AUDIO: &[u8] = b"RIFF\x04\0\0\0WAVE\xff";
        let directory =
            std::env::temp_dir().join(format!("stdo-studio-audio-{}", std::process::id()));
        fs::create_dir(&directory).await.unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(fs::canonicalize(directory).await.unwrap());
        let workspace = fixture.0.join("workspace");
        fs::create_dir(&workspace).await.unwrap();
        let outside = fixture.0.join("outside.wav");
        fs::write(&outside, AUDIO).await.unwrap();
        fs::write(workspace.join("local.wav"), AUDIO).await.unwrap();
        fs::write(workspace.join("fake.wav"), b"unrelated data")
            .await
            .unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("link.wav")).unwrap();
        let studio = Studio {
            root: workspace,
            initial: None,
            runs: Semaphore::new(1),
            artifacts: artifact::Store::default(),
        };
        let request = |method: &str, path: &str, headers: &str| {
            format!(
            "{method} /api/artifact?path={path} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n{headers}\r\n"
        )
        };
        let outside = outside.to_str().unwrap();
        for path in [outside, "fake.wav", "link.wav", "%2e%2e/outside.wav"] {
            let (headers, _) = exchange_bytes(&studio, request("GET", path, "")).await;
            assert!(headers.starts_with("HTTP/1.1 404 Not Found"), "{headers}");
        }
        let (headers, body) = exchange_bytes(&studio, request("GET", "local.wav", "")).await;
        assert!(headers.contains("Content-Type: audio/wav"));
        assert_eq!(body, AUDIO);
        let (headers, _) = exchange_bytes(
            &studio,
            request("GET", "local.wav", "Sec-Fetch-Site: cross-site\r\n"),
        )
        .await;
        assert!(headers.starts_with("HTTP/1.1 403 Forbidden"), "{headers}");

        let draft = json!({"inputs":[format!("output_wav={outside}")],"workflow":{
            "version":"1.0", "name":"audio",
            "inputs":{"output_wav":{"type":"path","default":"missing.wav"}},
            "steps":[
                {"id":"speech","cmd":["sh","-c",
                    "printf RI; sleep 0.05; printf 'FF\\004\\000\\000\\000WAVE\\377'; head -c 600000 /dev/zero"],
                    "capture_stdout":false},
                {"id":"summary","cmd":["echo","byte summary"],"stdin":"{{ steps.speech.exit_code }}"}
            ]
        }});
        let (headers,body) = exchange(&studio,format!(
            "POST /api/run/stream HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{draft}",
            draft.to_string().len()
        )).await;
        assert!(headers.contains("Content-Type: text/event-stream"));
        let events: Vec<serde_json::Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events
            .iter()
            .any(|event| event["type"] == "step_stdout_binary"
                && event["id"] == "speech"
                && event["format"] == "RIFF/WAV audio"));
        let chunks: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "step_stdout_chunk")
            .collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["id"], "summary");
        let result = &events.last().unwrap()["result"];
        assert_eq!(result["artifacts"][0]["path"], outside);
        assert_eq!(
            result["telemetry"]["steps"][0]["stdout_bytes"],
            AUDIO.len() + 600_000
        );
        assert_eq!(result["live_stdout_truncated"], false);

        let (headers, body) =
            exchange_bytes(&studio, request("GET", outside, "Range: bytes=0-1\r\n")).await;
        assert!(
            headers.starts_with("HTTP/1.1 206 Partial Content"),
            "{headers}"
        );
        assert!(headers.contains("Content-Range: bytes 0-1/13"));
        assert_eq!(body, b"RI");
        let (headers, body) = exchange_bytes(&studio, request("HEAD", outside, "")).await;
        assert!(headers.contains("Content-Length: 13"));
        assert!(body.is_empty());
        let (headers, body) =
            exchange_bytes(&studio, request("GET", outside, "Range: bytes=99-\r\n")).await;
        assert!(headers.starts_with("HTTP/1.1 416 Range Not Satisfiable"));
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn handles_http_over_an_in_memory_stream() {
        let studio = Studio {
            root: fs::canonicalize(env!("CARGO_MANIFEST_DIR")).await.unwrap(),
            initial: None,
            runs: Semaphore::new(1),
            artifacts: artifact::Store::default(),
        };
        let (headers, body) = exchange(
            &studio,
            "GET /api/workflow?path=examples%2F02-monitored-pipe-etl.yaml HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n".into(),
        ).await;
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");
        let data: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(data["layers"], json!([[0], [1], [2], [3], [4]]));
        assert_eq!(data["doctor"]["ok"], true);

        let body = json!({"workflow": {
            "version": "1.0", "name": "memory-test",
            "steps": [{
                "id": "say", "cmd": ["echo", "hello"], "timeout_ms": 5000,
                "retry": {"attempts": 2}, "cache_files": ["Cargo.toml"], "cache_env": ["LANG"]
            }],
            "output": "{{ steps.say.stdout }}"
        }})
        .to_string();
        let (headers, response) = exchange(&studio, format!(
            "POST /api/preview HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )).await;
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");
        let draft: serde_json::Value = serde_json::from_str(&response).unwrap();
        let step = &draft["workflow"]["steps"][0];
        assert_eq!(step["timeout_ms"], 5000);
        assert_eq!(step["retry"], json!({"attempts": 2, "backoff_ms": 0}));
        assert_eq!(step["cache_files"], json!(["Cargo.toml"]));
        assert_eq!(step["cache_env"], json!(["LANG"]));
        let mut draft = draft;
        draft["workflow"]["steps"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "sink", "cmd": ["cat"], "pipe_from": "say",
                "capture_stdout": false, "pipe_mode": "atomic"
            }));
        let draft = json!({"workflow": draft["workflow"]}).to_string();
        let (headers, response) = exchange(&studio, format!(
            "POST /api/preview HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{draft}",
            draft.len()
        )).await;
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");
        let draft: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(draft["graph_error"], serde_json::Value::Null);
        assert_eq!(draft["workflow"]["steps"][1]["capture_stdout"], false);
        assert_eq!(draft["workflow"]["steps"][1]["pipe_mode"], "atomic");

        let (headers, response) = exchange(&studio, format!(
            "POST /api/run HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )).await;
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");
        let result: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(result["stdout"], "hello\n");
        assert_eq!(result["telemetry"]["steps"][0]["stdout_bytes"], 6);
        assert_eq!(result["telemetry"]["steps"][0]["attempts"], 1);

        let (headers, response) = exchange(&studio, format!(
            "POST /api/run/stream HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )).await;
        assert!(headers.contains("Content-Type: text/event-stream\r\n"));
        let events: Vec<serde_json::Value> = response
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "layer_started",
                "step_started",
                "step_stdout_chunk",
                "step_finished",
                "workflow_finished"
            ]
        );
        assert_eq!(events[4]["result"]["stdout"], "hello\n");

        let (headers, _) = exchange(&studio, "POST /api/run HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n".into()).await;
        assert!(
            headers.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{headers}"
        );
    }

    #[tokio::test]
    async fn rejects_cross_origin_post_after_consuming_its_framed_body() {
        let studio = Studio {
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            initial: None,
            runs: Semaphore::new(1),
            artifacts: artifact::Store::default(),
        };
        // Larger than the server's initial read buffer and the duplex capacity.
        let body = "x".repeat(8192);
        let request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nOrigin: https://example.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (headers, response) = exchange(&studio, request).await;
        assert!(
            headers.starts_with("HTTP/1.1 403 Forbidden\r\n"),
            "{headers}"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap(),
            json!({"error": "Cross-origin requests are blocked"})
        );
    }
}
