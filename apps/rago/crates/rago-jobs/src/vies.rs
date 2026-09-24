//! EU VIES VAT-number validation (`Customers::EuAutoTaxesService` →
//! `customer.vies_check`).
//!
//! When a customer in an EU member state (or Northern Ireland) is created or
//! gets a new VAT number, a durable `customers.vies_check` job is queued. The
//! job asks the European Commission's public VIES REST service whether the
//! number is registered, stores the verdict on the customer (`vies_valid`,
//! `vies_name`, `vies_address`, `vies_error`) and emits `customer.vies_check`.
//!
//! VIES needs no credentials. Its base URL can be overridden with
//! `RAGO_VIES_BASE_URL` (default: the Commission endpoint). Network calls go
//! through the injectable [`HttpTransport`]; a build without the `live`
//! transport reports the lookup as unavailable, which is recorded on the
//! customer and re-tried by the job queue, never fabricated as a verdict.

use chrono::Utc;
use rago_db::lifecycle::ViesResult;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::HttpTransport;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::webhook_emit::record_webhook;
use crate::JobError;

/// Durable job type of a VIES lookup.
pub const JOB_VIES_CHECK: &str = "customers.vies_check";

/// The Commission's VIES REST endpoint.
pub const DEFAULT_VIES_BASE_URL: &str = "https://ec.europa.eu/taxation_customs/vies/rest-api";

/// Attempts the queue makes for one lookup (VIES member-state services are
/// regularly unavailable for short windows).
const VIES_MAX_ATTEMPTS: i32 = 5;

/// VIES member-state codes (ISO country code → VIES prefix; Greece is `EL`,
/// Northern Ireland `XI`).
const VIES_COUNTRIES: [(&str, &str); 28] = [
    ("AT", "AT"), ("BE", "BE"), ("BG", "BG"), ("CY", "CY"), ("CZ", "CZ"), ("DE", "DE"),
    ("DK", "DK"), ("EE", "EE"), ("ES", "ES"), ("FI", "FI"), ("FR", "FR"), ("GR", "EL"),
    ("HR", "HR"), ("HU", "HU"), ("IE", "IE"), ("IT", "IT"), ("LT", "LT"), ("LU", "LU"),
    ("LV", "LV"), ("MT", "MT"), ("NL", "NL"), ("PL", "PL"), ("PT", "PT"), ("RO", "RO"),
    ("SE", "SE"), ("SI", "SI"), ("SK", "SK"), ("XI", "XI"),
];

/// The VIES member-state prefix of an ISO country code, when it is one.
pub fn vies_country(country: &str) -> Option<&'static str> {
    let cc = country.trim().to_ascii_uppercase();
    let cc = if cc == "EL" { "GR".to_string() } else { cc };
    VIES_COUNTRIES.iter().find(|(iso, _)| *iso == cc).map(|(_, vies)| *vies)
}

/// Normalize a VAT number for VIES: keep alphanumerics, upper-case, and drop a
/// leading member-state prefix (`DE123…` → `123…`).
pub fn normalize_vat_number(raw: &str, vies_prefix: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let greek_iso = vies_prefix == "EL" && cleaned.starts_with("GR");
    if cleaned.starts_with(vies_prefix) || greek_iso {
        cleaned[2..].to_string()
    } else {
        cleaned
    }
}

/// Whether a normalized number has a plausible VIES shape (2..=12 alphanumerics).
fn valid_format(number: &str) -> bool {
    (2..=12).contains(&number.len()) && number.chars().all(|c| c.is_ascii_alphanumeric())
}

/// `(vies_prefix, normalized_number)` when the customer is VIES-checkable.
fn checkable(country: Option<&str>, tax_id: Option<&str>) -> Option<(&'static str, String)> {
    let prefix = vies_country(country?)?;
    let raw = tax_id?.trim();
    if raw.is_empty() {
        return None;
    }
    Some((prefix, normalize_vat_number(raw, prefix)))
}

/// Queue a VIES lookup for `customer_id` when it is in the EU with a VAT
/// number that has not been checked yet (or changed since). Returns whether a
/// job was queued.
pub async fn enqueue_vies_check(db: &Db, customer_id: Uuid) -> Result<bool, JobError> {
    let Some((country, tax_id, checked)) = db.customer_vat_identity(customer_id).await? else {
        return Ok(false);
    };
    let Some((prefix, number)) = checkable(country.as_deref(), tax_id.as_deref()) else {
        return Ok(false);
    };
    if checked.as_deref() == Some(format!("{prefix}{number}").as_str()) {
        return Ok(false);
    }
    db.enqueue_job(
        JOB_VIES_CHECK,
        &json!({ "customer_id": customer_id.to_string() }),
        Utc::now().naive_utc(),
        VIES_MAX_ATTEMPTS,
    )
    .await?;
    Ok(true)
}

/// Queue a VIES lookup, logging (never propagating) a failure: used from
/// customer create/update paths, where the customer write already succeeded.
pub async fn enqueue_vies_check_logged(db: &Db, customer_id: Uuid) {
    if let Err(e) = enqueue_vies_check(db, customer_id).await {
        tracing::warn!(customer_id = %customer_id, error = %e, "VIES check could not be queued");
    }
}

/// A parsed VIES verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct ViesVerdict {
    pub valid: bool,
    pub name: Option<String>,
    pub address: Option<String>,
}

/// Blank VIES trader fields (`"---"` when the member state withholds them).
fn trader_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "---")
        .map(str::to_string)
}

/// Parse a `check-vat-number` response body.
pub fn parse_vies_response(body: &str) -> Result<ViesVerdict, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("vies_invalid_response: {e}"))?;
    if v.get("actionSucceed").and_then(Value::as_bool) == Some(false) {
        let code = v
            .get("errorWrappers")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("vies_error");
        return Err(code.to_string());
    }
    let valid = v
        .get("valid")
        .and_then(Value::as_bool)
        .ok_or_else(|| "vies_invalid_response: missing `valid`".to_string())?;
    Ok(ViesVerdict { valid, name: trader_field(&v, "name"), address: trader_field(&v, "address") })
}

/// Call VIES for `prefix` + `number`.
pub async fn lookup_vat<T: HttpTransport>(transport: &T, prefix: &str, number: &str) -> Result<ViesVerdict, String> {
    let base = std::env::var("RAGO_VIES_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VIES_BASE_URL.to_string());
    let url = format!("{}/check-vat-number", base.trim_end_matches('/'));
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let body = json!({ "countryCode": prefix, "vatNumber": number }).to_string();
    let resp = transport
        .post(&url, &headers, body)
        .await
        .map_err(|e| format!("vies_unavailable: {e}"))?;
    if !resp.is_success() {
        return Err(format!("vies_http_{}", resp.status));
    }
    parse_vies_response(&resp.body)
}

/// Run the VIES check of one customer: look the number up, store the result
/// and emit `customer.vies_check`. A malformed number is recorded as invalid
/// without a network call. An unavailable service is recorded on the customer
/// and returned as an error so the queue retries it. Returns the stored result
/// (`None` when the customer is no longer VIES-checkable).
pub async fn check_customer_vies<T: HttpTransport>(
    db: &Db,
    transport: &T,
    customer_id: Uuid,
) -> Result<Option<ViesResult>, JobError> {
    let customer = db
        .find_customer(customer_id)
        .await?
        .ok_or_else(|| JobError::Integration("customer_not_found".into()))?;
    let Some((prefix, number)) =
        checkable(customer.country.as_deref(), customer.tax_identification_number.as_deref())
    else {
        return Ok(None);
    };
    let checked_number = format!("{prefix}{number}");
    let format_ok = valid_format(&number);

    let (result, retry_error) = if !format_ok {
        let r = ViesResult {
            checked_number,
            valid: Some(false),
            name: None,
            address: None,
            error: Some("invalid_format".into()),
        };
        (r, None)
    } else {
        match lookup_vat(transport, prefix, &number).await {
            Ok(v) => (
                ViesResult { checked_number, valid: Some(v.valid), name: v.name, address: v.address, error: None },
                None,
            ),
            Err(e) => (
                ViesResult { checked_number, valid: None, name: None, address: None, error: Some(e.clone()) },
                Some(e),
            ),
        }
    };
    db.record_vies_result(customer.id, &result).await?;

    record_webhook(
        db,
        customer.organization_id,
        WebhookKind::CustomerViesCheck,
        json!({
            "lago_id": customer.id,
            "external_id": customer.external_id,
            "name": customer.name,
            "email": customer.email,
            "country": customer.country,
            "tax_identification_number": customer.tax_identification_number,
            "vies_check": {
                "valid": result.valid,
                "valid_format": format_ok,
                "country_code": prefix,
                "vat_number": number,
                "name": result.name,
                "address": result.address,
                "error": result.error,
                "checked_at": Utc::now().to_rfc3339(),
            },
        }),
    )
    .await?;

    match retry_error {
        // Recorded and announced; fail the job so the queue retries the lookup.
        Some(e) => Err(JobError::Integration(e)),
        None => Ok(Some(result)),
    }
}

