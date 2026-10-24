//! Melty Linux Client: Native Panel (Library, Mashups, Settings, Diagnostics).
//! No more website main window; Mashup data comes directly from public JSON endpoints.

#[allow(dead_code)] // Login handoff is parked for a future website bridge.
mod auth;
mod autostart;
mod config;
mod deeplink;
mod gamesrt;
mod mashups;
mod mods;
#[allow(dead_code)] // Parts of the old navigation rules are only used by tests anymore.
mod nav;
mod panel;
mod settings;
mod tray;

use config::{Config, IDENTIFIER, LOGIN_START_ARG};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

pub struct AppState {
    pub config: Config,
    pub settings: Mutex<settings::Settings>,
    pub prefs: Mutex<settings::Prefs>,
    pub settings_dir: PathBuf,
    pub revision: AtomicU64,
    pub account: Arc<auth::Account>,
    pub mods: Arc<mods::Mods>,
    pub mashups: mashups::MashupCache,
    /// From a `melty://m/<slug>` link until the panel picks it up.
    pub pending_mashup: Mutex<Option<String>>,
}

impl AppState {
    /// Save "start at login" and (only in installed builds) adjust the autostart entry.
    pub fn set_startup(&self, value: settings::Startup) -> Result<(), String> {
        settings::save_startup(&self.settings_dir, value).map_err(|e| e.to_string())?;
        self.settings.lock().unwrap().startup = value;
        if !cfg!(debug_assertions) {
            if let (Ok(exe), Some(dir)) = (std::env::current_exe(), autostart::config_home(|k| std::env::var(k).ok())) {
                if let Err(e) = autostart::apply(value, &exe, &dir) {
                    eprintln!("[settings] Autostart entry not written: {e}");
                }
            }
        }
        Ok(())
    }
}

/// "Open Melty" and the tray show the panel; there is no website window anymore.
pub fn show_main(app: &AppHandle) {
    panel::open(app, "mashups");
}

/// Open external http(s) URLs in the default browser. Other schemes are rejected.
pub fn open_external(app: &AppHandle, url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        eprintln!("[window] external target rejected: {url}");
        return;
    }
    if let Err(e) = app.opener().open_url(url, None::<&str>) {
        eprintln!("[window] could not open {url}: {e}");
    }
}

fn early_config_dir() -> Option<PathBuf> {
    autostart::config_home(|k| std::env::var(k).ok()).map(|h| h.join(IDENTIFIER))
}

/// Set rendering workarounds from settings as environment variables (must happen before GTK/WebKit).
fn apply_prefs_env() {
    let prefs = early_config_dir().map(|d| settings::load_prefs(&d)).unwrap_or_default();
    let nvidia = std::path::Path::new("/proc/driver/nvidia").exists();
    for (k, v) in settings::env_for(&prefs, nvidia, |k| std::env::var_os(k).is_some()) {
        std::env::set_var(k, v);
    }
}

fn handle_link(app: &AppHandle, url: &str) {
    let web = app.state::<AppState>().config.web_base.as_str().to_string();
    match deeplink::classify(url, &web) {
        Some(deeplink::Link::Retry) => panel::open(app, "mashups"),
        Some(deeplink::Link::Panel(tab)) => panel::open(app, &tab),
        Some(deeplink::Link::Mashup(slug)) => {
            *app.state::<AppState>().pending_mashup.lock().unwrap() = Some(slug.clone());
            panel::open(app, "mashups");
            // If the panel is already open: show the detail immediately, not just on the next fetch.
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.eval(&format!(
                    "window.__openMashup&&window.__openMashup({})",
                    serde_json::to_string(&slug).expect("json")
                ));
            }
        }
        Some(deeplink::Link::Page(target)) => open_external(app, &target),
        Some(deeplink::Link::AuthImport(_)) => {
            eprintln!("[deeplink] Login import is no longer supported without website window");
        }
        None => eprintln!("[deeplink] ignored: {url}"),
    }
}

pub fn run() {
    apply_prefs_env();
    let config = Config::load();

    tauri::Builder::default()
        // Must be the first plugin. A second start (e.g. via a melty://-link) goes to the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let handled: Vec<&String> = argv.iter().filter(|a| a.starts_with("melty://")).collect();
            if handled.is_empty() {
                panel::open(app, "mashups");
            } else {
                for arg in handled {
                    handle_link(app, arg);
                }
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            panel::panel_state,
            panel::panel_set,
            panel::panel_launch,
            panel::panel_reveal,
            panel::panel_open_site,
            panel::panel_restart,
            panel::mashups_search,
            panel::mashups_refresh,
            panel::mashups_detail,
            panel::mashups_open,
            mods::account_status,
            mods::account_login,
            mods::account_logout,
            mods::mods_state,
            mods::mods_library,
            mods::mods_install,
            mods::mods_uninstall,
            mods::mods_set_enabled,
            mods::mods_set_game_copy,
            mods::mods_pick_game_folder,
            mods::mods_launch,
            mods::mods_stop,
            mods::mods_open_folder,
            mods::mods_clear_jobs,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let dir = app.path().app_config_dir()?;
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let loaded = settings::load(&dir);
            let prefs = settings::load_prefs(&dir);
            let startup = loaded.startup;
            app.manage(AppState {
                config,
                settings: Mutex::new(loaded),
                prefs: Mutex::new(prefs),
                settings_dir: dir.clone(),
                revision: AtomicU64::new(1),
                account: Arc::new(auth::Account::new(data.join("melty-token"))),
                mods: Arc::new(mods::Mods::new(data.join("mods"))),
                mashups: mashups::MashupCache::new(dir),
                pending_mashup: Mutex::new(None),
            });

            #[cfg(debug_assertions)]
            panel::write_e2e_report(&handle);

            let tray_ok = tray::create(&handle);
            let at_login = std::env::args().any(|a| a == LOGIN_START_ARG);
            if !(at_login && startup == settings::Startup::Tray && tray_ok) {
                panel::open(&handle, "mashups");
            }

            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if std::env::var_os("APPIMAGE").is_some() {
                    let _ = app.deep_link().register_all(); // AppImages have no installer that registers the schema
                }
                let h = handle.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        handle_link(&h, url.as_str());
                    }
                });
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    for url in urls {
                        handle_link(&handle, url.as_str());
                    }
                }
            }

            gamesrt::start(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Error starting Melty Linux");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_dir_matches_identifier() {
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg-test");
        assert_eq!(early_config_dir(), Some(PathBuf::from("/tmp/xdg-test/gg.melty.linux-client")));
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], IDENTIFIER, "IDENTIFIER and tauri.conf.json must match");
    }
}
