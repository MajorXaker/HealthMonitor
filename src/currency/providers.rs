//! HTTP clients for the free rate APIs. None of them needs an API key.
//!
//! - Coinbase:  `GET https://api.coinbase.com/v2/prices/BTC-USD/spot`
//! - CoinGecko: `GET https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd`
//!   (fallback for crypto when Coinbase fails)
//! - Gold API:  `GET https://api.gold-api.com/price/XAU`
//! - NBRB:      `GET https://api.nbrb.by/exrates/rates/USD?parammode=2`
//!
//! Parsing is split from I/O so that it can be unit-tested with fixed payloads.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

const COINBASE_URL: &str = "https://api.coinbase.com/v2/prices";
const COINGECKO_URL: &str = "https://api.coingecko.com/api/v3/simple/price";
const GOLD_API_URL: &str = "https://api.gold-api.com/price";
const NBRB_URL: &str = "https://api.nbrb.by/exrates/rates";

/// A successfully fetched value.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub value: f64,
    /// Units of the base currency `value` refers to.
    pub scale: u32,
    pub source: &'static str,
    /// Date the rate officially applies to (NBRB only).
    pub rate_date: Option<NaiveDate>,
}

/// Shared HTTP client: 15 s timeout and an explicit User-Agent (some public
/// APIs reject requests without one).
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("healthmon/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client for currency rates")
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("GET {url} returned HTTP {status}");
    }
    resp.text().await.with_context(|| format!("GET {url}: failed to read body"))
}

/// Validate that a parsed number is usable as a rate.
fn positive(value: f64, what: &str) -> Result<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(anyhow!("{what}: invalid value {value}"))
    }
}

// ─── Crypto ──────────────────────────────────────────────────────────────────

/// Spot price for a pair like `"BTC-USD"`: Coinbase first, CoinGecko as fallback.
pub async fn crypto_spot(client: &reqwest::Client, pair: &str) -> Result<Quote> {
    let coinbase = async {
        let body = get_text(client, &format!("{COINBASE_URL}/{pair}/spot")).await?;
        parse_coinbase(&body)
    }
    .await;

    let err = match coinbase {
        Ok(value) => return Ok(Quote { value, scale: 1, source: "coinbase", rate_date: None }),
        Err(e) => e,
    };

    let (base, quote) = pair
        .split_once('-')
        .ok_or_else(|| anyhow!("crypto pair must look like BTC-USD, got {pair:?}"))?;
    let Some(id) = coingecko_id(base) else {
        return Err(err.context(format!("no CoinGecko fallback for {base}")));
    };
    let vs = quote.to_lowercase();
    let body = get_text(client, &format!("{COINGECKO_URL}?ids={id}&vs_currencies={vs}"))
        .await
        .map_err(|e2| anyhow!("coinbase: {err:#}; coingecko: {e2:#}"))?;
    let value = parse_coingecko(&body, id, &vs)?;
    Ok(Quote { value, scale: 1, source: "coingecko", rate_date: None })
}

/// CoinGecko coin ids for common tickers (CoinGecko needs ids, not tickers).
fn coingecko_id(ticker: &str) -> Option<&'static str> {
    match ticker.to_uppercase().as_str() {
        "BTC" => Some("bitcoin"),
        "ETH" => Some("ethereum"),
        "SOL" => Some("solana"),
        "LTC" => Some("litecoin"),
        "XRP" => Some("ripple"),
        "DOGE" => Some("dogecoin"),
        "TON" => Some("the-open-network"),
        "USDT" => Some("tether"),
        _ => None,
    }
}

/// `{"data":{"amount":"83430.68","base":"BTC","currency":"USD"}}`
pub fn parse_coinbase(body: &str) -> Result<f64> {
    #[derive(Deserialize)]
    struct Resp { data: Data }
    #[derive(Deserialize)]
    struct Data { amount: String }
    let resp: Resp = serde_json::from_str(body).context("coinbase: unexpected JSON")?;
    let value: f64 = resp.data.amount.parse().context("coinbase: amount is not a number")?;
    positive(value, "coinbase")
}

/// `{"bitcoin":{"usd":83439}}`
pub fn parse_coingecko(body: &str, id: &str, vs: &str) -> Result<f64> {
    let json: serde_json::Value = serde_json::from_str(body).context("coingecko: unexpected JSON")?;
    let value = json
        .get(id)
        .and_then(|coin| coin.get(vs))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow!("coingecko: no {id}/{vs} in response"))?;
    positive(value, "coingecko")
}

// ─── Metals ──────────────────────────────────────────────────────────────────

/// Spot price in USD per troy ounce for a symbol like `"XAU"`.
pub async fn metal_spot(client: &reqwest::Client, symbol: &str) -> Result<Quote> {
    let body = get_text(client, &format!("{GOLD_API_URL}/{symbol}")).await?;
    let value = parse_gold_api(&body)?;
    Ok(Quote { value, scale: 1, source: "gold-api.com", rate_date: None })
}

/// `{"currency":"USD","name":"Gold","price":4176.899902,"symbol":"XAU",...}`
pub fn parse_gold_api(body: &str) -> Result<f64> {
    #[derive(Deserialize)]
    struct Resp { price: f64, currency: Option<String> }
    let resp: Resp = serde_json::from_str(body).context("gold-api: unexpected JSON")?;
    if let Some(cur) = resp.currency.as_deref() {
        if !cur.eq_ignore_ascii_case("USD") {
            bail!("gold-api: expected USD price, got {cur}");
        }
    }
    positive(resp.price, "gold-api")
}

// ─── NBRB ────────────────────────────────────────────────────────────────────

/// Official NBRB rate in BYN for a currency abbreviation like `"USD"`.
/// `parammode=2` selects lookup by ISO letter code; without `ondate` the
/// rate for today is returned.
pub async fn nbrb_rate(client: &reqwest::Client, abbreviation: &str) -> Result<Quote> {
    let body = get_text(client, &format!("{NBRB_URL}/{abbreviation}?parammode=2")).await?;
    parse_nbrb(&body)
}

/// ```json
/// {"Cur_ID":456,"Date":"2026-10-12T00:00:00","Cur_Abbreviation":"RUB",
///  "Cur_Scale":100,"Cur_Name":"Российских рублей","Cur_OfficialRate":3.5899}
/// ```
pub fn parse_nbrb(body: &str) -> Result<Quote> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "Date")]
        date: String,
        #[serde(rename = "Cur_Scale")]
        scale: u32,
        #[serde(rename = "Cur_OfficialRate")]
        rate: Option<f64>,
    }
    let resp: Resp = serde_json::from_str(body).context("nbrb: unexpected JSON")?;
    let rate = resp.rate.ok_or_else(|| anyhow!("nbrb: Cur_OfficialRate is null"))?;
    if resp.scale == 0 {
        bail!("nbrb: Cur_Scale is 0");
    }
    // "2026-10-12T00:00:00" → 2026-10-12
    let rate_date = resp
        .date
        .get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    Ok(Quote { value: positive(rate, "nbrb")?, scale: resp.scale, source: "nbrb.by", rate_date })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coinbase_payload() {
        let body = r#"{"data":{"amount":"83430.68","base":"BTC","currency":"USD"}}"#;
        assert_eq!(parse_coinbase(body).unwrap(), 83430.68);
    }

    #[test]
    fn coinbase_error_payload() {
        let body = r#"{"errors":[{"id":"not_found","message":"Invalid currency"}]}"#;
        assert!(parse_coinbase(body).is_err());
    }

    #[test]
    fn coingecko_payload() {
        let body = r#"{"bitcoin":{"usd":83439},"ethereum":{"usd":2551.08}}"#;
        assert_eq!(parse_coingecko(body, "ethereum", "usd").unwrap(), 2551.08);
        assert_eq!(parse_coingecko(body, "bitcoin", "usd").unwrap(), 83439.0);
        assert!(parse_coingecko(body, "solana", "usd").is_err());
    }

    #[test]
    fn gold_api_payload() {
        let body = r#"{"currency":"USD","currencySymbol":"$","exchangeRate":1.0,"name":"Gold",
            "price":4176.899902,"symbol":"XAU","updatedAt":"2026-10-12T09:21:10Z"}"#;
        assert!((parse_gold_api(body).unwrap() - 4176.899902).abs() < 1e-9);
    }

    #[test]
    fn gold_api_rejects_non_usd() {
        let body = r#"{"currency":"EUR","price":3500.0}"#;
        assert!(parse_gold_api(body).is_err());
    }

    #[test]
    fn nbrb_payload_with_scale() {
        let body = r#"{"Cur_ID":456,"Cur_Code":"643","Date":"2026-10-12T00:00:00",
            "Cur_Abbreviation":"RUB","Cur_Scale":100,"Cur_Name":"Российских рублей",
            "Cur_OfficialRate":3.5899}"#;
        let q = parse_nbrb(body).unwrap();
        assert_eq!(q.value, 3.5899);
        assert_eq!(q.scale, 100);
        assert_eq!(q.rate_date, NaiveDate::from_ymd_opt(2026, 10, 12));
    }

    #[test]
    fn nbrb_null_rate_is_error() {
        let body = r#"{"Date":"2026-10-12T00:00:00","Cur_Scale":1,"Cur_OfficialRate":null}"#;
        assert!(parse_nbrb(body).is_err());
    }

    #[test]
    fn invalid_numbers_rejected() {
        assert!(parse_coinbase(r#"{"data":{"amount":"0"}}"#).is_err());
        assert!(parse_coinbase(r#"{"data":{"amount":"abc"}}"#).is_err());
    }

    /// Hits the real APIs. Run with `cargo test -- --ignored live_`.
    #[tokio::test]
    #[ignore]
    async fn live_all_providers() {
        let client = build_client().unwrap();
        for pair in ["BTC-USD", "ETH-USD"] {
            let q = crypto_spot(&client, pair).await.unwrap();
            println!("{pair}: {q:?}");
        }
        for sym in ["XAU", "XAG"] {
            let q = metal_spot(&client, sym).await.unwrap();
            println!("{sym}: {q:?}");
        }
        for cur in ["USD", "EUR", "RUB"] {
            let q = nbrb_rate(&client, cur).await.unwrap();
            println!("{cur}: {q:?}");
        }
    }
}
