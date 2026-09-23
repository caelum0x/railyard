//! `ranguard-server` — the Ranguard HTTP service (zero dependencies).
//!
//! A small HTTP/1.1 server on `std::net::TcpListener` (one thread per
//! connection, capped) in front of [`ranguard::service::Service`]: signed
//! release manifests, integrity scans of client-reported fingerprints, and
//! anti-replay heartbeat sessions. Run it behind a TLS-terminating proxy; it
//! speaks plain HTTP and closes each connection after one response.
//!
//! Routes (all `/v1` routes need `Authorization: Bearer <RANGUARD_API_TOKEN>`):
//!
//! | Method | Path | Purpose |
//! |---|---|---|
//! | GET | `/healthz` | liveness (public) |
//! | GET | `/v1/stats` | open sessions, registered manifests |
//! | POST | `/v1/manifests` | sign + register `{root?, label?, entries:[{path,size,sha256}]}` |
//! | GET | `/v1/manifests` | list registered manifests |
//! | GET/DELETE | `/v1/manifests/:id` | fetch (with signed text) / delete |
//! | POST | `/v1/manifests/verify` | check a signed manifest text `{manifest}` |
//! | POST | `/v1/manifests/scan` | diff `{manifest_id | manifest, files:[...]}` |
//! | POST | `/v1/sessions` | open a heartbeat session `{label?}` -> id + session key |
//! | GET/DELETE | `/v1/sessions/:id` | status / close |
//! | POST | `/v1/sessions/:id/challenge` | issue `{counter, nonce}` |
//! | POST | `/v1/sessions/:id/heartbeat` | verify `{counter, nonce, tag}` |
//!
//! Configuration:
//! - `RANGUARD_KEY` (required, >= 16 bytes) — manifest signing key; also derives per-session keys.
//! - `RANGUARD_API_TOKEN` (required, >= 24 chars) — bearer token for `/v1`.
//! - `RANGUARD_BIND` — listen address (default `0.0.0.0:8110`).
//! - `RANGUARD_DATA_DIR` — persist registered manifests here (default: memory only).
//! - `RANGUARD_SESSION_TTL_SECS` — heartbeat session idle timeout (default 3600).
//! - `RANGUARD_MAX_SESSIONS` — open-session cap (default 100000).
//! - `RANGUARD_MAX_CONNECTIONS` — concurrent connection cap (default 256).

#![forbid(unsafe_code)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ranguard::service::{
    Config, Reply, Service, DEFAULT_MAX_SESSIONS, DEFAULT_SESSION_TTL_SECS, MAX_BODY_BYTES,
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MAX_CONNECTIONS: usize = 256;

fn main() {
    if let Err(e) = run() {
        eprintln!("ranguard-server: {e}");
        std::process::exit(1);
    }
}

fn env_required(key: &str) -> io::Result<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{key} must be set"),
        )),
    }
}

fn env_number<T: std::str::FromStr>(key: &str, default: T) -> io::Result<T> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{key} must be a positive integer"),
            )
        }),
        _ => Ok(default),
    }
}

fn run() -> io::Result<()> {
    let config = Config {
        signing_key: env_required("RANGUARD_KEY")?.into_bytes(),
        api_token: env_required("RANGUARD_API_TOKEN")?,
        data_dir: std::env::var("RANGUARD_DATA_DIR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from),
        session_ttl_secs: env_number("RANGUARD_SESSION_TTL_SECS", DEFAULT_SESSION_TTL_SECS)?
            .max(1),
        max_sessions: env_number("RANGUARD_MAX_SESSIONS", DEFAULT_MAX_SESSIONS)?.max(1),
    };
    let max_connections: usize =
        env_number("RANGUARD_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?.max(1);
    let bind = std::env::var("RANGUARD_BIND").unwrap_or_else(|_| "0.0.0.0:8110".to_string());

    let persistent = config.data_dir.is_some();
    let service = Arc::new(Service::new(config)?);
    let listener = TcpListener::bind(&bind)
        .map_err(|e| io::Error::new(e.kind(), format!("binding {bind}: {e}")))?;
    eprintln!(
        "ranguard-server listening on {bind} (manifests {})",
        if persistent { "persisted" } else { "in memory" }
    );

    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ranguard-server: accept failed: {e}");
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= max_connections {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = write_reply(
                &mut stream,
                &Reply::error(503, "busy", "too many concurrent connections"),
                false,
            );
            continue;
        }
        let service = Arc::clone(&service);
        let active = Arc::clone(&active);
        std::thread::spawn(move || {
            if let Err(e) = serve(&service, stream) {
                if e.kind() != io::ErrorKind::WouldBlock && e.kind() != io::ErrorKind::TimedOut {
                    eprintln!("ranguard-server: connection error: {e}");
                }
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    bearer: Option<String>,
    body: Vec<u8>,
}

/// Read one request. `Err(reply)` is a protocol error to report to the client.
fn read_request(stream: &TcpStream) -> io::Result<Result<Request, Reply>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut header_bytes = 0usize;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    header_bytes += line.len();
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(Err(Reply::error(400, "bad_request", "malformed request line")));
    };
    if !version.starts_with("HTTP/1.") {
        return Ok(Err(Reply::error(505, "http_version", "HTTP/1.x only")));
    }
    let (method, path) = (method.to_string(), path.to_string());

    let mut content_length = 0usize;
    let mut bearer = None;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 {
            return Ok(Err(Reply::error(400, "bad_request", "truncated headers")));
        }
        header_bytes += n;
        if header_bytes > MAX_HEADER_BYTES {
            return Ok(Err(Reply::error(431, "headers_too_large", "request headers too large")));
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Ok(Err(Reply::error(400, "bad_request", "malformed header")));
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => match value.parse::<usize>() {
                Ok(len) => content_length = len,
                Err(_) => return Ok(Err(Reply::error(400, "bad_request", "bad Content-Length"))),
            },
            "transfer-encoding" => {
                return Ok(Err(Reply::error(
                    411,
                    "length_required",
                    "chunked bodies are not supported; send Content-Length",
                )))
            }
            "authorization" => {
                bearer = value
                    .strip_prefix("Bearer ")
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
            }
            _ => {}
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Ok(Err(Reply::error(413, "payload_too_large", "request body too large")));
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Ok(Request {
        method,
        path,
        bearer,
        body,
    }))
}

fn serve(service: &Service, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let (reply, head_only) = match read_request(&stream)? {
        Ok(req) => {
            let reply = service.handle(&req.method, &req.path, req.bearer.as_deref(), &req.body);
            (reply, req.method == "HEAD")
        }
        Err(reply) => (reply, false),
    };
    write_reply(&mut stream, &reply, head_only)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

fn write_reply(stream: &mut TcpStream, reply: &Reply, head_only: bool) -> io::Result<()> {
    let body = reply.body.to_json();
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}
