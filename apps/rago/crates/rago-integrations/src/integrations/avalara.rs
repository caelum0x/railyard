//! Avalara tax integration — representative port of
//! `Integrations::Aggregator::Taxes::Invoices::CreateService` (Avalara path).
//!
//! Avalara models an invoice as a `CreateTransactionModel` (`type: SalesInvoice`
//! with per-fee `lines`). We POST it through the aggregator
//! (`.../v1/avalara/finalized_invoices`) and parse the same normalized per-fee
//! tax result the aggregator returns for every tax provider.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{FeeTaxResult, IntegrationError, TaxBreakdownItem, TaxProvider, TaxRequest};
use crate::transport::HttpTransport;

/// `Integrations::AvalaraIntegration` over an injectable transport `T`.
pub struct AvalaraTaxProvider<T: HttpTransport> {
    base_url: String,
    api_key: String,
    /// Avalara company code (`companyCode` in the transaction model).
    company_code: String,
    transport: T,
}

impl<T: HttpTransport> AvalaraTaxProvider<T> {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        company_code: impl Into<String>,
        transport: T,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            company_code: company_code.into(),
            transport,
        }
    }

    fn action_url(&self) -> String {
        format!("{}/v1/avalara/finalized_invoices", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl<T: HttpTransport> TaxProvider for AvalaraTaxProvider<T> {
    fn provider_type(&self) -> &'static str {
        "avalara"
    }

    async fn fetch_invoice_taxes(&self, req: &TaxRequest) -> Result<Vec<FeeTaxResult>, IntegrationError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        // Avalara `CreateTransactionModel`: a SalesInvoice with per-fee lines.
        let body = json!([{
            "type": "SalesInvoice",
            "companyCode": self.company_code,
            "code": req.invoice_id,
            "currencyCode": req.currency,
            "customerCode": req.customer_external_id,
            "lines": req.fees.iter().enumerate().map(|(i, f)| json!({
                "number": (i + 1).to_string(),
                "itemCode": f.item_code,
                "taxCode": f.item_code,
                "lago_item_id": f.item_id,
                "amount": f.amount_cents,
            })).collect::<Vec<_>>(),
        }])
        .to_string();

        let resp = self.transport.post(&self.action_url(), &headers, body).await?;
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("code").and_then(Value::as_str).map(str::to_string),
                message: json.get("error").and_then(Value::as_str).unwrap_or("avalara error").to_string(),
            });
        }

        // The aggregator normalizes every tax provider to the same shape:
        // `{ fees: [ { item_id, tax_amount_cents, tax_breakdown: [...] } ] }`.
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
            out.push(FeeTaxResult { item_id, tax_amount_cents, tax_breakdown: breakdown });
        }
        Ok(out)
    }
}
