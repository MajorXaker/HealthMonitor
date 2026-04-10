//! Healthcheck module: shared types and sub-modules for HTTP and file checks.

pub mod file_check;
pub mod http_check;
pub mod runner;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// The live status of a single healthcheck target.
///
/// One instance exists per configured check and is updated in-place by the
/// background runner. The API endpoint serialises all instances to JSON.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct HealthStatus {
    /// Name of the healthcheck, matching [`crate::config::HealthCheckConfig::name`].
    pub name: String,
    /// Check type: `"http"` or `"file"`.
    pub check_type: String,
    /// `true` if the check is currently considered healthy.
    pub healthy: bool,
    /// Timestamp of the most recent check attempt, or `None` before the first run.
    pub last_checked: Option<DateTime<Utc>>,
    /// Number of consecutive failures since the last successful check.
    pub consecutive_failures: u32,
}
