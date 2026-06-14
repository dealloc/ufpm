//! Locating the `FoundryVTT` installation root on the local machine.
//!
//! Resolution order:
//!
//! 1. An explicit override (`--data-path` / `UFPM_DATA_PATH`), used as-is.
//! 2. The platform-default `options.json`, following its `dataPath` field:
//!
//!    | Platform | Location |
//!    |---|---|
//!    | Linux | `$XDG_DATA_HOME` or `~/.local/share`, then `FoundryVTT/Config/options.json` |
//!    | macOS | `~/Library/Application Support/FoundryVTT/Config/options.json` |
//!    | Windows | `%LOCALAPPDATA%\FoundryVTT\Config\options.json` |
//!
//! Installations whose `options.json` declares a non-null `awsConfig` store
//! their data on S3, which `ufpm` does not support yet; they are rejected
//! with a dedicated error instead of being misread as local.

use super::{Installation, RootSource};
use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{debug, trace};

/// Errors that can occur while locating a `FoundryVTT` installation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform has no known default data directory.
    #[error("could not determine the platform data directory; pass --data-path")]
    NoPlatformDefault,

    /// No `options.json` exists at the platform-default location.
    #[error(
        "no FoundryVTT configuration found at {}; pass --data-path if your installation is portable",
        .0.display()
    )]
    OptionsNotFound(PathBuf),

    /// An `options.json` exists but could not be read.
    #[error("failed to read {}", path.display())]
    OptionsUnreadable {
        /// The `options.json` that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// An `options.json` exists but is not valid JSON.
    #[error("failed to parse {}", path.display())]
    OptionsInvalid {
        /// The `options.json` that could not be parsed.
        path: PathBuf,
        /// The underlying parse error.
        #[source]
        source: serde_json::Error,
    },

    /// The installation stores its data on S3.
    #[error(
        "this installation stores its data on S3 (`awsConfig` is set in {}), which ufpm does not support yet",
        .0.display()
    )]
    S3Unsupported(PathBuf),

    /// The resolved root does not have the expected directory layout.
    #[error(
        "{} does not look like a FoundryVTT root: missing the `{missing}` directory",
        root.display()
    )]
    InvalidRoot {
        /// The directory that failed validation.
        root: PathBuf,
        /// The expected child directory that was absent.
        missing: &'static str,
    },
}

/// Resolves the `FoundryVTT` installation root.
///
/// Uses `explicit` when given (from `--data-path` / `UFPM_DATA_PATH`),
/// otherwise discovers the root through the platform-default `options.json`.
///
/// # Errors
///
/// Returns an [`Error`] when no installation can be located, the resolved
/// root does not have the expected layout, or the installation is S3-backed.
pub fn resolve(explicit: Option<&Path>) -> Result<Installation, Error> {
    if let Some(root) = explicit {
        debug!(root = %root.display(), "using explicit data path");
        from_explicit(root)
    } else {
        trace!("discovering FoundryVTT installation from platform defaults");
        let base = platform_base().ok_or(Error::NoPlatformDefault)?;
        from_default_base(&base)
    }
}

/// The platform-specific directory `FoundryVTT` keeps its default
/// configuration in (`~/.local/share`, `~/Library/Application Support` or
/// `%LOCALAPPDATA%`).
fn platform_base() -> Option<PathBuf> {
    dirs::data_local_dir()
}

/// Resolves an explicitly-provided root.
///
/// # Errors
///
/// Returns an [`Error`] when the directory layout is wrong or the root's own
/// `options.json` declares S3 storage.
fn from_explicit(root: &Path) -> Result<Installation, Error> {
    let installation = Installation::new(root.to_path_buf(), RootSource::Explicit);
    validate(&installation)?;
    Ok(installation)
}

/// Follows the default `options.json` under `base` to the configured
/// `dataPath`, falling back to the default folder itself when the field is
/// absent.
///
/// # Errors
///
/// Returns an [`Error`] when the `options.json` is missing or invalid, the
/// installation is S3-backed, or the resolved root has the wrong layout.
fn from_default_base(base: &Path) -> Result<Installation, Error> {
    let default_root = base.join("FoundryVTT");
    let options_path = default_root.join("Config").join("options.json");
    if !options_path.is_file() {
        return Err(Error::OptionsNotFound(options_path));
    }

    let options = read_options(&options_path)?;
    if options.aws_config.is_some() {
        return Err(Error::S3Unsupported(options_path));
    }

    let root = options.data_path.unwrap_or(default_root);
    debug!(root = %root.display(), "resolved installation root");
    let installation = Installation::new(root, RootSource::Discovered);
    validate(&installation)?;
    Ok(installation)
}

/// Checks that the installation has the expected `Config`/`Data` layout and
/// that its own `options.json` (when present) does not declare S3 storage.
///
/// # Errors
///
/// Returns an [`Error`] when a required directory is missing, the root's
/// `options.json` is unreadable, or the installation is S3-backed.
fn validate(installation: &Installation) -> Result<(), Error> {
    let checks = [
        (installation.config_dir(), "Config"),
        (installation.data_dir(), "Data"),
    ];
    for (dir, missing) in checks {
        if !dir.is_dir() {
            return Err(Error::InvalidRoot {
                root: installation.root().to_path_buf(),
                missing,
            });
        }
    }

    let options_path = installation.config_dir().join("options.json");
    if options_path.is_file() {
        let options = read_options(&options_path)?;
        if options.aws_config.is_some() {
            return Err(Error::S3Unsupported(options_path));
        }
    }

    Ok(())
}

/// Reads and parses an `options.json` file.
///
/// # Errors
///
/// Returns an [`Error`] when the file cannot be read or is not valid JSON.
fn read_options(path: &Path) -> Result<FoundryOptions, Error> {
    let raw = std::fs::read_to_string(path).map_err(|source| Error::OptionsUnreadable {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| Error::OptionsInvalid {
        path: path.to_path_buf(),
        source,
    })
}

/// The subset of `FoundryVTT`'s `options.json` that `ufpm` understands.
#[derive(Debug, Deserialize)]
struct FoundryOptions {
    /// The configured user-data directory (the `FoundryVTT` root).
    #[serde(rename = "dataPath")]
    data_path: Option<PathBuf>,

    /// S3 storage configuration; any non-null value marks the installation
    /// as S3-backed.
    #[serde(rename = "awsConfig")]
    aws_config: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    //! Unit tests for installation discovery, using temporary directories so
    //! the platform-specific path logic stays injectable.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use serde_json::json;
    use std::fs;

    /// Creates the minimal valid `FoundryVTT` root layout under `dir`.
    fn make_root(dir: &Path) {
        fs::create_dir_all(dir.join("Config")).unwrap();
        fs::create_dir_all(dir.join("Data")).unwrap();
    }

    /// Writes a default-location `options.json` under `base` and returns its path.
    fn write_options(base: &Path, contents: &str) -> PathBuf {
        let config = base.join("FoundryVTT").join("Config");
        fs::create_dir_all(&config).unwrap();
        let path = config.join("options.json");
        fs::write(&path, contents).unwrap();
        path
    }

    /// An explicit root with a valid layout resolves as-is.
    #[test]
    fn explicit_valid_root() {
        let dir = tempfile::tempdir().unwrap();
        make_root(dir.path());

        let installation = from_explicit(dir.path()).unwrap();
        assert_eq!(installation.root(), dir.path());
        assert_eq!(installation.source(), RootSource::Explicit);
    }

    /// An explicit root without a `Data` directory is rejected.
    #[test]
    fn explicit_root_missing_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Config")).unwrap();

        let error = from_explicit(dir.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidRoot {
                missing: "Data",
                ..
            }
        ));
    }

    /// Discovery follows `dataPath` to a custom root.
    #[test]
    fn discovery_follows_data_path() {
        let base = tempfile::tempdir().unwrap();
        let custom = tempfile::tempdir().unwrap();
        make_root(custom.path());
        let options = json!({ "dataPath": custom.path(), "awsConfig": null });
        write_options(base.path(), &options.to_string());

        let installation = from_default_base(base.path()).unwrap();
        assert_eq!(installation.root(), custom.path());
        assert_eq!(installation.source(), RootSource::Discovered);
    }

    /// Discovery falls back to the default folder when `dataPath` is absent.
    #[test]
    fn discovery_defaults_to_base_folder() {
        let base = tempfile::tempdir().unwrap();
        let default_root = base.path().join("FoundryVTT");
        make_root(&default_root);
        write_options(base.path(), "{}");

        let installation = from_default_base(base.path()).unwrap();
        assert_eq!(installation.root(), default_root);
    }

    /// A missing `options.json` produces a "not found" error.
    #[test]
    fn discovery_without_options_json() {
        let base = tempfile::tempdir().unwrap();

        let error = from_default_base(base.path()).unwrap_err();
        assert!(matches!(error, Error::OptionsNotFound(_)));
    }

    /// A non-null `awsConfig` is rejected as an unsupported S3 installation.
    #[test]
    fn s3_installations_are_rejected() {
        let base = tempfile::tempdir().unwrap();
        let options = json!({ "awsConfig": { "region": "eu-west-1" } });
        write_options(base.path(), &options.to_string());

        let error = from_default_base(base.path()).unwrap_err();
        assert!(matches!(error, Error::S3Unsupported(_)));
    }

    /// An explicit root whose own `options.json` declares S3 is rejected too.
    #[test]
    fn explicit_root_with_s3_options_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        make_root(dir.path());
        let options = json!({ "awsConfig": "s3-config.json" });
        fs::write(
            dir.path().join("Config").join("options.json"),
            options.to_string(),
        )
        .unwrap();

        let error = from_explicit(dir.path()).unwrap_err();
        assert!(matches!(error, Error::S3Unsupported(_)));
    }

    /// `awsConfig: null` counts as a local installation.
    #[test]
    fn null_aws_config_is_local() {
        let dir = tempfile::tempdir().unwrap();
        make_root(dir.path());
        let options = json!({ "awsConfig": null });
        fs::write(
            dir.path().join("Config").join("options.json"),
            options.to_string(),
        )
        .unwrap();

        assert!(from_explicit(dir.path()).is_ok());
    }

    /// Invalid JSON in `options.json` is reported as a parse error.
    #[test]
    fn invalid_options_json() {
        let base = tempfile::tempdir().unwrap();
        write_options(base.path(), "not json");

        let error = from_default_base(base.path()).unwrap_err();
        assert!(matches!(error, Error::OptionsInvalid { .. }));
    }
}
