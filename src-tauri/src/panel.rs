//! Natives Panel des Linux-Clients (lokale Oberfläche `ui/panel.html`): Bibliothek, Mashups, Einstellungen, Diagnose.
//! Die Befehle sind nur dem Fenster "panel" erlaubt (eigene Capability `panel.json`, zusätzlich Label-Prüfung);
//! die melty.gg-Seite kann sie nicht aufrufen.

use crate::mashups::{self, SearchQuery};
use crate::{config, gamesrt, settings, AppState};
use melty_games::{InstalledGame, Launch};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use tauri::window::Color;
use tauri::{AppHandle, Manager, State, Webview, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

const NOT_ALLOWED: &str = "Not allowed.";

pub(crate) fn guard(webview: &Webview) -> Result<(), String> {
    if webview.label() == "panel" {
        Ok(())
    } else {
        Err(NOT_ALLOWED.to_string())
    }
}

/// Öffnet das Panel (oder holt es nach vorn) und wählt den Tab.
pub fn open(app: &AppHandle, tab: &str) {
    let tab = if ["games", "mashups", "installed", "settings", "diagnostics"].contains(&tab) { tab } else { "games" };
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.eval(&format!("window.__setTab&&window.__setTab({})", serde_json::to_string(tab).expect("json")));
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App(format!("panel.html?tab={tab}").into()))
        .title("Melty Linux")
        .inner_size(1000.0, 720.0)
        .min_inner_size(760.0, 520.0)
        .background_color(Color(20, 20, 20, 255))
        .on_navigation(|url| crate::nav::is_internal(url))
        .build();
    if let Err(e) = built {
        eprintln!("[panel] konnte nicht geöffnet werden: {e}");
    }
}

pub fn game_json(g: &InstalledGame) -> Value {
    json!({
        "installationId": g.page.installation_id,
        "gameId": g.page.game_id,
        "name": g.page.display_name,
        "appId": g.page.store_app_ids.steam,
        "storeAppIds": g.page.store_app_ids,
        "launcher": g.page.launcher,
        "launchKind": match &g.launch { Launch::Uri(_) => "uri", Launch::Command { .. } => "command" },
        "running": g.page.running,
        "processName": g.page.process_name,
        "installDir": g.install_dir,
        "protonPrefix": g.proton_prefix,
    })
}

pub fn settings_json(s: &settings::Settings, p: &settings::Prefs) -> Value {
    json!({
        "startup": s.startup.as_str(),
        "notifications": s.notifications,
        "gpu": p.gpu.as_str(),
        "forceX11": p.force_x11,
    })
}

fn env_json() -> Value {
    let var = |k: &str| std::env::var(k).ok();
    json!({
        "sessionType": var("XDG_SESSION_TYPE"),
        "waylandDisplay": var("WAYLAND_DISPLAY"),
        "display": var("DISPLAY"),
        "gdkBackend": var("GDK_BACKEND"),
        "dmabufDisabled": var("WEBKIT_DISABLE_DMABUF_RENDERER"),
        "nvidiaDriver": std::path::Path::new("/proc/driver/nvidia").exists(),
        "desktop": var("XDG_CURRENT_DESKTOP"),
        "archiveTool": crate::mods::archive::external_tool(),
    })
}

fn collect(revision: u64) -> (Vec<Value>, Value, HashSet<String>) {
    let inv = melty_games::inventory(&gamesrt::home(), &gamesrt::proc_root(), revision);
    let steam = melty_games::steam::scan(&gamesrt::home());
    let games: Vec<Value> = inv.games.iter().map(game_json).collect();
    // Schlüssel für den Mashup-Filter "nur installierte Spiele": Slug und Store-IDs.
    let mut installed = HashSet::new();
    for g in &inv.games {
        installed.insert(g.page.game_id.clone());
        installed.insert(melty_games::slug_for(&g.page.display_name));
        if !g.page.store_app_ids.steam.is_empty() {
            installed.insert(g.page.store_app_ids.steam.clone());
            installed.insert(format!("steam:{}", g.page.store_app_ids.steam));
        }
    }
    let detection = json!({
        "state": inv.detection_state,
        "problems": inv.problems,
        "roots": steam.roots.iter().map(|r| json!({ "path": r.path, "flavor": r.flavor })).collect::<Vec<_>>(),
        "libraries": steam.libraries,
        "skippedTools": steam.skipped_tools,
    });
    (games, detection, installed)
}

#[tauri::command]
pub async fn panel_state(webview: Webview, state: State<'_, AppState>) -> Result<Value, String> {
    guard(&webview)?;
    let rev = state.revision.load(Ordering::SeqCst);
    let (games, detection, _) = tauri::async_runtime::spawn_blocking(move || collect(rev)).await.map_err(|e| e.to_string())?;
    let settings = settings_json(&state.settings.lock().unwrap(), &state.prefs.lock().unwrap());
    let pending = state.pending_mashup.lock().unwrap().take();
    {
        let home = gamesrt::home();
        let proc_root = gamesrt::proc_root();
        for record in state.mods.records() {
            if record.key.starts_with("mod:") {
                state.mods.prune_running(&record.mod_id, &home, &proc_root);
            }
        }
    }
    Ok(json!({
        "app": {
            "name": "Melty Linux Client",
            "version": env!("CARGO_PKG_VERSION"),
            "compat": config::COMPAT_VERSION,
            "webBase": state.config.web_base.as_str(),
            "configDir": state.settings_dir,
        },
        "settings": settings,
        "games": games,
        "detection": detection,
        "env": env_json(),
        "account": state.account.status(),
        "mods": state.mods.snapshot(),
        "pendingMashup": pending,
    }))
}

/// Eine Einstellung ändern. Schlüssel: startup, notifications, gpu, forceX11.
#[tauri::command]
pub fn panel_set(webview: Webview, state: State<'_, AppState>, key: String, value: Value) -> Result<Value, String> {
    guard(&webview)?;
    let bad = |what: &str| format!("Ungültiger Wert für {what}");
    match key.as_str() {
        "startup" => {
            let v = value.as_str().and_then(settings::Startup::parse).ok_or_else(|| bad("startup"))?;
            state.set_startup(v)?;
        }
        "notifications" => {
            let v = value.as_bool().ok_or_else(|| bad("notifications"))?;
            settings::save_notifications(&state.settings_dir, v).map_err(|e| e.to_string())?;
            state.settings.lock().unwrap().notifications = v;
        }
        "gpu" | "forceX11" => {
            let mut prefs = state.prefs.lock().unwrap();
            match key.as_str() {
                "gpu" => prefs.gpu = value.as_str().and_then(settings::GpuMode::parse).ok_or_else(|| bad("gpu"))?,
                _ => prefs.force_x11 = value.as_bool().ok_or_else(|| bad("forceX11"))?,
            }
            settings::save_prefs(&state.settings_dir, &prefs).map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("Unbekannte Einstellung: {key}")),
    }
    Ok(settings_json(&state.settings.lock().unwrap(), &state.prefs.lock().unwrap()))
}

fn find_game(installation_id: &str) -> Result<InstalledGame, String> {
    if installation_id.len() != 24 || !installation_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Ungültige Installations-ID".into());
    }
    melty_games::inventory(&gamesrt::home(), &gamesrt::proc_root(), 0)
        .games
        .into_iter()
        .find(|g| g.page.installation_id == installation_id)
        .ok_or_else(|| "Spiel nicht gefunden".to_string())
}

/// Startet ein erkanntes Spiel über seinen Launcher (URI oder festes Programm, nie über eine Shell).
#[tauri::command]
pub async fn panel_launch(app: AppHandle, webview: Webview, installation_id: String) -> Result<(), String> {
    guard(&webview)?;
    let game = tauri::async_runtime::spawn_blocking(move || find_game(&installation_id)).await.map_err(|e| e.to_string())??;
    match &game.launch {
        Launch::Uri(uri) => app.opener().open_url(uri.clone(), None::<&str>).map_err(|e| e.to_string()),
        Launch::Command { program, args } => std::process::Command::new(program)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("{program} konnte nicht gestartet werden: {e}")),
    }
}

/// Öffnet den Installationsordner eines erkannten Spiels im Dateimanager (nur Ordner erkannter Spiele).
#[tauri::command]
pub async fn panel_reveal(app: AppHandle, webview: Webview, installation_id: String) -> Result<(), String> {
    guard(&webview)?;
    let game = tauri::async_runtime::spawn_blocking(move || find_game(&installation_id)).await.map_err(|e| e.to_string())??;
    app.opener().open_path(game.install_dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// Öffnet melty.gg im Standardbrowser (es gibt kein Website-Fenster mehr).
#[tauri::command]
pub fn panel_open_site(app: AppHandle, webview: Webview, state: State<'_, AppState>) -> Result<(), String> {
    guard(&webview)?;
    crate::open_external(&app, state.config.web_base.as_str());
    Ok(())
}

#[tauri::command]
pub fn panel_restart(app: AppHandle, webview: Webview) -> Result<(), String> {
    guard(&webview)?;
    app.restart()
}

fn sort_key(value: Option<String>) -> String {
    match value.as_deref() {
        Some(v @ ("top" | "new" | "updated" | "rating" | "name")) => v.to_string(),
        _ => "top".to_string(),
    }
}

/// Sucht im lokal gecachten Katalog; Filter und Sortierung laufen ohne weitere Netzaufrufe.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mashups_search(
    webview: Webview,
    state: State<'_, AppState>,
    text: Option<String>,
    sort: Option<String>,
    game: Option<String>,
    installed_only: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Value, String> {
    guard(&webview)?;
    let installed_only = installed_only.unwrap_or(false);
    let installed_games = if installed_only {
        tauri::async_runtime::spawn_blocking(|| collect(0).2).await.map_err(|e| e.to_string())?
    } else {
        HashSet::new()
    };
    let query = SearchQuery {
        text: text.unwrap_or_default().chars().take(120).collect(),
        sort: sort_key(sort),
        game: game.filter(|g| !g.is_empty()),
        installed_only,
        installed_games,
        offset: offset.unwrap_or(0),
        limit: limit.unwrap_or(24).clamp(1, 120),
    };
    let result = mashups::search(&state.mashups, &state.config.web_base, query).await?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

/// Erzwingt einen neuen Katalog-Abruf.
#[tauri::command]
pub async fn mashups_refresh(webview: Webview, state: State<'_, AppState>) -> Result<Value, String> {
    guard(&webview)?;
    let info = mashups::refresh(&state.mashups, &state.config.web_base).await?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

/// Detail eines Mashups (`/api/mods/<slug>`).
#[tauri::command]
pub async fn mashups_detail(webview: Webview, state: State<'_, AppState>, slug: String) -> Result<Value, String> {
    guard(&webview)?;
    mashups::detail(&state.config.web_base, &slug).await
}

/// Öffnet die Mashup-Seite im Standardbrowser.
#[tauri::command]
pub fn mashups_open(app: AppHandle, webview: Webview, state: State<'_, AppState>, slug: String) -> Result<(), String> {
    guard(&webview)?;
    if slug.is_empty() || slug.len() > 80 || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("Ungültiger Mashup-Name".into());
    }
    let base = &state.config.web_base;
    let url = base.join(&format!("/m/{slug}")).map_err(|e| e.to_string())?;
    if url.origin() != base.origin() {
        return Err("Ungültiges Ziel".into());
    }
    crate::open_external(&app, url.as_str());
    Ok(())
}

/// Nur in Debug-Builds: schreibt für `tests/e2e/run.py` einen Bericht über Spiele-Erkennung und
/// Mashup-Suche in die Datei aus `MELTY_E2E_REPORT`. Wird im normalen Betrieb nie aktiviert.
#[cfg(debug_assertions)]
pub fn write_e2e_report(app: &AppHandle) {
    let Some(path) = std::env::var_os("MELTY_E2E_REPORT").map(std::path::PathBuf::from) else { return };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let base = state.config.web_base.clone();
        let mut out = serde_json::Map::new();

        let rev = state.revision.load(Ordering::SeqCst);
        let (installed, first_game) = match tauri::async_runtime::spawn_blocking(move || collect(rev)).await {
            Ok((games, detection, installed)) => {
                let first_game = games.iter().find_map(|g| g.get("gameId").and_then(Value::as_str).map(str::to_string));
                out.insert("games".into(), json!(games));
                out.insert("detection".into(), detection);
                (installed, first_game)
            }
            Err(e) => {
                out.insert("error".into(), json!(e.to_string()));
                (HashSet::new(), None)
            }
        };

        let mut top = None;
        let search = SearchQuery { sort: "top".into(), limit: 60, ..Default::default() };
        match mashups::search(&state.mashups, &base, search).await {
            Ok(r) => {
                out.insert("top".into(), serde_json::to_value(&r).unwrap_or(Value::Null));
                top = Some(r);
            }
            Err(e) => {
                out.insert("searchError".into(), json!(e));
            }
        }

        let search = SearchQuery { installed_only: true, installed_games: installed.clone(), limit: 60, ..Default::default() };
        match mashups::search(&state.mashups, &base, search).await {
            Ok(r) => {
                out.insert("installed".into(), serde_json::to_value(&r).unwrap_or(Value::Null));
            }
            Err(e) => {
                out.insert("installedError".into(), json!(e));
            }
        }

        let search = SearchQuery { text: "gta".into(), limit: 60, ..Default::default() };
        match mashups::search(&state.mashups, &base, search).await {
            Ok(r) => {
                out.insert("text".into(), serde_json::to_value(&r).unwrap_or(Value::Null));
            }
            Err(e) => {
                out.insert("textError".into(), json!(e));
            }
        }

        let game = top
            .as_ref()
            .and_then(|r| r.games.iter().find(|f| installed.contains(&f.slug)).map(|f| f.slug.clone()))
            .or(first_game);
        if let Some(game) = game {
            let search = SearchQuery { game: Some(game.clone()), limit: 60, ..Default::default() };
            match mashups::search(&state.mashups, &base, search).await {
                Ok(r) => {
                    out.insert("gameFilter".into(), json!({ "game": game, "result": r }));
                }
                Err(e) => {
                    out.insert("gameError".into(), json!(e));
                }
            }
        }

        if let Some(slug) = top.as_ref().and_then(|r| r.items.first().map(|m| m.slug.clone())) {
            match mashups::detail(&base, &slug).await {
                Ok(d) => {
                    out.insert("detail".into(), json!({ "slug": slug, "mod": d }));
                }
                Err(e) => {
                    out.insert("detailError".into(), json!(e));
                }
            }
        }

        let text = serde_json::to_string_pretty(&Value::Object(out)).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("[e2e] Bericht konnte nicht geschrieben werden: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn find_game_rejects_garbage_before_scanning() {
        assert!(find_game("").is_err());
        assert!(find_game("620; rm -rf").is_err());
        assert!(find_game("../etc").is_err());
    }

    #[test]
    fn settings_shape() {
        let v = settings_json(&settings::Settings::default(), &settings::Prefs::default());
        assert_eq!(v, json!({ "startup": "open", "notifications": false, "gpu": "auto", "forceX11": false }));
    }

    #[test]
    fn sort_keys_are_whitelisted() {
        assert_eq!(sort_key(Some("rating".into())), "rating");
        assert_eq!(sort_key(Some("../../etc".into())), "top");
        assert_eq!(sort_key(None), "top");
    }
}
