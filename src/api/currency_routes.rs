//! Axum route handlers for the currency rate endpoints (added in v1.2).

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use super::AppState;
use crate::auth::BasicAuth;
use crate::currency::{self, format::render_text, Rate};

/// Response body for `GET /currency/json`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CurrencyResponse {
    /// All configured rates in display order.
    pub rates: Vec<Rate>,
}

fn disabled() -> Response {
    (StatusCode::NOT_FOUND, "Currency rates are disabled").into_response()
}

/// `GET /currency` — rates as plain text, one per line, 21 characters wide.
///
/// Designed for the device display: label, a `*` marker when the value is
/// stale, and the value right-aligned. Missing values show as `n/a`.
#[utoipa::path(
    get,
    path = "/currency",
    responses(
        (status = 200, description = "Rates as plain text", body = String, content_type = "text/plain"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Currency rates are disabled in config"),
    ),
    security(("basicAuth" = []))
)]
pub async fn get_currency_text(_auth: BasicAuth, State(state): State<AppState>) -> Response {
    if !state.currency_active {
        return disabled();
    }
    let slots = state.currency_state.read().await;
    let rates = currency::snapshot(&slots, chrono::Utc::now());
    let body = render_text(&slots, &rates);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// `GET /currency/json` — rates with value, source and timestamps.
#[utoipa::path(
    get,
    path = "/currency/json",
    responses(
        (status = 200, description = "Rates with metadata", body = CurrencyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Currency rates are disabled in config"),
    ),
    security(("basicAuth" = []))
)]
pub async fn get_currency_json(_auth: BasicAuth, State(state): State<AppState>) -> Response {
    if !state.currency_active {
        return disabled();
    }
    let slots = state.currency_state.read().await;
    let rates = currency::snapshot(&slots, chrono::Utc::now());
    Json(CurrencyResponse { rates }).into_response()
}
