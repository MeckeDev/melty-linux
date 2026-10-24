//! Lutris: `~/.local/share/lutris/pga.db` (SQLite) plus optionale YAML-Konfiguration je Spiel.
//! Start über `lutris:rungame/<slug>`; der Prefix wird für die Prozess-Erkennung mitgeführt.

use crate::{DetectedGame, Launch, LauncherScan, StoreAppIds};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn db_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/lutris/pga.db"),
        home.join(".var/app/net.lutris.Lutris/data/lutris/pga.db"),
    ]
}

fn config_dirs(home: &Path) -> Vec<PathBuf> {
    vec![home.join(".config/lutris/games"), home.join(".var/app/net.lutris.Lutris/config/lutris/games")]
}

fn text(row: &rusqlite::Row<'_>, columns: &[String], name: &str) -> Option<String> {
    let idx = columns.iter().position(|c| c == name)?;
    match row.get::<_, rusqlite::types::Value>(idx).ok()? {
        rusqlite::types::Value::Text(s) => Some(s),
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        rusqlite::types::Value::Real(f) => Some(f.to_string()),
        _ => None,
    }
}

fn integer(row: &rusqlite::Row<'_>, columns: &[String], name: &str) -> Option<i64> {
    let idx = columns.iter().position(|c| c == name)?;
    match row.get::<_, rusqlite::types::Value>(idx).ok()? {
        rusqlite::types::Value::Integer(i) => Some(i),
        rusqlite::types::Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

/// Winziger YAML-Leser für die flachen Schlüssel, die Lutris schreibt (`prefix:`, `exe:`, `directory:`).
/// Kein vollständiger YAML-Parser: verschachtelte Werte werden bewusst ignoriert.
fn flat_yaml(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else { continue };
        let value = rest.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if !value.is_empty() && value != "~" && value != "null" {
            return Some(value.replace("$HOME", &std::env::var("HOME").unwrap_or_default()));
        }
    }
    None
}

fn expand(value: &str, home: &Path) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}

pub fn scan(home: &Path) -> LauncherScan {
    let mut out = LauncherScan::default();
    let configs = config_dirs(home);
    for db in db_candidates(home) {
        if !db.is_file() {
            continue;
        }
        out.roots.push(db.clone());
        let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
            out.problems.push(format!("{}: konnte nicht geöffnet werden", db.display()));
            continue;
        };
        let _ = conn.busy_timeout(Duration::from_millis(800));
        let mut stmt = match conn.prepare("SELECT * FROM games") {
            Ok(stmt) => stmt,
            Err(e) => {
                out.problems.push(format!("{}: {e}", db.display()));
                continue;
            }
        };
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map([], |row| {
            let name = text(row, &columns, "name").unwrap_or_default();
            let slug = text(row, &columns, "slug").unwrap_or_default();
            let runner = text(row, &columns, "runner").unwrap_or_default();
            let directory = text(row, &columns, "directory").unwrap_or_default();
            let configpath = text(row, &columns, "configpath").unwrap_or_default();
            let installed = integer(row, &columns, "installed");
            let id = text(row, &columns, "id").unwrap_or_default();
            Ok((id, name, slug, runner, directory, configpath, installed))
        });
        let Ok(rows) = rows else {
            out.problems.push(format!("{}: Spiele konnten nicht gelesen werden", db.display()));
            continue;
        };
        for row in rows.flatten() {
            let (id, name, slug, runner, directory, configpath, installed) = row;
            if name.trim().is_empty() || installed == Some(0) {
                continue;
            }
            let key = if slug.trim().is_empty() { id.clone() } else { slug.clone() };
            if key.is_empty() {
                continue;
            }
            // Steam-Spiele kommen bereits aus dem Steam-Scanner; nicht doppelt anzeigen.
            if runner.eq_ignore_ascii_case("steam") {
                continue;
            }
            let yml = lutris_config(&configs, &slug, &configpath);
            let text = yml.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            let prefix = flat_yaml(&text, "prefix").map(|p| expand(&p, home));
            let directory = if directory.trim().is_empty() {
                flat_yaml(&text, "directory").map(|p| expand(&p, home)).unwrap_or_default()
            } else {
                expand(&directory, home)
            };
            let exe = flat_yaml(&text, "exe").map(|p| expand(&p, home));
            let install_dir = if !directory.as_os_str().is_empty() {
                directory
            } else if let Some(exe) = exe.as_ref() {
                exe.parent().map(Path::to_path_buf).unwrap_or_default()
            } else if let Some(prefix) = prefix.as_ref() {
                prefix.clone()
            } else {
                PathBuf::new()
            };
            out.games.push(DetectedGame {
                display_name: name,
                launcher: "Lutris",
                store_app_ids: StoreAppIds { lutris: Some(key.clone()), ..Default::default() },
                install_dir,
                proton_prefix: prefix,
                launch: Launch::Uri(format!("lutris:rungame/{key}")),
            });
        }
    }
    out
}

fn lutris_config(dirs: &[PathBuf], slug: &str, configpath: &str) -> Option<PathBuf> {
    if !configpath.trim().is_empty() {
        let p = PathBuf::from(configpath);
        if p.is_absolute() && p.is_file() {
            return Some(p);
        }
    }
    for dir in dirs {
        let p = dir.join(format!("{slug}.yml"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::testutil::*;

    #[test]
    fn reads_database_and_yaml_prefix() {
        let home = Tmp::new("lutris");
        let db = home.path().join(".local/share/lutris/pga.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE games (id INTEGER PRIMARY KEY, name TEXT, slug TEXT, runner TEXT, directory TEXT, installed INTEGER, configpath TEXT);
                 INSERT INTO games (name, slug, runner, directory, installed) VALUES ('Elden Ring', 'elden-ring', 'wine', '', 1);
                 INSERT INTO games (name, slug, runner, directory, installed) VALUES ('Portal 2', 'portal-2', 'steam', '/tmp/p2', 1);
                 INSERT INTO games (name, slug, runner, directory, installed) VALUES ('Old', 'old', 'wine', '', 0);",
            )
            .unwrap();
        }
        let yml = home.path().join(".config/lutris/games/elden-ring.yml");
        write(&yml, "game:\n  exe: drive_c/Games/Elden/eldenring.exe\n  prefix: ~/Games/elden\n");
        std::fs::create_dir_all(home.path().join("Games/elden/drive_c")).unwrap();

        let scan = scan(home.path());
        assert_eq!(scan.games.len(), 1, "{:?}", scan.problems);
        let game = &scan.games[0];
        assert_eq!(game.display_name, "Elden Ring");
        assert_eq!(game.launcher, "Lutris");
        assert_eq!(game.store_app_ids.lutris.as_deref(), Some("elden-ring"));
        assert_eq!(game.launch, Launch::Uri("lutris:rungame/elden-ring".into()));
        assert_eq!(game.proton_prefix.as_deref(), Some(home.path().join("Games/elden").as_path()));
        assert!(game.install_dir.ends_with("drive_c/Games/Elden"), "{}", game.install_dir.display());
    }

    #[test]
    fn missing_database_is_silent() {
        let home = Tmp::new("lutris-empty");
        let scan = scan(home.path());
        assert!(scan.games.is_empty() && scan.problems.is_empty() && scan.roots.is_empty());
    }
}
