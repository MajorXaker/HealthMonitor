//! HTTP-based healthcheck implementation.
//!
//! Issues an HTTP GET request to the target URL and returns `true` if a 2xx
//! status code is received within the configured timeout.

use std::time::Duration;
use tracing::{info, warn};

/// Perform an HTTP GET healthcheck against `address`.
///
/// Returns `true` when the server responds with a 2xx status code within
/// 10 seconds. Returns `false` on any network error, timeout, or non-2xx
/// response.
pub async fn check_http(address: &str) -> bool {
    let client = match reqwest::ClientBuilder::new()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(address, error = %e, "Failed to build HTTP client");
            return false;
        }
    };

    match client.get(address).send().await {
        Ok(resp) => {
            let status = resp.status();
            let ok = status.is_success();
            if ok {
                info!(address, %status, "HTTP check passed");
            } else {
                warn!(address, %status, "HTTP check failed — non-2xx response");
            }
            ok
        }
        Err(e) => {
            warn!(address, error = %e, "HTTP check failed — request error");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    /// Unit tests for http_check are integration-level (require a live server).
    /// Logic is covered by the runner tests; a live-server integration test
    /// can be added separately with `#[ignore]`.
    #[test]
    fn http_check_module_compiles() {
        // Confirm the module compiles and exports check_http as an async fn.
        // We just need the symbol to be accessible; calling it requires a live server.
        fn _type_check() {
            let _ = super::check_http;
        }
    }
}
