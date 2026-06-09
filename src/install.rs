//! Transactional package installation.
//!
//! The unit of atomicity is one package directory. An install/update runs:
//! download → extract into a staging directory *on the same filesystem* →
//! validate the staged manifest → rename the previous directory to a backup
//! → rename the staged directory into place → drop the backup. Any failure
//! after the first rename restores the backup, so the previous state
//! survives every failure mode; a crash mid-swap is healed by [`recover`]
//! on the next run.

pub mod download;
pub mod extract;

use crate::foundry::{Installation, PackageType, local};
use indicatif::ProgressBar;
use std::io;
use std::path::{Path, PathBuf};

/// Errors that can occur while installing a package.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The archive download failed.
    #[error("downloading `{name}` failed")]
    Download {
        /// The package being downloaded.
        name: String,
        /// The underlying download error.
        #[source]
        source: download::Error,
    },

    /// The archive could not be extracted.
    #[error("extracting `{name}` failed")]
    Extract {
        /// The package being extracted.
        name: String,
        /// The underlying extraction error.
        #[source]
        source: extract::Error,
    },

    /// The staged archive is not the package that was requested.
    #[error("the archive contains `{found}`, not the requested `{expected}`")]
    WrongPackage {
        /// The package that was requested.
        expected: String,
        /// The package id found in the staged manifest.
        found: String,
    },

    /// The archive layout or manifest is unusable.
    #[error("the archive for `{name}` is unusable: {reason}")]
    InvalidArchive {
        /// The package whose archive is unusable.
        name: String,
        /// Why the archive was rejected.
        reason: String,
    },

    /// Plain I/O failure during staging or the swap.
    #[error("I/O failed at {}", path.display())]
    Io {
        /// The file or directory the operation failed on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// A blocking task could not be joined (runtime shutdown).
    #[error("internal failure: {0}")]
    Internal(String),
}

/// Everything needed to install one resolved package.
#[derive(Clone, Debug)]
pub struct Job {
    /// The package type being installed.
    pub kind: PackageType,

    /// The package slug; also its installation directory name.
    pub name: String,

    /// The version being installed (for filenames and reporting).
    pub version: String,

    /// The release zip URL: from the package manifest for free packages, a
    /// time-limited signed URL for protected ones.
    pub download_url: String,

    /// Whether the package is protected (signed URLs expire and must be
    /// re-requested when a download fails with an authorization error).
    pub protected: bool,
}

impl Job {
    /// The cache filename for this job's archive.
    fn archive_filename(&self) -> String {
        format!(
            "{}-{}-{}.zip",
            self.kind.api_name(),
            sanitize(&self.name),
            sanitize(&self.version)
        )
    }
}

/// Replaces path-hostile characters in a filename component.
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Downloads the job's archive into `downloads_dir` (resuming a previous
/// partial download when possible) and returns the archive path.
///
/// # Errors
///
/// Returns an [`Error`] when the download ultimately fails.
pub async fn download_archive(
    http: &reqwest::Client,
    downloads_dir: &Path,
    job: &Job,
    bar: &ProgressBar,
) -> Result<PathBuf, Error> {
    let dest = downloads_dir.join(job.archive_filename());
    download::fetch(http, &job.download_url, &dest, bar)
        .await
        .map_err(|source| Error::Download {
            name: job.name.clone(),
            source,
        })?;
    Ok(dest)
}

/// Extracts, validates and atomically swaps a downloaded archive into place.
/// On success the staging area and the archive are cleaned up; on failure
/// the previously installed state is left untouched.
///
/// # Errors
///
/// Returns an [`Error`] when extraction, validation or the swap fails.
pub async fn apply(installation: &Installation, job: &Job, archive: PathBuf) -> Result<(), Error> {
    let staging = ufpm_dir(installation)
        .join("staging")
        .join(job.kind.directory())
        .join(&job.name);
    remove_if_exists(&staging)?;
    std::fs::create_dir_all(&staging).map_err(|source| Error::Io {
        path: staging.clone(),
        source,
    })?;

    let blocking_archive = archive.clone();
    let blocking_staging = staging.clone();
    tokio::task::spawn_blocking(move || extract::extract(&blocking_archive, &blocking_staging))
        .await
        .map_err(|join| Error::Internal(join.to_string()))?
        .map_err(|source| Error::Extract {
            name: job.name.clone(),
            source,
        })?;

    let root = locate_package_root(&staging, job.kind, &job.name)?;
    validate_staged(&root, job)?;
    swap_in(installation, job.kind, &job.name, &root)?;

    // Best-effort cleanup; leftovers are swept by `recover` on the next run.
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&archive);
    Ok(())
}

/// Finds the directory inside `staging` that holds the package manifest:
/// either the staging root itself or a single wrapping top-level folder
/// (a layout many archives use).
///
/// # Errors
///
/// Returns an [`Error`] when no manifest can be located.
fn locate_package_root(staging: &Path, kind: PackageType, name: &str) -> Result<PathBuf, Error> {
    if staging.join(kind.manifest_filename()).is_file() {
        return Ok(staging.to_path_buf());
    }

    let entries: Vec<PathBuf> = std::fs::read_dir(staging)
        .map_err(|source| Error::Io {
            path: staging.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();

    if let [single] = entries.as_slice()
        && single.is_dir()
        && single.join(kind.manifest_filename()).is_file()
    {
        return Ok(single.clone());
    }

    Err(Error::InvalidArchive {
        name: name.to_owned(),
        reason: format!("no {} at the archive root", kind.manifest_filename()),
    })
}

/// Checks that the staged directory really contains the requested package.
///
/// # Errors
///
/// Returns an [`Error`] when the staged manifest is unreadable or belongs to
/// a different package.
fn validate_staged(root: &Path, job: &Job) -> Result<(), Error> {
    let staged = local::read_installed(root, job.kind).map_err(|reason| Error::InvalidArchive {
        name: job.name.clone(),
        reason,
    })?;
    if staged.id != job.name {
        return Err(Error::WrongPackage {
            expected: job.name.clone(),
            found: staged.id,
        });
    }
    Ok(())
}

/// Replaces `Data/<kind>/<name>` with the staged directory, restoring the
/// previous directory if the swap fails partway.
///
/// # Errors
///
/// Returns an [`Error`] when a rename fails; the previous state is restored
/// before returning.
fn swap_in(
    installation: &Installation,
    kind: PackageType,
    name: &str,
    staged_root: &Path,
) -> Result<(), Error> {
    let parent = installation.packages_dir(kind);
    std::fs::create_dir_all(&parent).map_err(|source| Error::Io {
        path: parent.clone(),
        source,
    })?;
    let target = parent.join(name);

    let backup_parent = ufpm_dir(installation).join("backup").join(kind.directory());
    std::fs::create_dir_all(&backup_parent).map_err(|source| Error::Io {
        path: backup_parent.clone(),
        source,
    })?;
    let backup = backup_parent.join(name);

    let had_previous = target.exists();
    if had_previous {
        std::fs::rename(&target, &backup).map_err(|source| Error::Io {
            path: target.clone(),
            source,
        })?;
    }

    if let Err(source) = std::fs::rename(staged_root, &target) {
        if had_previous {
            // Best-effort restore; `recover` finishes the job next run if
            // this rename fails too.
            let _ = std::fs::rename(&backup, &target);
        }
        return Err(Error::Io {
            path: target,
            source,
        });
    }

    if had_previous {
        // Non-fatal: a leftover backup is swept by `recover` next run.
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Removes an installed package transactionally: the directory is renamed
/// into the trash area first (one atomic disappearance), then deleted
/// best-effort — a failed deletion is swept by [`recover`] on the next run
/// instead of leaving a half-removed package in place.
///
/// # Errors
///
/// Returns an [`Error`] when the package directory cannot be moved away.
pub fn remove(installation: &Installation, kind: PackageType, name: &str) -> Result<(), Error> {
    let target = installation.packages_dir(kind).join(name);
    let trash_parent = ufpm_dir(installation).join("trash").join(kind.directory());
    std::fs::create_dir_all(&trash_parent).map_err(|source| Error::Io {
        path: trash_parent.clone(),
        source,
    })?;
    let trash = trash_parent.join(name);
    remove_if_exists(&trash)?;

    std::fs::rename(&target, &trash).map_err(|source| Error::Io {
        path: target,
        source,
    })?;
    // Best-effort: leftovers in the trash are swept by `recover` next run.
    let _ = std::fs::remove_dir_all(&trash);
    Ok(())
}

/// What [`recover`] found and did.
#[derive(Debug, Default)]
pub struct Recovery {
    /// Packages restored from backups left by an interrupted operation.
    pub restored: Vec<String>,

    /// Packages whose backup could not be restored, with the reason.
    pub failed: Vec<(String, io::Error)>,
}

/// Sweeps leftovers of a previously interrupted run: backups whose target
/// vanished mid-swap are restored, backups of completed swaps are dropped,
/// and the staging area is cleared. Everything is best-effort — failures
/// are reported in the result, never fatal.
#[must_use]
pub fn recover(installation: &Installation) -> Recovery {
    let mut recovery = Recovery::default();
    let base = ufpm_dir(installation);

    for kind in [PackageType::Module, PackageType::System] {
        let dir = base.join("backup").join(kind.directory());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let parent = installation.packages_dir(kind);
            let target = parent.join(entry.file_name());
            if target.exists() {
                // The swap completed; the backup is a leftover.
                let _ = std::fs::remove_dir_all(entry.path());
            } else if let Err(error) = std::fs::create_dir_all(&parent)
                .and_then(|()| std::fs::rename(entry.path(), &target))
            {
                recovery.failed.push((name, error));
            } else {
                recovery.restored.push(name);
            }
        }
    }

    for transient in ["staging", "trash"] {
        let dir = base.join(transient);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    recovery
}

/// The directory `ufpm` uses for staging and backups, kept inside `Data`
/// so renames stay on one filesystem.
fn ufpm_dir(installation: &Installation) -> PathBuf {
    installation.data_dir().join(".ufpm")
}

/// Removes a directory tree if it exists.
///
/// # Errors
///
/// Returns an [`Error`] when the directory exists but cannot be removed.
fn remove_if_exists(dir: &Path) -> Result<(), Error> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Transaction tests: happy installs, failure rollback and crash
    //! recovery, all against fabricated Foundry roots.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use crate::foundry::discovery;
    use std::fs::File;
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// Builds a valid Foundry root and returns the resolved installation.
    fn fake_foundry(root: &Path) -> Installation {
        std::fs::create_dir_all(root.join("Config")).unwrap();
        std::fs::create_dir_all(root.join("Data")).unwrap();
        discovery::resolve(Some(root)).unwrap()
    }

    /// Builds a zip archive at `path` from `(name, contents)` entries.
    fn build_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        for (name, contents) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    /// A job for the `pkg` test module.
    fn job() -> Job {
        Job {
            kind: PackageType::Module,
            name: "pkg".to_owned(),
            version: "2.0.0".to_owned(),
            download_url: String::new(),
            protected: false,
        }
    }

    /// Removing a package moves it out atomically and leaves nothing behind.
    #[test]
    fn remove_deletes_the_package() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let target = installation.packages_dir(PackageType::Module).join("pkg");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("module.json"), r#"{ "id": "pkg" }"#).unwrap();

        remove(&installation, PackageType::Module, "pkg").unwrap();

        assert!(!target.exists());
        assert!(
            !ufpm_dir(&installation)
                .join("trash")
                .join("modules")
                .join("pkg")
                .exists()
        );
    }

    /// Removing something that is not installed fails cleanly.
    #[test]
    fn remove_missing_package_errors() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());

        assert!(remove(&installation, PackageType::Module, "ghost").is_err());
    }

    /// Installing into an empty slot lands the package and cleans up.
    #[tokio::test]
    async fn installs_a_new_package() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let archive = dir.path().join("pkg.zip");
        build_zip(
            &archive,
            &[("module.json", r#"{ "id": "pkg", "version": "2.0.0" }"#)],
        );

        apply(&installation, &job(), archive.clone()).await.unwrap();

        let manifest = installation
            .packages_dir(PackageType::Module)
            .join("pkg")
            .join("module.json");
        assert!(manifest.is_file());
        assert!(!archive.exists(), "archive should be cleaned up");
        assert!(
            !ufpm_dir(&installation)
                .join("staging")
                .join("modules")
                .join("pkg")
                .exists()
        );
    }

    /// Archives wrapped in a single top-level folder are lifted.
    #[tokio::test]
    async fn lifts_single_top_level_folders() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let archive = dir.path().join("pkg.zip");
        build_zip(
            &archive,
            &[("pkg-release/module.json", r#"{ "id": "pkg" }"#)],
        );

        apply(&installation, &job(), archive).await.unwrap();

        assert!(
            installation
                .packages_dir(PackageType::Module)
                .join("pkg")
                .join("module.json")
                .is_file()
        );
    }

    /// Updating replaces the previous version and drops the backup.
    #[tokio::test]
    async fn updates_replace_the_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let target = installation.packages_dir(PackageType::Module).join("pkg");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("module.json"),
            r#"{ "id": "pkg", "version": "1.0.0" }"#,
        )
        .unwrap();
        let archive = dir.path().join("pkg.zip");
        build_zip(
            &archive,
            &[("module.json", r#"{ "id": "pkg", "version": "2.0.0" }"#)],
        );

        apply(&installation, &job(), archive).await.unwrap();

        let manifest = std::fs::read_to_string(target.join("module.json")).unwrap();
        assert!(manifest.contains("2.0.0"));
        assert!(
            !ufpm_dir(&installation)
                .join("backup")
                .join("modules")
                .join("pkg")
                .exists()
        );
    }

    /// An archive containing the wrong package never touches the previous
    /// installation.
    #[tokio::test]
    async fn wrong_package_leaves_previous_state_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let target = installation.packages_dir(PackageType::Module).join("pkg");
        std::fs::create_dir_all(&target).unwrap();
        let original = r#"{ "id": "pkg", "version": "1.0.0" }"#;
        std::fs::write(target.join("module.json"), original).unwrap();
        let archive = dir.path().join("pkg.zip");
        build_zip(&archive, &[("module.json", r#"{ "id": "imposter" }"#)]);

        let error = apply(&installation, &job(), archive).await.unwrap_err();

        assert!(matches!(error, Error::WrongPackage { .. }));
        assert_eq!(
            std::fs::read_to_string(target.join("module.json")).unwrap(),
            original
        );
    }

    /// An unusable archive (no manifest anywhere) is rejected.
    #[tokio::test]
    async fn manifestless_archives_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let archive = dir.path().join("pkg.zip");
        build_zip(&archive, &[("readme.txt", "no manifest here")]);

        let error = apply(&installation, &job(), archive).await.unwrap_err();

        assert!(matches!(error, Error::InvalidArchive { .. }));
    }

    /// A backup without its target (crash mid-swap) is restored by the
    /// recovery sweep; staging leftovers are cleared.
    #[test]
    fn recovery_restores_orphaned_backups() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let backup = ufpm_dir(&installation)
            .join("backup")
            .join("modules")
            .join("pkg");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("module.json"), r#"{ "id": "pkg" }"#).unwrap();
        let staging = ufpm_dir(&installation)
            .join("staging")
            .join("modules")
            .join("junk");
        std::fs::create_dir_all(&staging).unwrap();

        let recovery = recover(&installation);

        assert_eq!(recovery.restored, ["pkg"]);
        assert!(
            installation
                .packages_dir(PackageType::Module)
                .join("pkg")
                .join("module.json")
                .is_file()
        );
        assert!(!ufpm_dir(&installation).join("staging").exists());
    }

    /// A backup whose target exists (crash after the swap) is dropped, and
    /// the installed target wins.
    #[test]
    fn recovery_completes_finished_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let target = installation.packages_dir(PackageType::Module).join("pkg");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("module.json"),
            r#"{ "id": "pkg", "version": "2.0.0" }"#,
        )
        .unwrap();
        let backup = ufpm_dir(&installation)
            .join("backup")
            .join("modules")
            .join("pkg");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(
            backup.join("module.json"),
            r#"{ "id": "pkg", "version": "1.0.0" }"#,
        )
        .unwrap();

        let recovery = recover(&installation);

        assert!(recovery.restored.is_empty());
        assert!(!backup.exists());
        let manifest = std::fs::read_to_string(target.join("module.json")).unwrap();
        assert!(manifest.contains("2.0.0"));
    }
}
