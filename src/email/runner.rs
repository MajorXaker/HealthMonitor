//! Background email polling loop.
//!
//! Periodically connects to the IMAP server, fetches new message metadata, and
//! persists any previously-unseen messages to PostgreSQL.

use sqlx::PgPool;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use chrono::{NaiveDate, Utc};

use crate::config::EmailConfig;
use crate::email::{db, imap_client};


pub fn parse_since_date(lookback: &str) -> Option<NaiveDate> {
    let lookback = lookback.trim();
    if lookback.ends_with('d') {
        let days: i64 = lookback.trim_end_matches('d').parse().ok()?;
        Some((Utc::now() - chrono::Duration::days(days)).date_naive())
    } else if lookback.ends_with('h') {
        let hours: i64 = lookback.trim_end_matches('h').parse().ok()?;
        Some((Utc::now() - chrono::Duration::hours(hours)).date_naive())
    } else {
        None
    }
}

/// Start the email monitoring background loop for all configured accounts.
///
/// Spawns one tokio task per account. Each task polls its configured IMAP
/// mailbox every `config.poll_interval_seconds` seconds. New messages (those
/// not already in the database) are persisted. The loops run until the process
/// exits; errors are logged but do not abort the loops.
pub async fn run_email_loop(configs: Vec<EmailConfig>, pool: PgPool) {
    for config in configs {
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            run_single_account_loop(config, pool_clone).await;
        });
    }
}

/// Internal loop for a single IMAP account.
async fn run_single_account_loop(config: EmailConfig, pool: PgPool) {
    info!(
        account = %config.name,
        host = %config.host,
        mailbox = %config.mailbox,
        interval_seconds = config.poll_interval_seconds,
        "Email polling loop started"
    );

    let mut first_scan = true;

    loop {
        let since = if first_scan {
            None
        } else {
            match parse_since_date(&config.recent_lookback) {
                Some(date) => Some(date),
                None => {
                    warn!(account = %config.name, lookback = %config.recent_lookback,
                      "Could not parse recent_lookback — falling back to SEARCH ALL");
                    None
                }
            }
        };

        match imap_client::fetch_emails(&config, since).await {
            Ok(emails) => {
                first_scan = false;
                let total = emails.len();
                let mut new_count = 0u32;

                for email in &emails {
                    match db::insert_email_if_new(&pool, email).await {
                        Ok(true) => {
                            new_count += 1;
                            info!(
                                account = %config.name,
                                message_id = %email.message_id,
                                subject = %email.subject,
                                "New email stored"
                            );
                        }
                        Ok(false) => {
                            // Already in DB — skip silently.
                        }
                        Err(e) => {
                            warn!(
                                account = %config.name,
                                message_id = %email.message_id,
                                error = %e,
                                "Failed to insert email into DB"
                            );
                        }
                    }
                }

                info!(
                    account = %config.name,
                    total_fetched = total,
                    new_stored = new_count,
                    "Email poll cycle complete"
                );
            }
            Err(e) => {
                // Keep first_scan = true so the retry is also a full scan.
                warn!(account = %config.name, error = %e, "Email fetch failed — will retry next interval");
            }
        }

        sleep(Duration::from_secs(config.poll_interval_seconds)).await;
    }
}
