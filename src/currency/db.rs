//! PostgreSQL persistence for the latest currency rates (`currency_rates` table,
//! migration 0002).
//!
//! The database is a cache of the last successful fetch per pair. Requests to
//! `/currency` are still served from memory; the table only matters at startup,
//! where it restores the last known values and lets fresh groups skip their
//! first fetch.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use super::{Rate, RateSlot};
use super::runner::nbrb_label;
use super::RateGroup;

/// A row of `currency_rates`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredRate {
    pub pair: String,
    pub rate_group: String,
    pub value: f64,
    pub scale: i32,
    pub source: String,
    pub rate_date: Option<NaiveDate>,
    pub fetched_at: DateTime<Utc>,
}

/// Read all stored rates.
pub async fn load_rates(pool: &PgPool) -> Result<Vec<StoredRate>> {
    sqlx::query_as::<_, StoredRate>(
        r#"
        SELECT pair, rate_group, value, scale, source, rate_date, fetched_at
        FROM currency_rates
        "#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to load currency rates")
}

/// Insert or update the stored value for one pair.
/// Rates without a value are not stored.
pub async fn upsert_rate(pool: &PgPool, rate: &Rate) -> Result<()> {
    let (Some(value), Some(fetched_at), Some(source)) =
        (rate.value, rate.fetched_at, rate.source.as_deref())
    else {
        return Ok(());
    };
    sqlx::query(
        r#"
        INSERT INTO currency_rates (pair, rate_group, value, scale, source, rate_date, fetched_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (pair) DO UPDATE SET
            rate_group = EXCLUDED.rate_group,
            value      = EXCLUDED.value,
            scale      = EXCLUDED.scale,
            source     = EXCLUDED.source,
            rate_date  = EXCLUDED.rate_date,
            fetched_at = EXCLUDED.fetched_at
        "#,
    )
    .bind(&rate.pair)
    .bind(rate.group.as_str())
    .bind(value)
    .bind(i32::try_from(rate.scale).unwrap_or(1))
    .bind(source)
    .bind(rate.rate_date)
    .bind(fetched_at)
    .execute(pool)
    .await
    .with_context(|| format!("Failed to store rate {}", rate.pair))?;
    Ok(())
}

/// Copy stored values into the matching slots (by `pair` and group).
/// Rows for pairs no longer in the config are ignored. Returns how many slots
/// were restored.
pub fn restore_into(slots: &mut [RateSlot], stored: &[StoredRate]) -> usize {
    let mut restored = 0;
    for row in stored {
        let Some(slot) = slots
            .iter_mut()
            .find(|s| s.rate.pair == row.pair && s.rate.group.as_str() == row.rate_group)
        else {
            continue;
        };
        let rate = &mut slot.rate;
        rate.value = Some(row.value);
        rate.scale = u32::try_from(row.scale).ok().filter(|s| *s > 0).unwrap_or(1);
        rate.source = Some(row.source.clone());
        rate.rate_date = row.rate_date;
        rate.fetched_at = Some(row.fetched_at);
        if rate.group == RateGroup::Nbrb {
            rate.label = nbrb_label(&rate.pair, rate.scale);
        }
        restored += 1;
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CurrencyConfig;
    use crate::currency::runner::initial_slots;

    fn row(pair: &str, group: &str, value: f64, scale: i32) -> StoredRate {
        StoredRate {
            pair: pair.into(),
            rate_group: group.into(),
            value,
            scale,
            source: "test".into(),
            rate_date: None,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn restore_matches_by_pair_and_group() {
        let mut slots = initial_slots(&CurrencyConfig::default());
        let stored = vec![
            row("BTC-USD", "crypto", 83000.0, 1),
            row("RUB-BYN", "nbrb", 3.59, 100),
            row("DOGE-USD", "crypto", 0.1, 1),   // not configured → ignored
            row("XAU-USD", "crypto", 1.0, 1),    // wrong group → ignored
        ];
        assert_eq!(restore_into(&mut slots, &stored), 2);

        let btc = slots.iter().find(|s| s.rate.pair == "BTC-USD").unwrap();
        assert_eq!(btc.rate.value, Some(83000.0));
        let rub = slots.iter().find(|s| s.rate.pair == "RUB-BYN").unwrap();
        assert_eq!(rub.rate.label, "100RUB-BYN");
        assert_eq!(rub.rate.scale, 100);
        let gold = slots.iter().find(|s| s.rate.pair == "XAU-USD").unwrap();
        assert!(gold.rate.value.is_none());
    }

    /// Round trip through a real database. Needs `DATABASE_URL`; run with
    /// `DATABASE_URL=postgres://... cargo test -- --ignored db_`.
    #[tokio::test]
    #[ignore]
    async fn db_upsert_and_load_round_trip() {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let pool = PgPool::connect(&url).await.unwrap();
        crate::email::db::run_migrations(&pool).await.unwrap();

        let mut slots = initial_slots(&CurrencyConfig::default());
        let rub = slots.iter_mut().find(|s| s.rate.pair == "RUB-BYN").unwrap();
        rub.rate.value = Some(3.5);
        rub.rate.scale = 100;
        rub.rate.source = Some("nbrb.by".into());
        rub.rate.rate_date = NaiveDate::from_ymd_opt(2026, 10, 12);
        rub.rate.fetched_at = Some(Utc::now());
        upsert_rate(&pool, &rub.rate).await.unwrap();
        rub.rate.value = Some(3.6); // update in place
        upsert_rate(&pool, &rub.rate).await.unwrap();

        let stored = load_rates(&pool).await.unwrap();
        let row = stored.iter().find(|r| r.pair == "RUB-BYN").unwrap();
        assert_eq!(row.value, 3.6);
        assert_eq!(row.scale, 100);
        assert_eq!(row.rate_group, "nbrb");
        assert_eq!(row.rate_date, NaiveDate::from_ymd_opt(2026, 10, 12));
    }
}
