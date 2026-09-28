//! Command definitions and handlers.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand};
use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{Client, Stored, CLIENT_ID};
use crate::{parse_since, parse_usd, usd};

#[derive(Parser, Debug)]
#[command(name = "sy", version, about = "Switchyard: one key for every tool your agent needs")]
pub struct Cli {
    /// Print raw JSON responses.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Sign in with the device-code flow (no key is ever pasted).
    Login {
        #[arg(long, env = "SWITCHYARD_URL")]
        url: String,
        #[arg(long, default_value = "tools:read tools:call")]
        scope: String,
    },
    /// Forget the stored token.
    Logout,
    /// Search tools by task.
    Search {
        query: Vec<String>,
        #[arg(long)]
        side_effect: Option<String>,
        /// Max price in dollars, e.g. 0.01.
        #[arg(long)]
        max_price: Option<String>,
    },
    /// Show schemas, price, example and routing for a tool or capability.
    Describe { id: String },
    /// Call a tool or capability.
    Call(CallArgs),
    /// Balance, open holds and spend against caps.
    Balance,
    /// Start a credit top-up (Dodo checkout); prints the checkout URL.
    Topup { amount: String },
    /// Usage and spend.
    Usage {
        #[arg(long, default_value = "tool")]
        by: String,
        #[arg(long, default_value = "30d")]
        since: String,
    },
    /// Recent calls (audit log).
    Calls {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// API keys (one key works on Relay and Switchyard).
    #[command(subcommand)]
    Keys(KeysCmd),
    /// Write-only provider credentials.
    #[command(subcommand)]
    Cred(CredCmd),
    /// SKILL.md bundles.
    #[command(subcommand)]
    Bundle(BundleCmd),
    /// Project caps, routing policy and pinned MCP tools.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Stdio MCP shim for local clients (forwards to /mcp).
    Mcp,
}

#[derive(Args, Debug)]
pub struct CallArgs {
    pub id: String,
    /// JSON input, `@file.json`, or `-` for stdin.
    #[arg(long, default_value = "{}")]
    pub input: String,
    /// Pin a provider (disables fallback).
    #[arg(long)]
    pub pin: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    /// Max cost in dollars.
    #[arg(long)]
    pub max_cost: Option<String>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub no_cache: bool,
}

#[derive(Subcommand, Debug)]
pub enum KeysCmd {
    Ls,
    Create {
        /// Comma-separated scopes.
        #[arg(long, default_value = "tools:read,tools:call")]
        scopes: String,
        /// Comma-separated tool or capability ids this key may call.
        #[arg(long)]
        tools: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
        /// Daily cap in dollars.
        #[arg(long)]
        daily_cap: Option<String>,
    },
    Revoke { id: String },
}

#[derive(Subcommand, Debug)]
pub enum CredCmd {
    /// Store a credential; the value is read from stdin and never echoed.
    Add {
        provider: String,
        #[arg(long)]
        value_stdin: bool,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "api_key")]
        kind: String,
    },
    Ls,
    Rm { id: String },
}

#[derive(Subcommand, Debug)]
pub enum BundleCmd {
    /// Push a folder with SKILL.md (+ optional bundle.yaml).
    Push { dir: PathBuf },
    Ls,
    /// Write SKILL.md locally (tool ids only, never secrets).
    Install {
        id: String,
        #[arg(long, default_value = ".claude/skills")]
        dir: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    Show,
    /// Set caps in dollars; `--monthly off` removes a cap.
    Cap {
        #[arg(long)]
        monthly: Option<String>,
        #[arg(long)]
        daily: Option<String>,
    },
    /// Pin up to 20 tools as first-class MCP tools.
    Pin { ids: Vec<String> },
    /// free_first | cheapest | fastest | pinned
    Policy { policy: String },
}

/// Injectable I/O so commands are testable.
pub struct Io {
    pub stdin: Box<dyn BufRead + Send>,
    pub out: Box<dyn Write + Send>,
    pub config_dir: PathBuf,
    pub env_token: Option<String>,
    pub env_url: Option<String>,
    pub open_browser: bool,
}

impl Io {
    pub fn stdio() -> Self {
        let config_dir = std::env::var("SWITCHYARD_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config/switchyard")
        });
        Io {
            stdin: Box::new(std::io::BufReader::new(std::io::stdin())),
            out: Box::new(std::io::stdout()),
            config_dir,
            env_token: std::env::var("SWITCHYARD_TOKEN").ok().filter(|s| !s.is_empty()),
            env_url: std::env::var("SWITCHYARD_URL").ok().filter(|s| !s.is_empty()),
            open_browser: std::env::var("SWITCHYARD_NO_BROWSER").is_err(),
        }
    }
}

fn check(r: &crate::client::Reply) -> anyhow::Result<&Value> {
    if (200..300).contains(&r.status) {
        return Ok(&r.body);
    }
    let e = &r.body["error"];
    let mut msg = format!("{} {}: {}", r.status, e["code"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or(&r.text));
    match e["code"].as_str() {
        Some("insufficient_balance") => msg.push_str(&format!("\n  balance {} < estimate {}. Top up: sy topup 20", usd(e["details"]["balance_micros"].as_i64().unwrap_or(0)), usd(e["details"]["estimated_cost_micros"].as_i64().unwrap_or(0)))),
        Some("cap_exceeded") => msg.push_str(&format!("\n  cap `{}` reached. Raise it: sy project cap --daily 5", e["details"]["cap"].as_str().unwrap_or("?"))),
        Some("invalid_input") => {
            for x in e["details"]["errors"].as_array().into_iter().flatten() {
                msg.push_str(&format!("\n  {} {}", x["pointer"].as_str().unwrap_or(""), x["message"].as_str().unwrap_or("")));
            }
        }
        Some("consent_required") => msg.push_str(&format!("\n  approve once: {}", e["details"]["consent_url"].as_str().unwrap_or(""))),
        _ => {}
    }
    bail!(msg)
}

fn read_input(io: &mut Io, spec: &str) -> anyhow::Result<Value> {
    let text = if spec == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut io.stdin, &mut s)?;
        s
    } else if let Some(path) = spec.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))?
    } else {
        spec.to_string()
    };
    serde_json::from_str(&text).context("--input must be JSON (or @file.json, or - for stdin)")
}

fn open_url(io: &Io, url: &str) {
    if !io.open_browser {
        return;
    }
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(cmd).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Minimal SKILL.md front matter reader (`name:` / `description:`).
pub fn front_matter(md: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut desc = None;
    if let Some(rest) = md.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some(v) = line.strip_prefix("name:") {
                    name = Some(v.trim().trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("description:") {
                    desc = Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    (name, desc)
}

async fn login(io: &mut Io, url: &str, scope: &str) -> anyhow::Result<i32> {
    let base = url.trim_end_matches('/').to_string();
    let http = Client::http();
    let start: Value = http.post(format!("{base}/oauth/device/code")).form(&[("client_id", CLIENT_ID), ("scope", scope)]).send().await?.json().await?;
    let (Some(device), Some(code), Some(verify)) = (start["device_code"].as_str(), start["user_code"].as_str(), start["verification_uri_complete"].as_str()) else {
        bail!("device authorization failed: {start}");
    };
    writeln!(io.out, "Open {verify}\nand confirm the code {code}")?;
    open_url(io, verify);
    let mut interval = start["interval"].as_u64().unwrap_or(5);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(start["expires_in"].as_u64().unwrap_or(600));
    loop {
        if std::time::Instant::now() > deadline {
            bail!("the code expired; run `sy login` again");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let v: Value = http
            .post(format!("{base}/oauth/token"))
            .form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("device_code", device), ("client_id", CLIENT_ID)])
            .send()
            .await?
            .json()
            .await?;
        if let Some(at) = v["access_token"].as_str() {
            Stored {
                url: base.clone(),
                access_token: at.into(),
                refresh_token: v["refresh_token"].as_str().unwrap_or_default().into(),
                expires_at: chrono::Utc::now().timestamp() + v["expires_in"].as_i64().unwrap_or(600),
            }
            .save(&io.config_dir)?;
            writeln!(io.out, "Signed in. Scopes: {}", v["scope"].as_str().unwrap_or(""))?;
            return Ok(0);
        }
        match v["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(other) => bail!("{other}: {}", v["error_description"].as_str().unwrap_or("")),
            None => bail!("unexpected token response: {v}"),
        }
    }
}

pub async fn run(cli: Cli, io: &mut Io) -> anyhow::Result<i32> {
    match &cli.cmd {
        Cmd::Login { url, scope } => return login(io, url, scope).await,
        Cmd::Logout => {
            let _ = std::fs::remove_file(Stored::path(&io.config_dir));
            writeln!(io.out, "Signed out.")?;
            return Ok(0);
        }
        _ => {}
    }
    let mut c = Client::from_env(&io.config_dir, io.env_token.clone(), io.env_url.clone())?;
    let json_out = cli.json;
    match cli.cmd {
        Cmd::Login { .. } | Cmd::Logout => unreachable!(),
        Cmd::Search { query, side_effect, max_price } => {
            let mut u = url::Url::parse("http://x/v1/tools/search").expect("static");
            u.query_pairs_mut().append_pair("q", &query.join(" "));
            if let Some(s) = side_effect {
                u.query_pairs_mut().append_pair("side_effect", &s);
            }
            if let Some(p) = max_price {
                u.query_pairs_mut().append_pair("max_price_micros", &parse_usd(&p)?.to_string());
            }
            let r = c.get(&format!("/v1/tools/search?{}", u.query().unwrap_or(""))).await?;
            let v = check(&r)?;
            if json_out {
                writeln!(io.out, "{}", pretty(v))?;
            } else {
                for hit in v["data"]["results"].as_array().into_iter().flatten() {
                    writeln!(io.out, "{}  {}", hit["capability"].as_str().unwrap_or(""), hit["title"].as_str().unwrap_or(""))?;
                    for cand in hit["candidates"].as_array().into_iter().flatten() {
                        let price = cand["price"]["micros"].as_i64().unwrap_or(0);
                        let tag = if cand["requires_byo_key"] == true { " [your key]" } else if cand["free"] == true { " [free]" } else { "" };
                        writeln!(io.out, "    {:<34} {:<10} {} {}{}", cand["tool_id"].as_str().unwrap_or(""), cand["provider"].as_str().unwrap_or(""), usd(price), cand["price"]["basis"].as_str().unwrap_or(""), tag)?;
                    }
                }
            }
        }
        Cmd::Describe { id } => {
            let r = c.get(&format!("/v1/tools/{id}")).await?;
            writeln!(io.out, "{}", pretty(&check(&r)?["data"]))?;
        }
        Cmd::Call(a) => {
            let input = read_input(io, &a.input)?;
            let mut options = json!({ "dry_run": a.dry_run });
            if let Some(p) = &a.pin {
                options["provider"] = json!(p);
            }
            if let Some(m) = &a.max_cost {
                options["max_cost_micros"] = json!(parse_usd(m)?);
            }
            if a.no_cache {
                options["cache"] = json!("bypass");
            }
            let mut headers = Vec::new();
            if let Some(k) = &a.idempotency_key {
                headers.push(("idempotency-key", k.clone()));
            }
            let r = c.request(Method::POST, &format!("/v1/tools/{}/call", a.id), Some(&json!({ "input": input, "options": options })), &headers).await?;
            let v = check(&r)?;
            if json_out {
                writeln!(io.out, "{}", pretty(v))?;
            } else {
                writeln!(io.out, "{}", pretty(&v["data"]))?;
                let m = &v["meta"];
                if m["dry_run"] == true {
                    writeln!(io.out, "-- dry run: would hold {}", usd(m["quote_micros"].as_i64().unwrap_or(0)))?;
                } else {
                    writeln!(
                        io.out,
                        "-- {} via {} · {} · cache {}{}",
                        m["tool"].as_str().unwrap_or(""),
                        m["provider"].as_str().unwrap_or(""),
                        usd(m["cost_micros"].as_i64().unwrap_or(0)),
                        m["cache"].as_str().unwrap_or(""),
                        if m["fallback_chain"].as_array().map(|a| a.len() > 1).unwrap_or(false) { " · fell back" } else { "" }
                    )?;
                }
            }
        }
        Cmd::Balance => {
            let r = c.get("/v1/balance").await?;
            let d = &check(&r)?["data"];
            if json_out {
                writeln!(io.out, "{}", pretty(d))?;
            } else {
                match d["balance"]["available_micros"].as_i64() {
                    Some(a) => writeln!(io.out, "Balance {} (held {})", usd(a), usd(d["balance"]["held_micros"].as_i64().unwrap_or(0)))?,
                    None => writeln!(io.out, "Balance unavailable")?,
                }
                let p = &d["project"];
                writeln!(io.out, "Today {} · this month {}", usd(p["spent_today_micros"].as_i64().unwrap_or(0)), usd(p["spent_this_month_micros"].as_i64().unwrap_or(0)))?;
                if let Some(cap) = p["daily_cap_micros"].as_i64() {
                    writeln!(io.out, "Daily cap {} ({} left)", usd(cap), usd(p["daily_remaining_micros"].as_i64().unwrap_or(0)))?;
                }
                if let Some(cap) = p["monthly_cap_micros"].as_i64() {
                    writeln!(io.out, "Monthly cap {} ({} left)", usd(cap), usd(p["monthly_remaining_micros"].as_i64().unwrap_or(0)))?;
                }
            }
        }
        Cmd::Topup { amount } => {
            let r = c.post("/v1/balance/topup", &json!({ "amount_micros": parse_usd(&amount)? })).await?;
            let url = check(&r)?["data"]["checkout_url"].as_str().unwrap_or("").to_string();
            writeln!(io.out, "Complete the payment at:\n{url}")?;
            open_url(io, &url);
        }
        Cmd::Usage { by, since } => {
            let from = (chrono::Utc::now() - parse_since(&since)?).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let r = c.get(&format!("/v1/usage?group_by={by}&from={}", url::form_urlencoded::byte_serialize(from.as_bytes()).collect::<String>())).await?;
            let d = &check(&r)?["data"];
            if json_out {
                writeln!(io.out, "{}", pretty(d))?;
            } else {
                for row in d["rows"].as_array().into_iter().flatten() {
                    writeln!(io.out, "{:<40} {:>6} calls  {}", row["group"].as_str().unwrap_or(""), row["calls"], usd(row["cost_micros"].as_i64().unwrap_or(0)))?;
                }
                writeln!(io.out, "Total {} over {} calls", usd(d["total_cost_micros"].as_i64().unwrap_or(0)), d["total_calls"])?;
            }
        }
        Cmd::Calls { limit } => {
            let r = c.get(&format!("/v1/usage/calls?limit={limit}")).await?;
            let d = &check(&r)?["data"];
            for row in d["calls"].as_array().into_iter().flatten() {
                writeln!(
                    io.out,
                    "{}  {:<34} {:<10} {:<8} {}",
                    row["id"].as_str().unwrap_or(""),
                    row["tool_id"].as_str().unwrap_or(""),
                    row["provider_id"].as_str().unwrap_or("-"),
                    row["status"].as_str().unwrap_or(""),
                    usd(row["cost_micros"].as_i64().unwrap_or(0))
                )?;
            }
        }
        Cmd::Keys(k) => match k {
            KeysCmd::Ls => {
                let r = c.get("/v1/keys").await?;
                for key in check(&r)?["data"].as_array().into_iter().flatten() {
                    let state = if key["revoked_at"].is_null() { "active" } else { "revoked" };
                    writeln!(io.out, "{}  {:<8} {}  {}", key["id"].as_str().unwrap_or(""), state, key["scopes"], key["description"].as_str().unwrap_or(""))?;
                }
            }
            KeysCmd::Create { scopes, tools, description, daily_cap } => {
                let mut body = json!({ "scopes": scopes.split(',').map(str::trim).collect::<Vec<_>>(), "description": description });
                if let Some(t) = tools {
                    body["tool_allow"] = json!(t.split(',').map(str::trim).collect::<Vec<_>>());
                }
                if let Some(d) = daily_cap {
                    body["per_key_daily_cap_micros"] = json!(parse_usd(&d)?);
                }
                let r = c.post("/v1/keys", &body).await?;
                let d = &check(&r)?["data"];
                writeln!(io.out, "{}\n(shown once; works on Relay and Switchyard)", d["key"].as_str().unwrap_or(""))?;
            }
            KeysCmd::Revoke { id } => {
                check(&c.post(&format!("/v1/keys/{id}/revoke"), &json!({})).await?)?;
                writeln!(io.out, "Revoked {id}")?;
            }
        },
        Cmd::Cred(cmd) => match cmd {
            CredCmd::Add { provider, value_stdin, name, kind } => {
                if !value_stdin {
                    bail!("pass --value-stdin and pipe the secret in, e.g. `pbpaste | sy cred add brave --value-stdin`");
                }
                let mut value = String::new();
                io.stdin.read_line(&mut value)?;
                let value = value.trim().to_string();
                if value.is_empty() {
                    bail!("no value on stdin");
                }
                let r = c.post("/v1/credentials", &json!({ "provider": provider, "value": value, "name": name, "kind": kind })).await?;
                let d = &check(&r)?["data"];
                writeln!(io.out, "Stored {} for {} (value is write-only)", d["id"].as_str().unwrap_or(""), d["provider"].as_str().unwrap_or(""))?;
            }
            CredCmd::Ls => {
                let r = c.get("/v1/credentials").await?;
                for cr in check(&r)?["data"].as_array().into_iter().flatten() {
                    writeln!(io.out, "{}  {:<12} {}  last_ok={} last_error={}", cr["id"].as_str().unwrap_or(""), cr["provider"].as_str().unwrap_or(""), cr["name"].as_str().unwrap_or(""), cr["last_ok_at"], cr["last_error"])?;
                }
            }
            CredCmd::Rm { id } => {
                check(&c.request(Method::DELETE, &format!("/v1/credentials/{id}"), None, &[]).await?)?;
                writeln!(io.out, "Deleted {id}")?;
            }
        },
        Cmd::Bundle(cmd) => match cmd {
            BundleCmd::Push { dir } => {
                let md = std::fs::read_to_string(dir.join("SKILL.md")).with_context(|| format!("{}/SKILL.md", dir.display()))?;
                let (fm_name, fm_desc) = front_matter(&md);
                let meta: Value = match std::fs::read_to_string(dir.join("bundle.yaml")) {
                    Ok(t) => serde_yaml::from_str(&t).context("bundle.yaml")?,
                    Err(_) => json!({}),
                };
                let id = meta["id"].as_str().map(str::to_string).or(fm_name.clone()).or_else(|| dir.file_name().map(|f| f.to_string_lossy().to_string())).context("bundle id")?;
                let body = json!({
                    "id": id,
                    "name": meta["name"].as_str().map(str::to_string).or(fm_name).unwrap_or_else(|| id.clone()),
                    "description": meta["description"].as_str().map(str::to_string).or(fm_desc).unwrap_or_default(),
                    "skill_md": md,
                    "tool_ids": meta["tool_ids"].as_array().cloned().unwrap_or_default(),
                    "credential_refs": meta["credential_refs"].as_array().cloned().unwrap_or_default(),
                });
                let d = check(&c.post("/v1/bundles", &body).await?)?["data"].clone();
                writeln!(io.out, "Pushed {} v{}", d["id"].as_str().unwrap_or(""), d["version"])?;
            }
            BundleCmd::Ls => {
                let r = c.get("/v1/bundles").await?;
                for b in check(&r)?["data"].as_array().into_iter().flatten() {
                    writeln!(io.out, "{:<24} {}{}", b["id"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or(""), if b["public"] == true { " [public]" } else { "" })?;
                }
            }
            BundleCmd::Install { id, dir } => {
                let r = c.get(&format!("/v1/bundles/{id}/skill.md")).await?;
                if !(200..300).contains(&r.status) {
                    check(&r)?;
                }
                let target = dir.join(&id);
                std::fs::create_dir_all(&target)?;
                std::fs::write(target.join("SKILL.md"), &r.text)?;
                writeln!(io.out, "Installed {}", target.join("SKILL.md").display())?;
            }
        },
        Cmd::Project(cmd) => {
            let body = match cmd {
                ProjectCmd::Show => {
                    let r = c.get("/v1/project").await?;
                    writeln!(io.out, "{}", pretty(&check(&r)?["data"]))?;
                    return Ok(0);
                }
                ProjectCmd::Cap { monthly, daily } => {
                    let conv = |v: &Option<String>| -> anyhow::Result<Option<Value>> {
                        Ok(match v.as_deref() {
                            None => None,
                            Some("off") | Some("none") => Some(Value::Null),
                            Some(x) => Some(json!(parse_usd(x)?)),
                        })
                    };
                    let mut b = json!({});
                    if let Some(m) = conv(&monthly)? {
                        b["monthly_cap_micros"] = m;
                    }
                    if let Some(d) = conv(&daily)? {
                        b["daily_cap_micros"] = d;
                    }
                    b
                }
                ProjectCmd::Pin { ids } => json!({ "pinned_tools": ids }),
                ProjectCmd::Policy { policy } => json!({ "routing_policy": policy }),
            };
            let r = c.request(Method::PATCH, "/v1/project", Some(&body), &[]).await?;
            let d = &check(&r)?["data"];
            writeln!(
                io.out,
                "Project {}: monthly {} · daily {} · policy {} · pinned {}",
                d["name"].as_str().unwrap_or(""),
                d["monthly_cap_micros"].as_i64().map(usd).unwrap_or_else(|| "none".into()),
                d["daily_cap_micros"].as_i64().map(usd).unwrap_or_else(|| "none".into()),
                d["routing_policy"].as_str().unwrap_or(""),
                d["pinned_tools"]
            )?;
        }
        Cmd::Mcp => {
            let mut line = String::new();
            loop {
                line.clear();
                if io.stdin.read_line(&mut line)? == 0 {
                    break;
                }
                let msg = line.trim();
                if msg.is_empty() {
                    continue;
                }
                match c.mcp(msg).await {
                    Ok(Some(resp)) => {
                        writeln!(io.out, "{}", resp.trim())?;
                        io.out.flush()?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let id = serde_json::from_str::<Value>(msg).ok().and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null);
                        writeln!(io.out, "{}", json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": e.to_string() } }))?;
                        io.out.flush()?;
                    }
                }
            }
        }
    }
    Ok(0)
}
