//! Eine ausdrücklich gewählte, kompatible Spielkopie für ein einzelnes Mashup.
//!
//! Manche Mashups (z. B. EldenCraft) binden sich an einen bestimmten Build des Hauptspiels. Eine
//! Spielaktualisierung macht ihre Bridge dann unbrauchbar. Statt die Steam-Installation zu ändern,
//! kann der Nutzer pro Mashup einen anderen, vollständigen Spielordner auswählen. Der Client prüft
//! dessen Version, kopiert nur die Dateien des Mashups hinein und startet über dasselbe Proton-Präfix
//! und dieselbe Steam-App-ID wie zuvor.
//!
//! Die Kopie bleibt Eigentum des Nutzers. Wir merken uns deshalb genau, welche Dateien wir dort
//! abgelegt haben, überschreiben keine fremden Dateien und entfernen beim Aufräumen nur Dateien,
//! deren Prüfsumme noch zu unserem Eintrag passt.

use super::journal::{self, InstalledFile, Record};
use super::model;
use super::paths;
use super::version;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Ergebnis einer geprüften Spielkopie.
#[derive(Debug, Clone)]
pub struct CopyInfo {
    pub root: PathBuf,
    pub version: Option<version::GameVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    root: String,
    #[serde(default)]
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    rel: String,
    sha256: String,
}

/// Das primäre Spiel eines Mashups. Ältere Journale haben eventuell kein `primaryGameSlug`.
pub fn primary_slug(record: &Record) -> Option<&str> {
    record
        .primary_game_slug
        .as_deref()
        .or_else(|| record.recipe.games.iter().find(|game| game.role == "primary").map(|game| game.slug.as_str()))
        .or_else(|| record.recipe.games.first().map(|game| game.slug.as_str()))
}

/// Kann für dieses Mashup überhaupt eine Spielkopie verwendet werden? Das geht nur, wenn das
/// Rezept ein Programm aus `{game}` startet; bei einem Steam-URI-Start gibt es keinen sinnvollen
/// Ordner, den der Client ersetzen könnte.
pub fn available(record: &Record) -> bool {
    primary_slug(record).is_some_and(|slug| {
        expected_game_subpath(record, slug).is_some() && requirement(record, slug).is_some()
    })
}

/// Die Startdatei des Hauptspiels innerhalb von `{game}`.
///
/// Viele Rezepte starten nicht die `.exe` direkt, sondern einen Launcher (me3, ModEngine usw.)
/// und übergeben den Spielpfad als Argument, z. B. `--exe {game}/Game/eldenring.exe`. Für die
/// Versionsprüfung und die Spielkopie muss dieser Pfad genauso gefunden werden wie ein direkter
/// Startpfad.
pub fn expected_game_subpath(record: &Record, primary: &str) -> Option<String> {
    let launch = record.recipe.launch.as_ref()?;
    if launch.kind != "exe" {
        return None;
    }
    let belongs_to_primary = |target: &model::Target| {
        target.root == "game"
            && target
                .game_slug
                .as_deref()
                .map_or(true, |slug| slug == primary)
    };

    if let Some(target) = launch.path.as_deref().and_then(model::parse_target) {
        if belongs_to_primary(&target) && !target.subpath.is_empty() {
            return Some(target.subpath);
        }
    }

    // Launcher-Rezepte nennen das Spiel oft als eigenes Argument. Nur ausführbare Dateien
    // berücksichtigen, damit z. B. ein Datenordner nicht als Startdatei gilt.
    launch.args.iter().find_map(|argument| {
        let target = model::parse_target(argument)?;
        if !belongs_to_primary(&target) || target.subpath.is_empty() {
            return None;
        }
        target
            .subpath
            .to_ascii_lowercase()
            .ends_with(".exe")
            .then_some(target.subpath)
    })
}

fn requirement<'a>(record: &'a Record, primary: &str) -> Option<&'a str> {
    record
        .recipe
        .games
        .iter()
        .find(|game| game.slug == primary)
        .and_then(|game| game.version.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Prüft einen ausgewählten Ordner, ohne etwas zu verändern.
///
/// Der Nutzer darf sowohl den eigentlichen Spielordner (z. B. `ELDEN RING`) als auch den
/// Unterordner mit der `.exe` (z. B. `ELDEN RING/Game`) auswählen. In beiden Fällen wird der
/// gemeinsame Wurzelordner ermittelt, den `{game}` im Rezept erwartet.
pub fn validate(
    record: &Record,
    selected: &Path,
    current_root: &Path,
    game_name: &str,
) -> Result<CopyInfo, String> {
    let primary = primary_slug(record)
        .ok_or_else(|| "Dieses Mashup nennt kein Hauptspiel; eine Spielkopie ist nicht möglich.".to_string())?;
    let subpath = expected_game_subpath(record, primary).ok_or_else(|| {
        "Dieses Mashup startet kein Programm aus dem Spielordner; eine Spielkopie ist dafür nicht nötig."
            .to_string()
    })?;

    let selected = std::fs::canonicalize(selected).map_err(|error| {
        format!(
            "Der gewählte Spielordner konnte nicht gelesen werden ({}): {error}",
            selected.display()
        )
    })?;
    if !selected.is_dir() {
        return Err(format!("Der gewählte Pfad ist kein Ordner: {}", selected.display()));
    }

    let mut probe = selected.as_path();
    let mut root = None;
    for _ in 0..4 {
        let candidate = paths::safe_join(probe, &subpath)?;
        if candidate.is_file() {
            root = Some(probe.to_path_buf());
            break;
        }
        let Some(parent) = probe.parent() else { break };
        if parent == probe {
            break;
        }
        probe = parent;
    }
    let root = root.ok_or_else(|| {
        format!(
            "Im gewählten Ordner fehlt die erwartete Startdatei „{subpath}“ für {game_name}. Wähle den Spielordner, der „{subpath}“ enthält."
        )
    })?;

    let current = std::fs::canonicalize(current_root).unwrap_or_else(|_| current_root.to_path_buf());
    if root == current {
        return Err(
            "Das ist bereits die Steam-Installation. Eine Spielkopie ist nur für einen anderen Ordner nötig."
                .to_string(),
        );
    }
    if paths::is_within(&current, &root) || paths::is_within(&root, &current) {
        return Err(
            "Die Spielkopie muss außerhalb der Steam-Installation liegen, damit Steam sie bei einem Update nicht verändert."
                .to_string(),
        );
    }

    let executable = paths::safe_join(&root, &subpath)?;
    let actual = version::read_file_version(&executable);
    if let Some(required) = requirement(record, primary) {
        let Some(actual) = actual else {
            return Err(format!(
                "Die Version von {} konnte nicht gelesen werden. Ist das eine vollständige Windows-Kopie des Spiels?",
                executable.display()
            ));
        };
        if version::matches_requirement(required, actual) == Some(false) {
            return Err(format!(
                "Der gewählte Ordner enthält {game_name} {actual}, aber {} unterstützt nur {}. Wähle eine Kopie in diesem Versionsbereich.",
                record.title,
                version::requirement_label(required)
            ));
        }
    }

    Ok(CopyInfo { root, version: actual })
}

/// Kopiert die Dateien des Mashups, die im Spielwurzelordner liegen, in die gewählte Spielkopie.
///
/// Fremde, abweichende Dateien werden nicht überschrieben. Nur Dateien, die laut Manifest von uns
/// stammen, dürfen aktualisiert oder später wieder entfernt werden.
pub fn apply(
    record: &Record,
    source_root: &Path,
    root: &Path,
    mods_dir: &Path,
    primary: &str,
) -> Result<(), String> {
    let root_text = root.to_string_lossy().into_owned();
    let previous = load_manifest(mods_dir, &record.key);

    let files: Vec<&InstalledFile> = record
        .files
        .iter()
        .filter(|file| {
            file.root == "game" && file.game_slug.as_deref().map_or(true, |slug| slug == primary)
        })
        .collect();

    struct Pending {
        rel: String,
        source: PathBuf,
        target: PathBuf,
        sha256: String,
    }

    let mut pending = Vec::new();
    for file in files {
        // Lesen darf durch symbolische Links führen (z. B. eine Steam-Bibliothek auf einer anderen
        // Platte). Geschrieben wird nur über `safe_join`, das Links im Ziel ablehnt.
        let rel = model::normalize_entry(&file.rel)
            .ok_or_else(|| format!("Ungültiger Mashup-Pfad im Journal: {}", file.rel))?;
        let source = source_root.join(&rel);
        if !source.is_file() {
            return Err(format!(
                "Die Mashup-Datei {} fehlt in der Steam-Installation. Installiere das Mashup neu.",
                source.display()
            ));
        }
        let target = paths::safe_join(root, &file.rel)?;
        let sha256 = super::sha256_file(&source)?;

        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if !meta.is_file() {
                return Err(format!(
                    "In der Spielkopie liegt unter „{}“ bereits etwas anderes als eine Datei. Bitte wähle eine saubere Kopie des Spiels.",
                    file.rel
                ));
            }
            let target_hash = super::sha256_file(&target)?;
            if target_hash != sha256 {
                let ours = previous.as_ref().is_some_and(|manifest| {
                    manifest.root == root_text
                        && manifest
                            .files
                            .iter()
                            .any(|entry| entry.rel == file.rel && entry.sha256 == target_hash)
                });
                if !ours {
                    return Err(format!(
                        "Die Spielkopie enthält bereits eine andere Datei unter „{}“. Damit würden fremde Mods überschrieben. Bitte wähle eine saubere Kopie des Spiels oder entferne diese Datei dort.",
                        file.rel
                    ));
                }
            }
        }

        pending.push(Pending { rel: file.rel.clone(), source, target, sha256 });
    }

    // Erst alle Dateien neben ihr Ziel kopieren. So bleibt die Kopie bei einem Fehler weitgehend
    // unverändert und es gibt keine halb geschriebenen Dateien.
    let mut temporary = Vec::new();
    for entry in &pending {
        let Some(parent) = entry.target.parent() else {
            cleanup_temporary(&temporary);
            return Err(format!("Ungültiges Ziel für {}:", entry.rel));
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            cleanup_temporary(&temporary);
            return Err(format!("Konnte {} nicht anlegen: {error}", parent.display()));
        }
        let name = entry.target.file_name().and_then(|name| name.to_str()).unwrap_or("file");
        let temp = parent.join(format!(".{name}.melty-copy-{}", std::process::id()));
        if let Err(error) = std::fs::copy(&entry.source, &temp) {
            cleanup_temporary(&temporary);
            return Err(format!(
                "Konnte {} nicht in die Spielkopie kopieren: {error}",
                entry.source.display()
            ));
        }
        temporary.push((temp, entry.target.clone()));
    }

    let manifest = Manifest {
        root: root_text.clone(),
        files: pending
            .iter()
            .map(|entry| ManifestFile { rel: entry.rel.clone(), sha256: entry.sha256.clone() })
            .collect(),
    };
    if let Err(error) = save_manifest(mods_dir, &record.key, &manifest) {
        cleanup_temporary(&temporary);
        return Err(error);
    }

    for (temp, target) in &temporary {
        if let Err(error) = std::fs::rename(temp, target) {
            cleanup_temporary(&temporary);
            return Err(format!("Konnte {} nicht ersetzen: {error}", target.display()));
        }
    }

    // Dateien, die in einer früheren Version des Mashups in derselben Kopie lagen, werden entfernt,
    // sofern der Nutzer sie nicht verändert hat. Bei einem Wechsel des Ordners wird die alte Kopie
    // auf dieselbe vorsichtige Weise bereinigt.
    if let Some(previous) = previous {
        if previous.root == root_text {
            let current: HashSet<&str> = pending.iter().map(|entry| entry.rel.as_str()).collect();
            for old in &previous.files {
                if !current.contains(old.rel.as_str()) {
                    remove_if_unchanged(root, &old.rel, &old.sha256);
                }
            }
        } else {
            cleanup_manifest(&previous);
        }
    }

    Ok(())
}

/// Entfernt die von uns in die Spielkopie gelegten Dateien, sofern sie noch unverändert sind.
pub fn cleanup(mods_dir: &Path, record_key: &str) -> Result<(), String> {
    let Some(manifest) = load_manifest(mods_dir, record_key) else {
        return Ok(());
    };
    cleanup_manifest(&manifest);
    remove_manifest(mods_dir, record_key)
}

fn cleanup_manifest(manifest: &Manifest) {
    let root = PathBuf::from(&manifest.root);
    for file in &manifest.files {
        remove_if_unchanged(&root, &file.rel, &file.sha256);
    }
}

fn remove_if_unchanged(root: &Path, rel: &str, sha256: &str) {
    // Das Manifest kann von einem früheren Lauf stammen. Wenn der Ordner inzwischen durch einen
    // Link ersetzt wurde, niemals durch ihn hindurch löschen.
    let Ok(meta) = std::fs::symlink_metadata(root) else { return };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return;
    }
    let Ok(target) = paths::safe_join(root, rel) else { return };
    if !target.is_file() {
        return;
    }
    if super::sha256_file(&target).ok().as_deref() != Some(sha256) {
        return;
    }
    if std::fs::remove_file(&target).is_ok() {
        prune_empty_dirs(&target, root);
    }
}

fn prune_empty_dirs(file: &Path, root: &Path) {
    let mut current = file.parent();
    while let Some(dir) = current {
        if dir == root || !paths::is_within(root, dir) {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
}

fn cleanup_temporary(temporary: &[(PathBuf, PathBuf)]) {
    for (temp, _) in temporary {
        let _ = std::fs::remove_file(temp);
    }
}

fn manifest_path(mods_dir: &Path, record_key: &str) -> PathBuf {
    mods_dir
        .join("gamecopies")
        .join(format!("{}.json", journal::safe_key(record_key)))
}

fn load_manifest(mods_dir: &Path, record_key: &str) -> Option<Manifest> {
    let path = manifest_path(mods_dir, record_key);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            eprintln!("[mods] Spielkopie-Manifest unlesbar ({}): {error}", path.display());
            None
        }
    }
}

fn save_manifest(mods_dir: &Path, record_key: &str, manifest: &Manifest) -> Result<(), String> {
    let path = manifest_path(mods_dir, record_key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Konnte den Spielkopie-Ordner nicht anlegen: {error}"))?;
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, text)
        .map_err(|error| format!("Konnte das Spielkopie-Manifest nicht schreiben: {error}"))?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Konnte das Spielkopie-Manifest nicht ersetzen: {error}"));
    }
    Ok(())
}

fn remove_manifest(mods_dir: &Path, record_key: &str) -> Result<(), String> {
    let path = manifest_path(mods_dir, record_key);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Konnte das Spielkopie-Manifest nicht entfernen: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::journal::InstalledFile;
    use crate::mods::model::{Launch, Recipe, RecipeGame};
    use std::collections::BTreeMap;

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "melty-gamecopy-{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(requirement: Option<&str>, files: Vec<InstalledFile>) -> Record {
        Record {
            key: "mod:test".into(),
            mod_id: "test".into(),
            slug: "test".into(),
            title: "Test Mashup".into(),
            release_id: "release".into(),
            version: "1.0.0".into(),
            game_slugs: vec!["game".into()],
            primary_game_slug: Some("game".into()),
            installation_ids: BTreeMap::new(),
            enabled: true,
            installed_at: 0,
            updated_at: 0,
            files,
            backups: vec![],
            runtime_data: vec![],
            roots: vec![],
            recipe: Recipe {
                games: vec![RecipeGame {
                    slug: "game".into(),
                    role: "primary".into(),
                    version: requirement.map(str::to_string),
                    ..Default::default()
                }],
                launch: Some(Launch {
                    kind: "exe".into(),
                    path: Some("{game}/Game/game.exe".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            loaders: vec![],
            game_paths: BTreeMap::new(),
        }
    }

    fn installed_file(source_root: &Path, rel: &str, content: &[u8]) -> InstalledFile {
        let path = source_root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        InstalledFile {
            root: "game".into(),
            game_slug: Some("game".into()),
            rel: rel.into(),
            abs: path.to_string_lossy().into_owned(),
            sha256: crate::mods::sha256_file(&path).unwrap(),
            size: content.len() as u64,
            parked: None,
        }
    }

    fn synthetic_pe(version: version::GameVersion) -> Vec<u8> {
        let mut file = vec![0u8; 0x400];
        file[0..2].copy_from_slice(b"MZ");
        file[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let pe = 0x40usize;
        file[pe..pe + 4].copy_from_slice(b"PE\0\0");
        file[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
        file[pe + 20..pe + 22].copy_from_slice(&0xf0u16.to_le_bytes());
        let optional = pe + 24;
        file[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        let resource = optional + 112 + 16;
        file[resource..resource + 4].copy_from_slice(&0x1000u32.to_le_bytes());
        file[resource + 4..resource + 8].copy_from_slice(&0x200u32.to_le_bytes());
        let section = optional + 0xf0;
        file[section..section + 5].copy_from_slice(b".rsrc");
        file[section + 8..section + 12].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        file[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());
        let raw = 0x200usize;
        file[raw..raw + 4].copy_from_slice(&[0xbd, 0x04, 0xef, 0xfe]);
        file[raw + 4..raw + 8].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        let ms = ((version.0[0] as u32) << 16) | version.0[1] as u32;
        let ls = ((version.0[2] as u32) << 16) | version.0[3] as u32;
        file[raw + 8..raw + 12].copy_from_slice(&ms.to_le_bytes());
        file[raw + 12..raw + 16].copy_from_slice(&ls.to_le_bytes());
        file
    }

    #[test]
    fn only_offers_copies_for_versioned_game_launches() {
        assert!(!available(&record(None, vec![])));
        assert!(available(&record(Some(">=1.0.0"), vec![])));
        let mut uri = record(Some(">=1.0.0"), vec![]);
        uri.recipe.launch = Some(Launch {
            kind: "uri".into(),
            uri: Some("steam://rungameid/1".into()),
            ..Default::default()
        });
        assert!(!available(&uri));
    }

    #[test]
    fn finds_the_game_behind_a_launcher_argument() {
        let mut record = record(Some(">=2.7.1.0 <=2.7.1.1"), vec![]);
        record.recipe.launch = Some(Launch {
            kind: "exe".into(),
            path: Some("{managed}/me3/bin/me3.exe".into()),
            args: vec!["launch".into(), "--exe".into(), "{game}/Game/game.exe".into()],
            ..Default::default()
        });

        assert_eq!(expected_game_subpath(&record, "game").as_deref(), Some("Game/game.exe"));
        assert!(available(&record));
    }

    #[test]
    fn finds_game_root_and_rejects_steam_install() {
        let base = temp_dir("root");
        let current = base.join("steam").join("ELDEN RING");
        std::fs::create_dir_all(current.join("Game")).unwrap();
        std::fs::write(current.join("Game/game.exe"), b"steam").unwrap();
        let copy = base.join("copy").join("ELDEN RING");
        std::fs::create_dir_all(copy.join("Game")).unwrap();
        std::fs::write(copy.join("Game/game.exe"), b"copy").unwrap();

        let record = record(None, vec![]);
        let root = std::fs::canonicalize(&copy).unwrap();
        assert_eq!(validate(&record, &copy, &current, "Elden Ring").unwrap().root, root);
        assert_eq!(validate(&record, &copy.join("Game"), &current, "Elden Ring").unwrap().root, root);

        let error = validate(&record, &current, &current, "Elden Ring").unwrap_err();
        assert!(error.contains("Steam-Installation"), "{error}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_a_copy_outside_the_supported_version() {
        let base = temp_dir("version");
        let current = base.join("steam").join("ELDEN RING");
        std::fs::create_dir_all(current.join("Game")).unwrap();
        std::fs::write(current.join("Game/game.exe"), b"steam").unwrap();
        let copy = base.join("copy").join("ELDEN RING");
        std::fs::create_dir_all(copy.join("Game")).unwrap();
        std::fs::write(copy.join("Game/game.exe"), synthetic_pe(version::GameVersion([2, 7, 2, 0]))).unwrap();

        let record = record(Some(">=2.7.0.0 <=2.7.1.1"), vec![]);
        let error = validate(&record, &copy, &current, "Elden Ring").unwrap_err();
        assert!(error.contains("2.7.2.0"), "{error}");

        std::fs::write(copy.join("Game/game.exe"), synthetic_pe(version::GameVersion([2, 7, 1, 1]))).unwrap();
        let info = validate(&record, &copy, &current, "Elden Ring").unwrap();
        assert_eq!(info.version, Some(version::GameVersion([2, 7, 1, 1])));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn copies_mashup_files_and_refuses_foreign_conflicts() {
        let base = temp_dir("apply");
        let source = base.join("source");
        std::fs::create_dir_all(source.join("Game")).unwrap();
        let files = vec![
            installed_file(&source, "Game/erbridge/core.dll", b"bridge"),
            installed_file(&source, "Game/dinput8.dll", b"loader"),
        ];
        let record = record(None, files);
        let mods_dir = base.join("mods");
        let copy = base.join("copy");
        std::fs::create_dir_all(copy.join("Game")).unwrap();
        std::fs::write(copy.join("Game/game.exe"), b"copy").unwrap();

        apply(&record, &source, &copy, &mods_dir, "game").unwrap();
        assert_eq!(std::fs::read(copy.join("Game/dinput8.dll")).unwrap(), b"loader");
        assert_eq!(std::fs::read(copy.join("Game/erbridge/core.dll")).unwrap(), b"bridge");

        let conflict = base.join("conflict");
        std::fs::create_dir_all(conflict.join("Game")).unwrap();
        std::fs::write(conflict.join("Game/dinput8.dll"), b"other mod").unwrap();
        let mut other = record.clone();
        other.key = "mod:other".into();
        other.mod_id = "other".into();
        let error = apply(&other, &source, &conflict, &mods_dir, "game").unwrap_err();
        assert!(error.contains("andere Datei"), "{error}");

        cleanup(&mods_dir, &record.key).unwrap();
        assert!(!copy.join("Game/dinput8.dll").exists());
        assert!(!copy.join("Game/erbridge").exists());
        assert!(copy.join("Game/game.exe").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn removes_only_unchanged_files_and_stale_entries() {
        let base = temp_dir("cleanup");
        let source = base.join("source");
        std::fs::create_dir_all(source.join("Game")).unwrap();
        let first = installed_file(&source, "Game/one.dll", b"one");
        let second = installed_file(&source, "Game/two.dll", b"two");
        let mut record = record(None, vec![first, second]);
        let mods_dir = base.join("mods");
        let copy = base.join("copy");
        std::fs::create_dir_all(copy.join("Game")).unwrap();

        apply(&record, &source, &copy, &mods_dir, "game").unwrap();
        std::fs::write(copy.join("Game/two.dll"), b"changed by user").unwrap();

        // Eine neue Mashup-Version enthält nur noch `one.dll`; `two.dll` wurde vom Nutzer geändert
        // und darf deshalb nicht entfernt werden.
        record.files.retain(|file| file.rel == "Game/one.dll");
        apply(&record, &source, &copy, &mods_dir, "game").unwrap();
        assert!(copy.join("Game/one.dll").exists());
        assert_eq!(std::fs::read(copy.join("Game/two.dll")).unwrap(), b"changed by user");

        cleanup(&mods_dir, &record.key).unwrap();
        assert!(!copy.join("Game/one.dll").exists());
        assert_eq!(std::fs::read(copy.join("Game/two.dll")).unwrap(), b"changed by user");
        let _ = std::fs::remove_dir_all(base);
    }
}
