//! Port of `cache/charges.go`.

use crate::models::Charge;
use crate::utils::result::Result;

use super::store::Cache;

pub const CHARGE_PREFIX: &str = "ch";
pub const CHARGE_MODEL_NAME: &str = "charges";
pub const CHARGE_TOPIC: &str = ".public.charges";

impl Cache {
    pub fn build_charge_key(
        &self,
        organization_id: &str,
        plan_id: &str,
        billable_metric_id: &str,
        id: &str,
    ) -> String {
        format!("{CHARGE_PREFIX}:{organization_id}:{plan_id}:{billable_metric_id}:{id}")
    }

    pub fn set_charge(&self, ch: &Charge) -> Result<bool> {
        let key = self.build_charge_key(
            &ch.organization_id,
            &ch.plan_id,
            &ch.billable_metric_id,
            &ch.id,
        );
        self.set_json(&key, ch)
    }

    pub fn get_charge(
        &self,
        organization_id: &str,
        plan_id: &str,
        billable_metric_id: &str,
        id: &str,
    ) -> Result<Charge> {
        let key = self.build_charge_key(organization_id, plan_id, billable_metric_id, id);
        self.get_json::<Charge>(&key)
    }

    pub fn search_charge(
        &self,
        organization_id: &str,
        plan_id: &str,
        billable_metric_id: &str,
    ) -> Result<Vec<Charge>> {
        let key = format!("{CHARGE_PREFIX}:{organization_id}:{plan_id}:{billable_metric_id}:");
        self.search_json::<Charge>(&key)
    }

    pub fn delete_charge(&self, ch: &Charge) -> Result<bool> {
        let key = self.build_charge_key(
            &ch.organization_id,
            &ch.plan_id,
            &ch.billable_metric_id,
            &ch.id,
        );
        self.delete(&key)
    }
}
