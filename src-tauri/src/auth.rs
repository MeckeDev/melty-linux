//! Anmeldung per Browser-Handoff (`auth.js` des Originals, Konstanten identisch):
//! Seite → `openLogin(href)` → System-Browser mit `desktop_handoff*`-Parametern → Polling auf `/auth/desktop-handoff`
//! → `/auth/desktop-import#<fragment>` im App-Fenster. Das Passwort sieht die App nie.

use crate::config;
use base64::Engine;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use url::Url;

pub const HANDOFF_ID_PARAM: &str = "desktop_handoff";
pub const HANDOFF_SECRET_PARAM: &str = "desktop_handoff_secret";
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HANDOFF_POLL: Duration = Duration::from_millis(900);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DUPLICATE_IMPORT: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, PartialEq)]
pub struct Handoff {
    pub url: String,
    pub poll_url: String,
}

fn url_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// Wie `URLSearchParams.set`: vorhandene Werte des Schlüssels ersetzen.
fn set_param(url: &mut Url, key: &str, value: &str) {
    let kept: Vec<(String, String)> = url.query_pairs().filter(|(k, _)| k != key).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let mut q = url.query_pairs_mut();
    q.clear();
    for (k, v) in &kept {
        q.append_pair(k, v);
    }
    q.append_pair(key, value);
}

fn poll_url(origin: &Url, id: &str, secret: &str) -> Option<String> {
    let mut p = origin.join("/auth/desktop-handoff").ok()?;
    p.query_pairs_mut().append_pair(HANDOFF_ID_PARAM, id).append_pair(HANDOFF_SECRET_PARAM, secret);
    Some(p.to_string())
}

pub fn with_handoff_using(href: &str, id: &str, secret: &str) -> Option<Handoff> {
    let mut url = Url::parse(href).ok()?;
    let is_http = url.scheme() == "https" || url.scheme() == "http";
    if is_http && url.path() == "/auth/google" && url.query_pairs().any(|(k, v)| k == "desktop" && v == "1") {
        set_param(&mut url, HANDOFF_ID_PARAM, id);
        set_param(&mut url, HANDOFF_SECRET_PARAM, secret);
        let poll = poll_url(&Url::parse(&url.origin().ascii_serialization()).ok()?, id, secret)?;
        return Some(Handoff { url: url.to_string(), poll_url: poll });
    }
    if url.host_str().is_some_and(|h| h.ends_with(".supabase.co")) && url.path().starts_with("/auth/") {
        let redirect = url.query_pairs().find(|(k, _)| k == "redirect_to").map(|(_, v)| v.into_owned())?;
        let mut redirect_url = Url::parse(&redirect).ok()?;
        if redirect_url.path() != "/auth/callback" || !redirect_url.query_pairs().any(|(k, v)| k == "desktop" && v == "1") {
            return None;
        }
        set_param(&mut redirect_url, HANDOFF_ID_PARAM, id);
        set_param(&mut redirect_url, HANDOFF_SECRET_PARAM, secret);
        set_param(&mut url, "redirect_to", redirect_url.as_str());
        let poll = poll_url(&Url::parse(&redirect_url.origin().ascii_serialization()).ok()?, id, secret)?;
        return Some(Handoff { url: url.to_string(), poll_url: poll });
    }
    None
}

pub fn with_handoff(href: &str) -> Option<Handoff> {
    with_handoff_using(href, &url_token(18), &url_token(36))
}

/// Schlüssel zur Erkennung doppelter Imports: SHA-256 der normalisierten Query (wie im Original).
pub fn import_key(fragment: &str) -> String {
    let stripped = fragment.strip_prefix(['?', '#']).unwrap_or(fragment);
    let normalized: String = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(url::form_urlencoded::parse(stripped.as_bytes())).finish();
    Sha256::digest(normalized.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn import_url(web_base: &Url, fragment: &str) -> Option<Url> {
    let mut url = web_base.join("/auth/desktop-import").ok()?;
    url.set_fragment(Some(fragment.strip_prefix(['?', '#']).unwrap_or(fragment)));
    Some(url)
}

#[derive(Default)]
pub struct AuthState {
    polling: Mutex<Option<Arc<AtomicBool>>>,
    last_import: Mutex<Option<(String, Instant)>>,
}

impl AuthState {
    /// Startet einen neuen Poll und bricht den vorherigen ab.
    pub fn begin_poll(&self) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.polling.lock().unwrap().replace(cancel.clone()) {
            old.store(true, Ordering::SeqCst);
        }
        cancel
    }
    pub fn stop_poll(&self) {
        if let Some(old) = self.polling.lock().unwrap().take() {
            old.store(true, Ordering::SeqCst);
        }
    }
    pub fn recently_imported(&self, key: &str) -> bool {
        self.last_import.lock().unwrap().as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < DUPLICATE_IMPORT)
    }
    pub fn mark_imported(&self, key: String) {
        *self.last_import.lock().unwrap() = Some((key, Instant::now()));
    }
}

/// Fragt den Server ab, bis die Anmeldung im Browser abgeschlossen ist. `None` bei Abbruch/Zeitüberschreitung.
pub async fn poll_handoff(poll_url: String, cancel: Arc<AtomicBool>) -> Option<String> {
    let client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).user_agent(config::Config::user_agent()).build().ok()?;
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    while !cancel.load(Ordering::SeqCst) && Instant::now() < deadline {
        if let Ok(resp) = client.get(&poll_url).header("accept", "application/json").header("cache-control", "no-store").send().await {
            if resp.status().as_u16() == 200 {
                if let Ok(text) = resp.text().await {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if v.get("status").and_then(|s| s.as_str()) == Some("ready") {
                            if let Some(f) = v.get("fragment").and_then(|f| f.as_str()) {
                                let stripped = f.strip_prefix(['?', '#']).unwrap_or(f);
                                let keys = url::form_urlencoded::parse(stripped.as_bytes()).map(|(k, _)| k.into_owned()).collect::<Vec<_>>();
                                eprintln!("[auth] Handoff bereit: fragment_len={} keys={keys:?}", f.len());
                                if !stripped.is_empty() {
                                    return Some(f.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        tokio::time::sleep(HANDOFF_POLL).await;
    }
    None
}

/// Zustand der Kontoverbindung. Das Token selbst wird nie an die Oberfläche gegeben.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub signed_in: bool,
    pub pending: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
struct LoginStatus {
    pending: bool,
    error: Option<String>,
}

/// Das Geräte-Token und der Login-Handoff. Das Token liegt mit Modus 0600 auf der Platte.
pub struct Account {
    token_file: PathBuf,
    token: Mutex<Option<String>>,
    login: Mutex<LoginStatus>,
    pub auth: AuthState,
}

impl Account {
    pub fn new(token_file: PathBuf) -> Account {
        let token = std::fs::read_to_string(&token_file).ok().and_then(|v| valid_token(&v));
        Account { token_file, token: Mutex::new(token), login: Mutex::new(LoginStatus::default()), auth: AuthState::default() }
    }

    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }

    pub fn signed_in(&self) -> bool {
        self.token.lock().unwrap().is_some()
    }

    pub fn status(&self) -> Status {
        let login = self.login.lock().unwrap();
        Status { signed_in: self.token.lock().unwrap().is_some(), pending: login.pending, error: login.error.clone() }
    }

    fn save_token(&self, token: &str) -> Result<(), String> {
        let token = valid_token(token).ok_or_else(|| "melty.gg sent an invalid device token.".to_string())?;
        if let Some(parent) = self.token_file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Could not create the token folder: {e}"))?;
        }
        let temporary = self.token_file.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, token.as_bytes()).map_err(|e| format!("Could not write the token: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Could not protect the token: {e}"))?;
        }
        std::fs::rename(&temporary, &self.token_file).map_err(|e| format!("Could not store the token: {e}"))?;
        *self.token.lock().unwrap() = Some(token);
        Ok(())
    }

    pub fn logout(&self) {
        self.auth.stop_poll();
        let _ = std::fs::remove_file(&self.token_file);
        *self.token.lock().unwrap() = None;
        let mut login = self.login.lock().unwrap();
        login.pending = false;
        login.error = None;
    }

    fn set_pending(&self, pending: bool, error: Option<String>) {
        let mut login = self.login.lock().unwrap();
        login.pending = pending;
        login.error = error;
    }
}

fn valid_token(value: &str) -> Option<String> {
    let value = value.trim();
    let (prefix, rest) = value.split_once('_')?;
    if !(2..=8).contains(&prefix.len()) || !prefix.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    if rest.len() < 16 || !rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return None;
    }
    Some(value.to_string())
}

/// Öffnet den System-Browser, wartet auf den Handoff und importiert die Sitzung in ein unsichtbares
/// Webview. Dort wird das Geräte-Token mit den Sitzungs-Cookies gemünzt; das Passwort sieht die App nie.
pub async fn login(app: AppHandle, account: Arc<Account>, web_base: Url) -> Result<(), String> {
    account.set_pending(true, None);
    let login_url = match web_base.join("/auth/google") {
        Ok(url) => url,
        Err(error) => {
            let message = error.to_string();
            account.set_pending(false, Some(message.clone()));
            return Err(message);
        }
    };
    let mut login_url = login_url;
    login_url.query_pairs_mut().append_pair("desktop", "1").append_pair("next", "/discover");
    let Some(handoff) = with_handoff(login_url.as_str()) else {
        let message = "Could not prepare the Melty login.".to_string();
        account.set_pending(false, Some(message.clone()));
        return Err(message);
    };
    if let Err(error) = app.opener().open_url(handoff.url, None::<&str>) {
        let message = format!("Could not open the browser: {error}");
        account.set_pending(false, Some(message.clone()));
        return Err(message);
    }
    let cancel = account.auth.begin_poll();
    let fragment = match poll_handoff(handoff.poll_url, cancel).await {
        Some(fragment) => fragment,
        None => {
            account.set_pending(false, Some("Die Anmeldung wurde nicht abgeschlossen.".into()));
            return Err("Die Anmeldung wurde nicht abgeschlossen.".into());
        }
    };
    let result = import_session(&app, &account, &web_base, &fragment).await;
    account.auth.mark_imported(import_key(&fragment));
    match &result {
        Ok(()) => account.set_pending(false, None),
        Err(error) => account.set_pending(false, Some(error.clone())),
    }
    result
}

async fn import_session(app: &AppHandle, account: &Account, web_base: &Url, fragment: &str) -> Result<(), String> {
    let url = import_url(web_base, fragment).ok_or_else(|| "melty.gg sent an invalid login redirect.".to_string())?;
    eprintln!("[auth] Import-URL: path={} fragment_len={}", url.path(), url.fragment().map(str::len).unwrap_or(0));
    if let Some(old) = app.get_webview_window("auth-import") {
        let _ = old.close();
    }
    // Die Import-Seite setzt die Sitzungs-Cookies. Ein unsichtbares WebKit-Fenster lädt die Seite
    // unter Linux nicht zuverlässig, deshalb kurz sichtbar machen und nach dem Laden verstecken.
    let window = WebviewWindowBuilder::new(app, "auth-import", WebviewUrl::External(url))
        .title("Melty-Anmeldung")
        .user_agent(&config::Config::user_agent())
        .visible(true)
        .skip_taskbar(true)
        .focused(false)
        .inner_size(460.0, 340.0)
        .build()
        .map_err(|e| format!("Could not open the login window: {e}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(config::Config::user_agent())
        .build()
        .map_err(|e| e.to_string())?;
    let mint = web_base.join("/api/desktop/device-token").map_err(|e| e.to_string())?;
    let mut last_status = 0u16;
    let mut first_cookies = true;
    for attempt in 0..80 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cookies = window.cookies_for_url(web_base.clone()).unwrap_or_default();
        if cookies.is_empty() {
            if attempt % 10 == 0 {
                eprintln!("[auth] Import-Seite hat noch keine Cookies (Versuch {attempt}).");
            }
            continue;
        }
        if first_cookies {
            // Nur Namen protokollieren, niemals Werte. So lässt sich erkennen, ob die Import-Seite
            // bereits die Sitzungs-Cookies oder nur Analyse-Cookies gesetzt hat.
            let names = cookies.iter().map(|c| c.name().to_string()).collect::<Vec<_>>().join(",");
            eprintln!("[auth] Import-Cookies: {names}");
            // Der Seite Zeit geben, die restlichen Cookies zu setzen, bevor wir das Token anfordern.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            first_cookies = false;
        }
        let cookie_header = cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ");
        // Wie im Original: kein Origin/Referer manuell setzen; die Anfrage kommt aus derselben Sitzung.
        let response = client
            .post(mint.clone())
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .header("cookie", cookie_header)
            .json(&serde_json::json!({ "deviceName": "Melty desktop" }))
            .send()
            .await;
        match response {
            Ok(response) => {
                last_status = response.status().as_u16();
                if response.status().is_success() {
                    let body: serde_json::Value = response.json().await.map_err(|e| format!("melty.gg sent an invalid login response: {e}"))?;
                    let token = body.get("token").and_then(|v| v.as_str()).ok_or_else(|| "melty.gg did not send a device token.".to_string())?;
                    account.save_token(token)?;
                    let _ = window.close();
                    return Ok(());
                }
                if last_status != 401 && last_status != 403 {
                    let text = response.text().await.unwrap_or_default();
                    let snippet: String = text.chars().take(300).collect();
                    eprintln!("[auth] Token-Anfrage abgelehnt: HTTP {last_status} {snippet}");
                    let _ = window.close();
                    return Err(format!("melty.gg rejected the login (HTTP {last_status})."));
                }
            }
            Err(e) => {
                let _ = window.close();
                return Err(format!("Could not reach melty.gg: {e}"));
            }
        }
    }
    let _ = window.close();
    Err(if last_status == 401 || last_status == 403 {
        "Die Anmeldung konnte nicht übernommen werden. Bitte erneut anmelden.".into()
    } else {
        "Die Anmeldung hat zu lange gedauert.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_handoff() {
        let h = with_handoff_using("https://melty.gg/auth/google?desktop=1&next=%2Fdiscover", "ID", "SECRET").unwrap();
        assert_eq!(h.url, "https://melty.gg/auth/google?desktop=1&next=%2Fdiscover&desktop_handoff=ID&desktop_handoff_secret=SECRET");
        assert_eq!(h.poll_url, "https://melty.gg/auth/desktop-handoff?desktop_handoff=ID&desktop_handoff_secret=SECRET");
        // vorhandene Parameter werden ersetzt, nicht verdoppelt
        let h = with_handoff_using("https://melty.gg/auth/google?desktop=1&desktop_handoff=OLD", "ID", "S").unwrap();
        assert_eq!(h.url.matches("desktop_handoff=").count(), 1);
        assert!(h.url.contains("desktop_handoff=ID"));
        assert!(with_handoff_using("https://melty.gg/auth/google", "ID", "S").is_none(), "ohne desktop=1 kein Handoff");
        assert!(with_handoff_using("ftp://melty.gg/auth/google?desktop=1", "ID", "S").is_none());
    }

    #[test]
    fn supabase_handoff() {
        let href = "https://abc.supabase.co/auth/v1/authorize?provider=google&redirect_to=https%3A%2F%2Fmelty.gg%2Fauth%2Fcallback%3Fdesktop%3D1";
        let h = with_handoff_using(href, "ID", "S").unwrap();
        assert!(h.url.starts_with("https://abc.supabase.co/auth/v1/authorize?provider=google&redirect_to="));
        let redirect = Url::parse(&h.url).unwrap().query_pairs().find(|(k, _)| k == "redirect_to").unwrap().1.into_owned();
        assert_eq!(redirect, "https://melty.gg/auth/callback?desktop=1&desktop_handoff=ID&desktop_handoff_secret=S");
        assert_eq!(h.poll_url, "https://melty.gg/auth/desktop-handoff?desktop_handoff=ID&desktop_handoff_secret=S");
        // fremde oder falsche Weiterleitungen werden nicht angefasst
        for bad in [
            "https://abc.supabase.co/auth/v1/authorize?redirect_to=https%3A%2F%2Fmelty.gg%2Fother%3Fdesktop%3D1",
            "https://abc.supabase.co/auth/v1/authorize?redirect_to=https%3A%2F%2Fmelty.gg%2Fauth%2Fcallback",
            "https://abc.supabase.co/auth/v1/authorize",
            "https://abc.supabase.co/rest/v1/x?redirect_to=https%3A%2F%2Fmelty.gg%2Fauth%2Fcallback%3Fdesktop%3D1",
        ] {
            assert!(with_handoff_using(bad, "ID", "S").is_none(), "{bad}");
        }
    }

    #[test]
    fn tokens_have_expected_shape() {
        let (a, b) = (url_token(18), url_token(36));
        assert_eq!((a.len(), b.len()), (24, 48));
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(a, url_token(18));
    }

    #[test]
    fn import_key_and_url() {
        assert_eq!(import_key("#a=1&b=2"), import_key("a=1&b=2"));
        assert_eq!(import_key("a=b%20c"), import_key("a=b+c"), "Normalisierung wie URLSearchParams");
        assert_ne!(import_key("a=1"), import_key("a=2"));
        let u = import_url(&Url::parse("https://melty.gg").unwrap(), "#access_token=x&type=y").unwrap();
        assert_eq!(u.as_str(), "https://melty.gg/auth/desktop-import#access_token=x&type=y");
    }

    #[test]
    fn duplicate_import_window_and_cancel() {
        let s = AuthState::default();
        assert!(!s.recently_imported("k"));
        s.mark_imported("k".into());
        assert!(s.recently_imported("k") && !s.recently_imported("other"));
        let first = s.begin_poll();
        let second = s.begin_poll();
        assert!(first.load(Ordering::SeqCst) && !second.load(Ordering::SeqCst), "neuer Login bricht alten Poll ab");
        s.stop_poll();
        assert!(second.load(Ordering::SeqCst));
    }
}
