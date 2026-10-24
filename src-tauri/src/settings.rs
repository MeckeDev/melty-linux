//! Zwei Einstellungen der Seite, in denselben Dateien wie im Original (`startup-window-settings.json`,
//! `desktop-notification-settings.json`), atomar geschrieben.

use serde_json::{json, Value};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Open,
    Tray,
    Off,
}

impl Startup {
    pub fn as_str(self) -> &'static str {
        match self {
            Startup::Open => "open",
            Startup::Tray => "tray",
            Startup::Off => "off",
        }
    }
    pub fn parse(s: &str) -> Option<Startup> {
        match s {
            "open" => Some(Startup::Open),
            "tray" => Some(Startup::Tray),
            "off" => Some(Startup::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub startup: Startup,
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { startup: Startup::Open, notifications: false }
    }
}

fn read_json(dir: &Path, name: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(&fs::read_to_string(dir.join(name)).ok()?).ok()?;
    v.is_object().then_some(v)
}

fn write_json(dir: &Path, name: &str, value: &Value) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(value).expect("json")))?;
    fs::rename(&tmp, dir.join(name))
}

pub fn load(dir: &Path) -> Settings {
    let startup = read_json(dir, "startup-window-settings.json")
        .and_then(|v| v.get("behavior").and_then(Value::as_str).and_then(Startup::parse))
        .unwrap_or(Startup::Open);
    let notifications = read_json(dir, "desktop-notification-settings.json")
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false);
    Settings { startup, notifications }
}

pub fn save_startup(dir: &Path, value: Startup) -> std::io::Result<()> {
    write_json(dir, "startup-window-settings.json", &json!({ "behavior": value.as_str() }))
}

pub fn save_notifications(dir: &Path, enabled: bool) -> std::io::Result<()> {
    write_json(dir, "desktop-notification-settings.json", &json!({ "enabled": enabled }))
}


// ---------------------------------------------------------------------------------------------------------------
// Einstellungen des Linux-Clients (nicht Teil der Windows-App): Darstellung/Fenster. Wirken teils erst nach Neustart.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuMode {
    /// DMABUF-Renderer nur auf NVIDIA abschalten (bekannter Fehler: leeres Fenster).
    Auto,
    On,
    Off,
}

impl GpuMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuMode::Auto => "auto",
            GpuMode::On => "on",
            GpuMode::Off => "off",
        }
    }
    pub fn parse(s: &str) -> Option<GpuMode> {
        match s {
            "auto" => Some(GpuMode::Auto),
            "on" => Some(GpuMode::On),
            "off" => Some(GpuMode::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub gpu: GpuMode,
    pub force_x11: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { gpu: GpuMode::Auto, force_x11: false }
    }
}

const PREFS_FILE: &str = "app-preferences.json";

pub fn load_prefs(dir: &Path) -> Prefs {
    let Some(v) = read_json(dir, PREFS_FILE) else { return Prefs::default() };
    let d = Prefs::default();
    Prefs {
        gpu: v.get("gpu").and_then(Value::as_str).and_then(GpuMode::parse).unwrap_or(d.gpu),
        force_x11: v.get("forceX11").and_then(Value::as_bool).unwrap_or(d.force_x11),
    }
}

pub fn save_prefs(dir: &Path, p: &Prefs) -> std::io::Result<()> {
    write_json(dir, PREFS_FILE, &json!({ "gpu": p.gpu.as_str(), "forceX11": p.force_x11 }))
}

/// Umgebungsvariablen, die vor dem Start von GTK/WebKit zu setzen sind. Vom Nutzer gesetzte Variablen gewinnen.
pub fn env_for(p: &Prefs, nvidia_present: bool, is_set: impl Fn(&str) -> bool) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let dmabuf = match p.gpu {
        GpuMode::On => true,
        GpuMode::Auto => nvidia_present,
        GpuMode::Off => false,
    };
    if dmabuf && !is_set("WEBKIT_DISABLE_DMABUF_RENDERER") {
        out.push(("WEBKIT_DISABLE_DMABUF_RENDERER", "1"));
    }
    if p.force_x11 && !is_set("GDK_BACKEND") {
        out.push(("GDK_BACKEND", "x11"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("melty-settings-{tag}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn defaults_roundtrip_and_garbage() {
        let d = tmp("a");
        assert_eq!(load(&d), Settings::default());
        save_startup(&d, Startup::Tray).unwrap();
        save_notifications(&d, true).unwrap();
        assert_eq!(load(&d), Settings { startup: Startup::Tray, notifications: true });
        fs::write(d.join("startup-window-settings.json"), "{\"behavior\":\"banana\"}").unwrap();
        fs::write(d.join("desktop-notification-settings.json"), "kaputt").unwrap();
        assert_eq!(load(&d), Settings::default(), "ungültige Werte → Standard wie im Original");
        assert!(Startup::parse("OPEN").is_none());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn prefs_roundtrip_defaults_and_garbage() {
        let d = tmp("p");
        assert_eq!(load_prefs(&d), Prefs::default());
        let p = Prefs { gpu: GpuMode::On, force_x11: true };
        save_prefs(&d, &p).unwrap();
        assert_eq!(load_prefs(&d), p);
        fs::write(d.join("app-preferences.json"), "{\"gpu\":\"x\",\"forceX11\":\"ja\"}").unwrap();
        assert_eq!(load_prefs(&d), Prefs::default());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn env_rules() {
        let none = |_: &str| false;
        let auto = Prefs::default();
        assert_eq!(env_for(&auto, true, none), vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1")]);
        assert!(env_for(&auto, false, none).is_empty());
        let off = Prefs { gpu: GpuMode::Off, ..Prefs::default() };
        assert!(env_for(&off, true, none).is_empty(), "Off schaltet auch auf NVIDIA nichts um");
        let on_x11 = Prefs { gpu: GpuMode::On, force_x11: true };
        assert_eq!(env_for(&on_x11, false, none), vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1"), ("GDK_BACKEND", "x11")]);
        assert!(env_for(&on_x11, false, |_: &str| true).is_empty(), "vom Nutzer gesetzte Variablen gewinnen");
    }
}
