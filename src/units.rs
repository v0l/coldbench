//! Amounts and dates as a person reads them.

use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};

pub const SATS_PER_BTC: u64 = 100_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    Btc,
    Sats,
}

impl Unit {
    pub const ALL: [Unit; 2] = [Unit::Btc, Unit::Sats];

    pub fn label(self) -> &'static str {
        match self {
            Unit::Btc => "BTC",
            Unit::Sats => "sats",
        }
    }

    /// `1.23 456 789` or `123,456,789`, with a sign for negative amounts.
    pub fn format(self, sats: i64) -> String {
        let sign = if sats < 0 { "-" } else { "" };
        let n = sats.unsigned_abs();
        match self {
            Unit::Sats => format!("{sign}{}", group(&n.to_string(), ',')),
            Unit::Btc => {
                let whole = n / SATS_PER_BTC;
                let frac = format!("{:08}", n % SATS_PER_BTC);
                format!("{sign}{whole}.{} {} {}", &frac[..2], &frac[2..5], &frac[5..])
            }
        }
    }

    /// The same without the unit's grouping, to put back in a field.
    pub fn plain(self, sats: u64) -> String {
        match self {
            Unit::Sats => sats.to_string(),
            Unit::Btc => {
                let s = format!("{}.{:08}", sats / SATS_PER_BTC, sats % SATS_PER_BTC);
                let t = s.trim_end_matches('0');
                t.trim_end_matches('.').to_string()
            }
        }
    }

    pub fn with_unit(self, sats: i64) -> String {
        format!("{} {}", self.format(sats), self.label())
    }

    /// Read what a person typed. `None` for nothing, nonsense, or more precision than a satoshi.
    pub fn parse(self, text: &str) -> Option<u64> {
        let t: String = text.chars().filter(|c| !c.is_whitespace() && *c != ',' && *c != '_').collect();
        if t.is_empty() {
            return None;
        }
        match self {
            Unit::Sats => t.parse().ok(),
            Unit::Btc => {
                let (whole, frac) = t.split_once('.').unwrap_or((&t, ""));
                if frac.len() > 8 || (whole.is_empty() && frac.is_empty()) {
                    return None;
                }
                let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
                if !digits(whole) || !digits(frac) {
                    return None;
                }
                let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
                let frac: u64 = format!("{frac:0<8}").parse().ok()?;
                whole.checked_mul(SATS_PER_BTC)?.checked_add(frac)
            }
        }
    }
}

fn group(digits: &str, sep: char) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// `2026-10-23 10:28` in the local zone.
pub fn when(unix: u64) -> String {
    match Local.timestamp_opt(unix as i64, 0).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "-".into(),
    }
}

pub fn ago(unix: u64, now: u64) -> String {
    let s = now.saturating_sub(unix);
    match s {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", (s + 30) / 60),
        5400..=129_599 => format!("{} h ago", (s + 1800) / 3600),
        _ => format!("{} days ago", (s + 43_200) / 86_400),
    }
}

pub fn now() -> u64 {
    DateTime::<Local>::from(std::time::SystemTime::now()).timestamp().max(0) as u64
}

/// `bc1qxy2k...x0wlh` for a table cell.
pub fn short(s: &str, keep: usize) -> String {
    if s.chars().count() <= keep * 2 + 3 {
        return s.to_string();
    }
    let head: String = s.chars().take(keep).collect();
    let tail: String = s.chars().rev().take(keep).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}...{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btc_is_grouped_like_sparrow() {
        assert_eq!(Unit::Btc.format(123_456_789), "1.23 456 789");
        assert_eq!(Unit::Btc.format(1_000), "0.00 001 000");
        assert_eq!(Unit::Btc.format(-50_000_000), "-0.50 000 000");
        assert_eq!(Unit::Btc.format(0), "0.00 000 000");
    }

    #[test]
    fn sats_are_grouped_by_thousands() {
        assert_eq!(Unit::Sats.format(1_234_567), "1,234,567");
        assert_eq!(Unit::Sats.format(999), "999");
        assert_eq!(Unit::Sats.format(-1_000), "-1,000");
    }

    #[test]
    fn parsing_takes_what_people_type() {
        assert_eq!(Unit::Btc.parse("0.001"), Some(100_000));
        assert_eq!(Unit::Btc.parse("1"), Some(100_000_000));
        assert_eq!(Unit::Btc.parse(".5"), Some(50_000_000));
        assert_eq!(Unit::Btc.parse("1.23 456 789"), Some(123_456_789));
        assert_eq!(Unit::Btc.parse("0.123456789"), None);
        assert_eq!(Unit::Btc.parse("abc"), None);
        assert_eq!(Unit::Btc.parse("."), None);
        assert_eq!(Unit::Btc.parse(""), None);
        assert_eq!(Unit::Sats.parse("12,345"), Some(12_345));
        assert_eq!(Unit::Sats.parse("1.5"), None);
        assert_eq!(Unit::Btc.parse("-1"), None);
    }

    #[test]
    fn plain_round_trips() {
        for n in [0u64, 1, 1_000, 123_456_789, 2_100_000_000_000_000] {
            for u in Unit::ALL {
                assert_eq!(u.parse(&u.plain(n)), Some(n), "{u:?} {n}");
                assert_eq!(u.parse(&u.format(n as i64)), Some(n), "{u:?} {n}");
            }
        }
        assert_eq!(Unit::Btc.plain(150_000_000), "1.5");
        assert_eq!(Unit::Btc.plain(100_000_000), "1");
    }

    #[test]
    fn long_addresses_are_shortened() {
        let a = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        assert_eq!(short(a, 6), "bc1qxy...hx0wlh");
        assert_eq!(short("abc", 6), "abc");
    }

    #[test]
    fn age_reads_naturally() {
        assert_eq!(ago(1000, 1010), "just now");
        assert_eq!(ago(0, 600), "10 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 86_400 * 3), "3 days ago");
    }
}
