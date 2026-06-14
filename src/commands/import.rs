//! `ufpm import`: install packages from a manifest file.

use crate::cli::GlobalArgs;
use crate::foundry::{PackageType, discovery, local};
use crate::manifest::ExportManifest;
use crate::ui::Reporter;
use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

/// Reads a manifest file and installs all listed packages.
///
/// Already-installed packages are filtered out before the confirmation
/// prompt so the user sees exactly what will change.
///
/// # Errors
///
/// Fails when the manifest cannot be read or parsed, or when the install
/// pipeline fails at the index/installation level.
pub async fn run(
    path: Option<&Path>,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("ufpm.toml"));

    let contents = std::fs::read_to_string(&path)?;
    let manifest: ExportManifest = toml::from_str(&contents)?;

    if let Some(world) = &manifest.world {
        reporter.status(&format!("importing packages for world '{world}'"));
    }

    let installation = discovery::resolve(global.data_path.as_deref())?;
    let installed_systems = installed_ids(&installation, PackageType::System, reporter)?;
    let installed_modules = installed_ids(&installation, PackageType::Module, reporter)?;

    let (systems_to_install, systems_skipped) = partition(&manifest.systems, &installed_systems);
    let (modules_to_install, modules_skipped) = partition(&manifest.modules, &installed_modules);

    let to_install = systems_to_install.len() + modules_to_install.len();
    let skipped = systems_skipped.len() + modules_skipped.len();

    reporter.status(&format!(
        "to install: {} system(s), {} module(s)",
        systems_to_install.len(),
        modules_to_install.len(),
    ));
    if skipped > 0 {
        reporter.status(&format!(
            "skipping {skipped} already installed package(s)"
        ));
    }

    if to_install == 0 {
        reporter.status("all packages already installed; nothing to do");
        return Ok(ExitCode::SUCCESS);
    }

    if !systems_to_install.is_empty() {
        reporter.status(&format!("systems: {}", systems_to_install.join(", ")));
    }
    if !modules_to_install.is_empty() {
        reporter.status(&format!("modules: {}", modules_to_install.join(", ")));
    }

    if !reporter.confirm(&format!("install {to_install} package(s)?"), global.yes)? {
        reporter.status("aborted; nothing was installed");
        return Ok(ExitCode::SUCCESS);
    }

    let system_code =
        super::package::install_packages(PackageType::System, &systems_to_install, global, reporter)
            .await?;
    let module_code =
        super::package::install_packages(PackageType::Module, &modules_to_install, global, reporter)
            .await?;

    if system_code == ExitCode::FAILURE || module_code == ExitCode::FAILURE {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Collects the ids of all installed packages of the given type into a set.
fn installed_ids(
    installation: &crate::foundry::Installation,
    kind: PackageType,
    reporter: &Reporter,
) -> anyhow::Result<HashSet<String>> {
    let scan = local::scan(installation, kind)?;
    for (path, reason) in &scan.skipped {
        reporter.warn(&format!("skipping {}: {reason}", path.display()));
    }
    Ok(scan.packages.into_iter().map(|p| p.id).collect())
}

/// Splits `slugs` into (to_install, already_installed), preserving order.
fn partition(slugs: &[String], installed: &HashSet<String>) -> (Vec<String>, Vec<String>) {
    slugs
        .iter()
        .cloned()
        .partition(|slug| !installed.contains(slug))
}
