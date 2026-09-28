//! `relay-price-crosscheck`: compare Relay's catalog with LiteLLM's MIT price
//! list and print what differs by more than 5%. Read-only (RY-RL2): it never
//! writes the catalog and never calls the gateway's price editor.
//!
//! ```text
//! relay-price-crosscheck [--catalog PATH] [--routes PATH] [--litellm PATH|URL]
//!                        [--tolerance-bps N] [--json] [--strict]
//! ```
//! Defaults: `apps/relay/config/catalog.json`, the provider canary's
//! `routes.json`, and LiteLLM's file on GitHub. `--strict` exits 3 when a row is
//! flagged (for a scheduled check); otherwise the exit code is 0 on any report.

#![expect(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process::ExitCode;

use relay_catalog::litellm::{
    DEFAULT_TOLERANCE_BPS, LITELLM_PRICES_URL, cross_check, fetch_litellm_prices,
    parse_litellm_prices, parse_route_legs, render_text,
};
use relay_catalog::{CatalogSnapshot, bundled_snapshot};

struct Args {
    catalog: PathBuf,
    routes: PathBuf,
    litellm: String,
    tolerance_bps: u32,
    json: bool,
    strict: bool,
}

fn parse_args() -> Result<Args, String> {
    let relay = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut args = Args {
        catalog: relay.join("config/catalog.json"),
        routes: relay.join("tools/provider-canary/routes.json"),
        litellm: LITELLM_PRICES_URL.to_string(),
        tolerance_bps: DEFAULT_TOLERANCE_BPS,
        json: false,
        strict: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--catalog" => args.catalog = PathBuf::from(value()?),
            "--routes" => args.routes = PathBuf::from(value()?),
            "--litellm" => args.litellm = value()?,
            "--tolerance-bps" => {
                args.tolerance_bps = value()?.parse().map_err(|_| "bad --tolerance-bps")?;
            }
            "--json" => args.json = true,
            "--strict" => args.strict = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

async fn run(args: &Args) -> Result<bool, String> {
    let snapshot: CatalogSnapshot = if args.catalog.as_os_str() == "builtin" {
        bundled_snapshot().map_err(|e| e.to_string())?
    } else {
        let text = std::fs::read_to_string(&args.catalog)
            .map_err(|e| format!("read {}: {e}", args.catalog.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parse catalog: {e}"))?
    };
    let routes = match std::fs::read_to_string(&args.routes) {
        Ok(text) => parse_route_legs(&text).map_err(|e| e.to_string())?,
        Err(_) => Default::default(),
    };
    let litellm_json = if args.litellm.starts_with("https://")
        || args.litellm.starts_with("http://")
    {
        fetch_litellm_prices(&args.litellm)
            .await
            .map_err(|e| e.to_string())?
    } else {
        std::fs::read_to_string(&args.litellm).map_err(|e| format!("read {}: {e}", args.litellm))?
    };
    let prices = parse_litellm_prices(&litellm_json).map_err(|e| e.to_string())?;
    let report = cross_check(&snapshot, &prices, &routes, args.tolerance_bps);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render_text(&report));
    }
    Ok(report.mismatches.is_empty())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            eprintln!(
                "usage: relay-price-crosscheck [--catalog PATH|builtin] [--routes PATH] \
                 [--litellm PATH|URL] [--tolerance-bps N] [--json] [--strict]"
            );
            return ExitCode::from(2);
        }
    };
    match run(&args).await {
        Ok(clean) if !clean && args.strict => ExitCode::from(3),
        Ok(_) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("relay-price-crosscheck: {message}");
            ExitCode::from(1)
        }
    }
}
