//! healthmon — Healthcheck & Email Monitor Service
//!
//! Entry point that wires together configuration, database, background tasks,
//! and the axum HTTP server.

mod api;
mod auth;
mod config;
mod email;
mod healthcheck;

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use api::{AppState, create_router};
use auth::Credentials;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialise structured logging.
    //    Log level can be overridden with the RUST_LOG environment variable.
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("healthmon starting up");

    // 2. Load application configuration from config.json.
    let cfg = config::load_config().unwrap_or_else(|e| {
        error!(error = %e, "Failed to load config.json — aborting");
        std::process::exit(1);
    });

    // 3. Write the annotated example config so operators always have an
    //    up-to-date reference alongside the running binary.
    if let Err(e) = config::write_example_config() {
        // Non-fatal: log and continue.
        tracing::warn!(error = %e, "Failed to write config.example.json");
    }

    // 4. Connect to PostgreSQL.
    info!(url = %cfg.database_url, "Connecting to PostgreSQL");
    let pool = sqlx::PgPool::connect(&cfg.database_url).await.unwrap_or_else(|e| {
        error!(error = %e, "Failed to connect to PostgreSQL — aborting");
        std::process::exit(1);
    });

    // 5. Run database migrations (idempotent).
    email::db::run_migrations(&pool).await.unwrap_or_else(|e| {
        error!(error = %e, "Failed to run DB migrations — aborting");
        std::process::exit(1);
    });

    // 6. Create shared healthcheck state.
    let healthcheck_state: Arc<RwLock<Vec<healthcheck::HealthStatus>>> =
        Arc::new(RwLock::new(Vec::new()));

    // 7. Spawn the healthcheck background task.
    {
        let state_clone = Arc::clone(&healthcheck_state);
        let checks = cfg.healthchecks.clone();
        tokio::spawn(async move {
            healthcheck::runner::run_healthcheck_loop(checks, state_clone).await;
        });
    }

    // 8. Spawn the email monitoring background task (only if configured).
    if !cfg.emails.is_empty() {
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            email::runner::run_email_loop(cfg.emails.clone(), pool_clone).await;
        });
    } else {
        info!("No email accounts configured — email monitoring disabled");
    }

    // 9. Build the axum router.
    let app_state = AppState {
        credentials: Credentials {
            username: cfg.auth.username.clone(),
            password: cfg.auth.password.clone(),
        },
        healthcheck_state,
        db_pool: pool,
    };
    let router = create_router(app_state, cfg.server.enable_docs);

    // 10. Start the HTTP server.
    let bind_addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    info!(address = %bind_addr, "HTTP server listening");

    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap_or_else(|e| {
        error!(error = %e, address = %bind_addr, "Failed to bind TCP listener — aborting");
        std::process::exit(1);
    });

    axum::serve(listener, router).await.unwrap_or_else(|e| {
        error!(error = %e, "HTTP server error");
        std::process::exit(1);
    });

    Ok(())
}
