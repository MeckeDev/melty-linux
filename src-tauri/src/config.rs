//! Laufzeit-Konfiguration. Wie im Original (`config.js`): Produktions-URL melty.gg; Überschreibung per
//! `MELTY_WEB_URL` nur mit `MELTY_DEV_DESKTOP=1`.

use url::Url;

pub const PRODUCTION_URL: &str = "https://melty.gg";
/// Version der Windows-App, deren Bridge dieser Client nachbildet. Wird als `Melty/<Version>` im User-Agent und als
/// `desktopVersion`/`appVersion` gemeldet (die Seite kann daran Fähigkeiten festmachen).
pub const COMPAT_VERSION: &str = "1.0.132";
pub const TITLE: &str = "Melty";
/// Muss `identifier` in tauri.conf.json entsprechen (Ordnername unter ~/.config).
pub const IDENTIFIER: &str = "gg.melty.linux-client";
pub const PROTOCOL: &str = "melty";
pub const LOGIN_START_ARG: &str = "--melty-startup";

#[derive(Debug, Clone)]
pub struct Config {
    pub web_base: Url,
    pub is_dev: bool,
}

impl Config {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Config {
        let is_dev = get("MELTY_DEV_DESKTOP").as_deref() == Some("1");
        let override_url = if is_dev { get("MELTY_WEB_URL").and_then(|v| Url::parse(&v).ok()) } else { None };
        Config { web_base: override_url.unwrap_or_else(|| Url::parse(PRODUCTION_URL).expect("const")), is_dev }
    }
    pub fn load() -> Config {
        Config::from_env(|k| std::env::var(k).ok())
    }

    /// `<web>/discover?desktopVersion=<Version>` wie `homeUrl()` im Original.
    pub fn home_url(&self) -> Url {
        let mut u = self.web_base.join("/discover").expect("join");
        u.query_pairs_mut().append_pair("desktopVersion", COMPAT_VERSION);
        u
    }

    /// User-Agent für melty.gg. Die Seite lässt die Desktop-Import-Seite nur durch, wenn sowohl
    /// `Electron/` als auch `Melty/` darin vorkommen (Middleware); WebKit allein wird auf `/`
    /// umgeleitet und verliert dabei das Login-Fragment.
    pub fn user_agent() -> String {
        format!(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Electron/33.0.0 Melty/{COMPAT_VERSION} MeltyLinux/{}",
            env!("CARGO_PKG_VERSION")
        )
    }
}

#[allow(dead_code)] // nur noch von den geparkten Navigationsregeln/Tests genutzt
pub fn same_origin(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin() && a.origin().is_tuple(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn override_needs_dev_flag() {
        let c = Config::from_env(env(&[("MELTY_WEB_URL", "http://127.0.0.1:9")]));
        assert_eq!(c.web_base.as_str(), "https://melty.gg/");
        let c = Config::from_env(env(&[("MELTY_DEV_DESKTOP", "1"), ("MELTY_WEB_URL", "http://127.0.0.1:9")]));
        assert_eq!(c.web_base.as_str(), "http://127.0.0.1:9/");
        assert!(c.is_dev);
    }

    #[test]
    fn home_url_and_origin() {
        let c = Config::from_env(env(&[]));
        assert_eq!(c.home_url().as_str(), "https://melty.gg/discover?desktopVersion=1.0.132");
        assert!(same_origin("https://melty.gg/a", "https://melty.gg/b?x"));
        assert!(!same_origin("https://melty.gg.evil.com/", "https://melty.gg/"));
        assert!(!same_origin("http://melty.gg/", "https://melty.gg/"));
        assert!(!same_origin("data:text/html,x", "data:text/html,y"), "opake Origins sind nie gleich");
        assert!(Config::user_agent().contains("Melty/1.0.132"));
    }
}
