//! `ufpm` — an unofficial command-line package manager for `FoundryVTT`.
//!
//! `ufpm` manages modules and systems inside a local `FoundryVTT` user-data
//! directory, working around the limitations of Foundry's built-in package
//! manager. See `PLAN.md` at the repository root for the overall design.

mod api;
mod cli;
mod commands;
mod constants;
mod foundry;
mod index;
mod install;
mod manifest;
mod resolve;
mod ui;

use clap::Parser;
use std::process::ExitCode;

/// Parses the command line, dispatches to the requested command and renders
/// any resulting error (with its cause chain) on stderr.
///
/// Runs on a single-threaded tokio runtime: the workload is network-bound,
/// and blocking work is pushed to the blocking pool via `spawn_blocking`.
///
/// # Panics
///
/// Panics when the tokio runtime cannot be initialized.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = cli::Args::parse();

    match commands::run(&args).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            for cause in error.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}
