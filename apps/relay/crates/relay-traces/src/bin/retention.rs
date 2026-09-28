//! `relay-traces-retention`: apply the OD-RY2 trace retention once and exit.
//!
//! Run from a systemd timer (see `deploy/`). Reads:
//! - `TENSORZERO_POSTGRES_URL` (required)
//! - `RELAY_TRACE_BODY_RETENTION_DAYS` (default 14)
//! - `RELAY_TRACE_METADATA_RETENTION_DAYS` (default 90)
//!
//! Flags: `--dry-run` (count only, delete nothing), `--schema <name>`
//! (default `tensorzero`; use a scratch copy to rehearse).

use std::process::ExitCode;

use relay_traces::retention::{self, BODY_DAYS_ENV, METADATA_DAYS_ENV, RetentionPolicy};
use relay_traces::store::DEFAULT_SCHEMA;
use sqlx::postgres::PgPoolOptions;

const DATABASE_URL_ENV: &str = "TENSORZERO_POSTGRES_URL";

struct Args {
    dry_run: bool,
    schema: String,
}

fn parse_args() -> Result<Args, String> {
    let mut dry_run = false;
    let mut schema = DEFAULT_SCHEMA.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--schema" => {
                schema = args
                    .next()
                    .ok_or_else(|| "`--schema` needs a value".to_string())?;
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args { dry_run, schema })
}

async fn run() -> Result<(), String> {
    let args = parse_args()?;
    let url = std::env::var(DATABASE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{DATABASE_URL_ENV} must be set"))?;
    let policy = RetentionPolicy::from_values(
        std::env::var(BODY_DAYS_ENV).ok().as_deref(),
        std::env::var(METADATA_DAYS_ENV).ok().as_deref(),
        args.dry_run,
    )
    .map_err(|error| error.to_string())?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|error| format!("could not connect to Postgres: {error}"))?;
    let report = retention::apply(&pool, &args.schema, &policy)
        .await
        .map_err(|error| error.to_string())?;
    let json = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    tracing::info!(
        target: "relay_traces_retention",
        dry_run = report.dry_run,
        total_rows = report.total_rows(),
        report = %json,
        "trace retention finished"
    );
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(target: "relay_traces_retention", %error, "trace retention failed");
            ExitCode::FAILURE
        }
    }
}
