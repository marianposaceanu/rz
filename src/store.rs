use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::DateTime;

use crate::{
    model::Snapshot,
    storage::{self, Usage},
};

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    pub root: PathBuf,
}

#[derive(Debug)]
pub struct SnapshotEntry {
    pub path: PathBuf,
    state: Result<Snapshot, String>,
}

impl SnapshotEntry {
    pub fn saved(path: PathBuf, snapshot: Snapshot) -> Self {
        Self {
            path,
            state: Ok(snapshot),
        }
    }

    pub fn snapshot(&self) -> Result<&Snapshot> {
        self.state
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub fn id(&self) -> &str {
        self.state
            .as_ref()
            .map(|snapshot| snapshot.id.as_str())
            .unwrap_or_else(|_| {
                self.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("(unknown)")
            })
    }
}

impl SnapshotStore {
    pub fn entries(&self) -> Result<Vec<SnapshotEntry>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("could not list snapshot storage"),
        };
        let mut snapshots = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.')
                || !entry.file_type()?.is_dir()
                || !entry.path().join("state.json").is_file()
            {
                continue;
            }
            let path = entry.path();
            let state = Self::read(&path).map_err(|error| format!("{error:#}"));
            snapshots.push(SnapshotEntry { path, state });
        }
        snapshots.sort_by_cached_key(|entry| {
            (
                entry
                    .state
                    .as_ref()
                    .ok()
                    .and_then(|snapshot| DateTime::parse_from_rfc3339(&snapshot.saved_at).ok()),
                entry.id().to_owned(),
            )
        });
        Ok(snapshots)
    }

    pub fn read(directory: &Path) -> Result<Snapshot> {
        let path = directory.join("state.json");
        let content =
            fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_slice(&content)
            .with_context(|| format!("invalid snapshot state in {}", path.display()))
    }

    pub fn resolve(&self, selector: Option<&str>) -> Result<SnapshotEntry> {
        let mut entries = self.entries()?;
        if entries.is_empty() {
            bail!("no saved sessions; create one with rz --save NAME");
        }
        let Some(selector) = selector else {
            return Ok(entries.pop().expect("non-empty snapshots"));
        };
        let exact = entries
            .iter()
            .position(|entry| entry.path.file_name().is_some_and(|name| name == selector));
        let matching = exact.or_else(|| {
            entries.iter().rposition(|entry| {
                entry.snapshot().is_ok_and(|snapshot| {
                    snapshot.name == selector || snapshot.timestamp == selector
                })
            })
        });
        matching
            .map(|index| entries.swap_remove(index))
            .ok_or_else(|| anyhow::anyhow!("no saved session matches {selector:?}; run rz --list"))
    }

    pub fn usage(&self) -> Result<Usage> {
        if !self.root.try_exists()? {
            return Ok(Usage::default());
        }
        storage::usage(&self.root)
    }

    pub fn latest_id(&self, name: &str) -> Result<Option<String>> {
        Ok(self.entries()?.iter().rev().find_map(|entry| {
            entry
                .snapshot()
                .ok()
                .filter(|snapshot| snapshot.name == name)
                .map(|snapshot| snapshot.id.clone())
        }))
    }

    pub fn remove(&self, directory: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(directory)?;
        if directory.parent() != Some(self.root.as_path())
            || directory.file_name().is_none()
            || !metadata.is_dir()
            || metadata.is_symlink()
        {
            bail!("refusing to remove unsafe snapshot path {directory:?}");
        }
        fs::remove_dir_all(directory).context("could not remove snapshot")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn write(root: &Path, id: &str, saved_at: &str) {
        storage::write_json(
            &root.join(id).join("state.json"),
            &serde_json::json!({
                "version": 1, "id": id, "name": "work", "timestamp": id, "saved_at": saved_at,
            }),
        )
        .unwrap();
    }

    #[test]
    fn resolves_latest_name_timestamp_and_exact_id_across_timezones() {
        let temp = TempDir::new().unwrap();
        let store = SnapshotStore {
            root: temp.path().to_owned(),
        };
        write(temp.path(), "lexically_later", "2026-11-22T14:00:00+03:00");
        write(temp.path(), "actually_newer", "2026-11-22T12:00:00Z");
        assert_eq!(store.resolve(None).unwrap().id(), "actually_newer");
        assert_eq!(store.resolve(Some("work")).unwrap().id(), "actually_newer");
        assert_eq!(
            store.latest_id("work").unwrap().as_deref(),
            Some("actually_newer")
        );
        assert_eq!(store.latest_id("lexically_later").unwrap(), None);
        assert_eq!(
            store.resolve(Some("lexically_later")).unwrap().id(),
            "lexically_later"
        );
        assert!(store.resolve(Some("missing")).is_err());
    }

    #[test]
    fn reports_corrupt_snapshots_and_ignores_staging_and_symlinks() {
        let temp = TempDir::new().unwrap();
        let store = SnapshotStore {
            root: temp.path().join("snapshots"),
        };
        assert!(store.entries().unwrap().is_empty());
        assert_eq!(store.usage().unwrap().allocated, 0);
        write(&store.root, "valid", "2026-11-22T12:00:00Z");
        write(&store.root, ".staging", "2026-11-23T12:00:00Z");
        symlink(store.root.join("valid"), store.root.join("linked")).unwrap();
        fs::create_dir(store.root.join("broken")).unwrap();
        fs::write(store.root.join("broken/state.json"), "unfinished").unwrap();
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries[0]
                .snapshot()
                .unwrap_err()
                .to_string()
                .contains("broken/state.json")
        );
        assert_eq!(store.resolve(None).unwrap().id(), "valid");
    }

    #[test]
    fn removal_is_limited_to_direct_child_directories() {
        let temp = TempDir::new().unwrap();
        let store = SnapshotStore {
            root: temp.path().join("snapshots"),
        };
        write(&store.root, "old", "2026-11-20T12:00:00Z");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        for path in [
            store.root.clone(),
            store.root.join(".."),
            store.root.join("../outside"),
        ] {
            assert!(store.remove(&path).is_err(), "{}", path.display());
        }
        store.remove(&store.root.join("old")).unwrap();
        assert!(outside.is_dir());
        assert!(store.entries().unwrap().is_empty());
    }
}
