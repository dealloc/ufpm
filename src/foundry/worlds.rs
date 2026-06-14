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
use tracing::{debug, trace};

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

    /// The requested world does not exist.
    #[error("world '{id}' was not found")]
    WorldNotFound {
        /// The world id that was requested.
        id: String,
    },
}

/// The subset of a `world.json` that the usage scan reads.
#[derive(Debug, Deserialize)]
struct WorldManifest {
    /// The system the world is built on.
    #[serde(default)]
    system: Option<String>,
}

/// Scans a single world by id and returns what it uses.
///
/// # Errors
///
/// Returns [`Error::WorldNotFound`] when no `worlds/<id>/world.json` exists.
/// Per-world read problems (unreadable database, etc.) are recorded in
/// [`Usage::unreadable`] rather than returned as hard errors.
pub fn scan_world(installation: &Installation, world_id: &str) -> Result<Usage, Error> {
    let path = installation.worlds_dir().join(world_id);
    debug!(world = world_id, path = %path.display(), "scanning single world");
    if !path.join("world.json").is_file() {
        debug!(world = world_id, "world.json not found");
        return Err(Error::WorldNotFound {
            id: world_id.to_owned(),
        });
    }

    let mut usage = Usage {
        worlds: 1,
        ..Usage::default()
    };

    match read_world_system(&path) {
        Ok(Some(system)) => {
            debug!(world = world_id, system, "found system");
            usage.systems.insert(system);
        }
        Ok(None) => {
            debug!(world = world_id, "no system declared in world.json");
        }
        Err(reason) => {
            debug!(world = world_id, reason, "failed to read world system");
            usage.unreadable.push((world_id.to_owned(), reason));
            return Ok(usage);
        }
    }

    match read_enabled_modules(&path) {
        Ok(modules) => {
            debug!(world = world_id, count = modules.len(), "found enabled modules");
            usage.modules.extend(modules);
        }
        Err(reason) => {
            debug!(world = world_id, reason, "failed to read enabled modules");
            usage.unreadable.push((world_id.to_owned(), reason));
        }
    }

    Ok(usage)
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
    debug!(dir = %dir.display(), "scanning all worlds");
    let mut usage = Usage::default();
    if !dir.exists() {
        debug!("worlds directory does not exist; skipping");
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
            trace!(path = %path.display(), "skipping non-world entry");
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        debug!(world = %id, "scanning world");
        usage.worlds += 1;

        match read_world_system(&path) {
            Ok(Some(system)) => {
                debug!(world = %id, system, "found system");
                usage.systems.insert(system);
            }
            Ok(None) => {
                debug!(world = %id, "no system declared in world.json");
            }
            Err(reason) => {
                debug!(world = %id, reason, "failed to read world system");
                usage.unreadable.push((id, reason));
                continue;
            }
        }

        match read_enabled_modules(&path) {
            Ok(modules) => {
                debug!(world = %id, count = modules.len(), "found enabled modules");
                usage.modules.extend(modules);
            }
            Err(reason) => {
                debug!(world = %id, reason, "failed to read enabled modules");
                usage.unreadable.push((id, reason));
            }
        }
    }

    debug!(worlds = usage.worlds, unreadable = usage.unreadable.len(), "world scan complete");
    Ok(usage)
}

/// Reads the system id from a world's `world.json`.
///
/// # Errors
///
/// Returns a human-readable reason when the manifest cannot be read or
/// parsed.
fn read_world_system(world: &Path) -> Result<Option<String>, String> {
    let manifest_path = world.join("world.json");
    trace!(path = %manifest_path.display(), "reading world.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("world.json unreadable: {error}"))?;
    let manifest: WorldManifest =
        serde_json::from_str(&raw).map_err(|error| format!("world.json invalid: {error}"))?;
    trace!(system = ?manifest.system, "parsed world.json");
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
    let database = world.join("data").join("settings");
    trace!(path = %database.display(), "checking for settings database");
    if !database.is_dir() {
        debug!(path = %database.display(), "settings database not present; world has no enabled modules");
        return Ok(HashSet::new());
    }

    let temp =
        tempfile::tempdir().map_err(|error| format!("could not create a temp dir: {error}"))?;
    let copy = temp.path().join("data");
    trace!(src = %database.display(), dst = %copy.display(), "copying settings database to temp dir");
    copy_dir(&database, &copy)
        .map_err(|error| format!("could not copy the world database: {error}"))?;

    let options = rusty_leveldb::Options {
        create_if_missing: false,
        ..rusty_leveldb::Options::default()
    };
    debug!(path = %copy.display(), "opening settings database copy");
    let mut db = rusty_leveldb::DB::open(&copy, options)
        .map_err(|error| format!("could not open the world database: {error}"))?;
    let mut iter = db
        .new_iter()
        .map_err(|error| format!("could not iterate the world database: {error}"))?;

    let mut modules = HashSet::new();
    while let Some((key, value)) = iter.next() {
        trace!(key = %String::from_utf8_lossy(&key), "iterating settings record");
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(&value) else {
            trace!(key = %String::from_utf8_lossy(&key), "skipping non-JSON record");
            continue;
        };
        let doc_key = document.get("key").and_then(serde_json::Value::as_str);
        trace!(doc_key, "parsed document key");
        if doc_key != Some("core.moduleConfiguration") {
            continue;
        }
        let Some(configuration) = document.get("value").and_then(serde_json::Value::as_str) else {
            debug!("core.moduleConfiguration has no string value field; skipping");
            continue;
        };
        trace!(raw = configuration, "raw moduleConfiguration value");
        let Ok(configuration) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(configuration)
        else {
            debug!("could not parse moduleConfiguration as a JSON object; skipping");
            continue;
        };
        for (module, enabled) in configuration {
            trace!(module, enabled = %enabled, "module configuration entry");
            if enabled.as_bool() == Some(true) {
                debug!(module, "module is enabled");
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
        let mut db = rusty_leveldb::DB::open(world.join("data").join("settings"), options).unwrap();
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

        let snapshot: Vec<(String, u64)> = list_files(&world.join("data").join("settings"));
        scan_usage(&installation).unwrap();

        assert_eq!(list_files(&world.join("data").join("settings")), snapshot);
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
        fs::create_dir_all(world.join("data").join("settings")).unwrap();
        fs::write(world.join("data").join("settings").join("CURRENT"), "garbage\n").unwrap();

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
