use std::{io, time::Duration};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

pub(super) const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Request {
    pub method: String,
    pub target: String,
    pub body: Vec<u8>,
}

pub(super) struct Problem {
    status: &'static str,
    message: String,
}

impl Problem {
    pub fn new(status: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new("400 Bad Request", message)
    }

    pub fn io(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::new("404 Not Found", "Workflow file not found")
        } else {
            Self::new("500 Internal Server Error", error.to_string())
        }
    }
}

pub(super) struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    pub fn json(value: Value) -> Self {
        Self::json_status("200 OK", value)
    }

    fn json_status(status: &'static str, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(&value).expect("JSON value serializes"),
        }
    }

    pub fn html(body: &'static str) -> Self {
        Self {
            status: "200 OK",
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }

    pub async fn send(self, socket: &mut (impl AsyncWrite + Unpin)) {
        let headers = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
            self.status, self.content_type, self.body.len()
        );
        let _ = timeout(SOCKET_TIMEOUT, async {
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(&self.body).await?;
            socket.shutdown().await
        })
        .await;
    }
}

impl From<Problem> for Response {
    fn from(problem: Problem) -> Self {
        Self::json_status(problem.status, json!({"error": problem.message}))
    }
}

impl Request {
    pub async fn read(socket: &mut (impl AsyncRead + Unpin), port: u16) -> Result<Self, Problem> {
        timeout(SOCKET_TIMEOUT, Self::read_message(socket, port))
            .await
            .map_err(|_| {
                Problem::new("408 Request Timeout", "Timed out reading the HTTP request")
            })?
    }

    async fn read_message(
        socket: &mut (impl AsyncRead + Unpin),
        port: u16,
    ) -> Result<Self, Problem> {
        let mut data = Vec::new();
        let mut chunk = [0; 4096];
        let header_end = loop {
            if let Some(offset) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                if offset + 4 <= MAX_HEADERS {
                    break offset;
                }
            }
            if data.len() >= MAX_HEADERS {
                return Err(Problem::new(
                    "431 Request Header Fields Too Large",
                    "HTTP headers are too large",
                ));
            }
            let count = socket
                .read(&mut chunk)
                .await
                .map_err(|_| Problem::bad("Could not read request"))?;
            if count == 0 {
                return Err(Problem::bad("Incomplete HTTP headers"));
            }
            data.extend_from_slice(&chunk[..count]);
        };
        let headers = std::str::from_utf8(&data[..header_end])
            .map_err(|_| Problem::bad("Invalid HTTP headers"))?;
        let mut lines = headers.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split(' ');
        let (Some(method), Some(target), Some("HTTP/1.1"), None) = (
            request_line.next(),
            request_line.next(),
            request_line.next(),
            request_line.next(),
        ) else {
            return Err(Problem::bad("Expected an HTTP/1.1 request"));
        };
        if !target.starts_with('/') || target.bytes().any(|byte| !byte.is_ascii_graphic()) {
            return Err(Problem::bad("Invalid request target"));
        }
        let mut host = None;
        let mut origin = None;
        let mut length = None;
        let mut content_type = None;
        for line in lines {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| Problem::bad("Invalid HTTP header"))?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(Problem::bad("Invalid HTTP header name"));
            }
            let field = match key.to_ascii_lowercase().as_str() {
                "host" => &mut host,
                "origin" => &mut origin,
                "content-length" => &mut length,
                "content-type" => &mut content_type,
                "transfer-encoding" => {
                    return Err(Problem::bad("Transfer-Encoding is unsupported"))
                }
                _ => continue,
            };
            if field.replace(value.trim()).is_some() {
                return Err(Problem::bad("Duplicate HTTP header"));
            }
        }
        let host = host.ok_or_else(|| Problem::bad("Missing Host header"))?;
        if host != format!("127.0.0.1:{port}")
            && host != format!("localhost:{port}")
            && !(port == 80 && matches!(host, "127.0.0.1" | "localhost"))
        {
            return Err(Problem::new(
                "403 Forbidden",
                "Use the local Studio address",
            ));
        }
        if origin.is_some_and(|origin| origin != format!("http://{host}")) {
            return Err(Problem::new(
                "403 Forbidden",
                "Cross-origin requests are blocked",
            ));
        }
        if method == "POST"
            && !content_type.is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
            })
        {
            return Err(Problem::new(
                "415 Unsupported Media Type",
                "Send an application/json body",
            ));
        }
        let length = match length {
            Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                value
                    .parse::<usize>()
                    .map_err(|_| Problem::bad("Invalid Content-Length"))?
            }
            Some(_) => return Err(Problem::bad("Invalid Content-Length")),
            None if method == "POST" => {
                return Err(Problem::new(
                    "411 Length Required",
                    "Missing Content-Length",
                ))
            }
            None => 0,
        };
        if length > MAX_BODY {
            return Err(Problem::new(
                "413 Content Too Large",
                "Request body exceeds 1 MiB",
            ));
        }
        let method = method.to_owned();
        let target = target.to_owned();
        let mut body = Vec::with_capacity(length);
        let already_read = &data[header_end + 4..];
        body.extend_from_slice(&already_read[..length.min(already_read.len())]);
        let present = body.len();
        body.resize(length, 0);
        socket
            .read_exact(&mut body[present..])
            .await
            .map_err(|_| Problem::bad("Incomplete request body"))?;
        Ok(Self {
            method,
            target,
            body,
        })
    }
}

pub(super) fn query_path(query: &str) -> Result<String, Problem> {
    let value = query
        .strip_prefix("path=")
        .filter(|path| !path.is_empty() && !path.contains('&'))
        .ok_or_else(|| Problem::bad("Expected a path query parameter"))?;
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'%' => {
                let hex = |byte: Option<u8>| byte.and_then(|byte| (byte as char).to_digit(16));
                let high = hex(input.next()).ok_or_else(|| Problem::bad("Invalid path escape"))?;
                let low = hex(input.next()).ok_or_else(|| Problem::bad("Invalid path escape"))?;
                (high * 16 + low) as u8
            }
            b'+' => b' ',
            other => other,
        });
    }
    String::from_utf8(bytes).map_err(|_| Problem::bad("Path must be UTF-8"))
}
