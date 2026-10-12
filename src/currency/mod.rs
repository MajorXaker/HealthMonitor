//! Currency and commodity rate monitoring.
//!
//! Three groups of rates are refreshed by independent background loops:
//!
//! | Group  | Source                                   | Default schedule        |
//! |--------|------------------------------------------|-------------------------|
//! | crypto | Coinbase spot price (CoinGecko fallback) | every 30 minutes        |
//! | metal  | gold-api.com (USD per troy ounce)        | every 60 minutes        |
//! | nbrb   | National Bank of the Republic of Belarus | daily at 07:00 UTC      |
//!
//! Requests are always served from memory. Every successful fetch is also
//! written to the `currency_rates` table, so after a restart the last known
//! values are available immediately and groups whose stored values are still
//! fresh skip their first fetch.

pub mod db;
pub mod format;
pub mod providers;
pub mod runner;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

/// Which background loop owns a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RateGroup {
    /// Cryptocurrency spot price in USD.
    Crypto,
    /// Precious metal spot price in USD per troy ounce.
    Metal,
    /// Official NBRB exchange rate in BYN.
    Nbrb,
}

impl RateGroup {
    /// Name used in JSON and in the `currency_rates.rate_group` column.
    pub fn as_str(self) -> &'static str {
        match self {
            RateGroup::Crypto => "crypto",
            RateGroup::Metal => "metal",
            RateGroup::Nbrb => "nbrb",
        }
    }
}

/// One rate as exposed by `GET /currency/json`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Rate {
    /// Stable identifier, e.g. `"BTC-USD"`, `"XAU-USD"`, `"RUB-BYN"`.
    pub pair: String,
    /// Short label used in the plain-text rendering, e.g. `"Gold-USD"`, `"100RUB-BYN"`.
    pub label: String,
    /// Source group.
    pub group: RateGroup,
    /// Latest value, or `null` before the first successful fetch.
    /// For NBRB rates this is the price of `scale` units of the base currency.
    pub value: Option<f64>,
    /// Units of the base currency `value` refers to (NBRB quotes RUB per 100).
    pub scale: u32,
    /// Provider that delivered the current value, e.g. `"coinbase"`.
    pub source: Option<String>,
    /// Date the official rate applies to (NBRB only).
    pub rate_date: Option<NaiveDate>,
    /// When healthmon last fetched this value successfully.
    pub fetched_at: Option<DateTime<Utc>>,
    /// `true` when the value is missing or older than expected for its schedule.
    pub stale: bool,
}

/// A rate plus the bookkeeping that is not exposed over the API.
#[derive(Debug, Clone)]
pub struct RateSlot {
    pub rate: Rate,
    /// Older than this → `stale = true`.
    pub max_age: Duration,
    /// Decimal places in the plain-text rendering.
    pub decimals: usize,
}

/// Shared, in-memory rate table in display order.
pub type RateState = Arc<RwLock<Vec<RateSlot>>>;

/// Return a copy of all rates with `stale` evaluated at `now`.
pub fn snapshot(slots: &[RateSlot], now: DateTime<Utc>) -> Vec<Rate> {
    slots
        .iter()
        .map(|slot| {
            let mut rate = slot.rate.clone();
            rate.stale = match (rate.value, rate.fetched_at) {
                (Some(_), Some(at)) => (now - at)
                    .to_std()
                    .map(|age| age > slot.max_age)
                    .unwrap_or(false), // fetched_at in the future: clock skew, not stale
                _ => true,
            };
            rate
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(value: Option<f64>, fetched_at: Option<DateTime<Utc>>) -> RateSlot {
        RateSlot {
            rate: Rate {
                pair: "BTC-USD".into(),
                label: "BTC-USD".into(),
                group: RateGroup::Crypto,
                value,
                scale: 1,
                source: None,
                rate_date: None,
                fetched_at,
                stale: false,
            },
            max_age: Duration::from_secs(3600),
            decimals: 2,
        }
    }

    #[test]
    fn missing_value_is_stale() {
        let now = Utc::now();
        assert!(snapshot(&[slot(None, None)], now)[0].stale);
    }

    #[test]
    fn fresh_and_old_values() {
        let now = Utc::now();
        let fresh = slot(Some(1.0), Some(now - chrono::TimeDelta::minutes(10)));
        let old = slot(Some(1.0), Some(now - chrono::TimeDelta::minutes(61)));
        let rates = snapshot(&[fresh, old], now);
        assert!(!rates[0].stale);
        assert!(rates[1].stale);
    }
}
