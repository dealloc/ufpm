//! Implementations of the `ufpm` commands.
//!
//! This layer stays thin: it wires the parsed CLI arguments to the domain
//! modules (`foundry`, `api`, `index`, …) and owns user-facing orchestration.

mod cache;
mod doctor;
mod export;
mod import;
mod package;

use crate::cli::{Args, Command};
use crate::foundry::PackageType;
use crate::ui::Reporter;
use std::process::ExitCode;

/// Dispatches the parsed command line to the matching command implementation
/// and returns the process exit code.
///
/// # Errors
///
/// Propagates whatever error the executed command produces.
pub async fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let reporter = Reporter::new(&args.global);

    match &args.command {
        Command::Doctor => {
            doctor::run(&args.global, &reporter)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cache { action } => {
            cache::run(action, &args.global, &reporter).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Module { action } => {
            package::run(PackageType::Module, action, &args.global, &reporter).await
        }
        Command::System { action } => {
            package::run(PackageType::System, action, &args.global, &reporter).await
        }
        Command::Export { world, output } => {
            export::run(world.as_deref(), output.as_deref(), &args.global, &reporter).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Import { path } => {
            import::run(path.as_deref(), &args.global, &reporter).await
        }
        Command::Completions { shell } => {
            let mut command = <Args as clap::CommandFactory>::command();
            clap_complete::generate(*shell, &mut command, "ufpm", &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
    }
}
