//! Admin mutations — slice 6: the quote → order form step of the order flow
//! (`CreateOrderForm`). Signing and execution live in slice 5 and go through
//! [`rago_jobs::orders`].

use async_graphql::{Context, Object, Result, ID};

use crate::context::{db, require_org};
use crate::types_admin5::OrderForm;

/// Map an order-flow refusal to a GraphQL error.
pub(crate) fn order_flow_error(e: rago_jobs::orders::OrderFlowError) -> async_graphql::Error {
    match e {
        rago_jobs::orders::OrderFlowError::Invalid(code) => async_graphql::Error::new(code),
        rago_jobs::orders::OrderFlowError::Job(err) => {
            async_graphql::Error::new(format!("order_flow_failed: {err}"))
        }
    }
}

/// Admin CRUD mutation root — slice 6 (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation6;

#[Object]
impl AdminMutation6 {
    /// `CreateOrderForm` — issue the quote's approved version as an order form
    /// for the customer to sign. Returns the existing open form if one exists.
    async fn create_order_form(&self, ctx: &Context<'_>, quote_id: ID) -> Result<OrderForm> {
        let org = require_org(ctx)?;
        let quote_id = uuid::Uuid::parse_str(&quote_id).map_err(|_| async_graphql::Error::new("invalid id"))?;
        let form = rago_jobs::orders::create_order_form(db(ctx), org.id, quote_id)
            .await
            .map_err(order_flow_error)?;
        Ok(form.into())
    }
}
