//! Stream (metered channel) resource handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::StreamId;
use mint_store::LedgerStore;
use mint_stream::{CloseStreamRequest, OpenStreamRequest, TickStreamRequest};

use crate::dto::{parse_account, StreamCloseReq, StreamOpenReq, StreamTickReq, StreamView};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

/// `POST /v1/streams` → `StreamLedger::open_stream`.
pub async fn open<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<StreamOpenReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .streams
        .open_stream(OpenStreamRequest {
            from,
            to,
            rate_micros: body.rate_micros,
            cap_micros: body.cap_micros,
            unit: body.unit,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok_status(
        StatusCode::CREATED,
        views::stream_open_receipt(&receipt),
    ))
}

/// `POST /v1/streams/:id/tick` → `StreamLedger::tick_stream`.
pub async fn tick<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<StreamTickReq>,
) -> ApiResult {
    let receipt = st
        .streams
        .tick_stream(TickStreamRequest {
            stream_id: id,
            units: body.units,
            seq: body.seq,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok(views::stream_tick_receipt(&receipt)))
}

/// `POST /v1/streams/:id/close` → `StreamLedger::close_stream`.
pub async fn close<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<StreamCloseReq>,
) -> ApiResult {
    let receipt = st
        .streams
        .close_stream(CloseStreamRequest {
            stream_id: id,
            units: body.units,
            seq: body.seq,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok(views::stream_close_receipt(&receipt)))
}

/// `GET /v1/streams/:id` → `StreamLedger::read_stream`.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let stream_id = StreamId::parse(&id).map_err(ApiError::from)?;
    let record = st.streams.read_stream(&stream_id).await?;
    Ok(ok(StreamView::from(record)))
}
