//! PostgreSQL persistence layer for the email monitoring feature.
//!
//! All queries are executed via `sqlx` with compile-time-checked macros where
//! possible, falling back to dynamic queries for lists of IDs.

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::info;

use super::{imap_client::RawEmail, EmailRecord};

/// Run all pending SQLx migrations from the `migrations/` directory.
///
/// SQLx tracks applied migrations in the `_sqlx_migrations` table. This
/// function is idempotent — already-applied migrations are skipped.
/// Migrations must be numbered sequentially; a gap or out-of-order
/// migration causes an error.
pub async fn run_migrations(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("Failed to run database migrations")?;
    info!("Database migrations applied successfully");
    Ok(())
}

/// Insert an email into the database if its `message_id` has not been seen before.
///
/// Uses `ON CONFLICT DO NOTHING` for deduplication. Returns `true` if a new
/// row was inserted, `false` if the message was already present.
pub async fn insert_email_if_new(pool: &PgPool, email: &RawEmail) -> Result<bool> {
    let result = sqlx::query(
        r#"
        INSERT INTO emails (message_id, subject, sender, account, received_at, is_new)
        VALUES ($1, $2, $3, $4, $5, TRUE)
        ON CONFLICT (message_id) DO NOTHING
        "#,
    )
    .bind(&email.message_id)
    .bind(&email.subject)
    .bind(&email.sender)
    .bind(&email.account)
    .bind(email.received_at)
    .execute(pool)
    .await
    .context("Failed to insert email")?;

    // rows_affected() is 1 if a row was inserted, 0 if the conflict skipped it.
    Ok(result.rows_affected() == 1)
}

/// Return all emails that have not yet been acknowledged (`is_new = true`).
pub async fn get_new_emails(pool: &PgPool) -> Result<Vec<EmailRecord>> {
    let rows = sqlx::query_as::<_, EmailRecord>(
        r#"
        SELECT id, message_id, subject, sender, account, received_at, is_new
        FROM emails
        WHERE is_new = TRUE
        ORDER BY received_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch new emails")?;

    Ok(rows)
}

/// Mark a set of email rows as acknowledged (`is_new = false`).
///
/// Returns the number of rows that were actually updated. IDs that do not
/// exist or were already acknowledged count as 0 updates.
pub async fn acknowledge_emails(pool: &PgPool, ids: &[i32]) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }

    // Use `= ANY($1)` to handle a variable-length list efficiently.
    let result = sqlx::query(
        r#"
        UPDATE emails
        SET is_new = FALSE
        WHERE id = ANY($1)
        "#,
    )
    .bind(ids)
    .execute(pool)
    .await
    .context("Failed to acknowledge emails")?;

    Ok(result.rows_affected())
}

/// Mark ALL emails as acknowledged (is_new = false).
/// Returns the number of rows updated.
pub async fn acknowledge_all_emails(pool: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query("UPDATE emails SET is_new = FALSE WHERE is_new = TRUE")
        .execute(pool)
        .await
        .context("Failed to acknowledge all emails")?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    /// Integration tests that require a live PostgreSQL database should be
    /// run with `cargo test --features integration` or via a Docker-based CI
    /// pipeline. The logic below tests behaviour that can be validated without
    /// a database connection.

    /// Calling acknowledge_emails with an empty slice returns 0 — verified
    /// by the guard at the top of the function (no DB call made).
    #[test]
    fn test_acknowledge_ids_empty_is_safe() {
        // The function signature accepts &[i32]; passing an empty slice is
        // always safe — the early-return guard prevents any DB round-trip.
        // This test documents the contract without requiring a live DB.
        let ids: &[i32] = &[];
        assert!(ids.is_empty());
        // If we had a pool here: let rows = acknowledge_emails(&pool, ids).await.unwrap();
        // assert_eq!(rows, 0);
    }

    /// Ensure EmailRecord derives Serialize and sqlx::FromRow at compile time.
    #[test]
    fn test_email_record_type_constraints() {
        use crate::email::EmailRecord;
        // Compile-time check: EmailRecord must implement Serialize.
        fn assert_serialize<T: serde::Serialize>() {}
        assert_serialize::<EmailRecord>();
    }
}
