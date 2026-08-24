//! The enforcement decision: the single point where Abacus either lets a request
//! through (with a hold reserved) or kills it before a single upstream byte is
//! sent.

use serde::{Deserialize, Serialize};

use crate::error::AbacusError;

/// The output of the reserve gate.
///
/// - [`Allow`](EnforcementDecision::Allow) carries the opened `hold_id`; the hot
///   path forwards the request and later captures the EXACT actual against it.
/// - [`Kill`](EnforcementDecision::Kill) carries the reason + the HTTP `status`
///   to return to the caller; NO upstream request is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum EnforcementDecision {
    /// Budget has room; a Mint hold reserving `est_max` is open.
    Allow {
        /// The opened Mint hold id, threaded to settle.
        hold_id: String,
    },
    /// Budget would be breached; the request is refused at the edge.
    Kill {
        /// Stable machine reason (the offending error's code).
        reason: String,
        /// HTTP status to surface to the caller.
        status: u16,
    },
}

impl EnforcementDecision {
    /// An allow decision carrying the opened hold id.
    pub fn allow(hold_id: impl Into<String>) -> Self {
        EnforcementDecision::Allow {
            hold_id: hold_id.into(),
        }
    }

    /// A kill decision derived from the enforcing error's code + status.
    pub fn kill(err: &AbacusError) -> Self {
        EnforcementDecision::Kill {
            reason: err.code().to_string(),
            status: err.status_for(),
        }
    }

    /// `true` iff the request may proceed upstream.
    pub fn is_allowed(&self) -> bool {
        matches!(self, EnforcementDecision::Allow { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_carries_reason_and_status() {
        let d = EnforcementDecision::kill(&AbacusError::AllowanceExceeded);
        match d {
            EnforcementDecision::Kill { reason, status } => {
                assert_eq!(reason, "allowance_exceeded");
                assert_eq!(status, 402);
            }
            _ => panic!("expected kill"),
        }
    }

    #[test]
    fn allow_is_allowed() {
        assert!(EnforcementDecision::allow("hold_x").is_allowed());
        assert!(!EnforcementDecision::kill(&AbacusError::InsufficientFunds).is_allowed());
    }
}
