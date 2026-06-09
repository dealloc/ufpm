//! `ufpm cache`: manage the cached package index.

use crate::cli::{CacheAction, GlobalArgs};
use crate::foundry::{PackageType, discovery};
use crate::ui::{self, Reporter};
use crate::{api, index};

/// Dispatches a `ufpm cache` action.
///
/// # Errors
///
/// Propagates whatever error the executed action produces.
pub async fn run(
    action: &CacheAction,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    match action {
        CacheAction::Update => update(global, reporter).await,
        CacheAction::Info => info(),
        CacheAction::Clear => clear(reporter),
    }
}

/// Force-refreshes the indexes for both package types concurrently.
///
/// # Errors
///
/// Fails when the installation or license cannot be resolved, or when an
/// index can neither be fetched nor served from disk.
async fn update(global: &GlobalArgs, reporter: &Reporter) -> anyhow::Result<()> {
    let installation = discovery::resolve(global.data_path.as_deref())?;
    let license = installation.load_license()?;
    let client = api::Client::new(license)?;
    let cache = index::Cache::open()?;

    let spinner = reporter.spinner("fetching package indexes (one slow API call per type)");
    let (modules, systems) = tokio::join!(
        cache.ensure(PackageType::Module, &client, true),
        cache.ensure(PackageType::System, &client, true),
    );
    spinner.finish_and_clear();

    report_refresh(PackageType::Module, modules, reporter)?;
    report_refresh(PackageType::System, systems, reporter)?;
    Ok(())
}

/// Reports the outcome of one index refresh on stderr.
///
/// # Errors
///
/// Propagates the refresh failure itself.
fn report_refresh(
    kind: PackageType,
    result: Result<(index::Snapshot, index::Source), index::Error>,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    let (snapshot, source) = result?;
    if let index::Source::StaleFallback { error } = source {
        reporter.warn(&format!(
            "refreshing the {kind} index failed ({error}); keeping {} old data",
            ui::format_age(snapshot.age())
        ));
    } else {
        reporter.status(&format!(
            "{kind} index: {} packages, {} owned",
            snapshot.packages.len(),
            snapshot.owned.len()
        ));
    }
    Ok(())
}

/// Prints the cache location and per-type summaries to stdout.
///
/// # Errors
///
/// Fails when the platform cache directory cannot be determined.
fn info() -> anyhow::Result<()> {
    let cache = index::Cache::open()?;
    println!("location  {}", cache.dir().display());
    for kind in [PackageType::Module, PackageType::System] {
        match cache.info(kind) {
            Some(info) => println!(
                "{:<8}  {} packages, {} owned, fetched {} ago",
                kind.api_name(),
                info.packages,
                info.owned,
                ui::format_age(info.age)
            ),
            None => println!("{:<8}  not cached", kind.api_name()),
        }
    }
    Ok(())
}

/// Deletes all cached data.
///
/// # Errors
///
/// Fails when the cache directory cannot be determined or removed.
fn clear(reporter: &Reporter) -> anyhow::Result<()> {
    let cache = index::Cache::open()?;
    cache.clear()?;
    reporter.status("cache cleared");
    Ok(())
}
