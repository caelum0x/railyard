//! # switchyard-cli
//!
//! A thin client over the Switchyard HTTP API. `sy login` uses the OAuth
//! device flow, so no key is ever pasted; the refreshable token is stored in
//! `~/.config/switchyard/credentials.json` (mode 0600). CI can set
//! `SWITCHYARD_TOKEN` (an `sk-t0-` key) instead. No telemetry is sent anywhere.

mod client;
mod commands;

pub use client::{Client, Stored};
pub use commands::{run, Cli, Io};

use clap::Parser;

/// Process entry point shared by the `switchyard` and `sy` binaries.
pub async fn main_entry() -> i32 {
    let cli = Cli::parse();
    let mut io = Io::stdio();
    match run(cli, &mut io).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

/// Format micro-USD as dollars.
pub fn usd(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let m = micros.unsigned_abs();
    if m % 10_000 == 0 {
        format!("{sign}${}.{:02}", m / 1_000_000, (m % 1_000_000) / 10_000)
    } else {
        format!("{sign}${}.{:06}", m / 1_000_000, m % 1_000_000)
    }
}

/// Parse `$20`, `20`, `0.5` into micro-USD.
pub fn parse_usd(s: &str) -> anyhow::Result<i64> {
    let t = s.trim().trim_start_matches('$');
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    if whole.is_empty() && frac.is_empty() || frac.len() > 6 || !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("not a dollar amount: {s}");
    }
    let w: i64 = if whole.is_empty() { 0 } else { whole.parse()? };
    let f: i64 = format!("{frac:0<6}").parse()?;
    Ok(w * 1_000_000 + f)
}

/// Parse `7d`, `24h`, `30m` into a duration.
pub fn parse_since(s: &str) -> anyhow::Result<chrono::Duration> {
    let (n, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = n.parse().map_err(|_| anyhow::anyhow!("use forms like 7d, 24h, 30m"))?;
    Ok(match unit {
        "d" => chrono::Duration::days(n),
        "h" => chrono::Duration::hours(n),
        "m" => chrono::Duration::minutes(n),
        _ => anyhow::bail!("use forms like 7d, 24h, 30m"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money() {
        assert_eq!(usd(20_000_000), "$20.00");
        assert_eq!(usd(4_000), "$0.004000");
        assert_eq!(usd(-1_500_000), "-$1.50");
        assert_eq!(parse_usd("$20").unwrap(), 20_000_000);
        assert_eq!(parse_usd("0.5").unwrap(), 500_000);
        assert_eq!(parse_usd("2.000001").unwrap(), 2_000_001);
        for bad in ["", "abc", "1.1234567", "-3", "$"] {
            assert!(parse_usd(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn since() {
        assert_eq!(parse_since("7d").unwrap(), chrono::Duration::days(7));
        assert_eq!(parse_since("24h").unwrap(), chrono::Duration::hours(24));
        assert!(parse_since("7w").is_err());
        assert!(parse_since("x").is_err());
    }
}
