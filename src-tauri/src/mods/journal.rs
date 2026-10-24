//! Persistent install journal and backups.
//!
//! Only paths recorded here are ever removed by this client. Every write is atomic (temporary file +
//! rename), and a corrupt journal is parked instead of being silently ignored.

use super::model::{normalize_entry, Recipe};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledFile {
    pub root: String,
    #[serde(default)]
    pub game_slug: Option<String>,
    pub rel: String,
    /// Absolute destination at install time. Removal double-checks it against `roots`.
    #[serde(default)]
    pub abs: String,
    pub sha256: String,
    pub size: u64,
    /// Relative path under `mods/parked/<record-key>/` while the mod is disabled.
    #[serde(default)]
    pub parked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub root: String,
    #[serde(default)]
    pub game_slug: Option<String>,
    pub rel: String,
    #[serde(default)]
    pub abs: String,
    /// Relative path under `mods/backups/<record-key>/`.
    pub backup: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTarget {
    pub root: String,
    #[serde(default)]
    pub game_slug: Option<String>,
    pub rel: String,
    #[serde(default)]
    pub abs: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub key: String,
    pub mod_id: String,
    pub slug: String,
    pub title: String,
    pub release_id: String,
    pub version: String,
    #[serde(default)]
    pub game_slugs: Vec<String>,
    #[serde(default)]
    pub primary_game_slug: Option<String>,
    #[serde(default)]
    pub installation_ids: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub installed_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub files: Vec<InstalledFile>,
    #[serde(default)]
    pub backups: Vec<Backup>,
    #[serde(default)]
    pub runtime_data: Vec<StoredTarget>,
    /// All destination roots that were trusted when this record was written. Uninstall only removes
    /// files below one of them.
    #[serde(default)]
    pub roots: Vec<String>,
    pub recipe: Recipe,
    #[serde(default)]
    pub loaders: Vec<String>,
    /// Explicit, per-mashup game folders (game slug -> absolute game root). This is used for a
    /// separate, compatible copy of a game without touching the Steam installation.
    #[serde(default)]
    pub game_paths: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

impl Record {
    pub fn key(mod_id: &str) -> String {
        format!("mod:{mod_id}")
    }

    pub fn folder(&self) -> &str {
        &self.slug
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    #[serde(default)]
    pub records: Vec<Record>,
}

impl Default for Journal {
    fn default() -> Self {
        Journal { version: VERSION, records: Vec::new() }
    }
}

impl Journal {
    pub fn load(dir: &Path) -> Journal {
        let path = dir.join("journal.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Journal::default(),
            Err(e) => {
                eprintln!("[mods] journal unreadable: {e}");
                return Journal::default();
            }
        };
        match serde_json::from_str::<Journal>(&text) {
            Ok(mut journal) if journal.version == VERSION => {
                journal.records.retain(valid_record);
                journal
            }
            _ => {
                let backup = dir.join(format!("journal.json.corrupt-{}", now()));
                let _ = std::fs::rename(&path, &backup);
                eprintln!("[mods] journal corrupt, parked at {}", backup.display());
                Journal::default()
            }
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        let path = dir.join("journal.json");
        let temporary = dir.join(format!("journal.json.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&temporary, text).map_err(|e| format!("Could not write the journal: {e}"))?;
        set_private(&temporary)?;
        if let Err(e) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("Could not replace the journal: {e}"));
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.key == key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Record> {
        self.records.iter_mut().find(|r| r.key == key)
    }

    pub fn remove(&mut self, key: &str) -> Option<Record> {
        let index = self.records.iter().position(|r| r.key == key)?;
        Some(self.records.remove(index))
    }

    pub fn upsert(&mut self, record: Record) {
        if let Some(existing) = self.get_mut(&record.key) {
            *existing = record;
        } else {
            self.records.push(record);
        }
    }
}

fn valid_record(record: &Record) -> bool {
    !record.key.is_empty()
        && record.key.starts_with("mod:")
        && !record.slug.is_empty()
        && record.slug.len() <= 80
        && record.slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && record.files.iter().all(|f| normalize_entry(&f.rel).as_deref() == Some(f.rel.as_str()))
        && record.backups.iter().all(|b| normalize_entry(&b.rel).as_deref() == Some(b.rel.as_str()) && normalize_entry(&b.backup).is_some())
}

pub fn set_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Could not protect {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn backup_dir(mods_dir: &Path, key: &str) -> PathBuf {
    mods_dir.join("backups").join(safe_key(key))
}

pub fn parked_dir(mods_dir: &Path, key: &str) -> PathBuf {
    mods_dir.join("parked").join(safe_key(key))
}

pub(crate) fn safe_key(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("melty-journal-{tag}-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn journal_round_trip_and_corrupt_parking() {
        let dir = tmp("rt");
        let mut journal = Journal::default();
        assert!(journal.records.is_empty());
        journal.records.push(Record {
            key: Record::key("abc"),
            mod_id: "abc".into(),
            slug: "my-mod".into(),
            title: "My mod".into(),
            release_id: "r1".into(),
            version: "1".into(),
            game_slugs: vec![],
            primary_game_slug: None,
            installation_ids: BTreeMap::new(),
            enabled: true,
            installed_at: 1,
            updated_at: 2,
            files: vec![InstalledFile { root: "game".into(), game_slug: None, rel: "Mod/a.dll".into(), abs: "/tmp/Mod/a.dll".into(), sha256: "a".repeat(64), size: 3, parked: None }],
            backups: vec![],
            runtime_data: vec![],
            roots: vec![],
            recipe: Recipe::default(),
            loaders: vec![],
            game_paths: BTreeMap::new(),
        });
        journal.save(&dir).unwrap();
        assert_eq!(Journal::load(&dir).records.len(), 1);
        std::fs::write(dir.join("journal.json"), "{not json").unwrap();
        assert!(Journal::load(&dir).records.is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("corrupt")));
    }

    #[test]
    fn unsafe_records_are_dropped() {
        let dir = tmp("bad");
        let mut journal = Journal::default();
        journal.records.push(Record {
            key: Record::key("abc"),
            mod_id: "abc".into(),
            slug: "my-mod".into(),
            title: "My mod".into(),
            release_id: "r1".into(),
            version: "1".into(),
            game_slugs: vec![],
            primary_game_slug: None,
            installation_ids: BTreeMap::new(),
            enabled: true,
            installed_at: 1,
            updated_at: 2,
            files: vec![InstalledFile { root: "game".into(), game_slug: None, rel: "../evil".into(), abs: "/tmp/evil".into(), sha256: "a".repeat(64), size: 3, parked: None }],
            backups: vec![],
            runtime_data: vec![],
            roots: vec![],
            recipe: Recipe::default(),
            loaders: vec![],
            game_paths: BTreeMap::new(),
        });
        journal.save(&dir).unwrap();
        assert!(Journal::load(&dir).records.is_empty());
    }
}
