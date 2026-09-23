//! SSH git transport (russh). Authenticates by **public key** — the offered
//! key is matched against either:
//!
//! 1. **User SSH keys** (`ssh_keys` table): identifies the user; full RBAC
//!    applies as on the HTTP transport.
//! 2. **Deploy keys** (`deploy_keys` table): a repository-scoped credential;
//!    the key's `read_only` flag gates push access.
//!
//! The SSH host key is **persistent**: loaded from
//! `$RITHUB_DATA_ROOT/ssh_host_key` on startup, or generated (ed25519) and
//! saved there on first boot. This means clients can pin the server's host
//! fingerprint in `known_hosts` and it won't change across restarts.
//!
//! Each git command is served by piping the SSH channel to a `rit` subprocess
//! (`rit upload-pack` / `rit receive-pack`), reusing rit's tested stateful
//! stdin/stdout protocol. Protection is applied by passing the repo's protected
//! branches to `rit receive-pack --block-branch <b>` for non-admin pushers, so
//! a direct push to a protected branch is rejected exactly as over HTTP.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use russh::keys::key::{KeyPair, PublicKey};
use russh::keys::PublicKeyBase64;
use russh::server::{Auth, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, CryptoVec};
use rithub_core::{AccessStore, DeployKeyStore, RepoRole, ReviewStore, SshKeyStore, Store, User,
                   Visibility};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// SSH identity (resolved during auth_publickey)
// ---------------------------------------------------------------------------

/// Who authenticated over SSH.
enum SshIdentity {
    /// A regular user identified by their registered SSH key.
    User(User),
    /// A deploy key scoped to a specific repository.
    DeployKey { repo_id: Uuid, read_only: bool },
}

// ---------------------------------------------------------------------------
// Persistent host key
// ---------------------------------------------------------------------------

/// Load the ed25519 host key from `path`, or generate one and persist it there.
///
/// The file is written in PKCS8 PEM format (`-----BEGIN PRIVATE KEY-----`),
/// which `decode_secret_key` can read back and is compatible with OpenSSH
/// `ssh-keygen -t ed25519` output (when converted to PKCS8).
pub fn load_or_generate_host_key(path: &Path) -> anyhow::Result<KeyPair> {
    if path.exists() {
        let pem = std::fs::read_to_string(path)
            .with_context(|| format!("reading SSH host key {}", path.display()))?;
        russh::keys::decode_secret_key(&pem, None)
            .with_context(|| format!("decoding SSH host key {}", path.display()))
    } else {
        let key = KeyPair::generate_ed25519()
            .ok_or_else(|| anyhow!("failed to generate ed25519 SSH host key"))?;
        // encode_pkcs8_pem(key, writer) writes a PEM-encoded PKCS8 private key.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory for {}", path.display()))?;
        }
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&key, &mut buf)
            .context("encoding SSH host key to PKCS8 PEM")?;
        std::fs::write(path, &buf)
            .with_context(|| format!("writing SSH host key {}", path.display()))?;
        // Restrict to owner-read-only on Unix (0600).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("setting permissions on {}", path.display()))?;
        }
        tracing::info!(path = %path.display(), "generated new persistent SSH host key");
        Ok(key)
    }
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

/// Start the SSH server, listening on `bind` (e.g. `127.0.0.1:2222`).
/// `host_key_path` is the path to persist / load the ed25519 host key from.
pub async fn serve(
    state: AppState,
    bind: String,
    rit_bin: String,
    host_key_path: &Path,
) -> anyhow::Result<()> {
    let host_key = load_or_generate_host_key(host_key_path)?;
    let config = Arc::new(Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(3600)),
        auth_rejection_time: std::time::Duration::from_secs(1),
        keys: vec![host_key],
        ..Default::default()
    });
    let mut server = SshServer {
        state,
        rit_bin: Arc::new(rit_bin),
    };
    tracing::info!(%bind, "rithub SSH transport listening");
    server.run_on_address(config, bind).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// russh Server + Handler
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SshServer {
    state: AppState,
    rit_bin: Arc<String>,
}

impl russh::server::Server for SshServer {
    type Handler = SshHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> SshHandler {
        SshHandler {
            state: self.state.clone(),
            rit_bin: self.rit_bin.clone(),
            identity: None,
            channels: HashMap::new(),
        }
    }
}

struct SshHandler {
    state: AppState,
    rit_bin: Arc<String>,
    identity: Option<SshIdentity>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

#[async_trait]
impl Handler for SshHandler {
    type Error = anyhow::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // russh has already verified the client possesses the matching private
        // key before this callback is invoked.
        let blob = public_key.public_key_base64();

        // 1. Check user SSH keys.
        if let Ok(Some(user)) = self.state.store.find_user_by_ssh_key(&blob).await {
            self.identity = Some(SshIdentity::User(user));
            return Ok(Auth::Accept);
        }

        // 2. Check deploy keys (repo-scoped).
        if let Ok(Some(dk)) = self.state.store.find_deploy_key_by_blob(&blob).await {
            self.identity = Some(SshIdentity::DeployKey {
                repo_id: dk.repo_id,
                read_only: dk.read_only,
            });
            return Ok(Auth::Accept);
        }

        Ok(Auth::Reject {
            proceed_with_methods: None,
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).to_string();
        let handle = session.handle();
        let Some(channel) = self.channels.remove(&channel_id) else {
            return Ok(());
        };

        match self.prepare(&command).await {
            Ok(plan) => {
                spawn_pipe(handle, channel_id, channel, self.rit_bin.clone(), plan);
            }
            Err(message) => {
                let _ = handle
                    .data(channel_id, CryptoVec::from(format!("rithub: {message}\n")))
                    .await;
                let _ = handle.exit_status_request(channel_id, 1).await;
                let _ = handle.eof(channel_id).await;
                let _ = handle.close(channel_id).await;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Command preparation
// ---------------------------------------------------------------------------

/// A validated, authorized git command ready to serve.
struct Plan {
    /// `"git-upload-pack"` or `"git-receive-pack"`.
    service: &'static str,
    git_dir: String,
    /// Protected branches to block for a non-admin pusher (receive only).
    block_branches: Vec<String>,
}

impl SshHandler {
    /// Parse + authorize an exec command. Returns a [`Plan`] or a client-safe
    /// error message.
    async fn prepare(&self, command: &str) -> Result<Plan, String> {
        let identity = self.identity.as_ref().ok_or("not authenticated")?;
        let (service, raw_path) = parse_git_command(command).ok_or("unsupported command")?;
        let (org, repo) = split_repo_path(raw_path).ok_or("bad repository path")?;

        let meta = self
            .state
            .store
            .get_repo(&org, &repo)
            .await
            .map_err(|_| "repository not found".to_string())?;

        match identity {
            SshIdentity::User(user) => {
                let role = self
                    .state
                    .store
                    .effective_role(user.id, meta.org_id, meta.id)
                    .await
                    .ok()
                    .flatten();

                match service {
                    "git-upload-pack" => {
                        let ok = meta.visibility == Visibility::Public
                            || matches!(role, Some(r) if r >= RepoRole::Read);
                        if !ok {
                            return Err("access denied".into());
                        }
                        Ok(Plan {
                            service,
                            git_dir: meta.disk_path,
                            block_branches: Vec::new(),
                        })
                    }
                    "git-receive-pack" => {
                        if !matches!(role, Some(r) if r >= RepoRole::Write) {
                            return Err("write access denied".into());
                        }
                        let block_branches =
                            if matches!(role, Some(r) if r >= RepoRole::Admin) {
                                Vec::new()
                            } else {
                                self.state
                                    .store
                                    .list_push_protected_branches(meta.id)
                                    .await
                                    .unwrap_or_default()
                            };
                        Ok(Plan {
                            service,
                            git_dir: meta.disk_path,
                            block_branches,
                        })
                    }
                    _ => Err("unsupported service".into()),
                }
            }

            SshIdentity::DeployKey { repo_id, read_only } => {
                // A deploy key is scoped to exactly one repository.
                if *repo_id != meta.id {
                    return Err("access denied: key not registered for this repository".into());
                }
                match service {
                    "git-upload-pack" => Ok(Plan {
                        service,
                        git_dir: meta.disk_path,
                        block_branches: Vec::new(),
                    }),
                    "git-receive-pack" => {
                        if *read_only {
                            return Err("write access denied: deploy key is read-only".into());
                        }
                        // Deploy keys never bypass protected branches.
                        let block_branches = self
                            .state
                            .store
                            .list_push_protected_branches(meta.id)
                            .await
                            .unwrap_or_default();
                        Ok(Plan {
                            service,
                            git_dir: meta.disk_path,
                            block_branches,
                        })
                    }
                    _ => Err("unsupported service".into()),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Channel piping
// ---------------------------------------------------------------------------

/// Pipe the SSH channel to a `rit` subprocess serving the git command.
fn spawn_pipe(
    handle: russh::server::Handle,
    channel_id: ChannelId,
    channel: Channel<Msg>,
    rit_bin: Arc<String>,
    plan: Plan,
) {
    use std::process::Stdio;
    use tokio::process::Command;

    tokio::spawn(async move {
        let mut command = Command::new(&*rit_bin);
        if plan.service == "git-upload-pack" {
            command.arg("upload-pack");
        } else {
            command.arg("receive-pack");
            for branch in &plan.block_branches {
                command.arg("--block-branch").arg(branch);
            }
        }
        command
            .arg(&plan.git_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = handle.exit_status_request(channel_id, 1).await;
                let _ = handle.close(channel_id).await;
                return;
            }
        };
        let mut child_stdin = child.stdin.take().expect("piped stdin");
        let mut child_stdout = child.stdout.take().expect("piped stdout");

        let stream = channel.into_stream();
        let (mut client_read, mut client_write) = tokio::io::split(stream);

        // Client → git stdin.
        let to_child = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_read, &mut child_stdin).await;
            let _ = child_stdin.shutdown().await;
        });
        // git stdout → client.
        let _ = tokio::io::copy(&mut child_stdout, &mut client_write).await;
        let _ = client_write.shutdown().await;
        let _ = to_child.await;

        let code = child
            .wait()
            .await
            .ok()
            .and_then(|s| s.code())
            .unwrap_or(0) as u32;
        let _ = handle.exit_status_request(channel_id, code).await;
        let _ = handle.eof(channel_id).await;
        let _ = handle.close(channel_id).await;
    });
}

// ---------------------------------------------------------------------------
// Path / command parsing
// ---------------------------------------------------------------------------

/// Parse `git-upload-pack '<path>'` / `git-receive-pack '<path>'`.
fn parse_git_command(command: &str) -> Option<(&'static str, &str)> {
    let command = command.trim();
    for service in ["git-upload-pack", "git-receive-pack"] {
        if let Some(rest) = command.strip_prefix(service) {
            let path = rest.trim().trim_matches(|c| c == '\'' || c == '"' || c == ' ');
            let service = if service == "git-upload-pack" {
                "git-upload-pack"
            } else {
                "git-receive-pack"
            };
            return Some((service, path));
        }
    }
    None
}

/// Split `/acme/widgets.git` (or `acme/widgets.git`) into `(org, repo)`.
fn split_repo_path(path: &str) -> Option<(String, String)> {
    let path = path.trim_start_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.splitn(2, '/');
    let org = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if org.is_empty()
        || repo.is_empty()
        || org.contains("..")
        || repo.contains("..")
        || repo.contains('/')
    {
        return None;
    }
    Some((org, repo))
}
