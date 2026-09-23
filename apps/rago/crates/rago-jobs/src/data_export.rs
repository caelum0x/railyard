//! Asynchronous data-export job — the worker side of `DataExports::*`.
//!
//! A `data_exports` row is enqueued as a `data_export` job; the worker builds a
//! CSV of the requested resource (invoices / events / customers) and writes it
//! back to the row (`content` + `row_count` + `status = completed`), from which
//! it is downloadable.

use rago_db::Db;
use uuid::Uuid;

use crate::JobError;

/// Job type: build a CSV data export.
pub const JOB_DATA_EXPORT: &str = "data_export";

/// A single CSV cell, escaped per RFC 4180 (quote when it contains `,`, `"`, or
/// a newline; double embedded quotes).
fn csv_cell(value: &str) -> String {
    if value.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn csv_row(cells: &[String]) -> String {
    cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(",")
}

const EXPORT_LIMIT: i64 = 100_000;

/// Accept the dashboard's singular / Lago-style names (`invoice`, `customer`,
/// `credit_note`, `event`) as well as the plural REST names.
fn normalize_resource_type(r: &str) -> &str {
    match r {
        "invoice" => "invoices",
        "customer" => "customers",
        "event" => "events",
        "credit_note" => "credit_notes",
        other => other,
    }
}

/// Build the CSV for `resource_type` and return `(csv, data_row_count)`.
pub async fn build_csv(db: &Db, org: Uuid, resource_type: &str) -> Result<(String, i32), JobError> {
    let resource_type = normalize_resource_type(resource_type);
    let mut lines: Vec<String> = Vec::new();
    let count;
    match resource_type {
        "invoices" => {
            lines.push(csv_row(&[
                "number".into(), "currency".into(), "sub_total_excluding_taxes_amount_cents".into(),
                "taxes_amount_cents".into(), "total_amount_cents".into(), "status".into(),
                "payment_status".into(), "issuing_date".into(),
            ]));
            let invoices = db.all_invoices(org, EXPORT_LIMIT).await?;
            count = invoices.len() as i32;
            for inv in &invoices {
                lines.push(csv_row(&[
                    inv.number.clone(),
                    inv.currency.clone().unwrap_or_default(),
                    inv.sub_total_excluding_taxes_amount_cents.to_string(),
                    inv.taxes_amount_cents.to_string(),
                    inv.total_amount_cents.to_string(),
                    inv.status.as_str().to_string(),
                    inv.payment_status.as_str().to_string(),
                    inv.issuing_date.map(|d| d.to_string()).unwrap_or_default(),
                ]));
            }
        }
        "customers" => {
            lines.push(csv_row(&[
                "external_id".into(), "name".into(), "email".into(),
                "country".into(), "currency".into(),
            ]));
            let customers = db.list_customers(org, EXPORT_LIMIT, 0).await?;
            count = customers.len() as i32;
            for c in &customers {
                lines.push(csv_row(&[
                    c.external_id.clone(),
                    c.name.clone().unwrap_or_default(),
                    c.email.clone().unwrap_or_default(),
                    c.country.clone().unwrap_or_default(),
                    c.currency.clone().unwrap_or_default(),
                ]));
            }
        }
        "events" => {
            lines.push(csv_row(&[
                "transaction_id".into(), "code".into(), "external_subscription_id".into(),
                "timestamp".into(),
            ]));
            let events = db.all_events(org, EXPORT_LIMIT).await?;
            count = events.len() as i32;
            for e in &events {
                lines.push(csv_row(&[
                    e.transaction_id.clone(),
                    e.code.clone(),
                    e.external_subscription_id.clone().unwrap_or_default(),
                    e.timestamp.map(|t| t.to_string()).unwrap_or_default(),
                ]));
            }
        }
        "credit_notes" => {
            lines.push(csv_row(&[
                "number".into(), "invoice_id".into(), "currency".into(),
                "credit_amount_cents".into(), "refund_amount_cents".into(),
                "total_amount_cents".into(), "issuing_date".into(),
            ]));
            let notes = db.credit_notes_for_org(org, EXPORT_LIMIT, 0).await?;
            count = notes.len() as i32;
            for cn in &notes {
                lines.push(csv_row(&[
                    cn.number.clone(),
                    cn.invoice_id.to_string(),
                    cn.total_amount_currency.clone(),
                    cn.credit_amount_cents.to_string(),
                    cn.refund_amount_cents.to_string(),
                    cn.total_amount_cents.to_string(),
                    cn.issuing_date.to_string(),
                ]));
            }
        }
        other => return Err(JobError::Integration(format!("unknown export resource_type: {other}"))),
    }
    // Trailing newline (POSIX text file).
    let mut csv = lines.join("\n");
    csv.push('\n');
    Ok((csv, count))
}

/// Build the CSV of a finance-assistant session (`filters.session_id`).
pub async fn build_finance_assistant_csv(db: &Db, org: Uuid, filters: &serde_json::Value) -> Result<(String, i32), JobError> {
    let session = filters
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Integration("finance_assistant export requires filters.session_id".into()))?;
    let rows = db.finance_assistant_session(org, session).await?;
    let mut lines = vec![csv_row(&["asked_at".into(), "question".into(), "answer".into(), "status".into()])];
    for (q, a, status, at) in &rows {
        lines.push(csv_row(&[at.to_string(), q.clone(), a.clone().unwrap_or_default(), status.clone()]));
    }
    let mut csv = lines.join("\n");
    csv.push('\n');
    Ok((csv, rows.len() as i32))
}

/// Process one enqueued export: build the CSV and store it on the row.
pub async fn process(db: &Db, export_id: Uuid, org: Uuid, resource_type: &str) -> Result<(), JobError> {
    let (csv, rows) = if resource_type == "finance_assistant" {
        let filters = db.data_export_filters(export_id).await?;
        build_finance_assistant_csv(db, org, &filters).await?
    } else {
        build_csv(db, org, resource_type).await?
    };
    db.complete_data_export(export_id, &csv, rows).await?;
    Ok(())
}
