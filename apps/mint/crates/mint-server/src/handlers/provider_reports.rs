//! Reconciliation against the provider's OWN books.
//!
//! `GET /v1/reconciliation` compares Mint's reserve movements with Mint's own
//! payout table. This module closes the loop with the other side: an operator
//! imports a provider report (Stripe itemized balance / payouts export, PayPal
//! settlement report, an on-chain USDC transfer list, … as CSV or JSON), the
//! lines are parsed and stored, and every read re-matches them against the
//! live ledger:
//!
//! - `matched`             — same reference, same amount;
//! - `amount_mismatch`     — same reference, different amount;
//! - `missing_in_mint`     — the provider moved money Mint never recorded;
//! - `reversal_unrecorded` — a provider refund / chargeback of a deposit Mint
//!   minted but has not clawed back yet (actionable from the console);
//! - `status_mismatch`     — a provider-paid payout Mint still shows unpaid;
//! - `missing_at_provider` — a Mint movement inside the report period that the
//!   provider report does not contain.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_store::{
    provider_tag_of, LedgerStore, PayoutQuery, PayoutRecord, PayoutRecordStatus, ProviderReport,
    ProviderReportLine, ReportLineKind, ReserveMovement,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;

/// Maximum report lines accepted per import.
const MAX_LINES: usize = 50_000;
/// Maximum raw report size (bytes).
const MAX_CONTENT: usize = 20 * 1024 * 1024;
/// Upper bound of Mint rows scanned for a match.
const SCAN_LIMIT: usize = 200_000;
/// Slack around the report period when looking for the Mint-side movement of
/// a line (provider timestamps lag Mint's by settlement delays).
const MATCH_SLACK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Micros per major currency unit.
const MICROS_PER_UNIT: i64 = 1_000_000;

/// Column aliases, in priority order (headers are normalised to lowercase
/// snake case before lookup).
const REFERENCE_COLS: &[&str] = &[
    "reference",
    "ref",
    "deposit_ref",
    "payout_ref",
    "payment_intent_id",
    "payment_intent",
    "transaction_id",
    "tx_hash",
    "txhash",
    "hash",
    "order_id",
    "checkout_id",
    "payout_id",
    "transfer_id",
    "source",
    "id",
];
const AMOUNT_COLS: &[&str] = &[
    "amount_micros",
    "amount",
    "gross",
    "gross_amount",
    "value",
    "amount_usd",
    "total",
];
const TYPE_COLS: &[&str] = &[
    "type",
    "kind",
    "reporting_category",
    "transaction_type",
    "event",
    "event_type",
    "category",
    "description",
];
const CURRENCY_COLS: &[&str] = &["currency", "currency_code", "asset", "token"];
const DATE_COLS: &[&str] = &[
    "occurred_ms",
    "created",
    "created_utc",
    "created_at",
    "date",
    "occurred_at",
    "timestamp",
    "time",
    "block_timestamp",
    "available_on",
    "available_on_utc",
];

/// `POST /v1/reconciliation/reports` body.
#[derive(Debug, Deserialize)]
pub struct ImportReq {
    /// Provider tag (`stripe`, `paypal`, `creem`, `dodo`, `polar`, `revenuecat`,
    /// `adapty`, `usdc`, `iyzico`).
    pub provider: String,
    /// Label shown in the console (usually the file name).
    #[serde(default)]
    pub label: Option<String>,
    /// `csv` (default when the content does not start with `[`) or `json`.
    #[serde(default)]
    pub format: Option<String>,
    /// The raw report text.
    pub content: String,
    /// Unit of the amount column: `major` (default, e.g. `12.34` USD),
    /// `cents` or `micros`.
    #[serde(default)]
    pub amount_unit: Option<String>,
    /// Force a specific reference column instead of the alias heuristic.
    #[serde(default)]
    pub reference_column: Option<String>,
    /// Claw back refunds / chargebacks the report lists for deposits Mint
    /// minted but never reversed (`reversal_unrecorded`). Defaults to `true`
    /// for Iyzico, whose merchant panel refunds may predate its notification
    /// webhook, and `false` otherwise (webhooks already apply them).
    #[serde(default)]
    pub apply_reversals: Option<bool>,
}

const KNOWN_PROVIDERS: &[&str] = &[
    "stripe",
    "paypal",
    "creem",
    "dodo",
    "polar",
    "revenuecat",
    "adapty",
    "usdc",
    "iyzico",
];

#[derive(Clone, Copy)]
enum AmountUnit {
    Major,
    Cents,
    Micros,
}

fn normalise_header(h: &str) -> String {
    let mut out = String::with_capacity(h.len());
    let mut last_us = false;
    for c in h.trim().trim_start_matches('\u{feff}').chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_us = false;
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        }
    }
    out.trim_end_matches('_').to_string()
}

/// Minimal RFC 4180 CSV parser (quoted fields, `""` escapes, CRLF / LF).
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => in_quotes = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if row.iter().any(|f| !f.trim().is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// Parse a decimal amount string into micros, honouring the unit. Returns the
/// SIGNED value (the sign feeds the deposit/payout heuristic).
fn parse_amount(raw: &str, unit: AmountUnit, column: &str) -> Option<i64> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '(')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    // Accounting negatives: "(12.00)".
    let negative = cleaned.starts_with('-') || cleaned.starts_with('(');
    let digits = cleaned.trim_start_matches(['-', '(']);
    if column == "amount_micros" || matches!(unit, AmountUnit::Micros) {
        let v: i64 = digits.split('.').next()?.parse().ok()?;
        return Some(if negative { -v } else { v });
    }
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    let int_v: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let v = match unit {
        AmountUnit::Cents => int_v.checked_mul(MICROS_PER_UNIT / 100)?,
        _ => {
            let mut frac = frac_part.chars().take(6).collect::<String>();
            while frac.len() < 6 {
                frac.push('0');
            }
            let frac_v: i64 = frac.parse().ok()?;
            int_v.checked_mul(MICROS_PER_UNIT)?.checked_add(frac_v)?
        }
    };
    Some(if negative { -v } else { v })
}

/// Days since 1970-01-01 for a proleptic Gregorian civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse a report timestamp: unix seconds / ms, `YYYY-MM-DD`,
/// `YYYY-MM-DD HH:MM[:SS]` or ISO-8601 with `T` / `Z` (UTC assumed).
fn parse_date(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        return Some(if n > 100_000_000_000 { n } else { n * 1000 });
    }
    let bytes = t.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let y: i64 = t.get(0..4)?.parse().ok()?;
    let mo: i64 = t.get(5..7)?.parse().ok()?;
    let d: i64 = t.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, mo, d) * 86_400;
    if let Some(time) = t.get(11..) {
        let hh: i64 = time.get(0..2).and_then(|v| v.parse().ok()).unwrap_or(0);
        let mm: i64 = time.get(3..5).and_then(|v| v.parse().ok()).unwrap_or(0);
        let ss: i64 = time.get(6..8).and_then(|v| v.parse().ok()).unwrap_or(0);
        secs += hh * 3600 + mm * 60 + ss;
    }
    Some(secs * 1000)
}

fn classify(type_raw: Option<&str>, signed_amount: i64) -> ReportLineKind {
    let t = type_raw.unwrap_or("").to_ascii_lowercase();
    if t.contains("refund") {
        ReportLineKind::Refund
    } else if t.contains("dispute") || t.contains("chargeback") || t.contains("reversal") {
        ReportLineKind::Dispute
    } else if t.contains("payout") || t.contains("transfer") || t.contains("withdraw") {
        ReportLineKind::Payout
    } else if t.contains("fee") || t.contains("adjust") || t.contains("tax") {
        ReportLineKind::Other
    } else if t.contains("charge")
        || t.contains("payment")
        || t.contains("deposit")
        || t.contains("sale")
        || t.contains("purchase")
        || t.contains("capture")
        || t.contains("invoice")
        || t.contains("order")
        || t.contains("initial")
        || t.contains("renewal")
    {
        ReportLineKind::Deposit
    } else if signed_amount < 0 {
        ReportLineKind::Payout
    } else {
        ReportLineKind::Deposit
    }
}

fn pick(row: &HashMap<String, String>, cols: &[&'static str]) -> Option<(&'static str, String)> {
    cols.iter().find_map(|c| {
        row.get(*c)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| (*c, v.to_string()))
    })
}

/// Turn normalised rows into typed report lines.
fn rows_to_lines(
    rows: Vec<HashMap<String, String>>,
    provider: &str,
    unit: AmountUnit,
    reference_column: Option<&str>,
) -> Result<Vec<ProviderReportLine>, ApiError> {
    let mut lines = Vec::with_capacity(rows.len());
    for (idx, row) in rows.into_iter().enumerate() {
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let reference = match reference_column {
            Some(col) => row
                .get(col)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            None => pick(&row, REFERENCE_COLS).map(|(_, v)| v),
        };
        let Some(reference) = reference else {
            // Summary / subtotal rows carry no reference; skip them.
            continue;
        };
        // Strip a `<provider>:` namespace when the report echoes Mint refs.
        let reference = reference
            .strip_prefix(&format!("{provider}:"))
            .map(str::to_string)
            .unwrap_or(reference);
        let (amount_col, amount_raw) = pick(&row, AMOUNT_COLS).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "line {line_no}: no amount column (expected one of: {})",
                AMOUNT_COLS.join(", ")
            ))
        })?;
        let signed = parse_amount(&amount_raw, unit, amount_col).ok_or_else(|| {
            ApiError::BadRequest(format!("line {line_no}: unparsable amount '{amount_raw}'"))
        })?;
        let kind = classify(pick(&row, TYPE_COLS).map(|(_, v)| v).as_deref(), signed);
        lines.push(ProviderReportLine {
            line_no,
            kind,
            reference,
            amount_micros: signed.saturating_abs(),
            currency: pick(&row, CURRENCY_COLS).map(|(_, v)| v.to_ascii_lowercase()),
            occurred_ms: pick(&row, DATE_COLS).and_then(|(_, v)| parse_date(&v)),
        });
    }
    Ok(lines)
}

fn json_rows(content: &str) -> Result<Vec<HashMap<String, String>>, ApiError> {
    let v: Value = serde_json::from_str(content)
        .map_err(|e| ApiError::BadRequest(format!("report is not valid JSON: {e}")))?;
    let arr = match v {
        Value::Array(a) => a,
        // Provider APIs wrap lists: `{ "data": [...] }` (Stripe) /
        // `{ "transaction_details": [...] }` (PayPal).
        Value::Object(mut o) => ["data", "items", "transaction_details", "transactions"]
            .iter()
            .find_map(|k| match o.remove(*k) {
                Some(Value::Array(a)) => Some(a),
                _ => None,
            })
            .ok_or_else(|| {
                ApiError::BadRequest("JSON report must be an array of line objects".into())
            })?,
        _ => {
            return Err(ApiError::BadRequest(
                "JSON report must be an array of line objects".into(),
            ))
        }
    };
    Ok(arr
        .into_iter()
        .filter_map(|item| match item {
            Value::Object(map) => Some(flatten(map)),
            _ => None,
        })
        .collect())
}

/// Flatten one level of nesting (`transaction_info.transaction_id` →
/// `transaction_id`) and stringify scalars.
fn flatten(map: Map<String, Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in map {
        match v {
            Value::Object(inner) => {
                for (ik, iv) in inner {
                    if let Some(s) = scalar(&iv) {
                        out.entry(normalise_header(&ik)).or_insert(s);
                    }
                }
            }
            other => {
                if let Some(s) = scalar(&other) {
                    out.insert(normalise_header(&k), s);
                }
            }
        }
    }
    out
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn csv_rows(content: &str) -> Result<Vec<HashMap<String, String>>, ApiError> {
    let mut rows = parse_csv(content).into_iter();
    let header = rows
        .next()
        .ok_or_else(|| ApiError::BadRequest("report is empty".into()))?;
    let header: Vec<String> = header.iter().map(|h| normalise_header(h)).collect();
    if header.iter().all(|h| h.is_empty()) {
        return Err(ApiError::BadRequest("report header row is empty".into()));
    }
    Ok(rows
        .map(|r| {
            header
                .iter()
                .cloned()
                .zip(r.into_iter())
                .filter(|(h, _)| !h.is_empty())
                .collect()
        })
        .collect())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn report_view(r: &ProviderReport) -> Value {
    json!({
        "id": r.id,
        "provider": r.provider,
        "label": r.label,
        "imported_ms": r.imported_ms,
        "line_count": r.line_count,
        "period_start_ms": r.period_start_ms,
        "period_end_ms": r.period_end_ms,
    })
}

/// `POST /v1/reconciliation/reports` → parse + store a provider report and
/// return its match results.
pub async fn import<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<ImportReq>,
) -> ApiResult {
    let provider = body.provider.trim().to_ascii_lowercase();
    if !KNOWN_PROVIDERS.contains(&provider.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "unknown provider '{provider}' (expected one of: {})",
            KNOWN_PROVIDERS.join(", ")
        )));
    }
    if body.content.trim().is_empty() {
        return Err(ApiError::BadRequest("report content is empty".into()));
    }
    if body.content.len() > MAX_CONTENT {
        return Err(ApiError::BadRequest("report exceeds 20 MiB".into()));
    }
    let unit = match body.amount_unit.as_deref().unwrap_or("major") {
        "major" => AmountUnit::Major,
        "cents" => AmountUnit::Cents,
        "micros" => AmountUnit::Micros,
        other => {
            return Err(ApiError::BadRequest(format!(
                "amount_unit '{other}' must be major, cents or micros"
            )))
        }
    };
    let is_json = match body.format.as_deref() {
        Some("json") => true,
        Some("csv") => false,
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "format '{other}' must be csv or json"
            )))
        }
        None => matches!(
            body.content.trim_start().chars().next(),
            Some('[') | Some('{')
        ),
    };
    let rows = if is_json {
        json_rows(&body.content)?
    } else {
        csv_rows(&body.content)?
    };
    if rows.len() > MAX_LINES {
        return Err(ApiError::BadRequest(format!(
            "report has {} lines; split it (max {MAX_LINES})",
            rows.len()
        )));
    }
    let ref_col = body
        .reference_column
        .as_deref()
        .map(normalise_header)
        .filter(|c| !c.is_empty());
    let lines = rows_to_lines(rows, &provider, unit, ref_col.as_deref())?;
    if lines.is_empty() {
        return Err(ApiError::BadRequest(
            "no report line carried a reference; set reference_column to the id column".into(),
        ));
    }
    let period_start_ms = lines.iter().filter_map(|l| l.occurred_ms).min();
    let period_end_ms = lines.iter().filter_map(|l| l.occurred_ms).max();
    let report = ProviderReport {
        id: format!("rpt_{}", ulid::Ulid::new()),
        provider,
        label: body
            .label
            .map(|l| l.trim().chars().take(200).collect::<String>())
            .filter(|l| !l.is_empty()),
        imported_ms: now_ms(),
        line_count: lines.len() as u64,
        period_start_ms,
        period_end_ms,
    };
    let stored = st
        .ledger
        .store()
        .insert_provider_report(report, lines.clone())
        .await?;
    tracing::info!(report = %stored.id, provider = %stored.provider, lines = stored.line_count, "provider report imported");
    let apply_reversals = body
        .apply_reversals
        .unwrap_or(stored.provider == "iyzico");
    let result = match_report(&st, &stored, &lines, apply_reversals).await?;
    Ok(ok_status(StatusCode::CREATED, result))
}

/// `GET /v1/reconciliation/reports` → imported reports, newest first.
pub async fn list<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let reports = st.ledger.store().list_provider_reports(200).await?;
    Ok(ok(json!({
        "items": reports.iter().map(report_view).collect::<Vec<_>>(),
    })))
}

/// `GET /v1/reconciliation/reports/:id` → the report re-matched against the
/// live ledger.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let (report, lines) = st
        .ledger
        .store()
        .get_provider_report(&id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no report {id}")))?;
    Ok(ok(match_report(&st, &report, &lines, false).await?))
}

/// `DELETE /v1/reconciliation/reports/:id` → drop a mis-uploaded report.
pub async fn delete<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    if !st.ledger.store().delete_provider_report(&id).await? {
        return Err(ApiError::NotFound(format!("no report {id}")));
    }
    Ok(ok(json!({ "deleted": id })))
}

/// The raw part of a namespaced ref (`stripe:pi_1` ⇒ `pi_1`).
fn raw_ref(r: &str) -> &str {
    r.split_once(':').map(|(_, rest)| rest).unwrap_or(r)
}

/// Mint-internal corrections (payout-cancel re-credits, reversal claw-backs)
/// never appear in a provider report.
fn is_internal_ref(r: &str) -> bool {
    r.contains("#cancel") || r.contains("#reversal")
}

async fn match_report<S: LedgerStore + 'static>(
    st: &AppState<S>,
    report: &ProviderReport,
    lines: &[ProviderReportLine],
    apply_reversals: bool,
) -> Result<Value, ApiError> {
    let store = st.ledger.store();
    let provider = report.provider.as_str();
    let window = (
        report
            .period_start_ms
            .map(|s| s.saturating_sub(MATCH_SLACK_MS)),
        report
            .period_end_ms
            .map(|e| e.saturating_add(MATCH_SLACK_MS)),
    );
    let movements: Vec<ReserveMovement> = store
        .list_reserve_movements(provider, window.0, window.1, SCAN_LIMIT)
        .await?;
    let mints: HashMap<&str, &ReserveMovement> = movements
        .iter()
        .filter(|m| m.kind == "mint" && !is_internal_ref(&m.reference))
        .map(|m| (raw_ref(&m.reference), m))
        .collect();
    let payouts: Vec<PayoutRecord> = store
        .list_payouts(PayoutQuery {
            limit: SCAN_LIMIT,
            ..Default::default()
        })
        .await?
        .into_iter()
        .filter(|p| provider_tag_of(&p.payout_ref) == provider)
        .collect();
    let mut payout_by_ref: HashMap<&str, &PayoutRecord> = HashMap::new();
    for p in &payouts {
        payout_by_ref.insert(raw_ref(&p.payout_ref), p);
        if let Some(pr) = p.provider_ref.as_deref() {
            payout_by_ref.insert(pr, p);
        }
    }
    let reversals = store.list_deposit_reversals(None, SCAN_LIMIT).await?;
    let reversal_by_ref: HashMap<&str, _> = reversals
        .iter()
        .filter(|r| provider_tag_of(&r.deposit_ref) == provider)
        .map(|r| (raw_ref(&r.deposit_ref), r))
        .collect();

    // Cumulative refunded amount per reference across the whole report (a
    // payment refunded in several parts appears as several lines).
    let mut refund_totals: HashMap<&str, i64> = HashMap::new();
    for line in lines {
        if matches!(line.kind, ReportLineKind::Refund | ReportLineKind::Dispute) {
            let slot = refund_totals.entry(line.reference.as_str()).or_insert(0);
            *slot = slot.saturating_add(line.amount_micros);
        }
    }
    let mut reversals_applied: HashSet<String> = HashSet::new();

    let mut seen_mints: HashSet<&str> = HashSet::new();
    let mut seen_payouts: HashSet<&str> = HashSet::new();
    let mut items: Vec<Value> = Vec::with_capacity(lines.len());
    let mut counts: HashMap<&'static str, (u64, i64)> = HashMap::new();
    let mut bump = |status: &'static str, amount: i64| {
        let slot = counts.entry(status).or_insert((0, 0));
        slot.0 += 1;
        slot.1 = slot.1.saturating_add(amount);
    };

    for line in lines {
        let r = line.reference.as_str();
        let (status, mint_side): (&'static str, Value) = match line.kind {
            ReportLineKind::Deposit => match mints.get(r) {
                Some(m) => {
                    seen_mints.insert(raw_ref(&m.reference));
                    let status = if m.amount_micros == line.amount_micros {
                        "matched"
                    } else {
                        "amount_mismatch"
                    };
                    (
                        status,
                        json!({ "reference": m.reference, "amount_micros": m.amount_micros, "account_id": m.account_id, "ts_ms": m.ts_ms }),
                    )
                }
                None => ("missing_in_mint", Value::Null),
            },
            ReportLineKind::Payout => match payout_by_ref.get(r) {
                Some(p) => {
                    seen_payouts.insert(raw_ref(&p.payout_ref));
                    let status = if p.amount_micros.get() != line.amount_micros {
                        "amount_mismatch"
                    } else if p.status != PayoutRecordStatus::Paid {
                        "status_mismatch"
                    } else {
                        "matched"
                    };
                    (
                        status,
                        json!({ "reference": p.payout_ref, "amount_micros": p.amount_micros.get(), "account_id": p.account_id.as_str(), "status": p.status.as_str() }),
                    )
                }
                None => ("missing_in_mint", Value::Null),
            },
            ReportLineKind::Refund | ReportLineKind::Dispute => match reversal_by_ref.get(r) {
                Some(rev) => {
                    let status = if rev.amount_micros == line.amount_micros {
                        "matched"
                    } else {
                        "amount_mismatch"
                    };
                    (
                        status,
                        json!({ "reference": rev.deposit_ref, "amount_micros": rev.amount_micros, "account_id": rev.account_id, "status": rev.status.as_str() }),
                    )
                }
                None => match mints.get(r) {
                    Some(m) if apply_reversals => {
                        seen_mints.insert(raw_ref(&m.reference));
                        let applied = if reversals_applied.contains(&m.reference) {
                            Ok(())
                        } else {
                            let total = refund_totals
                                .get(r)
                                .copied()
                                .unwrap_or(line.amount_micros)
                                .min(m.amount_micros);
                            let reason = if line.kind == ReportLineKind::Dispute {
                                "dispute"
                            } else {
                                "refund"
                            };
                            crate::handlers::reversals::apply_reversal(
                                st,
                                &m.reference,
                                Some(total),
                                reason,
                                Some(format!("{provider} report import {}", report.id)),
                            )
                            .await
                            .map(|_| {
                                reversals_applied.insert(m.reference.clone());
                            })
                        };
                        match applied {
                            Ok(()) => (
                                "reversal_applied",
                                json!({ "reference": m.reference, "amount_micros": m.amount_micros, "account_id": m.account_id, "ts_ms": m.ts_ms }),
                            ),
                            Err(e) => {
                                tracing::warn!(reference = %m.reference, error = ?e, "report reversal could not be applied");
                                (
                                    "reversal_unrecorded",
                                    json!({ "reference": m.reference, "amount_micros": m.amount_micros, "account_id": m.account_id, "ts_ms": m.ts_ms }),
                                )
                            }
                        }
                    }
                    Some(m) => {
                        seen_mints.insert(raw_ref(&m.reference));
                        (
                            "reversal_unrecorded",
                            json!({ "reference": m.reference, "amount_micros": m.amount_micros, "account_id": m.account_id, "ts_ms": m.ts_ms }),
                        )
                    }
                    None => ("missing_in_mint", Value::Null),
                },
            },
            ReportLineKind::Other => ("informational", Value::Null),
        };
        bump(status, line.amount_micros);
        items.push(json!({
            "line_no": line.line_no,
            "kind": line.kind.as_str(),
            "reference": line.reference,
            "amount_micros": line.amount_micros,
            "currency": line.currency,
            "occurred_ms": line.occurred_ms,
            "status": status,
            "mint": mint_side,
        }));
    }

    // Mint-side movements inside the report period the provider never listed.
    let in_period = |ts: i64| {
        report.period_start_ms.is_none_or(|s| ts >= s)
            && report.period_end_ms.is_none_or(|e| ts <= e)
    };
    let mut missing_at_provider: Vec<Value> = Vec::new();
    let report_has_deposits = lines.iter().any(|l| l.kind == ReportLineKind::Deposit);
    let report_has_payouts = lines.iter().any(|l| l.kind == ReportLineKind::Payout);
    if report_has_deposits {
        for (r, m) in &mints {
            if !seen_mints.contains(r) && in_period(m.ts_ms) {
                bump("missing_at_provider", m.amount_micros);
                missing_at_provider.push(json!({
                    "kind": "deposit",
                    "reference": m.reference,
                    "amount_micros": m.amount_micros,
                    "account_id": m.account_id,
                    "ts_ms": m.ts_ms,
                }));
            }
        }
    }
    if report_has_payouts {
        for p in &payouts {
            let r = raw_ref(&p.payout_ref);
            if p.status == PayoutRecordStatus::Paid
                && !seen_payouts.contains(r)
                && in_period(p.created_ms)
            {
                bump("missing_at_provider", p.amount_micros.get());
                missing_at_provider.push(json!({
                    "kind": "payout",
                    "reference": p.payout_ref,
                    "amount_micros": p.amount_micros.get(),
                    "account_id": p.account_id.as_str(),
                    "ts_ms": p.created_ms,
                }));
            }
        }
    }
    missing_at_provider.sort_by_key(|v| v["ts_ms"].as_i64().unwrap_or(0));

    let summary: Map<String, Value> = [
        "matched",
        "amount_mismatch",
        "status_mismatch",
        "missing_in_mint",
        "reversal_unrecorded",
        "reversal_applied",
        "missing_at_provider",
        "informational",
    ]
    .iter()
    .map(|k| {
        let (n, micros) = counts.get(k).copied().unwrap_or((0, 0));
        (k.to_string(), json!({ "count": n, "micros": micros }))
    })
    .collect();
    let discrepancies: u64 = counts
        .iter()
        .filter(|(k, _)| !matches!(**k, "matched" | "informational" | "reversal_applied"))
        .map(|(_, (n, _))| *n)
        .sum();
    Ok(json!({
        "report": report_view(report),
        "summary": summary,
        "discrepancies": discrepancies,
        "lines": items,
        "missing_at_provider": missing_at_provider,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_parses_quotes_and_crlf() {
        let rows = parse_csv("a,b\r\n\"x, y\",\"he said \"\"hi\"\"\"\r\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            vec!["x, y".to_string(), "he said \"hi\"".to_string()]
        );
    }

    #[test]
    fn amounts_parse_by_unit() {
        assert_eq!(
            parse_amount("$1,234.56", AmountUnit::Major, "amount"),
            Some(1_234_560_000)
        );
        assert_eq!(
            parse_amount("-12.5", AmountUnit::Major, "amount"),
            Some(-12_500_000)
        );
        assert_eq!(
            parse_amount("(3.00)", AmountUnit::Major, "amount"),
            Some(-3_000_000)
        );
        assert_eq!(
            parse_amount("1999", AmountUnit::Cents, "amount"),
            Some(19_990_000)
        );
        assert_eq!(
            parse_amount("42", AmountUnit::Major, "amount_micros"),
            Some(42)
        );
    }

    #[test]
    fn dates_parse() {
        assert_eq!(parse_date("1970-01-02"), Some(86_400_000));
        assert_eq!(parse_date("1970-01-01T00:01:00Z"), Some(60_000));
        assert_eq!(parse_date("1700000000"), Some(1_700_000_000_000));
    }

    #[test]
    fn stripe_style_rows_become_lines() {
        let rows = csv_rows(
            "Payment Intent ID,Reporting Category,Gross,Currency,Created (UTC)\n\
             pi_1,charge,10.00,usd,2026-09-01 10:00:00\n\
             pi_1,refund,-10.00,usd,2026-09-02 10:00:00\n",
        )
        .unwrap();
        let lines = rows_to_lines(rows, "stripe", AmountUnit::Major, None).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].reference, "pi_1");
        assert_eq!(lines[0].kind, ReportLineKind::Deposit);
        assert_eq!(lines[0].amount_micros, 10_000_000);
        assert_eq!(lines[1].kind, ReportLineKind::Refund);
    }
}
