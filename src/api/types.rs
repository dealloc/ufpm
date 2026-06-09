//! Serde models for the package API responses.
//!
//! The API is undocumented, so these models are deliberately tolerant: they
//! only pin down the fields `ufpm` uses, default everything non-essential,
//! and ignore unknown fields so new API fields never break parsing.

use crate::foundry::PackageType;
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

/// The response of `POST /_api/packages/auth` (protected downloads).
#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    /// `"success"` on success; anything else marks a rejected request.
    #[serde(default)]
    pub status: String,

    /// The time-limited signed download URL.
    #[serde(default)]
    pub download: Option<String>,
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

    /// Dependency declarations (`FoundryVTT` v10+).
    #[serde(default)]
    relationships: Option<Relationships>,

    /// Legacy flat dependency list (pre-v10 manifests); treated as requires.
    #[serde(default)]
    dependencies: Option<Vec<Relationship>>,
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

    /// The packages this manifest declares as hard requirements
    /// (unversioned — `FoundryVTT` dependencies are effectively `latest`).
    #[must_use]
    pub fn requires(&self) -> Vec<DependencyRef> {
        let mut requires: Vec<DependencyRef> = self
            .relationships
            .iter()
            .flat_map(|relationships| &relationships.requires)
            .filter_map(Relationship::to_ref)
            .collect();
        requires.extend(
            self.dependencies
                .iter()
                .flatten()
                .filter_map(Relationship::to_ref),
        );
        requires
    }

    /// The packages this manifest recommends (optional companions).
    #[must_use]
    pub fn recommends(&self) -> Vec<DependencyRef> {
        self.relationships
            .iter()
            .flat_map(|relationships| &relationships.recommends)
            .filter_map(Relationship::to_ref)
            .collect()
    }
}

/// The `relationships` block of a v10+ manifest.
#[derive(Debug, Deserialize)]
struct Relationships {
    /// Hard dependencies.
    #[serde(default)]
    requires: Vec<Relationship>,

    /// Optional companions.
    #[serde(default)]
    recommends: Vec<Relationship>,
}

/// One dependency entry in a manifest, in either modern or legacy shape.
#[derive(Debug, Deserialize)]
struct Relationship {
    /// The dependency's package id (modern manifests).
    #[serde(default)]
    id: Option<String>,

    /// The dependency's package id (legacy manifests).
    #[serde(default)]
    name: Option<String>,

    /// The dependency's package type (`module`, `system`, `world`).
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

impl Relationship {
    /// Converts the raw entry to a typed reference; entries without an id
    /// and (deprecated) world dependencies yield `None`.
    fn to_ref(&self) -> Option<DependencyRef> {
        let id = self.id.as_deref().or(self.name.as_deref())?;
        let kind = match self.kind.as_deref() {
            Some("system") => PackageType::System,
            None | Some("module") => PackageType::Module,
            Some(_) => return None,
        };
        Some(DependencyRef {
            kind,
            id: id.to_owned(),
        })
    }
}

/// A typed, unversioned dependency reference from a manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyRef {
    /// The dependency's package type.
    pub kind: PackageType,

    /// The dependency's package id.
    pub id: String,
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

    /// Modern relationships parse into typed references; deprecated world
    /// dependencies and id-less entries are dropped.
    #[test]
    fn parses_modern_relationships() {
        let raw = r#"{
            "id": "pkg",
            "relationships": {
                "requires": [
                    { "id": "lib-wrapper", "type": "module" },
                    { "id": "pf2e", "type": "system" },
                    { "id": "old-world", "type": "world" },
                    { "type": "module" }
                ],
                "recommends": [ { "id": "dice-so-nice" } ]
            }
        }"#;

        let manifest: RemoteManifest = serde_json::from_str(raw).unwrap();

        assert_eq!(
            manifest.requires(),
            [
                DependencyRef {
                    kind: PackageType::Module,
                    id: "lib-wrapper".to_owned()
                },
                DependencyRef {
                    kind: PackageType::System,
                    id: "pf2e".to_owned()
                },
            ]
        );
        assert_eq!(
            manifest.recommends(),
            [DependencyRef {
                kind: PackageType::Module,
                id: "dice-so-nice".to_owned()
            }]
        );
    }

    /// Legacy flat `dependencies` lists count as requirements.
    #[test]
    fn parses_legacy_dependencies() {
        let raw = r#"{ "name": "pkg", "dependencies": [ { "name": "socketlib" } ] }"#;

        let manifest: RemoteManifest = serde_json::from_str(raw).unwrap();

        assert_eq!(
            manifest.requires(),
            [DependencyRef {
                kind: PackageType::Module,
                id: "socketlib".to_owned()
            }]
        );
        assert!(manifest.recommends().is_empty());
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
