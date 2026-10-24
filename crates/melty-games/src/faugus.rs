//! Faugus Launcher: `games.json` (XDG_DATA_HOME) und Start über `faugus-launcher --game <gameid>`.
//! Unter Flatpak entsprechend `flatpak run io.github.Faugus.faugus-launcher --game <gameid>`.

use crate::{DetectedGame, Launch, LauncherScan, StoreAppIds};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn candidates(home: &Path) -> Vec<(PathBuf, bool)> {
    vec![
        (home.join(".local/share/faugus-launcher/games.json"), false),
        (home.join(".config/faugus-launcher/games.json"), false),
        (home.join(".var/app/io.github.Faugus.faugus-launcher/data/faugus-launcher/games.json"), true),
        (home.join(".var/app/io.github.Faugus.faugus-launcher/config/faugus-launcher/games.json"), true),
    ]
}

pub fn scan(home: &Path) -> LauncherScan {
    let mut out = LauncherScan::default();
    for (file, flatpak) in candidates(home) {
        if !file.is_file() {
            continue;
        }
        out.roots.push(file.clone());
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                out.problems.push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                out.problems.push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        let Some(games) = value.as_array() else {
            out.problems.push(format!("{}: kein Array", file.display()));
            continue;
        };
        for game in games {
            if game.get("hidden").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(id) = game.get("gameid").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else { continue };
            let Some(title) = game.get("title").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else { continue };
            let path = game.get("path").and_then(Value::as_str).unwrap_or("").trim();
            let prefix = game.get("prefix").and_then(Value::as_str).unwrap_or("").trim();
            let install_dir = if !path.is_empty() {
                PathBuf::from(path).parent().map(Path::to_path_buf).unwrap_or_default()
            } else {
                PathBuf::from(prefix)
            };
            let launch = if flatpak {
                Launch::Command {
                    program: "flatpak".into(),
                    args: vec!["run".into(), "io.github.Faugus.faugus-launcher".into(), "--game".into(), id.to_string()],
                }
            } else {
                Launch::Command { program: "faugus-launcher".into(), args: vec!["--game".into(), id.to_string()] }
            };
            out.games.push(DetectedGame {
                display_name: title.to_string(),
                launcher: "Faugus",
                store_app_ids: StoreAppIds { faugus: Some(id.to_string()), ..Default::default() },
                install_dir,
                proton_prefix: (!prefix.is_empty()).then(|| PathBuf::from(prefix)),
                launch,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::testutil::*;

    #[test]
    fn reads_games_and_builds_launch_command() {
        let home = Tmp::new("faugus");
        let file = home.path().join(".local/share/faugus-launcher/games.json");
        write(
            &file,
            r#"[
              {"gameid":"Cyberpunk2077","title":"Cyberpunk 2077","path":"/games/Cyberpunk/bin/x64/Cyberpunk2077.exe","prefix":"/prefixes/cp","hidden":false},
              {"gameid":"Hidden","title":"Hidden","path":"/games/Hidden/x.exe","prefix":"/prefixes/h","hidden":true}
            ]"#,
        );
        let scan = scan(home.path());
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        assert_eq!(scan.games.len(), 1);
        let game = &scan.games[0];
        assert_eq!(game.display_name, "Cyberpunk 2077");
        assert_eq!(game.install_dir, PathBuf::from("/games/Cyberpunk/bin/x64"));
        assert_eq!(game.proton_prefix.as_deref(), Some(Path::new("/prefixes/cp")));
        assert_eq!(
            game.launch,
            Launch::Command { program: "faugus-launcher".into(), args: vec!["--game".into(), "Cyberpunk2077".into()] }
        );
    }

    #[test]
    fn broken_file_is_reported() {
        let home = Tmp::new("faugus-broken");
        write(&home.path().join(".local/share/faugus-launcher/games.json"), "nope");
        let scan = scan(home.path());
        assert!(scan.games.is_empty());
        assert_eq!(scan.problems.len(), 1);
    }
}
