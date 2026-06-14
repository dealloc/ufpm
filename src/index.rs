//! The locally cached package index.
//!
//! Fetching the index is one slow, unpaginated API call per package type, so
//! responses are cached on disk (in the platform cache directory) with a TTL
//! ([`crate::constants::INDEX_TTL`]). The raw response body is stored
//! verbatim inside a small envelope, which keeps the cache forward-compatible
//! with API changes and trivially diagnosable.
//!
//! When a refresh fails but stale data exists on disk, the stale data is
//! served with an explicit [`Source::StaleFallback`] marker — on a spotty
//! connection, old data beats no data.

use crate::api;
use crate::constants::INDEX_TTL;
use crate::foundry::PackageType;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, trace};

/// Errors produced by the index cache.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform has no known cache directory.
    #[error("could not determine the platform cache directory")]
    NoCacheDir,

    /// A cache file could not be read or written.
    #[error("cache I/O failed at {}", path.display())]
    Io {
        /// The file or directory the operation failed on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The index could not be fetched and no cached fallback exists.
    #[error(transparent)]
    Api(#[from] api::Error),
}

/// How a returned [`Snapshot`] was obtained.
///
/// The snapshot's own [`Snapshot::age`] tells how old the served data is.
#[derive(Debug)]
pub enum Source {
    /// Freshly fetched from the API.
    Refreshed,

    /// Served from disk; the cached data is within its TTL.
    Cached,

    /// Refreshing failed; stale cached data was served as a fallback.
    StaleFallback {
        /// Why the refresh failed.
        error: api::Error,
    },
}

/// A loaded package index for one package type.
#[derive(Debug)]
pub struct Snapshot {
    /// Every package of this type, as returned by the API.
    pub packages: Vec<api::types::Package>,

    /// IDs of the protected packages this license owns.
    pub owned: HashSet<u64>,

    /// When the index was fetched, in seconds since the Unix epoch.
    pub fetched_at: u64,
}

impl Snapshot {
    /// Builds a snapshot from a parsed API response.
    fn from_response(response: api::types::PackagesResponse, fetched_at: u64) -> Self {
        Self {
            packages: response.packages,
            owned: response.owned.into_iter().collect(),
            fetched_at,
        }
    }

    /// How old this snapshot is.
    #[must_use]
    pub fn age(&self) -> Duration {
        Duration::from_secs(now_secs().saturating_sub(self.fetched_at))
    }
}

/// Summary of one cached index file, for `ufpm cache info`.
#[derive(Debug)]
pub struct Info {
    /// How old the cached data is.
    pub age: Duration,

    /// Number of packages in the cached index.
    pub packages: usize,

    /// Number of owned (purchased) protected packages.
    pub owned: usize,
}

/// The on-disk envelope wrapped around a raw API response.
#[derive(Deserialize, Serialize)]
struct Envelope {
    /// When the response was fetched, in seconds since the Unix epoch.
    fetched_at: u64,

    /// The `FoundryVTT` version the request reported (diagnostic only).
    foundry_version: String,

    /// The raw, unmodified API response body.
    response: Box<RawValue>,
}

/// Handle to the on-disk index cache.
#[derive(Debug)]
pub struct Cache {
    /// The directory all of `ufpm`'s cached data lives in.
    dir: PathBuf,
}

impl Cache {
    /// Opens the cache at the platform default location (e.g. `~/.cache/ufpm`).
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the platform has no cache directory.
    pub fn open() -> Result<Self, Error> {
        let dir = dirs::cache_dir().ok_or(Error::NoCacheDir)?.join("ufpm");
        Ok(Self::at(dir))
    }

    /// Opens a cache rooted at an explicit directory (used by tests).
    #[must_use]
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory this cache stores its data in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The directory package archives (and partial downloads) are kept in.
    #[must_use]
    pub fn downloads_dir(&self) -> PathBuf {
        self.dir.join("downloads")
    }

    /// Returns the index for one package type, fetching or refreshing it as
    /// needed.
    ///
    /// Behaviour: serve from disk while within the TTL (unless `force` is
    /// set); otherwise fetch from the API and persist the response. When the
    /// fetch fails but stale data exists, the stale data is served with a
    /// [`Source::StaleFallback`] marker instead of failing.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the index can neither be fetched nor served
    /// from disk, or when the fresh response cannot be persisted.
    pub async fn ensure(
        &self,
        kind: PackageType,
        client: &api::Client,
        force: bool,
    ) -> Result<(Snapshot, Source), Error> {
        let mut cached = self.read(kind);

        if !force && let Some(snapshot) = cached.take_if(|snapshot| snapshot.age() <= INDEX_TTL) {
            debug!(kind = %kind.api_name(), age = ?snapshot.age(), "serving index from cache");
            return Ok((snapshot, Source::Cached));
        }

        debug!(kind = %kind.api_name(), "index TTL expired or forced; fetching from API");
        match client.get_packages_raw(kind).await {
            Ok(raw) => {
                let response = api::parse_packages(&raw)?;
                let fetched_at = now_secs();
                self.write(kind, &raw, fetched_at)?;
                Ok((
                    Snapshot::from_response(response, fetched_at),
                    Source::Refreshed,
                ))
            }
            Err(error) => match cached {
                Some(snapshot) => {
                    debug!(kind = %kind.api_name(), %error, "index refresh failed; using stale data");
                    Ok((snapshot, Source::StaleFallback { error }))
                }
                None => Err(error.into()),
            },
        }
    }

    /// Summarizes the cached index for one package type, if a readable one
    /// exists.
    #[must_use]
    pub fn info(&self, kind: PackageType) -> Option<Info> {
        let snapshot = self.read(kind)?;
        Some(Info {
            age: snapshot.age(),
            packages: snapshot.packages.len(),
            owned: snapshot.owned.len(),
        })
    }

    /// Deletes all cached data (indexes and, later, partial downloads).
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the cache directory exists but cannot be
    /// removed.
    pub fn clear(&self) -> Result<(), Error> {
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir).map_err(|source| Error::Io {
                path: self.dir.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// The cache file holding the index for one package type.
    fn file(&self, kind: PackageType) -> PathBuf {
        self.dir.join(format!("index-{}.json", kind.api_name()))
    }

    /// Reads and parses the cached index for one package type.
    ///
    /// Any failure (missing file, corrupt envelope, unparseable response) is
    /// treated as "no cache": the file will simply be refetched, which makes
    /// the cache self-healing.
    fn read(&self, kind: PackageType) -> Option<Snapshot> {
        trace!(path = %self.file(kind).display(), "reading cached index");
        let raw = std::fs::read_to_string(self.file(kind)).ok()?;
        let envelope: Envelope = serde_json::from_str(&raw).ok()?;
        let response = api::parse_packages(envelope.response.get()).ok()?;
        Some(Snapshot::from_response(response, envelope.fetched_at))
    }

    /// Persists a raw API response inside a cache envelope.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the response is not valid JSON or the cache
    /// file cannot be written.
    fn write(&self, kind: PackageType, raw: &str, fetched_at: u64) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|source| Error::Io {
            path: self.dir.clone(),
            source,
        })?;

        let envelope = Envelope {
            fetched_at,
            foundry_version: crate::constants::foundry_version(),
            response: RawValue::from_string(raw.to_owned()).map_err(api::Error::Invalid)?,
        };
        let path = self.file(kind);
        debug!(path = %path.display(), "persisting index to cache");
        let contents = serde_json::to_string(&envelope).map_err(api::Error::Invalid)?;
        std::fs::write(&path, contents).map_err(|source| Error::Io { path, source })
    }
}

/// The current time in seconds since the Unix epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    //! Tests for the cache TTL, refresh and fallback behaviour, using a mock
    //! API server and temporary cache directories.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The trimmed real-world index response used across the test suite.
    const FIXTURE: &str = include_str!("../tests/fixtures/index-module.json");

    /// Builds a client pointed at the given mock server.
    fn client_for(server: &MockServer) -> api::Client {
        api::Client::with_base_url(server.uri(), serde_json::json!({})).unwrap()
    }

    /// Mounts a successful index response on the server.
    async fn mount_index(server: &MockServer, expected_calls: u64) {
        Mock::given(method("POST"))
            .and(path("/_api/packages/get"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .expect(expected_calls)
            .mount(server)
            .await;
    }

    /// A first load fetches from the API and persists the result.
    #[tokio::test]
    async fn first_load_fetches_and_persists() {
        let server = MockServer::start().await;
        mount_index(&server, 1).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));

        let (snapshot, source) = cache
            .ensure(PackageType::Module, &client_for(&server), false)
            .await
            .unwrap();

        assert_eq!(snapshot.packages.len(), 8);
        assert!(matches!(source, Source::Refreshed));
        assert!(cache.file(PackageType::Module).is_file());
    }

    /// A second load within the TTL is served from disk without a request.
    #[tokio::test]
    async fn fresh_cache_skips_the_network() {
        let server = MockServer::start().await;
        mount_index(&server, 1).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));
        let client = client_for(&server);

        cache
            .ensure(PackageType::Module, &client, false)
            .await
            .unwrap();
        let (snapshot, source) = cache
            .ensure(PackageType::Module, &client, false)
            .await
            .unwrap();

        assert_eq!(snapshot.packages.len(), 8);
        assert!(matches!(source, Source::Cached));
    }

    /// `force` refetches even when the cache is fresh.
    #[tokio::test]
    async fn force_always_refetches() {
        let server = MockServer::start().await;
        mount_index(&server, 2).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));
        let client = client_for(&server);

        cache
            .ensure(PackageType::Module, &client, false)
            .await
            .unwrap();
        let (_, source) = cache
            .ensure(PackageType::Module, &client, true)
            .await
            .unwrap();

        assert!(matches!(source, Source::Refreshed));
    }

    /// A corrupt cache file is treated as missing and refetched.
    #[tokio::test]
    async fn corrupt_cache_self_heals() {
        let server = MockServer::start().await;
        mount_index(&server, 1).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));
        std::fs::create_dir_all(cache.dir()).unwrap();
        std::fs::write(cache.file(PackageType::Module), "garbage").unwrap();

        let (_, source) = cache
            .ensure(PackageType::Module, &client_for(&server), false)
            .await
            .unwrap();

        assert!(matches!(source, Source::Refreshed));
    }

    /// When a forced refresh fails but cached data exists, the stale data is
    /// served with an explicit marker.
    #[tokio::test]
    async fn failed_refresh_falls_back_to_stale_data() {
        let server = MockServer::start().await;
        mount_index(&server, 1).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));
        let client = client_for(&server);
        cache
            .ensure(PackageType::Module, &client, false)
            .await
            .unwrap();
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let (snapshot, source) = cache
            .ensure(PackageType::Module, &client, true)
            .await
            .unwrap();

        assert_eq!(snapshot.packages.len(), 8);
        assert!(matches!(source, Source::StaleFallback { .. }));
    }

    /// A failed fetch with no cached fallback is an error.
    #[tokio::test]
    async fn failed_fetch_without_fallback_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));

        let error = cache
            .ensure(PackageType::Module, &client_for(&server), false)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Api(api::Error::Http { .. })));
    }

    /// `info` summarizes cached data and `clear` removes it.
    #[tokio::test]
    async fn info_and_clear_round_trip() {
        let server = MockServer::start().await;
        mount_index(&server, 1).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("ufpm"));

        assert!(cache.info(PackageType::Module).is_none());

        cache
            .ensure(PackageType::Module, &client_for(&server), false)
            .await
            .unwrap();
        let info = cache.info(PackageType::Module).unwrap();
        assert_eq!(info.packages, 8);
        assert_eq!(info.owned, 1);

        cache.clear().unwrap();
        assert!(cache.info(PackageType::Module).is_none());
        assert!(!cache.dir().exists());
    }
}
