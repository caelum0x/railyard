//! Client-side transport: `clone`, `fetch`, and `push` against a rithub (or any
//! protocol-v0 smart-HTTP) server, or a local repository path, plus the
//! `[remote "<name>"]` configuration they rely on.
//!
//! ## Wire protocol
//! Protocol v0 over smart HTTP, exactly what `rit`'s own server
//! ([`crate::http::smart_http`]) and rithub speak:
//!
//! - **discover:** `GET <url>/info/refs?service=git-upload-pack|git-receive-pack`
//!   → `# service` banner, flush, ref advertisement (first ref carries caps).
//! - **fetch:** `POST <url>/git-upload-pack` with `want`/`have`/`done` pkt-lines
//!   → `NAK` + packfile. Only objects the server has are sent as `have`s.
//! - **push:** `POST <url>/git-receive-pack` with one `old new ref\0report-status`
//!   command + packfile → report-status (`unpack ok`, `ok <ref>` / `ng <ref> <why>`).
//!
//! Received packs go through the same quarantined, fsck'd
//! [`ReceiveTransaction`] the server uses before any ref moves.
//!
//! ## Transport
//! HTTP(S) requests are made with the system `curl` (TLS, proxies, and CA
//! bundles come from the platform). Credentials never appear on the command
//! line: they are passed to curl on stdin (`-K -`). Credentials come from the
//! URL userinfo for a single command, or `RIT_USERNAME` + `RIT_TOKEN` (a rithub
//! personal access token). Local paths (`/srv/repo.git`, `file:///srv/repo.git`)
//! are served in-process with no network at all.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::receive::{AtomicRefUpdate, ReceiveLimits, ReceiveTransaction};
use crate::upload_pack::{advertise_upload_pack, generate_pack, reachable_objects, upload_pack_v0};
use crate::wire::{decode_packet_lines, encode_packet_line, PacketLine, PacketLineLimits};
use crate::{
    advertise_receive_pack, execute_receive_pack, ObjectId, ReceivePackPolicy, Repository,
    RECEIVE_PACK_CAPABILITIES,
};

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

// ---------------------------------------------------------------------------
// Remote configuration (`<git_dir>/config`)
// ---------------------------------------------------------------------------

/// A configured remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

fn config_path(repo: &Repository) -> PathBuf {
    repo.git_dir().join("config")
}

/// Every `[remote "<name>"]` section with its `url`.
pub fn list_remotes(repo: &Repository) -> Result<Vec<Remote>> {
    let text = std::fs::read_to_string(config_path(repo)).unwrap_or_default();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current = trimmed
                .strip_prefix("[remote \"")
                .and_then(|rest| rest.strip_suffix("\"]"))
                .map(str::to_string);
            continue;
        }
        if let (Some(name), Some((key, value))) = (&current, trimmed.split_once('=')) {
            if key.trim() == "url" {
                out.insert(name.clone(), value.trim().to_string());
            }
        }
    }
    Ok(out.into_iter().map(|(name, url)| Remote { name, url }).collect())
}

/// Look up one remote by name.
pub fn get_remote(repo: &Repository, name: &str) -> Result<Remote> {
    list_remotes(repo)?
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow!("no such remote '{name}' (add one with `rit remote add {name} <url>`)"))
}

fn validate_remote_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || name.starts_with('.')
    {
        bail!("invalid remote name '{name}'");
    }
    Ok(())
}

/// Add a remote with the standard `+refs/heads/*:refs/remotes/<name>/*` refspec.
/// Any password embedded in the URL is stripped before it is stored.
pub fn add_remote(repo: &Repository, name: &str, url: &str) -> Result<Remote> {
    validate_remote_name(name)?;
    if list_remotes(repo)?.iter().any(|r| r.name == name) {
        bail!("remote '{name}' already exists");
    }
    let stored = strip_password(url);
    if stored.contains('\n') || stored.contains('"') {
        bail!("invalid remote URL");
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_path(repo))
        .context("opening repository config")?;
    write!(
        file,
        "[remote \"{name}\"]\n\turl = {stored}\n\tfetch = +refs/heads/*:refs/remotes/{name}/*\n"
    )?;
    Ok(Remote { name: name.to_string(), url: stored })
}

/// Remove a remote's config section (its remote-tracking refs are left as-is).
pub fn remove_remote(repo: &Repository, name: &str) -> Result<()> {
    let path = config_path(repo);
    let text = std::fs::read_to_string(&path).context("reading repository config")?;
    let header = format!("[remote \"{name}\"]");
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    let mut found = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skipping = trimmed == header;
            found |= skipping;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !found {
        bail!("no such remote '{name}'");
    }
    std::fs::write(&path, out).context("writing repository config")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Where a remote lives.
enum Endpoint {
    Local(PathBuf),
    Http { base: String, auth: Option<(String, String)> },
}

fn strip_password(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) if scheme == "http" || scheme == "https" => {
            let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
            match authority.rsplit_once('@') {
                Some((userinfo, host)) => {
                    let user = userinfo.split(':').next().unwrap_or_default();
                    format!("{scheme}://{user}@{host}/{path}")
                }
                None => url.to_string(),
            }
        }
        _ => url.to_string(),
    }
}

fn resolve_endpoint(url: &str) -> Result<Endpoint> {
    if let Some(rest) = url.strip_prefix("file://") {
        return Ok(Endpoint::Local(PathBuf::from(rest)));
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Ok(Endpoint::Local(PathBuf::from(url)));
    };
    if scheme != "http" && scheme != "https" {
        bail!("unsupported remote URL scheme '{scheme}' (use http, https, or a local path)");
    }
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let mut auth = userinfo.and_then(|u| {
        u.split_once(':')
            .map(|(user, pass)| (percent_decode(user), percent_decode(pass)))
    });
    if auth.is_none() {
        if let Ok(token) = std::env::var("RIT_TOKEN") {
            let user = userinfo
                .map(percent_decode)
                .or_else(|| std::env::var("RIT_USERNAME").ok())
                .unwrap_or_else(|| "token".to_string());
            auth = Some((user, token));
        }
    }
    let path = path.trim_end_matches('/');
    let path = if path.ends_with(".git") { path.to_string() } else { format!("{path}.git") };
    Ok(Endpoint::Http { base: format!("{scheme}://{host}/{path}"), auth })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("rit-{tag}-{}-{nanos}", std::process::id()))
}

/// Escape a value for a curl config file string.
fn curl_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Perform one HTTP request through the system `curl`; returns (status, body).
fn http_request(
    url: &str,
    auth: Option<&(String, String)>,
    body: Option<(&str, &[u8])>,
    accept: &str,
) -> Result<(u16, Vec<u8>)> {
    let out_path = temp_path("out");
    let in_path = temp_path("in");
    let mut config = format!("url = {}\n", curl_quote(url));
    if let Some((user, pass)) = auth {
        config.push_str(&format!("user = {}\n", curl_quote(&format!("{user}:{pass}"))));
    }
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--proto", "=http,https", "--max-redirs", "0"])
        .args(["-H", &format!("Accept: {accept}")])
        .args(["-H", "Git-Protocol: version=0"])
        .args(["-A", "rit/0.1"])
        .arg("-o")
        .arg(&out_path)
        .args(["-w", "%{http_code}", "-K", "-"]);
    if let Some((content_type, data)) = body {
        std::fs::write(&in_path, data).context("staging request body")?;
        cmd.args(["-X", "POST", "-H", &format!("Content-Type: {content_type}")])
            .arg("--data-binary")
            .arg(format!("@{}", in_path.display()));
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running curl (install curl to use http/https remotes)")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("curl stdin unavailable"))?
        .write_all(config.as_bytes())?;
    let output = child.wait_with_output().context("waiting for curl")?;
    let _ = std::fs::remove_file(&in_path);
    let response = std::fs::read(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    if !output.status.success() {
        bail!(
            "request to {} failed: {}",
            strip_password(url),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let status: u16 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
    Ok((status, response))
}

fn check_status(status: u16, body: &[u8], what: &str) -> Result<()> {
    match status {
        200 => Ok(()),
        401 => bail!("{what}: authentication required (set RIT_USERNAME and RIT_TOKEN to a rithub access token)"),
        403 => bail!("{what}: permission denied"),
        404 => bail!("{what}: repository not found (or you lack access)"),
        other => bail!(
            "{what}: server returned HTTP {other}: {}",
            String::from_utf8_lossy(&body[..body.len().min(200)]).trim()
        ),
    }
}

/// The parsed ref advertisement.
#[derive(Debug, Default)]
pub struct Advertisement {
    pub refs: Vec<(String, ObjectId)>,
    pub capabilities: Vec<String>,
    /// The branch HEAD points at (`symref=HEAD:refs/heads/<x>`), if advertised.
    pub head_symref: Option<String>,
}

fn parse_advertisement(bytes: &[u8]) -> Result<Advertisement> {
    let lines = decode_packet_lines(bytes, PacketLineLimits::default())
        .map_err(|e| anyhow!("malformed ref advertisement: {e}"))?;
    let mut adv = Advertisement::default();
    for line in lines {
        let PacketLine::Data(data) = line else { continue };
        let text = String::from_utf8_lossy(&data);
        let text = text.trim_end_matches('\n');
        if text.starts_with("# service=") {
            continue;
        }
        let (refpart, caps) = match text.split_once('\0') {
            Some((r, c)) => (r, Some(c)),
            None => (text, None),
        };
        if let Some(caps) = caps {
            adv.capabilities = caps.split(' ').map(str::to_string).collect();
            adv.head_symref = adv
                .capabilities
                .iter()
                .find_map(|c| c.strip_prefix("symref=HEAD:").map(str::to_string));
        }
        let Some((hex, name)) = refpart.split_once(' ') else { continue };
        if hex == ZERO_OID || name.ends_with("^{}") {
            continue;
        }
        adv.refs.push((name.to_string(), ObjectId::from_hex(hex)?));
    }
    Ok(adv)
}

fn pkt(line: &str) -> Result<Vec<u8>> {
    encode_packet_line(&PacketLine::Data(line.as_bytes().to_vec())).map_err(|e| anyhow!("{e}"))
}

/// Advertise refs for `service` (`git-upload-pack` or `git-receive-pack`).
fn discover(endpoint: &Endpoint, service: &str) -> Result<Advertisement> {
    match endpoint {
        Endpoint::Local(path) => {
            let remote = open_local(path)?;
            let bytes = if service == "git-upload-pack" {
                advertise_upload_pack(&remote)?
            } else {
                advertise_receive_pack(&remote, RECEIVE_PACK_CAPABILITIES)?
            };
            parse_advertisement(&bytes)
        }
        Endpoint::Http { base, auth } => {
            let url = format!("{base}/info/refs?service={service}");
            let (status, body) = http_request(&url, auth.as_ref(), None, "*/*")?;
            check_status(status, &body, "listing remote refs")?;
            parse_advertisement(&body)
        }
    }
}

fn open_local(path: &Path) -> Result<Repository> {
    let git_dir = if path.join("HEAD").exists() { path.to_path_buf() } else { path.join(".git") };
    Repository::open_git_dir(&git_dir)
        .with_context(|| format!("opening local remote {}", path.display()))
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// One ref moved by a fetch.
#[derive(Debug, Clone)]
pub struct FetchedRef {
    pub local: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
}

/// Summary of a fetch.
#[derive(Debug, Default)]
pub struct FetchReport {
    pub updated: Vec<FetchedRef>,
    pub objects: usize,
    pub head_symref: Option<String>,
}

/// Fetch from `url` and update local refs by the mapping `remote ref -> local
/// ref` (`map` returns `None` to skip a ref). Tracking updates are forced.
fn fetch_into(
    repo: &Repository,
    url: &str,
    map: &dyn Fn(&str) -> Option<String>,
) -> Result<FetchReport> {
    let endpoint = resolve_endpoint(url)?;
    let adv = discover(&endpoint, "git-upload-pack")?;
    let mut report = FetchReport { head_symref: adv.head_symref.clone(), ..FetchReport::default() };

    let targets: Vec<(String, ObjectId)> = adv
        .refs
        .iter()
        .filter(|(name, _)| name != "HEAD")
        .filter_map(|(name, oid)| map(name).map(|local| (local, *oid)))
        .collect();
    if targets.is_empty() {
        return Ok(report);
    }

    let odb = repo.odb();
    let mut wants: Vec<ObjectId> = targets.iter().map(|(_, oid)| *oid).filter(|oid| !odb.contains(oid)).collect();
    wants.sort();
    wants.dedup();
    // Haves: remote tips we already hold — the server is guaranteed to know them.
    let haves: Vec<ObjectId> = adv.refs.iter().map(|(_, oid)| *oid).filter(|oid| odb.contains(oid)).collect();

    let mut tx = ReceiveTransaction::begin(repo, ReceiveLimits::default())?;
    if !wants.is_empty() {
        let mut body = Vec::new();
        for want in &wants {
            body.extend(pkt(&format!("want {want}\n"))?);
        }
        body.extend_from_slice(b"0000");
        for have in &haves {
            body.extend(pkt(&format!("have {have}\n"))?);
        }
        body.extend(pkt("done\n")?);

        let response = match &endpoint {
            Endpoint::Local(path) => upload_pack_v0(&open_local(path)?, &body)?,
            Endpoint::Http { base, auth } => {
                let (status, resp) = http_request(
                    &format!("{base}/git-upload-pack"),
                    auth.as_ref(),
                    Some(("application/x-git-upload-pack-request", &body)),
                    "application/x-git-upload-pack-result",
                )?;
                check_status(status, &resp, "fetching objects")?;
                resp
            }
        };
        let pack = strip_nak(&response)?;
        report.objects = tx.stage_pack(pack)?;
    }

    let mut updates = Vec::new();
    for (local, new) in &targets {
        let old = repo.refs().resolve(local).ok();
        if old == Some(*new) {
            continue;
        }
        updates.push(AtomicRefUpdate {
            name: local.clone(),
            expected_old: old,
            new: Some(*new),
            allow_non_fast_forward: true,
        });
        report.updated.push(FetchedRef { local: local.clone(), old, new: *new });
    }
    if updates.is_empty() {
        return Ok(report);
    }
    let outcome = tx.finish_atomic(updates)?;
    if !outcome.committed {
        let reasons: Vec<String> = outcome
            .results
            .iter()
            .filter_map(|r| r.error.as_ref().map(|e| format!("{}: {e}", r.name)))
            .collect();
        bail!("fetch could not update refs: {}", reasons.join("; "));
    }
    Ok(report)
}

/// Skip the leading `NAK`/`ACK` pkt-lines and return the raw packfile bytes.
fn strip_nak(response: &[u8]) -> Result<&[u8]> {
    let mut offset = 0;
    loop {
        let rest = &response[offset..];
        if rest.starts_with(b"PACK") {
            return Ok(rest);
        }
        if rest.len() < 4 {
            bail!("server response did not contain a packfile");
        }
        let len = std::str::from_utf8(&rest[..4])
            .ok()
            .and_then(|h| usize::from_str_radix(h, 16).ok())
            .ok_or_else(|| anyhow!("malformed upload-pack response"))?;
        if len == 0 {
            offset += 4;
            continue;
        }
        if len < 4 || len > rest.len() {
            bail!("malformed upload-pack response");
        }
        let payload = String::from_utf8_lossy(&rest[4..len]);
        if payload.starts_with("ERR ") {
            bail!("remote error: {}", payload.trim_start_matches("ERR ").trim());
        }
        offset += len;
    }
}

/// `rit fetch <remote>`: update `refs/remotes/<remote>/*` from the remote's heads.
pub fn fetch(repo: &Repository, remote: &str) -> Result<FetchReport> {
    let remote_cfg = get_remote(repo, remote)?;
    let name = remote_cfg.name.clone();
    fetch_into(repo, &remote_cfg.url, &move |r: &str| {
        r.strip_prefix("refs/heads/").map(|b| format!("refs/remotes/{name}/{b}"))
    })
}

/// `rit clone <url> <dir>`: create the repository, fetch every branch and tag,
/// configure `origin`, and (non-bare) check out the remote's default branch.
pub fn clone(url: &str, directory: &Path, bare: bool) -> Result<(Repository, FetchReport)> {
    if directory.exists() && std::fs::read_dir(directory)?.next().is_some() {
        bail!("destination path '{}' already exists and is not empty", directory.display());
    }
    let endpoint = resolve_endpoint(url)?;
    let adv = discover(&endpoint, "git-upload-pack")?;
    let default_branch = adv
        .head_symref
        .as_deref()
        .and_then(|r| r.strip_prefix("refs/heads/"))
        .map(str::to_string)
        .or_else(|| {
            adv.refs
                .iter()
                .find_map(|(n, _)| n.strip_prefix("refs/heads/").map(str::to_string))
        })
        .unwrap_or_else(|| "main".to_string());

    std::fs::create_dir_all(directory)?;
    let repo = Repository::init(directory, bare, Some(&default_branch))?;
    add_remote(&repo, "origin", url)?;
    let report = fetch_into(&repo, url, &|r: &str| {
        if let Some(b) = r.strip_prefix("refs/heads/") {
            Some(if bare { format!("refs/heads/{b}") } else { format!("refs/remotes/origin/{b}") })
        } else if r.starts_with("refs/tags/") {
            Some(r.to_string())
        } else {
            None
        }
    })?;

    if !bare {
        let tracking = format!("refs/remotes/origin/{default_branch}");
        if let Ok(tip) = repo.refs().resolve(&tracking) {
            repo.refs().compare_and_swap(&format!("refs/heads/{default_branch}"), None, tip)?;
            let tree = repo.commit(&tip)?.tree;
            repo.checkout_tree(tree)?;
        }
    }
    Ok((repo, report))
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// Outcome of pushing one ref.
#[derive(Debug)]
pub struct PushReport {
    pub remote_ref: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    pub objects: usize,
    pub up_to_date: bool,
}

/// `rit push <remote> <src>[:<dst>]` (or `:<dst>` to delete a remote branch).
pub fn push(repo: &Repository, remote: &str, refspec: &str, force: bool) -> Result<PushReport> {
    let remote_cfg = get_remote(repo, remote)?;
    let endpoint = resolve_endpoint(&remote_cfg.url)?;
    let (src, dst) = match refspec.split_once(':') {
        Some((s, d)) => (s.to_string(), d.to_string()),
        None => (refspec.to_string(), refspec.to_string()),
    };
    let qualify = |name: &str| {
        if name.starts_with("refs/") { name.to_string() } else { format!("refs/heads/{name}") }
    };
    let dst = qualify(&dst);
    let new = if src.is_empty() {
        None
    } else {
        Some(repo.resolve(&src).with_context(|| format!("resolving '{src}'"))?)
    };

    let adv = discover(&endpoint, "git-receive-pack")?;
    let old = adv.refs.iter().find(|(n, _)| *n == dst).map(|(_, o)| *o);
    if old == new {
        return Ok(PushReport { remote_ref: dst, old, new, objects: 0, up_to_date: true });
    }
    if let (Some(old), Some(new)) = (old, new) {
        if !repo.odb().contains(&old) {
            bail!("the remote {dst} has commits you do not have; run `rit fetch {remote}` and merge first");
        }
        if !force
            && !crate::fsck::is_ancestor(repo.odb(), old, new, crate::fsck::ConnectivityLimits::default())?
        {
            bail!("rejected: {dst} is not a fast-forward (fetch and merge, or pass --force)");
        }
    }

    let mut body = Vec::new();
    let command = format!(
        "{} {} {dst}\0report-status\n",
        old.map(|o| o.to_string()).unwrap_or_else(|| ZERO_OID.to_string()),
        new.map(|o| o.to_string()).unwrap_or_else(|| ZERO_OID.to_string()),
    );
    body.extend(pkt(&command)?);
    body.extend_from_slice(b"0000");
    let mut objects = 0;
    if let Some(new) = new {
        // Objects the server already has: every advertised tip we also hold.
        let known: Vec<ObjectId> = adv
            .refs
            .iter()
            .map(|(_, o)| *o)
            .filter(|o| repo.odb().contains(o))
            .collect();
        let send = reachable_objects(repo, &[new], &known)?;
        objects = send.len();
        body.extend(generate_pack(repo, &send)?);
    }

    let response = match &endpoint {
        Endpoint::Local(path) => {
            let remote_repo = open_local(path)?;
            execute_receive_pack(
                &remote_repo,
                &body,
                RECEIVE_PACK_CAPABILITIES,
                PacketLineLimits::default(),
                ReceiveLimits::default(),
                ReceivePackPolicy { allow_non_fast_forward: force },
            )?
            .response
        }
        Endpoint::Http { base, auth } => {
            let (status, resp) = http_request(
                &format!("{base}/git-receive-pack"),
                auth.as_ref(),
                Some(("application/x-git-receive-pack-request", &body)),
                "application/x-git-receive-pack-result",
            )?;
            check_status(status, &resp, "pushing")?;
            resp
        }
    };
    check_report_status(&response, &dst)?;
    // Keep the remote-tracking ref in sync with what we just pushed.
    if let Some(branch) = dst.strip_prefix("refs/heads/") {
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let current = repo.refs().resolve(&tracking).ok();
        match new {
            Some(new) => {
                let _ = repo.refs().compare_and_swap(&tracking, current, new);
            }
            None => {
                if let Some(cur) = current {
                    let _ = repo.refs().compare_and_delete(&tracking, cur);
                }
            }
        }
    }
    Ok(PushReport { remote_ref: dst, old, new, objects, up_to_date: false })
}

fn check_report_status(response: &[u8], dst: &str) -> Result<()> {
    if response.is_empty() {
        bail!("server sent no push status");
    }
    let lines = decode_packet_lines(response, PacketLineLimits::default())
        .map_err(|e| anyhow!("malformed push status: {e}"))?;
    let mut unpack_ok = false;
    let mut ref_ok = false;
    for line in lines {
        let PacketLine::Data(data) = line else { continue };
        let text = String::from_utf8_lossy(&data).trim_end().to_string();
        if let Some(rest) = text.strip_prefix("unpack ") {
            if rest != "ok" {
                bail!("remote failed to unpack objects: {rest}");
            }
            unpack_ok = true;
        } else if let Some(rest) = text.strip_prefix("ng ") {
            let (name, reason) = rest.split_once(' ').unwrap_or((rest, "rejected"));
            bail!("! [remote rejected] {name} ({reason})");
        } else if let Some(name) = text.strip_prefix("ok ") {
            ref_ok |= name == dst;
        }
    }
    if !unpack_ok || !ref_ok {
        bail!("push was not acknowledged for {dst}");
    }
    Ok(())
}
