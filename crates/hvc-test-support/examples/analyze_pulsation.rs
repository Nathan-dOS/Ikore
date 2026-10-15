//! Post-run analysis of a pulsation recorded in the execution database.
//!
//! Usage:
//!   analyze_pulsation [--db PATH] [--id ID | --latest | --latest-done]
//!                     [--json OUTPUT.json] [--html OUTPUT.html]
//!
//! With no selector it analyzes the most recent pulsation that reached `done`.
//! With no `--db` it resolves the same path the desktop app uses, honouring
//! `HVC_DATA_DIR`. `--html` writes the self-contained pulsation report of
//! ADR-024 from the same telemetry.

use std::path::PathBuf;

use hvc_core::app_core::AppPaths;
use hvc_core::reports::render_pulsation_report;
use hvc_test_support::pulsation_analysis::{Selector, analyze, render_text, to_html_report};

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) => {
            eprintln!("analyze_pulsation: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut database: Option<PathBuf> = None;
    let mut selector = Selector::LatestDone;
    let mut json_output: Option<PathBuf> = None;
    let mut html_output: Option<PathBuf> = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--db" => database = Some(arguments.next().ok_or("--db needs a path")?.into()),
            "--id" => selector = Selector::Id(arguments.next().ok_or("--id needs a value")?),
            "--latest" => selector = Selector::Latest,
            "--latest-done" => selector = Selector::LatestDone,
            "--json" => json_output = Some(arguments.next().ok_or("--json needs a path")?.into()),
            "--html" => html_output = Some(arguments.next().ok_or("--html needs a path")?.into()),
            "--help" | "-h" => {
                println!(
                    "usage: analyze_pulsation [--db PATH] [--id ID | --latest | --latest-done] [--json OUTPUT.json] [--html OUTPUT.html]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let database = match database {
        Some(path) => path,
        None => AppPaths::resolve()
            .map_err(|error| error.to_string())?
            .database_path(),
    };

    let report = analyze(&database, &selector)?;
    print!("{}", render_text(&report));
    if let Some(path) = json_output {
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("wrote {}", path.display());
    }
    if let Some(path) = html_output {
        std::fs::write(&path, render_pulsation_report(&to_html_report(&report)?))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
