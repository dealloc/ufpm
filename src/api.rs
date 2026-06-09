//! HTTP client for the undocumented `foundryvtt.com` package API.
//!
//! Every request carries two pieces of authentication: the constant
//! [`crate::constants::API_KEY`] in the `Authorization` header, and the full
//! (opaque) `license.json` contents in the request body.
//!
//! The API is undocumented and may change shape at any time; the models in
//! [`types`] therefore only pin down the fields `ufpm` actually uses and
//! ignore everything else.

pub mod types;

use crate::constants;
use crate::foundry::PackageType;
use std::time::Duration;

/// Errors produced by the `FoundryVTT` package API client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request never produced a usable HTTP response.
    #[error("request to the FoundryVTT API failed")]
    Transport(#[from] reqwest::Error),

    /// The API answered with a non-success HTTP status.
    #[error("the FoundryVTT API answered HTTP {status}; has the undocumented API changed?")]
    Http {
        /// The HTTP status code of the response.
        status: reqwest::StatusCode,
    },

    /// The response body could not be parsed into the expected shape.
    #[error("could not parse the FoundryVTT API response; has the undocumented API changed?")]
    Invalid(#[source] serde_json::Error),

    /// The API answered, but reported a non-success status in the body.
    #[error("the FoundryVTT API reported status {0:?}")]
    Failed(String),
}

/// Client for the `foundryvtt.com` package API.
#[derive(Debug)]
pub struct Client {
    /// The underlying HTTP client.
    http: reqwest::Client,
    /// Base URL of the API; injectable so tests can point at a mock server.
    base_url: String,
    /// Opaque `license.json` contents sent with every request. Never logged.
    license: serde_json::Value,
}

impl Client {
    /// Creates a client for the production `foundryvtt.com` API.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the underlying HTTP client cannot be built.
    pub fn new(license: serde_json::Value) -> Result<Self, Error> {
        Self::with_base_url("https://foundryvtt.com".to_owned(), license)
    }

    /// Creates a client against an arbitrary base URL (used by tests).
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the underlying HTTP client cannot be built.
    pub fn with_base_url(base_url: String, license: serde_json::Value) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base_url,
            license,
        })
    }

    /// The underlying HTTP client, for downloads from third-party hosts.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Fetches and parses a package manifest from its manifest URL.
    ///
    /// Manifest URLs point at arbitrary third-party hosts (GitHub, GitLab,
    /// random S3 buckets …), so — deliberately — no `Authorization` header
    /// is sent here.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the request fails, the host answers with a
    /// non-success status, or the body is not a JSON manifest.
    pub async fn fetch_manifest(&self, url: &str) -> Result<types::RemoteManifest, Error> {
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Http { status });
        }
        let text = response.text().await?;
        serde_json::from_str(&text).map_err(Error::Invalid)
    }

    /// Fetches the full package index for one package type and returns the
    /// raw response body.
    ///
    /// The API has no pagination: this is a single, slow call that returns
    /// every package of the given type.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the request fails or the API answers with a
    /// non-success HTTP status.
    pub async fn get_packages_raw(&self, kind: PackageType) -> Result<String, Error> {
        let body = serde_json::json!({
            "type": kind.api_name(),
            "version": constants::foundry_version(),
            "license": self.license,
        });

        let response = self
            .http
            .post(format!("{}/_api/packages/get", self.base_url))
            .header(reqwest::header::AUTHORIZATION, constants::API_KEY)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Http { status });
        }

        Ok(response.text().await?)
    }
}

/// Parses a raw `/_api/packages/get` response body, validating the embedded
/// `status` field.
///
/// # Errors
///
/// Returns an [`Error`] when the body is not the expected JSON shape or the
/// API reported a non-success status.
pub fn parse_packages(raw: &str) -> Result<types::PackagesResponse, Error> {
    let response: types::PackagesResponse = serde_json::from_str(raw).map_err(Error::Invalid)?;
    if response.status == "success" {
        Ok(response)
    } else {
        Err(Error::Failed(response.status))
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the API client against a mock HTTP server.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The trimmed real-world index response used across the test suite.
    const FIXTURE: &str = include_str!("../tests/fixtures/index-module.json");

    /// The client sends the constant API key, the license body and the
    /// package type, and parses the response.
    #[tokio::test]
    async fn fetches_and_parses_the_index() {
        let server = MockServer::start().await;
        let license = serde_json::json!({ "license": "opaque-blob" });
        Mock::given(method("POST"))
            .and(path("/_api/packages/get"))
            .and(header("Authorization", constants::API_KEY))
            .and(body_partial_json(serde_json::json!({
                "type": "module",
                "license": { "license": "opaque-blob" },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base_url(server.uri(), license).unwrap();
        let raw = client.get_packages_raw(PackageType::Module).await.unwrap();
        let response = parse_packages(&raw).unwrap();

        assert_eq!(response.packages.len(), 8);
        assert_eq!(response.owned, vec![3293]);
    }

    /// A non-success HTTP status is reported as an HTTP error.
    #[tokio::test]
    async fn reports_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = Client::with_base_url(server.uri(), serde_json::json!({})).unwrap();
        let error = client
            .get_packages_raw(PackageType::Module)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Http { .. }));
    }

    /// A body-level non-success status is reported as a failure.
    #[test]
    fn reports_body_level_failures() {
        let error = parse_packages(r#"{ "status": "error", "packages": [] }"#).unwrap_err();
        assert!(matches!(error, Error::Failed(status) if status == "error"));
    }

    /// Garbage bodies are reported as parse errors.
    #[test]
    fn reports_unparseable_bodies() {
        let error = parse_packages("<html>nope</html>").unwrap_err();
        assert!(matches!(error, Error::Invalid(_)));
    }
}
