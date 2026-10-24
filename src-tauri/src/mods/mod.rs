//! Mashup-Verwaltung: Konto, Bibliothek, Installation, Deinstallation, Aktivieren/Deaktivieren und Start.
//!
//! Die Datenform und die Sicherheitsregeln folgen dem öffentlichen Desktop-Protokoll von Melty
//! (`/api/desktop/mods/library`, `…/install-ticket`). Der Client lädt und entpackt nur, was in einem
//! gültigen Ticket steht, schreibt ausschließlich unter erlaubte Wurzelordner und führt ein Journal,
//! aus dem sich jede Änderung wieder zurücknehmen lässt.

pub mod archive;
pub mod claim;
pub mod gamecopy;
pub mod journal;
pub mod launch;
pub mod model;
pub mod paths;
mod version;

use crate::config::{self, Config};
use crate::{auth, gamesrt, mashups, panel, AppState};
use journal::{Backup, InstalledFile, Journal, Record, StoredTarget};
use model::{LibraryMod, LibraryResponse, Recipe, Ticket, TicketFile};
use paths::{GameEnv, InstallEnv};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State, Webview};
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningView {
    pub mod_id: String,
    pub slug: String,
    pub pid: u32,
    pub started_at: u64,
    pub stoppable: bool,
}

#[derive(Debug, Clone)]
pub struct RunningProcess {
    pub mod_id: String,
    pub slug: String,
    pub pid: u32,
    pub started_at: u64,
    pub stoppable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub mod_id: String,
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub phase: String,
    pub done: bool,
    pub error: Option<String>,
    /// Maschinenlesbarer Grund des letzten Fehlers (z. B. `not-owned`), damit das Panel die passende
    /// Aktion anbieten kann.
    pub error_code: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub started_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    pub mod_id: String,
    pub slug: String,
    pub title: String,
    pub release_id: String,
    pub version: String,
    pub game_slugs: Vec<String>,
    pub primary_game_slug: Option<String>,
    pub enabled: bool,
    pub installed_at: u64,
    pub updated_at: u64,
    pub loaders: Vec<String>,
    pub file_count: usize,
    pub managed: bool,
    /// Explizite Spielordner pro Spiel-Slug (nur gesetzt, wenn der Nutzer eine Spielkopie gewählt hat).
    pub game_paths: BTreeMap<String, String>,
    /// Ob dieses Mashup überhaupt mit einer separaten Spielkopie gestartet werden kann.
    pub game_copy_available: bool,
}

/// Alles, was das Panel über Mashups wissen muss. Rezepte und Dateipfade bleiben in Rust.
pub struct Mods {
    pub dir: PathBuf,
    journal: Mutex<Journal>,
    jobs: Mutex<HashMap<String, JobView>>,
    running: Mutex<HashMap<String, RunningProcess>>,
    companions: Mutex<HashMap<String, Vec<RunningProcess>>>,
    install_lock: tokio::sync::Mutex<()>,
}

impl Mods {
    pub fn new(dir: PathBuf) -> Mods {
        for sub in ["managed", "minecraft", "downloads", "staging", "backups", "parked", "prefixes"] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }
        let journal = Journal::load(&dir);
        Mods {
            dir,
            journal: Mutex::new(journal),
            jobs: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            companions: Mutex::new(HashMap::new()),
            install_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Value {
        let installed: Vec<RecordView> = {
            let journal = self.journal.lock().unwrap();
            journal
                .records
                .iter()
                .filter(|record| record.key.starts_with("mod:"))
                .map(|record| RecordView {
                    mod_id: record.mod_id.clone(),
                    slug: record.slug.clone(),
                    title: record.title.clone(),
                    release_id: record.release_id.clone(),
                    version: record.version.clone(),
                    game_slugs: record.game_slugs.clone(),
                    primary_game_slug: record.primary_game_slug.clone(),
                    enabled: record.enabled,
                    installed_at: record.installed_at,
                    updated_at: record.updated_at,
                    loaders: record.loaders.clone(),
                    file_count: record.files.len(),
                    managed: record.files.iter().any(|file| file.root == "managed"),
                    game_paths: record.game_paths.clone(),
                    game_copy_available: gamecopy::available(record),
                })
                .collect()
        };
        let mut jobs: Vec<JobView> = self.jobs.lock().unwrap().values().cloned().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.started_at));
        let running = self.running_views();
        json!({ "installed": installed, "jobs": jobs, "running": running })
    }

    pub fn record(&self, mod_id: &str) -> Option<Record> {
        self.journal.lock().unwrap().get(&Record::key(mod_id)).cloned()
    }

    pub fn records(&self) -> Vec<Record> {
        self.journal.lock().unwrap().records.clone()
    }

    fn update_record(&self, record: Record) -> Result<(), String> {
        let mut journal = self.journal.lock().unwrap();
        journal.upsert(record);
        self.save_journal(&journal)
    }

    fn job_start(&self, mod_id: &str, kind: &str) -> Result<String, String> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.values().any(|job| job.mod_id == mod_id && !job.done) {
            return Err("Für dieses Mashup läuft bereits eine Aufgabe.".into());
        }
        let id = format!("{kind}-{mod_id}-{}", journal::now());
        jobs.insert(
            id.clone(),
            JobView {
                id: id.clone(),
                mod_id: mod_id.to_string(),
                slug: String::new(),
                title: String::new(),
                kind: kind.to_string(),
                phase: "starting".into(),
                done: false,
                error: None,
                error_code: None,
                bytes_done: 0,
                bytes_total: 0,
                started_at: journal::now(),
            },
        );
        Ok(id)
    }

    fn job_meta(&self, job: &str, slug: &str, title: &str) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.slug = slug.to_string();
            job.title = title.to_string();
        }
    }

    fn job_title(&self, job: &str) -> Option<String> {
        self.jobs.lock().unwrap().get(job).map(|job| job.title.clone()).filter(|title| !title.trim().is_empty())
    }

    fn job_slug(&self, job: &str) -> Option<String> {
        self.jobs.lock().unwrap().get(job).map(|job| job.slug.clone()).filter(|slug| !slug.trim().is_empty())
    }

    fn job_error_code(&self, job: &str, code: &str) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.error_code = Some(code.to_string());
        }
    }

    fn job_phase(&self, job: &str, phase: &str, total: u64) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.phase = phase.to_string();
            if total > 0 {
                job.bytes_total = total;
            }
        }
    }

    fn job_total_add(&self, job: &str, bytes: u64) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.bytes_total = job.bytes_total.saturating_add(bytes);
        }
    }

    fn job_add_bytes(&self, job: &str, bytes: u64) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.bytes_done = job.bytes_done.saturating_add(bytes);
        }
    }

    fn job_finish(&self, job: &str, result: Result<(), String>) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job) {
            job.done = true;
            match result {
                Ok(()) => job.phase = "done".into(),
                Err(error) => {
                    job.phase = "error".into();
                    job.error = Some(error);
                }
            }
        }
    }

    fn running_views(&self) -> Vec<RunningView> {
        let mut views: Vec<RunningView> = {
            let running = self.running.lock().unwrap();
            running
                .values()
                .map(|run| RunningView {
                    mod_id: run.mod_id.clone(),
                    slug: run.slug.clone(),
                    pid: run.pid,
                    started_at: run.started_at,
                    stoppable: run.stoppable,
                })
                .collect()
        };
        let companions = self.companions.lock().unwrap();
        for view in &mut views {
            if companions
                .get(&view.mod_id)
                .is_some_and(|list| list.iter().any(|run| run.stoppable && run.pid != 0))
            {
                view.stoppable = true;
            }
        }
        for (mod_id, list) in companions.iter() {
            if views.iter().any(|view| &view.mod_id == mod_id) {
                continue;
            }
            if let Some(run) = list.first() {
                views.push(RunningView {
                    mod_id: run.mod_id.clone(),
                    slug: run.slug.clone(),
                    pid: run.pid,
                    started_at: run.started_at,
                    stoppable: list.iter().any(|run| run.stoppable && run.pid != 0),
                });
            }
        }
        views
    }

    pub fn running(&self) -> Vec<RunningProcess> {
        let mut out: Vec<RunningProcess> = self.running.lock().unwrap().values().cloned().collect();
        for list in self.companions.lock().unwrap().values() {
            out.extend(list.iter().cloned());
        }
        out
    }

    /// Entfernt veraltete Einträge: beendete Direktstarts und Steam-Starts, die nach kurzer Zeit kein
    /// laufendes Spiel gemeldet haben. Begleitprogramme werden ebenfalls auf tote PIDs geprüft.
    pub fn prune_running(&self, mod_id: &str, home: &Path, proc_root: &Path) {
        {
            let mut companions = self.companions.lock().unwrap();
            if let Some(list) = companions.get_mut(mod_id) {
                list.retain(|run| run.pid == 0 || proc_root.join(run.pid.to_string()).exists());
                if list.is_empty() {
                    companions.remove(mod_id);
                }
            }
        }
        let mut running = self.running.lock().unwrap();
        let Some(run) = running.get(mod_id) else { return };
        if run.pid != 0 {
            if proc_root.join(run.pid.to_string()).exists() {
                return;
            }
            running.remove(mod_id);
            return;
        }
        if journal::now().saturating_sub(run.started_at) < 30 {
            return;
        }
        // Steam/URI-Starts haben keine eigene PID. Solange das Spiel laut Inventar läuft, bleibt der
        // Eintrag stehen; danach darf das Mashup wieder gestartet werden.
        let game_running = self
            .journal
            .lock()
            .unwrap()
            .get(&Record::key(mod_id))
            .and_then(|record| {
                let primary = record.primary_game_slug.as_ref()?;
                let installation = record.installation_ids.get(primary)?;
                Some(installation.clone())
            })
            .is_some_and(|installation| {
                installed_games(home, proc_root)
                    .iter()
                    .any(|game| game.page.installation_id == installation && game.page.running)
            });
        if !game_running {
            running.remove(mod_id);
        }
    }

    pub fn is_running(&self, mod_id: &str) -> bool {
        if self.running.lock().unwrap().contains_key(mod_id) {
            return true;
        }
        self.companions
            .lock()
            .unwrap()
            .get(mod_id)
            .is_some_and(|list| !list.is_empty())
    }

    fn set_running(&self, run: RunningProcess) {
        self.running.lock().unwrap().insert(run.mod_id.clone(), run);
    }

    fn clear_running(&self, mod_id: &str, pid: Option<u32>) {
        let mut running = self.running.lock().unwrap();
        if let Some(pid) = pid {
            if running.get(mod_id).is_some_and(|run| run.pid != pid) {
                return;
            }
        }
        running.remove(mod_id);
    }

    fn add_companion(&self, run: RunningProcess) {
        let mut companions = self.companions.lock().unwrap();
        let list = companions.entry(run.mod_id.clone()).or_default();
        list.retain(|entry| entry.pid != run.pid);
        list.push(run);
    }

    fn clear_companion(&self, mod_id: &str, pid: u32) {
        let mut companions = self.companions.lock().unwrap();
        if let Some(list) = companions.get_mut(mod_id) {
            list.retain(|entry| entry.pid != pid);
            if list.is_empty() {
                companions.remove(mod_id);
            }
        }
    }

    pub async fn stop(&self, mod_id: &str) -> Result<(), String> {
        let run = self.running.lock().unwrap().get(mod_id).cloned();
        let companions = self.companions.lock().unwrap().remove(mod_id).unwrap_or_default();
        for companion in companions.iter().filter(|run| run.stoppable && run.pid != 0) {
            kill_group(companion.pid).await;
        }
        let Some(run) = run else {
            return if companions.is_empty() {
                Err("Dieses Mashup läuft nicht (mehr).".into())
            } else {
                Ok(())
            };
        };
        if !run.stoppable || run.pid == 0 {
            return Err("Dieses Spiel wurde über Steam gestartet. Beende es dort.".into());
        }
        self.running.lock().unwrap().remove(mod_id);
        kill_group(run.pid).await;
        Ok(())
    }

    fn save_journal(&self, journal: &Journal) -> Result<(), String> {
        journal.save(&self.dir)
    }
}

async fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        for _ in 0..16 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).output();
    }
}

#[derive(Debug, Clone)]
struct Api {
    base: url::Url,
    token: String,
}

#[derive(Debug)]
struct ApiError {
    status: u16,
    code: Option<String>,
    message: String,
}

impl Api {
    async fn json(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
        let url = self.base.join(path).map_err(|e| ApiError { status: 0, code: None, message: e.to_string() })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(Config::user_agent())
            .build()
            .map_err(|e| ApiError { status: 0, code: None, message: e.to_string() })?;
        let mut request = client
            .request(method, url)
            .header("accept", "application/json")
            .header("cache-control", "no-store")
            .header("x-melty-app-version", config::COMPAT_VERSION)
            .header("authorization", format!("Bearer {}", self.token));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| ApiError { status: 0, code: None, message: format!("melty.gg ist nicht erreichbar: {e}") })?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(api_error_details(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| ApiError { status, code: None, message: format!("melty.gg sent an invalid response: {e}") })
    }

    async fn library(&self) -> Result<Vec<LibraryMod>, ApiError> {
        let value = self.json(reqwest::Method::GET, "/api/desktop/mods/library", None).await?;
        let response: LibraryResponse = serde_json::from_value(value).map_err(|e| ApiError { status: 0, code: None, message: format!("melty.gg sent an invalid library: {e}") })?;
        Ok(response.mods.into_iter().filter(|m| valid_mod_id(&m.mod_id) && valid_slug(&m.slug)).collect())
    }

    async fn ticket(&self, mod_id: &str) -> Result<Ticket, ApiError> {
        if !valid_mod_id(mod_id) {
            return Err(ApiError { status: 400, code: None, message: "Ungültiges Mashup.".into() });
        }
        let value = self
            .json(reqwest::Method::POST, &format!("/api/desktop/mods/{mod_id}/install-ticket"), Some(json!({})))
            .await?;
        let ticket: Ticket = serde_json::from_value(value).map_err(|e| ApiError { status: 0, code: None, message: format!("melty.gg sent an invalid install ticket: {e}") })?;
        model::validate_ticket(ticket).map_err(|message| ApiError { status: 0, code: None, message })
    }
}

fn api_error_details(status: u16, text: &str) -> ApiError {
    let body = serde_json::from_str::<Value>(text).ok();
    let message = body
        .as_ref()
        .and_then(|value| value.get("error").or_else(|| value.get("message")).and_then(|v| v.as_str()).map(str::to_string))
        .filter(|message| !message.is_empty() && message.len() <= 300)
        .unwrap_or_else(|| format!("melty.gg antwortete mit HTTP {status}."));
    let code = body
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(|value| value.as_str())
        .filter(|code| !code.is_empty() && code.len() <= 80)
        .map(str::to_string);
    ApiError { status, code, message }
}

fn valid_mod_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_slug(value: &str) -> bool {
    model::slug_ok(value)
}

fn api_for(app: &AppHandle) -> Result<(Api, Arc<auth::Account>, Config), String> {
    let state = app.state::<AppState>();
    let token = state.account.token().ok_or_else(|| "Bitte melde dich zuerst bei Melty an.".to_string())?;
    Ok((Api { base: state.config.web_base.clone(), token }, state.account.clone(), state.config.clone()))
}

/// Konto-Befehle (nur für das Panel).
#[tauri::command]
pub fn account_status(webview: Webview, app: AppHandle) -> Result<Value, String> {
    panel::guard(&webview)?;
    let state = app.state::<AppState>();
    Ok(serde_json::to_value(state.account.status()).map_err(|e| e.to_string())?)
}

#[tauri::command]
pub fn account_login(webview: Webview, app: AppHandle) -> Result<(), String> {
    panel::guard(&webview)?;
    let (account, base) = {
        let state = app.state::<AppState>();
        (state.account.clone(), state.config.web_base.clone())
    };
    if account.status().pending {
        return Err("Die Anmeldung läuft bereits.".into());
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = auth::login(app2, account, base).await {
            eprintln!("[auth] {error}");
        }
    });
    Ok(())
}

#[tauri::command]
pub fn account_logout(webview: Webview, app: AppHandle) -> Result<(), String> {
    panel::guard(&webview)?;
    let state = app.state::<AppState>();
    state.account.logout();
    if let Some(window) = app.get_webview_window("auth-import") {
        let _ = window.close();
    }
    Ok(())
}

#[tauri::command]
pub fn mods_state(webview: Webview, state: State<'_, AppState>) -> Result<Value, String> {
    panel::guard(&webview)?;
    Ok(state.mods.snapshot())
}

#[tauri::command]
pub async fn mods_library(webview: Webview, app: AppHandle) -> Result<Value, String> {
    panel::guard(&webview)?;
    let (api, account, _config) = match api_for(&app) {
        Ok((api, account, config)) => (api, account, config),
        Err(_) => return Ok(json!({ "signedIn": false, "mods": [] })),
    };
    match api.library().await {
        Ok(mods) => {
            let journal = { let state = app.state::<AppState>(); state.mods.records() };
            let installed: HashMap<String, String> = journal
                .iter()
                .filter(|record| record.key.starts_with("mod:"))
                .map(|record| (record.mod_id.clone(), record.release_id.clone()))
                .collect();
            let items: Vec<Value> = mods
                .into_iter()
                .map(|m| {
                    let update = installed.get(&m.mod_id).is_some_and(|release| release != &m.release_id);
                    json!({
                        "modId": m.mod_id,
                        "slug": m.slug,
                        "title": m.title,
                        "releaseId": m.release_id,
                        "version": m.version,
                        "gameSlugs": m.game_slugs,
                        "installed": installed.contains_key(&m.mod_id),
                        "update": update,
                    })
                })
                .collect();
            Ok(json!({ "signedIn": true, "mods": items, "account": account.status() }))
        }
        Err(error) if error.status == 401 => {
            account.logout();
            Ok(json!({ "signedIn": false, "mods": [], "error": "Anmeldung abgelaufen. Bitte erneut anmelden." }))
        }
        Err(error) => Err(error.message),
    }
}

#[tauri::command]
pub fn mods_install(
    webview: Webview,
    app: AppHandle,
    mod_id: String,
    slug: Option<String>,
    title: Option<String>,
) -> Result<String, String> {
    panel::guard(&webview)?;
    start_install(&app, &mod_id, slug.as_deref(), title.as_deref())
}

fn start_install(app: &AppHandle, mod_id: &str, slug: Option<&str>, title: Option<&str>) -> Result<String, String> {
    if !valid_mod_id(mod_id) {
        return Err("Ungültiges Mashup.".into());
    }
    let (mods, account, config) = {
        let state = app.state::<AppState>();
        if state.account.token().is_none() {
            return Err("Bitte melde dich zuerst bei Melty an.".into());
        }
        (state.mods.clone(), state.account.clone(), state.config.clone())
    };
    let job = mods.job_start(mod_id, "install")?;
    // Die Oberfläche kennt Name und Slug bereits. Damit kann das Panel auch dann eine sinnvolle
    // Meldung samt „Auf melty.gg holen“ anzeigen, wenn das Ticket noch vor der Antwort scheitert.
    let slug = slug.filter(|slug| valid_slug(slug)).unwrap_or_default();
    let title = title.unwrap_or_default().trim();
    if !slug.is_empty() || !title.is_empty() {
        mods.job_meta(&job, slug, title);
    }
    let app = app.clone();
    let job2 = job.clone();
    let mod_id = mod_id.to_string();
    tauri::async_runtime::spawn(async move {
        let result = install_inner(&app, &mods, &account, &config, &mod_id, &job2).await;
        mods.job_finish(&job2, result);
    });
    Ok(job)
}

#[tauri::command]
pub fn mods_uninstall(webview: Webview, app: AppHandle, mod_id: String, remove_data: Option<bool>) -> Result<String, String> {
    panel::guard(&webview)?;
    if !valid_mod_id(&mod_id) {
        return Err("Ungültiges Mashup.".into());
    }
    let (mods, home, proc_root) = {
        let state = app.state::<AppState>();
        (state.mods.clone(), gamesrt::home(), gamesrt::proc_root())
    };
    if mods.record(&mod_id).is_none() {
        return Err("Dieses Mashup ist nicht installiert.".into());
    }
    let (slug, title) = mods.record(&mod_id).map(|record| (record.slug, record.title)).unwrap_or_default();
    let job = mods.job_start(&mod_id, "uninstall")?;
    mods.job_meta(&job, &slug, &title);
    mods.job_phase(&job, "remove", 0);
    let mods2 = mods.clone();
    let job2 = job.clone();
    let remove_data = remove_data.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let result = uninstall_sync(&mods2, &mod_id, remove_data, false, &home, &proc_root);
        mods2.job_finish(&job2, result);
    });
    Ok(job)
}

#[tauri::command]
pub fn mods_set_enabled(webview: Webview, app: AppHandle, mod_id: String, enabled: bool) -> Result<(), String> {
    panel::guard(&webview)?;
    let (mods, home, proc_root) = {
        let state = app.state::<AppState>();
        (state.mods.clone(), gamesrt::home(), gamesrt::proc_root())
    };
    set_enabled_sync(&mods, &mod_id, enabled, &home, &proc_root)
}

#[tauri::command]
pub async fn mods_set_game_copy(
    webview: Webview,
    app: AppHandle,
    mod_id: String,
    path: Option<String>,
) -> Result<Value, String> {
    panel::guard(&webview)?;
    if !valid_mod_id(&mod_id) {
        return Err("Ungültiges Mashup.".into());
    }
    let (mods, home, proc_root) = {
        let state = app.state::<AppState>();
        (state.mods.clone(), gamesrt::home(), gamesrt::proc_root())
    };
    tauri::async_runtime::spawn_blocking(move || {
        set_game_copy_sync(&mods, &mod_id, path.as_deref(), &home, &proc_root)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn set_game_copy_sync(
    mods: &Arc<Mods>,
    mod_id: &str,
    path: Option<&str>,
    home: &Path,
    proc_root: &Path,
) -> Result<Value, String> {
    let key = Record::key(mod_id);
    let mut record = mods.record(mod_id).ok_or_else(|| "Dieses Mashup ist nicht installiert.".to_string())?;
    let primary = gamecopy::primary_slug(&record)
        .ok_or_else(|| "Dieses Mashup nennt kein Hauptspiel.".to_string())?
        .to_string();
    let previous = record.game_paths.get(&primary).cloned();

    // Auch wenn das Rezept inzwischen ohne Spielkopie startet, muss eine alte Auswahl wieder
    // entfernt werden können.
    let Some(path) = path else {
        if previous.is_none() {
            return Ok(json!({ "active": false }));
        }
        record.game_paths.remove(&primary);
        record.updated_at = journal::now();
        mods.update_record(record)?;
        if let Err(error) = gamecopy::cleanup(&mods.dir, &key) {
            eprintln!("[mods] Spielkopie konnte nicht vollständig bereinigt werden: {error}");
        }
        return Ok(json!({ "active": false }));
    };

    if !gamecopy::available(&record) {
        return Err("Dieses Mashup startet kein Programm aus dem Spielordner oder nennt keinen Versionsbereich; eine Spielkopie ist dafür nicht nötig.".into());
    }
    let current = installed_games(home, proc_root)
        .into_iter()
        .find(|game| {
            record
                .installation_ids
                .get(&primary)
                .is_some_and(|id| id == &game.page.installation_id)
                || game.page.game_id == primary
        })
        .ok_or_else(|| "Das Spiel für dieses Mashup ist nicht installiert.".to_string())?;

    let info = gamecopy::validate(&record, Path::new(path), &current.install_dir, &current.page.display_name)?;
    let root_text = info.root.to_string_lossy().into_owned();
    record.game_paths.insert(primary.clone(), root_text.clone());
    record.updated_at = journal::now();
    mods.update_record(record.clone())?;
    if let Err(error) = gamecopy::apply(&record, &current.install_dir, &info.root, &mods.dir, &primary) {
        let mut revert = record;
        match previous {
            Some(path) => {
                revert.game_paths.insert(primary, path);
            }
            None => {
                revert.game_paths.remove(&primary);
            }
        }
        revert.updated_at = journal::now();
        let _ = mods.update_record(revert);
        return Err(error);
    }
    Ok(json!({
        "active": true,
        "path": root_text,
        "version": info.version.map(|version| version.to_string()),
    }))
}

/// Öffnet den nativen GTK-Ordnerdialog. `FileChooserNative` nutzt unter Wayland das
/// xdg-desktop-portal und bleibt damit auch ohne X11 lauffähig.
#[tauri::command]
pub async fn mods_pick_game_folder(
    webview: Webview,
    app: AppHandle,
    start: Option<String>,
) -> Result<Option<String>, String> {
    panel::guard(&webview)?;
    let home = gamesrt::home();
    let start = start.filter(|path| Path::new(path).is_dir());
    tauri::async_runtime::spawn_blocking(move || pick_game_folder(&app, start, &home))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(unix)]
fn pick_game_folder(app: &AppHandle, start: Option<String>, home: &Path) -> Result<Option<String>, String> {
    use gtk::prelude::*;
    use std::sync::mpsc;

    let (sender, receiver) = mpsc::channel::<Result<Option<String>, String>>();
    let handle = app.clone();
    let start = start.map(PathBuf::from).or_else(|| Some(home.to_path_buf()));
    app.run_on_main_thread(move || {
        if gtk::init().is_err() {
            let _ = sender.send(Err("GTK konnte für den Ordnerdialog nicht initialisiert werden.".into()));
            return;
        }
        let parent = handle.get_webview_window("panel").and_then(|window| window.gtk_window().ok());
        let dialog = gtk::FileChooserNative::new(
            Some("Kompatiblen Spielordner auswählen"),
            parent.as_ref(),
            gtk::FileChooserAction::SelectFolder,
            Some("Auswählen"),
            Some("Abbrechen"),
        );
        dialog.set_modal(true);
        if let Some(start) = &start {
            dialog.set_current_folder(start);
        }
        let response = dialog.run();
        let path = if response == gtk::ResponseType::Accept {
            dialog.filename().map(|path| path.to_string_lossy().into_owned())
        } else {
            None
        };
        dialog.hide();
        let _ = sender.send(Ok(path));
    })
    .map_err(|error| format!("Konnte den Ordnerdialog nicht öffnen: {error}"))?;

    receiver
        .recv_timeout(Duration::from_secs(600))
        .map_err(|_| "Der Ordnerdialog wurde nicht geschlossen.".to_string())?
}

#[cfg(windows)]
fn pick_game_folder(_app: &AppHandle, start: Option<String>, home: &Path) -> Result<Option<String>, String> {
    let start = start.map(PathBuf::from).unwrap_or_else(|| home.to_path_buf());
    let escaped = start.to_string_lossy().replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
         Add-Type -AssemblyName System.Windows.Forms; \
         $dialog = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $dialog.SelectedPath = '{escaped}'; \
         if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $dialog.SelectedPath }}"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|error| format!("Konnte den Ordnerdialog nicht öffnen: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Der Ordnerdialog wurde mit Fehler beendet: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if path.is_empty() { None } else { Some(path) })
}

#[tauri::command]
pub async fn mods_launch(webview: Webview, app: AppHandle, mod_id: String) -> Result<Value, String> {
    panel::guard(&webview)?;
    let record = {
        let state = app.state::<AppState>();
        state.mods.record(&mod_id)
    }
    .ok_or_else(|| "Dieses Mashup ist nicht installiert.".to_string())?;
    launch::launch(app, record).await
}

#[tauri::command]
pub async fn mods_stop(webview: Webview, app: AppHandle, mod_id: String) -> Result<(), String> {
    panel::guard(&webview)?;
    let mods = { let state = app.state::<AppState>(); state.mods.clone() };
    mods.stop(&mod_id).await
}

#[tauri::command]
pub fn mods_open_folder(webview: Webview, app: AppHandle, mod_id: String) -> Result<(), String> {
    panel::guard(&webview)?;
    let (record, mods_dir, home, proc_root) = {
        let state = app.state::<AppState>();
        (state.mods.record(&mod_id), state.mods.dir.clone(), gamesrt::home(), gamesrt::proc_root())
    };
    let record = record.ok_or_else(|| "Dieses Mashup ist nicht installiert.".to_string())?;
    let managed = mods_dir.join("managed").join(&record.slug);
    let path = if managed.is_dir() {
        managed
    } else {
        installed_games(&home, &proc_root)
            .into_iter()
            .find(|game| {
                record
                    .installation_ids
                    .values()
                    .any(|id| id == &game.page.installation_id)
            })
            .map(|game| game.install_dir)
            .ok_or_else(|| "Der Mashup-Ordner wurde nicht gefunden.".to_string())?
    };
    app.opener().open_path(path.to_string_lossy().to_string(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mods_clear_jobs(webview: Webview, state: State<'_, AppState>) -> Result<(), String> {
    panel::guard(&webview)?;
    let mut jobs = state.mods.jobs.lock().unwrap();
    jobs.retain(|_, job| !job.done);
    Ok(())
}

fn installed_games(home: &Path, proc_root: &Path) -> Vec<melty_games::InstalledGame> {
    melty_games::inventory(home, proc_root, 0).games
}

fn steam_ids_from_detail(detail: &Option<Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(games) = detail.as_ref().and_then(|value| value.get("games")).and_then(|value| value.as_array()) else {
        return out;
    };
    for game in games {
        let Some(slug) = game.get("slug").and_then(|v| v.as_str()) else { continue };
        if let Some(app) = game.get("steamAppId").and_then(|v| v.as_str()) {
            if !app.is_empty() && app.bytes().all(|b| b.is_ascii_digit()) {
                out.insert(slug.to_string(), app.to_string());
            }
        }
    }
    out
}

fn map_games(
    recipe: &Recipe,
    app_ids: &HashMap<String, String>,
    games: &[melty_games::InstalledGame],
) -> Result<(HashMap<String, GameEnv>, Option<String>, BTreeMap<String, String>), String> {
    let mut mapped = HashMap::new();
    let mut names = HashMap::new();
    for game in recipe.games.iter() {
        names.insert(game.slug.clone(), game.slug.clone());
    }
    let primary = recipe
        .games
        .iter()
        .find(|game| game.role == "primary")
        .or_else(|| recipe.games.first())
        .map(|game| game.slug.clone());
    for game in &recipe.games {
        let app = game.steam_app_id.clone().or_else(|| app_ids.get(&game.slug).cloned());
        let found = games
            .iter()
            .find(|installed| {
                app.as_ref().is_some_and(|app| &installed.page.store_app_ids.steam == app)
            })
            .or_else(|| games.iter().find(|installed| installed.page.game_id == game.slug))
            .or_else(|| games.iter().find(|installed| melty_games::slug_for(&installed.page.display_name) == game.slug));
        if let Some(found) = found {
            mapped.insert(game.slug.clone(), GameEnv {
                installation_id: found.page.installation_id.clone(),
                install_dir: found.install_dir.clone(),
                prefix: found.proton_prefix.clone(),
                steam_app_id: (!found.page.store_app_ids.steam.is_empty()).then(|| found.page.store_app_ids.steam.clone()),
            });
        }
    }
    if let Some(primary) = &primary {
        if !mapped.contains_key(primary) {
            let name = names.get(primary).cloned().unwrap_or_else(|| primary.clone());
            return Err(format!("Installiere zuerst das Spiel für dieses Mashup ({name})."));
        }
    }
    let installation_ids = mapped.iter().map(|(slug, game)| (slug.clone(), game.installation_id.clone())).collect();
    Ok((mapped, primary, installation_ids))
}

fn download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .user_agent(Config::user_agent())
        .build()
        .map_err(|e| e.to_string())
}

/// Stellt vor dem Start sicher, dass ein vom Rezept verlangter Loader wirklich auf der Platte liegt.
/// Ältere Installationen können einen Loader-Eintrag im Journal haben, dessen Dateien bei einem
/// früheren Update entfernt wurden. Ohne diese Reparatur startet das Spiel, aber die Bridge des
/// Mashups wird nie geladen.
pub(crate) async fn ensure_launch_loader(
    app: &AppHandle,
    mods: &Arc<Mods>,
    loader: &str,
    game: &GameEnv,
    env: &InstallEnv,
) -> Result<(), String> {
    let client = download_client()?;
    let job = format!("launch-loader-{loader}");
    loaders::ensure(app, mods, &client, loader, env, &job).await?;
    launch::ensure_loader_override(loader, game, env)
}

async fn download_file(
    client: &reqwest::Client,
    file: &TicketFile,
    dest: &Path,
    mods: &Mods,
    job: &str,
) -> Result<PathBuf, String> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() == file.size && sha256_file(dest).ok().as_deref() == Some(file.sha256.as_str()) {
            mods.job_add_bytes(job, file.size);
            return Ok(dest.to_path_buf());
        }
        let _ = std::fs::remove_file(dest);
    }
    let temporary = dest.with_extension(format!("{}.part", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    let mut response = client
        .get(&file.url)
        .header("accept", "application/octet-stream")
        .send()
        .await
        .map_err(|e| format!("Download von {} fehlgeschlagen: {e}", file.file_name))?;
    if !response.status().is_success() {
        return Err(format!("Download von {} fehlgeschlagen (HTTP {}).", file.file_name, response.status()));
    }
    let mut out = std::fs::File::create(&temporary).map_err(|e| format!("Could not write the download: {e}"))?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| format!("Download von {} abgebrochen: {e}", file.file_name))?;
        let Some(chunk) = chunk else { break };
        written = written.saturating_add(chunk.len() as u64);
        if written > file.size.saturating_add(8 * 1024 * 1024) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("{} ist größer als im Ticket angegeben.", file.file_name));
        }
        hasher.update(&chunk);
        out.write_all(&chunk).map_err(|e| format!("Could not write the download: {e}"))?;
        mods.job_add_bytes(job, chunk.len() as u64);
    }
    out.flush().ok();
    drop(out);
    let digest = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if digest != file.sha256.to_ascii_lowercase() {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Die Prüfsumme von {} stimmt nicht. Der Download wurde verworfen.", file.file_name));
    }
    if written != file.size {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("{} hat eine unerwartete Größe.", file.file_name));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temporary, dest).map_err(|e| format!("Could not keep the download: {e}"))?;
    Ok(dest.to_path_buf())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        use std::io::Read;
        let read = file.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .take(80)
        .collect()
}

struct Commit {
    files: Vec<InstalledFile>,
    backups: Vec<Backup>,
    roots: Vec<String>,
}

fn commit_planned(
    mods: &Mods,
    key: &str,
    planned: &[model::PlannedFile],
    staging: &Path,
    env: &InstallEnv,
    _job: &str,
) -> Result<Commit, String> {
    let mut files = Vec::new();
    let mut backups = Vec::new();
    let mut roots: Vec<String> = Vec::new();
    let backup_root = journal::backup_dir(&mods.dir, key);
    for (index, planned) in planned.iter().enumerate() {
        let source = staging.join(&planned.component).join(&planned.source);
        let meta = std::fs::symlink_metadata(&source).map_err(|e| format!("Die Datei {} fehlt im Download: {e}", planned.source))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(format!("Die Datei {} ist keine reguläre Datei.", planned.source));
        }
        let resolved = paths::resolve_target(&planned.target, env)?;
        let base = paths::safe_base(&resolved.root)?;
        let dest = paths::safe_join(&base, &resolved.rel)?;
        if roots.iter().all(|root| root != &base.to_string_lossy().to_string()) {
            roots.push(base.to_string_lossy().to_string());
        }
        if dest.exists() {
            if dest.is_dir() {
                return Err(format!("Am Ziel liegt bereits ein Ordner: {}", dest.display()));
            }
            std::fs::create_dir_all(&backup_root).map_err(|e| e.to_string())?;
            let backup_name = format!("{index:04}-{}", sanitize_name(dest.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
            let backup_path = backup_root.join(&backup_name);
            std::fs::copy(&dest, &backup_path).map_err(|e| format!("Could not back up {}: {e}", dest.display()))?;
            backups.push(Backup {
                root: resolved.token.clone(),
                game_slug: resolved.game_slug.clone(),
                rel: resolved.rel.clone(),
                abs: dest.to_string_lossy().to_string(),
                backup: backup_name,
            });
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        std::fs::copy(&source, &dest).map_err(|e| format!("Could not install {}: {e}", planned.source))?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(planned.size);
        let digest = sha256_file(&dest)?;
        files.push(InstalledFile {
            root: resolved.token.clone(),
            game_slug: resolved.game_slug.clone(),
            rel: resolved.rel.clone(),
            abs: dest.to_string_lossy().to_string(),
            sha256: digest,
            size,
            parked: None,
        });
    }
    Ok(Commit { files, backups, roots })
}

fn rollback(commit: &Commit, mods: &Mods, key: &str) {
    for file in commit.files.iter().rev() {
        let path = PathBuf::from(&file.abs);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }
    for backup in &commit.backups {
        let source = journal::backup_dir(&mods.dir, key).join(&backup.backup);
        if source.exists() {
            if let Some(parent) = PathBuf::from(&backup.abs).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(source, &backup.abs);
        }
    }
    let _ = std::fs::remove_dir_all(journal::backup_dir(&mods.dir, key));
}

fn apply_settings(
    mods: &Mods,
    key: &str,
    recipe: &Recipe,
    env: &InstallEnv,
    commit: &mut Commit,
) -> Result<(), String> {
    let backup_root = journal::backup_dir(&mods.dir, key);
    for (index, settings) in recipe.settings.iter().enumerate() {
        let target = model::parse_target(&settings.file).ok_or_else(|| format!("Unsichere Einstellungsdatei: {}", settings.file))?;
        if target.root == "game" {
            return Err("Einstellungsdateien dürfen nicht im Spielordner liegen.".into());
        }
        let resolved = paths::resolve_target(&target, env)?;
        let base = paths::safe_base(&resolved.root)?;
        let dest = paths::safe_join(&base, &resolved.rel)?;
        if commit.files.iter().any(|file| file.abs == dest.to_string_lossy()) {
            continue;
        }
        if dest.exists() {
            std::fs::create_dir_all(&backup_root).map_err(|e| e.to_string())?;
            let name = format!("set-{index:02}-{}", sanitize_name(dest.file_name().and_then(|n| n.to_str()).unwrap_or("settings")));
            let backup = backup_root.join(&name);
            std::fs::copy(&dest, &backup).map_err(|e| format!("Could not back up settings: {e}"))?;
            commit.backups.push(Backup {
                root: resolved.token.clone(),
                game_slug: resolved.game_slug.clone(),
                rel: resolved.rel.clone(),
                abs: dest.to_string_lossy().to_string(),
                backup: name,
            });
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut text = std::fs::read_to_string(&dest).unwrap_or_default();
        let xml = settings.file.to_ascii_lowercase().ends_with(".xml");
        for (name, value) in &settings.set {
            text = set_line(&text, name, value.as_deref(), xml);
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        std::fs::write(&dest, text).map_err(|e| format!("Could not write settings: {e}"))?;
        let digest = sha256_file(&dest)?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        commit.files.push(InstalledFile {
            root: resolved.token.clone(),
            game_slug: resolved.game_slug.clone(),
            rel: resolved.rel.clone(),
            abs: dest.to_string_lossy().to_string(),
            sha256: digest,
            size,
            parked: None,
        });
    }
    Ok(())
}

fn set_line(text: &str, key: &str, value: Option<&str>, xml: bool) -> String {
    if xml {
        let open = format!("<{key}>");
        let close = format!("</{key}>");
        if let Some(start) = text.find(&open) {
            if let Some(end) = text[start..].find(&close) {
                let end = start + end + close.len();
                return match value {
                    Some(value) => format!("{}{open}{}{close}{}", &text[..start], value, &text[end..]),
                    None => format!("{}{}", &text[..start], &text[end..]),
                };
            }
        }
        if let Some(value) = value {
            for root in ["</configuration>", "</root>", "</settings>"] {
                if let Some(pos) = text.find(root) {
                    return format!("{}{open}{}{close}\n{}", &text[..pos], value, &text[pos..]);
                }
            }
            return format!("{text}{open}{}{close}\n", value);
        }
        return text.to_string();
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut found = false;
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(key) {
            if rest.starts_with('=') || rest.starts_with(':') {
                found = true;
                match value {
                    Some(value) => *line = format!("{key}={value}"),
                    None => line.clear(),
                }
                break;
            }
        }
    }
    if !found {
        if let Some(value) = value {
            lines.push(format!("{key}={value}"));
        }
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n") + "\n"
}

fn not_owned_message(mods: &Mods, job: &str) -> String {
    let name = mods.job_title(job).map(|title| format!("„{title}“")).unwrap_or_else(|| "Dieses Mashup".into());
    format!(
        "{name} ist noch nicht in deiner Melty-Bibliothek. Öffne „Auf melty.gg holen“ – die Seite fügt es automatisch hinzu. Klicke danach auf „Erneut versuchen“."
    )
}

/// Holt das Install-Ticket. Ist das Mashup noch nicht in der Bibliothek, wird es – sofern
/// kostenlos – über die vorhandene melty.gg-Websitzung automatisch hinzugefügt und das Ticket
/// anschließend erneut angefordert.
async fn ticket_or_claim(
    app: &AppHandle,
    mods: &Arc<Mods>,
    api: &Api,
    config: &Config,
    token: &str,
    mod_id: &str,
    job: &str,
) -> Result<Ticket, String> {
    let error = match api.ticket(mod_id).await {
        Ok(ticket) => return Ok(ticket),
        Err(error) => error,
    };
    if error.code.as_deref() != Some("not-owned") {
        return Err(error.message);
    }

    let Some(slug) = mods.job_slug(job) else { return Err(not_owned_message(mods, job)) };
    let Ok(detail) = mashups::detail(&config.web_base, &slug).await else {
        return Err(not_owned_message(mods, job));
    };
    if !claim::is_free(&detail) {
        let name = mods.job_title(job).map(|title| format!("„{title}“")).unwrap_or_else(|| "Dieses Mashup".into());
        mods.job_error_code(job, "paid");
        return Err(format!(
            "{name} ist kostenpflichtig. Käufe lassen sich in diesem Client nicht abschließen; hole es einmalig auf melty.gg."
        ));
    }

    mods.job_phase(job, "claim", 0);
    match claim::claim_free(app, &config.web_base, &slug, mod_id, token).await {
        Ok(()) => {}
        Err(claim::ClaimError::NoSession) => {
            let name = mods.job_title(job).map(|title| format!("„{title}“")).unwrap_or_else(|| "Dieses Mashup".into());
            mods.job_error_code(job, "claim-session");
            return Err(format!(
                "{name} konnte nicht automatisch zur Bibliothek hinzugefügt werden: Deine melty.gg-Sitzung ist abgelaufen. Melde dich im Client neu an und starte die Installation erneut."
            ));
        }
        Err(claim::ClaimError::Failed(message)) => {
            let name = mods.job_title(job).map(|title| format!("„{title}“")).unwrap_or_else(|| "Dieses Mashup".into());
            mods.job_error_code(job, "claim-failed");
            return Err(format!("{name} konnte nicht zur Bibliothek hinzugefügt werden: {message}"));
        }
    }

    match api.ticket(mod_id).await {
        Ok(ticket) => Ok(ticket),
        Err(error) if error.code.as_deref() == Some("not-owned") => {
            mods.job_error_code(job, "not-owned");
            Err(not_owned_message(mods, job))
        }
        Err(error) => Err(error.message),
    }
}

async fn install_inner(
    app: &AppHandle,
    mods: &Arc<Mods>,
    account: &Arc<auth::Account>,
    config: &Config,
    mod_id: &str,
    job: &str,
) -> Result<(), String> {
    let _guard = mods.install_lock.lock().await;
    let token = account.token().ok_or_else(|| "Bitte melde dich zuerst bei Melty an.".to_string())?;
    let api = Api { base: config.web_base.clone(), token: token.clone() };
    mods.job_phase(job, "ticket", 0);
    let ticket = ticket_or_claim(app, mods, &api, config, &token, mod_id, job).await?;
    mods.job_meta(job, &ticket.mashup.slug, &ticket.mashup.title);

    if let Some(existing) = mods.record(mod_id) {
        if existing.release_id == ticket.release.id {
            return Err("Dieses Mashup ist bereits in dieser Version installiert.".into());
        }
    }

    // Abhängigkeiten werden noch nicht automatisch mitinstalliert, aber ein unvollständiger Start
    // wäre schlimmer als eine klare Meldung.
    if !ticket.depends_on.is_empty() {
        let installed: HashSet<String> = mods
            .records()
            .into_iter()
            .filter(|record| record.key.starts_with("mod:"))
            .map(|record| record.slug)
            .collect();
        let missing: Vec<String> = ticket
            .depends_on
            .iter()
            .filter(|dependency| !installed.contains(&dependency.slug))
            .map(|dependency| dependency.slug.clone())
            .collect();
        if !missing.is_empty() {
            mods.job_error_code(job, "dependency-missing");
            return Err(format!(
                "Dieses Mashup benötigt zuerst: {}. Installiere diese Mashups zuerst.",
                missing.join(", ")
            ));
        }
    }

    let home = gamesrt::home();
    let proc_root = gamesrt::proc_root();
    let games = installed_games(&home, &proc_root);
    let detail = mashups::detail(&config.web_base, &ticket.mashup.slug).await.ok();
    let app_ids = steam_ids_from_detail(&detail);
    let (mut game_envs, primary, installation_ids) = map_games(&ticket.recipe, &app_ids, &games)?;
    // Rezepte mit %LOCALAPPDATA% & Co. müssen in das Proton-Präfix schreiben. Bei einem Steam-Spiel,
    // das noch nie lief, existiert das Präfix noch nicht; dann jetzt einmalig anlegen, bevor Dateien
    // geplant werden.
    if paths::recipe_uses_windows_profile(&ticket.recipe) {
        if let Some(slug) = &primary {
            if let Some(game_env) = game_envs.get(slug) {
                let prefix_missing = game_env
                    .prefix
                    .as_ref()
                    .map_or(true, |prefix| paths::wine_root(prefix).is_none());
                if prefix_missing && game_env.steam_app_id.is_some() {
                    if let Some(game) = games
                        .iter()
                        .find(|game| game.page.installation_id == game_env.installation_id)
                        .cloned()
                    {
                        mods.job_phase(job, "prefix", 0);
                        let home2 = home.clone();
                        let prefix = tauri::async_runtime::spawn_blocking(move || launch::ensure_prefix(&game, &home2))
                            .await
                            .map_err(|error| format!("Konnte das Spielpräfix nicht vorbereiten: {error}"))??;
                        if let Some(game_env) = game_envs.get_mut(slug) {
                            game_env.prefix = Some(prefix);
                        }
                    }
                }
            }
        }
    }
    let steam_root = primary
        .as_ref()
        .and_then(|slug| game_envs.get(slug))
        .and_then(paths::steam_root_from)
        .or_else(|| melty_games::steam::find_roots(&home).first().map(|root| root.path.clone()));
    let env = InstallEnv {
        home: home.clone(),
        mods_dir: mods.dir.clone(),
        mod_slug: ticket.mashup.slug.clone(),
        games: game_envs,
        primary_slug: primary.clone(),
        steam_root,
    };

    // Loader, die das Rezept verlangt, müssen unterstützt werden – bevor irgendetwas geschrieben wird.
    let mut loaders = Vec::new();
    for requirement in &ticket.recipe.requirements {
        if requirement.kind == "loader" && !loaders.contains(&requirement.id) {
            if loaders::pin(&requirement.id).is_none() {
                return Err(format!(
                    "Dieses Mashup braucht den Loader „{}“, den dieser Client noch nicht selbst installieren kann.",
                    requirement.id
                ));
            }
            loaders.push(requirement.id.clone());
        }
    }

    let used: HashSet<String> = ticket
        .recipe
        .components
        .iter()
        .filter(|component| component.installs())
        .map(|component| component.file_name.clone())
        .collect();
    let files: Vec<&TicketFile> = ticket.files.iter().filter(|file| used.contains(&file.file_name)).collect();
    let total: u64 = files.iter().map(|file| file.size).sum::<u64>();
    mods.job_phase(job, "download", total);

    let client = download_client()?;
    let downloads = mods.dir.join("downloads");
    std::fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;
    let mut downloaded: HashMap<String, PathBuf> = HashMap::new();
    for file in &files {
        let dest = downloads.join(&file.sha256.to_ascii_lowercase());
        let path = download_file(&client, file, &dest, mods, job).await?;
        downloaded.insert(file.file_name.clone(), path);
    }

    mods.job_phase(job, "unpack", 0);
    let staging = mods.dir.join("staging").join(job);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let mut staged: BTreeMap<String, Vec<model::StagedEntry>> = BTreeMap::new();
    for component in ticket.recipe.components.iter().filter(|component| component.installs()) {
        let file = downloaded
            .get(&component.file_name)
            .ok_or_else(|| format!("Der Download für {} fehlt.", component.file_name))?;
        let target = staging.join(&component.id);
        let entries = if component.unpacks() {
            archive::list(file)?;
            archive::extract(file, &target)?
        } else {
            archive::stage_single(file, &target, &component.file_name)?
        };
        if entries.is_empty() {
            return Err(format!("{} enthält keine Dateien.", component.file_name));
        }
        staged.insert(component.id.clone(), entries);
    }
    let planned = model::plan_files(&ticket.recipe, &staged);
    if planned.is_empty() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("Das Rezept dieses Mashups legt keine Dateien ab.".into());
    }
    let mut seen = HashSet::new();
    for file in &planned {
        let key = format!("{}:{:?}:{}", file.target.root, file.target.game_slug, file.target.subpath);
        if !seen.insert(key) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err("Das Rezept legt zwei Dateien an denselben Ort.".into());
        }
    }

    // Loader zuerst: sie sind von mehreren Mashups geteilt und bleiben beim Entfernen der alten
    // Version bestehen. Erst danach wird die alte Version ersetzt.
    for loader in &loaders {
        if loaders::provided_by_recipe(loader, &planned) {
            continue;
        }
        loaders::ensure(app, mods, &client, loader, &env, job).await?;
    }
    // Ein Update ersetzt den Datensatz, darf aber eine bereits gewählte Spielkopie nicht verlieren.
    let previous_game_paths = mods.record(mod_id).map(|record| record.game_paths).unwrap_or_default();
    if mods.record(mod_id).is_some() {
        mods.job_phase(job, "update", 0);
        uninstall_sync(mods, mod_id, false, true, &home, &proc_root)?;
    }

    let key = Record::key(mod_id);
    mods.job_phase(job, "install", 0);
    let mut commit = commit_planned(mods, &key, &planned, &staging, &env, job)?;
    if let Err(error) = apply_settings(mods, &key, &ticket.recipe, &env, &mut commit) {
        rollback(&commit, mods, &key);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let mut override_error = None;
    if let Some(game) = env.primary_slug.as_ref().and_then(|slug| env.games.get(slug)) {
        for loader in &loaders {
            if let Err(error) = launch::ensure_loader_override(loader, game, &env) {
                override_error = Some(error);
                break;
            }
        }
    }
    if let Some(error) = override_error {
        rollback(&commit, mods, &key);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let mut runtime_data = Vec::new();
    for entry in &ticket.recipe.runtime_data {
        if let Some(target) = model::parse_target(entry) {
            if let Ok(resolved) = paths::resolve_target(&target, &env) {
                if target.root != "game" && !target.subpath.is_empty() {
                    let abs = resolved.path().to_string_lossy().to_string();
                    runtime_data.push(StoredTarget {
                        root: resolved.token,
                        game_slug: resolved.game_slug,
                        rel: resolved.rel,
                        abs,
                    });
                }
            }
        }
    }
    let record = Record {
        key: key.clone(),
        mod_id: mod_id.to_string(),
        slug: ticket.mashup.slug.clone(),
        title: ticket.mashup.title.clone(),
        release_id: ticket.release.id.clone(),
        version: ticket.release.version.clone(),
        game_slugs: ticket.recipe.games.iter().map(|game| game.slug.clone()).collect(),
        primary_game_slug: primary,
        installation_ids,
        enabled: true,
        installed_at: journal::now(),
        updated_at: journal::now(),
        files: commit.files.clone(),
        backups: commit.backups.clone(),
        runtime_data,
        roots: commit.roots.clone(),
        recipe: ticket.recipe.clone(),
        loaders,
        game_paths: previous_game_paths
            .into_iter()
            .filter(|(slug, _)| ticket.recipe.games.iter().any(|game| &game.slug == slug))
            .collect(),
    };
    let saved = {
        let mut journal = mods.journal.lock().unwrap();
        let previous = journal.get(&key).cloned();
        journal.upsert(record);
        match mods.save_journal(&journal) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(previous) = previous {
                    journal.upsert(previous);
                } else {
                    journal.remove(&key);
                }
                Err(error)
            }
        }
    };
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(error) = saved {
        rollback(&commit, mods, &key);
        return Err(error);
    }
    mods.job_phase(job, "done", 0);
    Ok(())
}

fn uninstall_sync(
    mods: &Mods,
    mod_id: &str,
    remove_data: bool,
    keep_loaders: bool,
    home: &Path,
    proc_root: &Path,
) -> Result<(), String> {
    let key = Record::key(mod_id);
    let record = mods.journal.lock().unwrap().get(&key).cloned().ok_or_else(|| "Dieses Mashup ist nicht installiert.".to_string())?;
    if mods.is_running(mod_id) {
        return Err("Beende zuerst das Spiel.".into());
    }
    if let Some(primary) = &record.primary_game_slug {
        if let Some(id) = record.installation_ids.get(primary) {
            if installed_games(home, proc_root).iter().any(|game| &game.page.installation_id == id && game.page.running) {
                return Err("Beende zuerst das Spiel.".into());
            }
        }
    }
    remove_record_files(mods, &record, remove_data)?;
    if let Err(error) = gamecopy::cleanup(&mods.dir, &record.key) {
        eprintln!("[mods] Spielkopie konnte nicht vollständig bereinigt werden: {error}");
    }
    {
        let mut journal = mods.journal.lock().unwrap();
        journal.remove(&key);
        mods.save_journal(&journal)?;
    }
    let _ = std::fs::remove_dir_all(mods.dir.join("managed").join(&record.slug));
    let _ = std::fs::remove_dir_all(mods.dir.join("minecraft").join(&record.slug));
    let _ = std::fs::remove_dir_all(journal::parked_dir(&mods.dir, &key));
    let _ = std::fs::remove_dir_all(journal::backup_dir(&mods.dir, &key));
    if !keep_loaders {
        let remaining = mods.journal.lock().unwrap().records.clone();
        for loader in &record.loaders {
            let still_used = remaining.iter().any(|other| other.key.starts_with("mod:") && other.loaders.contains(loader));
            if still_used {
                continue;
            }
            let loader_key = loaders::record_key(loader, record.primary_game_slug.as_deref().unwrap_or(""));
            if let Some(loader_record) = mods.journal.lock().unwrap().get(&loader_key).cloned() {
                let _ = remove_record_files(mods, &loader_record, true);
                let mut journal = mods.journal.lock().unwrap();
                journal.remove(&loader_key);
                let _ = mods.save_journal(&journal);
            }
        }
    }
    Ok(())
}

fn remove_record_files(mods: &Mods, record: &Record, remove_data: bool) -> Result<(), String> {
    let allowed: Vec<PathBuf> = record
        .roots
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(mods.dir.clone()))
        .collect();
    let mut removed_dirs: HashSet<PathBuf> = HashSet::new();
    for file in &record.files {
        let target = PathBuf::from(&file.abs);
        if file.abs.is_empty() || !allowed.iter().any(|root| paths::is_within(root, &target)) {
            eprintln!("[mods] refusing to remove file outside the journal roots: {}", file.abs);
            continue;
        }
        let parked = file.parked.as_ref().map(|parked| journal::parked_dir(&mods.dir, &record.key).join(parked));
        if let Some(parked) = parked {
            let _ = std::fs::remove_file(parked);
            continue;
        }
        if !target.exists() {
            continue;
        }
        if sha256_file(&target).ok().as_deref() == Some(file.sha256.as_str()) {
            if let Some(parent) = target.parent() {
                removed_dirs.insert(parent.to_path_buf());
            }
            std::fs::remove_file(&target).map_err(|e| format!("Could not remove {}: {e}", target.display()))?;
        } else {
            eprintln!("[mods] leaving changed file in place: {}", target.display());
        }
    }
    for backup in &record.backups {
        let target = PathBuf::from(&backup.abs);
        if !allowed.iter().any(|root| paths::is_within(root, &target)) {
            continue;
        }
        if !target.exists() {
            let source = journal::backup_dir(&mods.dir, &record.key).join(&backup.backup);
            if source.exists() {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(source, &target);
            }
        }
    }
    for dir in removed_dirs {
        let mut current = dir;
        while allowed.iter().any(|root| paths::is_within(root, &current)) && current != mods.dir {
            if std::fs::remove_dir(&current).is_err() {
                break;
            }
            let Some(parent) = current.parent() else { break };
            current = parent.to_path_buf();
        }
    }
    if remove_data {
        for target in &record.runtime_data {
            let path = PathBuf::from(&target.abs);
            if !target.rel.is_empty() && allowed.iter().any(|root| paths::is_within(root, &path)) && path.exists() {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
    Ok(())
}

fn set_enabled_sync(mods: &Mods, mod_id: &str, enabled: bool, home: &Path, proc_root: &Path) -> Result<(), String> {
    let key = Record::key(mod_id);
    let mut record = mods.journal.lock().unwrap().get(&key).cloned().ok_or_else(|| "Dieses Mashup ist nicht installiert.".to_string())?;
    if record.enabled == enabled {
        return Ok(());
    }
    if enabled {
        // Dateien zurücklegen.
        let parked_root = journal::parked_dir(&mods.dir, &key);
        for file in record.files.iter_mut() {
            let Some(parked) = file.parked.clone() else { continue };
            let source = parked_root.join(&parked);
            let target = PathBuf::from(&file.abs);
            if target.exists() {
                return Err(format!("Die Datei {} existiert bereits; bitte das Mashup neu installieren.", target.display()));
            }
            if source.exists() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                match std::fs::rename(&source, &target) {
                    Ok(()) => {}
                    Err(_) => {
                        std::fs::copy(&source, &target).map_err(|e| format!("Could not restore {}: {e}", target.display()))?;
                        let _ = std::fs::remove_file(&source);
                    }
                }
            }
            file.parked = None;
        }
        let _ = std::fs::remove_dir_all(&parked_root);
    } else {
        if mods.is_running(mod_id) {
            return Err("Beende zuerst das Spiel.".into());
        }
        if let Some(primary) = &record.primary_game_slug {
            if let Some(id) = record.installation_ids.get(primary) {
                if installed_games(home, proc_root).iter().any(|game| &game.page.installation_id == id && game.page.running) {
                    return Err("Beende zuerst das Spiel.".into());
                }
            }
        }
        let parked_root = journal::parked_dir(&mods.dir, &key);
        std::fs::create_dir_all(&parked_root).map_err(|e| e.to_string())?;
        for (index, file) in record.files.iter_mut().enumerate() {
            if file.parked.is_some() {
                continue;
            }
            let target = PathBuf::from(&file.abs);
            if !target.exists() {
                continue;
            }
            let name = format!("{index:04}-{}", sanitize_name(target.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
            let parked = parked_root.join(&name);
            match std::fs::rename(&target, &parked) {
                Ok(()) => {}
                Err(_) => {
                    std::fs::copy(&target, &parked).map_err(|e| format!("Could not disable {}: {e}", target.display()))?;
                    std::fs::remove_file(&target).map_err(|e| format!("Could not disable {}: {e}", target.display()))?;
                }
            }
            file.parked = Some(name);
        }
        if let Err(error) = gamecopy::cleanup(&mods.dir, &key) {
            eprintln!("[mods] Spielkopie konnte beim Deaktivieren nicht bereinigt werden: {error}");
        }
    }
    record.enabled = enabled;
    record.updated_at = journal::now();
    let mut journal = mods.journal.lock().unwrap();
    journal.upsert(record);
    mods.save_journal(&journal)
}

mod loaders {
    use super::*;

    pub struct Pin {
        pub id: &'static str,
        pub name: &'static str,
        pub url: &'static str,
        pub file_name: &'static str,
        pub size: u64,
        pub sha256: &'static str,
        pub extract_from: &'static str,
        pub target: &'static str,
        pub launch: Option<&'static str>,
    }

    const PINS: &[Pin] = &[
        Pin {
            id: "bepinex-5",
            name: "BepInEx 5",
            url: "https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.5/BepInEx_win_x64_5.4.23.5.zip",
            file_name: "BepInEx_win_x64_5.4.23.5.zip",
            size: 639118,
            sha256: "82f9878551030f54657792c0740d9d51a09500eeae1fba21106b0c441e6732c4",
            extract_from: "",
            target: "{game}",
            launch: None,
        },
        Pin {
            id: "bepinex-6-il2cpp",
            name: "BepInEx 6 (IL2CPP)",
            url: "https://github.com/BepInEx/BepInEx/releases/download/v6.0.0-pre.2/BepInEx-Unity.IL2CPP-win-x64-6.0.0-pre.2.zip",
            file_name: "BepInEx-Unity.IL2CPP-win-x64-6.0.0-pre.2.zip",
            size: 34146254,
            sha256: "616ec7eb06cf11b2a0000e8fcef04d1b12bb58e84a2e0bdac9523234fc193ceb",
            extract_from: "",
            target: "{game}",
            launch: None,
        },
        Pin {
            id: "bepinexpack-peak",
            name: "BepInEx für PEAK",
            url: "https://thunderstore.io/package/download/BepInEx/BepInExPack_PEAK/5.4.75301/",
            file_name: "BepInEx-BepInExPack_PEAK-5.4.75301.zip",
            size: 670172,
            sha256: "a71432613ab0b2f560046bc341d949c9d853b4f5d5223ab5e7f2ef1f113522e7",
            extract_from: "BepInExPack_PEAK/",
            target: "{game}",
            launch: None,
        },
        Pin {
            id: "melonloader",
            name: "MelonLoader",
            url: "https://github.com/LavaGang/MelonLoader/releases/download/v0.7.3/MelonLoader.x64.zip",
            file_name: "MelonLoader.x64.zip",
            size: 20155622,
            sha256: "5b2b2f3d1cd42b59ec886c5bdc2663edae87a0097a4f4a8f58c0965a99dda416",
            extract_from: "",
            target: "{game}",
            launch: None,
        },
        Pin {
            id: "owml",
            name: "OWML",
            url: "https://github.com/ow-mods/owml/releases/download/2.16.3/OWML.zip",
            file_name: "OWML.zip",
            size: 9159084,
            sha256: "6aaffdc7e958fe05f8c096e1eb69a0b6241facadbef624177d7573089b9bd2ed",
            extract_from: "",
            target: "{appdata}/OuterWildsModManager/OWML",
            launch: Some("{appdata}/OuterWildsModManager/OWML/OWML.Launcher.exe"),
        },
        Pin {
            id: "ultimate-asi-loader",
            name: "Ultimate ASI Loader",
            url: "https://github.com/ThirteenAG/Ultimate-ASI-Loader/releases/download/v10.0.0/Ultimate-ASI-Loader-NoPDB_x64.zip",
            file_name: "Ultimate-ASI-Loader-NoPDB_x64.zip",
            size: 899742,
            sha256: "3385950d6af79257d18420b1d6904bc47480170b3dabe345f8c1339d0a49183f",
            extract_from: "",
            target: "{game}",
            launch: None,
        },
    ];

    /// Ultimate ASI Loader ships as a proxy DLL. Some mashups already include a compatible loader
    /// themselves; installing our copy before their files would only be overwritten again.
    pub fn provided_by_recipe(id: &str, planned: &[model::PlannedFile]) -> bool {
        id == "ultimate-asi-loader"
            && planned.iter().any(|file| {
                file.target.root == "game"
                    && matches!(
                        file.target.subpath.to_ascii_lowercase().as_str(),
                        "dinput8.dll" | "version.dll" | "winmm.dll" | "dsound.dll"
                    )
            })
    }

    pub fn pin(id: &str) -> Option<&'static Pin> {
        PINS.iter().find(|pin| pin.id == id)
    }

    pub fn record_key(id: &str, game_slug: &str) -> String {
        format!("loader:{id}:{game_slug}")
    }

    pub fn launch_for(id: &str) -> Option<&'static str> {
        pin(id).and_then(|pin| pin.launch)
    }

    fn markers(id: &str, game_dir: &Path) -> bool {
        match id {
            "bepinex-5" => game_dir.join("BepInEx/core/BepInEx.Preloader.dll").exists(),
            // Beim PEAK-Paket müssen auch der Doorstop und die Proxy-DLL vorhanden sein; sonst
            // startet das Spiel zwar, lädt aber keine Plugins und damit keine Bridge.
            "bepinexpack-peak" => {
                game_dir.join("BepInEx/core/BepInEx.Preloader.dll").exists()
                    && game_dir.join("winhttp.dll").is_file()
                    && game_dir.join("doorstop_config.ini").is_file()
            }
            "bepinex-6-il2cpp" => game_dir.join("BepInEx/core/BepInEx.Unity.IL2CPP.dll").exists(),
            "melonloader" => game_dir.join("MelonLoader/MelonLoader.dll").exists() || game_dir.join("MelonLoader/net6/MelonLoader.dll").exists(),
            "owml" => false,
            "ultimate-asi-loader" => std::fs::metadata(game_dir.join("dinput8.dll"))
                .is_ok_and(|meta| meta.len() == 2431208),
            _ => false,
        }
    }

    pub async fn ensure(
        _app: &AppHandle,
        mods: &Arc<Mods>,
        client: &reqwest::Client,
        id: &str,
        env: &InstallEnv,
        job: &str,
    ) -> Result<(), String> {
        let pin = pin(id).ok_or_else(|| format!("Loader {id} wird nicht unterstützt."))?;
        let Some(primary) = &env.primary_slug else {
            return Err("Dieses Mashup braucht ein Spiel, bevor ein Loader installiert werden kann.".into());
        };
        let Some(game) = env.games.get(primary) else {
            return Err("Das Spiel für den Loader wurde nicht gefunden.".into());
        };
        let key = record_key(id, primary);
        let recorded = mods.journal.lock().unwrap().get(&key).is_some();
        // OWML hat keinen Datei-Marker. Bei allen anderen Loadern darf ein alter Journal-Eintrag
        // eine beschädigte Installation nicht verdecken: fehlt der Marker, wird neu installiert.
        if recorded && (id == "owml" || markers(id, &game.install_dir)) {
            return Ok(());
        }
        let target = model::parse_target(pin.target).ok_or_else(|| "Ungültiges Loader-Ziel.".to_string())?;
        if id != "owml" && markers(id, &game.install_dir) {
            // Ein anderer Mod-Manager hat den Loader schon installiert: nichts überschreiben.
            let record = Record {
                key: key.clone(),
                mod_id: id.to_string(),
                slug: format!("loader-{id}-{primary}"),
                title: pin.name.to_string(),
                release_id: pin.sha256.to_string(),
                version: pin.file_name.to_string(),
                game_slugs: vec![primary.clone()],
                primary_game_slug: Some(primary.clone()),
                installation_ids: BTreeMap::from([(primary.clone(), game.installation_id.clone())]),
                enabled: true,
                installed_at: journal::now(),
                updated_at: journal::now(),
                files: vec![],
                backups: vec![],
                runtime_data: vec![],
                roots: vec![game.install_dir.to_string_lossy().to_string()],
                recipe: Recipe::default(),
                loaders: vec![id.to_string()],
                game_paths: BTreeMap::new(),
            };
            let mut journal = mods.journal.lock().unwrap();
            journal.upsert(record);
            mods.save_journal(&journal)?;
            return Ok(());
        }
        mods.job_total_add(job, pin.size);
        mods.job_phase(job, "loader", 0);
        let file = TicketFile {
            file_name: pin.file_name.to_string(),
            size: pin.size,
            sha256: pin.sha256.to_string(),
            url: pin.url.to_string(),
        };
        let dest = mods.dir.join("downloads").join(pin.sha256);
        let archive = download_file(client, &file, &dest, mods, job).await?;
        let staging = mods.dir.join("staging").join(format!("{job}-loader-{id}"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        let entries = archive::extract(&archive, &staging)?;
        let filtered: Vec<model::StagedEntry> = entries
            .into_iter()
            .filter_map(|entry| {
                if pin.extract_from.is_empty() {
                    Some(entry)
                } else {
                    entry.path.strip_prefix(pin.extract_from).map(|rest| model::StagedEntry { path: rest.to_string(), size: entry.size })
                }
            })
            .collect();
        if filtered.is_empty() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("Der Loader-Download {} enthält keine passenden Dateien.", pin.file_name));
        }
        let root = paths::resolve_target(&target, env)?.root;
        let base = paths::safe_base(&root)?;
        let mut files = Vec::new();
        let mut backups = Vec::new();
        let backup_root = journal::backup_dir(&mods.dir, &key);
        for (index, entry) in filtered.iter().enumerate() {
            let source = staging.join(&entry.path);
            if !source.is_file() {
                continue;
            }
            let dest = paths::safe_join(&base, &entry.path)?;
            if dest.exists() {
                if dest.is_dir() {
                    let _ = std::fs::remove_dir_all(&staging);
                    return Err(format!("Loader-Ziel ist ein Ordner: {}", dest.display()));
                }
                std::fs::create_dir_all(&backup_root).map_err(|e| e.to_string())?;
                let name = format!("{index:04}-{}", sanitize_name(dest.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
                let backup = backup_root.join(&name);
                std::fs::copy(&dest, &backup).map_err(|e| e.to_string())?;
                backups.push(Backup {
                    root: target.root.clone(),
                    game_slug: target.game_slug.clone().or_else(|| Some(primary.clone())),
                    rel: entry.path.clone(),
                    abs: dest.to_string_lossy().to_string(),
                    backup: name,
                });
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&source, &dest).map_err(|e| format!("Could not install loader file {}: {e}", entry.path))?;
            let digest = sha256_file(&dest)?;
            files.push(InstalledFile {
                root: target.root.clone(),
                game_slug: target.game_slug.clone().or_else(|| Some(primary.clone())),
                rel: entry.path.clone(),
                abs: dest.to_string_lossy().to_string(),
                sha256: digest,
                size: entry.size,
                parked: None,
            });
        }
        let _ = std::fs::remove_dir_all(&staging);
        let record = Record {
            key: key.clone(),
            mod_id: id.to_string(),
            slug: format!("loader-{id}-{primary}"),
            title: pin.name.to_string(),
            release_id: pin.sha256.to_string(),
            version: pin.file_name.to_string(),
            game_slugs: vec![primary.clone()],
            primary_game_slug: Some(primary.clone()),
            installation_ids: BTreeMap::from([(primary.clone(), game.installation_id.clone())]),
            enabled: true,
            installed_at: journal::now(),
            updated_at: journal::now(),
            files,
            backups,
            runtime_data: vec![],
            roots: vec![base.to_string_lossy().to_string()],
            recipe: Recipe::default(),
            loaders: vec![id.to_string()],
            game_paths: BTreeMap::new(),
        };
        let mut journal = mods.journal.lock().unwrap();
        journal.upsert(record);
        mods.save_journal(&journal)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "melty-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn api_errors_keep_the_code_for_not_owned() {
        let error = api_error_details(403, r#"{"error":"Get this mashup first.","code":"not-owned"}"#);
        assert_eq!(error.status, 403);
        assert_eq!(error.code.as_deref(), Some("not-owned"));
        assert_eq!(error.message, "Get this mashup first.");

        let plain = api_error_details(500, "not json");
        assert_eq!(plain.code, None);
        assert_eq!(plain.message, "melty.gg antwortete mit HTTP 500.");
    }

    #[test]
    fn asi_loader_is_pinned_and_recipe_provided_copy_is_detected() {
        assert!(loaders::pin("ultimate-asi-loader").is_some());
        let planned = vec![model::PlannedFile {
            component: "main".into(),
            source: "dinput8.dll".into(),
            target: model::parse_target("{game}/dinput8.dll").unwrap(),
            size: 1,
        }];
        assert!(loaders::provided_by_recipe("ultimate-asi-loader", &planned));
        assert!(!loaders::provided_by_recipe("melonloader", &planned));
        let elsewhere = vec![model::PlannedFile {
            component: "main".into(),
            source: "dinput8.dll".into(),
            target: model::parse_target("{localappdata}/Thing/dinput8.dll").unwrap(),
            size: 1,
        }];
        assert!(!loaders::provided_by_recipe("ultimate-asi-loader", &elsewhere));
    }

    #[test]
    fn commit_disable_enable_and_uninstall_roundtrip() {
        let root = temp_root("roundtrip");
        let game = root.join("game");
        fs::create_dir_all(game.join("BepInEx/plugins")).unwrap();
        let mods_dir = root.join("mods");
        let mods = Mods::new(mods_dir.clone());
        let staging = root.join("staging");
        fs::create_dir_all(staging.join("main")).unwrap();
        fs::write(staging.join("main/demo.dll"), b"hello").unwrap();

        let env = InstallEnv {
            home: root.join("home"),
            mods_dir: mods_dir.clone(),
            mod_slug: "demo".into(),
            games: HashMap::from([(
                "demo-game".to_string(),
                GameEnv {
                    installation_id: "1".into(),
                    install_dir: game.clone(),
                    prefix: None,
                    steam_app_id: None,
                },
            )]),
            primary_slug: Some("demo-game".into()),
            steam_root: None,
        };
        let dest = game.join("BepInEx/plugins/demo.dll");
        fs::write(&dest, b"original").unwrap();
        let planned = vec![model::PlannedFile {
            component: "main".into(),
            source: "demo.dll".into(),
            target: model::parse_target("{game}/BepInEx/plugins/demo.dll").unwrap(),
            size: 5,
        }];
        let commit = commit_planned(&mods, "mod:test", &planned, &staging, &env, "job").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert_eq!(commit.backups.len(), 1);

        let record = Record {
            key: "mod:test".into(),
            mod_id: "test".into(),
            slug: "demo".into(),
            title: "Demo".into(),
            release_id: "r1".into(),
            version: "1".into(),
            game_slugs: vec!["demo-game".into()],
            primary_game_slug: Some("demo-game".into()),
            installation_ids: BTreeMap::from([("demo-game".into(), "1".into())]),
            enabled: true,
            installed_at: journal::now(),
            updated_at: journal::now(),
            files: commit.files.clone(),
            backups: commit.backups.clone(),
            runtime_data: vec![],
            roots: commit.roots.clone(),
            recipe: Recipe::default(),
            loaders: vec![],
            game_paths: BTreeMap::new(),
        };
        {
            let mut saved = mods.journal.lock().unwrap();
            saved.upsert(record);
            mods.save_journal(&saved).unwrap();
        }

        set_enabled_sync(&mods, "test", false, &root.join("home"), &root.join("proc")).unwrap();
        assert!(!dest.exists());
        assert!(mods.record("test").unwrap().files[0].parked.is_some());
        set_enabled_sync(&mods, "test", true, &root.join("home"), &root.join("proc")).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");

        uninstall_sync(&mods, "test", false, false, &root.join("home"), &root.join("proc")).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"original");
        assert!(mods.record("test").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn companions_keep_a_mashup_running_until_they_exit() {
        let root = std::env::temp_dir().join(format!(
            "melty-companions-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mods = Mods::new(root.join("mods"));
        assert!(!mods.is_running("mod:companion"));
        mods.add_companion(RunningProcess {
            mod_id: "mod:companion".into(),
            slug: "companion".into(),
            pid: 4242,
            started_at: journal::now(),
            stoppable: true,
        });
        assert!(mods.is_running("mod:companion"));
        let views = mods.running_views();
        assert!(views.iter().any(|view| view.mod_id == "mod:companion" && view.stoppable));
        mods.clear_companion("mod:companion", 4242);
        assert!(!mods.is_running("mod:companion"));
        let _ = fs::remove_dir_all(&root);
    }
}
