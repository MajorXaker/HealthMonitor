//! IMAP client for fetching email metadata.
//!
//! Connects to an IMAP server (with or without TLS), selects the configured
//! mailbox, and downloads header metadata (subject, sender, date) for all
//! messages. Messages are **not** deleted from the server.
//!
//! `async-imap` uses `futures::AsyncRead + futures::AsyncWrite` streams.
//! For the non-TLS path we adapt a `tokio::net::TcpStream` via
//! `tokio_util::compat::TokioAsyncReadCompatExt`.

use anyhow::{Context, Result};
use async_imap::Session;
use chrono::{DateTime, NaiveDate, Utc};
use futures::TryStreamExt;
use mail_parser::MessageParser;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};
use tracing::{info, warn};

use crate::config::EmailConfig;

/// Minimal email metadata fetched from the IMAP server.
///
/// This is a transient type used between the IMAP client and the database
/// layer. Once inserted into the DB, it becomes an [`crate::email::EmailRecord`].
#[derive(Debug, Clone)]
pub struct RawEmail {
    /// Value of the `Message-ID` header, used for deduplication.
    pub message_id: String,
    /// Email subject.
    pub subject: String,
    /// Sender address from the `From` header.
    pub sender: String,
    /// Account name from the EmailConfig that fetched this message.
    pub account: String,
    /// Date/time the message was received, parsed from the `Date` header.
    pub received_at: DateTime<Utc>,
}

/// Connect to the IMAP server and fetch metadata for all messages in the
/// configured mailbox.
///
/// On any connection or protocol error the function logs a warning and returns
/// an empty `Vec` rather than propagating the error, so the background polling
/// loop keeps running.
pub async fn fetch_emails(config: &EmailConfig, since: Option<NaiveDate>) -> Result<Vec<RawEmail>> {
    if config.use_tls {
        fetch_emails_tls(config, since).await
    } else {
        fetch_emails_plain(config, since).await
    }
}

/// Internal helper: tag each RawEmail with the account name from config.
fn tag_with_account(mut emails: Vec<RawEmail>, account: &str) -> Vec<RawEmail> {
    for e in &mut emails {
        e.account = account.to_string();
    }
    emails
}

/// Fetch emails over a TLS-encrypted IMAP connection (IMAPS).
///
/// `async-native-tls` returns a `TlsStream` that already implements
/// `futures::AsyncRead + AsyncWrite`, so it can be passed directly to
/// `async_imap::Client::new`.
async fn fetch_emails_tls(config: &EmailConfig, since: Option<NaiveDate>) -> Result<Vec<RawEmail>> {
    let addr = format!("{}:{}", config.host, config.port);
    let tcp = tokio::net::TcpStream::connect(&addr)
        .await
        .context("TCP connect for IMAPS failed")?;

    // Wrap the tokio stream with a compat shim so async-native-tls can use it.
    let compat_tcp = tcp.compat();
    let tls_connector = async_native_tls::TlsConnector::new();
    let tls_stream = tls_connector
        .connect(&config.host, compat_tcp)
        .await
        .context("TLS handshake failed")?;

    // TlsStream implements futures AsyncRead/AsyncWrite directly.
    let client = async_imap::Client::new(tls_stream);
    let mut session = client
        .login(&config.username, &config.password)
        .await
        .map_err(|(e, _)| anyhow::anyhow!("IMAP login failed: {}", e))?;

    let emails = fetch_from_session(&mut session, config, since).await;
    let _ = session.logout().await;
    emails.map(|v| tag_with_account(v, &config.name))
}

/// Type alias for a tokio stream wrapped with the futures-compat shim.
type CompatTcpStream = Compat<tokio::net::TcpStream>;

/// Fetch emails over a plain (unencrypted) IMAP connection.
///
/// `tokio_util::compat` adapts the `tokio::io::AsyncRead/Write` traits to the
/// `futures::AsyncRead/Write` traits that `async-imap` requires.
async fn fetch_emails_plain(config: &EmailConfig, since: Option<NaiveDate>) -> Result<Vec<RawEmail>> {
    let addr = format!("{}:{}", config.host, config.port);
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .context("IMAP TCP connect failed")?;

    // Adapt tokio stream to futures-compatible stream expected by async-imap.
    let compat_stream: CompatTcpStream = stream.compat();

    let client = async_imap::Client::new(compat_stream);
    let mut session = client
        .login(&config.username, &config.password)
        .await
        .map_err(|(e, _)| anyhow::anyhow!("IMAP login failed: {}", e))?;

    let emails = fetch_from_session(&mut session, config, since).await;
    let _ = session.logout().await;
    emails.map(|v| tag_with_account(v, &config.name))
}

/// Execute IMAP commands against an already-authenticated session.
///
/// Selects the mailbox, searches for all message sequence numbers, fetches
/// the raw header block for each, and parses out subject / sender / date.
async fn fetch_from_session<T>(
    session: &mut Session<T>,
    config: &EmailConfig,
    since: Option<NaiveDate>,
) -> Result<Vec<RawEmail>>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + std::fmt::Debug,
{
    // Select the mailbox (e.g. "INBOX").
    session
        .select(&config.mailbox)
        .await
        .context("IMAP SELECT failed")?;

    // Fetch all sequence numbers via SEARCH ALL.
    let search_query = match since {
        Some(date) => {
            let formatted = date.format("%d-%b-%Y").to_string();
            info!(mailbox = %config.mailbox, since = %formatted, "Using SEARCH SINCE (incremental scan)");
            format!("SINCE {}", formatted)
        }
        None => {
            info!(mailbox = %config.mailbox, "Using SEARCH ALL (full scan)");
            "ALL".to_string()
        }
    };

    let seq_set = session.search(&search_query).await.context("IMAP SEARCH failed")?;

    if seq_set.is_empty() {
        info!(mailbox = %config.mailbox, "No messages found");
        return Ok(vec![]);
    }

    // Build a comma-separated sequence-set string, e.g. "1,2,3".
    let seq_str: String = seq_set
        .iter()
        .map(|n: &u32| n.to_string())
        .collect::<Vec<_>>()
        .join(",");

    info!(mailbox = %config.mailbox, count = seq_set.len(), "Fetching message headers");

    // Fetch only the header fields we need; PEEK avoids marking messages as read.
    let messages_stream = session
        .fetch(
            &seq_str,
            "(BODY.PEEK[HEADER.FIELDS (FROM DATE SUBJECT MESSAGE-ID)])",
        )
        .await
        .context("IMAP FETCH failed")?;

    let messages: Vec<async_imap::types::Fetch> = messages_stream
        .try_collect()
        .await
        .context("collecting IMAP fetch results")?;

    let parser = MessageParser::default();
    let mut results = Vec::with_capacity(messages.len());

    for msg in &messages {
        // The header bytes live in the `header()` section of the fetch.
        let header_bytes = match msg.header() {
            Some(h) => h,
            None => {
                warn!("IMAP message missing header body — skipping");
                continue;
            }
        };

        // Use mail-parser to interpret the header block.
        let parsed = match parser.parse(header_bytes) {
            Some(p) => p,
            None => {
                warn!("Failed to parse message headers — skipping");
                continue;
            }
        };

        // Extract subject (fallback to empty string).
        let subject = parsed
            .subject()
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Extract sender from the first From address.
        let sender = parsed
            .from()
            .and_then(|al| al.first())
            .map(|addr| {
                addr.address()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| addr.name().map(|n| n.to_string()).unwrap_or_default())
            })
            .unwrap_or_default();

        // Extract date; fall back to now() if absent or unparseable.
        let received_at = parsed
            .date()
            .map(|d| {
                DateTime::from_timestamp(d.to_timestamp(), 0)
                    .unwrap_or_else(Utc::now)
            })
            .unwrap_or_else(Utc::now);

        // Extract Message-ID for deduplication; synthesise one if absent.
        let message_id = parsed
            .message_id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| format!("seq-{}", msg.message));

        results.push(RawEmail {
            message_id,
            subject,
            sender,
            account: String::new(), // tagged after fetch by tag_with_account()
            received_at,
        });
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use mail_parser::MessageParser;

    /// Given a static raw email byte string, assert that subject, sender, and
    /// date are parsed correctly.
    #[test]
    fn test_parse_raw_email_fields() {
        let raw = b"From: Alice <alice@example.com>\r\n\
                    Subject: Hello World\r\n\
                    Date: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
                    Message-ID: <unique-id-123@example.com>\r\n\
                    \r\n";

        let parser = MessageParser::default();
        let parsed = parser.parse(raw).expect("should parse");

        assert_eq!(parsed.subject().unwrap(), "Hello World");
        assert_eq!(
            parsed
                .from()
                .and_then(|al| al.first())
                .and_then(|a| a.address())
                .unwrap(),
            "alice@example.com"
        );
        assert_eq!(
            parsed.message_id().unwrap(),
            "unique-id-123@example.com"
        );
        // Date should be parseable and non-zero.
        let ts = parsed.date().unwrap().to_timestamp();
        assert!(ts > 0, "timestamp should be positive");
    }

    /// A message with no Message-ID header should not panic.
    #[test]
    fn test_parse_email_without_message_id() {
        let raw = b"From: Bob <bob@example.com>\r\n\
                    Subject: No ID\r\n\
                    Date: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
                    \r\n";

        let parser = MessageParser::default();
        let parsed = parser.parse(raw).expect("should parse");

        // message_id returns None when absent; our code falls back to a synthetic id.
        assert!(parsed.message_id().is_none());
    }
}
