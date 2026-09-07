//! SSH git transport (russh). Authenticates by **public key** (the offered key
//! is matched against the registered `ssh_keys`), then serves `git-upload-pack`
//! (clone/fetch) and `git-receive-pack` (push) over the SSH channel against the
//! same repository host — with the **same RBAC + protected-branch enforcement**
//! as the HTTP transport.
//!
//! Each git command is served by piping the SSH channel to a `rit` subprocess
//! (`rit upload-pack` / `rit receive-pack`), reusing rit's tested stateful
//! stdin/stdout protocol. Protection is applied by passing the repo's protected
//! branches to `rit receive-pack --block-branch <b>` for non-admin pushers, so a
//! direct push to a protected branch is rejected exactly as over HTTP.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use russh::keys::key::{KeyPair, PublicKey};
use russh::keys::PublicKeyBase64;
use russh::server::{Auth, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, CryptoVec};
use rithub_core::{AccessStore, RepoRole, ReviewStore, SshKeyStore, Store, User, Visibility};
use tokio::io::AsyncWriteExt;

use crate::state::AppState;

/// Start the SSH server, listening on `bind` (e.g. `127.0.0.1:2222`).
pub async fn serve(state: AppState, bind: String, rit_bin: String) -> anyhow::Result<()> {
    let host_key = KeyPair::generate_ed25519().expect("generate ed25519 host key");
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
            user: None,
            channels: HashMap::new(),
        }
    }
}

struct SshHandler {
    state: AppState,
    rit_bin: Arc<String>,
    user: Option<User>,
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
        // Identify the user by the offered key's blob (russh has already verified
        // the client owns the private key before this is called).
        let blob = public_key.public_key_base64();
        match self.state.store.find_user_by_ssh_key(&blob).await {
            Ok(Some(user)) => {
                self.user = Some(user);
                Ok(Auth::Accept)
            }
            _ => Ok(Auth::Reject {
                proceed_with_methods: None,
            }),
        }
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
        let user = self.user.as_ref().ok_or("not authenticated")?;
        let (service, raw_path) = parse_git_command(command).ok_or("unsupported command")?;
        let (org, repo) = split_repo_path(raw_path).ok_or("bad repository path")?;

        let meta = self
            .state
            .store
            .get_repo(&org, &repo)
            .await
            .map_err(|_| "repository not found".to_string())?;

        let role = self
            .state
            .store
            .effective_role(user.id, meta.org_id, meta.id)
            .await
            .ok()
            .flatten();

        match service {
            "git-upload-pack" => {
                // Read: public repos are readable; private needs ≥Read.
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
                // Write: requires ≥Write.
                if !matches!(role, Some(r) if r >= RepoRole::Write) {
                    return Err("write access denied".into());
                }
                // Non-admins are blocked from direct pushes to protected branches.
                let block_branches = if matches!(role, Some(r) if r >= RepoRole::Admin) {
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
}

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
