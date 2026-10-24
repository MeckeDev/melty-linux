//! Spiele-Erkennung für Melty-Linux (Phase 1). Liefert, was `meltyDesktop.listInstalledGames()` der Seite zurückgibt.
//!
//! Datenform und Berechnungen sind aus Melty 1.0.132 abgeleitet (`library.js: forPage`, `installed-games.js`).
//! Erkannt werden Steam, Lutris, Heroic (Epic/GOG/Amazon) und Faugus Launcher.

pub mod faugus;
pub mod heroic;
pub mod lutris;
pub mod procs;
pub mod steam;
pub mod vdf;

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Wie `normalizeText` im Original: Kleinbuchstaben, `.exe` am Ende weg, Nicht-[a-z0-9] → Leerzeichen, trimmen.
pub fn normalize_text(value: &str) -> String {
    let lower = value.to_lowercase();
    let lower = lower.strip_suffix(".exe").unwrap_or(&lower);
    let mut out = String::new();
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Wie `slugFor` im Original.
pub fn slug_for(value: &str) -> String {
    normalize_text(value).replace(' ', "-")
}

/// Store-IDs je Launcher. `steam` bleibt immer vorhanden (die Windows-Bridge erwartet das Feld),
/// alle anderen sind optional.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreAppIds {
    pub steam: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lutris: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heroic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faugus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottles: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prism: Option<String>,
}

impl StoreAppIds {
    pub fn steam(app_id: impl Into<String>) -> Self {
        StoreAppIds { steam: app_id.into(), ..Default::default() }
    }

    fn as_map(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        if !self.steam.is_empty() {
            map.insert("steam".into(), self.steam.clone().into());
        }
        for (key, value) in [
            ("lutris", &self.lutris),
            ("heroic", &self.heroic),
            ("gog", &self.gog),
            ("faugus", &self.faugus),
            ("bottles", &self.bottles),
            ("prism", &self.prism),
        ] {
            if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                map.insert(key.into(), value.clone().into());
            }
        }
        map
    }
}

/// Wie ein Launcher ein Spiel startet. Wird nie aus UI-Eingaben gebaut, nur aus erkannten Spielen.
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    /// URI über den Standard-Handler (Steam, Lutris, Heroic).
    Uri(String),
    /// Fester Programmname plus Argumente (Faugus, Bottles). Kein Shell.
    Command { program: String, args: Vec<String> },
}

impl Launch {
    pub fn label(&self) -> &'static str {
        match self {
            Launch::Uri(uri) if uri.starts_with("steam:") => "Steam",
            Launch::Uri(uri) if uri.starts_with("lutris:") => "Lutris",
            Launch::Uri(uri) if uri.starts_with("heroic:") => "Heroic",
            Launch::Uri(_) => "URI",
            Launch::Command { .. } => "Command",
        }
    }
}

/// Ein Eintrag von `games:list`, Feld für Feld wie `forPage` im Original.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PageGame {
    pub revision: u64,
    pub detection_state: &'static str, // "checking" | "ready" | "unavailable"
    pub installation_id: String,
    pub game_id: String,
    pub display_name: String,
    pub launcher: &'static str,
    pub status: &'static str,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    pub store_app_ids: StoreAppIds,
}

/// Was nur die nativen Teile brauchen (Mods, Start), nie die Seite.
#[derive(Debug, Clone)]
pub struct InstalledGame {
    pub page: PageGame,
    pub install_dir: PathBuf,
    pub proton_prefix: Option<PathBuf>,
    pub launch: Launch,
}

impl InstalledGame {
    pub fn launch_uri(&self) -> Option<&str> {
        match &self.launch {
            Launch::Uri(uri) => Some(uri),
            Launch::Command { .. } => None,
        }
    }
}

/// Ein von einem Launcher gefundenes Spiel, bevor die gemeinsamen Felder berechnet werden.
#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub display_name: String,
    pub launcher: &'static str,
    pub store_app_ids: StoreAppIds,
    pub install_dir: PathBuf,
    pub proton_prefix: Option<PathBuf>,
    pub launch: Launch,
}

/// Ergebnis eines Launcher-Scans.
#[derive(Debug, Default)]
pub struct LauncherScan {
    pub games: Vec<DetectedGame>,
    pub problems: Vec<String>,
    pub roots: Vec<PathBuf>,
}

/// `sha256(JSON.stringify([gameId, launcher, installDir.toLowerCase(), storeAppIds])).slice(0, 24)` wie im Original.
pub fn installation_id(game_id: &str, launcher: &str, install_dir: &str, steam_app_id: &str) -> String {
    installation_id_with(game_id, launcher, install_dir, &StoreAppIds::steam(steam_app_id))
}

pub fn installation_id_with(game_id: &str, launcher: &str, install_dir: &str, ids: &StoreAppIds) -> String {
    let json = serde_json::json!([game_id, launcher, install_dir.to_lowercase(), ids.as_map()]);
    let digest = Sha256::digest(serde_json::to_string(&json).expect("json").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..24].to_string()
}

pub struct Inventory {
    pub games: Vec<InstalledGame>,
    pub detection_state: &'static str,
    pub problems: Vec<String>,
}

/// Findet installierte Spiele und markiert laufende anhand von `proc_root` (normalerweise `/proc`).
pub fn inventory(home: &Path, proc_root: &Path, revision: u64) -> Inventory {
    let steam = steam::scan(home);
    let mut problems = steam.problems.clone();
    let mut detected: Vec<DetectedGame> = steam
        .apps
        .iter()
        .filter(|a| a.installed)
        .map(|app| DetectedGame {
            display_name: app.name.clone(),
            launcher: "Steam",
            store_app_ids: StoreAppIds::steam(app.app_id.clone()),
            install_dir: app.install_dir.clone(),
            proton_prefix: app.proton_prefix.clone(),
            launch: Launch::Uri(format!("steam://rungameid/{}", app.app_id)),
        })
        .collect();

    for scan in [lutris::scan(home), heroic::scan(home), faugus::scan(home)] {
        problems.extend(scan.problems);
        detected.extend(scan.games);
    }

    let procs = procs::scan(proc_root);
    let host: Vec<(&procs::Proc, PathBuf)> = procs.iter().filter_map(|p| p.host_path().map(|h| (p, h))).collect();
    // Teil-Fehler (unlesbare Bibliothek/Manifest) ⇒ "unavailable", wie im Original; sonst "ready".
    let detection_state = if problems.is_empty() { "ready" } else { "unavailable" };

    let mut games: Vec<InstalledGame> = detected
        .into_iter()
        .map(|game| {
            let canon = std::fs::canonicalize(&game.install_dir).unwrap_or_else(|_| game.install_dir.clone());
            let running = host
                .iter()
                .find(|(p, path)| {
                    process_matches(path, &game.install_dir, &canon)
                        || game
                            .proton_prefix
                            .as_ref()
                            .and_then(|prefix| p.host_path_with_prefix(prefix))
                            .is_some_and(|path| process_matches(&path, &game.install_dir, &canon))
                })
                .map(|(p, _)| (*p).clone());
            let game_id = slug_for(&game.display_name);
            let page = PageGame {
                revision,
                detection_state,
                installation_id: installation_id_with(&game_id, game.launcher, &game.install_dir.to_string_lossy(), &game.store_app_ids),
                game_id,
                display_name: game.display_name,
                launcher: game.launcher,
                status: "installed",
                running: running.is_some(),
                process_name: running.map(|p| p.name),
                store_app_ids: game.store_app_ids,
            };
            InstalledGame { page, install_dir: game.install_dir, proton_prefix: game.proton_prefix, launch: game.launch }
        })
        .collect();
    games.sort_by(|a, b| a.page.display_name.to_lowercase().cmp(&b.page.display_name.to_lowercase()));
    games.dedup_by(|a, b| a.page.installation_id == b.page.installation_id);
    Inventory { games, detection_state, problems }
}

fn process_matches(path: &Path, install_dir: &Path, canon: &Path) -> bool {
    !install_dir.as_os_str().is_empty() && (path.starts_with(install_dir) || path.starts_with(canon))
}

/// Direkt das, was `games:list` zurückgibt.
pub fn page_games(home: &Path, proc_root: &Path, revision: u64) -> Vec<PageGame> {
    inventory(home, proc_root, revision).games.into_iter().map(|g| g.page).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::testutil::*;

    #[test]
    fn slug_matches_original_semantics() {
        assert_eq!(slug_for("Hades II"), "hades-ii");
        assert_eq!(slug_for("The Witcher® 3: Wild Hunt"), "the-witcher-3-wild-hunt");
        assert_eq!(slug_for("SkyrimSE.exe"), "skyrimse");
        assert_eq!(slug_for("  --Half-Life: Alyx--  "), "half-life-alyx");
        assert_eq!(slug_for("ELDEN RING"), "elden-ring");
        assert_eq!(slug_for("***"), "");
    }

    #[test]
    fn installation_id_is_stable_and_24_hex() {
        let a = installation_id("portal-2", "Steam", "/Home/U/Portal 2", "620");
        let b = installation_id("portal-2", "Steam", "/home/u/portal 2", "620");
        assert_eq!(a, b, "installDir wird kleingeschrieben");
        assert_eq!(a.len(), 24);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, installation_id("portal-2", "Steam", "/home/u/portal 2", "621"));
        let lutris = StoreAppIds { lutris: Some("portal-2".into()), ..Default::default() };
        assert_ne!(a, installation_id_with("portal-2", "Lutris", "/home/u/portal 2", &lutris));
    }

    #[test]
    fn end_to_end_page_shape_and_running_detection() {
        let home = Tmp::new("e2e");
        let root = home.path().join(".local/share/Steam");
        manifest(&root, "271590", "Grand Theft Auto V", "Grand Theft Auto V", true);
        manifest(&root, "620", "Portal 2", "Portal 2", true);
        let proc_root = Tmp::new("e2e-proc");
        let gta = format!("Z:\\{}\\steamapps\\common\\Grand Theft Auto V\\GTA5.exe", root.display().to_string().trim_start_matches('/').replace('/', "\\"));
        write(&proc_root.path().join("4242/comm"), "GrandTheftAuto\n");
        std::fs::write(proc_root.path().join("4242/cmdline"), format!("{gta}\0")).unwrap();

        let list = page_games(home.path(), proc_root.path(), 7);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display_name, "Grand Theft Auto V"); // alphabetisch
        assert!(list[0].running && list[0].process_name.as_deref() == Some("GTA5.exe"));
        assert!(!list[1].running && list[1].process_name.is_none());

        let json = serde_json::to_value(&list[0]).unwrap();
        for key in ["revision", "detectionState", "installationId", "gameId", "displayName", "launcher", "status", "running", "processName", "storeAppIds"] {
            assert!(json.get(key).is_some(), "Feld {key} fehlt");
        }
        assert_eq!(json["storeAppIds"]["steam"], "271590");
        assert_eq!(json["detectionState"], "ready");
        assert!(serde_json::to_value(&list[1]).unwrap().get("processName").is_none(), "processName nur wenn laufend");
    }
}
