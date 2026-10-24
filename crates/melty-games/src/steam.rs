//! Steam unter Linux: Installationen (nativ, Flatpak, Snap), Bibliotheken, App-Manifeste, Proton-Prefixe.
//! Ersetzt `steamRoots`/`steamFolders` des windows-inspector (Registry + Laufwerksbuchstaben).

use crate::vdf;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Flavor {
    Native,
    Flatpak,
    Snap,
}

#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub flavor: Flavor,
}

#[derive(Debug, Clone)]
pub struct App {
    pub app_id: String,
    pub name: String,
    /// `<library>/steamapps/common/<installdir>`
    pub install_dir: PathBuf,
    /// Ordner existiert wirklich (ein Manifest allein reicht nicht, vgl. `known_missing_files` im Original).
    pub installed: bool,
    pub state_flags: Option<u32>,
    pub library: PathBuf,
    /// `<library>/steamapps/compatdata/<appid>/pfx`, falls vorhanden (Spiel lief schon über Proton).
    pub proton_prefix: Option<PathBuf>,
    pub flavor: Flavor,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub roots: Vec<Root>,
    pub libraries: Vec<PathBuf>,
    pub apps: Vec<App>,
    /// Proton, Steam Linux Runtime & Co. – erscheinen als Manifeste, sind aber keine Spiele.
    pub skipped_tools: usize,
    /// Nicht lesbare Bibliotheken/Manifeste (z. B. nicht eingehängte Platte).
    pub problems: Vec<String>,
}

pub fn find_roots(home: &Path) -> Vec<Root> {
    let mut candidates = vec![
        (home.join(".local/share/Steam"), Flavor::Native),
        (home.join(".steam/steam"), Flavor::Native),
        (home.join(".steam/root"), Flavor::Native),
        (home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"), Flavor::Flatpak),
        (home.join(".var/app/com.valvesoftware.Steam/data/Steam"), Flavor::Flatpak),
        (home.join("snap/steam/common/.local/share/Steam"), Flavor::Snap),
    ];
    #[cfg(windows)]
    {
        if let Some(pd) = std::env::var_os("ProgramFiles(x86)") {
            candidates.push((PathBuf::from(pd).join("Steam"), Flavor::Native));
        }
        if let Some(pd) = std::env::var_os("ProgramFiles") {
            candidates.push((PathBuf::from(pd).join("Steam"), Flavor::Native));
        }
        if let Some(ld) = std::env::var_os("LOCALAPPDATA") {
            candidates.push((PathBuf::from(ld).join("Steam"), Flavor::Native));
        }
    }
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for (path, flavor) in candidates {
        if !path.join("steamapps").is_dir() {
            continue;
        }
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key.clone()) {
            roots.push(Root { path: key, flavor });
        }
    }
    roots
}

/// Alle Bibliotheksordner einer Steam-Installation (Wurzel inklusive).
pub fn libraries(root: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];
    for file in [root.join("steamapps/libraryfolders.vdf"), root.join("config/libraryfolders.vdf")] {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        let doc = match vdf::parse(&text) {
            Ok(d) => d,
            Err(e) => {
                problems.push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        let Some(folders) = doc.obj("libraryfolders") else { continue };
        for (key, value) in folders.iter() {
            if !key.chars().all(|c| c.is_ascii_digit()) {
                continue; // z. B. "contentstatsid"
            }
            let path = match value {
                vdf::Value::Obj(o) => o.str("path"), // aktuelles Format
                vdf::Value::Str(s) => Some(s.as_str()), // altes Format: "1" "D:\\Lib"
            };
            if let Some(p) = path.map(str::trim).filter(|p| !p.is_empty()) {
                libs.push(PathBuf::from(p));
            }
        }
        break; // die erste lesbare Datei genügt
    }
    let mut seen = HashSet::new();
    libs.retain(|p| seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
    libs
}

/// Proton, Steam Linux Runtime, Redistributables: Namenskriterium statt fester App-IDs.
pub fn is_tool(app_id: &str, name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    app_id == "228980" // Steamworks Common Redistributables
        || n == "proton"
        || n.starts_with("proton ")
        || n.starts_with("proton-")
        || n.starts_with("steam linux runtime")
        || n.starts_with("steamworks common redistributables")
}

fn read_manifest(path: &Path, library: &Path, flavor: Flavor) -> Result<Option<App>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc = vdf::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let state = doc.obj("AppState").unwrap_or(&doc);
    let from_file = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("appmanifest_"))
        .and_then(|n| n.strip_suffix(".acf"))
        .map(str::to_string);
    let app_id = state.str("appid").map(str::to_string).or(from_file);
    let (Some(app_id), Some(name), Some(dir)) = (app_id, state.str("name"), state.str("installdir")) else {
        return Err(format!("{}: appid/name/installdir fehlt", path.display()));
    };
    if is_tool(&app_id, name) {
        return Ok(None);
    }
    let install_dir = library.join("steamapps/common").join(dir);
    let prefix = library.join("steamapps/compatdata").join(&app_id).join("pfx");
    Ok(Some(App {
        installed: install_dir.is_dir(),
        state_flags: state.str("StateFlags").and_then(|s| s.parse().ok()),
        proton_prefix: prefix.is_dir().then_some(prefix),
        name: name.to_string(),
        install_dir,
        library: library.to_path_buf(),
        flavor,
        app_id,
    }))
}

pub fn scan(home: &Path) -> Scan {
    let mut out = Scan { roots: find_roots(home), ..Scan::default() };
    let mut seen_libs = HashSet::new();
    let mut seen_apps: HashSet<String> = HashSet::new();
    let roots = out.roots.clone();
    for root in &roots {
        for lib in libraries(&root.path, &mut out.problems) {
            let key = fs::canonicalize(&lib).unwrap_or_else(|_| lib.clone());
            if !seen_libs.insert(key) {
                continue;
            }
            let steamapps = lib.join("steamapps");
            let entries = match fs::read_dir(&steamapps) {
                Ok(e) => e,
                Err(e) => {
                    // z. B. externe Platte nicht eingehängt → "unavailable", nicht "kein Spiel"
                    out.problems.push(format!("{}: {e}", steamapps.display()));
                    continue;
                }
            };
            out.libraries.push(lib.clone());
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("appmanifest_") && n.ends_with(".acf"))
                })
                .collect();
            files.sort();
            for file in files {
                match read_manifest(&file, &lib, root.flavor) {
                    Ok(Some(app)) => {
                        if seen_apps.insert(app.app_id.clone()) {
                            out.apps.push(app);
                        }
                    }
                    Ok(None) => out.skipped_tools += 1,
                    Err(e) => out.problems.push(e),
                }
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    pub struct Tmp(pub PathBuf);
    impl Tmp {
        pub fn new(tag: &str) -> Tmp {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let p = std::env::temp_dir().join(format!("melty-games-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    pub fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    pub fn manifest(lib: &Path, id: &str, name: &str, dir: &str, installed: bool) {
        write(
            &lib.join(format!("steamapps/appmanifest_{id}.acf")),
            &format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"{dir}\"\n}}\n"),
        );
        if installed {
            std::fs::create_dir_all(lib.join("steamapps/common").join(dir)).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn native_root_with_extra_library_and_tools() {
        let home = Tmp::new("native");
        let root = home.path().join(".local/share/Steam");
        let extra = home.path().join("games/SteamLibrary");
        write(
            &root.join("steamapps/libraryfolders.vdf"),
            &format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{r}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{e}\"\n\t}}\n}}\n",
                r = root.display(),
                e = extra.display()
            ),
        );
        manifest(&root, "620", "Portal 2", "Portal 2", true);
        manifest(&root, "1493710", "Proton Experimental", "Proton - Experimental", true);
        manifest(&root, "1628350", "Steam Linux Runtime 3.0 (sniper)", "SteamLinuxRuntime_sniper", true);
        manifest(&root, "228980", "Steamworks Common Redistributables", "Steamworks Shared", true);
        manifest(&extra, "271590", "Grand Theft Auto V", "Grand Theft Auto V", true);
        manifest(&extra, "1245620", "ELDEN RING", "ELDEN RING", false); // Manifest ohne Ordner
        std::fs::create_dir_all(extra.join("steamapps/compatdata/271590/pfx")).unwrap();
        // Symlink .steam/steam → gleiche Wurzel darf nicht doppelt zählen
        std::fs::create_dir_all(home.path().join(".steam")).unwrap();
        std::os::unix::fs::symlink(&root, home.path().join(".steam/steam")).unwrap();

        let s = scan(home.path());
        assert_eq!(s.roots.len(), 1, "Symlink muss dedupliziert werden");
        assert_eq!(s.libraries.len(), 2);
        assert_eq!(s.skipped_tools, 3);
        let names: Vec<_> = s.apps.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"Portal 2") && names.contains(&"Grand Theft Auto V") && names.contains(&"ELDEN RING"));
        assert_eq!(s.apps.len(), 3);
        let gta = s.apps.iter().find(|a| a.app_id == "271590").unwrap();
        assert!(gta.installed && gta.proton_prefix.is_some());
        let er = s.apps.iter().find(|a| a.app_id == "1245620").unwrap();
        assert!(!er.installed, "fehlender Ordner → nicht installiert");
        assert!(s.problems.is_empty(), "{:?}", s.problems);
    }

    #[test]
    fn flatpak_and_old_libraryfolders_format() {
        let home = Tmp::new("flatpak");
        let root = home.path().join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        let lib2 = home.path().join("lib2");
        write(&root.join("steamapps/libraryfolders.vdf"), &format!("\"LibraryFolders\"\n{{\n\t\"TimeNextStatsReport\"\t\"1\"\n\t\"1\"\t\"{}\"\n}}\n", lib2.display()));
        manifest(&lib2, "570", "Dota 2", "dota 2 beta", true);
        let s = scan(home.path());
        assert_eq!(s.roots[0].flavor, Flavor::Flatpak);
        assert_eq!(s.apps.len(), 1);
        assert_eq!(s.apps[0].flavor, Flavor::Flatpak);
    }

    #[test]
    fn unmounted_library_is_a_problem_not_silence() {
        let home = Tmp::new("unmounted");
        let root = home.path().join(".local/share/Steam");
        write(&root.join("steamapps/libraryfolders.vdf"), "\"libraryfolders\" { \"1\" { \"path\" \"/mnt/gibt-es-nicht\" } }");
        manifest(&root, "620", "Portal 2", "Portal 2", true);
        let s = scan(home.path());
        assert_eq!(s.apps.len(), 1);
        assert!(s.problems.iter().any(|p| p.contains("gibt-es-nicht")), "{:?}", s.problems);
    }

    #[test]
    fn broken_manifest_is_reported_and_others_survive() {
        let home = Tmp::new("broken");
        let root = home.path().join(".local/share/Steam");
        manifest(&root, "620", "Portal 2", "Portal 2", true);
        write(&root.join("steamapps/appmanifest_999.acf"), "\"AppState\" { \"appid\" ");
        let s = scan(home.path());
        assert_eq!(s.apps.len(), 1);
        assert_eq!(s.problems.len(), 1);
    }
}
