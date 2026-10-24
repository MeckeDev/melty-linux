//! Heroic Games Launcher (Epic über Legendary, GOG, Amazon, Sideload).
//! Liest die JSON-Dateien unter `~/.config/heroic` bzw. dem Flatpak-Pfad und startet über
//! `heroic://launch?appName=…&runner=…`.

use crate::{DetectedGame, Launch, LauncherScan, StoreAppIds};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/heroic"),
        home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
    ]
}

fn app_url(app: &str, runner: &str) -> String {
    let encode = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    format!("heroic://launch?appName={}&runner={}", encode(app), encode(runner))
}

fn entries(value: &Value) -> Vec<&Value> {
    if let Some(array) = value.as_array() {
        return array.iter().filter(|v| v.is_object()).collect();
    }
    let Some(object) = value.as_object() else { return Vec::new() };
    if let Some(array) = object.get("installed").and_then(Value::as_array) {
        return array.iter().filter(|v| v.is_object()).collect();
    }
    object.values().filter(|v| v.is_object()).collect()
}

fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| value.get(*key).and_then(Value::as_str)).filter(|s| !s.trim().is_empty())
}

fn find_key(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if keys.iter().any(|key| k.eq_ignore_ascii_case(key)) {
                    if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                        return Some(s.to_string());
                    }
                }
            }
            map.values().find_map(|v| find_key(v, keys))
        }
        Value::Array(items) => items.iter().find_map(|v| find_key(v, keys)),
        _ => None,
    }
}

fn wine_prefix(root: &Path, app: &str) -> Option<PathBuf> {
    let file = root.join("GamesConfig").join(format!("{app}.json"));
    let text = std::fs::read_to_string(file).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    find_key(&value, &["winePrefix", "wine_prefix", "prefix"]).map(PathBuf::from)
}

fn read_file(root: &Path, relative: &str, runner: &str, out: &mut LauncherScan) {
    let path = root.join(relative);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            out.problems.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    for item in entries(&value) {
        if item.get("is_dlc").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let app = field(item, &["app_name", "appName", "appname", "id"]);
        let title = field(item, &["title", "name"]).or(app);
        let install = field(item, &["install_path", "installPath", "path", "folder"]);
        let (Some(app), Some(title), Some(install)) = (app, title, install) else { continue };
        let prefix = wine_prefix(root, app);
        let store_app_ids = match runner {
            "gog" => StoreAppIds { heroic: Some(app.to_string()), gog: Some(app.to_string()), ..Default::default() },
            _ => StoreAppIds { heroic: Some(app.to_string()), ..Default::default() },
        };
        out.games.push(DetectedGame {
            display_name: title.to_string(),
            launcher: "Heroic",
            store_app_ids,
            install_dir: PathBuf::from(install),
            proton_prefix: prefix,
            launch: Launch::Uri(app_url(app, runner)),
        });
    }
}

pub fn scan(home: &Path) -> LauncherScan {
    let mut out = LauncherScan::default();
    let mut seen: Vec<(String, String)> = Vec::new();
    for root in roots(home) {
        if !root.is_dir() {
            continue;
        }
        out.roots.push(root.clone());
        let before = out.games.len();
        read_file(&root, "legendary/installed.json", "legendary", &mut out);
        read_file(&root, "gog_store/installed.json", "gog", &mut out);
        read_file(&root, "sideload_apps/library.json", "sideload", &mut out);
        for game in &mut out.games[before..] {
            if let Some(app) = game.store_app_ids.heroic.clone() {
                if seen.contains(&(app.clone(), game.launcher.to_string())) {
                    continue;
                }
                seen.push((app, game.launcher.to_string()));
            }
        }
    }
    // Duplikate aus mehreren Roots entfernen (gleicher Heroic-Ordner in Flatpak und nativ).
    let mut unique: Vec<DetectedGame> = Vec::new();
    for game in out.games {
        if !unique.iter().any(|g| {
            g.store_app_ids.heroic.is_some() && g.store_app_ids.heroic == game.store_app_ids.heroic && g.launch == game.launch
        }) {
            unique.push(game);
        }
    }
    out.games = unique;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::testutil::*;

    #[test]
    fn reads_legendary_gog_and_sideload() {
        let home = Tmp::new("heroic");
        let root = home.path().join(".config/heroic");
        write(
            &root.join("legendary/installed.json"),
            r#"{"Fortnite":{"app_name":"Fortnite","title":"Fortnite","install_path":"/games/Fortnite"}}"#,
        );
        write(
            &root.join("gog_store/installed.json"),
            r#"{"installed":[{"appName":"1207658924","title":"The Witcher 3","install_path":"/games/Witcher3"}]}"#,
        );
        write(
            &root.join("sideload_apps/library.json"),
            r#"[{"app_name":"my-game","title":"My Game","install_path":"/games/MyGame"}]"#,
        );
        write(&root.join("GamesConfig/Fortnite.json"), r#"{"winePrefix":"/prefixes/Fortnite","wineVersion":{}}"#);
        let scan = scan(home.path());
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        assert_eq!(scan.games.len(), 3, "{:?}", scan.games.iter().map(|g| &g.display_name).collect::<Vec<_>>());
        let fortnite = scan.games.iter().find(|g| g.display_name == "Fortnite").unwrap();
        assert_eq!(fortnite.launch, Launch::Uri("heroic://launch?appName=Fortnite&runner=legendary".into()));
        assert_eq!(fortnite.proton_prefix.as_deref(), Some(Path::new("/prefixes/Fortnite")));
        let gog = scan.games.iter().find(|g| g.display_name == "The Witcher 3").unwrap();
        assert_eq!(gog.store_app_ids.gog.as_deref(), Some("1207658924"));
        assert_eq!(gog.launch, Launch::Uri("heroic://launch?appName=1207658924&runner=gog".into()));
        let side = scan.games.iter().find(|g| g.display_name == "My Game").unwrap();
        assert_eq!(side.launch, Launch::Uri("heroic://launch?appName=my-game&runner=sideload".into()));
    }

    #[test]
    fn broken_json_is_reported_and_others_survive() {
        let home = Tmp::new("heroic-broken");
        let root = home.path().join(".config/heroic");
        write(&root.join("legendary/installed.json"), "kaputt");
        write(&root.join("sideload_apps/library.json"), r#"[{"app_name":"x","title":"X","install_path":"/x"}]"#);
        let scan = scan(home.path());
        assert_eq!(scan.games.len(), 1);
        assert_eq!(scan.problems.len(), 1);
    }
}
