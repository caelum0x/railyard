//! `content-lint` — batch closed-vocabulary linter for Aeonrift champion content.
//!
//! Loads + validates every `content/champions/*.ron` champion spec through the
//! ability-DSL gate and prints a per-file report. Exits non-zero if any spec
//! fails to parse (unknown effect/targeting/field) or validate, so it drops
//! straight into CI.
//!
//! Usage:
//!   cargo run -p aeonrift-content-lint            # lint the shipped roster
//!   cargo run -p aeonrift-content-lint -- <dir>   # lint an arbitrary content dir

use std::path::PathBuf;
use std::process::ExitCode;

use aeonrift_content_lint::{default_champions_dir, lint_champions_dir};

fn main() -> ExitCode {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_champions_dir);

    let report = match lint_champions_dir(&dir) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("content-lint: cannot read {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };

    for outcome in &report.outcomes {
        match &outcome.error {
            None => println!("  ok    {}", outcome.slug),
            Some(reason) => println!("  FAIL  {}: {reason}", outcome.slug),
        }
    }

    println!(
        "\ncontent-lint: {} passed, {} failed ({} total) in {}",
        report.passed(),
        report.failed(),
        report.total(),
        dir.display()
    );

    if report.all_passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
