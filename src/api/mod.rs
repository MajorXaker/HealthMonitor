//! API layer: axum router setup and shared application state.

pub mod email_routes;
pub mod healthcheck_routes;

use std::sync::Arc;
use tokio::sync::RwLock;

use axum::{
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sqlx::PgPool;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::Credentials;
use crate::healthcheck::HealthStatus;

/// Shared application state threaded through all axum handlers.
#[derive(Clone)]
pub struct AppState {
    /// Credentials used to validate HTTP Basic Auth on every request.
    pub credentials: Credentials,
    /// Live healthcheck statuses, updated by the background runner.
    pub healthcheck_state: Arc<RwLock<Vec<HealthStatus>>>,
    /// PostgreSQL connection pool for email persistence.
    pub db_pool: PgPool,
    /// app config
    pub email_active: bool,
}

/// Allow the axum [`BasicAuth`] extractor to retrieve [`Credentials`] from [`AppState`].
impl axum::extract::FromRef<AppState> for Credentials {
    fn from_ref(state: &AppState) -> Self {
        state.credentials.clone()
    }
}

/// `GET /__heartbeat__` — public liveness probe.
///
/// Returns 200 OK with `{"status": "ok"}`. No authentication required.
/// Intended for use by load balancers and container orchestrators.
#[utoipa::path(
    get,
    path = "/__heartbeat__",
    responses((status = 200, description = "Service is alive"))
)]
async fn heartbeat() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        healthcheck_routes::get_healthchecks,
        email_routes::get_emails,
        email_routes::acknowledge_emails,
        email_routes::acknowledge_all_emails,
        heartbeat,
    ),
    components(schemas(
        crate::healthcheck::HealthStatus,
        crate::email::EmailRecord,
        crate::api::email_routes::AcknowledgeRequest,
        crate::api::email_routes::AcknowledgeResponse,
    )),
    info(title = "healthmon", version = "0.1.0", description = "Healthcheck & Email Monitor API"),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

struct SecurityAddon;
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "basicAuth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Basic)
                        .build(),
                ),
            );
        }
    }
}

/// Build and return the axum [`Router`] with all routes and shared state.
///
/// Routes:
/// - `GET  /__heartbeat__`             → public liveness probe
/// - `GET  /healthchecks`              → current status of all configured checks
/// - `GET  /emails`                    → emails with `is_new = true`
/// - `POST /emails/acknowledge`        → mark emails as read
/// - `POST /emails/acknowledge-all`    → mark all emails as read
/// - `GET  /docs`                      → Swagger UI
pub fn create_router(app_state: AppState, enable_docs: bool) -> Router {
    let router = Router::new().route("/__heartbeat__", get(heartbeat))
        .route(
            "/healthchecks",
            get(healthcheck_routes::get_healthchecks),
        )
        .route("/emails", get(email_routes::get_emails))
        .route(
            "/emails/acknowledge",
            post(email_routes::acknowledge_emails),
        )
        .route(
            "/emails/acknowledge-all",
            post(email_routes::acknowledge_all_emails),
        );

    let router = if enable_docs {
        router.merge(SwaggerUi::new("/docs").url("/docs/openapi.json", ApiDoc::openapi()))
    } else {
        router
    };

    router.with_state(app_state)
}
