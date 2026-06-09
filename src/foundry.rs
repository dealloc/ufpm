//! Reading and modelling a local `FoundryVTT` installation.

pub mod discovery;

use std::path::{Path, PathBuf};

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

    /// The directory containing installed modules.
    #[must_use]
    pub fn modules_dir(&self) -> PathBuf {
        self.data_dir().join("modules")
    }

    /// The directory containing installed systems.
    #[must_use]
    pub fn systems_dir(&self) -> PathBuf {
        self.data_dir().join("systems")
    }

    /// The directory containing the user's worlds.
    #[must_use]
    pub fn worlds_dir(&self) -> PathBuf {
        self.data_dir().join("worlds")
    }
}
