//! Tenant isolation — the organization-ownership guard shared by the REST and
//! GraphQL surfaces.
//!
//! Every org-scoped table carries `id UUID` + `organization_id UUID`. Many
//! repository finders look records up by primary key alone (they predate the
//! multi-tenant surfaces), so the API layers call [`Db::foreign_ids`] with every
//! UUID a request references *before* any handler/resolver runs: a UUID that
//! exists in another organization's rows is rejected as "not found". UUIDs that
//! exist nowhere (new ids, typos) pass through and fail naturally in the
//! handler. Primary keys are v4 UUIDs, so a UUID identifies at most one record.

use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// Every table with `id UUID` + `organization_id UUID` (see `migrations/`).
/// Static identifiers only — never user input — so the query is injection-free.
pub const TENANT_TABLES: &[&str] = &[
    "customers", "billable_metrics", "plans", "charges", "subscriptions", "taxes",
    "invoices", "fees", "api_keys", "wallets", "wallet_transactions", "coupons",
    "applied_coupons", "credits", "credit_notes", "invoice_subscriptions", "payments",
    "webhooks", "webhook_endpoints", "payment_provider_customers", "entitlement_features",
    "entitlement_privileges", "entitlement_entitlements", "entitlement_entitlement_values",
    "activity_logs", "audit_logs", "add_ons", "quotes", "data_exports",
    "wallet_recurring_rules", "integration_connections", "integration_mappings",
    "usage_alerts", "usage_alert_triggers", "dunning_campaigns", "lifetime_usages",
    "daily_usages", "inbound_webhooks", "charge_filters", "credit_note_items", "alerts",
    "alert_thresholds", "billing_entities", "invoice_custom_sections", "adjusted_fees",
    "fixed_charges", "payment_providers", "payment_methods", "payment_requests",
    "integration_customers", "integration_collection_mappings", "integration_items",
    "memberships", "roles", "invites", "billing_entity_taxes", "product_categories",
    "products", "product_filters", "rate_cards", "rate_card_rates",
    "plan_applied_rate_cards", "rate_phases", "pricing_units", "commitments",
    "quote_versions", "order_forms", "orders", "ai_conversations", "ai_messages",
    "finance_assistant_messages", "dunning_campaign_thresholds",
    "payment_request_applied_invoices", "webhook_deliveries", "events",
];

/// Upper bound on ids checked per request (defensive: a request referencing more
/// UUIDs than this is rejected by the callers rather than scanned).
pub const MAX_CHECKED_IDS: usize = 500;

fn foreign_ids_sql() -> String {
    let mut parts: Vec<String> = TENANT_TABLES
        .iter()
        .map(|t| format!("SELECT id FROM {t} WHERE id = ANY($1) AND organization_id <> $2"))
        .collect();
    // Another organization's own id is foreign too.
    parts.push("SELECT id FROM organizations WHERE id = ANY($1) AND id <> $2".to_string());
    parts.join(" UNION ALL ")
}

impl Db {
    /// The subset of `ids` that belong to an organization other than `org`.
    /// Empty means every id is either owned by `org` or does not exist.
    pub async fn foreign_ids(&self, org: Uuid, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&foreign_ids_sql())
            .bind(ids)
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get::<Uuid, _>("id")).collect())
    }

    /// `true` when `id` is not owned by another organization.
    pub async fn not_foreign(&self, org: Uuid, id: Uuid) -> Result<bool> {
        Ok(self.foreign_ids(org, &[id]).await?.is_empty())
    }
}

/// Extract every UUID literal embedded in `text` (hyphenated 8-4-4-4-12 form).
pub fn uuids_in_text(text: &str) -> Vec<Uuid> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    if bytes.len() < 36 {
        return out;
    }
    let mut i = 0;
    while i + 36 <= bytes.len() {
        let window = &text[i..i + 36];
        let boundary_ok = (i == 0 || !bytes[i - 1].is_ascii_hexdigit())
            && (i + 36 == bytes.len() || !bytes[i + 36].is_ascii_hexdigit());
        if boundary_ok && looks_like_uuid(window.as_bytes()) {
            if let Ok(u) = Uuid::parse_str(window) {
                if !out.contains(&u) {
                    out.push(u);
                }
                i += 36;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn looks_like_uuid(b: &[u8]) -> bool {
    b.len() == 36
        && b.iter().enumerate().all(|(idx, c)| match idx {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_uuids_in_text() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let s = format!("/api/v1/invoices/{a}/void?x={b}&y={a}");
        assert_eq!(uuids_in_text(&s), vec![a, b]);
        assert!(uuids_in_text("no ids here").is_empty());
    }
}
