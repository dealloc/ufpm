//! Command-line interface definitions.
//!
//! The CLI follows the `ufpm <domain> <action> [args]` shape (for example
//! `ufpm cache update` or `ufpm module install <name>`). The full surface is
//! declared up front; actions are implemented phase by phase (see `PLAN.md`).
//!
//! Output conventions: command *data* is printed on stdout, all status output
//! (progress, warnings, prompts) goes to stderr so command output pipes
//! cleanly.

use clap::{ArgAction, Parser, Subcommand};
use std::path::PathBuf;

/// An unofficial package manager for `FoundryVTT`.
#[derive(Debug, Parser)]
#[command(name = "ufpm", version, propagate_version = true)]
pub struct Args {
    /// Flags shared by every subcommand.
    #[command(flatten)]
    pub global: GlobalArgs,

    /// The command to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted by every `ufpm` subcommand.
#[derive(Debug, clap::Args)]
pub struct GlobalArgs {
    /// Path to the `FoundryVTT` root (the directory containing `Config/` and `Data/`).
    #[arg(long, env = "UFPM_DATA_PATH", global = true, value_name = "PATH")]
    pub data_path: Option<PathBuf>,

    /// Assume "yes" for every confirmation prompt.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Disable progress bars and spinners; print plain status lines instead.
    #[arg(long, global = true)]
    pub no_progress: bool,

    /// Increase verbosity (-v: steps, -vv: decisions, -vvv: HTTP traces).
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Suppress all status output; only errors are printed.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,
}

/// Top-level `ufpm` commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the cached package index.
    Cache {
        /// The cache operation to perform.
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Manage `FoundryVTT` modules.
    Module {
        /// The module operation to perform.
        #[command(subcommand)]
        action: PackageAction,
    },
    /// Manage `FoundryVTT` systems.
    System {
        /// The system operation to perform.
        #[command(subcommand)]
        action: PackageAction,
    },
    /// Diagnose the local setup: resolved paths, license and cache state.
    Doctor,

    /// Generate a shell completion script on stdout.
    #[command(hide = true)]
    Completions {
        /// The shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Operations on the cached package index.
#[derive(Debug, Subcommand)]
pub enum CacheAction {
    /// Force-refresh the package indexes now.
    Update,
    /// Show the cache location, age and package counts.
    Info,
    /// Delete the cached indexes and any partial downloads.
    Clear,
}

/// Operations shared by `ufpm module` and `ufpm system`.
#[derive(Debug, Subcommand)]
pub enum PackageAction {
    /// List packages from the index.
    List {
        /// Only show installed packages.
        #[arg(long)]
        installed: bool,
        /// Only show protected packages owned by this license.
        #[arg(long)]
        owned: bool,
        /// Print at most this many rows.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Search packages by name, title or author.
    Search {
        /// Case-insensitive substring to search for.
        query: String,
        /// Only show installed packages.
        #[arg(long)]
        installed: bool,
        /// Only show protected packages owned by this license.
        #[arg(long)]
        owned: bool,
    },
    /// Show details for a single package.
    Info {
        /// The package slug (for example `dice-so-nice`).
        name: String,
    },
    /// Add one or more packages.
    Add {
        /// The package slugs to add.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// List installed packages that have updates available.
    Outdated {
        /// Exit with a non-zero status when updates are available.
        #[arg(long)]
        check: bool,
    },
    /// Update installed packages to their latest versions.
    Update {
        /// The package slugs to update; updates everything outdated when omitted.
        names: Vec<String>,
    },
    /// Remove one or more installed packages.
    Remove {
        /// The package slugs to remove.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// List installed packages that no world uses.
    Unused {
        /// Delete the unused packages after confirmation.
        #[arg(long)]
        prune: bool,
    },
}

#[cfg(test)]
mod tests {
    //! Sanity checks for the clap definitions.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use clap::CommandFactory;

    /// The clap command definition is internally consistent.
    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    /// Repeated `-v` flags raise the verbosity level.
    #[test]
    fn parses_stacked_verbosity_flags() {
        let args = Args::try_parse_from(["ufpm", "-vvv", "doctor"]).unwrap();
        assert_eq!(args.global.verbose, 3);
    }

    /// `--quiet` and `-v` are mutually exclusive.
    #[test]
    fn quiet_conflicts_with_verbose() {
        assert!(Args::try_parse_from(["ufpm", "-v", "--quiet", "doctor"]).is_err());
    }
}
