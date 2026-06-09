//! Resumable, progress-reporting file downloads.
//!
//! Downloads stream to a `.part` file next to the final destination, with a
//! small `.part.meta` sidecar recording the source URL and the server's
//! validator (`ETag` / `Last-Modified`). An interrupted download is resumed
//! with `Range` + `If-Range`: hosts that support ranges continue where the
//! transfer stopped, hosts that do not (or whose content changed) transparently
//! restart from scratch. Download hosts are arbitrary third parties, so
//! nothing is assumed about their capabilities.

use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// How often a failed download is attempted in total.
const MAX_ATTEMPTS: u32 = 3;

/// Errors that can occur while downloading a file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request never produced a usable HTTP response.
    #[error("the download request failed")]
    Transport(#[from] reqwest::Error),

    /// The host answered with a non-success HTTP status.
    #[error("the download host answered HTTP {status}")]
    Http {
        /// The HTTP status code of the response.
        status: reqwest::StatusCode,
    },

    /// Plain I/O failure on the partial or final file.
    #[error("I/O failed at {}", path.display())]
    Io {
        /// The file the operation failed on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// The sidecar persisted next to a partial download.
#[derive(Debug, Deserialize, Serialize)]
struct Meta {
    /// The canonical URL (without query/fragment) the partial data came
    /// from. Signed URLs are re-issued with fresh query parameters, so the
    /// query must not participate in resume matching; the `If-Range`
    /// validator protects against actual content changes.
    url: String,

    /// The server's `ETag` or `Last-Modified` value, used with `If-Range`.
    validator: Option<String>,
}

/// Strips the query and fragment off a URL for resume matching.
fn canonical(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Downloads `url` to `dest`, resuming a matching partial download when
/// possible and retrying transient failures up to [`MAX_ATTEMPTS`] times.
///
/// Progress (bytes, total when known, speed) is reported on `bar`.
///
/// # Errors
///
/// Returns an [`Error`] when the download keeps failing after retries, the
/// host answers with a client error, or the files cannot be written.
pub async fn fetch(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    bar: &ProgressBar,
) -> Result<(), Error> {
    let mut attempt = 1;
    loop {
        match fetch_once(http, url, dest, bar).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt < MAX_ATTEMPTS && is_transient(&error) => {
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Whether an error is worth retrying (network hiccups and server-side
/// errors; client errors are not).
fn is_transient(error: &Error) -> bool {
    match error {
        Error::Transport(_) => true,
        Error::Http { status } => status.is_server_error(),
        Error::Io { .. } => false,
    }
}

/// A single download attempt: resume when the partial data is usable,
/// restart otherwise.
///
/// # Errors
///
/// Returns an [`Error`] on request, status or I/O failure.
async fn fetch_once(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    bar: &ProgressBar,
) -> Result<(), Error> {
    let part = part_path(dest);
    let meta_path = meta_path(dest);
    let existing = resumable_bytes(&part, &meta_path, url);

    let mut request = http.get(url);
    if let Some((offset, validator)) = &existing {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        request = request.header(reqwest::header::IF_RANGE, validator);
    }

    let response = request.send().await?;
    let status = response.status();
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !status.is_success() {
        return Err(Error::Http { status });
    }

    let offset = match (&existing, resumed) {
        (Some((offset, _)), true) => *offset,
        _ => 0,
    };

    // Persist the validator before streaming so a torn download can be
    // resumed next time.
    if !resumed {
        let validator = response
            .headers()
            .get(reqwest::header::ETAG)
            .or_else(|| response.headers().get(reqwest::header::LAST_MODIFIED))
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        write_meta(&meta_path, url, validator)?;
    }

    if let Some(remaining) = response.content_length() {
        bar.set_style(crate::ui::byte_bar_style());
        bar.set_length(offset + remaining);
    }
    bar.set_position(offset);

    let mut file = open_part(&part, offset).await?;
    let mut response = response;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk)
            .await
            .map_err(|source| io_error(&part, source))?;
        bar.inc(chunk.len() as u64);
    }
    file.flush()
        .await
        .map_err(|source| io_error(&part, source))?;
    drop(file);

    tokio::fs::rename(&part, dest)
        .await
        .map_err(|source| io_error(dest, source))?;
    let _ = tokio::fs::remove_file(&meta_path).await; // best-effort cleanup
    Ok(())
}

/// Returns the resumable offset and validator when the partial file matches
/// this URL and a validator is available; `None` means start from scratch.
fn resumable_bytes(part: &Path, meta_path: &Path, url: &str) -> Option<(u64, String)> {
    let size = std::fs::metadata(part).ok()?.len();
    if size == 0 {
        return None;
    }
    let meta: Meta = serde_json::from_str(&std::fs::read_to_string(meta_path).ok()?).ok()?;
    if meta.url != canonical(url) {
        return None;
    }
    Some((size, meta.validator?))
}

/// Opens the partial file for appending at `offset` (truncating when the
/// download starts from scratch).
///
/// # Errors
///
/// Returns an [`Error`] when the file cannot be created or opened.
async fn open_part(part: &Path, offset: u64) -> Result<tokio::fs::File, Error> {
    if let Some(parent) = part.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io_error(parent, source))?;
    }
    let mut options = tokio::fs::OpenOptions::new();
    if offset > 0 {
        options.append(true);
    } else {
        options.write(true).create(true).truncate(true);
    }
    options
        .open(part)
        .await
        .map_err(|source| io_error(part, source))
}

/// Writes the resume sidecar.
///
/// # Errors
///
/// Returns an [`Error`] when the sidecar cannot be written.
fn write_meta(meta_path: &Path, url: &str, validator: Option<String>) -> Result<(), Error> {
    if let Some(parent) = meta_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    let meta = Meta {
        url: canonical(url).to_owned(),
        validator,
    };
    let contents = serde_json::to_string(&meta).unwrap_or_default();
    std::fs::write(meta_path, contents).map_err(|source| io_error(meta_path, source))
}

/// The partial-download path for a destination file.
fn part_path(dest: &Path) -> PathBuf {
    append_extension(dest, "part")
}

/// The resume-sidecar path for a destination file.
fn meta_path(dest: &Path) -> PathBuf {
    append_extension(dest, "part.meta")
}

/// Appends `suffix` to a path's file name (`pkg.zip` → `pkg.zip.part`).
fn append_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Builds an I/O [`Error`] for `path`.
fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    //! Download and resume tests against a mock host.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The full payload used across the download tests.
    const PAYLOAD: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    /// A hidden progress bar for tests.
    fn bar() -> ProgressBar {
        ProgressBar::hidden()
    }

    /// A plain download lands the payload at the destination and leaves no
    /// partial files behind.
    #[tokio::test]
    async fn downloads_to_destination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pkg.zip"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(PAYLOAD)
                    .insert_header("ETag", "\"v1\""),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pkg.zip");

        fetch(
            &reqwest::Client::new(),
            &format!("{}/pkg.zip", server.uri()),
            &dest,
            &bar(),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), PAYLOAD);
        assert!(!part_path(&dest).exists());
        assert!(!meta_path(&dest).exists());
    }

    /// A matching partial download resumes with a Range request and the
    /// halves are stitched together.
    #[tokio::test]
    async fn resumes_partial_downloads() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pkg.zip"))
            .and(header("Range", "bytes=10-"))
            .and(header("If-Range", "\"v1\""))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(&PAYLOAD[10..]))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pkg.zip");
        let url = format!("{}/pkg.zip", server.uri());
        std::fs::write(part_path(&dest), &PAYLOAD[..10]).unwrap();
        write_meta(&meta_path(&dest), &url, Some("\"v1\"".to_owned())).unwrap();

        fetch(&reqwest::Client::new(), &url, &dest, &bar())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), PAYLOAD);
    }

    /// A host that ignores ranges (or whose content changed) answers 200 and
    /// the download restarts cleanly instead of corrupting the file.
    #[tokio::test]
    async fn restarts_when_the_host_ignores_ranges() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pkg.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(PAYLOAD))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pkg.zip");
        let url = format!("{}/pkg.zip", server.uri());
        std::fs::write(part_path(&dest), b"stale-bytes").unwrap();
        write_meta(&meta_path(&dest), &url, Some("\"old\"".to_owned())).unwrap();

        fetch(&reqwest::Client::new(), &url, &dest, &bar())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), PAYLOAD);
    }

    /// A re-signed URL (same path, different query) still resumes: signed
    /// hosts re-issue URLs with fresh signatures after expiry.
    #[tokio::test]
    async fn resumes_across_resigned_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pkg.zip"))
            .and(header("Range", "bytes=10-"))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(&PAYLOAD[10..]))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pkg.zip");
        std::fs::write(part_path(&dest), &PAYLOAD[..10]).unwrap();
        write_meta(
            &meta_path(&dest),
            &format!("{}/pkg.zip?sig=old", server.uri()),
            Some("\"v1\"".to_owned()),
        )
        .unwrap();

        fetch(
            &reqwest::Client::new(),
            &format!("{}/pkg.zip?sig=fresh", server.uri()),
            &dest,
            &bar(),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), PAYLOAD);
    }

    /// A partial file for a different URL is ignored, not resumed.
    #[tokio::test]
    async fn ignores_partial_data_from_other_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pkg.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(PAYLOAD))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("pkg.zip");
        std::fs::write(part_path(&dest), b"other").unwrap();
        write_meta(
            &meta_path(&dest),
            "https://elsewhere.test/other.zip",
            Some("\"x\"".to_owned()),
        )
        .unwrap();

        fetch(
            &reqwest::Client::new(),
            &format!("{}/pkg.zip", server.uri()),
            &dest,
            &bar(),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), PAYLOAD);
    }

    /// Client errors are not retried and surface as HTTP errors.
    #[tokio::test]
    async fn client_errors_fail_fast() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();

        let error = fetch(
            &reqwest::Client::new(),
            &format!("{}/gone.zip", server.uri()),
            &dir.path().join("gone.zip"),
            &bar(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            Error::Http {
                status: reqwest::StatusCode::NOT_FOUND
            }
        ));
    }
}
