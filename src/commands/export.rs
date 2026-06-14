//! `ufpm export`: snapshot the installed packages to a manifest file.

use crate::cli::GlobalArgs;
use crate::foundry::{Installation, PackageType, discovery, local, worlds};
use crate::manifest::ExportManifest;
use crate::ui::Reporter;
use std::path::Path;
use std::process::ExitCode;

/// Exports installed packages to a TOML manifest file.
///
/// When `world` is given only the packages enabled for that world are
/// included; otherwise every installed package is listed.
///
/// # Errors
///
/// Fails when the installation cannot be resolved, the world cannot be
/// scanned, the packages directory cannot be listed, or the file cannot
/// be written.
pub async fn run(
    world: Option<&str>,
    output: Option<&Path>,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    let installation = discovery::resolve(global.data_path.as_deref())?;
    let output = output.map_or_else(|| std::path::PathBuf::from("ufpm.toml"), Path::to_path_buf);

    let manifest = if let Some(world_id) = world {
        world_manifest(&installation, world_id).await?
    } else {
        all_manifest(&installation)?
    };

    let toml = toml::to_string_pretty(&manifest)?;
    std::fs::write(&output, &toml)?;

    reporter.status(&format!(
        "wrote {} system(s) and {} module(s) to {}",
        manifest.systems.len(),
        manifest.modules.len(),
        output.display()
    ));
    Ok(ExitCode::SUCCESS)
}

/// Builds a manifest from a single world's usage.
///
/// # Errors
///
/// Returns [`worlds::Error::WorldNotFound`] if no `worlds/<id>/world.json`
/// exists. Database problems (corrupt files, permission errors) are recorded
/// in [`worlds::Usage::unreadable`] rather than returned as errors.
async fn world_manifest(
    installation: &Installation,
    world_id: &str,
) -> anyhow::Result<ExportManifest> {
    let scanned = installation.clone();
    let id = world_id.to_owned();
    let usage = tokio::task::spawn_blocking(move || worlds::scan_world(&scanned, &id))
        .await
        .map_err(|join| anyhow::anyhow!("internal failure: {join}"))??;

    let mut systems: Vec<String> = usage.systems.into_iter().collect();
    let mut modules: Vec<String> = usage.modules.into_iter().collect();
    systems.sort();
    modules.sort();

    Ok(ExportManifest {
        world: Some(world_id.to_owned()),
        systems,
        modules,
    })
}

/// Builds a manifest from all installed packages.
///
/// # Errors
///
/// Returns an error if the systems or modules packages directory exists but
/// cannot be listed (e.g. a permissions error). A missing directory is not
/// an error; it yields an empty list.
fn all_manifest(installation: &Installation) -> anyhow::Result<ExportManifest> {
    let system_scan = local::scan(installation, PackageType::System)?;
    let module_scan = local::scan(installation, PackageType::Module)?;

    let mut systems: Vec<String> = system_scan.packages.into_iter().map(|p| p.id).collect();
    let mut modules: Vec<String> = module_scan.packages.into_iter().map(|p| p.id).collect();
    systems.sort();
    modules.sort();

    Ok(ExportManifest {
        world: None,
        systems,
        modules,
    })
}
