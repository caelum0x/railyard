use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `taxes` — a tax rate. (Lago `app/models/tax.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tax {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    /// Percentage rate, e.g. `20.0` for 20%.
    pub rate: f64,
    pub applied_to_organization: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
