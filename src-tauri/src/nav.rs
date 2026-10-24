//! Navigationsregeln des Hauptfensters, 1:1 aus dem Original (`opensInBrowser`, `isOffAppPage`, `isRetryLink`).

use crate::config::same_origin;
use url::Url;

const APP_PAGES: &[&str] = &["/discover", "/home", "/search", "/g", "/m", "/u", "/j", "/library", "/publish", "/studio", "/create", "/chat"];
const AUTH_PAGES: &[&str] = &["/auth/login", "/auth/logout", "/auth/callback", "/auth/google", "/auth/desktop-export", "/auth/desktop-handoff", "/auth/desktop-import", "/success"];

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Im App-Fenster laden.
    Allow,
    /// Abbrechen und im Standardbrowser/-programm öffnen.
    External(String),
    /// Abbrechen und stattdessen die Startseite laden (Wiederholen-Link, Seiten außerhalb der App).
    Home,
    /// Abbrechen und als `melty://`-Link verarbeiten.
    DeepLink(String),
}

fn path_in(pathname: &str, prefixes: &[&str]) -> bool {
    let trimmed = pathname.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    prefixes.iter().any(|p| path == *p || path.starts_with(&format!("{p}/")))
}

pub fn opens_in_browser(url: &Url, web_base: &Url) -> bool {
    if ["discord", "mailto", "steam", "rockstar"].contains(&url.scheme()) {
        return true;
    }
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    if host == "accounts.google.com" || host == "accounts.youtube.com" {
        return true;
    }
    if host.ends_with(".supabase.co") && url.path().starts_with("/auth/") {
        return true;
    }
    !same_origin(url.as_str(), web_base.as_str())
}

pub fn is_off_app_page(url: &Url, web_base: &Url) -> bool {
    (url.scheme() == "http" || url.scheme() == "https")
        && same_origin(url.as_str(), web_base.as_str())
        && !path_in(url.path(), APP_PAGES)
        && !path_in(url.path(), AUTH_PAGES)
}

pub fn is_internal(url: &Url) -> bool {
    matches!(url.scheme(), "tauri" | "about" | "blob" | "data") || url.host_str() == Some("tauri.localhost")
}

pub fn decide(url: &Url, web_base: &Url) -> Decision {
    if url.as_str() == "melty://desktop/retry" {
        return Decision::Home;
    }
    if url.scheme() == crate::config::PROTOCOL {
        return Decision::DeepLink(url.to_string());
    }
    if is_internal(url) {
        return Decision::Allow;
    }
    if opens_in_browser(url, web_base) {
        return Decision::External(url.to_string());
    }
    if is_off_app_page(url, web_base) {
        return Decision::Home;
    }
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(u: &str) -> Decision {
        decide(&Url::parse(u).unwrap(), &Url::parse("https://melty.gg").unwrap())
    }

    #[test]
    fn app_pages_stay() {
        for u in ["https://melty.gg/discover?desktopVersion=1.0.132", "https://melty.gg/m/some-mod", "https://melty.gg/library/", "https://melty.gg/auth/desktop-import#a=1", "https://melty.gg/success", "https://melty.gg/g/hades"] {
            assert_eq!(d(u), Decision::Allow, "{u}");
        }
    }

    #[test]
    fn marketing_pages_go_home() {
        for u in ["https://melty.gg/", "https://melty.gg/about", "https://melty.gg/discoverx", "https://melty.gg/auth/other"] {
            assert_eq!(d(u), Decision::Home, "{u}");
        }
    }

    #[test]
    fn external_goes_to_browser() {
        for u in ["https://accounts.google.com/o/oauth2/v2/auth", "https://x.supabase.co/auth/v1/authorize", "https://example.com/", "https://melty.gg.evil.com/", "http://melty.gg/discover", "discord://invite/x", "mailto:a@b.c", "steam://rungameid/620"] {
            assert!(matches!(d(u), Decision::External(_)), "{u}");
        }
        assert_eq!(d("https://x.supabase.co/rest/v1/x"), Decision::External("https://x.supabase.co/rest/v1/x".into()), "fremde Origin ist ohnehin extern");
    }

    #[test]
    fn internal_and_links() {
        assert_eq!(d("tauri://localhost/offline.html"), Decision::Allow);
        assert_eq!(d("http://tauri.localhost/offline.html"), Decision::Allow);
        assert_eq!(d("about:blank"), Decision::Allow);
        assert_eq!(d("melty://desktop/retry"), Decision::Home);
        assert!(matches!(d("melty://m/x"), Decision::DeepLink(_)));
        assert_eq!(d("javascript:alert(1)"), Decision::Allow, "wird nicht als Navigation gewertet; die Seite selbst ist vertrauenswürdig");
    }
}
