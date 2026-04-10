//! Axum route handlers for the healthcheck REST endpoints.

use axum::{
    extract::State,
    response::IntoResponse,
    Json,
};

use crate::auth::BasicAuth;
use super::AppState;

/// `GET /healthchecks` — return the current status of all configured checks.
///
/// Requires HTTP Basic Auth. Returns a JSON array of [`HealthStatus`] objects,
/// one per configured check.
#[utoipa::path(
    get,
    path = "/healthchecks",
    responses(
        (status = 200, description = "List of healthcheck statuses", body = Vec<crate::healthcheck::HealthStatus>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("basicAuth" = []))
)]
pub async fn get_healthchecks(
    _auth: BasicAuth,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let statuses = state.healthcheck_state.read().await;
    Json(statuses.clone())
}
