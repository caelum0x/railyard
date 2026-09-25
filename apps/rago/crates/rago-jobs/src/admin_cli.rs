//! Operator bootstrap commands for `rago-worker`: create an organization and
//! mint API keys without hand-written SQL.
//!
//! ```text
//! rago-worker create-org --name "AutoGTM" [--slug autogtm] [--currency USD]
//!                        [--email billing@example.com] [--key-name default]
//! rago-worker create-api-key --org <org-uuid|slug> [--name relay]
//! ```
//!
//! Both commands print the new bearer value once on stdout as `API_KEY=...`
//! (plus the ids), so a deploy script can capture it into the consumer's
//! secret store. Key values use the same `rago_<32 hex>` shape the GraphQL
//! `createApiKey` / `rotateApiKey` mutations mint.

use rago_db::Db;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

/// Longest organization name / slug / key name accepted (the columns are
/// unbounded `varchar`, this is a sanity cap for operator input).
const MAX_TEXT_LEN: usize = 255;
/// Default name of the key minted alongside a new organization.
const DEFAULT_KEY_NAME: &str = "default";
/// Default billing currency for a new organization.
const DEFAULT_CURRENCY: &str = "USD";

/// A parsed operator command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminCommand {
    CreateOrg(CreateOrgArgs),
    CreateApiKey(CreateApiKeyArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOrgArgs {
    pub name: String,
    pub slug: String,
    pub currency: String,
    pub email: Option<String>,
    pub key_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateApiKeyArgs {
    /// Organization id (UUID) or slug.
    pub org: String,
    pub name: Option<String>,
}

/// The rows a command created. `api_key_value` is the bearer secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedKey {
    pub organization_id: Uuid,
    pub organization_slug: String,
    pub api_key_id: Uuid,
    pub api_key_value: String,
    pub api_key_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum AdminError {
    #[error("{0}")]
    Usage(String),
    #[error("an organization with slug `{0}` already exists")]
    SlugTaken(String),
    #[error("no organization matches `{0}` (pass its id or slug)")]
    OrgNotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Usage text shown for `--help` and on argument errors.
pub const USAGE: &str = "\
usage:
  rago-worker create-org --name <NAME> [--slug <SLUG>] [--currency <ISO>] [--email <EMAIL>] [--key-name <NAME>]
  rago-worker create-api-key --org <ORG_ID|SLUG> [--name <NAME>]

Both commands print the new API key once (API_KEY=...). Store it in the
consumer's secret store. Other jobs are selected with RAGO_CMD (see source).";

/// Parse `argv[1..]`. `Ok(None)` when no admin subcommand was given (the
/// worker then falls back to `RAGO_CMD`).
pub fn parse_args(args: &[String]) -> Result<Option<AdminCommand>, AdminError> {
    let Some((cmd, rest)) = args.split_first() else { return Ok(None) };
    match cmd.as_str() {
        "create-org" => parse_create_org(rest).map(|a| Some(AdminCommand::CreateOrg(a))),
        "create-api-key" => parse_create_api_key(rest).map(|a| Some(AdminCommand::CreateApiKey(a))),
        "-h" | "--help" | "help" => Err(AdminError::Usage(USAGE.into())),
        other => Err(AdminError::Usage(format!("unknown command `{other}`\n\n{USAGE}"))),
    }
}

/// Collect `--flag value` pairs, rejecting unknown and duplicate flags.
fn flags<'a>(rest: &'a [String], allowed: &[&str]) -> Result<Vec<(&'a str, &'a str)>, AdminError> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let (key, inline) = match flag.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v)),
            _ => (flag.as_str(), None),
        };
        if !allowed.contains(&key) {
            return Err(AdminError::Usage(format!("unknown option `{key}`\n\n{USAGE}")));
        }
        let value = match inline {
            Some(v) => v,
            None => it
                .next()
                .map(String::as_str)
                .ok_or_else(|| AdminError::Usage(format!("`{key}` needs a value")))?,
        };
        if out.iter().any(|(k, _)| *k == key) {
            return Err(AdminError::Usage(format!("`{key}` given twice")));
        }
        out.push((key, value));
    }
    Ok(out)
}

fn get<'a>(pairs: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn clean_text(label: &str, raw: &str) -> Result<String, AdminError> {
    let v = raw.trim();
    if v.is_empty() {
        return Err(AdminError::Usage(format!("{label} must not be empty")));
    }
    if v.chars().count() > MAX_TEXT_LEN {
        return Err(AdminError::Usage(format!("{label} is longer than {MAX_TEXT_LEN} characters")));
    }
    if v.chars().any(char::is_control) {
        return Err(AdminError::Usage(format!("{label} must not contain control characters")));
    }
    Ok(v.to_string())
}

/// Derive a URL-safe slug: lowercase ASCII letters/digits, other runs → `-`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TEXT_LEN
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn parse_create_org(rest: &[String]) -> Result<CreateOrgArgs, AdminError> {
    let pairs = flags(rest, &["--name", "--slug", "--currency", "--email", "--key-name"])?;
    let name = clean_text("--name", get(&pairs, "--name").ok_or_else(|| AdminError::Usage(format!("create-org needs --name\n\n{USAGE}")))?)?;
    let slug = match get(&pairs, "--slug") {
        Some(s) => s.trim().to_string(),
        None => slugify(&name),
    };
    if !valid_slug(&slug) {
        return Err(AdminError::Usage(format!(
            "slug `{slug}` is invalid: use lowercase letters, digits and inner dashes (pass --slug)"
        )));
    }
    let currency = get(&pairs, "--currency").unwrap_or(DEFAULT_CURRENCY).trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(AdminError::Usage(format!("--currency `{currency}` is not a 3-letter ISO 4217 code")));
    }
    let email = match get(&pairs, "--email") {
        Some(e) => {
            let e = clean_text("--email", e)?;
            let valid = e.split_once('@').is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
            if !valid {
                return Err(AdminError::Usage(format!("--email `{e}` is not an email address")));
            }
            Some(e)
        }
        None => None,
    };
    let key_name = clean_text("--key-name", get(&pairs, "--key-name").unwrap_or(DEFAULT_KEY_NAME))?;
    Ok(CreateOrgArgs { name, slug, currency, email, key_name })
}

fn parse_create_api_key(rest: &[String]) -> Result<CreateApiKeyArgs, AdminError> {
    let pairs = flags(rest, &["--org", "--name"])?;
    let org = clean_text("--org", get(&pairs, "--org").ok_or_else(|| AdminError::Usage(format!("create-api-key needs --org\n\n{USAGE}")))?)?;
    let name = get(&pairs, "--name").map(|n| clean_text("--name", n)).transpose()?;
    Ok(CreateApiKeyArgs { org, name })
}

/// A fresh bearer value (`rago_` + 122 random bits, the GraphQL mint format).
pub fn generate_api_key_value() -> String {
    format!("rago_{}", Uuid::new_v4().simple())
}

async fn insert_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: Uuid,
    name: Option<&str>,
) -> Result<(Uuid, String), sqlx::Error> {
    let id = Uuid::new_v4();
    let value = generate_api_key_value();
    sqlx::query(
        "INSERT INTO api_keys (id, organization_id, value, name, permissions, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,'{}',now(),now())",
    )
    .bind(id)
    .bind(org)
    .bind(&value)
    .bind(name)
    .execute(&mut **tx)
    .await?;
    Ok((id, value))
}

/// Create an organization and its first API key in one transaction.
pub async fn create_org(db: &Db, args: &CreateOrgArgs) -> Result<CreatedKey, AdminError> {
    let mut tx = db.pool.begin().await?;
    // Serialize concurrent creates of the same slug (no unique index on slug).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rago.create_org:' || $1))")
        .bind(&args.slug)
        .execute(&mut *tx)
        .await?;
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM organizations WHERE slug = $1)")
        .bind(&args.slug)
        .fetch_one(&mut *tx)
        .await?;
    if taken {
        return Err(AdminError::SlugTaken(args.slug.clone()));
    }
    let org_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO organizations (id, name, slug, default_currency, email, hmac_key, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
    )
    .bind(org_id)
    .bind(&args.name)
    .bind(&args.slug)
    .bind(&args.currency)
    .bind(args.email.as_deref())
    .bind(Uuid::new_v4().to_string())
    .execute(&mut *tx)
    .await?;
    let (key_id, value) = insert_key(&mut tx, org_id, Some(&args.key_name)).await?;
    tx.commit().await?;
    Ok(CreatedKey {
        organization_id: org_id,
        organization_slug: args.slug.clone(),
        api_key_id: key_id,
        api_key_value: value,
        api_key_name: Some(args.key_name.clone()),
    })
}

/// Mint an additional API key for an existing organization (by id or slug).
pub async fn create_api_key(db: &Db, args: &CreateApiKeyArgs) -> Result<CreatedKey, AdminError> {
    let row = match Uuid::parse_str(&args.org) {
        Ok(id) => sqlx::query("SELECT id, slug FROM organizations WHERE id = $1").bind(id).fetch_optional(&db.pool).await?,
        Err(_) => {
            let rows = sqlx::query("SELECT id, slug FROM organizations WHERE slug = $1 LIMIT 2")
                .bind(&args.org)
                .fetch_all(&db.pool)
                .await?;
            if rows.len() > 1 {
                return Err(AdminError::Usage(format!("slug `{}` matches several organizations; pass the org id", args.org)));
            }
            rows.into_iter().next()
        }
    };
    let row = row.ok_or_else(|| AdminError::OrgNotFound(args.org.clone()))?;
    let org_id: Uuid = row.get("id");
    let slug: String = row.get("slug");
    let mut tx = db.pool.begin().await?;
    let (key_id, value) = insert_key(&mut tx, org_id, args.name.as_deref()).await?;
    tx.commit().await?;
    Ok(CreatedKey {
        organization_id: org_id,
        organization_slug: slug,
        api_key_id: key_id,
        api_key_value: value,
        api_key_name: args.name.clone(),
    })
}

/// Run a parsed command.
pub async fn run(db: &Db, cmd: &AdminCommand) -> Result<CreatedKey, AdminError> {
    match cmd {
        AdminCommand::CreateOrg(a) => create_org(db, a).await,
        AdminCommand::CreateApiKey(a) => create_api_key(db, a).await,
    }
}

/// The stdout block for a created key (`KEY=value` lines, shell-friendly).
pub fn render(created: &CreatedKey) -> String {
    format!(
        "ORGANIZATION_ID={}\nORGANIZATION_SLUG={}\nAPI_KEY_ID={}\nAPI_KEY_NAME={}\nAPI_KEY={}\n",
        created.organization_id,
        created.organization_slug,
        created.api_key_id,
        created.api_key_name.as_deref().unwrap_or(""),
        created.api_key_value,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_subcommand_falls_back() {
        assert_eq!(parse_args(&[]).unwrap(), None);
    }

    #[test]
    fn create_org_defaults() {
        let cmd = parse_args(&args(&["create-org", "--name", "Auto GTM!"])).unwrap().unwrap();
        assert_eq!(
            cmd,
            AdminCommand::CreateOrg(CreateOrgArgs {
                name: "Auto GTM!".into(),
                slug: "auto-gtm".into(),
                currency: "USD".into(),
                email: None,
                key_name: "default".into(),
            })
        );
    }

    #[test]
    fn create_org_all_flags_and_inline_values() {
        let cmd = parse_args(&args(&[
            "create-org", "--name=OpenJev", "--slug", "openjev", "--currency", "eur", "--email", "ops@openjev.dev", "--key-name", "relay",
        ]))
        .unwrap()
        .unwrap();
        let AdminCommand::CreateOrg(a) = cmd else { panic!("wrong command") };
        assert_eq!(a.slug, "openjev");
        assert_eq!(a.currency, "EUR");
        assert_eq!(a.email.as_deref(), Some("ops@openjev.dev"));
        assert_eq!(a.key_name, "relay");
    }

    #[test]
    fn create_org_rejects_bad_input() {
        for bad in [
            vec!["create-org"],
            vec!["create-org", "--name"],
            vec!["create-org", "--name", "   "],
            vec!["create-org", "--name", "!!!"],
            vec!["create-org", "--name", "X", "--slug", "Bad Slug"],
            vec!["create-org", "--name", "X", "--currency", "dollars"],
            vec!["create-org", "--name", "X", "--email", "nope"],
            vec!["create-org", "--name", "X", "--name", "Y"],
            vec!["create-org", "--name", "X", "--bogus", "1"],
        ] {
            assert!(matches!(parse_args(&args(&bad)), Err(AdminError::Usage(_))), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn create_api_key_parses() {
        let cmd = parse_args(&args(&["create-api-key", "--org", "autogtm", "--name", "relay"])).unwrap().unwrap();
        assert_eq!(cmd, AdminCommand::CreateApiKey(CreateApiKeyArgs { org: "autogtm".into(), name: Some("relay".into()) }));
        assert!(matches!(parse_args(&args(&["create-api-key"])), Err(AdminError::Usage(_))));
        assert!(matches!(parse_args(&args(&["frobnicate"])), Err(AdminError::Usage(_))));
    }

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("  Rally / AutoGTM  "), "rally-autogtm");
        assert_eq!(slugify("Menivor"), "menivor");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn key_values_are_unique_and_prefixed() {
        let a = generate_api_key_value();
        let b = generate_api_key_value();
        assert!(a.starts_with("rago_") && a.len() == 37);
        assert_ne!(a, b);
    }

    #[test]
    fn render_prints_the_key_once() {
        let out = render(&CreatedKey {
            organization_id: Uuid::nil(),
            organization_slug: "acme".into(),
            api_key_id: Uuid::nil(),
            api_key_value: "rago_x".into(),
            api_key_name: None,
        });
        assert_eq!(out.matches("rago_x").count(), 1);
        assert!(out.contains("ORGANIZATION_SLUG=acme\n"));
    }
}
