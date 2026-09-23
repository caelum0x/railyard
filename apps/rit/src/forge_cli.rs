//! `rit issue ...` and `rit pr ...`: work with rithub issues and pull requests
//! from the command line through the forge JSON API.
//!
//! The target repository is derived from a configured remote (default
//! `origin`): `https://host/<org>/<repo>.git` talks to `https://host/api/...`;
//! ssh remotes default to `https://<host>` (override with `RIT_API_URL`).
//! Authentication uses `RIT_USERNAME` + `RIT_TOKEN` (a rithub personal access
//! token), or credentials embedded in an http(s) remote URL.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use rit::remote::{forge_repo, get_remote, ForgeRepo};
use rit::Repository;
use serde_json::{json, Value};

/// Which remote identifies the forge repository.
#[derive(Args, Clone)]
pub struct ForgeTarget {
    /// Remote whose URL names the rithub repository.
    #[arg(long = "remote", default_value = "origin", global = true)]
    remote: String,
}

/// State filter for list commands.
#[derive(Clone, Copy, ValueEnum)]
pub enum StateFilter {
    Open,
    Closed,
    Merged,
    All,
}

#[derive(Args)]
pub struct IssueArgs {
    #[command(flatten)]
    target: ForgeTarget,
    #[command(subcommand)]
    action: IssueAction,
}

#[derive(Subcommand)]
enum IssueAction {
    /// List issues (open by default).
    List {
        #[arg(long = "state", value_enum, default_value = "open")]
        state: StateFilter,
    },
    /// Show one issue, optionally with its comments.
    View {
        number: i32,
        #[arg(long = "comments")]
        comments: bool,
    },
    /// Open a new issue.
    Create {
        /// Title (required).
        #[arg(short = 't', long = "title")]
        title: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Close an issue.
    Close { number: i32 },
    /// Reopen a closed issue.
    Reopen { number: i32 },
    /// Comment on an issue.
    Comment {
        number: i32,
        #[command(flatten)]
        body: BodyArgs,
    },
}

#[derive(Args)]
pub struct PrArgs {
    #[command(flatten)]
    target: ForgeTarget,
    #[command(subcommand)]
    action: PrAction,
}

#[derive(Subcommand)]
enum PrAction {
    /// List pull requests (open by default).
    List {
        #[arg(long = "state", value_enum, default_value = "open")]
        state: StateFilter,
    },
    /// Show one pull request with mergeability and approvals.
    View {
        number: i32,
        #[arg(long = "comments")]
        comments: bool,
    },
    /// Open a pull request from `--head` (default: current branch) into
    /// `--base` (default: the repository's default branch).
    Create {
        /// Title (required).
        #[arg(short = 't', long = "title")]
        title: String,
        /// Source branch (default: the current branch).
        #[arg(long = "head")]
        head: Option<String>,
        /// Target branch (default: the repository's default branch).
        #[arg(long = "base")]
        base: Option<String>,
        /// Source `org/repo` for a pull request from a fork.
        #[arg(long = "head-repo")]
        head_repo: Option<String>,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Merge a pull request (subject to branch protection).
    Merge {
        number: i32,
        /// How to merge.
        #[arg(long = "strategy", value_enum, default_value = "merge")]
        strategy: MergeStrategy,
        /// Merge commit message.
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
    },
    /// Close a pull request without merging.
    Close { number: i32 },
    /// Reopen a closed pull request.
    Reopen { number: i32 },
    /// Comment on a pull request.
    Comment {
        number: i32,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Print the pull request's diff.
    Diff {
        number: i32,
        /// Only list changed files with their line counts.
        #[arg(long = "stat")]
        stat: bool,
    },
    /// Show the check runs for the pull request's head commit.
    Checks { number: i32 },
}

#[derive(Clone, Copy, ValueEnum)]
enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

impl MergeStrategy {
    fn as_str(self) -> &'static str {
        match self {
            MergeStrategy::Merge => "merge",
            MergeStrategy::Squash => "squash",
            MergeStrategy::Rebase => "rebase",
        }
    }
}

/// A text body given inline or from a file (`-` = stdin).
#[derive(Args)]
struct BodyArgs {
    /// Body text (Markdown).
    #[arg(short = 'b', long = "body", conflicts_with = "body_file")]
    body: Option<String>,
    /// Read the body from a file, or `-` for stdin.
    #[arg(short = 'F', long = "body-file")]
    body_file: Option<String>,
}

impl BodyArgs {
    fn read(&self) -> Result<Option<String>> {
        if let Some(body) = &self.body {
            return Ok(Some(body.clone()));
        }
        match self.body_file.as_deref() {
            None => Ok(None),
            Some("-") => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text).context("reading body from stdin")?;
                Ok(Some(text))
            }
            Some(path) => Ok(Some(
                std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
            )),
        }
    }

    fn require(&self) -> Result<String> {
        match self.read()? {
            Some(text) if !text.trim().is_empty() => Ok(text),
            _ => bail!("a body is required (--body <text> or --body-file <path|->)"),
        }
    }
}

fn open_forge(repo_path: &Path, target: &ForgeTarget) -> Result<(Repository, ForgeRepo)> {
    let repo = Repository::discover(repo_path)
        .with_context(|| format!("opening repo at {}", repo_path.display()))?;
    let remote = get_remote(&repo, &target.remote)?;
    let forge = forge_repo(&remote.url)?;
    Ok((repo, forge))
}

fn state_query(state: StateFilter) -> &'static str {
    match state {
        StateFilter::Open => "?state=open",
        StateFilter::Closed => "?state=closed",
        StateFilter::Merged => "?state=merged",
        StateFilter::All => "",
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Who wrote an issue / PR / comment. The forge returns `author_login`
/// (joined from its users table); `author_name`, an embedded `author`
/// object/string, or `user.login` are accepted too. Deleted accounts show as
/// `ghost`, matching the web UI.
fn author(v: &Value) -> &str {
    let direct = ["author_login", "author_name", "author", "user_login"]
        .iter()
        .find_map(|key| v.get(*key).and_then(Value::as_str).filter(|a| !a.is_empty()));
    let nested = || {
        ["author", "user"].iter().find_map(|key| {
            v.get(*key)
                .and_then(|o| o.get("login").or_else(|| o.get("username")))
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
        })
    };
    direct.or_else(nested).unwrap_or("ghost")
}

/// `2026-09-24T10:11:12Z` -> `2026-09-24`.
fn day(ts: &str) -> &str {
    ts.get(..10).unwrap_or(ts)
}

fn print_body(body: &str) {
    if body.trim().is_empty() {
        println!("  (no description)");
    } else {
        for line in body.lines() {
            println!("  {line}");
        }
    }
}

fn print_comments(forge: &ForgeRepo, sub: &str) -> Result<()> {
    let comments = forge.api("GET", sub, None)?;
    let list = comments.as_array().cloned().unwrap_or_default();
    println!();
    if list.is_empty() {
        println!("No comments.");
        return Ok(());
    }
    println!("{} comment(s):", list.len());
    for c in &list {
        println!();
        println!("  -- {}, {} --", author(c), day(s(c, "created_at")));
        for line in s(c, "body").lines() {
            println!("  {line}");
        }
    }
    Ok(())
}

pub fn cmd_issue(repo_path: &Path, args: &IssueArgs) -> Result<()> {
    let (_, forge) = open_forge(repo_path, &args.target)?;
    match &args.action {
        IssueAction::List { state } => {
            let issues = forge.api("GET", &format!("/issues{}", state_query(*state)), None)?;
            let list = issues.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("No issues in {}.", forge.slug());
            }
            for issue in &list {
                println!(
                    "#{:<5} {:<7} {}  {:<16} {}",
                    n(issue, "number"),
                    s(issue, "state"),
                    day(s(issue, "created_at")),
                    author(issue),
                    s(issue, "title")
                );
            }
        }
        IssueAction::View { number, comments } => {
            let issue = forge.api("GET", &format!("/issues/{number}"), None)?;
            println!("#{} {}", n(&issue, "number"), s(&issue, "title"));
            println!(
                "state: {}   opened: {} by {}   comments: {}",
                s(&issue, "state"),
                day(s(&issue, "created_at")),
                author(&issue),
                n(&issue, "comment_count")
            );
            let join = |key: &str| {
                issue
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            };
            let labels = join("labels");
            let assignees = join("assignees");
            if !labels.is_empty() {
                println!("labels: {labels}");
            }
            if !assignees.is_empty() {
                println!("assignees: {assignees}");
            }
            println!("{}", forge.web_url(&format!("issues/{number}")));
            println!();
            print_body(s(&issue, "body"));
            if *comments {
                print_comments(&forge, &format!("/issues/{number}/comments"))?;
            }
        }
        IssueAction::Create { title, body } => {
            let issue = forge.api(
                "POST",
                "/issues",
                Some(&json!({ "title": title, "body": body.read()? })),
            )?;
            let number = n(&issue, "number");
            println!("Opened issue #{number}: {}", s(&issue, "title"));
            println!("{}", forge.web_url(&format!("issues/{number}")));
        }
        IssueAction::Close { number } => {
            forge.api("POST", &format!("/issues/{number}/close"), None)?;
            println!("Closed issue #{number} in {}", forge.slug());
        }
        IssueAction::Reopen { number } => {
            forge.api("POST", &format!("/issues/{number}/reopen"), None)?;
            println!("Reopened issue #{number} in {}", forge.slug());
        }
        IssueAction::Comment { number, body } => {
            forge.api(
                "POST",
                &format!("/issues/{number}/comments"),
                Some(&json!({ "body": body.require()? })),
            )?;
            println!("Commented on issue #{number}");
            println!("{}", forge.web_url(&format!("issues/{number}")));
        }
    }
    Ok(())
}

fn current_branch(repo: &Repository) -> Result<String> {
    let head = std::fs::read_to_string(repo.git_dir().join("HEAD")).context("reading HEAD")?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .context("HEAD is detached; pass --head <branch>")
}

pub fn cmd_pr(repo_path: &Path, args: &PrArgs) -> Result<()> {
    let (repo, forge) = open_forge(repo_path, &args.target)?;
    match &args.action {
        PrAction::List { state } => {
            let prs = forge.api("GET", &format!("/pulls{}", state_query(*state)), None)?;
            let list = prs.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("No pull requests in {}.", forge.slug());
            }
            for pr in &list {
                println!(
                    "#{:<5} {:<7} {:<16} {} -> {}  {}",
                    n(pr, "number"),
                    s(pr, "state"),
                    author(pr),
                    s(pr, "head_branch"),
                    s(pr, "base_branch"),
                    s(pr, "title")
                );
            }
        }
        PrAction::View { number, comments } => {
            let pr = forge.api("GET", &format!("/pulls/{number}"), None)?;
            println!("#{} {}", n(&pr, "number"), s(&pr, "title"));
            println!(
                "state: {}   {} -> {}   opened: {} by {}",
                s(&pr, "state"),
                s(&pr, "head_branch"),
                s(&pr, "base_branch"),
                day(s(&pr, "created_at")),
                author(&pr)
            );
            let mergeable = match pr.get("mergeable").and_then(Value::as_bool) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            };
            println!(
                "mergeable: {mergeable} ({})   approvals: {}",
                pr.get("merge_status").and_then(Value::as_str).unwrap_or("n/a"),
                n(&pr, "approvals")
            );
            if let Some(sha) = pr.get("merge_commit_sha").and_then(Value::as_str) {
                println!("merge commit: {sha}");
            }
            println!("{}", forge.web_url(&format!("pulls/{number}")));
            println!();
            print_body(s(&pr, "body"));
            if *comments {
                print_comments(&forge, &format!("/pulls/{number}/comments"))?;
            }
        }
        PrAction::Create { title, head, base, head_repo, body } => {
            let head = match head {
                Some(h) => h.clone(),
                None => current_branch(&repo)?,
            };
            let base = match base {
                Some(b) => b.clone(),
                None => {
                    let meta = forge.api("GET", "", None)?;
                    let b = s(&meta, "default_branch");
                    if b.is_empty() { "main".to_string() } else { b.to_string() }
                }
            };
            let pr = forge.api(
                "POST",
                "/pulls",
                Some(&json!({
                    "title": title,
                    "body": body.read()?,
                    "head": head,
                    "base": base,
                    "head_repo": head_repo,
                })),
            )?;
            let number = n(&pr, "number");
            println!("Opened pull request #{number}: {} ({head} -> {base})", s(&pr, "title"));
            println!("{}", forge.web_url(&format!("pulls/{number}")));
        }
        PrAction::Merge { number, strategy, message } => {
            let out = forge.api(
                "POST",
                &format!("/pulls/{number}/merge"),
                Some(&json!({ "strategy": strategy.as_str(), "message": message })),
            )?;
            let sha = s(&out, "merge_commit_sha");
            println!(
                "Merged pull request #{number} ({}){}",
                if s(&out, "kind").is_empty() { strategy.as_str() } else { s(&out, "kind") },
                if sha.is_empty() { String::new() } else { format!(" as {}", &sha[..sha.len().min(12)]) }
            );
        }
        PrAction::Close { number } => {
            forge.api("POST", &format!("/pulls/{number}/close"), None)?;
            println!("Closed pull request #{number} in {}", forge.slug());
        }
        PrAction::Reopen { number } => {
            forge.api("POST", &format!("/pulls/{number}/reopen"), None)?;
            println!("Reopened pull request #{number} in {}", forge.slug());
        }
        PrAction::Comment { number, body } => {
            forge.api(
                "POST",
                &format!("/pulls/{number}/comments"),
                Some(&json!({ "body": body.require()? })),
            )?;
            println!("Commented on pull request #{number}");
            println!("{}", forge.web_url(&format!("pulls/{number}")));
        }
        PrAction::Diff { number, stat } => {
            let diff = forge.api("GET", &format!("/pulls/{number}/diff"), None)?;
            let files = diff.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
            if files.is_empty() {
                println!("No changes.");
                return Ok(());
            }
            for f in &files {
                if *stat {
                    println!(
                        " {:<9} +{:<5} -{:<5} {}",
                        s(f, "status"),
                        n(f, "additions"),
                        n(f, "deletions"),
                        s(f, "path")
                    );
                    continue;
                }
                let path = s(f, "path");
                println!("diff --rit a/{path} b/{path}");
                if f.get("binary").and_then(Value::as_bool).unwrap_or(false) {
                    println!("Binary file {path} ({})", s(f, "status"));
                    continue;
                }
                let patch = s(f, "patch");
                print!("{patch}");
                if !patch.ends_with('\n') && !patch.is_empty() {
                    println!();
                }
            }
            if *stat {
                println!(
                    " {} file(s) changed, {} insertion(s), {} deletion(s)",
                    files.len(),
                    n(&diff, "additions"),
                    n(&diff, "deletions")
                );
            }
        }
        PrAction::Checks { number } => {
            let checks = forge.api("GET", &format!("/pulls/{number}/check-runs"), None)?;
            let sha = s(&checks, "sha");
            println!(
                "checks for {}: {}",
                &sha[..sha.len().min(12)],
                s(&checks, "state")
            );
            let runs = checks.get("check_runs").and_then(Value::as_array).cloned().unwrap_or_default();
            if runs.is_empty() {
                println!("No check runs reported yet.");
            }
            for run in &runs {
                let conclusion = run.get("conclusion").and_then(Value::as_str).unwrap_or("-");
                println!("  {:<24} {:<12} {}", s(run, "name"), s(run, "status"), conclusion);
                if let Some(url) = run.get("details_url").and_then(Value::as_str) {
                    println!("  {:<24} {url}", "");
                }
            }
        }
    }
    Ok(())
}
