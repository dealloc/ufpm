//! `ufpm module` / `ufpm system`: querying and managing packages.
//!
//! Both CLI domains share this implementation, parameterized by
//! [`PackageType`].

use crate::cli::{GlobalArgs, PackageAction};
use crate::foundry::version::Comparison;
use crate::foundry::{Installation, PackageType, discovery, local, version};
use crate::ui::{self, Reporter};
use crate::{api, index};
use std::collections::HashMap;
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
        PackageAction::Install { names } => {
            not_implemented(&format!("{kind} install {}", names.join(" ")))
        }
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
) -> anyhow::Result<(Installation, index::Snapshot)> {
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
    Ok((installation, snapshot))
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
    let (installation, snapshot) = load_snapshot(kind, global, reporter).await?;
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
    let (installation, snapshot) = load_snapshot(kind, global, reporter).await?;
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
    let (installation, snapshot) = load_snapshot(kind, global, reporter).await?;
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
