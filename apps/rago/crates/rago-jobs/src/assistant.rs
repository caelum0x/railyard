//! Finance assistant — answers billing questions with an OpenAI-compatible
//! chat-completions endpoint (OpenAI, or the Railyard Relay LLM gateway),
//! grounded in the organization's live billing figures.
//!
//! Env: `RAGO_ASSISTANT_API_KEY` (required), `RAGO_ASSISTANT_BASE_URL`
//! (default `https://api.openai.com/v1`), `RAGO_ASSISTANT_MODEL`
//! (default `gpt-4o-mini`). Without a key the assistant reports
//! [`JobError::NotConfigured`] — it never fabricates an answer.

use rago_db::Db;
use rago_integrations::HttpTransport;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::JobError;

/// Whether an assistant API key is configured.
pub fn is_configured() -> bool {
    std::env::var("RAGO_ASSISTANT_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false)
}

fn points(label: &str, rows: &[rago_db::AnalyticsPoint], last: usize) -> String {
    let start = rows.len().saturating_sub(last);
    let items: Vec<String> = rows[start..]
        .iter()
        .map(|p| format!("{} {} {:.2} ({} items)", p.month, p.currency, p.amount_cents as f64 / 100.0, p.count))
        .collect();
    format!("{label}: {}", if items.is_empty() { "none".to_string() } else { items.join("; ") })
}

/// The organization's billing snapshot used to ground answers.
async fn context(db: &Db, org: Uuid) -> Result<String, JobError> {
    let mrr = db.mrrs(org).await?;
    let revenue = db.gross_revenues(org).await?;
    let overdue = db.overdue_balances(org).await?;
    let customers = db.count_customers(org).await?;
    Ok([
        format!("Customers: {customers}"),
        points("Monthly recurring revenue by month", &mrr, 12),
        points("Gross revenue by month", &revenue, 12),
        points("Overdue balances", &overdue, 12),
    ]
    .join("\n"))
}

/// Answer the conversation (`[(role, content)]`, oldest first; the last entry
/// is the user's question). Returns the assistant's reply.
pub async fn answer<T: HttpTransport>(
    db: &Db,
    transport: &T,
    org: Uuid,
    history: &[(String, String)],
) -> Result<String, JobError> {
    let key = std::env::var("RAGO_ASSISTANT_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or(JobError::NotConfigured("RAGO_ASSISTANT_API_KEY"))?;
    let base = std::env::var("RAGO_ASSISTANT_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("RAGO_ASSISTANT_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let snapshot = context(db, org).await?;

    let mut messages = vec![json!({
        "role": "system",
        "content": format!(
            "You are the finance assistant of a usage-based billing system. Answer concisely using \
             only the figures below; say so when the data does not answer the question. Amounts \
             are in major currency units.\n\n{snapshot}"
        ),
    })];
    for (role, content) in history.iter().rev().take(20).rev() {
        let role = if role == "assistant" { "assistant" } else { "user" };
        messages.push(json!({ "role": role, "content": content }));
    }
    let body = json!({ "model": model, "messages": messages, "temperature": 0.2 }).to_string();
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {key}")),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let resp = transport
        .post(&url, &headers, body)
        .await
        .map_err(|e| JobError::Integration(e.to_string()))?;
    let json: Value = serde_json::from_str(&resp.body)
        .map_err(|e| JobError::Integration(format!("invalid assistant response: {e}")))?;
    if !resp.is_success() {
        let msg = json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("assistant request failed");
        return Err(JobError::Integration(msg.to_string()));
    }
    json.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JobError::Integration("empty assistant response".into()))
}
