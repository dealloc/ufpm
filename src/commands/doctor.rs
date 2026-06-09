//! `ufpm doctor`: prints a diagnostic report about the local setup.

use crate::cli::GlobalArgs;
use crate::foundry::{Installation, PackageType, RootSource, discovery};
use crate::ui::{self, Reporter};
use crate::{constants, index};
use std::path::Path;

/// Resolves the installation and prints the diagnostic report to stdout.
///
/// # Errors
///
/// Fails when no `FoundryVTT` installation can be located.
pub fn run(global: &GlobalArgs, reporter: &Reporter) -> anyhow::Result<()> {
    let installation = discovery::resolve(global.data_path.as_deref())?;
    print_report(&installation, reporter);
    Ok(())
}

/// Prints the report for a resolved installation.
fn print_report(installation: &Installation, reporter: &Reporter) {
    let source = match installation.source() {
        RootSource::Explicit => "explicit",
        RootSource::Discovered => "discovered",
    };

    let license = if installation.license_path().is_file() {
        "found"
    } else {
        reporter.warn("no license.json found; the FoundryVTT package API will not be reachable");
        "MISSING"
    };

    println!(
        "foundry root     {} ({source})",
        installation.root().display()
    );
    println!("license.json     {license}");
    println!(
        "modules          {}",
        count_subdirectories(&installation.modules_dir(), reporter)
    );
    println!(
        "systems          {}",
        count_subdirectories(&installation.systems_dir(), reporter)
    );
    println!(
        "worlds           {}",
        count_subdirectories(&installation.worlds_dir(), reporter)
    );
    println!(
        "foundry version  {} (override with {})",
        constants::foundry_version(),
        constants::FOUNDRY_VERSION_ENV
    );
    print_cache_report();
}

/// Prints one line per package type describing the cached index state.
fn print_cache_report() {
    let Ok(cache) = index::Cache::open() else {
        println!("cache            no platform cache directory");
        return;
    };

    for kind in [PackageType::Module, PackageType::System] {
        let label = format!("cache ({})", kind.api_name());
        match cache.info(kind) {
            Some(info) => println!(
                "{label:<16} {} packages, fetched {} ago",
                info.packages,
                ui::format_age(info.age)
            ),
            None => println!("{label:<16} not cached"),
        }
    }
}

/// Counts the direct subdirectories of `dir`, treating a missing directory
/// as empty (a fresh installation may not have created it yet).
fn count_subdirectories(dir: &Path, reporter: &Reporter) -> usize {
    reporter.detail(&format!("scanning {}", dir.display()));
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count(),
        Err(error) => {
            if dir.exists() {
                reporter.warn(&format!("could not read {}: {error}", dir.display()));
            }
            0
        }
    }
}
