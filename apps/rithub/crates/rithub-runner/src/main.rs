//! `rithub-runner`: check out a commit and run its workflows.
//!
//! ```text
//! RITHUB_RUNNER_TOKEN=... rithub-runner \
//!   --forge https://forge.example --org acme --repo app \
//!   --sha <40-hex> [--event push] [--checkout <dir>]
//! ```
//!
//! Without `--checkout` the runner clones `{forge}/{org}/{repo}.git` (with the
//! runner token as the HTTP password) into a temporary directory, checks out
//! the commit, runs, and removes the directory.

use std::path::PathBuf;

use anyhow::{bail, Context};
use rithub_runner::{run_commit, HttpReporter};

struct Args {
    forge: String,
    org: String,
    repo: String,
    sha: String,
    event: String,
    checkout: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut forge = None;
    let mut org = None;
    let mut repo = None;
    let mut sha = None;
    let mut event = "push".to_string();
    let mut checkout = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--forge" => forge = Some(value()?),
            "--org" => org = Some(value()?),
            "--repo" => repo = Some(value()?),
            "--sha" => sha = Some(value()?),
            "--event" => event = value()?,
            "--checkout" => checkout = Some(PathBuf::from(value()?)),
            other => bail!("unknown flag {other}"),
        }
    }
    let sha = sha.context("--sha is required")?;
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("--sha must be a 40-character commit id");
    }
    Ok(Args { forge: forge.context("--forge is required")?, org: org.context("--org is required")?, repo: repo.context("--repo is required")?, sha, event, checkout })
}

fn git(args: &[&str], dir: &std::path::Path) -> anyhow::Result<()> {
    let status = std::process::Command::new("git").args(args).current_dir(dir).status().context("running git")?;
    if !status.success() {
        bail!("git {} failed", args.first().copied().unwrap_or_default());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let args = parse_args()?;
    let token = std::env::var("RITHUB_RUNNER_TOKEN").context("RITHUB_RUNNER_TOKEN must be set")?;
    let (dir, temporary) = match &args.checkout {
        Some(d) => (d.clone(), false),
        None => {
            let dir = std::env::temp_dir().join(format!("rithub-runner-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir)?;
            let base = args.forge.trim_end_matches('/');
            let (scheme, host) = base.split_once("://").context("--forge must be a URL")?;
            let url = format!("{scheme}://runner:{token}@{host}/{}/{}.git", args.org, args.repo);
            git(&["clone", "--quiet", &url, "."], &dir)?;
            git(&["checkout", "--quiet", "--detach", &args.sha], &dir)?;
            (dir, true)
        }
    };
    let reporter = HttpReporter::new(&args.forge, &args.org, &args.repo, token);
    let result = run_commit(&dir, &args.sha, &args.event, &reporter).await;
    if temporary {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let reports = result?;
    for r in &reports {
        tracing::info!(check = %r.check_name, conclusion = r.conclusion.as_str(), "job finished");
    }
    if reports.iter().any(|r| r.conclusion != rithub_runner::Conclusion::Success) {
        std::process::exit(1);
    }
    Ok(())
}
