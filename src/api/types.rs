//! Serde models for the package API responses.
//!
//! The API is undocumented, so these models are deliberately tolerant: they
//! only pin down the fields `ufpm` uses, default everything non-essential,
//! and ignore unknown fields so new API fields never break parsing.

use crate::foundry::local::VersionField;
use serde::Deserialize;

/// The response of `POST /_api/packages/get` for one package type.
#[derive(Debug, Deserialize)]
pub struct PackagesResponse {
    /// `"success"` on success; anything else marks a failed request.
    pub status: String,

    /// Every package of the requested type known to `FoundryVTT`.
    #[serde(default)]
    pub packages: Vec<Package>,

    /// IDs of the protected packages this license has purchased.
    #[serde(default)]
    pub owned: Vec<u64>,
}

/// One package as listed in the index.
///
/// The index is a snapshot: [`Package::version`] describes the *latest*
/// version only, and older versions can never be retrieved.
#[derive(Clone, Debug, Deserialize)]
pub struct Package {
    /// Numeric package ID, matched against [`PackagesResponse::owned`].
    pub id: u64,

    /// The package slug (also its installation directory name).
    pub name: String,

    /// Human-readable title.
    pub title: String,

    /// Author display name.
    #[serde(default)]
    pub author: Option<String>,

    /// Short description (may contain HTML).
    #[serde(default)]
    pub description: Option<String>,

    /// Project or repository URL.
    #[serde(default)]
    pub url: Option<String>,

    /// Whether downloads require purchase and the auth endpoint.
    #[serde(default)]
    pub is_protected: bool,

    /// Slugs of the systems this package requires, when system-specific.
    ///
    /// The index-level `requires` field mirrors this exactly, so `ufpm` only
    /// reads `systems`; real module-to-module dependencies live in package
    /// manifests instead.
    #[serde(default)]
    pub systems: Vec<String>,

    /// The latest released version of the package.
    pub version: VersionInfo,

    /// The newest `FoundryVTT` generation the package is verified for.
    #[serde(default)]
    pub verified: Option<String>,

    /// When the package was last updated (ISO 8601).
    #[serde(default)]
    pub last_updated: Option<String>,
}

/// The latest-version snapshot of a package.
#[derive(Clone, Debug, Deserialize)]
pub struct VersionInfo {
    /// The version string; free-form in practice (`1.2.3`, `V1.1`, `1..1`).
    pub version: String,

    /// URL of the package manifest (`module.json` / `system.json`).
    pub manifest: String,

    /// Minimum `FoundryVTT` core version required, when declared.
    #[serde(default)]
    pub required_core_version: Option<String>,

    /// `FoundryVTT` core version the package is known compatible with.
    #[serde(default)]
    pub compatible_core_version: Option<String>,

    /// URL of the release notes, when published.
    #[serde(default)]
    pub notes: Option<String>,
}

/// A package manifest fetched from its (third-party) manifest URL.
///
/// Only the fields the install pipeline needs are modelled; manifests in the
/// wild are wildly inconsistent, so everything is optional and tolerant.
#[derive(Debug, Deserialize)]
pub struct RemoteManifest {
    /// The package id (`FoundryVTT` v10+).
    #[serde(default)]
    id: Option<String>,

    /// The legacy package identifier (pre-v10 manifests).
    #[serde(default)]
    name: Option<String>,

    /// The declared version; a string in modern manifests but occasionally
    /// a bare number in old ones.
    #[serde(default)]
    version: Option<VersionField>,

    /// URL of the release zip archive.
    #[serde(default)]
    pub download: Option<String>,
}

impl RemoteManifest {
    /// The package id, with the legacy `name` fallback.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref().or(self.name.as_deref())
    }

    /// The declared version, normalized to a string.
    #[must_use]
    pub fn version(&self) -> Option<String> {
        self.version.as_ref().map(VersionField::as_string)
    }
}

#[cfg(test)]
mod tests {
    //! Deserialization tests against a trimmed real-world API response.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;

    /// The trimmed real-world index response used across the test suite.
    const FIXTURE: &str = include_str!("../../tests/fixtures/index-module.json");

    /// The fixture (covering protected, marketplace, content-provider,
    /// weird-version and null-maximum permutations) deserializes fully.
    #[test]
    fn parses_the_real_world_fixture() {
        let response: PackagesResponse = serde_json::from_str(FIXTURE).unwrap();

        assert_eq!(response.status, "success");
        assert_eq!(response.packages.len(), 8);
        assert_eq!(response.owned, vec![3293]);

        let protected = response.packages.iter().filter(|p| p.is_protected).count();
        assert_eq!(protected, 3);

        for package in &response.packages {
            assert!(!package.name.is_empty());
            assert!(!package.version.version.is_empty());
            assert!(!package.version.manifest.is_empty());
        }
    }

    /// Missing optional fields fall back to defaults instead of failing.
    #[test]
    fn tolerates_minimal_packages() {
        let raw = r#"{
            "status": "success",
            "packages": [{
                "id": 1,
                "name": "tiny",
                "title": "Tiny",
                "version": { "version": "1.0.0", "manifest": "https://example.test/module.json" }
            }]
        }"#;

        let response: PackagesResponse = serde_json::from_str(raw).unwrap();
        let package = &response.packages[0];

        assert!(!package.is_protected);
        assert!(package.author.is_none());
        assert!(package.systems.is_empty());
        assert!(response.owned.is_empty());
    }
}
