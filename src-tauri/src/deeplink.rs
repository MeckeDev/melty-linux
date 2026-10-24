//! `melty://`-Links. Regeln 1:1 aus `mods/deep-link.js` (`modDeepLinkTarget`) und `auth.js` (`handleAuthLink`).

use regex::Regex;
use std::sync::OnceLock;
use url::Url;

struct Patterns {
    test: Regex,
    join: Regex,
    profile: Regex,
    link: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        test: Regex::new(r"^([a-z][a-z0-9+.-]*)://m/([a-z0-9][a-z0-9-]{0,79})/test/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/?(?:[?#].*)?$").unwrap(),
        join: Regex::new(r"^([a-z][a-z0-9+.-]*)://j/([a-z]{3,8}-[a-z]{3,8}-[2-9a-hj-km-np-z]{4})/?(?:[?#].*)?$").unwrap(),
        profile: Regex::new(r"^([a-z][a-z0-9+.-]*)://u/([A-Za-z0-9_-]{1,64})(/add)?/?(?:[?#].*)?$").unwrap(),
        link: Regex::new(r"^([a-z][a-z0-9+.-]*)://m/([a-z0-9][a-z0-9-]{0,79})(/install)?/?(?:[?#].*)?$").unwrap(),
    })
}

/// Seite auf melty.gg, zu der ein `melty://m|j|u/…`-Link führt.
pub fn target_for(url: &str, web_base: &str, protocol: &str) -> Option<String> {
    let p = patterns();
    let url = url.trim();
    let origin = Url::parse(web_base).ok()?.origin().ascii_serialization();
    let base = Url::parse(&origin).ok()?;
    if let Some(c) = p.test.captures(url) {
        if &c[1] != protocol {
            return None;
        }
        let mut t = base.join(&format!("/m/{}", &c[2])).ok()?;
        t.query_pairs_mut().append_pair("test", &c[3]);
        return Some(t.to_string());
    }
    if let Some(j) = p.join.captures(url) {
        if &j[1] != protocol {
            return None;
        }
        return Some(base.join(&format!("/j/{}", &j[2])).ok()?.to_string());
    }
    if let Some(pr) = p.profile.captures(url) {
        if &pr[1] != protocol {
            return None;
        }
        let mut t = base.join(&format!("/u/{}", &pr[2])).ok()?;
        if pr.get(3).is_some() {
            t.query_pairs_mut().append_pair("add", "1");
        }
        return Some(t.to_string());
    }
    let m = p.link.captures(url)?;
    if &m[1] != protocol {
        return None;
    }
    let mut t = base.join(&format!("/m/{}", &m[2])).ok()?;
    if m.get(3).is_some() {
        t.query_pairs_mut().append_pair("install", "1");
    }
    Some(t.to_string())
}

/// `melty://auth/success?<fragment>` → das Fragment für `/auth/desktop-import#…`.
pub fn auth_fragment(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != crate::config::PROTOCOL || parsed.host_str() != Some("auth") || parsed.path() != "/success" {
        return None;
    }
    let query: String = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(parsed.query_pairs()).finish();
    let fragment = if !query.is_empty() { query } else { parsed.fragment().unwrap_or("").to_string() };
    (!fragment.is_empty()).then_some(fragment)
}

#[derive(Debug, PartialEq)]
pub enum Link {
    /// Seite im App-Fenster öffnen.
    Page(String),
    /// Anmeldung abschließen (Fragment für `/auth/desktop-import`).
    AuthImport(String),
    /// `melty://desktop/retry`
    Retry,
    /// `melty://desktop/panel[/games|mashups|installed|settings|diagnostics]` – natives Panel des Linux-Clients.
    Panel(String),
    /// `melty://m/<slug>` – Mashup-Detail im nativen Panel öffnen.
    Mashup(String),
}

/// `melty://desktop/panel[/<tab>]` → Tab-Name (unbekannte Tabs fallen auf "games").
pub fn panel_tab(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    if parsed.scheme() != crate::config::PROTOCOL || parsed.host_str() != Some("desktop") {
        return None;
    }
    let rest = parsed.path().strip_prefix("/panel")?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let tab = rest.trim_matches('/');
    Some(if ["games", "mashups", "installed", "settings", "diagnostics"].contains(&tab) { tab.to_string() } else { "games".to_string() })
}

/// `melty://m/<slug>` (ohne `/install` oder `/test/…`) → Slug für das native Mashup-Detail.
pub fn mashup_slug(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    if parsed.scheme() != crate::config::PROTOCOL || parsed.host_str() != Some("m") {
        return None;
    }
    let slug = parsed.path().trim_matches('/');
    if slug.is_empty() || slug.len() > 80 {
        return None;
    }
    let mut chars = slug.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return None;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return None;
    }
    Some(slug.to_string())
}

pub fn classify(url: &str, web_base: &str) -> Option<Link> {
    if url.trim() == "melty://desktop/retry" {
        return Some(Link::Retry);
    }
    if let Some(tab) = panel_tab(url) {
        return Some(Link::Panel(tab));
    }
    if let Some(slug) = mashup_slug(url) {
        return Some(Link::Mashup(slug));
    }
    if let Some(f) = auth_fragment(url) {
        return Some(Link::AuthImport(f));
    }
    target_for(url, web_base, crate::config::PROTOCOL).map(Link::Page)
}

#[cfg(test)]
mod tests {
    use super::*;
    const WEB: &str = "https://melty.gg";

    #[test]
    fn mod_links() {
        assert_eq!(target_for("melty://m/my-mod", WEB, "melty").as_deref(), Some("https://melty.gg/m/my-mod"));
        assert_eq!(target_for("melty://m/my-mod/install", WEB, "melty").as_deref(), Some("https://melty.gg/m/my-mod?install=1"));
        assert_eq!(target_for("melty://m/my-mod/test/0123abcd-0123-4abc-8abc-0123456789ab?x=1", WEB, "melty").as_deref(), Some("https://melty.gg/m/my-mod?test=0123abcd-0123-4abc-8abc-0123456789ab"));
        assert_eq!(target_for("melty://j/abc-defg-2345", WEB, "melty").as_deref(), Some("https://melty.gg/j/abc-defg-2345"));
        assert_eq!(target_for("melty://u/Some_User/add", WEB, "melty").as_deref(), Some("https://melty.gg/u/Some_User?add=1"));
        assert_eq!(target_for("melty://u/Some_User", WEB, "melty").as_deref(), Some("https://melty.gg/u/Some_User"));
    }

    #[test]
    fn rejects_foreign_and_malformed() {
        assert_eq!(target_for("other://m/x", WEB, "melty"), None);
        assert_eq!(target_for("melty://m/../etc", WEB, "melty"), None);
        assert_eq!(target_for("melty://m/UPPER", WEB, "melty"), None);
        assert_eq!(target_for("melty://j/abc-defg-0000", WEB, "melty"), None, "0/1/i/l/o sind im Join-Code nicht erlaubt");
        assert_eq!(target_for("melty://x/y", WEB, "melty"), None);
        assert_eq!(target_for("https://melty.gg/m/x", WEB, "melty"), None);
    }

    #[test]
    fn auth_links() {
        assert_eq!(auth_fragment("melty://auth/success?access_token=a%20b&x=1").as_deref(), Some("access_token=a+b&x=1"));
        assert_eq!(auth_fragment("melty://auth/success#frag=1").as_deref(), Some("frag=1"));
        assert_eq!(auth_fragment("melty://auth/success"), None);
        assert_eq!(auth_fragment("melty://auth/other?a=1"), None);
        assert_eq!(auth_fragment("https://auth/success?a=1"), None);
        assert_eq!(classify("melty://desktop/retry", WEB), Some(Link::Retry));
        assert!(matches!(classify("melty://auth/success?a=1", WEB), Some(Link::AuthImport(_))));
        assert!(matches!(classify("melty://m/x/install", WEB), Some(Link::Page(_))));
    }

    #[test]
    fn panel_links() {
        assert_eq!(panel_tab("melty://desktop/panel").as_deref(), Some("games"));
        assert_eq!(panel_tab("melty://desktop/panel/settings").as_deref(), Some("settings"));
        assert_eq!(panel_tab("melty://desktop/panel/diagnostics/").as_deref(), Some("diagnostics"));
        assert_eq!(panel_tab("melty://desktop/panel/mashups").as_deref(), Some("mashups"));
        assert_eq!(panel_tab("melty://desktop/panel/installed").as_deref(), Some("installed"));
        assert_eq!(panel_tab("melty://desktop/panel/evil").as_deref(), Some("games"));
        assert_eq!(panel_tab("melty://desktop/retry"), None);
        assert_eq!(panel_tab("melty://desktop/panelx"), None);
        assert_eq!(panel_tab("https://desktop/panel"), None);
        assert_eq!(classify("melty://desktop/panel/settings", WEB), Some(Link::Panel("settings".into())));
    }

    #[test]
    fn mashup_links() {
        assert_eq!(mashup_slug("melty://m/minecraft-ring").as_deref(), Some("minecraft-ring"));
        assert_eq!(mashup_slug("melty://m/minecraft-ring/").as_deref(), Some("minecraft-ring"));
        assert_eq!(mashup_slug("melty://m/my-mod/install"), None);
        assert_eq!(mashup_slug("melty://m/my-mod/test/0123abcd-0123-4abc-8abc-0123456789ab"), None);
        assert_eq!(mashup_slug("melty://m/UPPER"), None);
        assert_eq!(mashup_slug("https://melty.gg/m/x"), None);
        assert_eq!(classify("melty://m/minecraft-ring", WEB), Some(Link::Mashup("minecraft-ring".into())));
        assert!(matches!(classify("melty://m/my-mod/install", WEB), Some(Link::Page(_))));
    }
}
