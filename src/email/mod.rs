//! Email monitoring module: shared types and sub-modules.

pub mod db;
pub mod imap_client;
pub mod runner;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// A row from the `emails` database table.
///
/// Returned by the `GET /emails` endpoint (filtered to `is_new = true`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct EmailRecord {
    /// Auto-incrementing primary key.
    pub id: i32,
    /// Unique identifier extracted from the email's `Message-ID` header.
    pub message_id: String,
    /// Email subject line.
    pub subject: String,
    /// Sender address (from the `From` header).
    pub sender: String,
    /// Account name from the EmailConfig that fetched this message.
    pub account: String,
    /// When the message was received, in UTC.
    pub received_at: DateTime<Utc>,
    /// `true` if the message has not yet been acknowledged via the API.
    pub is_new: bool,
}
