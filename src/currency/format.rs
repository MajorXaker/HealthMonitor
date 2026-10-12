//! Plain-text rendering for `GET /currency`.
//!
//! The output is meant for the device's 128x64 display: 21 monospace
//! characters per line, one rate per line, no header (the device draws its own).
//!
//! ```text
//! BTC-USD     83 430.68
//! ETH-USD      2 551.08
//! Gold-USD     4 176.90
//! Silver-USD      60.75
//! USD-BYN        3.0392
//! EUR-BYN        3.5401
//! 100RUB-BYN     3.5899
//! ```
//!
//! Layout per line: label left-aligned in 10 columns, one marker column
//! (`*` when the value is stale, space otherwise), value right-aligned in
//! 10 columns. Missing values are shown as `n/a`.

use super::{Rate, RateSlot};

/// Characters per line on the device display.
pub const LINE_WIDTH: usize = 21;
const LABEL_WIDTH: usize = 10;
const VALUE_WIDTH: usize = LINE_WIDTH - LABEL_WIDTH - 1;

/// Render all rates, one per line, each line ending with `\n`.
pub fn render_text(slots: &[RateSlot], rates: &[Rate]) -> String {
    let mut out = String::new();
    for (slot, rate) in slots.iter().zip(rates) {
        let label = truncate_chars(&rate.label, LABEL_WIDTH);
        let marker = if rate.stale { '*' } else { ' ' };
        let value = match rate.value {
            Some(v) => format_number(v, slot.decimals),
            None => "n/a".to_string(),
        };
        out.push_str(&format!(
            "{label:<lw$}{marker}{value:>vw$}\n",
            lw = LABEL_WIDTH,
            vw = VALUE_WIDTH
        ));
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Fixed decimals with a space as thousands separator: `83430.68` → `"83 430.68"`.
pub fn format_number(value: f64, decimals: usize) -> String {
    let fixed = format!("{:.*}", decimals, value.abs());
    let (int_part, frac_part) = match fixed.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (fixed.as_str(), None),
    };

    let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (i, ch) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(ch);
    }

    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    out.push_str(&grouped);
    if let Some(f) = frac_part {
        out.push('.');
        out.push_str(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::currency::{RateGroup, RateSlot};
    use std::time::Duration;

    fn slot(label: &str, value: Option<f64>, decimals: usize, stale: bool) -> (RateSlot, Rate) {
        let rate = Rate {
            pair: label.into(),
            label: label.into(),
            group: RateGroup::Crypto,
            value,
            scale: 1,
            source: None,
            rate_date: None,
            fetched_at: None,
            stale,
        };
        (RateSlot { rate: rate.clone(), max_age: Duration::from_secs(1), decimals }, rate)
    }

    #[test]
    fn number_grouping() {
        assert_eq!(format_number(83430.68, 2), "83 430.68");
        assert_eq!(format_number(1234567.0, 0), "1 234 567");
        assert_eq!(format_number(999.995, 2), "1 000.00"); // rounding carries into grouping
        assert_eq!(format_number(60.749001, 2), "60.75");
        assert_eq!(format_number(3.0392, 4), "3.0392");
        assert_eq!(format_number(-1500.5, 1), "-1 500.5");
    }

    #[test]
    fn lines_are_21_chars() {
        let rows = [
            slot("BTC-USD", Some(83430.68), 2, false),
            slot("100RUB-BYN", Some(3.5899), 4, true),
            slot("ETH-USD", None, 2, true),
        ];
        let slots: Vec<_> = rows.iter().map(|(s, _)| s.clone()).collect();
        let rates: Vec<_> = rows.iter().map(|(_, r)| r.clone()).collect();
        let text = render_text(&slots, &rates);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in &lines {
            assert_eq!(l.chars().count(), LINE_WIDTH, "line {l:?}");
        }
        assert_eq!(lines[0], "BTC-USD     83 430.68");
        assert_eq!(lines[1], "100RUB-BYN*    3.5899");
        assert_eq!(lines[2], "ETH-USD   *       n/a");
    }
}
