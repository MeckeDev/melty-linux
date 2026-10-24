//! "Start at login" (`startup behavior` ≠ off) via XDG-autostart instead of Windows registry.

use crate::config::{LOGIN_START_ARG, TITLE};
use crate::settings::Startup;
use std::fs;
use std::path::{Path, PathBuf};

pub const ENTRY_NAME: &str = "melty-linux-client.desktop";

/// Exec value according to the Desktop Entry specification: escape in quotes, `\`, `"`, `` ` `` and `$`, double `%`.
fn exec_quote(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '\\' => out.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={TITLE}\nComment=Melty (unofficial Linux client)\nExec={} {LOGIN_START_ARG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec_quote(&exe.to_string_lossy())
    )
}

pub fn entry_path(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(ENTRY_NAME)
}

pub fn apply(behavior: Startup, exe: &Path, config_home: &Path) -> std::io::Result<()> {
    let path = entry_path(config_home);
    match behavior {
        Startup::Off => match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Startup::Open | Startup::Tray => {
            fs::create_dir_all(path.parent().expect("parent"))?;
            fs::write(&path, desktop_entry(exe))
        }
    }
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    get("XDG_CONFIG_HOME").filter(|v| v.starts_with('/')).map(PathBuf::from).or_else(|| get("HOME").map(|h| PathBuf::from(h).join(".config")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_content_and_quoting() {
        let e = desktop_entry(Path::new("/opt/My Apps/melty-linux"));
        assert!(e.contains("Exec=\"/opt/My Apps/melty-linux\" --melty-startup\n"), "{e}");
        assert!(desktop_entry(Path::new("/x/a\"b$c")).contains("Exec=\"/x/a\\\\\"b\\\\$c\""));
        assert!(desktop_entry(Path::new("/x/100%")).contains("100%%"));
    }

    #[test]
    fn apply_creates_and_removes() {
        let base = std::env::temp_dir().join(format!("melty-autostart-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let exe = Path::new("/usr/bin/melty-linux");
        apply(Startup::Off, exe, &base).unwrap(); // nothing there → no error
        apply(Startup::Tray, exe, &base).unwrap();
        assert!(entry_path(&base).is_file());
        apply(Startup::Off, exe, &base).unwrap();
        assert!(!entry_path(&base).exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn config_home_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(config_home(env(&[("XDG_CONFIG_HOME", "/c"), ("HOME", "/h")])), Some(PathBuf::from("/c")));
        assert_eq!(config_home(env(&[("XDG_CONFIG_HOME", "relativ"), ("HOME", "/h")])), Some(PathBuf::from("/h/.config")));
        assert_eq!(config_home(env(&[])), None);
    }
}
