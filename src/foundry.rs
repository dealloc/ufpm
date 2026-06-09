//! Reading and modelling a local `FoundryVTT` installation.

pub mod discovery;
pub mod local;
pub mod version;

use std::fmt;
use std::path::{Path, PathBuf};

/// The kinds of `FoundryVTT` packages `ufpm` manages.
///
/// `FoundryVTT` also knows a `world` package type, which is deprecated and
/// deliberately unsupported.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageType {
    /// A module: an addon that extends systems or the core software.
    Module,
    /// A system: the rules implementation a world is built on.
    System,
}

impl PackageType {
    /// The name used for this type in `FoundryVTT` API requests.
    #[must_use]
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::System => "system",
        }
    }

    /// The `Data` subdirectory installed packages of this type live in.
    #[must_use]
    pub fn directory(self) -> &'static str {
        match self {
            Self::Module => "modules",
            Self::System => "systems",
        }
    }

    /// The manifest filename packages of this type carry.
    #[must_use]
    pub fn manifest_filename(self) -> &'static str {
        match self {
            Self::Module => "module.json",
            Self::System => "system.json",
        }
    }
}

impl fmt::Display for PackageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.api_name())
    }
}

/// Errors that can occur while loading the `license.json`.
#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    /// The file could not be read.
    #[error(
        "could not read {}; is FoundryVTT licensed on this machine?",
        path.display()
    )]
    Unreadable {
        /// The `license.json` that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The file is not valid JSON.
    #[error("could not parse {}", path.display())]
    Invalid {
        /// The `license.json` that could not be parsed.
        path: PathBuf,
        /// The underlying parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// How the root of an [`Installation`] was determined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootSource {
    /// Through `--data-path` or the `UFPM_DATA_PATH` environment variable.
    Explicit,
    /// By following `dataPath` in the platform-default `options.json`.
    Discovered,
}

/// A resolved local `FoundryVTT` installation.
///
/// `FoundryVTT` owns this directory tree and may modify it at any time; the
/// on-disk state is the only source of truth about installed packages.
#[derive(Clone, Debug)]
pub struct Installation {
    /// The root directory, containing `Config/` and `Data/`.
    root: PathBuf,
    /// How the root was located.
    source: RootSource,
}

impl Installation {
    /// Creates an installation rooted at `root`.
    fn new(root: PathBuf, source: RootSource) -> Self {
        Self { root, source }
    }

    /// The root directory of the installation.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How the root was located.
    #[must_use]
    pub fn source(&self) -> RootSource {
        self.source
    }

    /// The `Config` directory.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("Config")
    }

    /// The `license.json` file.
    ///
    /// Its contents are opaque and sensitive: pass them through verbatim,
    /// never log or inspect them.
    #[must_use]
    pub fn license_path(&self) -> PathBuf {
        self.config_dir().join("license.json")
    }

    /// The `Data` directory holding worlds, systems and modules.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("Data")
    }

    /// The directory containing installed packages of the given type.
    #[must_use]
    pub fn packages_dir(&self, kind: PackageType) -> PathBuf {
        self.data_dir().join(kind.directory())
    }

    /// The directory containing the user's worlds.
    #[must_use]
    pub fn worlds_dir(&self) -> PathBuf {
        self.data_dir().join("worlds")
    }

    /// Loads the `license.json` contents.
    ///
    /// The returned value is opaque and sensitive: pass it through to the
    /// API verbatim, never log or inspect it.
    ///
    /// # Errors
    ///
    /// Returns a [`LicenseError`] when the file is missing, unreadable or
    /// not valid JSON.
    pub fn load_license(&self) -> Result<serde_json::Value, LicenseError> {
        let path = self.license_path();
        let raw = std::fs::read_to_string(&path).map_err(|source| LicenseError::Unreadable {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&raw).map_err(|source| LicenseError::Invalid { path, source })
    }
}
