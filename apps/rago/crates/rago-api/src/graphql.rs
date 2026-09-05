//! GraphQL endpoint wiring: resolve the request's bearer token to a real
//! [`Principal`] (via the DB) and inject it into the async-graphql request
//! context, then execute. Unauthenticated requests still reach the schema but
//! every gated resolver returns an `unauthorized` error (no bypass).

use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use chrono::Utc;
use rago_graphql::{Principal, RagoSchema, UserSession};

use crate::state::AppState;

/// `POST /graphql` — execute a GraphQL request with an auth-resolved context.
pub async fn graphql_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let mut req = req.into_inner();

    // Resolve the bearer token → Principal (real DB check; dashboards would send
    // a session JWT — token issuance is a follow-up, the guard is real).
    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_whitespace().nth(1))
    {
        if let Ok(Some((key, org))) = state.db.authenticate(token).await {
            if !key.is_expired(Utc::now()) {
                req = req.data(Principal { organization: org });
            }
        } else if let Some(user_id) = rago_graphql::auth::verify_jwt(token) {
            // Dashboard session JWT: resolve the user + their organization and
            // inject both an org principal and the user session (for currentUser).
            if let Ok(Some((org, email))) = state.db.resolve_user_session(user_id).await {
                req = req
                    .data(Principal { organization: org })
                    .data(UserSession { id: user_id, email });
            }
        }
    }

    state.schema.execute(req).await.into()
}

/// `GET /graphql` — the GraphQL Playground IDE.
pub async fn graphql_playground() -> impl IntoResponse {
    Html(playground_source(GraphQLPlaygroundConfig::new("/graphql")))
}

/// Build the schema for the app state.
pub fn build_schema(db: rago_db::Db) -> RagoSchema {
    rago_graphql::build_schema(db)
}
