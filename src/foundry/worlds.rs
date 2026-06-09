//! Scanning worlds to find which systems and modules are actually used.
//!
//! A world's *system* is declared in its `world.json`. Its enabled *modules*
//! live inside the world's `LevelDB` database (`worlds/<id>/data`) as a
//! settings document whose `key` is `core.moduleConfiguration` and whose
//! `value` is a JSON-encoded map of `module id → enabled`.
//!
//! **Safety**: opening a `LevelDB` mutates it (log compaction), and the world
//! databases belong to `FoundryVTT`. The database directory is therefore
//! always copied to a temporary location first and only the copy is opened;
//! the original is never touched. Worlds that cannot be inspected are
//! reported as unreadable so callers can treat their usage as *unknown*
//! rather than *unused* — `--prune` must never delete based on a guess.
//!
//! This module does blocking I/O throughout; call it via `spawn_blocking`.

use super::Installation;
use rusty_leveldb::LdbIterator;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What the worlds of an installation use.
#[derive(Debug, Default)]
pub struct Usage {
    /// Ids of systems used by at least one world.
    pub systems: HashSet<String>,

    /// Ids of modules enabled in at least one world.
    pub modules: HashSet<String>,

    /// Worlds that could not be (fully) inspected, with the reason. While
    /// this is non-empty the usage data is incomplete: a package that looks
    /// unused may be used by one of these worlds.
    pub unreadable: Vec<(String, String)>,

    /// How many worlds were found.
    pub worlds: usize,
}

/// Errors that can occur while scanning worlds.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The worlds directory exists but could not be listed.
    #[error("could not list {}", path.display())]
    Unlistable {
        /// The directory that could not be listed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// The subset of a `world.json` that the usage scan reads.
#[derive(Debug, Deserialize)]
struct WorldManifest {
    /// The system the world is built on.
    #[serde(default)]
    system: Option<String>,
}

/// Scans every world in the installation for the system it uses and the
/// modules it has enabled.
///
/// # Errors
///
/// Returns an [`Error`] only when the worlds directory cannot be listed at
/// all; per-world problems land in [`Usage::unreadable`] instead.
pub fn scan_usage(installation: &Installation) -> Result<Usage, Error> {
    let dir = installation.worlds_dir();
    let mut usage = Usage::default();
    if !dir.exists() {
        return Ok(usage);
    }

    let entries = std::fs::read_dir(&dir).map_err(|source| Error::Unlistable {
        path: dir.clone(),
        source,
    })?;

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        // Only directories with a world.json are worlds.
        if !path.is_dir() || !path.join("world.json").is_file() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        usage.worlds += 1;

        match read_world_system(&path) {
            Ok(Some(system)) => {
                usage.systems.insert(system);
            }
            Ok(None) => {}
            Err(reason) => {
                usage.unreadable.push((id, reason));
                continue;
            }
        }

        match read_enabled_modules(&path) {
            Ok(modules) => usage.modules.extend(modules),
            Err(reason) => usage.unreadable.push((id, reason)),
        }
    }

    Ok(usage)
}

/// Reads the system id from a world's `world.json`.
///
/// # Errors
///
/// Returns a human-readable reason when the manifest cannot be read or
/// parsed.
fn read_world_system(world: &Path) -> Result<Option<String>, String> {
    let raw = std::fs::read_to_string(world.join("world.json"))
        .map_err(|error| format!("world.json unreadable: {error}"))?;
    let manifest: WorldManifest =
        serde_json::from_str(&raw).map_err(|error| format!("world.json invalid: {error}"))?;
    Ok(manifest.system)
}

/// Reads the enabled-module set from a world's `LevelDB`, operating on a
/// temporary copy so the original database is never opened (and thus never
/// mutated).
///
/// A world without a database (never launched) has no enabled modules.
///
/// # Errors
///
/// Returns a human-readable reason when the database exists but cannot be
/// copied or read.
fn read_enabled_modules(world: &Path) -> Result<HashSet<String>, String> {
    let database = world.join("data");
    if !database.is_dir() {
        return Ok(HashSet::new());
    }

    let temp =
        tempfile::tempdir().map_err(|error| format!("could not create a temp dir: {error}"))?;
    let copy = temp.path().join("data");
    copy_dir(&database, &copy)
        .map_err(|error| format!("could not copy the world database: {error}"))?;

    let options = rusty_leveldb::Options {
        create_if_missing: false,
        ..rusty_leveldb::Options::default()
    };
    let mut db = rusty_leveldb::DB::open(&copy, options)
        .map_err(|error| format!("could not open the world database: {error}"))?;
    let mut iter = db
        .new_iter()
        .map_err(|error| format!("could not iterate the world database: {error}"))?;

    let mut modules = HashSet::new();
    while let Some((_, value)) = iter.next() {
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(&value) else {
            continue;
        };
        if document.get("key").and_then(serde_json::Value::as_str)
            != Some("core.moduleConfiguration")
        {
            continue;
        }
        let Some(configuration) = document.get("value").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(configuration) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(configuration)
        else {
            continue;
        };
        for (module, enabled) in configuration {
            if enabled.as_bool() == Some(true) {
                modules.insert(module);
            }
        }
    }

    Ok(modules)
}

/// Recursively copies a directory.
///
/// # Errors
///
/// Returns the first I/O error encountered.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Usage-scan tests against fabricated worlds with real `LevelDB`
    //! databases.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;
    use crate::foundry::discovery;
    use std::fs;

    /// Builds a valid Foundry root and returns the resolved installation.
    fn fake_foundry(root: &Path) -> Installation {
        fs::create_dir_all(root.join("Config")).unwrap();
        fs::create_dir_all(root.join("Data")).unwrap();
        discovery::resolve(Some(root)).unwrap()
    }

    /// Creates a world directory with the given system.
    fn write_world(installation: &Installation, id: &str, system: &str) -> PathBuf {
        let world = installation.worlds_dir().join(id);
        fs::create_dir_all(&world).unwrap();
        fs::write(
            world.join("world.json"),
            format!(r#"{{ "id": "{id}", "system": "{system}" }}"#),
        )
        .unwrap();
        world
    }

    /// Writes a real `LevelDB` with a `core.moduleConfiguration` document.
    fn write_settings(world: &Path, configuration: &str) {
        let document = serde_json::json!({
            "key": "core.moduleConfiguration",
            "value": configuration,
        });
        let options = rusty_leveldb::Options {
            create_if_missing: true,
            ..rusty_leveldb::Options::default()
        };
        let mut db = rusty_leveldb::DB::open(world.join("data"), options).unwrap();
        db.put(b"!settings!abc", document.to_string().as_bytes())
            .unwrap();
        db.put(b"!actors!xyz", br#"{ "name": "Strahd" }"#).unwrap();
        db.flush().unwrap();
        drop(db);
    }

    /// Systems come from world.json, modules from the settings database;
    /// disabled modules do not count.
    #[test]
    fn collects_systems_and_enabled_modules() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let world = write_world(&installation, "my-world", "pf2e");
        write_settings(&world, r#"{ "dice-so-nice": true, "old-junk": false }"#);

        let usage = scan_usage(&installation).unwrap();

        assert_eq!(usage.worlds, 1);
        assert!(usage.systems.contains("pf2e"));
        assert!(usage.modules.contains("dice-so-nice"), "{usage:?}");
        assert!(!usage.modules.contains("old-junk"));
        assert!(usage.unreadable.is_empty());
    }

    /// Scanning never modifies the original world database.
    #[test]
    fn never_touches_the_original_database() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let world = write_world(&installation, "my-world", "pf2e");
        write_settings(&world, r#"{ "dice-so-nice": true }"#);

        let snapshot: Vec<(String, u64)> = list_files(&world.join("data"));
        scan_usage(&installation).unwrap();

        assert_eq!(list_files(&world.join("data")), snapshot);
    }

    /// File names and sizes of a directory, sorted.
    fn list_files(dir: &Path) -> Vec<(String, u64)> {
        let mut files: Vec<(String, u64)> = fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.metadata().map_or(0, |metadata| metadata.len()),
                )
            })
            .collect();
        files.sort();
        files
    }

    /// A corrupt database marks the world unreadable instead of failing or
    /// silently reporting "unused".
    #[test]
    fn corrupt_databases_are_reported_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        let world = write_world(&installation, "broken", "dnd5e");
        fs::create_dir_all(world.join("data")).unwrap();
        fs::write(world.join("data").join("CURRENT"), "garbage\n").unwrap();

        let usage = scan_usage(&installation).unwrap();

        assert!(usage.systems.contains("dnd5e"));
        assert_eq!(usage.unreadable.len(), 1);
        assert_eq!(usage.unreadable[0].0, "broken");
    }

    /// A world that was never launched (no database) just has no modules.
    #[test]
    fn worlds_without_databases_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        write_world(&installation, "fresh", "coc7");

        let usage = scan_usage(&installation).unwrap();

        assert!(usage.systems.contains("coc7"));
        assert!(usage.modules.is_empty());
        assert!(usage.unreadable.is_empty());
    }

    /// Directories without a world.json are not worlds.
    #[test]
    fn ignores_non_world_directories() {
        let dir = tempfile::tempdir().unwrap();
        let installation = fake_foundry(dir.path());
        fs::create_dir_all(installation.worlds_dir().join("not-a-world")).unwrap();

        let usage = scan_usage(&installation).unwrap();

        assert_eq!(usage.worlds, 0);
    }
}
