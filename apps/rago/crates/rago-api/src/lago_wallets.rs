//! Lago REST parity for wallets and metadata:
//!
//! * `/customers/:external_id/wallets[/:code]` — a customer's wallets by code
//!   (create, list, show, update, terminate), their alerts and metadata;
//! * `/wallets/:id/metadata[/:key]`, `/plans/:code/metadata[/:key]`,
//!   `/credit_notes/:id/metadata[/:key]` — create (replace), update (merge),
//!   destroy (all keys) and destroy one key;
//! * `/wallet_transactions/:id` (show, payment_url, consumptions, fundings)
//!   and `/wallets/:id/wallet_transactions`;
//! * `/customers/:external_id/payment_methods/:id` (destroy, set_as_default).

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_core::models::{Customer, WalletTransaction};
use rago_db::rest_parity::{LedgerTx, MetadataOwner};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{s, wrapped};
use crate::serializers as ser;
use crate::state::AppState;

const MAX_METADATA_KEYS: usize = 50;
const MAX_KEY_LEN: usize = 40;
const MAX_VALUE_LEN: usize = 255;

// ---- metadata ---------------------------------------------------------------

/// The `metadata` object of a request: string keys, string or null values.
fn metadata_input(body: &Value) -> ApiResult<Value> {
    let m = body
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::validation("metadata", "value_is_mandatory"))?;
    if m.len() > MAX_METADATA_KEYS {
        return Err(ApiError::validation("metadata", "too_many_keys"));
    }
    let mut out = Map::new();
    for (k, v) in m {
        if k.is_empty() || k.chars().count() > MAX_KEY_LEN {
            return Err(ApiError::validation("metadata", "invalid_key"));
        }
        match v {
            Value::Null => {
                out.insert(k.clone(), Value::Null);
            }
            Value::String(s) if s.chars().count() <= MAX_VALUE_LEN => {
                out.insert(k.clone(), Value::String(s.clone()));
            }
            Value::Number(n) => {
                out.insert(k.clone(), Value::String(n.to_string()));
            }
            Value::Bool(b) => {
                out.insert(k.clone(), Value::String(b.to_string()));
            }
            _ => return Err(ApiError::validation("metadata", "invalid_value")),
        }
    }
    Ok(Value::Object(out))
}

fn metadata_out(v: Option<Value>, what: &str) -> ApiResult<Json<Value>> {
    let v = v.ok_or_else(|| ApiError::NotFound(what.into()))?;
    let empty = v.as_object().is_none_or(Map::is_empty);
    Ok(Json(json!({ "metadata": if empty { Value::Null } else { v } })))
}

async fn write(
    st: &AppState,
    owner: MetadataOwner,
    org: Uuid,
    id: Uuid,
    body: &Value,
    merge: bool,
    what: &str,
) -> ApiResult<Json<Value>> {
    let input = metadata_input(body)?;
    let next = if merge {
        let mut current = st
            .db
            .metadata(owner, org, id)
            .await?
            .ok_or_else(|| ApiError::NotFound(what.into()))?;
        if let (Some(cur), Some(add)) = (current.as_object_mut(), input.as_object()) {
            for (k, v) in add {
                cur.insert(k.clone(), v.clone());
            }
        }
        current
    } else {
        input
    };
    if next.as_object().is_some_and(|o| o.len() > MAX_METADATA_KEYS) {
        return Err(ApiError::validation("metadata", "too_many_keys"));
    }
    metadata_out(st.db.write_metadata(owner, org, id, &next, false).await?, what)
}

async fn plan_id(st: &AppState, org: Uuid, code: &str) -> ApiResult<Uuid> {
    Ok(st
        .db
        .find_plan_by_code(org, code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?
        .id)
}

pub async fn create_plan_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = plan_id(&st, auth.organization.id, &code).await?;
    write(&st, MetadataOwner::Plan, auth.organization.id, id, &b, false, "plan").await
}
pub async fn update_plan_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = plan_id(&st, auth.organization.id, &code).await?;
    write(&st, MetadataOwner::Plan, auth.organization.id, id, &b, true, "plan").await
}
pub async fn destroy_plan_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = plan_id(&st, auth.organization.id, &code).await?;
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Plan, auth.organization.id, id, None)
            .await?,
        "plan",
    )
}
pub async fn destroy_plan_metadata_key(
    State(st): State<AppState>,
    auth: Authed,
    Path((code, key)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = plan_id(&st, auth.organization.id, &code).await?;
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Plan, auth.organization.id, id, Some(&key))
            .await?,
        "plan",
    )
}

pub async fn create_wallet_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    write(
        &st,
        MetadataOwner::Wallet,
        auth.organization.id,
        id,
        &b,
        false,
        "wallet",
    )
    .await
}
pub async fn update_wallet_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    write(&st, MetadataOwner::Wallet, auth.organization.id, id, &b, true, "wallet").await
}
pub async fn destroy_wallet_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Wallet, auth.organization.id, id, None)
            .await?,
        "wallet",
    )
}
pub async fn destroy_wallet_metadata_key(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Wallet, auth.organization.id, id, Some(&key))
            .await?,
        "wallet",
    )
}

pub async fn create_credit_note_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    write(
        &st,
        MetadataOwner::CreditNote,
        auth.organization.id,
        id,
        &b,
        false,
        "credit_note",
    )
    .await
}
pub async fn update_credit_note_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(b): Json<Value>,
) -> ApiResult<Json<Value>> {
    write(
        &st,
        MetadataOwner::CreditNote,
        auth.organization.id,
        id,
        &b,
        true,
        "credit_note",
    )
    .await
}
pub async fn destroy_credit_note_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::CreditNote, auth.organization.id, id, None)
            .await?,
        "credit_note",
    )
}
pub async fn destroy_credit_note_metadata_key(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::CreditNote, auth.organization.id, id, Some(&key))
            .await?,
        "credit_note",
    )
}

// ---- customer wallets by code ----------------------------------------------

async fn customer(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Customer> {
    st.db
        .find_customer_by_external_id(org, external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))
}

async fn wallet_id(st: &AppState, org: Uuid, external_id: &str, code: &str) -> ApiResult<Uuid> {
    let c = customer(st, org, external_id).await?;
    st.db
        .wallet_id_by_code(c.id, code)
        .await?
        .ok_or_else(|| ApiError::NotFound("wallet".into()))
}

/// A wallet's REST object with its code and metadata.
async fn wallet_json(st: &AppState, id: Uuid) -> ApiResult<Value> {
    let w = st
        .db
        .find_wallet(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let ext = st.db.customer_external_id(w.customer_id).await?.unwrap_or_default();
    let (code, metadata) = st.db.wallet_code_and_metadata(id).await?;
    let mut v = ser::wallet(&w, &ext);
    v["code"] = json!(code);
    v["metadata"] = if metadata.as_object().is_none_or(Map::is_empty) {
        Value::Null
    } else {
        metadata
    };
    Ok(v)
}

/// A code from a name (`Prepaid credits` → `prepaid_credits`).
fn code_from(name: Option<&str>) -> String {
    let base: String = name
        .unwrap_or("default")
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if base.is_empty() {
        "default".into()
    } else {
        base
    }
}

/// `POST /customers/:external_id/wallets` — Lago `Customers::WalletsController#create`.
pub async fn create_customer_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let c = customer(&st, org, &external_id).await?;
    let p = wrapped(&body, "wallet").clone();
    // A given code must be free; a derived one gets a numeric suffix.
    let code = match s(&p, "code").filter(|c| !c.trim().is_empty()) {
        Some(code) => {
            if st.db.wallet_code_in_use(c.id, &code).await? {
                return Err(ApiError::validation("code", "value_already_exist"));
            }
            code
        }
        None => {
            let base = code_from(s(&p, "name").as_deref());
            let mut candidate = base.clone();
            let mut n = 2;
            while st.db.wallet_code_in_use(c.id, &candidate).await? {
                candidate = format!("{base}_{n}");
                n += 1;
            }
            candidate
        }
    };
    let metadata = match p.get("metadata") {
        Some(Value::Object(_)) => Some(metadata_input(&p)?),
        _ => None,
    };
    let mut inner = p.as_object().cloned().unwrap_or_default();
    inner.insert("external_customer_id".into(), json!(external_id));
    let Json(made) = crate::handlers::create_wallet(State(st.clone()), auth, Json(json!({ "wallet": inner }))).await?;
    let id: Uuid = made["wallet"]["lago_id"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ApiError::Internal("wallet id".into()))?;
    st.db.set_wallet_code(id, &code).await?;
    if let Some(m) = metadata {
        st.db.write_metadata(MetadataOwner::Wallet, org, id, &m, false).await?;
    }
    Ok(Json(json!({ "wallet": wallet_json(&st, id).await? })))
}

/// `GET /customers/:external_id/wallets`.
pub async fn list_customer_wallets(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = customer(&st, auth.organization.id, &external_id).await?;
    let mut out = Vec::new();
    for w in st.db.wallets_for_customer(c.id).await? {
        out.push(wallet_json(&st, w.id).await?);
    }
    let total = out.len() as i64;
    Ok(Json(
        json!({ "wallets": out, "meta": crate::pagination::meta(1, total.max(1), total) }),
    ))
}

/// `GET /customers/:external_id/wallets/:code`.
pub async fn show_customer_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    Ok(Json(json!({ "wallet": wallet_json(&st, id).await? })))
}

/// `PUT /customers/:external_id/wallets/:code`.
pub async fn update_customer_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let id = wallet_id(&st, org, &external_id, &code).await?;
    let p = wrapped(&body, "wallet").clone();
    let _ = crate::crud::update_wallet(State(st.clone()), auth, Path(id), Json(json!({ "wallet": p }))).await?;
    if let Some(Value::Object(_)) = p.get("metadata") {
        st.db
            .write_metadata(MetadataOwner::Wallet, org, id, &metadata_input(&p)?, false)
            .await?;
    }
    Ok(Json(json!({ "wallet": wallet_json(&st, id).await? })))
}

/// `DELETE /customers/:external_id/wallets/:code` — terminate.
pub async fn terminate_customer_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    let _ = crate::crud::terminate_wallet(State(st.clone()), auth, Path(id)).await?;
    Ok(Json(json!({ "wallet": wallet_json(&st, id).await? })))
}

macro_rules! wallet_metadata_by_code {
    ($name:ident, $merge:expr) => {
        pub async fn $name(
            State(st): State<AppState>,
            auth: Authed,
            Path((external_id, code)): Path<(String, String)>,
            Json(b): Json<Value>,
        ) -> ApiResult<Json<Value>> {
            let id = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
            write(
                &st,
                MetadataOwner::Wallet,
                auth.organization.id,
                id,
                &b,
                $merge,
                "wallet",
            )
            .await
        }
    };
}
wallet_metadata_by_code!(create_customer_wallet_metadata, false);
wallet_metadata_by_code!(update_customer_wallet_metadata, true);

pub async fn destroy_customer_wallet_metadata(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let id = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Wallet, auth.organization.id, id, None)
            .await?,
        "wallet",
    )
}
pub async fn destroy_customer_wallet_metadata_key(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, key)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let id = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    metadata_out(
        st.db
            .delete_metadata(MetadataOwner::Wallet, auth.organization.id, id, Some(&key))
            .await?,
        "wallet",
    )
}

// ---- wallet alerts -----------------------------------------------------------

async fn wallet_alert(st: &AppState, wallet: Uuid, code: &str) -> ApiResult<rago_db::AlertRow> {
    st.db
        .alerts_for("Wallet", wallet)
        .await?
        .into_iter()
        .find(|a| a.code == code)
        .ok_or_else(|| ApiError::NotFound("alert".into()))
}

async fn alert_out(st: &AppState, id: Uuid) -> ApiResult<Value> {
    let row = st
        .db
        .find_alert(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("alert".into()))?;
    let th = st.db.alert_thresholds(id).await?;
    Ok(crate::subs::alert_json(&row, &th))
}

/// `POST /customers/:external_id/wallets/:code/alerts`.
pub async fn create_wallet_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let wallet = wallet_id(&st, org, &external_id, &code).await?;
    let p = wrapped(&body, "alert");
    let alert_type = crate::handlers::req_s(p, "alert_type")?;
    if ![
        "wallet_balance_amount",
        "wallet_credits_balance",
        "wallet_ongoing_balance_amount",
        "wallet_credits_ongoing_balance",
    ]
    .contains(&alert_type.as_str())
    {
        return Err(ApiError::validation("alert_type", "value_is_invalid"));
    }
    let alert_code = crate::handlers::req_s(p, "code")?;
    if wallet_alert(&st, wallet, &alert_code).await.is_ok() {
        return Err(ApiError::validation("code", "value_already_exist"));
    }
    let id = Uuid::new_v4();
    st.db
        .insert_alert(
            id,
            org,
            "Wallet",
            wallet,
            &alert_type,
            s(p, "name").as_deref(),
            &alert_code,
            None,
        )
        .await?;
    let thresholds = crate::subs::parse_thresholds(p);
    if thresholds.is_empty() {
        return Err(ApiError::validation("thresholds", "value_is_mandatory"));
    }
    st.db.replace_alert_thresholds(org, id, &thresholds).await?;
    Ok(Json(json!({ "alert": alert_out(&st, id).await? })))
}

/// `GET /customers/:external_id/wallets/:code/alerts`.
pub async fn list_wallet_alerts(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let wallet = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    let mut out = Vec::new();
    for a in st.db.alerts_for("Wallet", wallet).await? {
        out.push(alert_out(&st, a.id).await?);
    }
    Ok(Json(json!({ "alerts": out })))
}

pub async fn show_wallet_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, alert)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let wallet = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    let a = wallet_alert(&st, wallet, &alert).await?;
    Ok(Json(json!({ "alert": alert_out(&st, a.id).await? })))
}

pub async fn update_wallet_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, alert)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let wallet = wallet_id(&st, org, &external_id, &code).await?;
    let a = wallet_alert(&st, wallet, &alert).await?;
    let p = wrapped(&body, "alert");
    st.db
        .update_alert(a.id, s(p, "name").as_deref(), s(p, "code").as_deref())
        .await?;
    let thresholds = crate::subs::parse_thresholds(p);
    if !thresholds.is_empty() {
        st.db.replace_alert_thresholds(org, a.id, &thresholds).await?;
    }
    Ok(Json(json!({ "alert": alert_out(&st, a.id).await? })))
}

pub async fn destroy_wallet_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code, alert)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let wallet = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    let a = wallet_alert(&st, wallet, &alert).await?;
    let out = alert_out(&st, a.id).await?;
    st.db.soft_delete_alert(a.id).await?;
    Ok(Json(json!({ "alert": out })))
}

/// `DELETE /customers/:external_id/wallets/:code/alerts` — every alert.
pub async fn destroy_all_wallet_alerts(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let wallet = wallet_id(&st, auth.organization.id, &external_id, &code).await?;
    let mut out = Vec::new();
    for a in st.db.alerts_for("Wallet", wallet).await? {
        out.push(alert_out(&st, a.id).await?);
        st.db.soft_delete_alert(a.id).await?;
    }
    Ok(Json(json!({ "alerts": out })))
}

// ---- payment methods -------------------------------------------------------

async fn customer_method(
    st: &AppState,
    org: Uuid,
    external_id: &str,
    id: Uuid,
) -> ApiResult<rago_db::PaymentMethodRow> {
    let c = customer(st, org, external_id).await?;
    st.db
        .find_payment_method(id)
        .await?
        .filter(|m| m.1 == c.id)
        .ok_or_else(|| ApiError::NotFound("payment_method".into()))
}

fn method_json(m: &rago_db::PaymentMethodRow, external_id: &str) -> Value {
    json!({ "lago_id": m.0, "lago_customer_id": m.1, "external_customer_id": external_id, "provider_method_id": m.2, "is_default": m.3 })
}

/// `DELETE /customers/:external_id/payment_methods/:id`.
pub async fn destroy_payment_method(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let m = customer_method(&st, auth.organization.id, &external_id, id).await?;
    st.db.soft_delete_payment_method(id).await?;
    Ok(Json(json!({ "payment_method": method_json(&m, &external_id) })))
}

/// `PUT /customers/:external_id/payment_methods/:id/set_as_default`.
pub async fn set_default_payment_method(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    customer_method(&st, auth.organization.id, &external_id, id).await?;
    let m = st
        .db
        .set_default_payment_method(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("payment_method".into()))?;
    Ok(Json(json!({ "payment_method": method_json(&m, &external_id) })))
}

// ---- wallet transactions -----------------------------------------------------

async fn transaction(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<WalletTransaction> {
    st.db
        .find_wallet_transaction(org, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("wallet_transaction".into()))
}

/// `GET /wallet_transactions/:id`.
pub async fn show_wallet_transaction(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = transaction(&st, auth.organization.id, id).await?;
    Ok(Json(json!({ "wallet_transaction": ser::wallet_transaction(&t) })))
}

/// `POST /wallet_transactions/:id/payment_url` — the checkout of the credit
/// purchase's invoice (a pending purchased top-up only).
pub async fn wallet_transaction_payment_url(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = transaction(&st, auth.organization.id, id).await?;
    if t.status.as_str() != "pending" {
        return Err(ApiError::validation("wallet_transaction", "not_pending"));
    }
    let invoice = t
        .invoice_id
        .ok_or_else(|| ApiError::validation("wallet_transaction", "no_invoice"))?;
    let Json(details) = crate::subs::invoice_payment_url(State(st), auth, Path(invoice)).await?;
    Ok(Json(json!({
        "wallet_transaction_payment_details": {
            "lago_wallet_transaction_id": t.id,
            "lago_invoice_id": invoice,
            "payment_url": details["invoice_payment_details"]["payment_url"],
        }
    })))
}

/// FIFO allocation of a wallet's settled outbound transactions to the inbound
/// ones that funded them: `(inbound id, outbound id, credits)`.
pub fn allocate_fifo(ledger: &[LedgerTx]) -> Vec<(Uuid, Uuid, Decimal)> {
    let mut lots: Vec<(Uuid, Decimal)> = Vec::new();
    let mut out = Vec::new();
    for tx in ledger {
        if tx.inbound {
            lots.push((tx.id, tx.credit_amount));
            continue;
        }
        let mut need = tx.credit_amount;
        for lot in lots.iter_mut() {
            if need <= Decimal::ZERO {
                break;
            }
            if lot.1 <= Decimal::ZERO {
                continue;
            }
            let take = need.min(lot.1);
            lot.1 -= take;
            need -= take;
            out.push((lot.0, tx.id, take));
        }
    }
    out
}

async fn allocation_for(st: &AppState, t: &WalletTransaction) -> ApiResult<Vec<(Uuid, Uuid, Decimal)>> {
    Ok(allocate_fifo(&st.db.settled_wallet_ledger(t.wallet_id).await?))
}

/// `GET /wallet_transactions/:id/consumptions` — what an inbound top-up paid for.
pub async fn wallet_transaction_consumptions(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = transaction(&st, auth.organization.id, id).await?;
    if t.transaction_type.as_str() != "inbound" {
        return Err(ApiError::validation("wallet_transaction", "must_be_inbound"));
    }
    let rows: Vec<Value> = allocation_for(&st, &t)
        .await?
        .into_iter()
        .filter(|(inb, _, _)| *inb == t.id)
        .map(|(_, outb, credits)| json!({ "lago_outbound_wallet_transaction_id": outb, "consumed_credits": credits.normalize().to_string() }))
        .collect();
    Ok(Json(json!({ "wallet_transaction_consumptions": rows })))
}

/// `GET /wallet_transactions/:id/fundings` — which top-ups paid for an outbound one.
pub async fn wallet_transaction_fundings(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = transaction(&st, auth.organization.id, id).await?;
    if t.transaction_type.as_str() != "outbound" {
        return Err(ApiError::validation("wallet_transaction", "must_be_outbound"));
    }
    let rows: Vec<Value> = allocation_for(&st, &t)
        .await?
        .into_iter()
        .filter(|(_, outb, _)| *outb == t.id)
        .map(|(inb, _, credits)| json!({ "lago_inbound_wallet_transaction_id": inb, "consumed_credits": credits.normalize().to_string() }))
        .collect();
    Ok(Json(json!({ "wallet_transaction_fundings": rows })))
}

/// `GET /wallets/:id/wallet_transactions` (Lago's path for the wallet's transactions).
pub async fn wallet_wallet_transactions(
    st: State<AppState>,
    auth: Authed,
    id: Path<Uuid>,
    q: Query<crate::reads::WalletTransactionIndexQuery>,
) -> ApiResult<Json<Value>> {
    crate::reads::wallet_transactions(st, auth, id, q).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(n: u128, inbound: bool, credits: Decimal) -> LedgerTx {
        LedgerTx {
            id: Uuid::from_u128(n),
            inbound,
            credit_amount: credits,
            amount: credits,
            created_at: chrono::NaiveDateTime::default(),
        }
    }

    #[test]
    fn outbound_credits_draw_the_oldest_top_ups_first() {
        let ledger = [
            tx(1, true, Decimal::from(10)),
            tx(2, true, Decimal::from(5)),
            tx(3, false, Decimal::from(12)),
            tx(4, false, Decimal::from(2)),
        ];
        assert_eq!(
            allocate_fifo(&ledger),
            vec![
                (Uuid::from_u128(1), Uuid::from_u128(3), Decimal::from(10)),
                (Uuid::from_u128(2), Uuid::from_u128(3), Decimal::from(2)),
                (Uuid::from_u128(2), Uuid::from_u128(4), Decimal::from(2)),
            ]
        );
    }

    #[test]
    fn codes_come_from_names() {
        assert_eq!(code_from(Some("Prepaid credits!")), "prepaid_credits");
        assert_eq!(code_from(None), "default");
        assert_eq!(code_from(Some("  ")), "default");
    }

    #[test]
    fn metadata_values_are_short_strings() {
        assert!(metadata_input(&json!({ "metadata": { "a": "b", "n": 1, "x": null } })).is_ok());
        assert!(metadata_input(&json!({ "metadata": { "a": { "nested": 1 } } })).is_err());
        assert!(metadata_input(&json!({ "metadata": { "a": "x".repeat(256) } })).is_err());
        assert!(metadata_input(&json!({})).is_err());
    }
}
