//! Repository extensions for the GraphQL dashboard admin CRUD surface — slice 3:
//! the external-service config cluster (payment providers + integrations
//! orchestration). These rows persist provider CONFIG and MAPPINGS only; secret
//! material is referenced by a masked `secret_ref`, never stored raw. The actual
//! remote calls live in `rago-integrations` and require owner-provisioned
//! credentials.

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

// ---- row structs -----------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PaymentProviderRow {
    pub id: Uuid,
    pub provider_type: String,
    pub code: String,
    pub name: Option<String>,
    pub secret_ref: Option<String>,
    pub success_redirect_url: Option<String>,
    pub settings: Value,
    /// Masked (`****last4`) inbound webhook signing secret, if one is stored.
    pub webhook_secret_ref: Option<String>,
}

fn payment_provider_row(r: &sqlx::postgres::PgRow) -> PaymentProviderRow {
    PaymentProviderRow {
        id: r.get("id"),
        provider_type: r.get("provider_type"),
        code: r.get("code"),
        name: r.get("name"),
        secret_ref: r.get("secret_ref"),
        webhook_secret_ref: r.try_get("webhook_secret_ref").ok().flatten(),
        success_redirect_url: r.get("success_redirect_url"),
        settings: r.get("settings"),
    }
}

#[derive(Debug, Clone)]
pub struct PaymentProviderCustomerRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub provider: String,
    pub provider_customer_id: Option<String>,
    pub payment_provider_code: Option<String>,
    pub is_default: bool,
}

fn ppc_row(r: &sqlx::postgres::PgRow) -> PaymentProviderCustomerRow {
    PaymentProviderCustomerRow {
        id: r.get("id"),
        customer_id: r.get("customer_id"),
        provider: r.get("provider"),
        provider_customer_id: r.get("provider_customer_id"),
        payment_provider_code: r.get("payment_provider_code"),
        is_default: r.get("is_default"),
    }
}

/// A payment-method row `(id, customer_id, provider_method_id, is_default)`.
pub type PaymentMethodRow = (Uuid, Uuid, Option<String>, bool);

/// A payment-request row `(id, external_customer_id, email, amount_cents,
/// amount_currency, payment_status)`.
pub type PaymentRequestRow = (Uuid, Option<String>, Option<String>, i64, Option<String>, String);

#[derive(Debug, Clone)]
pub struct IntegrationRow {
    pub id: Uuid,
    pub integration_type: String,
    pub provider_code: String,
    pub code: Option<String>,
    pub name: Option<String>,
    pub settings: Value,
}

fn integration_row(r: &sqlx::postgres::PgRow) -> IntegrationRow {
    IntegrationRow {
        id: r.get("id"),
        integration_type: r.get("integration_type"),
        provider_code: r.get("provider_code"),
        code: r.get("code"),
        name: r.get("name"),
        settings: r.get("settings"),
    }
}

#[derive(Debug, Clone)]
pub struct IntegrationCustomerRow {
    pub id: Uuid,
    pub integration_id: Uuid,
    pub customer_id: Uuid,
    pub external_customer_id: Option<String>,
    pub code: Option<String>,
    pub is_default: bool,
}

fn integration_customer_row(r: &sqlx::postgres::PgRow) -> IntegrationCustomerRow {
    IntegrationCustomerRow {
        id: r.get("id"),
        integration_id: r.get("integration_id"),
        customer_id: r.get("customer_id"),
        external_customer_id: r.get("external_customer_id"),
        code: r.get("code"),
        is_default: r.get("is_default"),
    }
}

#[derive(Debug, Clone)]
pub struct IntegrationCollectionMappingRow {
    pub id: Uuid,
    pub integration_id: Uuid,
    pub mapping_type: String,
    pub external_id: Option<String>,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

fn icm_row(r: &sqlx::postgres::PgRow) -> IntegrationCollectionMappingRow {
    IntegrationCollectionMappingRow {
        id: r.get("id"),
        integration_id: r.get("integration_id"),
        mapping_type: r.get("mapping_type"),
        external_id: r.get("external_id"),
        external_account_code: r.get("external_account_code"),
        external_name: r.get("external_name"),
    }
}

#[derive(Debug, Clone)]
pub struct IntegrationMappingRow {
    pub id: Uuid,
    pub integration_connection_id: Uuid,
    pub mappable_type: String,
    pub mappable_id: Uuid,
    pub external_id: String,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

fn integration_mapping_row(r: &sqlx::postgres::PgRow) -> IntegrationMappingRow {
    IntegrationMappingRow {
        id: r.get("id"),
        integration_connection_id: r.get("integration_connection_id"),
        mappable_type: r.get("resource_type"),
        mappable_id: r.get("lago_id"),
        external_id: r.get("external_id"),
        external_account_code: r.get("external_account_code"),
        external_name: r.get("external_name"),
    }
}

/// An integration item `(id, external_id, item_type, external_name, external_account_code)`.
pub type IntegrationItemRow = (Uuid, String, Option<String>, Option<String>, Option<String>);

impl Db {
    // ---- payment providers ----------------------------------------------

    /// Upsert a payment-provider config by `(organization, code)`
    /// (`PaymentProviders::<Provider>Service#create_or_update`).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_payment_provider(
        &self,
        org: Uuid,
        provider_type: &str,
        code: &str,
        name: Option<&str>,
        secret_ref: Option<&str>,
        success_redirect_url: Option<&str>,
        settings: &Value,
    ) -> Result<PaymentProviderRow> {
        let existing = sqlx::query("SELECT id FROM payment_providers WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL")
            .bind(org)
            .bind(code)
            .fetch_optional(&self.pool)
            .await?;
        let id = if let Some(r) = existing {
            let id: Uuid = r.get("id");
            sqlx::query(
                "UPDATE payment_providers SET provider_type = $2, name = $3, \
                 secret_ref = COALESCE($4, secret_ref), success_redirect_url = COALESCE($5, success_redirect_url), \
                 settings = $6, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(provider_type)
            .bind(name)
            .bind(secret_ref)
            .bind(success_redirect_url)
            .bind(settings)
            .execute(&self.pool)
            .await?;
            id
        } else {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO payment_providers (id, organization_id, provider_type, code, name, secret_ref, success_redirect_url, settings, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())",
            )
            .bind(id)
            .bind(org)
            .bind(provider_type)
            .bind(code)
            .bind(name)
            .bind(secret_ref)
            .bind(success_redirect_url)
            .bind(settings)
            .execute(&self.pool)
            .await?;
            id
        };
        self.find_payment_provider(id).await.map(|o| o.expect("just upserted"))
    }

    pub async fn find_payment_provider(&self, id: Uuid) -> Result<Option<PaymentProviderRow>> {
        let row = sqlx::query("SELECT * FROM payment_providers WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(payment_provider_row))
    }

    pub async fn list_payment_providers(&self, org: Uuid) -> Result<Vec<PaymentProviderRow>> {
        let rows = sqlx::query("SELECT * FROM payment_providers WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(payment_provider_row).collect())
    }

    /// The default (first configured) provider config of a given type, if any.
    pub async fn default_payment_provider(&self, org: Uuid) -> Result<Option<PaymentProviderRow>> {
        let row = sqlx::query("SELECT * FROM payment_providers WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at LIMIT 1")
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(payment_provider_row))
    }

    pub async fn soft_delete_payment_provider(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE payment_providers SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- payment provider customers -------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_payment_provider_customer(
        &self,
        id: Uuid,
        org: Uuid,
        customer_id: Uuid,
        provider: &str,
        provider_customer_id: Option<&str>,
        code: Option<&str>,
    ) -> Result<PaymentProviderCustomerRow> {
        sqlx::query(
            "INSERT INTO payment_provider_customers (id, organization_id, customer_id, provider, provider_customer_id, payment_provider_code, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(customer_id)
        .bind(provider)
        .bind(provider_customer_id)
        .bind(code)
        .execute(&self.pool)
        .await?;
        Ok(self.find_payment_provider_customer(id).await?.expect("just inserted"))
    }

    pub async fn update_payment_provider_customer(
        &self,
        id: Uuid,
        provider_customer_id: Option<&str>,
        code: Option<&str>,
    ) -> Result<Option<PaymentProviderCustomerRow>> {
        sqlx::query(
            "UPDATE payment_provider_customers SET provider_customer_id = COALESCE($2, provider_customer_id), \
             payment_provider_code = COALESCE($3, payment_provider_code), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(provider_customer_id)
        .bind(code)
        .execute(&self.pool)
        .await?;
        self.find_payment_provider_customer(id).await
    }

    pub async fn find_payment_provider_customer(&self, id: Uuid) -> Result<Option<PaymentProviderCustomerRow>> {
        let row = sqlx::query("SELECT * FROM payment_provider_customers WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(ppc_row))
    }

    pub async fn list_payment_provider_customers(&self, customer_id: Uuid) -> Result<Vec<PaymentProviderCustomerRow>> {
        let rows = sqlx::query("SELECT * FROM payment_provider_customers WHERE customer_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(customer_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(ppc_row).collect())
    }

    pub async fn soft_delete_payment_provider_customer(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE payment_provider_customers SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Set a provider-customer connection as the customer's default (clears the
    /// others for that customer).
    pub async fn set_default_payment_provider_customer(&self, id: Uuid) -> Result<Option<PaymentProviderCustomerRow>> {
        let mut tx = self.pool.begin().await?;
        let cust: Option<Uuid> = sqlx::query("SELECT customer_id FROM payment_provider_customers WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get("customer_id"));
        if let Some(customer_id) = cust {
            sqlx::query("UPDATE payment_provider_customers SET is_default = false, updated_at = now() WHERE customer_id = $1")
                .bind(customer_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE payment_provider_customers SET is_default = true, updated_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.find_payment_provider_customer(id).await
    }

    // ---- payment methods -------------------------------------------------

    pub async fn insert_payment_method(&self, id: Uuid, org: Uuid, customer_id: Uuid, provider_method_id: Option<&str>, is_default: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO payment_methods (id, organization_id, customer_id, provider_method_id, is_default, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(customer_id)
        .bind(provider_method_id)
        .bind(is_default)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_payment_method(&self, id: Uuid) -> Result<Option<PaymentMethodRow>> {
        let row = sqlx::query("SELECT id, customer_id, provider_method_id, is_default FROM payment_methods WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("customer_id"), r.get("provider_method_id"), r.get("is_default"))))
    }

    pub async fn list_payment_methods(&self, customer_id: Uuid) -> Result<Vec<PaymentMethodRow>> {
        let rows = sqlx::query("SELECT id, customer_id, provider_method_id, is_default FROM payment_methods WHERE customer_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(customer_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("customer_id"), r.get("provider_method_id"), r.get("is_default"))).collect())
    }

    pub async fn soft_delete_payment_method(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE payment_methods SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn set_default_payment_method(&self, id: Uuid) -> Result<Option<PaymentMethodRow>> {
        let mut tx = self.pool.begin().await?;
        let cust: Option<Uuid> = sqlx::query("SELECT customer_id FROM payment_methods WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get("customer_id"));
        if let Some(customer_id) = cust {
            sqlx::query("UPDATE payment_methods SET is_default = false, updated_at = now() WHERE customer_id = $1").bind(customer_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE payment_methods SET is_default = true, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.find_payment_method(id).await
    }

    // ---- payment requests ------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_payment_request(
        &self,
        id: Uuid,
        org: Uuid,
        customer_id: Option<Uuid>,
        external_customer_id: &str,
        email: Option<&str>,
        amount_cents: i64,
        amount_currency: Option<&str>,
    ) -> Result<PaymentRequestRow> {
        sqlx::query(
            "INSERT INTO payment_requests (id, organization_id, customer_id, external_customer_id, email, amount_cents, amount_currency, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(customer_id)
        .bind(external_customer_id)
        .bind(email)
        .bind(amount_cents)
        .bind(amount_currency)
        .execute(&self.pool)
        .await?;
        Ok(self.find_payment_request(id).await?.expect("just inserted"))
    }

    pub async fn find_payment_request(&self, id: Uuid) -> Result<Option<PaymentRequestRow>> {
        let row = sqlx::query("SELECT id, external_customer_id, email, amount_cents, amount_currency, payment_status FROM payment_requests WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("external_customer_id"), r.get("email"), r.get("amount_cents"), r.get("amount_currency"), r.get("payment_status"))))
    }

    /// Find a payment request scoped to an organization — the tenant-safe
    /// lookup for API handlers. A cross-org id resolves to `None`.
    pub async fn find_payment_request_for_org(&self, org: Uuid, id: Uuid) -> Result<Option<PaymentRequestRow>> {
        let row = sqlx::query("SELECT id, external_customer_id, email, amount_cents, amount_currency, payment_status FROM payment_requests WHERE id = $1 AND organization_id = $2")
            .bind(id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("external_customer_id"), r.get("email"), r.get("amount_cents"), r.get("amount_currency"), r.get("payment_status"))))
    }

    pub async fn list_payment_requests(&self, org: Uuid, external_customer_id: &str) -> Result<Vec<PaymentRequestRow>> {
        let rows = sqlx::query("SELECT id, external_customer_id, email, amount_cents, amount_currency, payment_status FROM payment_requests WHERE organization_id = $1 AND external_customer_id = $2 ORDER BY created_at DESC")
            .bind(org)
            .bind(external_customer_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("external_customer_id"), r.get("email"), r.get("amount_cents"), r.get("amount_currency"), r.get("payment_status"))).collect())
    }

    // ---- integrations ----------------------------------------------------

    /// Insert an integration config row (`Integrations::<Provider>::CreateService`).
    pub async fn insert_integration(
        &self,
        id: Uuid,
        org: Uuid,
        integration_type: &str,
        provider_code: &str,
        code: Option<&str>,
        name: Option<&str>,
        settings: &Value,
    ) -> Result<IntegrationRow> {
        sqlx::query(
            "INSERT INTO integration_connections (id, organization_id, integration_type, provider_code, code, name, settings, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(integration_type)
        .bind(provider_code)
        .bind(code)
        .bind(name)
        .bind(settings)
        .execute(&self.pool)
        .await?;
        Ok(self.find_integration(id).await?.expect("just inserted"))
    }

    pub async fn update_integration(&self, id: Uuid, code: Option<&str>, name: Option<&str>, settings: Option<&Value>) -> Result<Option<IntegrationRow>> {
        sqlx::query(
            "UPDATE integration_connections SET code = COALESCE($2, code), name = COALESCE($3, name), \
             settings = COALESCE($4, settings), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(code)
        .bind(name)
        .bind(settings)
        .execute(&self.pool)
        .await?;
        self.find_integration(id).await
    }

    pub async fn find_integration(&self, id: Uuid) -> Result<Option<IntegrationRow>> {
        let row = sqlx::query("SELECT * FROM integration_connections WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(integration_row))
    }

    pub async fn list_integrations(&self, org: Uuid) -> Result<Vec<IntegrationRow>> {
        let rows = sqlx::query("SELECT * FROM integration_connections WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(integration_row).collect())
    }

    pub async fn soft_delete_integration(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE integration_connections SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- integration customers ------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_integration_customer(
        &self,
        id: Uuid,
        org: Uuid,
        integration_id: Uuid,
        customer_id: Uuid,
        external_customer_id: Option<&str>,
        code: Option<&str>,
    ) -> Result<IntegrationCustomerRow> {
        sqlx::query(
            "INSERT INTO integration_customers (id, organization_id, integration_id, customer_id, external_customer_id, code, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(integration_id)
        .bind(customer_id)
        .bind(external_customer_id)
        .bind(code)
        .execute(&self.pool)
        .await?;
        Ok(self.find_integration_customer(id).await?.expect("just inserted"))
    }

    pub async fn update_integration_customer(&self, id: Uuid, external_customer_id: Option<&str>, code: Option<&str>) -> Result<Option<IntegrationCustomerRow>> {
        sqlx::query(
            "UPDATE integration_customers SET external_customer_id = COALESCE($2, external_customer_id), \
             code = COALESCE($3, code), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(external_customer_id)
        .bind(code)
        .execute(&self.pool)
        .await?;
        self.find_integration_customer(id).await
    }

    pub async fn find_integration_customer(&self, id: Uuid) -> Result<Option<IntegrationCustomerRow>> {
        let row = sqlx::query("SELECT * FROM integration_customers WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(integration_customer_row))
    }

    pub async fn list_integration_customers(&self, customer_id: Uuid) -> Result<Vec<IntegrationCustomerRow>> {
        let rows = sqlx::query("SELECT * FROM integration_customers WHERE customer_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(customer_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(integration_customer_row).collect())
    }

    pub async fn soft_delete_integration_customer(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE integration_customers SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn set_default_integration_customer(&self, id: Uuid) -> Result<Option<IntegrationCustomerRow>> {
        let mut tx = self.pool.begin().await?;
        let cust: Option<Uuid> = sqlx::query("SELECT customer_id FROM integration_customers WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get("customer_id"));
        if let Some(customer_id) = cust {
            sqlx::query("UPDATE integration_customers SET is_default = false, updated_at = now() WHERE customer_id = $1").bind(customer_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE integration_customers SET is_default = true, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.find_integration_customer(id).await
    }

    // ---- integration collection mappings --------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_integration_collection_mapping(
        &self,
        id: Uuid,
        org: Uuid,
        integration_id: Uuid,
        mapping_type: &str,
        external_id: Option<&str>,
        external_account_code: Option<&str>,
        external_name: Option<&str>,
    ) -> Result<IntegrationCollectionMappingRow> {
        sqlx::query(
            "INSERT INTO integration_collection_mappings (id, organization_id, integration_id, mapping_type, external_id, external_account_code, external_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(integration_id)
        .bind(mapping_type)
        .bind(external_id)
        .bind(external_account_code)
        .bind(external_name)
        .execute(&self.pool)
        .await?;
        Ok(self.find_integration_collection_mapping(id).await?.expect("just inserted"))
    }

    pub async fn update_integration_collection_mapping(
        &self,
        id: Uuid,
        external_id: Option<&str>,
        external_account_code: Option<&str>,
        external_name: Option<&str>,
    ) -> Result<Option<IntegrationCollectionMappingRow>> {
        sqlx::query(
            "UPDATE integration_collection_mappings SET external_id = COALESCE($2, external_id), \
             external_account_code = COALESCE($3, external_account_code), external_name = COALESCE($4, external_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(external_id)
        .bind(external_account_code)
        .bind(external_name)
        .execute(&self.pool)
        .await?;
        self.find_integration_collection_mapping(id).await
    }

    pub async fn find_integration_collection_mapping(&self, id: Uuid) -> Result<Option<IntegrationCollectionMappingRow>> {
        let row = sqlx::query("SELECT * FROM integration_collection_mappings WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(icm_row))
    }

    pub async fn list_integration_collection_mappings(&self, integration_id: Uuid) -> Result<Vec<IntegrationCollectionMappingRow>> {
        let rows = sqlx::query("SELECT * FROM integration_collection_mappings WHERE integration_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(integration_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(icm_row).collect())
    }

    pub async fn soft_delete_integration_collection_mapping(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE integration_collection_mappings SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- integration mappings (mappable resources) ----------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_integration_mapping_v2(
        &self,
        id: Uuid,
        org: Uuid,
        integration_id: Uuid,
        mappable_type: &str,
        mappable_id: Uuid,
        external_id: &str,
        external_account_code: Option<&str>,
        external_name: Option<&str>,
    ) -> Result<IntegrationMappingRow> {
        sqlx::query(
            "INSERT INTO integration_mappings (id, organization_id, integration_connection_id, resource_type, lago_id, external_id, external_account_code, external_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())",
        )
        .bind(id)
        .bind(org)
        .bind(integration_id)
        .bind(mappable_type)
        .bind(mappable_id)
        .bind(external_id)
        .bind(external_account_code)
        .bind(external_name)
        .execute(&self.pool)
        .await?;
        Ok(self.find_integration_mapping_by_id(id).await?.expect("just inserted"))
    }

    pub async fn update_integration_mapping_v2(
        &self,
        id: Uuid,
        external_id: Option<&str>,
        external_account_code: Option<&str>,
        external_name: Option<&str>,
    ) -> Result<Option<IntegrationMappingRow>> {
        sqlx::query(
            "UPDATE integration_mappings SET external_id = COALESCE($2, external_id), \
             external_account_code = COALESCE($3, external_account_code), external_name = COALESCE($4, external_name), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(external_id)
        .bind(external_account_code)
        .bind(external_name)
        .execute(&self.pool)
        .await?;
        self.find_integration_mapping_by_id(id).await
    }

    pub async fn find_integration_mapping_by_id(&self, id: Uuid) -> Result<Option<IntegrationMappingRow>> {
        let row = sqlx::query("SELECT * FROM integration_mappings WHERE id = $1 AND (deleted_at IS NULL)")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(integration_mapping_row))
    }

    pub async fn list_integration_mappings(&self, integration_id: Uuid) -> Result<Vec<IntegrationMappingRow>> {
        let rows = sqlx::query("SELECT * FROM integration_mappings WHERE integration_connection_id = $1 AND deleted_at IS NULL ORDER BY created_at")
            .bind(integration_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(integration_mapping_row).collect())
    }

    pub async fn soft_delete_integration_mapping(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE integration_mappings SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- integration items (fetched cache) ------------------------------

    pub async fn list_integration_items(&self, integration_id: Uuid) -> Result<Vec<IntegrationItemRow>> {
        let rows = sqlx::query("SELECT id, external_id, item_type, external_name, external_account_code FROM integration_items WHERE integration_id = $1 ORDER BY created_at")
            .bind(integration_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("external_id"), r.get("item_type"), r.get("external_name"), r.get("external_account_code"))).collect())
    }
}
