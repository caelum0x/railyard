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
//!
//! SSH remotes (`ssh://[user@]host[:port]/org/repo.git` or scp-style
//! `git@host:org/repo.git`) run the system `ssh` client with
//! `git-upload-pack '<path>'` / `git-receive-pack '<path>'`, exactly what the
//! rithub SSH transport serves; keys come from the user's ssh agent/config.
//! Set `RIT_SSH` to use a different ssh program.

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
    /// `dest` is `[user@]host`; `path` is the repository path on the server.
    Ssh { dest: String, port: Option<u16>, path: String },
}

/// Parse an SSH remote: `ssh://[user@]host[:port]/path` or scp-style
/// `[user@]host:path`. Returns `None` when `url` is not an SSH remote.
fn parse_ssh(url: &str) -> Result<Option<Endpoint>> {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("ssh://").or_else(|| url.strip_prefix("git+ssh://")) {
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("ssh remote URL has no repository path"))?;
        let (dest, port) = match authority.rsplit_once(':') {
            Some((d, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                (d.to_string(), Some(p.parse::<u16>().context("invalid ssh port")?))
            }
            _ => (authority.to_string(), None),
        };
        if dest.is_empty() || dest.starts_with('-') {
            bail!("invalid ssh host in remote URL");
        }
        return Ok(Some(Endpoint::Ssh { dest, port, path: normalize_repo_path(path)? }));
    }
    if url.contains("://") {
        return Ok(None);
    }
    // scp-style: `host:path` where the host part has no '/' and is not a
    // local path that happens to contain a colon.
    if let Some((host, path)) = url.split_once(':') {
        if !host.is_empty() && !host.contains('/') && !path.is_empty() && !Path::new(url).exists() {
            if host.starts_with('-') {
                bail!("invalid ssh host in remote URL");
            }
            return Ok(Some(Endpoint::Ssh {
                dest: host.to_string(),
                port: None,
                path: normalize_repo_path(path)?,
            }));
        }
    }
    Ok(None)
}

/// Normalize a server-side repository path to `org/repo.git`, rejecting
/// anything that could escape into shell syntax on the remote.
fn normalize_repo_path(path: &str) -> Result<String> {
    let path = path.trim_start_matches('/').trim_end_matches('/');
    if path.is_empty()
        || path.contains("..")
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        bail!("invalid repository path in ssh remote URL");
    }
    Ok(if path.ends_with(".git") { path.to_string() } else { format!("{path}.git") })
}

/// Read one pkt-line (raw, including its length header) from `reader`.
/// Returns `None` at EOF. A flush packet is returned as `b"0000"`.
fn read_pkt_raw(reader: &mut impl std::io::Read) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = std::str::from_utf8(&header)
        .ok()
        .and_then(|h| usize::from_str_radix(h, 16).ok())
        .ok_or_else(|| anyhow!("malformed pkt-line from ssh remote"))?;
    let mut out = header.to_vec();
    if len >= 4 {
        let mut payload = vec![0u8; len - 4];
        reader.read_exact(&mut payload)?;
        out.extend(payload);
    }
    Ok(Some(out))
}

/// Run one stateful git service session over ssh: read the ref advertisement
/// (up to the first flush), then optionally send `request` and read the full
/// response until the server closes the stream. Returns `(advertisement,
/// response)`.
fn ssh_session(
    dest: &str,
    port: Option<u16>,
    path: &str,
    service: &str,
    request: Option<&[u8]>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    use std::io::Read;

    let program = std::env::var("RIT_SSH").unwrap_or_else(|_| "ssh".to_string());
    let mut cmd = Command::new(&program);
    cmd.arg("-T");
    if let Some(port) = port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg("--")
        .arg(dest)
        .arg(format!("{service} '{path}'"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running {program} (install OpenSSH to use ssh remotes)"))?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("ssh stdout unavailable"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("ssh stdin unavailable"))?;

    let mut advertisement = Vec::new();
    loop {
        match read_pkt_raw(&mut stdout)? {
            None => break,
            Some(pkt) => {
                let flush = pkt == b"0000";
                advertisement.extend(pkt);
                if flush {
                    break;
                }
            }
        }
    }

    let mut response = Vec::new();
    match request {
        Some(body) => {
            stdin.write_all(body).context("sending request over ssh")?;
            drop(stdin);
            stdout.read_to_end(&mut response).context("reading ssh response")?;
        }
        None => {
            // Nothing wanted: end the session politely with a flush.
            let _ = stdin.write_all(b"0000");
            drop(stdin);
            let _ = stdout.read_to_end(&mut response);
        }
    }
    let status = child.wait().context("waiting for ssh")?;
    if advertisement.is_empty() && !status.success() {
        bail!("ssh remote {dest}:{path} failed ({status}); check your ssh key and repository access");
    }
    Ok((advertisement, response))
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
    if let Some(ssh) = parse_ssh(url)? {
        return Ok(ssh);
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Ok(Endpoint::Local(PathBuf::from(url)));
    };
    if scheme != "http" && scheme != "https" {
        bail!("unsupported remote URL scheme '{scheme}' (use http, https, ssh, or a local path)");
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
        Endpoint::Ssh { dest, port, path } => {
            let (adv, _) = ssh_session(dest, *port, path, service, None)?;
            parse_advertisement(&adv)
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
            Endpoint::Ssh { dest, port, path } => {
                ssh_session(dest, *port, path, "git-upload-pack", Some(&body))?.1
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

/// `rit push <remote> <src>[:<dst>]` (or `:<dst>` to delete a remote ref).
///
/// Unqualified names are qualified the way git does it:
/// - a destination for an existing local TAG (and no same-named branch) goes to
///   `refs/tags/<name>`, never to a branch of the same name;
/// - a deletion (`:<name>`) targets whichever of `refs/heads/<name>` /
///   `refs/tags/<name>` the remote advertises, and fails when neither exists;
/// - everything else goes to `refs/heads/<name>`.
pub fn push(repo: &Repository, remote: &str, refspec: &str, force: bool) -> Result<PushReport> {
    let remote_cfg = get_remote(repo, remote)?;
    let endpoint = resolve_endpoint(&remote_cfg.url)?;
    let (src, dst) = match refspec.split_once(':') {
        Some((s, d)) => (s.to_string(), d.to_string()),
        None => (refspec.to_string(), refspec.to_string()),
    };
    let (src, force) = match src.strip_prefix('+') {
        Some(stripped) => (stripped.to_string(), true),
        None => (src, force),
    };
    if dst.is_empty() {
        bail!("invalid refspec '{refspec}': missing destination");
    }

    let adv = discover(&endpoint, "git-receive-pack")?;
    let advertised = |name: &str| adv.refs.iter().any(|(n, _)| n == name);

    let (dst, new) = if src.is_empty() {
        // Deletion: the ref must exist on the remote.
        let dst = if dst.starts_with("refs/") {
            dst
        } else if advertised(&format!("refs/heads/{dst}")) {
            format!("refs/heads/{dst}")
        } else if advertised(&format!("refs/tags/{dst}")) {
            format!("refs/tags/{dst}")
        } else {
            format!("refs/heads/{dst}")
        };
        if !advertised(&dst) {
            bail!("unable to delete '{dst}': remote ref does not exist");
        }
        (dst, None)
    } else {
        let local_tag = if src.starts_with("refs/tags/") {
            Some(src.clone())
        } else if !src.starts_with("refs/")
            && repo.refs().resolve(&format!("refs/tags/{src}")).is_ok()
            && repo.refs().resolve(&format!("refs/heads/{src}")).is_err()
        {
            Some(format!("refs/tags/{src}"))
        } else {
            None
        };
        // A tag is pushed as its own (possibly annotated) object, unpeeled.
        let new = match &local_tag {
            Some(tag_ref) => repo
                .refs()
                .resolve(tag_ref)
                .with_context(|| format!("resolving tag '{tag_ref}'"))?,
            None => repo.resolve(&src).with_context(|| format!("resolving '{src}'"))?,
        };
        let dst = if dst.starts_with("refs/") {
            dst
        } else if local_tag.is_some() {
            format!("refs/tags/{dst}")
        } else {
            format!("refs/heads/{dst}")
        };
        (dst, Some(new))
    };

    let old = adv.refs.iter().find(|(n, _)| *n == dst).map(|(_, o)| *o);
    if old == new {
        return Ok(PushReport { remote_ref: dst, old, new, objects: 0, up_to_date: true });
    }
    if let (Some(_), Some(_)) = (old, new) {
        if dst.starts_with("refs/tags/") && !force {
            bail!("rejected: tag {dst} already exists on the remote (pass --force to move it)");
        }
    }
    if let (Some(old), Some(new)) = (old, new) {
        if !dst.starts_with("refs/tags/") {
            if !repo.odb().contains(&old) {
                bail!("the remote {dst} has commits you do not have; run `rit fetch {remote}` and merge first");
            }
            if !force
                && !crate::fsck::is_ancestor(repo.odb(), old, new, crate::fsck::ConnectivityLimits::default())?
            {
                bail!("rejected: {dst} is not a fast-forward (fetch and merge, or pass --force)");
            }
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
        Endpoint::Ssh { dest, port, path } => {
            ssh_session(dest, *port, path, "git-receive-pack", Some(&body))?.1
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

/// `rit push --tags`: push every local tag the remote does not already have at
/// the same object. Existing remote tags pointing elsewhere are rejected
/// unless `force` (tags are not expected to move).
pub fn push_tags(repo: &Repository, remote: &str, force: bool) -> Result<Vec<PushReport>> {
    let tags: Vec<String> = repo
        .refs()
        .list()?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("refs/tags/"))
        .collect();
    let mut reports = Vec::with_capacity(tags.len());
    for tag in tags {
        reports.push(push(repo, remote, &format!("{tag}:{tag}"), force)?);
    }
    Ok(reports)
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

// ---------------------------------------------------------------------------
// Forge (rithub) JSON API client — used by `rit issue` / `rit pr`
// ---------------------------------------------------------------------------

/// A rithub repository addressed through its JSON API, derived from a remote.
#[derive(Debug, Clone)]
pub struct ForgeRepo {
    /// API origin, e.g. `https://rithub.example.com` (no trailing slash).
    pub api_base: String,
    pub org: String,
    pub repo: String,
    auth: Option<(String, String)>,
}

/// Split `.../org/repo(.git)` into `(prefix, org, repo)`.
fn split_org_repo(path: &str) -> Result<(String, String, String)> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        bail!("remote URL does not name an org/repo (expected .../<org>/<repo>.git)");
    }
    let repo = parts.pop().unwrap_or_default().to_string();
    let org = parts.pop().unwrap_or_default().to_string();
    Ok((parts.join("/"), org, repo))
}

/// Resolve the forge API target for a remote URL. HTTP(S) remotes use the same
/// origin; SSH remotes default to `https://<host>`. `RIT_API_URL` overrides the
/// API origin in both cases. Credentials: URL userinfo, else `RIT_USERNAME` +
/// `RIT_TOKEN`.
pub fn forge_repo(url: &str) -> Result<ForgeRepo> {
    let override_base = std::env::var("RIT_API_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty());
    match resolve_endpoint(url)? {
        Endpoint::Local(_) => bail!(
            "remote '{url}' is a local path; issue and pr commands need a rithub http(s) or ssh remote"
        ),
        Endpoint::Http { base, auth } => {
            let (scheme, rest) = base.split_once("://").unwrap_or(("https", base.as_str()));
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let (prefix, org, repo) = split_org_repo(path)?;
            let origin = if prefix.is_empty() {
                format!("{scheme}://{host}")
            } else {
                format!("{scheme}://{host}/{prefix}")
            };
            Ok(ForgeRepo { api_base: override_base.unwrap_or(origin), org, repo, auth })
        }
        Endpoint::Ssh { dest, path, .. } => {
            let host = dest.rsplit_once('@').map_or(dest.as_str(), |(_, h)| h).to_string();
            let (_, org, repo) = split_org_repo(&path)?;
            let auth = std::env::var("RIT_TOKEN").ok().map(|token| {
                (std::env::var("RIT_USERNAME").unwrap_or_else(|_| "token".to_string()), token)
            });
            Ok(ForgeRepo {
                api_base: override_base.unwrap_or_else(|| format!("https://{host}")),
                org,
                repo,
                auth,
            })
        }
    }
}

impl ForgeRepo {
    /// `org/repo`.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    /// Browser URL for a path under this repository (e.g. `issues/3`).
    pub fn web_url(&self, sub: &str) -> String {
        format!("{}/{}/{}/{sub}", self.api_base, self.org, self.repo)
    }

    /// Call `METHOD /api/orgs/{org}/repos/{repo}{sub}` with an optional JSON
    /// body; returns the parsed JSON response (`null` for an empty body).
    pub fn api(
        &self,
        method: &str,
        sub: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/api/orgs/{}/repos/{}{sub}", self.api_base, self.org, self.repo);
        let payload = body.map(serde_json::to_vec).transpose()?;
        let (status, response) = api_request(method, &url, self.auth.as_ref(), payload.as_deref())?;
        let parsed: serde_json::Value = if response.iter().all(|b| b.is_ascii_whitespace()) {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&response).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&response).into_owned())
            })
        };
        if (200..300).contains(&status) {
            return Ok(parsed);
        }
        let message = parsed
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        match status {
            401 => bail!("{message} (set RIT_USERNAME and RIT_TOKEN to a rithub access token)"),
            _ => bail!("{} {}: {message}", method, self.slug()),
        }
    }
}

/// One JSON API request through the system `curl`; returns (status, body).
fn api_request(
    method: &str,
    url: &str,
    auth: Option<&(String, String)>,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>)> {
    let out_path = temp_path("api-out");
    let in_path = temp_path("api-in");
    let mut config = format!("url = {}\n", curl_quote(url));
    if let Some((user, pass)) = auth {
        config.push_str(&format!("user = {}\n", curl_quote(&format!("{user}:{pass}"))));
    }
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--proto", "=http,https", "--max-redirs", "0"])
        .args(["-X", method])
        .args(["-H", "Accept: application/json"])
        .args(["-A", "rit/0.1"])
        .arg("-o")
        .arg(&out_path)
        .args(["-w", "%{http_code}", "-K", "-"]);
    if let Some(data) = body {
        std::fs::write(&in_path, data).context("staging request body")?;
        cmd.args(["-H", "Content-Type: application/json"])
            .arg("--data-binary")
            .arg(format!("@{}", in_path.display()));
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running curl (install curl to use rithub issue/pr commands)")?;
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
        bail!("request to {url} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let status: u16 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
    Ok((status, response))
}
