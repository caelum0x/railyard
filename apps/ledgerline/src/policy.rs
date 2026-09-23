//! Server-side account policy: which Mint accounts an ingest event may move
//! money between. Without it, anyone holding the Ledgerline API key could name
//! arbitrary Mint accounts in the event body and use the hold/capture + COGS
//! transfer saga as a general-purpose money mover.
//!
//! Configured by `LEDGERLINE_ACCOUNT_POLICY_JSON`:
//!
//! ```json
//! {
//!   "revenue_accounts":  ["acct_revenue"],
//!   "cogs_accounts":     ["acct_cogs"],
//!   "provider_accounts": ["acct_openai", "acct_fal"],
//!   "customers":         { "customer-1": "acct_customer_1" },
//!   "allow_unmapped_customers": false
//! }
//! ```
//!
//! Revenue / COGS / provider accounts must be on their allowlists. The customer
//! account must equal the mapping for `customer_id`; an unmapped customer is
//! rejected unless `allow_unmapped_customers` is set (then only a mapped
//! customer is pinned, and unmapped ones may name their own funding account).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::domain::MeterEvent;
use crate::error::LedgerlineError;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AccountPolicy {
    #[serde(default)]
    pub revenue_accounts: HashSet<String>,
    #[serde(default)]
    pub cogs_accounts: HashSet<String>,
    #[serde(default)]
    pub provider_accounts: HashSet<String>,
    #[serde(default)]
    pub customers: HashMap<String, String>,
    #[serde(default)]
    pub allow_unmapped_customers: bool,
    /// Set only by [`AccountPolicy::unrestricted`] (dev/test).
    #[serde(skip)]
    unrestricted: bool,
}

impl AccountPolicy {
    /// Parse and sanity-check the policy JSON.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let policy: AccountPolicy = serde_json::from_str(raw)
            .map_err(|e| format!("LEDGERLINE_ACCOUNT_POLICY_JSON is invalid: {e}"))?;
        if policy.revenue_accounts.is_empty()
            || policy.cogs_accounts.is_empty()
            || policy.provider_accounts.is_empty()
        {
            return Err(
                "LEDGERLINE_ACCOUNT_POLICY_JSON must list at least one revenue_accounts, \
                 cogs_accounts and provider_accounts entry"
                    .into(),
            );
        }
        if policy.customers.is_empty() && !policy.allow_unmapped_customers {
            return Err(
                "LEDGERLINE_ACCOUNT_POLICY_JSON must map customers (customer_id -> account) or \
                 set allow_unmapped_customers"
                    .into(),
            );
        }
        Ok(policy)
    }

    /// No restrictions — for local development and tests ONLY. The production
    /// binary refuses this unless `LEDGERLINE_ALLOW_UNRESTRICTED_ACCOUNTS=1`.
    pub fn unrestricted() -> Self {
        AccountPolicy {
            unrestricted: true,
            ..AccountPolicy::default()
        }
    }

    /// Reject an event that names accounts outside the policy.
    pub fn check(&self, event: &MeterEvent) -> Result<(), LedgerlineError> {
        if self.unrestricted {
            return Ok(());
        }
        let deny = |field: &str| {
            Err(LedgerlineError::Forbidden(format!(
                "{field} is not permitted by the Ledgerline account policy"
            )))
        };
        if !self.revenue_accounts.contains(&event.revenue_account) {
            return deny("revenue_account");
        }
        if !self.cogs_accounts.contains(&event.cogs_account) {
            return deny("cogs_account");
        }
        if !self.provider_accounts.contains(&event.provider_account) {
            return deny("provider_account");
        }
        match self.customers.get(&event.customer_id) {
            Some(bound) if bound == &event.customer_account => {}
            Some(_) => return deny("customer_account"),
            None if self.allow_unmapped_customers => {}
            None => return deny("customer_id"),
        }
        // A policy-controlled internal account must never be drained as if it
        // were a customer.
        if self.revenue_accounts.contains(&event.customer_account)
            || self.cogs_accounts.contains(&event.customer_account)
            || self.provider_accounts.contains(&event.customer_account)
        {
            return deny("customer_account");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EventOutcome, EventSource};

    fn event() -> MeterEvent {
        MeterEvent {
            idempotency_key: "i".into(),
            request_id: "r".into(),
            customer_id: "c1".into(),
            feature: "f".into(),
            model: "m".into(),
            source: EventSource::Sdk,
            outcome: EventOutcome::Succeeded,
            revenue_micros: 2,
            cogs_micros: 1,
            customer_account: "acct_c1".into(),
            revenue_account: "acct_rev".into(),
            cogs_account: "acct_cogs".into(),
            provider_account: "acct_prov".into(),
            source_settlement_receipt: None,
        }
    }

    fn policy() -> AccountPolicy {
        AccountPolicy::from_json(
            r#"{"revenue_accounts":["acct_rev"],"cogs_accounts":["acct_cogs"],
                "provider_accounts":["acct_prov"],"customers":{"c1":"acct_c1"}}"#,
        )
        .unwrap()
    }

    #[test]
    fn allows_policy_conforming_event() {
        assert!(policy().check(&event()).is_ok());
    }

    #[test]
    fn rejects_foreign_accounts() {
        let mut e = event();
        e.revenue_account = "acct_attacker".into();
        assert!(policy().check(&e).is_err());
        let mut e = event();
        e.customer_account = "acct_victim".into();
        assert!(policy().check(&e).is_err());
        let mut e = event();
        e.customer_id = "unknown".into();
        assert!(policy().check(&e).is_err());
    }
}
