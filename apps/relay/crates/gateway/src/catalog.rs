use std::sync::Arc;

use axum::{
    Extension, Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_catalog::{CatalogError, CatalogService, PriceQuoteRequest};
use relay_contract::{ApiError, ApiErrorEnvelope, DataEnvelope};

pub async fn catalog_handler(
    Extension(catalog): Extension<Arc<CatalogService>>,
) -> impl IntoResponse {
    Json(DataEnvelope {
        data: catalog.snapshot().await,
    })
}

pub async fn quote_handler(
    Extension(catalog): Extension<Arc<CatalogService>>,
    Json(request): Json<PriceQuoteRequest>,
) -> Response {
    match catalog
        .quote(&request.model, request.input_tokens, request.output_tokens)
        .await
    {
        Ok(quote) => Json(DataEnvelope { data: quote }).into_response(),
        Err(CatalogError::NotFound(_)) => catalog_error(
            StatusCode::NOT_FOUND,
            "relay_model_not_found",
            "requested Relay model was not found",
        ),
        Err(CatalogError::Overflow) => catalog_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "relay_quote_overflow",
            "requested token counts exceed Relay quote limits",
        ),
        Err(error) => {
            tracing::error!(error = %error, "Relay catalog quote failed");
            catalog_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_catalog_unavailable",
                "Relay catalog is unavailable",
            )
        }
    }
}

fn catalog_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ApiErrorEnvelope {
            error: ApiError {
                message: message.into(),
                kind: Some("relay_catalog_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;

    use super::*;

    async fn service() -> Arc<CatalogService> {
        Arc::new(CatalogService::builtin(123).await.unwrap())
    }

    #[tokio::test]
    async fn catalog_and_quote_handlers_return_typed_envelopes() {
        let service = service().await;
        let catalog = catalog_handler(Extension(service.clone()))
            .await
            .into_response();
        assert_eq!(catalog.status(), StatusCode::OK);
        let body = catalog.into_body().collect().await.unwrap().to_bytes();
        let envelope: DataEnvelope<relay_catalog::CatalogSnapshot> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope.data.revision, "relay-builtin-v1");
        assert!(!envelope.data.models.is_empty());

        let quote = quote_handler(
            Extension(service),
            Json(PriceQuoteRequest {
                model: "openai/gpt-4o".into(),
                input_tokens: 1_000,
                output_tokens: 500,
            }),
        )
        .await;
        assert_eq!(quote.status(), StatusCode::OK);
        let body = quote.into_body().collect().await.unwrap().to_bytes();
        let envelope: DataEnvelope<relay_catalog::PriceQuote> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope.data.amount_micros.get(), 7_500);
    }

    #[tokio::test]
    async fn unknown_model_is_a_stable_not_found_error() {
        let response = quote_handler(
            Extension(service().await),
            Json(PriceQuoteRequest {
                model: "missing/model".into(),
                input_tokens: 1,
                output_tokens: 1,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let envelope: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            envelope.error.code.as_deref(),
            Some("relay_model_not_found")
        );
    }
}
