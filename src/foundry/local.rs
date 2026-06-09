//! Scanning the packages installed in a local `FoundryVTT` data directory.
//!
//! The on-disk state is the only source of truth: `FoundryVTT` itself can
//! install, update or remove packages at any time, so `ufpm` rescans
//! `Data/modules/` and `Data/systems/` on every invocation and keeps no
//! bookkeeping of its own.

use super::{Installation, PackageType};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// An installed package found on disk.
#[derive(Clone, Debug)]
pub struct Installed {
    /// The package slug (directory name, validated against the manifest).
    pub id: String,

    /// Human-readable title from the manifest.
    pub title: Option<String>,

    /// Installed version from the manifest, when declared.
    pub version: Option<String>,
}

/// The result of scanning one package directory tree.
#[derive(Debug, Default)]
pub struct Scan {
    /// All packages with a readable manifest, sorted by id.
    pub packages: Vec<Installed>,

    /// Directories that exist but could not be understood, with the reason.
    /// These still *occupy* their slug, so callers must surface them rather
    /// than treat them as absent.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Errors that can occur while scanning for installed packages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The packages directory exists but could not be listed.
    #[error("could not list {}", path.display())]
    Unlistable {
        /// The directory that could not be listed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Scans the installation for installed packages of the given type.
///
/// A missing packages directory yields an empty scan (a fresh installation
/// may not have created it yet). Directories without a parseable manifest
/// are reported in [`Scan::skipped`] instead of failing the whole scan.
///
/// # Errors
///
/// Returns an [`Error`] only when the packages directory exists but cannot
/// be listed at all.
pub fn scan(installation: &Installation, kind: PackageType) -> Result<Scan, Error> {
    let dir = installation.packages_dir(kind);
    if !dir.exists() {
        return Ok(Scan::default());
    }

    let entries = std::fs::read_dir(&dir).map_err(|source| Error::Unlistable {
        path: dir.clone(),
        source,
    })?;

    let mut scan = Scan::default();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match read_installed(&path, kind) {
            Ok(installed) => scan.packages.push(installed),
            Err(reason) => scan.skipped.push((path, reason)),
        }
    }

    scan.packages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(scan)
}

/// Reads one installed (or staged) package directory into an [`Installed`]
/// entry.
///
/// # Errors
///
/// Returns a human-readable reason when the manifest is missing, unreadable
/// or lacks an id.
pub(crate) fn read_installed(path: &Path, kind: PackageType) -> Result<Installed, String> {
    let manifest_path = path.join(kind.manifest_filename());
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("no readable {}: {error}", kind.manifest_filename()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .map_err(|error| format!("invalid {}: {error}", kind.manifest_filename()))?;

    let id = manifest
        .id
        .or(manifest.name)
        .ok_or_else(|| format!("{} declares no id or name", kind.manifest_filename()))?;

    Ok(Installed {
        id,
        title: manifest.title,
        version: manifest.version.map(VersionField::into_string),
    })
}

/// The subset of a package manifest (`module.json` / `system.json`) that the
/// installed-package scan reads.
#[derive(Debug, Deserialize)]
struct Manifest {
    /// The package id (`FoundryVTT` v10+).
    #[serde(default)]
    id: Option<String>,

    /// The legacy package identifier (pre-v10 manifests).
    #[serde(default)]
    name: Option<String>,

    /// Human-readable title.
    #[serde(default)]
    title: Option<String>,

    /// Declared version; a string in modern manifests but occasionally a
    /// bare JSON number in old ones.
    #[serde(default)]
    version: Option<VersionField>,
}

/// A manifest version that may be either a string or a bare number.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum VersionField {
    /// The common case: a version string.
    Text(String),
    /// Legacy manifests sometimes declare `"version": 1.0`.
    Number(serde_json::Number),
}

impl VersionField {
    /// Normalizes the version to a string, consuming the field.
    pub(crate) fn into_string(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Number(number) => number.to_string(),
        }
    }

    /// Normalizes the version to a string.
    pub(crate) fn as_string(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the installed-package scan against fabricated data dirs.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use crate::foundry::discovery;
    use std::fs;

    /// Builds a valid Foundry root and returns the resolved installation.
    fn fake_foundry(root: &Path) -> Installation {
        fs::create_dir_all(root.join("Config")).unwrap();
        fs::create_dir_all(root.join("Data")).unwrap();
        discovery::resolve(Some(root)).unwrap()
    }

    /// Writes a module directory with the given manifest contents.
    fn write_module(root: &Path, dir_name: &str, manifest: &str) {
        let dir = root.join("Data").join("modules").join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("module.json"), manifest).unwrap();
    }

    /// Modern manifests parse with id, title and version.
    #[test]
    fn scans_modern_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        write_module(
            dir.path(),
            "dice-so-nice",
            r#"{ "id": "dice-so-nice", "title": "Dice So Nice!", "version": "5.1.1" }"#,
        );

        let scan = scan(&installation, PackageType::Module).unwrap();

        assert_eq!(scan.packages.len(), 1);
        let module = &scan.packages[0];
        assert_eq!(module.id, "dice-so-nice");
        assert_eq!(module.title.as_deref(), Some("Dice So Nice!"));
        assert_eq!(module.version.as_deref(), Some("5.1.1"));
        assert!(scan.skipped.is_empty());
    }

    /// Legacy manifests using `name` and a numeric version still parse.
    #[test]
    fn scans_legacy_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        write_module(
            dir.path(),
            "old-module",
            r#"{ "name": "old-module", "version": 1.5 }"#,
        );

        let scan = scan(&installation, PackageType::Module).unwrap();

        assert_eq!(scan.packages[0].id, "old-module");
        assert_eq!(scan.packages[0].version.as_deref(), Some("1.5"));
    }

    /// Broken directories are reported as skipped, not silently dropped.
    #[test]
    fn reports_unreadable_directories() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        write_module(dir.path(), "broken", "not json at all");
        fs::create_dir_all(dir.path().join("Data").join("modules").join("empty")).unwrap();

        let scan = scan(&installation, PackageType::Module).unwrap();

        assert!(scan.packages.is_empty());
        assert_eq!(scan.skipped.len(), 2);
    }

    /// A missing packages directory yields an empty scan.
    #[test]
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());

        let scan = scan(&installation, PackageType::System).unwrap();

        assert!(scan.packages.is_empty());
        assert!(scan.skipped.is_empty());
    }

    /// Results come back sorted by id.
    #[test]
    fn results_are_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        write_module(dir.path(), "zeta", r#"{ "id": "zeta" }"#);
        write_module(dir.path(), "alpha", r#"{ "id": "alpha" }"#);

        let scan = scan(&installation, PackageType::Module).unwrap();
        let ids: Vec<&str> = scan.packages.iter().map(|p| p.id.as_str()).collect();

        assert_eq!(ids, ["alpha", "zeta"]);
    }
}
