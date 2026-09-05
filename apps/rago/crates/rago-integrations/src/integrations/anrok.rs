//! Anrok tax integration — faithful port of
//! `Integrations::Aggregator::Taxes::Invoices::CreateService` (Anrok path).
//!
//! POSTs the finalized invoice's fees to `.../v1/anrok/finalized_invoices` and
//! parses the per-fee tax results (`item_id`, `tax_amount_cents`,
//! `tax_breakdown[{ name, rate, tax_amount, type }]`).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    FeeTaxResult, IntegrationError, TaxBreakdownItem, TaxProvider, TaxRequest,
};
use crate::transport::HttpTransport;

/// `Integrations::AnrokIntegration` over an injectable transport `T`.
pub struct AnrokTaxProvider<T: HttpTransport> {
    /// Aggregator base URL (Lago routes through the Nango proxy).
    base_url: String,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> AnrokTaxProvider<T> {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            transport,
        }
    }

    fn action_url(&self) -> String {
        format!("{}/v1/anrok/finalized_invoices", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl<T: HttpTransport> TaxProvider for AnrokTaxProvider<T> {
    fn provider_type(&self) -> &'static str {
        "anrok"
    }

    async fn fetch_invoice_taxes(
        &self,
        req: &TaxRequest,
    ) -> Result<Vec<FeeTaxResult>, IntegrationError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        // `Taxes::Invoices::Payloads` body: one invoice with its fees.
        let body = json!([{
            "id": req.invoice_id,
            "currency": req.currency,
            "contact": { "external_id": req.customer_external_id },
            "fees": req.fees.iter().map(|f| json!({
                "item_id": f.item_id,
                "item_code": f.item_code,
                "amount_cents": f.amount_cents,
            })).collect::<Vec<_>>(),
        }])
        .to_string();

        let resp = self.transport.post(&self.action_url(), &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("code").and_then(Value::as_str).map(str::to_string),
                message: json
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("anrok error")
                    .to_string(),
            });
        }

        // Response `{ "fees": [ { item_id, tax_amount_cents, tax_breakdown: [...] } ] }`.
        let fees = json.get("fees").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(fees.len());
        for fee in fees {
            let item_id = fee
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| IntegrationError::InvalidResponse("missing item_id".into()))?
                .to_string();
            let tax_amount_cents = fee.get("tax_amount_cents").and_then(Value::as_i64).unwrap_or(0);
            let breakdown = fee
                .get("tax_breakdown")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|b| TaxBreakdownItem {
                    name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    rate: b.get("rate").and_then(Value::as_f64).unwrap_or_else(|| {
                        b.get("rate").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0.0)
                    }),
                    tax_amount: b.get("tax_amount").and_then(Value::as_i64).unwrap_or(0),
                    type_: b.get("type").and_then(Value::as_str).unwrap_or("tax").to_string(),
                })
                .collect();
            out.push(FeeTaxResult {
                item_id,
                tax_amount_cents,
                tax_breakdown: breakdown,
            });
        }
        Ok(out)
    }
}
