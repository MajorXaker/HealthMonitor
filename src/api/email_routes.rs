//! Axum route handlers for the email REST endpoints.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::auth::BasicAuth;
use crate::email::db;
use super::AppState;

/// Request body for `POST /emails/acknowledge`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AcknowledgeRequest {
    /// Database IDs of the emails to mark as acknowledged (`is_new = false`).
    pub ids: Vec<i32>,
}

/// Response body for `POST /emails/acknowledge` and `POST /emails/acknowledge-all`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AcknowledgeResponse {
    /// Number of rows that were updated.
    pub acknowledged: u64,
}

/// `GET /emails` — return all emails that have not yet been acknowledged.
///
/// Requires HTTP Basic Auth. Returns a JSON array of email records where
/// `is_new = true`.
#[utoipa::path(
    get,
    path = "/emails",
    responses(
        (status = 200, description = "List of unacknowledged emails", body = Vec<crate::email::EmailRecord>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("basicAuth" = []))
)]
pub async fn get_emails(
    _auth: BasicAuth,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match db::get_new_emails(&state.db_pool).await {
        Ok(emails) => Json(emails).into_response(),
        Err(e) => {
            warn!(error = %e, "Failed to fetch new emails from DB");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch emails",
            )
                .into_response()
        }
    }
}

/// `POST /emails/acknowledge` — mark a set of emails as read.
///
/// Requires HTTP Basic Auth. The request body must be JSON with an `"ids"`
/// array of integer email IDs. Returns `{ "acknowledged": N }`.
#[utoipa::path(
    post,
    path = "/emails/acknowledge",
    request_body = AcknowledgeRequest,
    responses(
        (status = 200, description = "Emails acknowledged", body = AcknowledgeResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("basicAuth" = []))
)]
pub async fn acknowledge_emails(
    _auth: BasicAuth,
    State(state): State<AppState>,
    Json(body): Json<AcknowledgeRequest>,
) -> impl IntoResponse {
    match db::acknowledge_emails(&state.db_pool, &body.ids).await {
        Ok(count) => Json(AcknowledgeResponse { acknowledged: count }).into_response(),
        Err(e) => {
            warn!(error = %e, "Failed to acknowledge emails in DB");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to acknowledge emails",
            )
                .into_response()
        }
    }
}

/// `POST /emails/acknowledge-all` — mark every new email as read.
#[utoipa::path(
    post,
    path = "/emails/acknowledge-all",
    responses(
        (status = 200, description = "All emails acknowledged", body = AcknowledgeResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("basicAuth" = []))
)]
pub async fn acknowledge_all_emails(
    _auth: BasicAuth,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match db::acknowledge_all_emails(&state.db_pool).await {
        Ok(count) => Json(AcknowledgeResponse { acknowledged: count }).into_response(),
        Err(e) => {
            warn!(error = %e, "Failed to acknowledge all emails");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to acknowledge all emails").into_response()
        }
    }
}
