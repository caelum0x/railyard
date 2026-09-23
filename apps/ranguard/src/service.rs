//! The Ranguard HTTP service core (transport-agnostic).
//!
//! [`Service::handle`] maps one request (method, path, bearer token, body) to a
//! JSON [`Reply`]. The `ranguard-server` binary owns the socket/HTTP framing;
//! keeping the logic here makes it testable without a network.
//!
//! What the service offers a game backend:
//!
//! * **Signed release manifests.** `POST /v1/manifests` signs a baseline of
//!   file fingerprints (path, size, SHA-256) that the backend computed from a
//!   trusted build, and registers it under an id. The signed text is
//!   self-verifying (HMAC under `RANGUARD_KEY`), so it can be shipped with the
//!   client too.
//! * **Integrity scans.** `POST /v1/manifests/scan` diffs fingerprints a client
//!   reports against a registered (or supplied) manifest and returns
//!   modified / added / removed findings. `POST /v1/manifests/verify` checks a
//!   manifest's own signature.
//! * **Heartbeat sessions.** `POST /v1/sessions` opens an anti-replay
//!   challenge/response session and returns its per-session key (derived from
//!   `RANGUARD_KEY`); `/challenge` issues a CSPRNG nonce, `/heartbeat` verifies
//!   the rolling HMAC. Sessions expire after inactivity.
//!
//! Every `/v1` route requires `Authorization: Bearer <RANGUARD_API_TOKEN>` —
//! the service is called by the game backend, never directly by players.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::heartbeat::{Heartbeat, Outcome, Server};
use crate::hmac::{constant_time_eq, hmac_sha256};
use crate::json::{self, b, n, obj, s, Value};
use crate::manifest::{Entry, Manifest};
use crate::report::ChangeKind;
use crate::rng;
use crate::sha256::to_hex;

/// Largest request body accepted (a manifest of ~20k files fits comfortably).
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Most entries a single manifest or scan may carry.
pub const MAX_ENTRIES: usize = 100_000;
/// Default inactivity timeout for heartbeat sessions.
pub const DEFAULT_SESSION_TTL_SECS: u64 = 3600;
/// Default cap on concurrently open heartbeat sessions.
pub const DEFAULT_MAX_SESSIONS: usize = 100_000;

/// Runtime configuration (from the environment in the binary).
pub struct Config {
    /// Manifest signing + session-key derivation key (`RANGUARD_KEY`).
    pub signing_key: Vec<u8>,
    /// Bearer token the game backend presents (`RANGUARD_API_TOKEN`).
    pub api_token: String,
    /// Directory registered manifests persist to (`RANGUARD_DATA_DIR`).
    pub data_dir: Option<PathBuf>,
    pub session_ttl_secs: u64,
    pub max_sessions: usize,
}

/// A JSON reply: HTTP status plus body.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    fn ok(data: Value) -> Reply {
        Reply {
            status: 200,
            body: obj([("success", b(true)), ("data", data)]),
        }
    }

    fn created(data: Value) -> Reply {
        Reply {
            status: 201,
            ..Reply::ok(data)
        }
    }

    pub fn error(status: u16, code: &str, message: &str) -> Reply {
        Reply {
            status,
            body: obj([
                ("success", b(false)),
                (
                    "error",
                    obj([("code", s(code)), ("message", s(message))]),
                ),
            ]),
        }
    }
}

struct Session {
    server: Server,
    label: Option<String>,
    created_at: u64,
    last_seen: u64,
    accepted: u64,
    rejected: u64,
    last_outcome: Option<&'static str>,
}

struct Registered {
    manifest: Manifest,
    label: Option<String>,
}

pub struct Service {
    config: Config,
    api_token_hash: [u8; 32],
    sessions: Mutex<HashMap<String, Session>>,
    manifests: Mutex<HashMap<String, Registered>>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_id(prefix: &str) -> io::Result<String> {
    Ok(format!("{prefix}_{}", to_hex(&rng::random_bytes(16)?)))
}

fn is_valid_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn change_kind(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Modified => "modified",
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
    }
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Accepted => "accepted",
        Outcome::RejectedStaleCounter => "rejected_stale_counter",
        Outcome::RejectedBadNonce => "rejected_bad_nonce",
        Outcome::RejectedBadMac => "rejected_bad_mac",
    }
}

fn bad_request(message: &str) -> Reply {
    Reply::error(400, "bad_request", message)
}

fn internal(context: &str, error: impl std::fmt::Display) -> Reply {
    eprintln!("ranguard-server: {context}: {error}");
    Reply::error(500, "internal_error", context)
}

/// Optional short label (<= 200 chars, no control characters).
fn label_of(body: &Value) -> Result<Option<String>, Reply> {
    match body.get("label") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(l)) if l.chars().count() <= 200 && !l.chars().any(char::is_control) => {
            Ok(Some(l.trim().to_string()).filter(|l| !l.is_empty()))
        }
        Some(_) => Err(bad_request(
            "label must be a string of at most 200 characters without control characters",
        )),
    }
}

/// Parse `[{path, size, sha256}]` into manifest entries.
fn entries_of(value: Option<&Value>, field: &str) -> Result<Vec<Entry>, Reply> {
    let items = value
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request(&format!("{field} must be an array of {{path, size, sha256}}")))?;
    if items.len() > MAX_ENTRIES {
        return Err(bad_request(&format!("{field} may contain at most {MAX_ENTRIES} entries")));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let path = item.get("path").and_then(Value::as_str);
            let size = item.get("size").and_then(Value::as_u64);
            let sha = item.get("sha256").and_then(Value::as_str);
            match (path, size, sha) {
                (Some(path), Some(size), Some(sha)) => {
                    let entry = Entry {
                        rel_path: path.to_string(),
                        size,
                        sha256_hex: sha.to_ascii_lowercase(),
                    };
                    crate::manifest::validate_entry(&entry)
                        .map_err(|e| bad_request(&format!("{field}[{i}]: {e}")))?;
                    Ok(entry)
                }
                _ => Err(bad_request(&format!(
                    "{field}[{i}] needs path (string), size (integer) and sha256 (hex string)"
                ))),
            }
        })
        .collect()
}

fn manifest_view(id: Option<&str>, m: &Manifest, label: Option<&str>) -> Value {
    obj([
        ("manifest_id", id.map_or(Value::Null, s)),
        ("label", label.map_or(Value::Null, s)),
        ("root", s(m.root.as_str())),
        ("created_at", n(m.created_at)),
        ("entries", n(m.entries.len())),
        ("mac", s(m.mac_hex.as_str())),
        ("manifest", s(m.serialize())),
    ])
}

impl Service {
    /// Build the service and load any manifests persisted in `data_dir`.
    pub fn new(config: Config) -> io::Result<Service> {
        if config.signing_key.len() < 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RANGUARD_KEY must be at least 16 bytes",
            ));
        }
        if config.api_token.len() < 24 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RANGUARD_API_TOKEN must be at least 24 characters",
            ));
        }
        // Fail at boot, not at first challenge, if OS randomness is missing.
        rng::fill_random(&mut [0u8; 1])?;
        let api_token_hash = hmac_sha256(b"ranguard-api-token-v1", config.api_token.as_bytes());
        let service = Service {
            api_token_hash,
            sessions: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            config,
        };
        service.load_persisted()?;
        Ok(service)
    }

    fn load_persisted(&self) -> io::Result<()> {
        let Some(dir) = &self.config.data_dir else {
            return Ok(());
        };
        fs::create_dir_all(dir)?;
        let mut loaded = HashMap::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".manifest") else {
                continue;
            };
            if !is_valid_id(id, "mf") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            // Optional label header written by `persist`.
            let (label, body) = match text.split_once('\n') {
                Some((first, rest)) if first.starts_with("# label\t") => {
                    (Some(first["# label\t".len()..].to_string()), rest.to_string())
                }
                _ => (None, text),
            };
            let manifest = Manifest::deserialize(&body)?;
            if !manifest.verify_signature(&self.config.signing_key) {
                eprintln!(
                    "ranguard-server: skipping {}: signature does not verify under RANGUARD_KEY",
                    path.display()
                );
                continue;
            }
            loaded.insert(id.to_string(), Registered { manifest, label });
        }
        let count = loaded.len();
        self.manifests
            .lock()
            .map_err(|_| io::Error::other("manifest registry lock poisoned"))?
            .extend(loaded);
        if count > 0 {
            eprintln!("ranguard-server: loaded {count} persisted manifest(s)");
        }
        Ok(())
    }

    fn persist(&self, id: &str, reg: &Registered) -> io::Result<()> {
        let Some(dir) = &self.config.data_dir else {
            return Ok(());
        };
        let mut text = String::new();
        if let Some(label) = &reg.label {
            text.push_str("# label\t");
            text.push_str(label);
            text.push('\n');
        }
        text.push_str(&reg.manifest.serialize());
        let final_path = dir.join(format!("{id}.manifest"));
        let tmp = dir.join(format!(".{id}.manifest.tmp"));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &final_path)
    }

    fn authorized(&self, bearer: Option<&str>) -> bool {
        bearer.is_some_and(|token| {
            let presented = hmac_sha256(b"ranguard-api-token-v1", token.trim().as_bytes());
            constant_time_eq(&presented, &self.api_token_hash)
        })
    }

    fn session_key(&self, session_id: &str) -> [u8; 32] {
        hmac_sha256(
            &self.config.signing_key,
            format!("ranguard-session-v1\0{session_id}").as_bytes(),
        )
    }

    /// Route one request. `bearer` is the token from `Authorization: Bearer`.
    pub fn handle(&self, method: &str, path: &str, bearer: Option<&str>, body: &[u8]) -> Reply {
        let path = path.split('?').next().unwrap_or(path);
        if path == "/healthz" {
            return match method {
                "GET" | "HEAD" => Reply::ok(obj([("status", s("ok"))])),
                _ => Reply::error(405, "method_not_allowed", "use GET"),
            };
        }
        if !path.starts_with("/v1/") {
            return Reply::error(404, "not_found", "no such route");
        }
        if !self.authorized(bearer) {
            return Reply::error(401, "unauthorized", "missing or invalid API token");
        }
        if body.len() > MAX_BODY_BYTES {
            return Reply::error(413, "payload_too_large", "request body too large");
        }
        let parsed = if body.iter().all(u8::is_ascii_whitespace) {
            Value::Object(Default::default())
        } else {
            let Ok(text) = std::str::from_utf8(body) else {
                return bad_request("body must be UTF-8 JSON");
            };
            match json::parse(text) {
                Ok(v @ Value::Object(_)) => v,
                Ok(_) => return bad_request("body must be a JSON object"),
                Err(e) => return bad_request(&format!("invalid JSON: {e}")),
            }
        };

        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        match (method, segments.as_slice()) {
            ("GET", ["v1", "stats"]) => self.stats(),
            ("POST", ["v1", "manifests"]) => self.register_manifest(&parsed),
            ("GET", ["v1", "manifests"]) => self.list_manifests(),
            ("POST", ["v1", "manifests", "verify"]) => self.verify_manifest(&parsed),
            ("POST", ["v1", "manifests", "scan"]) => self.scan(&parsed),
            ("GET", ["v1", "manifests", id]) => self.get_manifest(id),
            ("DELETE", ["v1", "manifests", id]) => self.delete_manifest(id),
            ("POST", ["v1", "sessions"]) => self.open_session(&parsed),
            ("GET", ["v1", "sessions", id]) => self.session_status(id),
            ("DELETE", ["v1", "sessions", id]) => self.close_session(id),
            ("POST", ["v1", "sessions", id, "challenge"]) => self.challenge(id),
            ("POST", ["v1", "sessions", id, "heartbeat"]) => self.heartbeat(id, &parsed),
            (
                _,
                ["v1", "stats"]
                | ["v1", "manifests"]
                | ["v1", "manifests", _]
                | ["v1", "sessions"]
                | ["v1", "sessions", _]
                | ["v1", "sessions", _, "challenge" | "heartbeat"],
            ) => Reply::error(405, "method_not_allowed", "method not allowed on this route"),
            _ => Reply::error(404, "not_found", "no such route"),
        }
    }

    // ---- manifests ---------------------------------------------------------

    fn register_manifest(&self, body: &Value) -> Reply {
        let root = match body.get("root") {
            None | Some(Value::Null) => "release".to_string(),
            Some(Value::String(r)) if !r.trim().is_empty() && r.len() <= 512 => r.trim().to_string(),
            Some(_) => return bad_request("root must be a non-empty string of at most 512 bytes"),
        };
        let label = match label_of(body) {
            Ok(l) => l,
            Err(reply) => return reply,
        };
        let entries = match entries_of(body.get("entries"), "entries") {
            Ok(e) => e,
            Err(reply) => return reply,
        };
        let manifest = match Manifest::from_entries(&root, entries, &self.config.signing_key) {
            Ok(m) => m,
            Err(e) => return bad_request(&e.to_string()),
        };
        let id = match random_id("mf") {
            Ok(id) => id,
            Err(e) => return internal("could not generate manifest id", e),
        };
        let registered = Registered { manifest, label };
        if let Err(e) = self.persist(&id, &registered) {
            return internal("could not persist manifest", e);
        }
        let view = manifest_view(Some(&id), &registered.manifest, registered.label.as_deref());
        match self.manifests.lock() {
            Ok(mut map) => {
                map.insert(id, registered);
                Reply::created(view)
            }
            Err(_) => internal("manifest registry unavailable", "lock poisoned"),
        }
    }

    fn list_manifests(&self) -> Reply {
        let Ok(map) = self.manifests.lock() else {
            return internal("manifest registry unavailable", "lock poisoned");
        };
        let mut items: Vec<(&String, &Registered)> = map.iter().collect();
        items.sort_by(|a, b| b.1.manifest.created_at.cmp(&a.1.manifest.created_at));
        let list = items
            .into_iter()
            .map(|(id, reg)| {
                obj([
                    ("manifest_id", s(id.as_str())),
                    ("label", reg.label.as_deref().map_or(Value::Null, s)),
                    ("root", s(reg.manifest.root.as_str())),
                    ("created_at", n(reg.manifest.created_at)),
                    ("entries", n(reg.manifest.entries.len())),
                ])
            })
            .collect();
        Reply::ok(obj([("manifests", Value::Array(list))]))
    }

    fn get_manifest(&self, id: &str) -> Reply {
        let Ok(map) = self.manifests.lock() else {
            return internal("manifest registry unavailable", "lock poisoned");
        };
        match map.get(id) {
            Some(reg) => Reply::ok(manifest_view(Some(id), &reg.manifest, reg.label.as_deref())),
            None => Reply::error(404, "manifest_not_found", "no such manifest"),
        }
    }

    fn delete_manifest(&self, id: &str) -> Reply {
        if !is_valid_id(id, "mf") {
            return Reply::error(404, "manifest_not_found", "no such manifest");
        }
        let removed = match self.manifests.lock() {
            Ok(mut map) => map.remove(id).is_some(),
            Err(_) => return internal("manifest registry unavailable", "lock poisoned"),
        };
        if !removed {
            return Reply::error(404, "manifest_not_found", "no such manifest");
        }
        if let Some(dir) = &self.config.data_dir {
            let path = dir.join(format!("{id}.manifest"));
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != io::ErrorKind::NotFound {
                    return internal("could not delete persisted manifest", e);
                }
            }
        }
        Reply::ok(obj([("manifest_id", s(id)), ("deleted", b(true))]))
    }

    fn parse_manifest_text(&self, body: &Value) -> Result<Manifest, Reply> {
        let text = body
            .get("manifest")
            .and_then(Value::as_str)
            .ok_or_else(|| bad_request("manifest (signed manifest text) is required"))?;
        Manifest::deserialize(text).map_err(|e| bad_request(&format!("malformed manifest: {e}")))
    }

    fn verify_manifest(&self, body: &Value) -> Reply {
        let manifest = match self.parse_manifest_text(body) {
            Ok(m) => m,
            Err(reply) => return reply,
        };
        let valid = manifest.verify_signature(&self.config.signing_key);
        Reply::ok(obj([
            ("valid", b(valid)),
            ("root", s(manifest.root.as_str())),
            ("created_at", n(manifest.created_at)),
            ("entries", n(manifest.entries.len())),
        ]))
    }

    fn scan(&self, body: &Value) -> Reply {
        let files = match entries_of(body.get("files"), "files") {
            Ok(f) => f,
            Err(reply) => return reply,
        };
        let manifest = match body.get("manifest_id").and_then(Value::as_str) {
            Some(id) => {
                let Ok(map) = self.manifests.lock() else {
                    return internal("manifest registry unavailable", "lock poisoned");
                };
                match map.get(id) {
                    Some(reg) => reg.manifest.clone(),
                    None => return Reply::error(404, "manifest_not_found", "no such manifest"),
                }
            }
            None => match self.parse_manifest_text(body) {
                Ok(m) => m,
                Err(_) => return bad_request("provide manifest_id or manifest"),
            },
        };
        if !manifest.verify_signature(&self.config.signing_key) {
            return Reply::ok(obj([
                ("clean", b(false)),
                ("manifest_valid", b(false)),
                ("files_checked", n(0)),
                ("changes", Value::Array(Vec::new())),
            ]));
        }
        let report = manifest.diff_entries(&files);
        let changes = report
            .changes
            .iter()
            .map(|c| {
                obj([
                    ("kind", s(change_kind(&c.kind))),
                    ("path", s(c.path.as_str())),
                    ("detail", s(c.detail.as_str())),
                ])
            })
            .collect();
        Reply::ok(obj([
            ("clean", b(report.is_clean())),
            ("manifest_valid", b(true)),
            ("files_checked", n(report.files_checked)),
            ("changes", Value::Array(changes)),
        ]))
    }

    // ---- heartbeat sessions ------------------------------------------------

    /// Drop sessions idle longer than the TTL.
    fn sweep(&self, sessions: &mut HashMap<String, Session>, now: u64) {
        let ttl = self.config.session_ttl_secs;
        sessions.retain(|_, sess| now.saturating_sub(sess.last_seen) <= ttl);
    }

    fn open_session(&self, body: &Value) -> Reply {
        let label = match label_of(body) {
            Ok(l) => l,
            Err(reply) => return reply,
        };
        let id = match random_id("hb") {
            Ok(id) => id,
            Err(e) => return internal("could not generate session id", e),
        };
        let key = self.session_key(&id);
        let server = match Server::new_secure(&key) {
            Ok(server) => server,
            Err(e) => return internal("OS randomness unavailable", e),
        };
        let now = now_unix();
        let Ok(mut sessions) = self.sessions.lock() else {
            return internal("session store unavailable", "lock poisoned");
        };
        self.sweep(&mut sessions, now);
        if sessions.len() >= self.config.max_sessions {
            return Reply::error(
                503,
                "session_capacity",
                "too many open heartbeat sessions; close idle sessions or raise RANGUARD_MAX_SESSIONS",
            );
        }
        sessions.insert(
            id.clone(),
            Session {
                server,
                label: label.clone(),
                created_at: now,
                last_seen: now,
                accepted: 0,
                rejected: 0,
                last_outcome: None,
            },
        );
        Reply::created(obj([
            ("session_id", s(id.as_str())),
            ("session_key", s(to_hex(&key))),
            ("label", label.map_or(Value::Null, s)),
            ("created_at", n(now)),
            ("expires_after_idle_secs", n(self.config.session_ttl_secs)),
            (
                "tag_algorithm",
                s("HMAC-SHA256(session_key, counter_u64_be || nonce_16 || prev_accepted_tag_32)"),
            ),
        ]))
    }

    fn with_session(&self, id: &str, f: impl FnOnce(&mut Session) -> Reply) -> Reply {
        let now = now_unix();
        let Ok(mut sessions) = self.sessions.lock() else {
            return internal("session store unavailable", "lock poisoned");
        };
        self.sweep(&mut sessions, now);
        match sessions.get_mut(id) {
            Some(sess) => {
                sess.last_seen = now;
                f(sess)
            }
            None => Reply::error(404, "session_not_found", "no such session (it may have expired)"),
        }
    }

    fn session_status(&self, id: &str) -> Reply {
        let ttl = self.config.session_ttl_secs;
        self.with_session(id, |sess| {
            Reply::ok(obj([
                ("session_id", s(id)),
                ("label", sess.label.as_deref().map_or(Value::Null, s)),
                ("created_at", n(sess.created_at)),
                ("last_seen", n(sess.last_seen)),
                ("expires_after_idle_secs", n(ttl)),
                ("challenges_issued", n(sess.server.issued())),
                ("last_accepted_counter", n(sess.server.last_accepted())),
                ("accepted", n(sess.accepted)),
                ("rejected", n(sess.rejected)),
                ("challenge_pending", b(sess.server.pending().is_some())),
                ("last_outcome", sess.last_outcome.map_or(Value::Null, s)),
            ]))
        })
    }

    fn close_session(&self, id: &str) -> Reply {
        let Ok(mut sessions) = self.sessions.lock() else {
            return internal("session store unavailable", "lock poisoned");
        };
        match sessions.remove(id) {
            Some(sess) => Reply::ok(obj([
                ("session_id", s(id)),
                ("closed", b(true)),
                ("accepted", n(sess.accepted)),
                ("rejected", n(sess.rejected)),
            ])),
            None => Reply::error(404, "session_not_found", "no such session"),
        }
    }

    fn challenge(&self, id: &str) -> Reply {
        self.with_session(id, |sess| match sess.server.try_issue_challenge() {
            Ok(ch) => Reply::ok(obj([
                ("counter", n(ch.counter)),
                ("nonce", s(to_hex(&ch.nonce))),
            ])),
            Err(e) => internal("OS randomness unavailable", e),
        })
    }

    fn heartbeat(&self, id: &str, body: &Value) -> Reply {
        let counter = body.get("counter").and_then(Value::as_u64);
        let nonce = body.get("nonce").and_then(Value::as_str).and_then(parse_hex::<16>);
        let tag = body.get("tag").and_then(Value::as_str).and_then(parse_hex::<32>);
        let (Some(counter), Some(nonce), Some(tag)) = (counter, nonce, tag) else {
            return bad_request(
                "counter (integer), nonce (32 hex chars) and tag (64 hex chars) are required",
            );
        };
        self.with_session(id, |sess| {
            let outcome = sess.server.verify(&Heartbeat {
                counter,
                nonce,
                tag,
            });
            let accepted = outcome == Outcome::Accepted;
            if accepted {
                sess.accepted += 1;
            } else {
                sess.rejected += 1;
            }
            let name = outcome_name(&outcome);
            sess.last_outcome = Some(name);
            Reply::ok(obj([
                ("accepted", b(accepted)),
                ("outcome", s(name)),
                ("counter", n(counter)),
            ]))
        })
    }

    fn stats(&self) -> Reply {
        let now = now_unix();
        let sessions = match self.sessions.lock() {
            Ok(mut sessions) => {
                self.sweep(&mut sessions, now);
                sessions.len()
            }
            Err(_) => return internal("session store unavailable", "lock poisoned"),
        };
        let manifests = match self.manifests.lock() {
            Ok(map) => map.len(),
            Err(_) => return internal("manifest registry unavailable", "lock poisoned"),
        };
        Reply::ok(obj([
            ("open_sessions", n(sessions)),
            ("max_sessions", n(self.config.max_sessions)),
            ("registered_manifests", n(manifests)),
            ("persistent", b(self.config.data_dir.is_some())),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heartbeat::{Challenge, Client};

    const TOKEN: &str = "test-api-token-0123456789abcdef";

    fn service() -> Service {
        Service::new(Config {
            signing_key: b"test-signing-key-0123456789".to_vec(),
            api_token: TOKEN.into(),
            data_dir: None,
            session_ttl_secs: 60,
            max_sessions: 10,
        })
        .unwrap()
    }

    fn call(svc: &Service, method: &str, path: &str, body: &str) -> Reply {
        svc.handle(method, path, Some(TOKEN), body.as_bytes())
    }

    fn data(reply: &Reply) -> &Value {
        reply.body.get("data").expect("data")
    }

    #[test]
    fn rejects_missing_token() {
        let svc = service();
        assert_eq!(svc.handle("GET", "/v1/stats", None, b"").status, 401);
        assert_eq!(svc.handle("GET", "/healthz", None, b"").status, 200);
    }

    #[test]
    fn register_and_scan_manifest() {
        let svc = service();
        let hash = "a".repeat(64);
        let other = "b".repeat(64);
        let reg = call(
            &svc,
            "POST",
            "/v1/manifests",
            &format!(r#"{{"root":"v1.2","entries":[{{"path":"data/a.pak","size":10,"sha256":"{hash}"}}]}}"#),
        );
        assert_eq!(reg.status, 201);
        let id = data(&reg).get("manifest_id").unwrap().as_str().unwrap().to_string();

        let clean = call(
            &svc,
            "POST",
            "/v1/manifests/scan",
            &format!(r#"{{"manifest_id":"{id}","files":[{{"path":"data/a.pak","size":10,"sha256":"{hash}"}}]}}"#),
        );
        assert_eq!(data(&clean).get("clean"), Some(&Value::Bool(true)));

        let dirty = call(
            &svc,
            "POST",
            "/v1/manifests/scan",
            &format!(r#"{{"manifest_id":"{id}","files":[{{"path":"data/a.pak","size":10,"sha256":"{other}"}},{{"path":"x.dll","size":1,"sha256":"{hash}"}}]}}"#),
        );
        let changes = data(&dirty).get("changes").unwrap().as_array().unwrap();
        assert_eq!(changes.len(), 2);

        let text = data(&reg).get("manifest").unwrap().as_str().unwrap().to_string();
        let body = obj([("manifest", s(text.replace("v1.2", "v9.9")))]).to_json();
        let verify = call(&svc, "POST", "/v1/manifests/verify", &body);
        assert_eq!(data(&verify).get("valid"), Some(&Value::Bool(false)));
    }

    #[test]
    fn heartbeat_session_accepts_and_rejects_replay() {
        let svc = service();
        let open = call(&svc, "POST", "/v1/sessions", r#"{"label":"match-42"}"#);
        assert_eq!(open.status, 201);
        let id = data(&open).get("session_id").unwrap().as_str().unwrap().to_string();
        let key = parse_hex::<32>(data(&open).get("session_key").unwrap().as_str().unwrap()).unwrap();
        let mut client = Client::new(&key);

        let ch = call(&svc, "POST", &format!("/v1/sessions/{id}/challenge"), "");
        let challenge = Challenge {
            counter: data(&ch).get("counter").unwrap().as_u64().unwrap(),
            nonce: parse_hex::<16>(data(&ch).get("nonce").unwrap().as_str().unwrap()).unwrap(),
        };
        let hb = client.respond(&challenge);
        let body = obj([
            ("counter", n(hb.counter)),
            ("nonce", s(to_hex(&hb.nonce))),
            ("tag", s(to_hex(&hb.tag))),
        ])
        .to_json();
        let ok = call(&svc, "POST", &format!("/v1/sessions/{id}/heartbeat"), &body);
        assert_eq!(data(&ok).get("accepted"), Some(&Value::Bool(true)));
        client.commit(&hb);

        let replay = call(&svc, "POST", &format!("/v1/sessions/{id}/heartbeat"), &body);
        assert_eq!(
            data(&replay).get("outcome").and_then(Value::as_str),
            Some("rejected_stale_counter")
        );
        let status = call(&svc, "GET", &format!("/v1/sessions/{id}"), "");
        assert_eq!(data(&status).get("accepted").and_then(Value::as_u64), Some(1));
        assert_eq!(data(&status).get("rejected").and_then(Value::as_u64), Some(1));
    }
}
