//! Native adapters for our own products. We own both sides, so these are
//! typed clients against the products' real routes:
//!
//! - Relay: `POST {relay_url}/openai/v1/chat/completions` with the caller's key;
//! - Scribase control API: `.../environments/{env}/database/query`,
//!   `.../projects/{p}/branches`; project auth: `POST {project}/auth/v1/magiclink`;
//! - Rally: `/api/rally/v1/search/{companies,people,companies-by-domains}`,
//!   `/api/rally/v1/send`;
//! - Menivor: `POST /api/v1/videos`, `GET /api/v1/tasks/{id}`, `POST /api/v1/audio`.

use secrecy::ExposeSecret;
use serde_json::{json, Value};
use switchyard_core::{AdapterError, AdapterOutput, CallCtx, HttpRequest};

use crate::util::{config_url, first_array, host_of, pick, send, send_json, str_field, unwrap_envelope, with_bearer};

fn seg(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ------------------------------------------------------------------ Relay

pub async fn relay_chat(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let base = config_url(ctx, "relay_url")?;
    let key = ctx.caller_key.as_ref().ok_or_else(|| AdapterError::fatal("relay.chat.complete needs a Relay sk-t0 key (OAuth tokens cannot call models yet)"))?;
    let body = pick(input, &[("model", "model"), ("messages", "messages"), ("max_tokens", "max_tokens"), ("temperature", "temperature")]);
    let req = HttpRequest::post_json(format!("{base}/openai/v1/chat/completions"), &body).secret_header("authorization", format!("Bearer {}", key.expose_secret()));
    let (status, v) = send_json(ctx, req, false).await?;
    Ok(AdapterOutput {
        data: json!({ "content": v.pointer("/choices/0/message/content"), "model": v.get("model"), "usage": v.get("usage") }),
        status,
        units: None,
        reported_cost_micros: None,
    })
}

// --------------------------------------------------------------- Scribase

pub async fn scribase_query(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let base = config_url(ctx, "scribase_url")?;
    let (org, project) = (str_field(input, "organization")?, str_field(input, "project")?);
    let env = input.get("environment").and_then(Value::as_str).unwrap_or("production");
    let body = json!({ "query": str_field(input, "query")?, "limit": input.get("limit").and_then(Value::as_u64).unwrap_or(100) });
    let url = format!("{base}/v1/organizations/{}/projects/{}/environments/{}/database/query", seg(org), seg(project), seg(env));
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(url, &body))?, false).await?;
    let v = unwrap_envelope(v);
    let data = if v.get("rows").is_some() { v } else { json!({ "columns": null, "rows": v }) };
    Ok(AdapterOutput { data, status, units: None, reported_cost_micros: None })
}

pub async fn scribase_branch(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let base = config_url(ctx, "scribase_url")?;
    let (org, project) = (str_field(input, "organization")?, str_field(input, "project")?);
    let body = json!({
        "environment_id": str_field(input, "environment_id")?,
        "base_environment_id": input.get("base_environment_id").and_then(Value::as_str).unwrap_or("production"),
    });
    let url = format!("{base}/v1/organizations/{}/projects/{}/branches", seg(org), seg(project));
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(url, &body))?, false).await?;
    Ok(AdapterOutput { data: unwrap_envelope(v), status, units: None, reported_cost_micros: None })
}

pub async fn scribase_magic_link(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let project_url = str_field(input, "project_url")?.trim_end_matches('/');
    let host = host_of(project_url)?;
    // Only project hosts under our Scribase domain, when one is configured.
    if let Some(domain) = ctx.config("scribase_project_domain") {
        if !(host == domain || host.ends_with(&format!(".{domain}"))) {
            return Err(AdapterError::rejected(format!("project_url must be under {domain}")));
        }
    }
    let anon = ctx.credential.as_ref().ok_or_else(|| AdapterError::fatal("store the project's anon key as a `scribase` credential"))?;
    let mut url = url::Url::parse(&format!("{project_url}/auth/v1/magiclink")).map_err(|e| AdapterError::rejected(e.to_string()))?;
    if let Some(r) = input.get("redirect_to").and_then(Value::as_str) {
        url.query_pairs_mut().append_pair("redirect_to", r);
    }
    let mut req = HttpRequest::post_json(url.to_string(), &json!({ "email": str_field(input, "email")? })).secret_header("apikey", anon.expose());
    req.extra_hosts = vec![host];
    let resp = send(ctx, req).await?;
    if !(200..300).contains(&resp.status) {
        return Err(AdapterError::from_status(resp.status, &resp.text(), false));
    }
    Ok(AdapterOutput { data: json!({ "sent": true }), status: resp.status, units: Some(1), reported_cost_micros: None })
}

// ------------------------------------------------------------------ Rally

fn rally(ctx: &CallCtx, path: &str) -> Result<String, AdapterError> {
    Ok(format!("{}/api/rally/v1{path}", config_url(ctx, "rally_url")?))
}

pub async fn rally_leads(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(25);
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("companies");
    let filters = input.get("filters").cloned().unwrap_or(json!({}));
    let domains = input.get("domains").cloned();
    let (path, body) = match (kind, &domains) {
        ("companies", Some(d)) => ("/search/companies-by-domains", json!({ "domains": d })),
        ("companies", None) => ("/search/companies", json!({ "filters": filters, "page": 1, "pageSize": limit })),
        _ => ("/search/people", json!({ "peopleFilters": filters, "domains": domains, "page": 1, "pageSize": limit })),
    };
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(rally(ctx, path)?, &body))?, false).await?;
    let data = unwrap_envelope(v);
    let mut results = first_array(&data, &["results", "companies", "people", "items", "rows"]);
    results.truncate(limit as usize);
    let n = results.len() as u64;
    let total = data.get("total").or_else(|| data.get("totalCount")).cloned().unwrap_or(json!(n));
    Ok(AdapterOutput { data: json!({ "results": results, "total": total }), status, units: Some(n), reported_cost_micros: None })
}

pub async fn rally_enrich(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let domain = str_field(input, "domain")?.to_ascii_lowercase();
    let body = json!({ "domains": [domain] });
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(rally(ctx, "/search/companies-by-domains")?, &body))?, false).await?;
    let found = first_array(&unwrap_envelope(v), &["results", "companies", "items"]).into_iter().next();
    // Per-success pricing: a miss is free.
    let cost = if found.is_none() { Some(0) } else { None };
    Ok(AdapterOutput { data: json!({ "domain": domain, "company": found }), status, units: None, reported_cost_micros: cost })
}

pub async fn rally_send(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let body = pick(
        input,
        &[
            ("from", "from"),
            ("to", "to"),
            ("subject", "subject"),
            ("html", "html"),
            ("text", "text"),
            ("reply_to", "replyTo"),
            ("template_id", "templateId"),
            ("variables", "variables"),
        ],
    );
    let key = ctx.idempotency_key.clone().unwrap_or_else(|| ctx.call_id.clone());
    let req = with_bearer(ctx, HttpRequest::post_json(rally(ctx, "/send")?, &body).header("idempotency-key", format!("switchyard:{}:{key}", ctx.project_id)))?;
    let (status, v) = send_json(ctx, req, false).await?;
    let recipients = match input.get("to") {
        Some(Value::Array(a)) => a.len() as u64,
        _ => 1,
    };
    Ok(AdapterOutput { data: unwrap_envelope(v), status, units: Some(recipients), reported_cost_micros: None })
}

const DISPOSABLE: &[&str] = &[
    "mailinator.com", "guerrillamail.com", "10minutemail.com", "tempmail.com", "temp-mail.org", "yopmail.com", "trashmail.com",
    "sharklasers.com", "getnada.com", "dispostable.com", "maildrop.cc", "throwawaymail.com", "fakeinbox.com", "mintemail.com",
];
const ROLES: &[&str] = &["admin", "info", "sales", "support", "contact", "hello", "noreply", "no-reply", "office", "team", "billing", "abuse", "postmaster", "webmaster", "marketing", "jobs", "careers"];

pub fn email_syntax(email: &str) -> Option<(&str, &str)> {
    let (local, domain) = email.rsplit_once('@')?;
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().map(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic())).unwrap_or(false);
    (local_ok && domain_ok).then_some((local, domain))
}

pub async fn email_verify(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let email = str_field(input, "email")?.trim().to_ascii_lowercase();
    let Some((local, domain)) = email_syntax(&email) else {
        return Ok(AdapterOutput::ok(json!({ "email": email, "result": "undeliverable", "checks": { "syntax": false, "mx": false, "disposable": false, "role": false } })));
    };
    let mut u = url::Url::parse("https://cloudflare-dns.com/dns-query").expect("static url");
    u.query_pairs_mut().append_pair("name", domain).append_pair("type", "MX");
    let (_, dns) = send_json(ctx, HttpRequest::get(u.to_string()).header("accept", "application/dns-json"), false).await?;
    let mx: Vec<String> = dns
        .get("Answer")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|r| r.get("type").and_then(Value::as_u64) == Some(15)).filter_map(|r| r.get("data").and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default();
    // "0 ." is a null MX (RFC 7505): the domain accepts no mail.
    let has_mx = mx.iter().any(|m| m.trim() != "0 .");
    let disposable = DISPOSABLE.contains(&domain);
    let role = ROLES.contains(&local.split('+').next().unwrap_or(local));
    let result = if !has_mx { "undeliverable" } else if disposable || role { "risky" } else { "deliverable" };
    Ok(AdapterOutput::ok(json!({
        "email": email,
        "result": result,
        "checks": { "syntax": true, "mx": has_mx, "mx_hosts": mx, "disposable": disposable, "role": role, "smtp_probe": "not_performed" },
    })))
}

// ---------------------------------------------------------------- Menivor

fn menivor(ctx: &CallCtx, path: &str) -> Result<String, AdapterError> {
    Ok(format!("{}/api/v1{path}", config_url(ctx, "menivor_url")?))
}

fn task_id(v: &Value) -> Option<String> {
    ["/data/task_id", "/task_id", "/data/id", "/id"].iter().find_map(|p| v.pointer(p).and_then(Value::as_str)).map(str::to_string)
}

pub async fn menivor_render(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let mut body = pick(input, &[("subject", "video_subject"), ("script", "video_script"), ("aspect", "video_aspect"), ("voice", "voice_name"), ("language", "video_language")]);
    let seconds = input.get("duration_s").and_then(Value::as_u64).unwrap_or(30);
    body["video_clip_duration"] = json!(5);
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(menivor(ctx, "/videos")?, &body))?, false).await?;
    let id = task_id(&v).ok_or_else(|| AdapterError::retryable("Menivor did not return a task id"))?;
    Ok(AdapterOutput { data: json!({ "job_id": id, "status": "queued", "poll": "menivor.video.status" }), status, units: Some(seconds), reported_cost_micros: None })
}

pub async fn menivor_status(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let id = str_field(input, "job_id")?;
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::get(menivor(ctx, &format!("/tasks/{}", seg(id)))?))?, false).await?;
    let d = unwrap_envelope(v);
    Ok(AdapterOutput {
        data: json!({ "job_id": id, "state": d.get("state"), "progress": d.get("progress"), "videos": d.get("videos").or_else(|| d.get("combined_videos")) }),
        status,
        units: None,
        reported_cost_micros: None,
    })
}

pub async fn menivor_tts(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let text = str_field(input, "text")?;
    let body = pick(input, &[("text", "video_script"), ("voice", "voice_name"), ("rate", "voice_rate"), ("language", "video_language")]);
    let (status, v) = send_json(ctx, with_bearer(ctx, HttpRequest::post_json(menivor(ctx, "/audio")?, &body))?, false).await?;
    let kchars = (text.chars().count() as u64).div_ceil(1000);
    Ok(AdapterOutput {
        data: json!({ "job_id": task_id(&v), "audio_url": v.pointer("/data/audio_url").or_else(|| v.pointer("/data/audio_file")) }),
        status,
        units: Some(kchars.max(1)),
        reported_cost_micros: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_syntax_rules() {
        assert!(email_syntax("founder@stripe.com").is_some());
        assert!(email_syntax("a.b+tag@sub.example.co").is_some());
        for bad in ["", "no-at", "@x.com", "a@", "a..b@x.com", ".a@x.com", "a@x", "a@-x.com", "a@x.c0m", "a b@x.com"] {
            assert!(email_syntax(bad).is_none(), "{bad}");
        }
    }
}
