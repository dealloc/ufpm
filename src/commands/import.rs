//! `ufpm import`: install packages from a manifest file.

use crate::cli::GlobalArgs;
use crate::foundry::PackageType;
use crate::manifest::ExportManifest;
use crate::ui::Reporter;
use std::path::Path;
use std::process::ExitCode;

/// Reads a manifest file and installs all listed packages.
///
/// Prints a summary of what will be installed and asks for confirmation
/// (unless `--yes` was given). Then delegates to the shared install
/// pipeline for systems and modules in turn.
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
    reporter.status(&format!(
        "manifest: {} system(s), {} module(s)",
        manifest.systems.len(),
        manifest.modules.len()
    ));

    if !reporter.confirm("install listed packages?", global.yes)? {
        reporter.status("aborted; nothing was installed");
        return Ok(ExitCode::SUCCESS);
    }

    let system_code =
        super::package::install_packages(PackageType::System, &manifest.systems, global, reporter)
            .await?;
    let module_code =
        super::package::install_packages(PackageType::Module, &manifest.modules, global, reporter)
            .await?;

    if system_code == ExitCode::FAILURE || module_code == ExitCode::FAILURE {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
