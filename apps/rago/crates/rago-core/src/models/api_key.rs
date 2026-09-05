use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// `api_keys` — a bearer token gating the REST API. (Lago
/// `app/models/api_key.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub organization_id: Uuid,
    /// The secret bearer value presented in `Authorization: Bearer <value>`.
    pub value: String,
    pub name: Option<String>,
    /// `{ resource => [modes] }` permission map.
    pub permissions: Value,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ApiKey {
    /// `ApiKey#permit?` — with org-level api permissions disabled (the default),
    /// any valid key is permitted. When enabled, the key must list `mode` for
    /// `resource`.
    pub fn permit(&self, resource: &str, mode: &str, api_permissions_enabled: bool) -> bool {
        if !api_permissions_enabled {
            return true;
        }
        self.permissions
            .get(resource)
            .and_then(Value::as_array)
            .map(|modes| modes.iter().any(|m| m.as_str() == Some(mode)))
            .unwrap_or(false)
    }

    /// Whether the key is expired as of `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }
}
