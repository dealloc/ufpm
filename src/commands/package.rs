//! `ufpm module` / `ufpm system`: querying and managing packages.
//!
//! Both CLI domains share this implementation, parameterized by
//! [`PackageType`].

use crate::cli::{GlobalArgs, PackageAction};
use crate::foundry::version::Comparison;
use crate::foundry::{Installation, PackageType, discovery, local, version};
use crate::ui::{self, Reporter};
use crate::{api, constants, index, install};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

/// Dispatches a package action for one package type.
///
/// # Errors
///
/// Propagates whatever error the executed action produces.
pub async fn run(
    kind: PackageType,
    action: &PackageAction,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    match action {
        PackageAction::List {
            installed,
            owned,
            limit,
        } => {
            let options = ListOptions {
                query: None,
                installed_only: *installed,
                owned_only: *owned,
                limit: *limit,
            };
            list(kind, global, reporter, options).await
        }
        PackageAction::Search {
            query,
            installed,
            owned,
        } => {
            let options = ListOptions {
                query: Some(query.to_lowercase()),
                installed_only: *installed,
                owned_only: *owned,
                limit: None,
            };
            list(kind, global, reporter, options).await
        }
        PackageAction::Info { name } => info(kind, name, global, reporter).await,
        PackageAction::Outdated { check } => outdated(kind, *check, global, reporter).await,
        PackageAction::Install { names } => install_packages(kind, names, global, reporter).await,
        PackageAction::Update { names } => {
            not_implemented(&format!("{kind} update {}", names.join(" ")))
        }
        PackageAction::Remove { names } => {
            not_implemented(&format!("{kind} remove {}", names.join(" ")))
        }
        PackageAction::Unused { prune } => not_implemented(&format!(
            "{kind} unused{}",
            if *prune { " --prune" } else { "" }
        )),
    }
}

/// Stub for actions scheduled in a later implementation phase (see `PLAN.md`).
///
/// # Errors
///
/// Always fails with a "not implemented yet" message; that is the point.
fn not_implemented(action: &str) -> anyhow::Result<ExitCode> {
    anyhow::bail!("`ufpm {}` is not implemented yet", action.trim_end())
}

/// Filters applied by [`list`].
struct ListOptions {
    /// Lowercased search query; `None` lists everything.
    query: Option<String>,

    /// Only show packages that are installed locally.
    installed_only: bool,

    /// Only show protected packages this license has purchased.
    owned_only: bool,

    /// Print at most this many rows.
    limit: Option<usize>,
}

/// Resolves the installation and loads the package index, refreshing it when
/// stale and reporting where the data came from.
///
/// # Errors
///
/// Fails when the installation, license or index cannot be resolved.
async fn load_snapshot(
    kind: PackageType,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<(Installation, index::Snapshot, api::Client)> {
    let installation = discovery::resolve(global.data_path.as_deref())?;
    let license = installation.load_license()?;
    let client = api::Client::new(license)?;
    let cache = index::Cache::open()?;

    let spinner = reporter.spinner(&format!("loading {kind} index"));
    let result = cache.ensure(kind, &client, false).await;
    spinner.finish_and_clear();

    let (snapshot, source) = result?;
    match source {
        index::Source::Refreshed => reporter.detail(&format!(
            "fetched a fresh {kind} index ({} packages)",
            snapshot.packages.len()
        )),
        index::Source::Cached => reporter.detail(&format!(
            "using the cached {kind} index ({} old)",
            ui::format_age(snapshot.age())
        )),
        index::Source::StaleFallback { error } => reporter.warn(&format!(
            "refreshing the {kind} index failed ({error}); using {} old data",
            ui::format_age(snapshot.age())
        )),
    }
    Ok((installation, snapshot, client))
}

/// Scans the installed packages of `kind`, warning about directories that
/// could not be understood, and returns them keyed by id.
///
/// # Errors
///
/// Fails when the packages directory cannot be listed at all.
fn installed_map(
    installation: &Installation,
    kind: PackageType,
    reporter: &Reporter,
) -> anyhow::Result<HashMap<String, local::Installed>> {
    let scan = local::scan(installation, kind)?;
    for (path, reason) in &scan.skipped {
        reporter.warn(&format!("skipping {}: {reason}", path.display()));
    }
    Ok(scan
        .packages
        .into_iter()
        .map(|package| (package.id.clone(), package))
        .collect())
}

/// Lists or searches the index, with optional installed/owned filters.
///
/// # Errors
///
/// Fails when the index or the local installation cannot be read.
async fn list(
    kind: PackageType,
    global: &GlobalArgs,
    reporter: &Reporter,
    options: ListOptions,
) -> anyhow::Result<ExitCode> {
    let (installation, snapshot, _client) = load_snapshot(kind, global, reporter).await?;
    let installed = installed_map(&installation, kind, reporter)?;

    let mut rows: Vec<Vec<String>> = Vec::new();
    for package in &snapshot.packages {
        let local = installed.get(&package.name);
        if options.installed_only && local.is_none() {
            continue;
        }
        if options.owned_only && !(package.is_protected && snapshot.owned.contains(&package.id)) {
            continue;
        }
        if let Some(query) = &options.query
            && !matches_query(package, query)
        {
            continue;
        }
        rows.push(vec![
            package.name.clone(),
            local.map_or_else(
                || "-".to_owned(),
                |l| l.version.clone().unwrap_or_else(|| "?".to_owned()),
            ),
            package.version.version.clone(),
            flags(package, local, &snapshot),
            package.title.clone(),
        ]);
    }

    // Installed packages the index no longer lists still deserve a row: the
    // user has them on disk, and "delisted" is worth knowing.
    if options.installed_only && !options.owned_only {
        let known: std::collections::HashSet<&str> =
            snapshot.packages.iter().map(|p| p.name.as_str()).collect();
        for (id, local) in &installed {
            if known.contains(id.as_str()) {
                continue;
            }
            if let Some(query) = &options.query
                && !id.to_lowercase().contains(query)
            {
                continue;
            }
            rows.push(vec![
                id.clone(),
                local.version.clone().unwrap_or_else(|| "?".to_owned()),
                "-".to_owned(),
                "delisted".to_owned(),
                local.title.clone().unwrap_or_default(),
            ]);
        }
    }

    rows.sort_by(|a, b| a[0].cmp(&b[0]));
    let total = rows.len();
    if let Some(limit) = options.limit {
        rows.truncate(limit);
    }

    if rows.is_empty() {
        reporter.status(&format!("no matching {kind}s"));
        return Ok(ExitCode::SUCCESS);
    }

    ui::print_table(&["NAME", "INSTALLED", "LATEST", "FLAGS", "TITLE"], &rows);
    if total > rows.len() {
        reporter.status(&format!("… {} more hidden by --limit", total - rows.len()));
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether a package matches a lowercased search query (name, title or
/// author).
fn matches_query(package: &api::types::Package, query: &str) -> bool {
    package.name.to_lowercase().contains(query)
        || package.title.to_lowercase().contains(query)
        || package
            .author
            .as_deref()
            .is_some_and(|author| author.to_lowercase().contains(query))
}

/// Builds the FLAGS cell for a package row.
fn flags(
    package: &api::types::Package,
    local: Option<&local::Installed>,
    snapshot: &index::Snapshot,
) -> String {
    let mut flags: Vec<&str> = Vec::new();
    if package.is_protected {
        flags.push(if snapshot.owned.contains(&package.id) {
            "owned"
        } else {
            "protected"
        });
    }
    match local.map(|local| version_status(local, package)) {
        Some(Comparison::Newer) => flags.push("update"),
        Some(Comparison::Changed) => flags.push("changed"),
        _ => {}
    }
    if flags.is_empty() {
        "-".to_owned()
    } else {
        flags.join(",")
    }
}

/// Compares an installed package against its index entry; an unknown local
/// version counts as [`Comparison::Changed`], never as up to date.
fn version_status(local: &local::Installed, package: &api::types::Package) -> Comparison {
    local.version.as_deref().map_or(Comparison::Changed, |v| {
        version::against_installed(v, &package.version.version)
    })
}

/// Prints the detail block for a single package.
///
/// # Errors
///
/// Fails when the index cannot be loaded or no such package exists.
async fn info(
    kind: PackageType,
    name: &str,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    let (installation, snapshot, _client) = load_snapshot(kind, global, reporter).await?;
    let Some(package) = snapshot.packages.iter().find(|p| p.name == name) else {
        anyhow::bail!("the index lists no {kind} named `{name}`");
    };
    let installed = installed_map(&installation, kind, reporter)?;
    let local = installed.get(name);

    let print_field = |label: &str, value: Option<String>| {
        if let Some(value) = value
            && !value.is_empty()
        {
            println!("{label:<14} {value}");
        }
    };

    print_field("name", Some(package.name.clone()));
    print_field("title", Some(package.title.clone()));
    print_field("author", package.author.clone());
    print_field("type", Some(kind.to_string()));
    print_field("latest", Some(package.version.version.clone()));
    print_field(
        "installed",
        local.map(|local| {
            let version = local.version.clone().unwrap_or_else(|| "?".to_owned());
            let status = match version_status(local, package) {
                Comparison::Same => "up to date",
                Comparison::Newer => "update available",
                Comparison::Changed => "differs from the index",
            };
            format!("{version} ({status})")
        }),
    );
    print_field(
        "access",
        Some(if package.is_protected {
            if snapshot.owned.contains(&package.id) {
                "protected (owned)".to_owned()
            } else {
                "protected (purchase required)".to_owned()
            }
        } else {
            "free".to_owned()
        }),
    );
    if !package.systems.is_empty() {
        print_field("systems", Some(package.systems.join(", ")));
    }
    print_field(
        "requires core",
        package.version.required_core_version.clone(),
    );
    print_field(
        "compatible",
        package.version.compatible_core_version.clone(),
    );
    print_field("verified", package.verified.clone());
    print_field("updated", package.last_updated.clone());
    print_field("url", package.url.clone());
    print_field("manifest", Some(package.version.manifest.clone()));
    print_field("notes", package.version.notes.clone());
    print_field("description", package.description.clone());

    Ok(ExitCode::SUCCESS)
}

/// Installs the named packages: resolve against the index, fetch manifests
/// concurrently, download archives in parallel, then swap each into place
/// transactionally. Per-package failures never abort the rest; everything
/// lands in the end-of-run summary.
///
/// # Errors
///
/// Fails when the index or installation cannot be loaded at all; individual
/// package failures are reported in the summary and through the exit code.
async fn install_packages(
    kind: PackageType,
    names: &[String],
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    let (installation, snapshot, client) = load_snapshot(kind, global, reporter).await?;
    report_recovery(&install::recover(&installation), reporter);
    let installed = installed_map(&installation, kind, reporter)?;

    let mut summary = Summary::default();
    let requests = select_install_requests(names, &snapshot, &installed, reporter, &mut summary);
    let jobs = resolve_jobs(&client, kind, &requests, reporter, &mut summary).await;
    execute_jobs(&installation, &client, jobs, reporter, &mut summary).await?;

    Ok(summary.finish(reporter))
}

/// Warns about whatever the crash-recovery sweep found and did.
fn report_recovery(recovery: &install::Recovery, reporter: &Reporter) {
    for name in &recovery.restored {
        reporter.warn(&format!(
            "restored `{name}`, left behind by an interrupted operation"
        ));
    }
    for (name, error) in &recovery.failed {
        reporter.warn(&format!(
            "could not recover the backup of `{name}`: {error}"
        ));
    }
}

/// Validates the requested names against the index and installed state,
/// recording immediate successes/failures in the summary and returning the
/// packages that actually need installing.
fn select_install_requests<'a>(
    names: &[String],
    snapshot: &'a index::Snapshot,
    installed: &HashMap<String, local::Installed>,
    reporter: &Reporter,
    summary: &mut Summary,
) -> Vec<&'a api::types::Package> {
    let mut requests = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let Some(package) = snapshot.packages.iter().find(|p| p.name == *name) else {
            summary.fail(name, "-", "not found in the index");
            continue;
        };
        if package.is_protected {
            let reason = if snapshot.owned.contains(&package.id) {
                "protected packages are not supported yet"
            } else {
                "protected; purchase required"
            };
            summary.fail(name, "-", reason);
            continue;
        }
        if let Some(local) = installed.get(name)
            && version_status(local, package) == Comparison::Same
        {
            summary.ok(name, &package.version.version, "already up to date");
            continue;
        }
        warn_core_compatibility(package, reporter);
        requests.push(package);
    }
    requests
}

/// Fetches the manifests of the selected packages concurrently and turns
/// them into download jobs; failures land in the summary.
async fn resolve_jobs(
    client: &api::Client,
    kind: PackageType,
    requests: &[&api::types::Package],
    reporter: &Reporter,
    summary: &mut Summary,
) -> Vec<install::Job> {
    if requests.is_empty() {
        return Vec::new();
    }
    let spinner = reporter.spinner("resolving package manifests");
    let resolved: Vec<(String, Result<install::Job, String>)> =
        futures_util::stream::iter(requests.iter().map(|package| async move {
            let job = resolve_job(client, kind, package).await;
            (package.name.clone(), job)
        }))
        .buffer_unordered(constants::DOWNLOAD_CONCURRENCY)
        .collect()
        .await;
    spinner.finish_and_clear();

    let mut jobs = Vec::new();
    for (name, result) in resolved {
        match result {
            Ok(job) => jobs.push(job),
            Err(reason) => summary.fail(&name, "-", &reason),
        }
    }
    jobs
}

/// Downloads all job archives in parallel (with progress bars), then swaps
/// each package into place sequentially; outcomes land in the summary.
///
/// # Errors
///
/// Fails only when the downloads directory cannot be determined.
async fn execute_jobs(
    installation: &Installation,
    client: &api::Client,
    jobs: Vec<install::Job>,
    reporter: &Reporter,
    summary: &mut Summary,
) -> anyhow::Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    let downloads_dir = index::Cache::open()?.downloads_dir();
    let progress = reporter.downloads();
    let downloaded: Vec<(install::Job, Result<std::path::PathBuf, install::Error>)> =
        futures_util::stream::iter(jobs.into_iter().map(|job| {
            let bar = progress.bar(&job.name);
            let http = client.http();
            let downloads_dir = downloads_dir.clone();
            async move {
                let result = install::download_archive(http, &downloads_dir, &job, &bar).await;
                bar.finish_and_clear();
                (job, result)
            }
        }))
        .buffer_unordered(constants::DOWNLOAD_CONCURRENCY)
        .collect()
        .await;

    for (job, result) in downloaded {
        let outcome = match result {
            Ok(archive) => install::apply(installation, &job, archive).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => {
                reporter.detail(&format!("installed {} {}", job.name, job.version));
                summary.ok(&job.name, &job.version, "installed");
            }
            Err(error) => summary.fail(&job.name, &job.version, &error_chain(&error)),
        }
    }
    Ok(())
}

/// Accumulates the end-of-run summary of a mutating command.
#[derive(Default)]
struct Summary {
    /// Table rows: marker, name, version, note.
    rows: Vec<Vec<String>>,

    /// Whether any operation failed.
    failed: bool,
}

impl Summary {
    /// Records a successful operation.
    fn ok(&mut self, name: &str, version: &str, note: &str) {
        self.rows.push(Self::row("✓", name, version, note));
    }

    /// Records a failed operation.
    fn fail(&mut self, name: &str, version: &str, note: &str) {
        self.rows.push(Self::row("✗", name, version, note));
        self.failed = true;
    }

    /// Builds one summary-table row.
    fn row(marker: &str, name: &str, version: &str, note: &str) -> Vec<String> {
        vec![
            marker.to_owned(),
            name.to_owned(),
            version.to_owned(),
            note.to_owned(),
        ]
    }

    /// Prints the summary table and converts the result to an exit code.
    fn finish(mut self, reporter: &Reporter) -> ExitCode {
        self.rows.sort_by(|a, b| a[1].cmp(&b[1]));
        reporter.summary(&["", "NAME", "VERSION", "RESULT"], &self.rows);
        if self.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// Renders an error with its full cause chain on one line.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Warns when a package requires a newer `FoundryVTT` core than the version
/// `ufpm` assumes.
fn warn_core_compatibility(package: &api::types::Package, reporter: &Reporter) {
    if let Some(required) = &package.version.required_core_version
        && !required.is_empty()
        && version::compare(&constants::foundry_version(), required) == std::cmp::Ordering::Less
    {
        reporter.warn(&format!(
            "{} requires FoundryVTT core {required}, newer than the assumed {} — it may not work",
            package.name,
            constants::foundry_version()
        ));
    }
}

/// Resolves a package's manifest into an installable [`install::Job`].
///
/// # Errors
///
/// Returns a human-readable reason when the manifest cannot be fetched,
/// declares no download URL, or belongs to a different package.
async fn resolve_job(
    client: &api::Client,
    kind: PackageType,
    package: &api::types::Package,
) -> Result<install::Job, String> {
    let manifest = client
        .fetch_manifest(&package.version.manifest)
        .await
        .map_err(|error| format!("manifest fetch failed: {error}"))?;
    let Some(download) = manifest.download.clone() else {
        return Err("the manifest declares no download URL".to_owned());
    };
    if let Some(id) = manifest.id()
        && id != package.name
    {
        return Err(format!(
            "the manifest belongs to `{id}`, not `{}`",
            package.name
        ));
    }
    Ok(install::Job {
        kind,
        name: package.name.clone(),
        version: manifest
            .version()
            .unwrap_or_else(|| package.version.version.clone()),
        download_url: download,
    })
}

/// Lists installed packages whose index version differs from the installed
/// one. With `check`, exits non-zero when anything is outdated.
///
/// # Errors
///
/// Fails when the index or the local installation cannot be read.
async fn outdated(
    kind: PackageType,
    check: bool,
    global: &GlobalArgs,
    reporter: &Reporter,
) -> anyhow::Result<ExitCode> {
    let (installation, snapshot, _client) = load_snapshot(kind, global, reporter).await?;
    let scan = local::scan(&installation, kind)?;
    for (path, reason) in &scan.skipped {
        reporter.warn(&format!("skipping {}: {reason}", path.display()));
    }
    let by_name: HashMap<&str, &api::types::Package> = snapshot
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut delisted = 0usize;
    for local in &scan.packages {
        let Some(package) = by_name.get(local.id.as_str()) else {
            delisted += 1;
            continue;
        };
        let status = match version_status(local, package) {
            Comparison::Same => continue,
            Comparison::Newer => "update",
            Comparison::Changed => "changed",
        };
        rows.push(vec![
            local.id.clone(),
            local.version.clone().unwrap_or_else(|| "?".to_owned()),
            package.version.version.clone(),
            status.to_owned(),
            package.title.clone(),
        ]);
    }

    if delisted > 0 {
        reporter.detail(&format!(
            "{delisted} installed {kind}(s) are not listed in the index"
        ));
    }

    if rows.is_empty() {
        reporter.status(&format!("all installed {kind}s match the index"));
        return Ok(ExitCode::SUCCESS);
    }

    ui::print_table(
        &["NAME", "INSTALLED", "AVAILABLE", "STATUS", "TITLE"],
        &rows,
    );
    if check {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
