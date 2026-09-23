//! GraphQL admin read queries — slice 3. List/show for payment providers,
//! provider-customer connections, payment methods, payment requests, and the
//! integration mapping/customer/subsidiary surface. Merged into the schema's
//! `Query`; the `integrations` list lives in `query::Query`.

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{db, require_org};
use crate::types::Integration;
use crate::types_admin3::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

/// Admin query root — slice 3 (merged into the schema's `Query`).
#[derive(Default)]
pub struct AdminQuery3;

#[Object]
impl AdminQuery3 {
    // ---- payment providers ----------------------------------------------

    /// `paymentProviders`.
    async fn payment_providers(&self, ctx: &Context<'_>) -> Result<Vec<PaymentProvider>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_payment_providers(org.id).await?.into_iter().map(Into::into).collect())
    }

    /// `paymentProvider(id:)`.
    async fn payment_provider(&self, ctx: &Context<'_>, id: ID) -> Result<Option<PaymentProvider>> {
        require_org(ctx)?;
        Ok(db(ctx).find_payment_provider(uuid(&id)?).await?.map(Into::into))
    }

    /// `paymentProviderCustomers(customerId:)`.
    async fn payment_provider_customers(&self, ctx: &Context<'_>, customer_id: ID) -> Result<Vec<PaymentProviderCustomer>> {
        require_org(ctx)?;
        Ok(db(ctx).list_payment_provider_customers(uuid(&customer_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `paymentMethods(customerId:)`.
    async fn payment_methods(&self, ctx: &Context<'_>, customer_id: ID) -> Result<Vec<PaymentMethod>> {
        require_org(ctx)?;
        Ok(db(ctx).list_payment_methods(uuid(&customer_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `paymentRequests(externalCustomerId:)`.
    async fn payment_requests(&self, ctx: &Context<'_>, external_customer_id: String) -> Result<Vec<PaymentRequest>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_payment_requests(org.id, &external_customer_id).await?.into_iter().map(Into::into).collect())
    }

    // ---- integrations ----------------------------------------------------

    /// `integration(id:)`.
    async fn integration(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Integration>> {
        require_org(ctx)?;
        Ok(db(ctx).find_integration(uuid(&id)?).await?.map(Into::into))
    }

    /// `integrationCollectionMappings(integrationId:)`.
    async fn integration_collection_mappings(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationCollectionMapping>> {
        require_org(ctx)?;
        Ok(db(ctx).list_integration_collection_mappings(uuid(&integration_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `integrationMappings(integrationId:)`.
    async fn integration_mappings(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationMapping>> {
        require_org(ctx)?;
        Ok(db(ctx).list_integration_mappings(uuid(&integration_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `integrationCustomers(customerId:)`.
    async fn integration_customers(&self, ctx: &Context<'_>, customer_id: ID) -> Result<Vec<IntegrationCustomer>> {
        require_org(ctx)?;
        Ok(db(ctx).list_integration_customers(uuid(&customer_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `integrationItems(integrationId:)` — the locally cached fetched items.
    async fn integration_items(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationItem>> {
        require_org(ctx)?;
        Ok(db(ctx).list_integration_items(uuid(&integration_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `integrationSubsidiaries(integrationId:)` — the NetSuite subsidiaries
    /// cached by the last catalog fetch (`fetchIntegrationItems` or the
    /// console's "Refresh catalog"), read through SuiteQL with the
    /// integration's own TBA credentials.
    async fn integration_subsidiaries(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationItem>> {
        require_org(ctx)?;
        let rows = db(ctx).integration_items_of_type(uuid(&integration_id)?, Some("subsidiary")).await?;
        Ok(rows
            .into_iter()
            .map(|(id, i)| IntegrationItem {
                id: id.into(),
                external_id: i.external_id,
                item_type: Some(i.item_type),
                external_name: i.external_name,
                external_account_code: i.external_account_code,
            })
            .collect())
    }
}
