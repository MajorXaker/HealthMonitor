//! Background healthcheck runner.
//!
//! Spawns one tokio task per configured check. Each task runs its own
//! sleep-check loop, updating the shared [`HealthStatus`] state after every
//! iteration.

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use crate::config::HealthCheckConfig;
use crate::healthcheck::{
    file_check::check_file, http_check::check_http, HealthStatus,
};

/// Start background healthcheck loops for all configured checks.
///
/// Initialises the shared `state` with one [`HealthStatus`] entry per config,
/// then spawns a dedicated tokio task for each check that loops indefinitely.
///
/// Each task:
/// 1. Runs the appropriate check (HTTP or file).
/// 2. Updates the shared state (increments failure counter or resets it).
/// 3. Sleeps for `period_seconds`.
pub async fn run_healthcheck_loop(
    configs: Vec<HealthCheckConfig>,
    state: Arc<RwLock<Vec<HealthStatus>>>,
) {
    // Initialise all statuses as unknown/unhealthy before any check has run.
    {
        let mut statuses = state.write().await;
        for cfg in &configs {
            statuses.push(HealthStatus {
                name: cfg.name.clone(),
                check_type: cfg.check_type.clone(),
                healthy: false,
                last_checked: None,
                consecutive_failures: 0,
            });
        }
    }

    for cfg in configs {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            run_single_check_loop(cfg, state_clone).await;
        });
    }
}

/// Inner loop for a single check.  Runs until the process exits.
async fn run_single_check_loop(cfg: HealthCheckConfig, state: Arc<RwLock<Vec<HealthStatus>>>) {
    loop {
        let now = chrono::Utc::now();

        // Execute the appropriate check type.
        let is_healthy = match cfg.check_type.as_str() {
            "http" => check_http(&cfg.address).await,
            "file" => check_file(&cfg.address).await,
            other => {
                warn!(check_type = other, name = %cfg.name, "Unknown check type — treating as unhealthy");
                false
            }
        };

        // Update the shared state.
        update_status(&state, &cfg, is_healthy, now).await;

        sleep(Duration::from_secs(cfg.period_seconds)).await;
    }
}

/// Apply a check result to the matching [`HealthStatus`] entry in `state`.
///
/// This function is extracted so that it can be unit-tested without spawning
/// tasks.
pub async fn update_status(
    state: &Arc<RwLock<Vec<HealthStatus>>>,
    cfg: &HealthCheckConfig,
    is_healthy: bool,
    now: chrono::DateTime<chrono::Utc>,
) {
    let mut statuses = state.write().await;
    if let Some(status) = statuses.iter_mut().find(|s| s.name == cfg.name) {
        status.last_checked = Some(now);

        if is_healthy {
            // Reset failure counter on success.
            status.consecutive_failures = 0;
            status.healthy = true;
            info!(name = %cfg.name, "Healthcheck passed");
        } else {
            status.consecutive_failures += 1;
            // Only flip to unhealthy once we have reached the threshold.
            if status.consecutive_failures >= cfg.failure_threshold {
                status.healthy = false;
                warn!(
                    name = %cfg.name,
                    consecutive_failures = status.consecutive_failures,
                    "Healthcheck marked UNHEALTHY (threshold reached)"
                );
            } else {
                warn!(
                    name = %cfg.name,
                    consecutive_failures = status.consecutive_failures,
                    threshold = cfg.failure_threshold,
                    "Healthcheck failure (below threshold, still considered healthy)"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::healthcheck::HealthStatus;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Build a minimal config and a pre-populated state for testing.
    fn make_state(name: &str) -> (HealthCheckConfig, Arc<RwLock<Vec<HealthStatus>>>) {
        let cfg = HealthCheckConfig {
            name: name.to_string(),
            check_type: "http".to_string(),
            address: "http://example.com".to_string(),
            period_seconds: 30,
            failure_threshold: 3,
        };
        let status = HealthStatus {
            name: name.to_string(),
            check_type: "http".to_string(),
            healthy: true,
            last_checked: None,
            consecutive_failures: 0,
        };
        let state = Arc::new(RwLock::new(vec![status]));
        (cfg, state)
    }

    /// Failure counter should increment and `healthy` should flip to `false` once
    /// `consecutive_failures` reaches `failure_threshold`.
    #[tokio::test]
    async fn test_failure_threshold_logic() {
        let (cfg, state) = make_state("svc");
        let now = chrono::Utc::now();

        // Two failures — below threshold of 3, still healthy.
        update_status(&state, &cfg, false, now).await;
        update_status(&state, &cfg, false, now).await;
        {
            let s = state.read().await;
            assert_eq!(s[0].consecutive_failures, 2);
            assert!(s[0].healthy, "should still be healthy below threshold");
        }

        // Third failure — at threshold, should flip to unhealthy.
        update_status(&state, &cfg, false, now).await;
        {
            let s = state.read().await;
            assert_eq!(s[0].consecutive_failures, 3);
            assert!(!s[0].healthy, "should be unhealthy at threshold");
        }
    }

    /// After a series of failures, a healthy result should reset the counter
    /// and mark the check healthy again.
    #[tokio::test]
    async fn test_recovery_resets_failures() {
        let (cfg, state) = make_state("svc");
        let now = chrono::Utc::now();

        // Push past the threshold.
        for _ in 0..3 {
            update_status(&state, &cfg, false, now).await;
        }
        {
            let s = state.read().await;
            assert!(!s[0].healthy);
        }

        // Recover with a single healthy check.
        update_status(&state, &cfg, true, now).await;
        {
            let s = state.read().await;
            assert_eq!(s[0].consecutive_failures, 0, "failures should reset to 0");
            assert!(s[0].healthy, "should be healthy after recovery");
        }
    }
}
