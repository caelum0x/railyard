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
    let mut principal_org: Option<uuid::Uuid> = None;

    // Resolve the bearer token → Principal (real DB check; dashboards would send
    // a session JWT — token issuance is a follow-up, the guard is real).
    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_whitespace().nth(1))
    {
        if let Ok(Some((key, org))) = state.db.authenticate(token).await {
            if !key.is_expired(Utc::now()) {
                principal_org = Some(org.id);
                req = req.data(Principal { organization: org });
            }
        } else if let Some((user_id, pinned_org)) = rago_graphql::auth::verify_jwt_claims(token) {
            // Dashboard session JWT: resolve the user + their organization (an
            // SSO session is pinned to the org whose IdP authenticated it) and
            // inject both an org principal and the user session (for currentUser).
            let session = match pinned_org {
                Some(org) => state.db.resolve_member_session(user_id, org).await,
                None => state.db.resolve_user_session(user_id).await,
            };
            if let Ok(Some((org, email))) = session {
                principal_org = Some(org.id);
                req = req
                    .data(Principal { organization: org })
                    .data(UserSession { id: user_id, email });
            }
        }
    }

    // Tenant isolation: most resolvers address records by UUID, so reject the
    // whole operation when it references any record owned by another
    // organization (query text literals + variables).
    if let Some(org) = principal_org {
        if let Err(resp) = guard_tenant(&state, org, &req).await {
            return resp.into();
        }
    }

    state.schema.execute(req).await.into()
}

async fn guard_tenant(
    state: &AppState,
    org: uuid::Uuid,
    req: &async_graphql::Request,
) -> Result<(), async_graphql::Response> {
    use rago_db::tenancy::{uuids_in_text, MAX_CHECKED_IDS};
    let mut ids = uuids_in_text(&req.query);
    let vars = serde_json::to_string(&req.variables).unwrap_or_default();
    for id in uuids_in_text(&vars) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    for chunk in ids.chunks(MAX_CHECKED_IDS) {
        match state.db.foreign_ids(org, chunk).await {
            Ok(foreign) if foreign.is_empty() => {}
            Ok(foreign) => {
                tracing::warn!(organization_id = %org, foreign = ?foreign, "graphql cross-tenant reference rejected");
                return Err(async_graphql::Response::from_errors(vec![
                    async_graphql::ServerError::new("not_found", None),
                ]));
            }
            Err(e) => {
                tracing::error!(error = %e, "graphql tenant guard failed");
                return Err(async_graphql::Response::from_errors(vec![
                    async_graphql::ServerError::new("internal_error", None),
                ]));
            }
        }
    }
    Ok(())
}

/// `GET /graphql` — the GraphQL Playground IDE.
pub async fn graphql_playground() -> impl IntoResponse {
    Html(playground_source(GraphQLPlaygroundConfig::new("/graphql")))
}

/// Build the schema for the app state.
pub fn build_schema(db: rago_db::Db) -> RagoSchema {
    rago_graphql::build_schema(db)
}
