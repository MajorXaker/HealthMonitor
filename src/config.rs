//! Configuration structs and loading logic for healthmon.
//!
//! The application reads its configuration from `config.json` at startup.
//! An annotated example config is written to `config.example.json` on every launch.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

/// Top-level application configuration loaded from `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// HTTP Basic Auth credentials used to protect all REST endpoints.
    pub auth: AuthConfig,
    /// PostgreSQL connection URL, e.g. `postgres://user:pass@localhost:5432/db`.
    pub database_config: DatabaseConfig,
    /// TCP server binding configuration.
    pub server: ServerConfig,
    /// List of healthcheck definitions to run in background loops.
    pub healthchecks: Vec<HealthCheckConfig>,
    /// List of IMAP email account configurations (empty disables email monitoring).
    pub emails: Vec<EmailConfig>,
}

/// Credentials for the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Username for database
    pub user: String,
    /// Password for database
    pub password: String,
    /// Host of the db
    pub host: String,
    /// Port of the db
    pub port: u16,
    /// Name of the db
    pub database: String,

}

impl DatabaseConfig {
    pub fn get_connection_string(&self, anonymous: bool) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            if !anonymous {self.user.as_str()} else {"***"},
            if !anonymous {self.password.as_str()} else {"***"},
            self.host,
            self.port,
            self.database,
        )
    }
}
/// HTTP Basic Auth credentials stored in config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Username for Basic Auth.
    pub username: String,
    /// Password for Basic Auth.
    pub password: String,
}

/// TCP server bind settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// IP address to bind on, e.g. `"0.0.0.0"` or `"127.0.0.1"`.
    pub host: String,
    /// TCP port to listen on, e.g. `8080`.
    pub port: u16,
    /// Whether OpenApi documentation should be available
    pub enable_docs: bool
}

/// Configuration for a single healthcheck target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// Human-readable name shown in the API response.
    pub name: String,
    /// Type of check: `"http"` for HTTP GET checks, `"file"` for folder-presence checks.
    pub check_type: String,
    /// Target address: a URL for HTTP checks or a filesystem path for file checks.
    pub address: String,
    /// Interval between checks in seconds.
    pub period_seconds: u64,
    /// Number of consecutive failures before the check is marked unhealthy.
    pub failure_threshold: u32,
}

/// IMAP email monitoring configuration for a single account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// Human-readable account name used in logs and stored in the DB.
    pub name: String,
    /// IMAP server hostname.
    pub host: String,
    /// IMAP server port (typically 993 for TLS, 143 for plaintext).
    pub port: u16,
    /// IMAP login username.
    pub username: String,
    /// IMAP login password.
    pub password: String,
    /// Mailbox (folder) to poll, e.g. `"INBOX"`.
    pub mailbox: String,
    /// How often to poll for new messages, in seconds.
    pub poll_interval_seconds: u64,
    /// If `true`, connect with TLS (IMAPS); otherwise use plain TCP.
    pub use_tls: bool,
    #[serde(default = "default_recent_lookback")]
    pub recent_lookback: String,
}

fn default_recent_lookback() -> String {
    "1d".to_string()
}

/// Load the application configuration from `config.json` in the current working directory.
///
/// Returns an error if the file does not exist or if its contents cannot be parsed.
pub fn load_config() -> Result<AppConfig> {
    let data = fs::read_to_string("config.json")
        .context("Failed to read config.json — create it from config.example.json")?;
    let config: AppConfig =
        serde_json::from_str(&data).context("Failed to parse config.json")?;
    Ok(config)
}

/// Write an annotated example configuration to `config.example.json`.
///
/// This is called on every startup to keep the example in sync with the current
/// schema. It is safe to overwrite an existing file.
pub fn write_example_config() -> Result<()> {
    let example = AppConfig {
        auth: AuthConfig {
            username: "admin".to_string(),
            password: "changeme".to_string(),
        },
        database_config: DatabaseConfig {
            user: "postgres".to_string(),
            password: "pass".to_string(),
            host: "localhost".to_string(),
            port: 5432,
            database: "monitor".to_string(),
        },
        server: ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            enable_docs: true,
        },
        healthchecks: vec![
            HealthCheckConfig {
                name: "my-http-service".to_string(),
                check_type: "http".to_string(),
                address: "http://localhost:3000/health".to_string(),
                period_seconds: 30,
                failure_threshold: 3,
            },
            HealthCheckConfig {
                name: "my-file-trigger".to_string(),
                check_type: "file".to_string(),
                address: "/var/run/healthmon/trigger".to_string(),
                period_seconds: 60,
                failure_threshold: 2,
            },
        ],
        emails: vec![
            EmailConfig {
                name: "work-inbox".to_string(),
                host: "imap.example.com".to_string(),
                port: 993,
                username: "user@example.com".to_string(),
                password: "secret".to_string(),
                mailbox: "INBOX".to_string(),
                poll_interval_seconds: 60,
                use_tls: true,
                recent_lookback: "1d".to_string(),
            },
            EmailConfig {
                name: "alerts-inbox".to_string(),
                host: "imap.example.org".to_string(),
                port: 993,
                username: "alerts@example.org".to_string(),
                password: "secret2".to_string(),
                mailbox: "INBOX".to_string(),
                poll_interval_seconds: 120,
                use_tls: true,
                recent_lookback: "1d".to_string(),
            },
        ],
    };

    let json = serde_json::to_string_pretty(&example)
        .context("Failed to serialize example config")?;
    fs::write("config.example.json", json)
        .context("Failed to write config.example.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize a full AppConfig to JSON, then parse it back and assert fields match.
    #[test]
    fn test_parse_valid_config() {
        let original = AppConfig {
            auth: AuthConfig {
                username: "testuser".to_string(),
                password: "testpass".to_string(),
            },
            database_config: DatabaseConfig {
                user: "u".to_string(),
                password: "p".to_string(),
                host: "localhost".to_string(),
                port: 5432,
                database: "db".to_string(),
            },
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 9090,
                enable_docs: true,
            },
            healthchecks: vec![HealthCheckConfig {
                name: "svc".to_string(),
                check_type: "http".to_string(),
                address: "http://example.com".to_string(),
                period_seconds: 10,
                failure_threshold: 2,
            }],
            emails: vec![],
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: AppConfig = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.auth.username, "testuser");
        assert_eq!(parsed.auth.password, "testpass");
        assert_eq!(parsed.database_config.user, "u");
        assert_eq!(parsed.database_config.password, "p");
        assert_eq!(parsed.database_config.host, "localhost");
        assert_eq!(parsed.database_config.database, "db");

        let correct_string = parsed.database_config.get_connection_string(false);
        assert_eq!(correct_string, "postgres://u:p@localhost:5432/db");
        let str_for_logging = parsed.database_config.get_connection_string(true);
        assert_eq!(str_for_logging, "postgres://***:***@localhost:5432/db");

        assert_eq!(parsed.server.port, 9090);
        assert_eq!(parsed.healthchecks.len(), 1);
        assert_eq!(parsed.healthchecks[0].check_type, "http");
        assert!(parsed.emails.is_empty());
    }

    /// Parsing JSON without the required `auth` field must return an error.
    #[test]
    fn test_parse_missing_required_field() {
        let json = r#"{"database_url":"postgres://u:p@localhost/db","server":{"host":"127.0.0.1","port":8080},"healthchecks":[],"emails":[]}"#;
        let result: Result<AppConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "should fail when auth is missing");
    }
}
