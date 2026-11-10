use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use serde_json::{json, Value};

struct Studio {
    child: Child,
    root: PathBuf,
    address: SocketAddr,
}

impl Studio {
    fn start() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "stdo-studio-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("examples")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_stdo"))
            .args(["serve", "--port", "0", "-f", "stdo.yaml"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stderr.as_mut().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let address: SocketAddr = ready
            .trim()
            .strip_prefix("Studio listening on http://")
            .unwrap_or_else(|| panic!("unexpected startup output: {ready}"))
            .parse()
            .unwrap();
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(address.port(), 0);
        Self {
            child,
            root,
            address,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &str,
    ) -> (u16, String) {
        let mut socket = TcpStream::connect_timeout(&self.address, Duration::from_secs(5)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{headers}",
            self.address
        )
        .unwrap();
        if let Some(body) = body {
            let body = body.to_string();
            write!(
                socket,
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        } else {
            write!(socket, "\r\n").unwrap();
        }
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_owned())
    }

    fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Value {
        let (status, response) = self.request(method, path, body, "");
        assert_eq!(status, 200, "{response}");
        serde_json::from_str(&response).unwrap()
    }
}

impl Drop for Studio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn workflow() -> Value {
    json!({
        "version": "1.0",
        "name": "studio-test",
        "inputs": {"label": {"type": "string", "default": "before"}},
        "steps": [
            {"id": "first", "cmd": ["sh", "-c", "printf '%s' \"$1\"; printf note >&2", "stdo", "{{ inputs.label }}"], "cache": true},
            {"id": "second", "cmd": ["cat"], "pipe_from": "first"}
        ],
        "output": "{{ steps.second.stdout }}"
    })
}

#[test]
fn studio_serves_the_editor_and_round_trips_workflow_crud() {
    let studio = Studio::start();
    let (status, html) = studio.request("GET", "/", None, "");
    assert_eq!(status, 200);
    for id in ["toolbar", "edges-layer", "nodes-layer", "yaml-panel"] {
        assert!(html.contains(&format!("id=\"{id}\"")));
    }
    assert!(!html.contains("<script src="));

    let initial = studio.json("GET", "/api/workflows", None);
    assert_eq!(initial["selected"], "stdo.yaml");
    let file = "examples/a b.yaml";
    let url = "/api/workflow?path=examples%2Fa%20b.yaml";
    let saved = studio.json(
        "POST",
        "/api/workflow",
        Some(&json!({"path": file, "workflow": workflow()})),
    );
    assert_eq!(saved["workflow"]["name"], "studio-test");
    assert_eq!(saved["layers"], json!([[0], [1]]));
    assert_eq!(saved["doctor"]["ok"], true);
    let raw = fs::read_to_string(studio.root.join(file)).unwrap();
    assert_eq!(saved["yaml"], raw);
    assert_eq!(studio.json("GET", url, None), saved);
    assert!(studio.json("GET", "/api/workflows", None)["workflows"]
        .as_array()
        .unwrap()
        .contains(&json!(file)));

    let edited = raw.replace("name: studio-test", "name: revised");
    let preview = studio.json("POST", "/api/preview", Some(&json!({"yaml": edited})));
    assert_eq!(preview["workflow"]["name"], "revised");
    assert_eq!(fs::read_to_string(studio.root.join(file)).unwrap(), raw);
    let saved = studio.json(
        "POST",
        "/api/workflow",
        Some(&json!({"path": file, "yaml": edited})),
    );
    assert_eq!(studio.json("GET", url, None), saved);
    assert_eq!(saved["workflow"]["name"], "revised");

    let mut cyclic = workflow();
    cyclic["steps"][0]["pipe_from"] = json!("second");
    let (status, _) = studio.request(
        "POST",
        "/api/workflow",
        Some(&json!({"path": file, "workflow": cyclic})),
        "",
    );
    assert_eq!(status, 400);
    assert_eq!(studio.json("GET", url, None), saved);

    assert_eq!(studio.json("DELETE", url, None)["deleted"], true);
    assert_eq!(studio.request("GET", url, None, "").0, 404);
}

#[test]
fn studio_executes_with_overrides_cache_telemetry_and_failure_pids() {
    let studio = Studio::start();
    studio.json(
        "POST",
        "/api/workflow",
        Some(&json!({"path": "stdo.yaml", "workflow": workflow()})),
    );
    let run = |no_cache| {
        studio.json(
            "POST",
            "/api/run",
            Some(&json!({
                "path": "stdo.yaml", "inputs": ["label=from-studio"], "no_cache": no_cache
            })),
        )
    };
    for (no_cache, hit) in [(false, false), (false, true), (true, false)] {
        let result = run(no_cache);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["stdout"], "from-studio");
        assert_eq!(result["stderr"], "note");
        let step = &result["telemetry"]["steps"][0];
        assert_eq!(step["id"], "first");
        assert_eq!(step["cache_hit"], hit);
        assert_eq!(step["stdout_bytes"], 11);
        assert_eq!(step["stderr_bytes"], 4);
        assert!(step["duration_ms"].is_number());
    }

    let result = studio.json(
        "POST",
        "/api/run",
        Some(&json!({"workflow": {
            "version": "1.0", "name": "failure",
            "steps": [{"id": "bad", "cmd": ["sh", "-c", "printf 'diagnostic' >&2; exit 7"]}]
        }})),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["stderr"], "diagnostic");
    assert_eq!(result["telemetry"]["steps"][0]["exit_code"], 7);
    assert_eq!(result["pid_ledger"][0]["step"], "bad");
    assert_eq!(result["pid_ledger"][0]["status"], "Exited(7)");
    assert!(result["pid_ledger"][0]["pid"].as_u64().unwrap() > 0);
}

#[test]
fn studio_rejects_cross_origin_requests_and_paths_outside_the_workspace() {
    let studio = Studio::start();
    let mut draft = json!({"path":"stdo.yaml","workflow":workflow()});
    draft["workflow"]["description"] = json!("x".repeat(8192));
    let (status, response) = studio.request(
        "POST",
        "/api/workflow",
        Some(&draft),
        "Origin: https://example.com\r\n",
    );
    assert_eq!(status, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap(),
        json!({"error": "Cross-origin requests are blocked"})
    );
    let (status, _) = studio.request("GET", "/", None, "Host: example.com\r\n");
    assert_eq!(status, 400);
    let (status, _) = studio.request("GET", "/api/workflow?path=%2e%2e%2fstdo.yaml", None, "");
    assert_eq!(status, 400);
    std::os::unix::fs::symlink(
        studio.root.join("target.yaml"),
        studio.root.join("link.yaml"),
    )
    .unwrap();
    let (status, _) = studio.request(
        "POST",
        "/api/workflow",
        Some(&json!({"path":"link.yaml","workflow":workflow()})),
        "",
    );
    assert_eq!(status, 400);
    assert!(!studio.root.join("stdo.yaml").exists());
}
