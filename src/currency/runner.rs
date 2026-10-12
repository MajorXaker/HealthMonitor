//! Background loops that keep the in-memory rate table fresh.
//!
//! One tokio task per group. Each task fetches all of its rates, then sleeps
//! until the next scheduled run. After a failed fetch the task retries sooner
//! (5 minutes for interval groups, 15 minutes for the daily NBRB group), and
//! the previous value is kept and flagged stale once it gets too old.
//!
//! Successful fetches are stored in PostgreSQL. On startup a group whose stored
//! values are all still fresh waits for its next regular run instead of
//! fetching immediately (see [`first_run`]).

use std::time::Duration;

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use sqlx::PgPool;
use tracing::{info, warn};

use super::providers::{self, Quote};
use super::{Rate, RateGroup, RateSlot, RateState};
use crate::config::CurrencyConfig;

const INTERVAL_RETRY: Duration = Duration::from_secs(5 * 60);
const DAILY_RETRY: Duration = Duration::from_secs(15 * 60);
/// NBRB values older than this are stale (one daily update plus an hour of slack).
const NBRB_MAX_AGE: Duration = Duration::from_secs(25 * 3600);

/// When a group runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Schedule {
    /// Fixed period between runs.
    Every(Duration),
    /// Once a day at the given UTC time.
    DailyAt(NaiveTime),
}

/// Build the rate table (all values empty) in display order:
/// crypto, then metals, then NBRB.
pub fn initial_slots(cfg: &CurrencyConfig) -> Vec<RateSlot> {
    let mut slots = Vec::new();
    let crypto_age = max_age_for(cfg.crypto_interval_minutes);
    for key in &cfg.crypto_pairs {
        let pair = key.to_uppercase();
        slots.push(empty_slot(pair.clone(), pair, RateGroup::Crypto, crypto_age, 2));
    }
    let metal_age = max_age_for(cfg.metals_interval_minutes);
    for key in &cfg.metals {
        let sym = key.to_uppercase();
        let label = metal_label(&sym);
        slots.push(empty_slot(format!("{sym}-USD"), label, RateGroup::Metal, metal_age, 2));
    }
    for key in &cfg.nbrb_currencies {
        let cur = key.to_uppercase();
        let pair = format!("{cur}-BYN");
        slots.push(empty_slot(pair.clone(), pair, RateGroup::Nbrb, NBRB_MAX_AGE, 4));
    }
    slots
}

/// Spawn one background task per non-empty group.
/// Expects `state` to be pre-filled (and restored from the DB) by the caller.
pub fn spawn_currency_loops(cfg: CurrencyConfig, state: RateState, pool: PgPool) {
    let client = match providers::build_client() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Currency rates disabled: cannot build HTTP client");
            return;
        }
    };

    let daily_at = parse_daily_time(&cfg.nbrb_daily_at_utc);
    let groups = [
        (RateGroup::Crypto, cfg.crypto_pairs.clone(), Schedule::Every(minutes(cfg.crypto_interval_minutes))),
        (RateGroup::Metal, cfg.metals.clone(), Schedule::Every(minutes(cfg.metals_interval_minutes))),
        (RateGroup::Nbrb, cfg.nbrb_currencies.clone(), Schedule::DailyAt(daily_at)),
    ];

    for (group, keys, schedule) in groups {
        if keys.is_empty() {
            continue;
        }
        info!(?group, ?keys, ?schedule, "Starting currency rate loop");
        let state = state.clone();
        let client = client.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            run_group(group, keys, schedule, state, client, pool).await;
        });
    }
}

async fn run_group(
    group: RateGroup,
    keys: Vec<String>,
    schedule: Schedule,
    state: RateState,
    client: reqwest::Client,
    pool: PgPool,
) {
    // Values restored from the database may still be fresh: wait for them.
    let oldest = oldest_fetch(group, &keys, &state).await;
    let now = Utc::now();
    let start = first_run(schedule, oldest, now);
    if start > now {
        info!(?group, first_run = %start, "Stored rates are fresh — skipping startup fetch");
        let wait = (start - now).to_std().unwrap_or(Duration::ZERO);
        tokio::time::sleep(wait).await;
    }

    loop {
        let all_ok = refresh_group(group, &keys, &state, &client, &pool).await;
        let now = Utc::now();
        let next = next_run(schedule, all_ok, now);
        let wait = (next - now).to_std().unwrap_or(Duration::ZERO);
        info!(?group, all_ok, next_run = %next, "Currency rates refreshed");
        tokio::time::sleep(wait).await;
    }
}

/// Fetch every rate of a group and store successes. Returns `true` if all succeeded.
async fn refresh_group(
    group: RateGroup,
    keys: &[String],
    state: &RateState,
    client: &reqwest::Client,
    pool: &PgPool,
) -> bool {
    let mut all_ok = true;
    for key in keys {
        let key = key.to_uppercase();
        let result = match group {
            RateGroup::Crypto => providers::crypto_spot(client, &key).await,
            RateGroup::Metal => providers::metal_spot(client, &key).await,
            RateGroup::Nbrb => providers::nbrb_rate(client, &key).await,
        };
        let pair = pair_for(group, &key);
        match result {
            Ok(quote) => {
                // Update memory first, then persist outside the lock.
                let updated = {
                    let mut slots = state.write().await;
                    slots.iter_mut().find(|s| s.rate.pair == pair).map(|slot| {
                        apply_quote(&mut slot.rate, &quote, Utc::now());
                        slot.rate.clone()
                    })
                };
                if let Some(rate) = updated {
                    if let Err(e) = super::db::upsert_rate(pool, &rate).await {
                        // Not fatal: the value is still served from memory.
                        warn!(%pair, error = %format!("{e:#}"), "Failed to store rate");
                    }
                }
            }
            Err(e) => {
                all_ok = false;
                warn!(%pair, error = %format!("{e:#}"), "Failed to fetch rate");
            }
        }
    }
    all_ok
}

/// Store a fetched quote in a rate entry.
pub fn apply_quote(rate: &mut Rate, quote: &Quote, now: DateTime<Utc>) {
    rate.value = Some(quote.value);
    rate.scale = quote.scale;
    rate.source = Some(quote.source.to_string());
    rate.rate_date = quote.rate_date;
    rate.fetched_at = Some(now);
    if rate.group == RateGroup::Nbrb {
        rate.label = nbrb_label(&rate.pair, quote.scale);
    }
}

/// NBRB quotes some currencies per 10/100/1000 units: show it in the label.
pub fn nbrb_label(pair: &str, scale: u32) -> String {
    if scale == 1 {
        pair.to_string()
    } else {
        format!("{scale}{pair}")
    }
}

/// Oldest `fetched_at` among a group's rates; `None` if any of them has no value.
async fn oldest_fetch(group: RateGroup, keys: &[String], state: &RateState) -> Option<DateTime<Utc>> {
    let slots = state.read().await;
    let mut oldest: Option<DateTime<Utc>> = None;
    for key in keys {
        let pair = pair_for(group, &key.to_uppercase());
        let slot = slots.iter().find(|s| s.rate.pair == pair)?;
        slot.rate.value?;
        let at = slot.rate.fetched_at?;
        oldest = Some(oldest.map_or(at, |o| o.min(at)));
    }
    oldest
}

/// When a group should first run after startup, given the oldest stored fetch.
///
/// - Nothing stored (or any rate missing) → now.
/// - Interval groups → when the oldest stored value is one period old
///   (now, if that has already passed).
/// - Daily groups → now if the stored values predate the most recent daily
///   slot, otherwise the next slot.
pub fn first_run(schedule: Schedule, oldest: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    let Some(oldest) = oldest else { return now };
    match schedule {
        Schedule::Every(period) => (oldest + delta(period)).max(now),
        Schedule::DailyAt(at) => {
            let next = next_daily(now, at);
            let last_slot = next - TimeDelta::days(1);
            if oldest >= last_slot { next } else { now }
        }
    }
}

/// Next run time. Failures retry sooner, but a daily group never skips its
/// regular slot.
pub fn next_run(schedule: Schedule, all_ok: bool, now: DateTime<Utc>) -> DateTime<Utc> {
    match schedule {
        Schedule::Every(period) => {
            let wait = if all_ok { period } else { period.min(INTERVAL_RETRY) };
            now + delta(wait)
        }
        Schedule::DailyAt(at) => {
            let daily = next_daily(now, at);
            if all_ok {
                daily
            } else {
                daily.min(now + delta(DAILY_RETRY))
            }
        }
    }
}

/// The first `at` (UTC) strictly after `now`.
pub fn next_daily(now: DateTime<Utc>, at: NaiveTime) -> DateTime<Utc> {
    let today = now.date_naive().and_time(at).and_utc();
    if today > now {
        today
    } else {
        today + TimeDelta::days(1)
    }
}

/// `"07:00"` → 07:00. Invalid input logs a warning and falls back to 07:00.
pub fn parse_daily_time(s: &str) -> NaiveTime {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").unwrap_or_else(|_| {
        warn!(value = s, "Invalid currency.nbrb_daily_at_utc (expected HH:MM) — using 07:00");
        NaiveTime::from_hms_opt(7, 0, 0).expect("valid time")
    })
}

fn pair_for(group: RateGroup, key: &str) -> String {
    match group {
        RateGroup::Crypto => key.to_string(),
        RateGroup::Metal => format!("{key}-USD"),
        RateGroup::Nbrb => format!("{key}-BYN"),
    }
}

fn metal_label(symbol: &str) -> String {
    match symbol {
        "XAU" => "Gold-USD".into(),
        "XAG" => "Silver-USD".into(),
        "XPT" => "Plat-USD".into(),
        "XPD" => "Pallad-USD".into(),
        other => format!("{other}-USD"),
    }
}

fn empty_slot(pair: String, label: String, group: RateGroup, max_age: Duration, decimals: usize) -> RateSlot {
    RateSlot {
        rate: Rate {
            pair,
            label,
            group,
            value: None,
            scale: 1,
            source: None,
            rate_date: None,
            fetched_at: None,
            stale: true,
        },
        max_age,
        decimals,
    }
}

/// Stale after two missed runs plus five minutes of slack.
fn max_age_for(interval_minutes: u64) -> Duration {
    minutes(interval_minutes) * 2 + Duration::from_secs(5 * 60)
}

/// Minutes → Duration, never below one minute.
fn minutes(m: u64) -> Duration {
    Duration::from_secs(m.max(1) * 60)
}

fn delta(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::zero())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn next_daily_before_and_after() {
        let seven = t(7, 0);
        let before = Utc.with_ymd_and_hms(2026, 10, 12, 6, 59, 0).unwrap();
        let exactly = Utc.with_ymd_and_hms(2026, 10, 12, 7, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 10, 12, 9, 30, 0).unwrap();
        assert_eq!(next_daily(before, seven), Utc.with_ymd_and_hms(2026, 10, 12, 7, 0, 0).unwrap());
        assert_eq!(next_daily(exactly, seven), Utc.with_ymd_and_hms(2026, 10, 13, 7, 0, 0).unwrap());
        assert_eq!(next_daily(after, seven), Utc.with_ymd_and_hms(2026, 10, 13, 7, 0, 0).unwrap());
    }

    #[test]
    fn daily_failure_retries_but_keeps_slot() {
        let seven = t(7, 0);
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        assert_eq!(next_run(Schedule::DailyAt(seven), false, now), now + TimeDelta::minutes(15));
        // 6:50 failure → retry would be 7:05, but the 7:00 slot comes first
        let near = Utc.with_ymd_and_hms(2026, 10, 12, 6, 50, 0).unwrap();
        assert_eq!(
            next_run(Schedule::DailyAt(seven), false, near),
            Utc.with_ymd_and_hms(2026, 10, 12, 7, 0, 0).unwrap()
        );
    }

    #[test]
    fn interval_schedule() {
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let every30 = Schedule::Every(Duration::from_secs(1800));
        assert_eq!(next_run(every30, true, now), now + TimeDelta::minutes(30));
        assert_eq!(next_run(every30, false, now), now + TimeDelta::minutes(5));
    }

    #[test]
    fn first_run_without_stored_values_is_now() {
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let every30 = Schedule::Every(Duration::from_secs(1800));
        assert_eq!(first_run(every30, None, now), now);
        assert_eq!(first_run(Schedule::DailyAt(t(7, 0)), None, now), now);
    }

    #[test]
    fn first_run_interval_waits_for_fresh_values() {
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let every30 = Schedule::Every(Duration::from_secs(1800));
        // fetched 10 min ago → next fetch in 20 min
        let recent = now - TimeDelta::minutes(10);
        assert_eq!(first_run(every30, Some(recent), now), now + TimeDelta::minutes(20));
        // fetched 45 min ago → overdue → now
        let old = now - TimeDelta::minutes(45);
        assert_eq!(first_run(every30, Some(old), now), now);
    }

    #[test]
    fn first_run_daily() {
        let seven = Schedule::DailyAt(t(7, 0));
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let tomorrow_7 = Utc.with_ymd_and_hms(2026, 10, 13, 7, 0, 0).unwrap();
        // fetched today after the 07:00 slot → wait for tomorrow
        let today_730 = Utc.with_ymd_and_hms(2026, 10, 12, 7, 30, 0).unwrap();
        assert_eq!(first_run(seven, Some(today_730), now), tomorrow_7);
        // fetched yesterday → today's slot was missed → now
        let yesterday = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        assert_eq!(first_run(seven, Some(yesterday), now), now);
        // 06:30 now, fetched at 08:00 yesterday → still current until 07:00 today
        let early = Utc.with_ymd_and_hms(2026, 10, 12, 6, 30, 0).unwrap();
        let yday_8 = Utc.with_ymd_and_hms(2026, 10, 11, 8, 0, 0).unwrap();
        assert_eq!(
            first_run(seven, Some(yday_8), early),
            Utc.with_ymd_and_hms(2026, 10, 12, 7, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_time_fallback() {
        assert_eq!(parse_daily_time("07:00"), t(7, 0));
        assert_eq!(parse_daily_time(" 18:45 "), t(18, 45));
        assert_eq!(parse_daily_time("7am"), t(7, 0));
    }

    #[test]
    fn default_table_order_and_labels() {
        let slots = initial_slots(&CurrencyConfig::default());
        let labels: Vec<_> = slots.iter().map(|s| s.rate.label.as_str()).collect();
        assert_eq!(
            labels,
            ["BTC-USD", "ETH-USD", "Gold-USD", "Silver-USD", "USD-BYN", "EUR-BYN", "RUB-BYN"]
        );
        assert!(slots.iter().all(|s| s.rate.value.is_none()));
    }

    #[test]
    fn nbrb_scale_goes_into_label() {
        let mut slots = initial_slots(&CurrencyConfig::default());
        let rub = slots.iter_mut().find(|s| s.rate.pair == "RUB-BYN").unwrap();
        let quote = Quote { value: 3.5899, scale: 100, source: "nbrb.by", rate_date: None };
        apply_quote(&mut rub.rate, &quote, Utc::now());
        assert_eq!(rub.rate.label, "100RUB-BYN");
        assert_eq!(rub.rate.value, Some(3.5899));
    }
}
