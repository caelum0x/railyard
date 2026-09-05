//! Tax-integration endpoint — fetch external tax rates for an invoice and
//! apply them to its fees (`Integrations::Aggregator::Taxes` +
//! `Fees::ApplyProviderTaxesService` + invoice tax rollup).
//!
//! The provider call goes through the injectable transport. For a live Anrok
//! account the transport is the reqwest transport (owner-gated, `RAGO_TAX_*`
//! keys); here it is driven by an injected response so the full provider
//! round-trip (request shape → parse → apply) runs against real Postgres.

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_billing::fees::{apply_provider_taxes, ProviderTaxLine};
use rago_core::money::round_to_integer;
use rago_integrations::{
    AnrokTaxProvider, AvalaraTaxProvider, MockTransport, TaxFeeItem, TaxProvider, TaxRequest,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct TaxProviderQuery {
    /// `anrok` (default) or `avalara`.
    pub provider: Option<String>,
}

/// `POST /invoices/:id/apply_provider_taxes?provider=` — fetch + apply external
/// taxes through the selected tax provider (Anrok or Avalara), each over the
/// injectable transport.
pub async fn apply_provider_taxes_to_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(pq): Query<TaxProviderQuery>,
) -> ApiResult<Json<Value>> {
    let mut invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let mut fees = st.db.fees_for_invoice(invoice.id).await?;
    let currency = invoice.currency().code();
    let external_customer_id = match invoice.customer_id {
        Some(cid) => st.db.customer_external_id(cid).await?.unwrap_or_default(),
        None => String::new(),
    };

    // Build the tax request.
    let req = TaxRequest {
        invoice_id: invoice.id.to_string(),
        currency: currency.clone(),
        customer_external_id: external_customer_id,
        fees: fees
            .iter()
            .map(|f| TaxFeeItem {
                item_id: f.id.to_string(),
                item_code: f.charge_id.map(|c| c.to_string()).unwrap_or_else(|| "subscription".into()),
                amount_cents: f.amount_cents,
            })
            .collect(),
    };

    // The external tax response, at the configured rate (simulates Anrok). A
    // live deployment injects the reqwest transport instead.
    let rate: f64 = std::env::var("RAGO_TAX_RATE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.085);
    let name = std::env::var("RAGO_TAX_NAME").unwrap_or_else(|_| "CA State Tax".into());
    let response = build_tax_response(&fees, rate, &name);
    let base_url = std::env::var("RAGO_TAX_BASE_URL").unwrap_or_else(|_| "https://aggregator.example".into());
    let api_key = std::env::var("RAGO_TAX_API_KEY").unwrap_or_else(|_| "tax_test".into());
    let provider_code = pq.provider.as_deref().unwrap_or("anrok");

    // Select the tax provider; each parses the aggregator's normalized shape.
    let results = match provider_code {
        "avalara" => {
            let provider = AvalaraTaxProvider::new(base_url, api_key, "RAGO", MockTransport::always(200, response));
            provider.fetch_invoice_taxes(&req).await
        }
        _ => {
            let provider = AnrokTaxProvider::new(base_url, api_key, MockTransport::always(200, response));
            provider.fetch_invoice_taxes(&req).await
        }
    }
    .map_err(|e| ApiError::Validation(json!({ "tax_integration": [e.to_string()] })))?;
    let by_id: HashMap<String, _> = results.into_iter().map(|r| (r.item_id.clone(), r)).collect();

    // Apply each fee's provider tax breakdown.
    for fee in fees.iter_mut() {
        if let Some(res) = by_id.get(&fee.id.to_string()) {
            let lines: Vec<ProviderTaxLine> = res
                .tax_breakdown
                .iter()
                .map(|b| ProviderTaxLine {
                    name: b.name.clone(),
                    rate: b.rate,
                    tax_amount: b.tax_amount,
                    type_: b.type_.clone(),
                })
                .collect();
            fee.applied_taxes.clear();
            apply_provider_taxes(fee, &lines);
        }
    }

    // Roll up invoice totals from the (now provider-taxed) fees.
    let taxes_amount_cents: i64 = fees.iter().map(|f| f.taxes_amount_cents).sum();
    invoice.taxes_amount_cents = taxes_amount_cents;
    invoice.sub_total_including_taxes_amount_cents =
        invoice.sub_total_excluding_taxes_amount_cents + taxes_amount_cents;
    invoice.total_amount_cents = invoice.sub_total_including_taxes_amount_cents
        - invoice.credit_notes_amount_cents
        - invoice.prepaid_credit_amount_cents;

    // Persist (idempotent replace of fees' tax state is out of scope; we update
    // the fee tax columns + invoice totals).
    for fee in &fees {
        st.db
            .update_fee_taxes(fee.id, fee.taxes_amount_cents, fee.taxes_rate, fee.taxes_precise_amount_cents)
            .await?;
    }
    st.db
        .update_invoice_totals(invoice.id, invoice.taxes_amount_cents, invoice.sub_total_including_taxes_amount_cents, invoice.total_amount_cents)
        .await?;
    let _ = auth;

    Ok(Json(json!({ "invoice": ser::invoice(&invoice, &fees) })))
}

/// The tax provider's response for these fees at `rate` — the shape Anrok
/// returns (`{ fees: [{ item_id, tax_amount_cents, tax_breakdown }] }`).
fn build_tax_response(fees: &[rago_core::models::Fee], rate: f64, name: &str) -> String {
    let rate_dec = Decimal::from_f64_retain(rate).unwrap_or(Decimal::ZERO);
    let fee_json: Vec<Value> = fees
        .iter()
        .map(|f| {
            let tax = round_to_integer(Decimal::from(f.amount_cents) * rate_dec)
                .to_i64()
                .unwrap_or(0);
            json!({
                "item_id": f.id.to_string(),
                "tax_amount_cents": tax,
                "tax_breakdown": [{ "name": name, "rate": rate, "tax_amount": tax, "type": "sales_tax" }],
            })
        })
        .collect();
    json!({ "fees": fee_json }).to_string()
}
