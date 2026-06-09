//! Hardcoded constants that configure how `ufpm` talks to `FoundryVTT`.

use std::time::Duration;

/// `Authorization` header value for all `foundryvtt.com` API calls.
///
/// This is a public constant shipped inside the `FoundryVTT` client itself,
/// not a user secret; the per-user part of authentication is the
/// `license.json` body sent with every request.
pub const API_KEY: &str = "APIKey:foundryvtt_hkmg5t4zxc092e31mkfbg3";

/// How long a cached package index stays fresh before being refreshed.
pub const INDEX_TTL: Duration = Duration::from_hours(24);

/// The `FoundryVTT` release version reported to the package API and used for
/// compatibility warnings.
///
/// `FoundryVTT` offers no reliable way to detect the version of an
/// installation from its data directory, so `ufpm` ships a constant and lets
/// users override it through the [`FOUNDRY_VERSION_ENV`] environment variable.
pub const FOUNDRY_VERSION: &str = "14.362";

/// Environment variable that overrides [`FOUNDRY_VERSION`].
pub const FOUNDRY_VERSION_ENV: &str = "UFPM_FOUNDRY_VERSION";

/// Returns the effective `FoundryVTT` version: the [`FOUNDRY_VERSION_ENV`]
/// override when set, the built-in [`FOUNDRY_VERSION`] constant otherwise.
#[must_use]
pub fn foundry_version() -> String {
    std::env::var(FOUNDRY_VERSION_ENV).unwrap_or_else(|_| FOUNDRY_VERSION.to_owned())
}
