//! Resolving recipe destination tokens (`{game}`, `{appdata}`, `{managed}`, …) to Linux paths.
//!
//! Windows games normally run in a Proton/Wine prefix. Their `%APPDATA%`, `%LOCALAPPDATA%`,
//! `%USERPROFILE%` and `Documents` folders live inside that prefix, not in the Linux home directory, so
//! the prefix is preferred whenever the game has one. Native games fall back to the XDG layout.

use super::model::{parse_target, Recipe, ResolvedTarget, Target};
use melty_games::InstalledGame;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GameEnv {
    pub installation_id: String,
    pub install_dir: PathBuf,
    pub prefix: Option<PathBuf>,
    pub steam_app_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallEnv {
    pub home: PathBuf,
    pub mods_dir: PathBuf,
    pub mod_slug: String,
    pub games: HashMap<String, GameEnv>,
    pub primary_slug: Option<String>,
    pub steam_root: Option<PathBuf>,
}

pub fn game_env(game: &InstalledGame) -> GameEnv {
    GameEnv {
        installation_id: game.page.installation_id.clone(),
        install_dir: game.install_dir.clone(),
        prefix: game.proton_prefix.clone(),
        steam_app_id: (!game.page.store_app_ids.steam.is_empty()).then(|| game.page.store_app_ids.steam.clone()),
    }
}

/// The directory that contains `drive_c` for a Wine/Proton prefix. Steam stores it as `…/pfx`, Heroic and
/// Faugus sometimes point at the prefix itself.
pub fn wine_root(prefix: &Path) -> Option<PathBuf> {
    if prefix.join("drive_c").is_dir() {
        return Some(prefix.to_path_buf());
    }
    let pfx = prefix.join("pfx");
    if pfx.join("drive_c").is_dir() {
        return Some(pfx);
    }
    None
}

pub fn wine_user(prefix: &Path) -> Option<String> {
    let root = wine_root(prefix)?;
    let users = root.join("drive_c/users");
    for preferred in ["steamuser", "user"] {
        if users.join(preferred).is_dir() {
            return Some(preferred.to_string());
        }
    }
    std::fs::read_dir(users)
        .ok()?
        .flatten()
        .find(|e| e.path().is_dir())
        .and_then(|e| e.file_name().to_str().map(str::to_string))
}

fn xdg_documents(home: &Path) -> PathBuf {
    if let Ok(config) = std::fs::read_to_string(home.join(".config/user-dirs.dirs")) {
        for line in config.lines() {
            let Some(rest) = line.trim().strip_prefix("XDG_DOCUMENTS_DIR=") else { continue };
            let value = rest.trim().trim_matches('"');
            if let Some(rel) = value.strip_prefix("$HOME/") {
                return home.join(rel);
            }
        }
    }
    home.join("Documents")
}

/// The user-facing roots for the primary game. `None` means the token does not exist here.
fn user_root(env: &InstallEnv, token: &str) -> Option<PathBuf> {
    let primary = env.primary_slug.as_ref().and_then(|slug| env.games.get(slug));
    let profile = primary
        .and_then(|g| g.prefix.as_deref())
        .and_then(wine_root)
        .and_then(|root| {
            let user = wine_user(&root)?;
            Some(root.join("drive_c/users").join(user))
        });
    let documents = profile.as_ref().map(|p| p.join("Documents")).unwrap_or_else(|| xdg_documents(&env.home));
    match token {
        "userprofile" => Some(profile.unwrap_or_else(|| env.home.clone())),
        "documents" => Some(documents),
        "my-games" => Some(documents.join("My Games")),
        "saved-games" => Some(profile.as_ref().map(|p| p.join("Saved Games")).unwrap_or_else(|| env.home.join("Saved Games"))),
        "appdata" => Some(profile.as_ref().map(|p| p.join("AppData/Roaming")).unwrap_or_else(|| xdg_config(&env.home))),
        "localappdata" => Some(profile.as_ref().map(|p| p.join("AppData/Local")).unwrap_or_else(|| xdg_data(&env.home))),
        "locallow" => Some(profile.as_ref().map(|p| p.join("AppData/LocalLow")).unwrap_or_else(|| xdg_data(&env.home))),
        _ => None,
    }
}

fn xdg_config(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"))
}

fn xdg_data(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))
}

/// Find the Steam installation root that contains `steamapps`, starting at the game's install folder.
pub fn steam_root_from(game: &GameEnv) -> Option<PathBuf> {
    let mut current = game.install_dir.as_path();
    for _ in 0..6 {
        if current.join("steamapps").is_dir() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
    if let Some(prefix) = &game.prefix {
        // …/steamapps/compatdata/<appid>/pfx
        let mut current = prefix.as_path();
        for _ in 0..6 {
            if current.join("steamapps").is_dir() {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }
    None
}

/// `‎{token}` resolution. Returns an error with a user-readable reason when a required game is missing.
pub fn resolve_target(target: &Target, env: &InstallEnv) -> Result<ResolvedTarget, String> {
    let slug = target.game_slug.as_ref().or(env.primary_slug.as_ref());
    let game = match target.root.as_str() {
        "game" => {
            let slug = slug.ok_or_else(|| "This mashup does not say which game it is for.".to_string())?;
            let game = env.games.get(slug).ok_or_else(|| format!("Install the game for this mashup first ({slug})."))?;
            Some(game)
        }
        _ => None,
    };
    let (root, token) = match target.root.as_str() {
        "game" => (game.expect("set above").install_dir.clone(), "game"),
        "managed" => (env.mods_dir.join("managed").join(&env.mod_slug), "managed"),
        "minecraft-instance" => (env.mods_dir.join("minecraft").join(&env.mod_slug), "minecraft-instance"),
        "steam" => (
            env.steam_root.clone().ok_or_else(|| "Steam was not found; install Steam before this mashup.".to_string())?,
            "steam",
        ),
        other => (
            user_root(env, other).ok_or_else(|| format!("This client does not know the {{{other}}} folder."))?,
            other,
        ),
    };
    if !root.is_absolute() {
        return Err("Refused a relative installation folder.".into());
    }
    Ok(ResolvedTarget {
        root,
        rel: target.subpath.clone(),
        token: token.to_string(),
        game_slug: target.game_slug.clone().or_else(|| if target.root == "game" { env.primary_slug.clone() } else { None }),
    })
}

/// True when a recipe writes to a Windows user folder (`%LOCALAPPDATA%` etc.). For a Steam game the
/// Proton prefix must exist before those paths can be resolved, otherwise the files land in the
/// Linux XDG folders while the launcher later looks inside the prefix.
pub fn recipe_uses_windows_profile(recipe: &Recipe) -> bool {
    let Ok(text) = serde_json::to_string(recipe) else { return false };
    [
        "{appdata}",
        "{localappdata}",
        "{locallow}",
        "{userprofile}",
        "{documents}",
        "{my-games}",
        "{saved-games}",
    ]
    .iter()
    .any(|token| text.contains(token))
}

/// Resolve a string that is either a recipe target or a plain argument. Plain arguments are returned
/// unchanged, so a launch arg like `-windowed` stays as it is.
pub fn expand_launch_string(value: &str, env: &InstallEnv, windows: bool) -> Result<String, String> {
    if let Some(target) = parse_target(value) {
        let resolved = resolve_target(&target, env)?;
        let path = resolved.path();
        return Ok(if windows { wine_path(&path) } else { path.to_string_lossy().into_owned() });
    }
    Ok(value.to_string())
}

/// Convert a Linux path to the `Z:\…` form seen by Wine/Proton. Windows paths are kept.
pub fn wine_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.starts_with("Z:\\") || (text.len() >= 2 && text.as_bytes()[1] == b':') {
        return text.into_owned();
    }
    format!("Z:{}", text.replace('/', "\\"))
}

/// Wine/Proton adressiert Dateien im Präfix über `C:`. Der `Z:`-Pfad funktioniert zwar auch,
/// aber manche Windows-Programme (u. a. Java/ZipFS) prüfen Schreibrechte über die Windows-ACL
/// und halten Dateien auf `Z:` fälschlich für schreibgeschützt. Deshalb Programme innerhalb des
/// Präfixes möglichst als `C:\...` übergeben.
pub fn wine_path_in_prefix(path: &Path, prefix: &Path) -> String {
    let Some(root) = wine_root(prefix) else { return wine_path(path) };
    let drive_c = root.join("drive_c");
    let Ok(rel) = path.strip_prefix(&drive_c) else { return wine_path(path) };
    if rel.as_os_str().is_empty() {
        return "C:\\".into();
    }
    format!("C:\\{}", rel.to_string_lossy().replace('/', "\\"))
}

/// True if `child` is `parent` or inside it (lexically, after normalising `.` and `..`).
pub fn is_within(parent: &Path, child: &Path) -> bool {
    let normalize = |p: &Path| {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    };
    normalize(child).starts_with(normalize(parent))
}

/// Create a destination directory and return a canonical base. Refuses symlinked path components.
pub fn safe_base(root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("Could not create {}: {e}", root.display()))?;
    let mut current = root.to_path_buf();
    // Walk existing components and reject symlinks.
    let mut parts = Vec::new();
    for component in root.components() {
        match component {
            std::path::Component::RootDir => current = PathBuf::from("/"),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(format!("Refused a path with '..': {}", root.display()));
            }
            std::path::Component::Normal(part) => {
                current.push(part);
                parts.push(current.clone());
            }
            std::path::Component::Prefix(_) => {}
        }
    }
    for path in parts {
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(format!("Refused to write through a linked folder: {}", path.display()));
            }
        }
    }
    std::fs::canonicalize(root).map_err(|e| format!("Could not resolve {}: {e}", root.display()))
}

/// Join a relative recipe path under a canonical base and verify that no existing component is a symlink.
pub fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = super::model::normalize_entry(rel).ok_or_else(|| format!("Refused an unsafe path: {rel}"))?;
    let mut path = base.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(format!("Refused to write through a linked file: {}", path.display()));
            }
        }
    }
    if !is_within(base, &path) {
        return Err(format!("Path escaped its folder: {rel}"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("melty-paths-{tag}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn wine_paths() {
        assert_eq!(wine_path(Path::new("/home/u/Game")), "Z:\\home\\u\\Game");
        assert_eq!(wine_path(Path::new("C:\\Game")), "C:\\Game");
    }

    #[test]
    fn wine_paths_inside_a_prefix_use_the_c_drive() {
        let home = tmp("prefix-cdrive");
        let pfx = home.join("steamapps/compatdata/1/pfx");
        let exe = pfx.join("drive_c/users/steamuser/AppData/Local/EldenCraft/Prism/prismlauncher.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(
            wine_path_in_prefix(&exe, &pfx),
            "C:\\users\\steamuser\\AppData\\Local\\EldenCraft\\Prism\\prismlauncher.exe"
        );
        assert_eq!(
            wine_path_in_prefix(Path::new("/home/u/Game/game.exe"), &pfx),
            "Z:\\home\\u\\Game\\game.exe"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn prefix_roots_and_user() {
        let home = tmp("prefix");
        let pfx = home.join("steamapps/compatdata/1/pfx");
        std::fs::create_dir_all(pfx.join("drive_c/users/steamuser/Documents")).unwrap();
        assert_eq!(wine_root(&pfx).as_deref(), Some(pfx.as_path()));
        assert_eq!(wine_user(&pfx).as_deref(), Some("steamuser"));
        let env = InstallEnv {
            home: home.clone(),
            mods_dir: home.join("mods"),
            mod_slug: "m".into(),
            games: HashMap::from([(
                "game".to_string(),
                GameEnv { installation_id: "x".into(), install_dir: home.join("Game"), prefix: Some(pfx.clone()), steam_app_id: Some("1".into()) },
            )]),
            primary_slug: Some("game".into()),
            steam_root: Some(home.clone()),
        };
        let app = resolve_target(&parse_target("{appdata}/Foo").unwrap(), &env).unwrap();
        assert_eq!(app.path(), pfx.join("drive_c/users/steamuser/AppData/Roaming/Foo"));
        let doc = resolve_target(&parse_target("{documents}").unwrap(), &env).unwrap();
        assert_eq!(doc.path(), pfx.join("drive_c/users/steamuser/Documents"));
        let managed = resolve_target(&parse_target("{managed}/Mod").unwrap(), &env).unwrap();
        assert_eq!(managed.path(), home.join("mods/managed/m/Mod"));
    }

    #[test]
    fn windows_profile_tokens_are_detected() {
        let mut recipe = Recipe::default();
        assert!(!recipe_uses_windows_profile(&recipe));
        recipe.mappings.push(crate::mods::model::Mapping {
            component: "main".into(),
            from: String::new(),
            to: "{localappdata}/Game".into(),
        });
        assert!(recipe_uses_windows_profile(&recipe));

        let mut game_only = Recipe::default();
        game_only.mappings.push(crate::mods::model::Mapping {
            component: "main".into(),
            from: String::new(),
            to: "{game}".into(),
        });
        assert!(!recipe_uses_windows_profile(&game_only));
    }

    #[test]
    fn safe_join_rejects_links_and_escape() {
        let home = tmp("safe");
        let base = home.join("base");
        std::fs::create_dir_all(&base).unwrap();
        std::os::unix::fs::symlink("/tmp", base.join("link")).unwrap();
        assert!(safe_join(&safe_base(&base).unwrap(), "a/b").is_ok());
        assert!(safe_join(&safe_base(&base).unwrap(), "link/x").is_err());
        assert!(safe_join(&safe_base(&base).unwrap(), "../x").is_err());
    }
}
