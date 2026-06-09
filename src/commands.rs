//! Implementations of the `ufpm` commands.
//!
//! This layer stays thin: it wires the parsed CLI arguments to the domain
//! modules (`foundry`, `api`, `index`, …) and owns user-facing orchestration.

mod cache;
mod doctor;

use crate::cli::{Args, Command};
use crate::ui::Reporter;

/// Dispatches the parsed command line to the matching command implementation.
///
/// # Errors
///
/// Propagates whatever error the executed command produces.
pub async fn run(args: &Args) -> anyhow::Result<()> {
    let reporter = Reporter::new(&args.global);

    match &args.command {
        Command::Doctor => doctor::run(&args.global, &reporter),
        Command::Cache { action } => cache::run(action, &args.global, &reporter).await,
        Command::Module { .. } => not_implemented("module"),
        Command::System { .. } => not_implemented("system"),
    }
}

/// Stub for commands scheduled in a later implementation phase (see `PLAN.md`).
///
/// # Errors
///
/// Always fails with a "not implemented yet" message; that is the point.
fn not_implemented(domain: &str) -> anyhow::Result<()> {
    anyhow::bail!("`ufpm {domain}` is not implemented yet")
}
