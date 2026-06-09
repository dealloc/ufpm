//! Recursive dependency resolution for installs and updates.
//!
//! `FoundryVTT` dependencies are unversioned (effectively `latest`) and live
//! in package **manifests**, not the index — the index-level `requires`
//! field only mirrors the system requirement. Resolution therefore walks
//! manifests: hard `requires` are installed automatically, `recommends` are
//! gathered for one consolidated prompt, and a module's system requirement
//! (from the index `systems` field) is included when it is unambiguous.
//!
//! Resolution runs in two passes so a package can never end up in the
//! optional bucket when something hard-requires it: first the closure over
//! `requires` edges from the requested packages, then the closure of the
//! recommendations collected along the way.

use crate::api::types::DependencyRef;
use crate::foundry::PackageType;
use crate::{api, index, install};
use std::collections::{HashSet, VecDeque};

/// The package indexes for both package types.
#[derive(Debug)]
pub struct Indexes {
    /// The module index.
    pub module: index::Snapshot,

    /// The system index.
    pub system: index::Snapshot,
}

impl Indexes {
    /// The snapshot for one package type.
    #[must_use]
    pub fn get(&self, kind: PackageType) -> &index::Snapshot {
        match kind {
            PackageType::Module => &self.module,
            PackageType::System => &self.system,
        }
    }
}

/// The ids of everything already installed, per package type.
#[derive(Debug, Default)]
pub struct InstalledSets {
    /// Installed module ids.
    pub modules: HashSet<String>,

    /// Installed system ids.
    pub systems: HashSet<String>,
}

impl InstalledSets {
    /// Whether a package of the given type is installed.
    #[must_use]
    pub fn contains(&self, kind: PackageType, id: &str) -> bool {
        match kind {
            PackageType::Module => self.modules.contains(id),
            PackageType::System => self.systems.contains(id),
        }
    }
}

/// The outcome of dependency resolution: everything needed to run the
/// install, plus what went wrong along the way.
#[derive(Debug, Default)]
pub struct Plan {
    /// Jobs for the explicitly requested packages.
    pub requested: Vec<install::Job>,

    /// Jobs for missing hard dependencies (installed automatically).
    pub required: Vec<install::Job>,

    /// Jobs for missing recommended packages (subject to one prompt).
    pub recommended: Vec<install::Job>,

    /// Dependency-level problems that were skipped (unknown packages,
    /// unowned protected deps, unreadable manifests).
    pub warnings: Vec<String>,

    /// Requested-package failures: `(name, reason)`.
    pub failures: Vec<(String, String)>,
}

/// One resolved node: its installable job and outgoing dependency edges.
struct Node {
    /// The installable job for this package.
    job: install::Job,

    /// Hard dependencies.
    requires: Vec<DependencyRef>,

    /// Recommended companions.
    recommends: Vec<DependencyRef>,
}

/// Resolves the requested packages of `kind` into a full installation plan.
///
/// Already-installed dependencies are left untouched; unknown or
/// uninstallable dependencies become warnings (a missing dependency should
/// not block the package itself, matching Foundry's lenient model).
pub async fn plan(
    client: &api::Client,
    indexes: &Indexes,
    installed: &InstalledSets,
    kind: PackageType,
    requested: &[&api::types::Package],
    include_recommends: bool,
) -> Plan {
    let mut plan = Plan::default();
    let mut visited: HashSet<(PackageType, String)> = HashSet::new();
    let mut recommendations: Vec<DependencyRef> = Vec::new();

    // Pass 1: requested packages and the closure of their hard requirements.
    let mut queue: VecDeque<(PackageType, String, bool)> = requested
        .iter()
        .map(|package| (kind, package.name.clone(), true))
        .collect();
    while let Some((kind, name, is_requested)) = queue.pop_front() {
        if !visited.insert((kind, name.clone())) {
            continue;
        }
        match resolve_node(client, indexes, kind, &name).await {
            Err(reason) if is_requested => plan.failures.push((name, reason)),
            Err(reason) => plan
                .warnings
                .push(format!("skipping dependency `{name}`: {reason}")),
            Ok(node) => {
                enqueue_requirements(&node, indexes, installed, &visited, &mut queue, &mut plan);
                if include_recommends {
                    recommendations.extend(node.recommends.iter().cloned());
                }
                if is_requested {
                    plan.requested.push(node.job);
                } else {
                    plan.required.push(node.job);
                }
            }
        }
    }

    // Pass 2: recommendations (and *their* hard requirements). Anything the
    // first pass already covers is silently satisfied.
    let mut queue: VecDeque<(PackageType, String, bool)> = recommendations
        .into_iter()
        .filter(|dep| !installed.contains(dep.kind, &dep.id))
        .map(|dep| (dep.kind, dep.id, false))
        .collect();
    while let Some((kind, name, _)) = queue.pop_front() {
        if !visited.insert((kind, name.clone())) {
            continue;
        }
        match resolve_node(client, indexes, kind, &name).await {
            Err(reason) => plan
                .warnings
                .push(format!("skipping recommended `{name}`: {reason}")),
            Ok(node) => {
                enqueue_requirements(&node, indexes, installed, &visited, &mut queue, &mut plan);
                plan.recommended.push(node.job);
            }
        }
    }

    plan
}

/// Queues a node's missing hard dependencies, including its (unambiguous)
/// system requirement.
fn enqueue_requirements(
    node: &Node,
    indexes: &Indexes,
    installed: &InstalledSets,
    visited: &HashSet<(PackageType, String)>,
    queue: &mut VecDeque<(PackageType, String, bool)>,
    plan: &mut Plan,
) {
    for dep in &node.requires {
        if installed.contains(dep.kind, &dep.id) || visited.contains(&(dep.kind, dep.id.clone())) {
            continue;
        }
        queue.push_back((dep.kind, dep.id.clone(), false));
    }

    // The index `systems` field is the module's system requirement. With a
    // single entry it is unambiguous and treated as a hard dependency; with
    // several, any one of them satisfies it, so ufpm only reports it.
    if node.job.kind == PackageType::Module {
        let systems = &indexes
            .module
            .packages
            .iter()
            .find(|package| package.name == node.job.name)
            .map(|package| package.systems.clone())
            .unwrap_or_default();
        let satisfied = systems.is_empty()
            || systems.iter().any(|system| {
                installed.contains(PackageType::System, system)
                    || visited.contains(&(PackageType::System, system.clone()))
                    || queue
                        .iter()
                        .any(|(kind, name, _)| *kind == PackageType::System && name == system)
            });
        if satisfied {
            return;
        }
        if let [only] = systems.as_slice() {
            queue.push_back((PackageType::System, only.clone(), false));
        } else {
            plan.warnings.push(format!(
                "`{}` requires one of the systems [{}], none of which is installed",
                node.job.name,
                systems.join(", ")
            ));
        }
    }
}

/// Resolves one package into a node: its download job plus dependency edges.
///
/// Free packages get both from their manifest; protected packages get a
/// signed URL from the auth endpoint and a best-effort manifest read for
/// dependencies (protected manifests are usually public even when the
/// download is not).
///
/// # Errors
///
/// Returns a human-readable reason when the package cannot be installed.
async fn resolve_node(
    client: &api::Client,
    indexes: &Indexes,
    kind: PackageType,
    name: &str,
) -> Result<Node, String> {
    let snapshot = indexes.get(kind);
    let Some(package) = snapshot.packages.iter().find(|p| p.name == name) else {
        return Err("not found in the index".to_owned());
    };
    if package.is_protected && !snapshot.owned.contains(&package.id) {
        return Err("protected; purchase required".to_owned());
    }

    let manifest = client.fetch_manifest(&package.version.manifest).await;

    let job = if package.is_protected {
        let download = client
            .get_protected_download(kind, &package.name, &package.version.version)
            .await
            .map_err(|error| format!("protected download authorization failed: {error}"))?;
        install::Job {
            kind,
            name: package.name.clone(),
            version: package.version.version.clone(),
            download_url: download,
            protected: true,
        }
    } else {
        let manifest = manifest
            .as_ref()
            .map_err(|error| format!("manifest fetch failed: {error}"))?;
        let Some(download) = manifest.download.clone() else {
            return Err("the manifest declares no download URL".to_owned());
        };
        if let Some(id) = manifest.id()
            && id != package.name
        {
            return Err(format!("the manifest belongs to `{id}`, not `{name}`"));
        }
        install::Job {
            kind,
            name: package.name.clone(),
            version: manifest
                .version()
                .unwrap_or_else(|| package.version.version.clone()),
            download_url: download,
            protected: false,
        }
    };

    let (requires, recommends) = manifest
        .map(|manifest| (manifest.requires(), manifest.recommends()))
        .unwrap_or_default();

    Ok(Node {
        job,
        requires,
        recommends,
    })
}

#[cfg(test)]
mod tests {
    //! Resolution tests over a synthetic index and a mock manifest host.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use crate::api::types::{Package, VersionInfo};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Builds an index package pointing its manifest at the mock server.
    fn package(server: &MockServer, id: u64, name: &str, systems: &[&str]) -> Package {
        Package {
            id,
            name: name.to_owned(),
            title: name.to_owned(),
            author: None,
            description: None,
            url: None,
            is_protected: false,
            systems: systems.iter().map(|&s| s.to_owned()).collect(),
            version: VersionInfo {
                version: "1.0.0".to_owned(),
                manifest: format!("{}/{name}.json", server.uri()),
                required_core_version: None,
                compatible_core_version: None,
                notes: None,
            },
            verified: None,
            last_updated: None,
        }
    }

    /// Mounts a manifest with the given relationships on the mock server.
    async fn mount_manifest(server: &MockServer, name: &str, relationships: &str) {
        let body = format!(
            r#"{{ "id": "{name}", "version": "1.0.0",
                  "download": "{}/{name}.zip",
                  "relationships": {relationships} }}"#,
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path(format!("/{name}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    /// Builds the two index snapshots from module packages.
    fn indexes(modules: Vec<Package>, systems: Vec<Package>) -> Indexes {
        Indexes {
            module: index::Snapshot {
                packages: modules,
                owned: HashSet::new(),
                fetched_at: 0,
            },
            system: index::Snapshot {
                packages: systems,
                owned: HashSet::new(),
                fetched_at: 0,
            },
        }
    }

    /// Names of the jobs in a bucket, sorted.
    fn names(jobs: &[install::Job]) -> Vec<&str> {
        let mut names: Vec<&str> = jobs.iter().map(|job| job.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Requires are pulled in recursively; recommends are bucketed apart;
    /// installed and unknown dependencies are skipped (the latter with a
    /// warning); cycles terminate.
    #[tokio::test]
    async fn resolves_the_full_closure() {
        let server = MockServer::start().await;
        mount_manifest(
            &server,
            "alpha",
            r#"{ "requires": [ { "id": "beta" }, { "id": "alpha" }, { "id": "installed-dep" }, { "id": "ghost" } ],
                 "recommends": [ { "id": "gamma" } ] }"#,
        )
        .await;
        mount_manifest(&server, "beta", r#"{ "requires": [ { "id": "delta" } ] }"#).await;
        mount_manifest(&server, "gamma", "{}").await;
        mount_manifest(&server, "delta", "{}").await;
        let indexes = indexes(
            vec![
                package(&server, 1, "alpha", &[]),
                package(&server, 2, "beta", &[]),
                package(&server, 3, "gamma", &[]),
                package(&server, 4, "delta", &[]),
            ],
            Vec::new(),
        );
        let installed = InstalledSets {
            modules: ["installed-dep".to_owned()].into(),
            systems: HashSet::new(),
        };
        let client = api::Client::with_base_url(server.uri(), serde_json::json!({})).unwrap();
        let requested = [indexes.module.packages[0].clone()];
        let requested: Vec<&Package> = requested.iter().collect();

        let plan = plan(
            &client,
            &indexes,
            &installed,
            PackageType::Module,
            &requested,
            true,
        )
        .await;

        assert_eq!(names(&plan.requested), ["alpha"]);
        assert_eq!(names(&plan.required), ["beta", "delta"]);
        assert_eq!(names(&plan.recommended), ["gamma"]);
        assert!(plan.failures.is_empty());
        assert_eq!(
            plan.warnings.len(),
            1,
            "ghost should warn: {:?}",
            plan.warnings
        );
    }

    /// A single-system requirement becomes a hard system dependency; an
    /// ambiguous multi-system requirement only warns.
    #[tokio::test]
    async fn handles_system_requirements() {
        let server = MockServer::start().await;
        mount_manifest(&server, "single", "{}").await;
        mount_manifest(&server, "multi", "{}").await;
        mount_manifest(&server, "pf2e", "{}").await;
        let indexes = indexes(
            vec![
                package(&server, 1, "single", &["pf2e"]),
                package(&server, 2, "multi", &["dnd5e", "pf2e-old"]),
            ],
            vec![package(&server, 10, "pf2e", &[])],
        );
        let client = api::Client::with_base_url(server.uri(), serde_json::json!({})).unwrap();
        let requested: Vec<Package> = indexes.module.packages.clone();
        let requested: Vec<&Package> = requested.iter().collect();

        let plan = plan(
            &client,
            &indexes,
            &InstalledSets::default(),
            PackageType::Module,
            &requested,
            true,
        )
        .await;

        assert_eq!(names(&plan.requested), ["multi", "single"]);
        assert_eq!(names(&plan.required), ["pf2e"]);
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        assert!(plan.warnings[0].contains("multi"));
    }

    /// A package recommended by one node but required by another always
    /// lands in the required bucket.
    #[tokio::test]
    async fn requirement_beats_recommendation() {
        let server = MockServer::start().await;
        mount_manifest(
            &server,
            "rec-first",
            r#"{ "recommends": [ { "id": "shared" } ] }"#,
        )
        .await;
        mount_manifest(
            &server,
            "req-second",
            r#"{ "requires": [ { "id": "shared" } ] }"#,
        )
        .await;
        mount_manifest(&server, "shared", "{}").await;
        let indexes = indexes(
            vec![
                package(&server, 1, "rec-first", &[]),
                package(&server, 2, "req-second", &[]),
                package(&server, 3, "shared", &[]),
            ],
            Vec::new(),
        );
        let client = api::Client::with_base_url(server.uri(), serde_json::json!({})).unwrap();
        let requested: Vec<Package> = vec![
            indexes.module.packages[0].clone(),
            indexes.module.packages[1].clone(),
        ];
        let requested: Vec<&Package> = requested.iter().collect();

        let plan = plan(
            &client,
            &indexes,
            &InstalledSets::default(),
            PackageType::Module,
            &requested,
            true,
        )
        .await;

        assert_eq!(names(&plan.required), ["shared"]);
        assert!(plan.recommended.is_empty());
    }
}
