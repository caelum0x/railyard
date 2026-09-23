//! Git **smart-HTTP** server for `rit`.
//!
//! Implements the three endpoints a stock `git` client drives when the remote
//! URL is `http(s)://…`:
//!
//! | Method + path                     | Purpose                                   |
//! |-----------------------------------|-------------------------------------------|
//! | `GET  /info/refs?service=git-upload-pack`  | fetch/clone ref advertisement    |
//! | `GET  /info/refs?service=git-receive-pack` | push ref advertisement           |
//! | `POST /git-upload-pack`           | fetch/clone negotiation → `NAK` + packfile |
//! | `POST /git-receive-pack`          | push: pack + ref commands → report-status |
//!
//! ## Layering
//!
//! The **sync core** ([`info_refs`], [`upload_pack_rpc`], [`receive_pack_rpc`])
//! produces the exact response bytes from a [`Repository`]; it contains no async
//! and no socket code, so it is directly callable (and unit-testable) without a
//! runtime. It wraps the existing [`crate::upload_pack`] / [`crate::receive_pack`]
//! functions and adds only the smart-HTTP framing quirks: the
//! `# service=…\n` banner on `info/refs`, gzip request-body inflation, and the
//! `application/x-git-*` content types.
//!
//! The **async layer** ([`router`], [`serve`], [`serve_blocking`]) is a small
//! `axum` front door. Each handler moves the (fully buffered) request body onto
//! [`tokio::task::spawn_blocking`], opens the repository, and calls the sync
//! core — so the synchronous object/ref/pack plumbing is never run on an async
//! executor thread.
//!
//! ## Scope
//!
//! One repository per server instance (the endpoints live at the URL root). A
//! multi-repository forge composes several routers (or nests this one under a
//! per-repo path prefix); path-based repo routing, authentication, and TLS are
//! the embedding forge's concern, not this transport's.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::error::{ForgeError, Result};
use crate::receive::ReceiveLimits;
use crate::receive_pack::{
    advertise_receive_pack, execute_receive_pack, ReceivePackPolicy, RECEIVE_PACK_CAPABILITIES,
};
use crate::repo::Repository;
use crate::upload_pack::{advertise_upload_pack, upload_pack_v0};
use crate::wire::{
    encode_packet_line, encode_receive_status, parse_receive_pack_request, PacketLine,
    PacketLineLimits, ReceiveStatus,
};

/// Default ceiling on a buffered request body (1 GiB). Push packs can be large;
/// the per-packet and per-pack limits in [`PacketLineLimits`]/[`ReceiveLimits`]
/// bound the *processing* cost, while this caps the *buffering* cost so a single
/// request cannot exhaust memory.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024 * 1024;

/// The two Git services reachable over smart-HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// `git-upload-pack` — the fetch/clone (send objects to the client) side.
    UploadPack,
    /// `git-receive-pack` — the push (receive objects from the client) side.
    ReceivePack,
}

impl Service {
    /// Map a `service=…` query value to a [`Service`]. Unknown values yield
    /// `None`, which the caller turns into `403 Forbidden` (Git's convention for
    /// a service that is not enabled).
    pub fn from_query_value(value: &str) -> Option<Self> {
        match value {
            "git-upload-pack" => Some(Self::UploadPack),
            "git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    /// The wire service name (`git-upload-pack` / `git-receive-pack`).
    pub fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }
}

/// Server-side policy applied to pushes — never inferred from the client.
#[derive(Clone, Debug, Default)]
pub struct SmartHttpConfig {
    /// Branch short-names (e.g. `main`) whose direct push is refused with a
    /// per-ref `ng` status, mirroring the CLI's protected-branch enforcement.
    pub blocked_branches: Vec<String>,
    /// Allow a non-fast-forward branch update. A repository-owner decision, not
    /// a capability the pushing client can grant itself.
    pub allow_non_fast_forward: bool,
    /// Maximum buffered request-body size in bytes. Defaults to
    /// [`DEFAULT_MAX_BODY_BYTES`].
    pub max_body_bytes: usize,
}

impl SmartHttpConfig {
    /// A config with the default body cap and no protected branches.
    pub fn new() -> Self {
        Self {
            blocked_branches: Vec::new(),
            allow_non_fast_forward: false,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// A fully-formed HTTP response produced by the synchronous core: a status, a
/// static content type, and a body. The async layer copies these onto the
/// framework's response type verbatim.
#[derive(Clone, Debug)]
pub struct SmartResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl SmartResponse {
    fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            body,
        }
    }
}

// ---------------------------------------------------------------------------
// Synchronous core — no async, no sockets. Testable against a `Repository`.
// ---------------------------------------------------------------------------

/// Build the **smart-HTTP `info/refs` advertisement** for `service`.
///
/// Unlike the stateful (SSH/local) advertisement, smart-HTTP prefixes the ref
/// list with a service banner and a flush:
///
/// ```text
/// <pkt-line "# service=git-upload-pack\n">
/// 0000
/// <normal v0 ref advertisement, itself flush-terminated>
/// ```
///
/// The banner and its flush are exactly what a stock client verifies before it
/// reads the refs; the ref lines and capabilities come straight from the core
/// [`advertise_upload_pack`] / [`advertise_receive_pack`] functions.
pub fn info_refs(repo: &Repository, service: Service) -> Result<SmartResponse> {
    let mut body = encode_packet_line(&PacketLine::Data(
        format!("# service={}\n", service.name()).into_bytes(),
    ))?;
    body.extend_from_slice(b"0000");
    match service {
        Service::UploadPack => body.extend(advertise_upload_pack(repo)?),
        Service::ReceivePack => {
            body.extend(advertise_receive_pack(repo, RECEIVE_PACK_CAPABILITIES)?)
        }
    }
    let content_type = match service {
        Service::UploadPack => "application/x-git-upload-pack-advertisement",
        Service::ReceivePack => "application/x-git-receive-pack-advertisement",
    };
    Ok(SmartResponse::ok(content_type, body))
}

/// Serve a `POST /git-upload-pack` body: parse the `want`/`have`/`done`
/// negotiation and return `NAK` followed by the packfile. `body` must already
/// be inflated (see [`maybe_gunzip`]).
pub fn upload_pack_rpc(repo: &Repository, body: &[u8]) -> Result<SmartResponse> {
    let out = upload_pack_v0(repo, body)?;
    Ok(SmartResponse::ok(
        "application/x-git-upload-pack-result",
        out,
    ))
}

/// Serve a `POST /git-receive-pack` body: enforce protected-branch policy, then
/// quarantine the pack and apply the single ref update, returning the
/// report-status body. `body` must already be inflated (see [`maybe_gunzip`]).
pub fn receive_pack_rpc(
    repo: &Repository,
    body: &[u8],
    config: &SmartHttpConfig,
) -> Result<SmartResponse> {
    let packet_limits = PacketLineLimits::default();
    let receive_limits = ReceiveLimits::default();

    // Protected-branch enforcement mirrors the CLI receive-pack: if any command
    // targets a blocked branch, refuse the whole push with per-ref `ng` and
    // never touch objects or refs. A parse failure here falls through to
    // `execute_receive_pack`, which reports the framing error uniformly.
    if !config.blocked_branches.is_empty() {
        if let Ok(parsed) =
            parse_receive_pack_request(body, RECEIVE_PACK_CAPABILITIES, packet_limits)
        {
            let blocked = parsed.commands.iter().any(|command| {
                command
                    .name()
                    .strip_prefix("refs/heads/")
                    .map(|branch| config.blocked_branches.iter().any(|b| b == branch))
                    .unwrap_or(false)
            });
            if blocked {
                let report_status = parsed.capabilities.contains("report-status")
                    || parsed.capabilities.contains("report-status-v2");
                let out = if report_status {
                    let statuses: Vec<ReceiveStatus> = parsed
                        .commands
                        .iter()
                        .map(|command| ReceiveStatus {
                            ref_name: command.name().to_string(),
                            error: Some(
                                "protected branch: direct push blocked, open a pull request".into(),
                            ),
                        })
                        .collect();
                    encode_receive_status(None, &statuses)?
                } else {
                    Vec::new()
                };
                return Ok(SmartResponse::ok(
                    "application/x-git-receive-pack-result",
                    out,
                ));
            }
        }
    }

    let execution = execute_receive_pack(
        repo,
        body,
        RECEIVE_PACK_CAPABILITIES,
        packet_limits,
        receive_limits,
        ReceivePackPolicy {
            allow_non_fast_forward: config.allow_non_fast_forward,
        },
    )?;
    Ok(SmartResponse::ok(
        "application/x-git-receive-pack-result",
        execution.response,
    ))
}

/// Inflate a request body if it arrived `Content-Encoding: gzip`. Stock `git`
/// gzip-compresses the `git-upload-pack` negotiation body by default; the
/// `git-receive-pack` body (which already carries a compressed packfile) is
/// sent uncompressed. Returns the body unchanged when not gzip-encoded.
pub fn maybe_gunzip(body: Vec<u8>, gzip_encoded: bool) -> Result<Vec<u8>> {
    if !gzip_encoded {
        return Ok(body);
    }
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(&body[..]);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| ForgeError::Decompress(format!("gzip request body: {e}")))?;
    Ok(out)
}

/// Extract the `service` value from a raw query string (`service=git-upload-pack`).
fn service_from_query(query: Option<&str>) -> Option<Service> {
    let query = query?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "service")
        .and_then(|(_, value)| Service::from_query_value(value))
}

// ---------------------------------------------------------------------------
// Async layer — axum router + listener over the sync core.
// ---------------------------------------------------------------------------

/// Shared handler state: where the (bare) repository lives and the push policy.
#[derive(Clone)]
pub struct SmartHttpState {
    git_dir: Arc<PathBuf>,
    config: Arc<SmartHttpConfig>,
}

impl SmartHttpState {
    /// Serve the repository whose Git directory is `git_dir` under `config`.
    pub fn new(git_dir: impl Into<PathBuf>, config: SmartHttpConfig) -> Self {
        Self {
            git_dir: Arc::new(git_dir.into()),
            config: Arc::new(config),
        }
    }
}

/// Build the smart-HTTP [`Router`] for one repository.
///
/// Mount it at the URL root (a client clones `http://host/`) or nest it under a
/// path prefix for per-repo routing. The request-body limit follows
/// [`SmartHttpConfig::max_body_bytes`] so large pushes are accepted.
pub fn router(state: SmartHttpState) -> Router {
    let body_limit = state.config.max_body_bytes;
    Router::new()
        .route("/info/refs", get(get_info_refs))
        .route("/git-upload-pack", post(post_upload_pack))
        .route("/git-receive-pack", post(post_receive_pack))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

/// Bind `addr` and serve the smart-HTTP router until the process exits.
pub async fn serve(addr: SocketAddr, state: SmartHttpState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await
}

/// Blocking entry point for a synchronous CLI: build a multi-threaded Tokio
/// runtime and drive [`serve`] to completion. Lets a plain `fn main` host the
/// server without becoming async itself.
pub fn serve_blocking(addr: SocketAddr, state: SmartHttpState) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ForgeError::Io)?;
    runtime.block_on(serve(addr, state)).map_err(ForgeError::Io)
}

/// `GET /info/refs?service=git-(upload|receive)-pack`.
async fn get_info_refs(State(state): State<SmartHttpState>, RawQuery(query): RawQuery) -> Response {
    let Some(service) = service_from_query(query.as_deref()) else {
        // No/unknown service: this transport speaks smart-HTTP only.
        return error_response(
            StatusCode::FORBIDDEN,
            "service not enabled (smart HTTP only)",
        );
    };
    let git_dir = state.git_dir.clone();
    let produced =
        run_blocking(move || info_refs(&Repository::open_git_dir(&*git_dir)?, service)).await;
    into_response(produced)
}

/// `POST /git-upload-pack` — fetch/clone negotiation.
async fn post_upload_pack(
    State(state): State<SmartHttpState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let gzip = is_gzip(&headers);
    let git_dir = state.git_dir.clone();
    let raw = body.to_vec();
    let produced = run_blocking(move || {
        let body = maybe_gunzip(raw, gzip)?;
        upload_pack_rpc(&Repository::open_git_dir(&*git_dir)?, &body)
    })
    .await;
    into_response(produced)
}

/// `POST /git-receive-pack` — push.
async fn post_receive_pack(
    State(state): State<SmartHttpState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let gzip = is_gzip(&headers);
    let git_dir = state.git_dir.clone();
    let config = state.config.clone();
    let raw = body.to_vec();
    let produced = run_blocking(move || {
        let body = maybe_gunzip(raw, gzip)?;
        receive_pack_rpc(&Repository::open_git_dir(&*git_dir)?, &body, &config)
    })
    .await;
    into_response(produced)
}

/// Run a blocking, `Repository`-touching closure on the blocking pool.
async fn run_blocking<F>(f: F) -> Result<SmartResponse>
where
    F: FnOnce() -> Result<SmartResponse> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) => Err(ForgeError::ResourceLimit(format!(
            "smart-http worker task failed: {join}"
        ))),
    }
}

/// Was the request body gzip-encoded?
fn is_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').any(|token| token.trim() == "gzip"))
        .unwrap_or(false)
}

/// Map a sync-core result onto the framework response, hiding internal error
/// detail behind a generic `500` (server-side context stays in the error type
/// for the caller to log).
fn into_response(produced: Result<SmartResponse>) -> Response {
    match produced {
        Ok(smart) => Response::builder()
            .status(smart.status)
            .header(header::CONTENT_TYPE, smart.content_type)
            // Advertisements and RPC results must never be cached by proxies.
            .header(
                header::CACHE_CONTROL,
                "no-cache, max-age=0, must-revalidate",
            )
            .header(header::PRAGMA, "no-cache")
            .body(Body::from(smart.body))
            .unwrap_or_else(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
            }),
        Err(error) => {
            let status = match error {
                ForgeError::NotARepository(_) | ForgeError::ObjectNotFound(_) => {
                    StatusCode::NOT_FOUND
                }
                ForgeError::ResourceLimit(_) => StatusCode::PAYLOAD_TOO_LARGE,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error_response(status, "smart-http request failed")
        }
    }
}

/// A plain-text error response with the given status.
fn error_response(status: StatusCode, message: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(format!("{message}\n")))
        .expect("static error response is always valid")
}
