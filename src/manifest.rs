//! Manifest format for `ufpm export` / `ufpm import`.
//!
//! The manifest is a small TOML file that records the slugs of all installed
//! packages (and, optionally, the world they were exported from). Version pins
//! are deliberately omitted: import always installs the latest available
//! version of each slug.

use serde::{Deserialize, Serialize};

/// A snapshot of an installation's packages, serialisable to/from TOML.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ExportManifest {
    /// The world this manifest was scoped to, when exported with `--world`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,

    /// System slugs included in the snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub systems: Vec<String>,

    /// Module slugs included in the snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
}
