//! Mashup-Katalog von melty.gg: öffentliche JSON-Endpunkte (`/api/mods`, `/api/mods/<slug>`).
//!
//! Der Katalog-Endpunkt liefert maximal 60 Einträge pro Seite und unterstützt keine Filter nach
//! Spiel/Genre. Deshalb wird der vollständige Katalog einmal geladen, lokal zwischengespeichert
//! (eine Stunde gültig) und im Speicher gefiltert/sortiert. Siehe `SPIKE.md`.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const PAGE: usize = 60;
const MAX_PAGES: usize = 400;
const PARALLEL: usize = 8;
const TTL: Duration = Duration::from_secs(60 * 60);
const CATALOG_FILE: &str = "mashup-catalog.json";

/// Fehlende **und** `null`-Felder als Standardwert lesen; die API lässt einzelne Felder gelegentlich weg.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameRef {
    pub slug: String,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub short_name: Option<String>,
    #[serde(default)]
    pub steam_app_id: Option<String>,
    #[serde(default)]
    pub store_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Creator {
    #[serde(default, deserialize_with = "null_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mashup {
    pub id: String,
    pub slug: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub tagline: String,
    #[serde(default, deserialize_with = "null_default")]
    pub creator: Creator,
    #[serde(default, deserialize_with = "null_default")]
    pub games: Vec<GameRef>,
    #[serde(default, deserialize_with = "null_default")]
    pub pricing: String,
    #[serde(default, deserialize_with = "null_default")]
    pub price_cents: i64,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub preview_media: Vec<Value>,
    #[serde(default, deserialize_with = "null_default")]
    pub made_by: String,
    #[serde(default, deserialize_with = "null_default")]
    pub version: String,
    #[serde(default, deserialize_with = "null_default")]
    pub status: String,
    #[serde(default, deserialize_with = "null_default")]
    pub gets: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub installs: i64,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub stars: f64,
    #[serde(default)]
    pub remix_of_id: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub play: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInfo {
    pub count: usize,
    pub fetched_at: u64,
    pub age_seconds: u64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameFacet {
    pub slug: String,
    pub name: String,
    pub short_name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub items: Vec<Mashup>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub catalog: CatalogInfo,
    pub games: Vec<GameFacet>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    pub sort: String,
    pub game: Option<String>,
    pub installed_only: bool,
    pub installed_games: HashSet<String>,
    pub offset: usize,
    pub limit: usize,
}

struct Cached {
    mods: Vec<Mashup>,
    fetched_at: u64,
}

pub struct MashupCache {
    dir: PathBuf,
    inner: Mutex<Option<Cached>>,
    refreshing: AtomicBool,
    /// Zeitpunkt des letzten fehlgeschlagenen automatischen Abrufs (Offline-Backoff).
    last_failure: Mutex<Option<u64>>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl MashupCache {
    pub fn new(dir: PathBuf) -> Self {
        let cache = MashupCache { dir, inner: Mutex::new(None), refreshing: AtomicBool::new(false), last_failure: Mutex::new(None) };
        cache.load_disk();
        cache
    }

    /// Nach einem Fehlschlag 60 s lang keinen neuen automatischen Vollabruf starten.
    fn should_retry(&self) -> bool {
        self.last_failure.lock().unwrap().is_none_or(|t| now().saturating_sub(t) >= 60)
    }

    fn mark_failure(&self) {
        *self.last_failure.lock().unwrap() = Some(now());
    }

    fn mark_success(&self) {
        *self.last_failure.lock().unwrap() = None;
    }

    fn load_disk(&self) {
        let Ok(text) = std::fs::read_to_string(self.dir.join(CATALOG_FILE)) else { return };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct File {
            fetched_at: u64,
            mods: Vec<Mashup>,
        }
        if let Ok(file) = serde_json::from_str::<File>(&text) {
            if !file.mods.is_empty() {
                *self.inner.lock().unwrap() = Some(Cached { mods: file.mods, fetched_at: file.fetched_at });
            }
        }
    }

    fn save_disk(&self, cached: &Cached) {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct File<'a> {
            fetched_at: u64,
            mods: &'a [Mashup],
        }
        let file = File { fetched_at: cached.fetched_at, mods: &cached.mods };
        if let Ok(text) = serde_json::to_string(&file) {
            let tmp = self.dir.join(format!("{CATALOG_FILE}.tmp"));
            if std::fs::create_dir_all(&self.dir).is_ok() && std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, self.dir.join(CATALOG_FILE));
            }
        }
    }

    fn info(&self, cached: &Cached) -> CatalogInfo {
        let age = now().saturating_sub(cached.fetched_at);
        CatalogInfo { count: cached.mods.len(), fetched_at: cached.fetched_at, age_seconds: age, stale: age > TTL.as_secs() }
    }

    fn snapshot(&self) -> Option<(Vec<Mashup>, CatalogInfo)> {
        let guard = self.inner.lock().unwrap();
        let cached = guard.as_ref()?;
        Some((cached.mods.clone(), self.info(cached)))
    }
}

async fn fetch_page(client: &reqwest::Client, base: &Url, offset: usize) -> Result<Vec<Mashup>, String> {
    let mut url = base.join("/api/mods").map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("limit", &PAGE.to_string())
        .append_pair("offset", &offset.to_string())
        .append_pair("sort", "top");
    let response = client
        .get(url)
        .header("accept", "application/json")
        .header("cache-control", "no-store")
        .send()
        .await
        .map_err(|e| format!("melty.gg nicht erreichbar: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Katalog: HTTP {}", response.status()));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    #[derive(Deserialize)]
    struct Page {
        #[serde(default, deserialize_with = "null_default")]
        mods: Vec<Mashup>,
    }
    let page: Page = serde_json::from_str(&text).map_err(|e| format!("Katalog: ungültige Antwort: {e}"))?;
    Ok(page.mods)
}

/// Lädt den vollständigen Katalog. Bei Fehlern bleibt der alte Stand erhalten.
pub async fn refresh(cache: &MashupCache, base: &Url) -> Result<CatalogInfo, String> {
    if cache.refreshing.swap(true, Ordering::SeqCst) {
        // Ein anderer Lauf lädt bereits; kurz warten und den Stand übernehmen.
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !cache.refreshing.load(Ordering::SeqCst) {
                return cache.snapshot().map(|(_, info)| info).ok_or_else(|| "Katalog wird geladen".to_string());
            }
        }
        return Err("Katalog wird bereits geladen".into());
    }
    let result = load_all(cache, base).await;
    cache.refreshing.store(false, Ordering::SeqCst);
    result
}

async fn load_all(cache: &MashupCache, base: &Url) -> Result<CatalogInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(Config::user_agent())
        .build()
        .map_err(|e| e.to_string())?;
    let mut mods = fetch_page(&client, base, 0).await?;
    if mods.is_empty() {
        return Err("Katalog ist leer".into());
    }
    let mut offset = mods.len();
    let mut pages = 1;
    while mods.len() % PAGE == 0 && pages < MAX_PAGES {
        let start = offset;
        let futures = (0..PARALLEL).map(|i| fetch_page(&client, base, start + i * PAGE));
        let results = futures::future::join_all(futures).await;
        let mut last_len = 0;
        for result in results {
            let page = result?;
            last_len = page.len();
            mods.extend(page);
            offset += PAGE;
            pages += 1;
            if last_len < PAGE {
                break;
            }
        }
        if last_len < PAGE {
            break;
        }
    }
    // Doppelte Slugs entfernen (Seiten können sich bei Änderungen überschneiden).
    let mut seen = HashSet::new();
    mods.retain(|m| !m.slug.is_empty() && seen.insert(m.slug.clone()));
    if mods.is_empty() {
        return Err("Katalog ist leer".into());
    }
    let cached = Cached { mods, fetched_at: now() };
    let info = cache.info(&cached);
    cache.save_disk(&cached);
    *cache.inner.lock().unwrap() = Some(cached);
    cache.mark_success();
    Ok(info)
}

fn matches_text(m: &Mashup, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut hay = format!(
        "{} {} {} {} {}",
        m.title, m.tagline, m.slug, m.creator.name, m.made_by
    )
    .to_lowercase();
    for game in &m.games {
        hay.push(' ');
        hay.push_str(&game.slug.to_lowercase());
        hay.push(' ');
        hay.push_str(&game.name.to_lowercase());
        if let Some(short) = &game.short_name {
            hay.push(' ');
            hay.push_str(&short.to_lowercase());
        }
    }
    hay.contains(needle)
}

fn matches_installed(m: &Mashup, installed: &HashSet<String>) -> bool {
    m.games.iter().any(|g| {
        installed.contains(&g.slug)
            || g.steam_app_id.as_ref().is_some_and(|id| installed.contains(&format!("steam:{id}")))
    })
}

fn matches_game(m: &Mashup, game: &str) -> bool {
    m.games.iter().any(|g| g.slug == game || g.steam_app_id.as_deref() == Some(game))
}

pub fn facets(mods: &[Mashup]) -> Vec<GameFacet> {
    let mut map: std::collections::HashMap<String, GameFacet> = std::collections::HashMap::new();
    for m in mods {
        for g in &m.games {
            let entry = map.entry(g.slug.clone()).or_insert_with(|| GameFacet {
                slug: g.slug.clone(),
                name: g.name.clone(),
                short_name: g.short_name.clone().unwrap_or_else(|| g.name.clone()),
                count: 0,
            });
            entry.count += 1;
        }
    }
    let mut out: Vec<GameFacet> = map.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

pub fn filter(mods: &[Mashup], q: &SearchQuery) -> Vec<Mashup> {
    let needle = q.text.trim().to_lowercase();
    let mut filtered: Vec<Mashup> = mods
        .iter()
        .filter(|m| matches_text(m, &needle))
        .filter(|m| q.game.as_deref().is_none_or(|game| matches_game(m, game)))
        .filter(|m| !q.installed_only || matches_installed(m, &q.installed_games))
        .cloned()
        .collect();
    match q.sort.as_str() {
        "new" => filtered.sort_by(|a, b| b.published_at.cmp(&a.published_at).then_with(|| a.title.cmp(&b.title))),
        "updated" => filtered.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.title.cmp(&b.title))),
        "rating" => filtered.sort_by(|a, b| {
            b.stars.partial_cmp(&a.stars).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.installs.cmp(&a.installs))
        }),
        "name" => filtered.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        // "top" und Standard: meiste Installationen, dann Aufrufe.
        _ => filtered.sort_by(|a, b| b.installs.cmp(&a.installs).then_with(|| b.gets.cmp(&a.gets))),
    }
    filtered
}

pub async fn search(cache: &MashupCache, base: &Url, q: SearchQuery) -> Result<SearchResult, String> {
    let (mods, info) = match cache.snapshot() {
        Some((mods, info)) if !info.stale => (mods, info),
        Some((mods, info)) => {
            if !cache.should_retry() {
                (mods, info)
            } else {
                match refresh(cache, base).await {
                    // Nach erfolgreicher Erneuerung den frischen Stand verwenden.
                    Ok(fresh) => (cache.snapshot().map(|(mods, _)| mods).unwrap_or(mods), fresh),
                    // Offline: den letzten Stand weiter anzeigen, `stale` signalisiert das in der UI.
                    Err(_) => {
                        cache.mark_failure();
                        (mods, info)
                    }
                }
            }
        }
        None => {
            let info = refresh(cache, base).await?;
            let (mods, _) = cache.snapshot().ok_or_else(|| "Katalog nicht verfügbar".to_string())?;
            (mods, info)
        }
    };
    let all_facets = facets(&mods);
    let filtered = filter(&mods, &q);
    let total = filtered.len();
    let limit = q.limit.clamp(1, 120);
    let offset = q.offset.min(total);
    let items = filtered.into_iter().skip(offset).take(limit).collect();
    Ok(SearchResult { items, total, offset, limit, catalog: info, games: all_facets })
}

pub async fn detail(base: &Url, slug: &str) -> Result<Value, String> {
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("Ungültiger Mashup-Name".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(Config::user_agent())
        .build()
        .map_err(|e| e.to_string())?;
    let url = base.join(&format!("/api/mods/{slug}")).map_err(|e| e.to_string())?;
    let response = client.get(url).header("accept", "application/json").send().await.map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("Mashup nicht gefunden".into());
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let value: Value = response.json().await.map_err(|e| format!("Ungültige Antwort: {e}"))?;
    Ok(value.get("mod").cloned().unwrap_or(value))
}

/// Absolute Medien-URL (die API liefert relative Pfade).
#[allow(dead_code)] // von der Oberfläche genutzt; hier für Tests und spätere Aufrufer
pub fn media_url(base: &Url, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else if path.starts_with('/') {
        base.join(path).map(|u| u.to_string()).unwrap_or_else(|_| path.to_string())
    } else {
        base.join(&format!("/{path}")).map(|u| u.to_string()).unwrap_or_else(|_| path.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(slug: &str, title: &str, installs: i64, stars: f64, games: &[&str]) -> Mashup {
        Mashup {
            id: slug.into(),
            slug: slug.into(),
            title: title.into(),
            tagline: format!("{title} tagline"),
            creator: Creator { name: "Mecke".into(), ..Default::default() },
            games: games
                .iter()
                .map(|g| GameRef { slug: g.to_string(), name: g.to_string(), short_name: None, steam_app_id: None, store_url: None })
                .collect(),
            installs,
            stars,
            published_at: Some(format!("2026-10-{:02}", installs.clamp(1, 28))),
            ..Default::default()
        }
    }

    fn sample() -> Vec<Mashup> {
        vec![
            m("minecraft-ring", "Minecraft Ring", 10, 4.5, &["elden-ring", "minecraft-java"]),
            m("gta-craft", "GTA Craft", 50, 3.9, &["gta-v", "minecraft-java"]),
            m("portal-craft", "Portal Craft", 20, 4.9, &["portal-2"]),
        ]
    }

    #[test]
    fn filters_by_text_game_and_install() {
        let all = sample();
        let mut q = SearchQuery { text: "minecraft".into(), ..Default::default() };
        assert_eq!(filter(&all, &q).len(), 2);
        q.text.clear();
        q.game = Some("portal-2".into());
        assert_eq!(filter(&all, &q)[0].slug, "portal-craft");
        q.game = None;
        q.installed_only = true;
        q.installed_games = ["elden-ring".to_string()].into_iter().collect();
        assert_eq!(filter(&all, &q)[0].slug, "minecraft-ring");
    }

    #[test]
    fn sorts_and_pages_locally() {
        let all = sample();
        let mut q = SearchQuery { sort: "top".into(), ..Default::default() };
        assert_eq!(filter(&all, &q)[0].slug, "gta-craft");
        q.sort = "rating".into();
        assert_eq!(filter(&all, &q)[0].slug, "portal-craft");
        q.sort = "new".into();
        assert_eq!(filter(&all, &q)[0].slug, "gta-craft");
        q.sort = "name".into();
        assert_eq!(filter(&all, &q)[0].slug, "gta-craft");
    }

    #[test]
    fn facets_count_games() {
        let f = facets(&sample());
        let mc = f.iter().find(|g| g.slug == "minecraft-java").unwrap();
        assert_eq!(mc.count, 2);
    }

    #[test]
    fn tolerates_null_fields() {
        let m: Mashup = serde_json::from_str(
            r#"{"id":"x","slug":"x","title":null,"tagline":null,"creator":null,"games":null,
                "pricing":null,"priceCents":null,"coverUrl":null,"previewMedia":null,"madeBy":null,
                "version":null,"status":null,"gets":null,"installs":null,"publishedAt":null,
                "updatedAt":null,"stars":null,"remixOfId":null,"play":null}"#,
        )
        .unwrap();
        assert_eq!(m.title, "");
        assert_eq!(m.creator, Creator::default());
        assert!(m.games.is_empty());
        assert_eq!(m.installs, 0);
        assert_eq!(m.stars, 0.0);
    }

    #[test]
    fn failure_backoff_blocks_automatic_retry() {
        let dir = std::env::temp_dir().join(format!("melty-mashup-backoff-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = MashupCache::new(dir.clone());
        assert!(cache.should_retry());
        cache.mark_failure();
        assert!(!cache.should_retry(), "nach einem Fehler 60 s warten");
        cache.mark_success();
        assert!(cache.should_retry());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn media_urls_are_absolute() {
        let base = Url::parse("https://melty.gg").unwrap();
        assert_eq!(media_url(&base, "/api/mods/media/x"), "https://melty.gg/api/mods/media/x");
        assert_eq!(media_url(&base, "https://cdn/x"), "https://cdn/x");
    }

    #[test]
    fn catalog_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("melty-mashup-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = MashupCache::new(dir.clone());
        let cached = Cached { mods: sample(), fetched_at: now() };
        cache.save_disk(&cached);
        let again = MashupCache::new(dir.clone());
        let (mods, info) = again.snapshot().unwrap();
        assert_eq!(mods.len(), 3);
        assert!(!info.stale);
        let _ = std::fs::remove_dir_all(dir);
    }
}
