//! Zip extraction with path-traversal protection.
//!
//! Package archives come from arbitrary third-party hosts, so every entry
//! path is validated (via the zip crate's `enclosed_name`) before anything
//! touches the filesystem: absolute paths and `..` components are rejected
//! outright instead of escaping the staging directory.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Errors that can occur while extracting a package archive.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The archive could not be opened or read.
    #[error("could not read the archive at {}", path.display())]
    Archive {
        /// The archive file.
        path: PathBuf,
        /// The underlying zip error.
        #[source]
        source: zip::result::ZipError,
    },

    /// An entry declares a path that would escape the extraction directory.
    #[error("the archive entry `{0}` has an unsafe path; refusing to extract")]
    UnsafePath(String),

    /// Plain I/O failure while writing extracted files.
    #[error("I/O failed at {}", path.display())]
    Io {
        /// The file or directory the operation failed on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Extracts `archive` into `dest` (which is created if needed).
///
/// This does blocking I/O; call it through `tokio::task::spawn_blocking`.
///
/// # Errors
///
/// Returns an [`Error`] when the archive is unreadable, contains an unsafe
/// path, or the filesystem refuses a write.
pub fn extract(archive: &Path, dest: &Path) -> Result<(), Error> {
    let file = File::open(archive).map_err(|source| Error::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|source| Error::Archive {
        path: archive.to_path_buf(),
        source,
    })?;

    for position in 0..zip.len() {
        let mut entry = zip.by_index(position).map_err(|source| Error::Archive {
            path: archive.to_path_buf(),
            source,
        })?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(Error::UnsafePath(entry.name().to_owned()));
        };
        let out = dest.join(relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|source| Error::Io { path: out, source })?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut target = File::create(&out).map_err(|source| Error::Io {
            path: out.clone(),
            source,
        })?;
        io::copy(&mut entry, &mut target).map_err(|source| Error::Io { path: out, source })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Extraction tests, including the zip-slip guard.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

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

    /// Regular archives extract with their directory structure intact.
    #[test]
    fn extracts_nested_entries() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("pkg.zip");
        build_zip(
            &archive,
            &[
                ("module.json", r#"{ "id": "pkg" }"#),
                ("scripts/main.js", "console.log('hi')"),
            ],
        );
        let dest = dir.path().join("out");

        extract(&archive, &dest).unwrap();

        assert!(dest.join("module.json").is_file());
        assert!(dest.join("scripts").join("main.js").is_file());
    }

    /// Entries that try to escape the destination are rejected.
    #[test]
    fn rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        build_zip(&archive, &[("../evil.txt", "gotcha")]);
        let dest = dir.path().join("out");

        let error = extract(&archive, &dest).unwrap_err();

        assert!(matches!(error, Error::UnsafePath(_)));
        assert!(!dir.path().join("evil.txt").exists());
    }

    /// Garbage files are reported as archive errors.
    #[test]
    fn rejects_non_zip_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("not.zip");
        std::fs::write(&archive, "definitely not a zip").unwrap();

        let error = extract(&archive, &dir.path().join("out")).unwrap_err();

        assert!(matches!(error, Error::Archive { .. }));
    }
}
