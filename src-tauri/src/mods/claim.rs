//! Kostenlose Mashups direkt im Client zur melty.gg-Bibliothek hinzufügen.
//!
//! Der Desktop-Token gilt nur für `/api/desktop/…`; `/api/mods/<slug>/get` braucht die
//! Websitzung. Die liegt bereits im WebKit-Speicher des Panels (aus dem Login). Wir lesen die
//! Supabase-Cookies nur im Speicher, setzen sie nie ins Log und schicken sie ausschließlich an
//! melty.gg. Ist die Sitzung abgelaufen, lädt ein kurzes In-App-Fenster die Mashup-Seite; die
//! Seite erneuert die Sitzung selbst und trägt das Mashup ein.

use crate::config::{self, Config};
use serde_json::Value;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use url::Url;

const CLAIM_TIMEOUT: Duration = Duration::from_secs(20);
const CLAIM_POLL: Duration = Duration::from_millis(600);

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Keine gültige Websitzung; ein Login ist nötig.
    NoSession,
    /// Der Server hat das Hinzufügen abgelehnt.
    Failed(String),
}

/// `pricing`/`priceCents` kommen aus `/api/mods/<slug>`.
pub fn is_free(detail: &Value) -> bool {
    let pricing = detail.get("pricing").and_then(Value::as_str).unwrap_or_default();
    if pricing == "free" {
        return true;
    }
    if pricing == "paid" {
        return false;
    }
    let cents = detail.get("priceCents").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
    });
    cents.is_none_or(|cents| cents <= 0)
}

/// Fügt ein kostenloses Mashup hinzu. Zuerst unsichtbar über die vorhandene Websitzung, bei
/// abgelaufener Sitzung über ein kurzes In-App-Fenster.
pub async fn claim_free(app: &AppHandle, base: &Url, slug: &str, mod_id: &str, token: &str) -> Result<(), ClaimError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(Config::user_agent())
        .build()
        .map_err(|error| ClaimError::Failed(error.to_string()))?;

    match claim_with_cookie(app, base, slug, None, &client).await {
        Ok(()) => return Ok(()),
        Err(ClaimError::NoSession) => {}
        Err(error) => return Err(error),
    }

    claim_in_window(app, base, slug, mod_id, token, &client).await
}

async fn claim_in_window(
    app: &AppHandle,
    base: &Url,
    slug: &str,
    mod_id: &str,
    token: &str,
    client: &reqwest::Client,
) -> Result<(), ClaimError> {
    let url = base
        .join(&format!("/m/{slug}"))
        .map_err(|error| ClaimError::Failed(error.to_string()))?;
    for (label, window) in app.webview_windows() {
        if label.starts_with("mashup-claim") {
            let _ = window.close();
        }
    }
    let label = format!("mashup-claim-{}-{}", std::process::id(), super::journal::now());
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title("Mashup wird hinzugefügt")
        .user_agent(&Config::user_agent())
        .inner_size(480.0, 360.0)
        .skip_taskbar(true)
        .focused(false)
        .visible(true)
        .build()
        .map_err(|error| ClaimError::Failed(format!("Konnte das Bestätigungsfenster nicht öffnen: {error}")))?;

    let deadline = Instant::now() + CLAIM_TIMEOUT;
    let mut result = Err(ClaimError::NoSession);
    let mut last_error = None;
    while Instant::now() < deadline {
        tokio::time::sleep(CLAIM_POLL).await;
        // Die Seite leitet bei fehlender Sitzung auf /auth/… um. Dann ist ein Login nötig.
        if window.url().ok().is_some_and(|url| url.path().starts_with("/auth/")) {
            result = Err(ClaimError::NoSession);
            break;
        }
        match claim_with_cookie(app, base, slug, Some(&window), client).await {
            Ok(()) => {
                result = Ok(());
                break;
            }
            Err(ClaimError::NoSession) => {}
            Err(error) => last_error = Some(error),
        }
        if library_contains(client, base, token, mod_id).await {
            result = Ok(());
            break;
        }
    }
    let _ = window.close();
    match (result, last_error) {
        (Err(_), Some(error)) => Err(error),
        (result, _) => result,
    }
}

async fn claim_with_cookie(
    app: &AppHandle,
    base: &Url,
    slug: &str,
    window: Option<&WebviewWindow>,
    client: &reqwest::Client,
) -> Result<(), ClaimError> {
    let cookie = window
        .and_then(|window| auth_cookie_from(window, base))
        .or_else(|| session_cookie(app, base))
        .ok_or(ClaimError::NoSession)?;
    let url = base
        .join(&format!("/api/mods/{slug}/get"))
        .map_err(|error| ClaimError::Failed(error.to_string()))?;
    let response = client
        .post(url)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|error| ClaimError::Failed(format!("melty.gg ist nicht erreichbar: {error}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        return Ok(());
    }
    let body: Value = serde_json::from_str(&text).unwrap_or_default();
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    let message = body
        .get("error")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("melty.gg hat das Mashup nicht hinzugefügt.");
    if status.as_u16() == 401 || code == "signed-out" {
        return Err(ClaimError::NoSession);
    }
    Err(ClaimError::Failed(format!("{message} (HTTP {})", status.as_u16())))
}

/// Sucht die Supabase-Sitzung im Cookie-Speicher des Panels bzw. des Login-Fensters.
fn session_cookie(app: &AppHandle, base: &Url) -> Option<String> {
    for label in ["panel", "auth-import"] {
        if let Some(window) = app.get_webview_window(label) {
            if let Some(cookie) = auth_cookie_from(&window, base) {
                return Some(cookie);
            }
        }
    }
    None
}

fn auth_cookie_from(window: &WebviewWindow, base: &Url) -> Option<String> {
    let cookies = window.cookies_for_url(base.clone()).ok()?;
    join_auth_cookies(cookies.into_iter().map(|cookie| (cookie.name().to_string(), cookie.value().to_string())))
}

fn join_auth_cookies(cookies: impl Iterator<Item = (String, String)>) -> Option<String> {
    let mut direct: Vec<(String, String)> = Vec::new();
    let mut chunks: std::collections::BTreeMap<String, std::collections::BTreeMap<usize, String>> = std::collections::BTreeMap::new();
    for (name, value) in cookies {
        if !name.starts_with("sb-") || !name.contains("-auth-token") {
            continue;
        }
        if let Some((family, index)) = name.rsplit_once('.') {
            if family.ends_with("-auth-token") {
                if let Ok(index) = index.parse::<usize>() {
                    chunks.entry(family.to_string()).or_default().insert(index, value);
                    continue;
                }
            }
        }
        if name.ends_with("-auth-token") {
            direct.push((name, value));
        }
    }
    if let Some((name, value)) = direct.into_iter().next() {
        return Some(format!("{name}={value}"));
    }
    chunks.into_iter().find_map(|(name, parts)| {
        let value: String = parts.into_values().collect();
        (!value.is_empty()).then(|| format!("{name}={value}"))
    })
}

async fn library_contains(client: &reqwest::Client, base: &Url, token: &str, mod_id: &str) -> bool {
    let Ok(url) = base.join("/api/desktop/mods/library") else { return false };
    let Ok(response) = client
        .get(url)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("cache-control", "no-store")
        .header("x-melty-app-version", config::COMPAT_VERSION)
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let Ok(value) = response.json::<Value>().await else { return false };
    value
        .get("mods")
        .and_then(Value::as_array)
        .is_some_and(|mods| mods.iter().any(|item| item.get("modId").and_then(Value::as_str) == Some(mod_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn free_detection_matches_the_catalogue() {
        assert!(is_free(&json!({ "pricing": "free", "priceCents": 0 })));
        assert!(is_free(&json!({ "priceCents": 0 })));
        assert!(!is_free(&json!({ "pricing": "paid", "priceCents": 0 })));
        assert!(!is_free(&json!({ "pricing": "paid", "priceCents": 499 })));
        assert!(!is_free(&json!({ "priceCents": 199 })));
        assert!(!is_free(&json!({ "priceCents": "199" })));
        assert!(is_free(&json!({ "priceCents": "0" })));
    }

    #[test]
    fn chunked_session_cookies_are_joined_in_order() {
        let cookie = join_auth_cookies(
            vec![
                ("sb-abc-auth-token.1".to_string(), "def".to_string()),
                ("sb-abc-auth-token.0".to_string(), "base64-abc".to_string()),
                ("unrelated".to_string(), "x".to_string()),
            ]
            .into_iter(),
        );
        assert_eq!(cookie.as_deref(), Some("sb-abc-auth-token=base64-abcdef"));

        let direct = join_auth_cookies(vec![("sb-abc-auth-token".to_string(), "value".to_string())].into_iter());
        assert_eq!(direct.as_deref(), Some("sb-abc-auth-token=value"));
    }
}
