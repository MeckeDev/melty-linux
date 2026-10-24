//! Wire formats of the Melty desktop mod API (`install-ticket`, `/api/desktop/mods/library`).
//!
//! Everything that comes from the network is validated here before it is written to disk. The shapes are
//! taken from the public protocol used by Melty 1.0.132 (see `ANALYSIS.md` and `SPIKE.md`); this is an
//! independent implementation and contains no code from the original client.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

const ROOTS: &[&str] = &[
    "game",
    "documents",
    "my-games",
    "saved-games",
    "appdata",
    "localappdata",
    "managed",
    "minecraft-instance",
    "locallow",
    "userprofile",
    "steam",
];
const KINDS: &[&str] = &["main", "optional", "symbols", "extra"];

fn mod_id_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn slug_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `normalizeEntryPath` from the recipe format: returns a relative `a/b/c`, or `None` for anything that
/// could escape the destination (absolute paths, drive letters, `..`, control characters).
pub fn normalize_entry(raw: &str) -> Option<String> {
    let unified = raw.replace('\\', "/");
    let unified = unified.strip_prefix("./").unwrap_or(&unified);
    if unified.is_empty() || unified.starts_with('/') {
        return None;
    }
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let mut parts = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        if part.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
            return None;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// `isJunkPath` from the original: metadata that must never be installed.
pub fn is_junk_path(path: &str) -> bool {
    let unified = path.replace('\\', "/");
    let parts: Vec<&str> = unified.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| p.eq_ignore_ascii_case("__macosx")) {
        return true;
    }
    let name = parts.last().copied().unwrap_or("").to_ascii_lowercase();
    matches!(name.as_str(), ".ds_store" | "thumbs.db" | "ehthumbs.db" | "desktop.ini")
        || name.starts_with("._")
        || name.ends_with(".lnk")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: String,
    pub game_slug: Option<String>,
    pub subpath: String,
}

/// `parseTarget` from the recipe format.
pub fn parse_target(value: &str) -> Option<Target> {
    let text = value.trim();
    let rest = text.strip_prefix('{')?;
    let close = rest.find('}')?;
    let head = &rest[..close];
    let mut split = head.splitn(2, ':');
    let root = split.next()?.to_string();
    let game_slug = split.next().map(str::to_string);
    if !ROOTS.contains(&root.as_str()) {
        return None;
    }
    if game_slug.is_some() && root != "game" {
        return None;
    }
    if let Some(slug) = &game_slug {
        if !slug_ok(slug) {
            return None;
        }
    }
    let tail = &rest[close + 1..];
    let subpath = if tail.is_empty() {
        String::new()
    } else {
        let tail = tail.strip_prefix('/')?;
        if tail.is_empty() {
            return None;
        }
        normalize_entry(tail)?
    };
    Some(Target { root, game_slug, subpath })
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub games: Vec<RecipeGame>,
    #[serde(default)]
    pub launch: Option<Launch>,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    #[serde(default)]
    pub settings: Vec<SettingsFile>,
    #[serde(default)]
    pub runtime_data: Vec<String>,
    #[serde(default)]
    pub setup: Option<Setup>,
    #[serde(default)]
    pub together: Vec<Together>,
}

/// Ein Begleitprogramm, das zusammen mit dem Hauptspiel läuft (z. B. der Minecraft-Client).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Together {
    #[serde(default)]
    pub launch: Option<Launch>,
    /// `true`: Begleitprogramm zuerst starten, danach `waitSeconds` warten und das Hauptspiel starten.
    /// `false`/fehlend: Hauptspiel zuerst starten, danach `waitSeconds` warten und das Begleitprogramm starten.
    #[serde(default)]
    pub start_first: Option<bool>,
    #[serde(default)]
    pub wait_seconds: Option<u64>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub stop_with_primary: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub done: Option<SetupDone>,
    #[serde(default)]
    pub launch: Option<Launch>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetupDone {
    pub file: String,
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub id: String,
    pub file_name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub extract: Option<bool>,
}

impl Component {
    /// Components that end up in the installation. Optional/extra/symbols components are not installed by
    /// default (the website offers them separately; the desktop protocol only maps required/main files).
    pub fn installs(&self) -> bool {
        self.required || self.kind == "main"
    }

    /// A file is unpacked unless the recipe says otherwise and it looks like an archive.
    pub fn unpacks(&self) -> bool {
        self.extract.unwrap_or_else(|| {
            let name = self.file_name.to_ascii_lowercase();
            [".zip", ".7z", ".rar", ".tar", ".tgz", ".txz", ".tbz", ".tbz2", ".gz", ".xz", ".bz2"]
                .iter()
                .any(|ext| name.ends_with(ext))
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Mapping {
    pub component: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecipeGame {
    pub slug: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub own_copy: bool,
    #[serde(default)]
    pub steam_app_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsFile {
    pub file: String,
    #[serde(default)]
    pub set: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    #[serde(default)]
    pub kind: String,
    /// Spiel-Slug für `kind: "game"` in einem `together`-Eintrag.
    #[serde(default)]
    pub game: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub ticket_version: u32,
    #[serde(rename = "mod")]
    pub mashup: TicketMod,
    pub release: Release,
    pub recipe: Recipe,
    #[serde(default)]
    pub files: Vec<TicketFile>,
    #[serde(default)]
    pub depends_on: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketMod {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub id: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketFile {
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependency {
    pub slug: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMod {
    pub mod_id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub release_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub game_slugs: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct LibraryResponse {
    #[serde(default)]
    pub mods: Vec<LibraryMod>,
}

/// A planned copy: `source` is relative to the staging directory of `component`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub component: String,
    pub source: String,
    pub target: Target,
    pub size: u64,
}

pub fn validate_ticket(ticket: Ticket) -> Result<Ticket, String> {
    if ticket.ticket_version != 1 {
        return Err("melty.gg sent an install ticket with an unknown version.".into());
    }
    if !mod_id_ok(&ticket.mashup.id) {
        return Err("melty.gg sent an invalid mashup id.".into());
    }
    if !slug_ok(&ticket.mashup.slug) {
        return Err("melty.gg sent an invalid mashup slug.".into());
    }
    if ticket.mashup.title.trim().is_empty() || ticket.mashup.title.len() > 120 {
        return Err("melty.gg sent an invalid mashup title.".into());
    }
    if ticket.release.id.trim().is_empty() || ticket.release.id.len() > 200 {
        return Err("melty.gg sent an invalid release.".into());
    }
    if ticket.release.version.len() > 40 {
        return Err("melty.gg sent an invalid version.".into());
    }

    let mut component_ids = HashSet::new();
    for component in &ticket.recipe.components {
        if component.id.len() > 64
            || component.id.is_empty()
            || !component.id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            || !component.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
            || !component_ids.insert(component.id.clone())
        {
            return Err("melty.gg sent a recipe with an invalid component.".into());
        }
        if component.file_name.trim().is_empty() || component.file_name.len() > 255 {
            return Err("melty.gg sent a recipe with a missing file name.".into());
        }
        if !KINDS.contains(&component.kind.as_str()) {
            return Err("melty.gg sent a recipe with an unknown file kind.".into());
        }
        if component.label.len() > 80 {
            return Err("melty.gg sent a recipe with a long label.".into());
        }
    }
    for mapping in &ticket.recipe.mappings {
        if !component_ids.contains(&mapping.component) {
            return Err("melty.gg sent a mapping for an unknown file.".into());
        }
        if parse_target(&mapping.to).is_none() {
            return Err(format!("melty.gg sent an unsafe install folder: {}", mapping.to));
        }
        let raw = mapping.from.replace('\\', "/");
        if !raw.is_empty() {
            let directory = raw.ends_with('/');
            let normalized = normalize_entry(&raw);
            if normalized.is_none() || (!directory && normalized.as_deref() != Some(raw.as_str())) {
                return Err("melty.gg sent an unsafe source path.".into());
            }
        }
    }
    for game in &ticket.recipe.games {
        if !slug_ok(&game.slug) {
            return Err("melty.gg sent an invalid game slug.".into());
        }
        if !matches!(game.role.as_str(), "primary" | "companion" | "secondary") {
            return Err("melty.gg sent an invalid game role.".into());
        }
        if let Some(app) = &game.steam_app_id {
            if app.is_empty() || !app.bytes().all(|b| b.is_ascii_digit()) {
                return Err("melty.gg sent an invalid Steam app id.".into());
            }
        }
    }
    if ticket.recipe.games.iter().filter(|g| g.role == "primary").count() > 1 {
        return Err("melty.gg sent a recipe with more than one main game.".into());
    }
    if let Some(launch) = &ticket.recipe.launch {
        validate_launch(launch, &component_ids)?;
    }
    if let Some(setup) = &ticket.recipe.setup {
        if setup.label.len() > 80 || setup.label.chars().any(|c| c.is_control()) {
            return Err("melty.gg sent a setup with an invalid label.".into());
        }
        if let Some(done) = &setup.done {
            let target = parse_target(&done.file).ok_or_else(|| "melty.gg sent an unsafe setup marker.".to_string())?;
            if target.subpath.is_empty() {
                return Err("melty.gg sent an unsafe setup marker.".into());
            }
            if let Some(marker) = &done.contains {
                if marker.len() > 200 || marker.chars().any(|c| c.is_control()) {
                    return Err("melty.gg sent an invalid setup marker.".into());
                }
            }
        }
        if let Some(launch) = &setup.launch {
            validate_launch(launch, &component_ids)?;
        }
    }
    if ticket.recipe.together.len() > 4 {
        return Err("melty.gg sent too many companion programs.".into());
    }
    for together in &ticket.recipe.together {
        let Some(launch) = &together.launch else {
            return Err("melty.gg sent a companion without a program.".into());
        };
        validate_launch(launch, &component_ids)?;
        if together.wait_seconds.is_some_and(|seconds| seconds > 300) {
            return Err("melty.gg sent an invalid companion delay.".into());
        }
        if launch.kind == "game" {
            let Some(slug) = launch.game.as_deref() else {
                return Err("melty.gg sent a companion game without a game slug.".into());
            };
            if !slug_ok(slug) || !ticket.recipe.games.iter().any(|game| game.slug == slug) {
                return Err("melty.gg sent a companion for an unknown game.".into());
            }
        }
    }
    for requirement in &ticket.recipe.requirements {
        if !matches!(requirement.kind.as_str(), "loader" | "mod" | "library" | "external") {
            return Err("melty.gg sent a recipe with an unknown requirement.".into());
        }
        if requirement.id.len() > 120 || requirement.id.chars().any(|c| c.is_control()) {
            return Err("melty.gg sent a recipe with an invalid requirement.".into());
        }
    }
    if ticket.recipe.settings.len() > 4 {
        return Err("melty.gg sent too many settings files.".into());
    }
    for entry in &ticket.recipe.runtime_data {
        let target = parse_target(entry);
        if target.is_none() || target.as_ref().is_some_and(|t| t.subpath.is_empty() || t.root == "game") {
            return Err(format!("melty.gg sent an unsafe data folder: {entry}"));
        }
    }

    let mut names = HashSet::new();
    for file in &ticket.files {
        if file.file_name.trim().is_empty() || file.file_name.len() > 255 || file.file_name.contains('/') || file.file_name.contains('\\') {
            return Err("melty.gg sent an invalid file name.".into());
        }
        if !names.insert(file.file_name.clone()) {
            return Err("melty.gg sent the same file twice.".into());
        }
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("melty.gg sent a file without a valid checksum.".into());
        }
        if file.size > 40 * 1024 * 1024 * 1024 {
            return Err("melty.gg sent a download that is too large.".into());
        }
        let url = url::Url::parse(&file.url).map_err(|_| "melty.gg sent an invalid download link.".to_string())?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("melty.gg sent a download link that is not encrypted.".into());
        }
    }
    for component in &ticket.recipe.components {
        if component.installs() && !names.contains(&component.file_name) {
            return Err(format!("melty.gg did not send the file {}.", component.file_name));
        }
    }
    for dependency in &ticket.depends_on {
        if !slug_ok(&dependency.slug) {
            return Err("melty.gg sent an invalid dependency.".into());
        }
    }
    Ok(ticket)
}

fn validate_launch(launch: &Launch, components: &HashSet<String>) -> Result<(), String> {
    let plain = |s: &str| !s.is_empty() && s.len() <= 400 && !s.contains(['\r', '\n', '\0']);
    match launch.kind.as_str() {
        "none" => Ok(()),
        "game" => {
            if launch.args.len() > 32 || !launch.args.iter().all(|a| plain(a)) {
                return Err("melty.gg sent invalid game arguments.".into());
            }
            validate_launch_env(launch)?;
            Ok(())
        }
        "exe" => {
            let path = launch.path.as_deref().unwrap_or("");
            let target = parse_target(path).ok_or_else(|| "melty.gg sent an unsafe program path.".to_string())?;
            let name = path.to_ascii_lowercase();
            if target.subpath.is_empty() || !(name.ends_with(".exe") || name.ends_with(".bat") || name.ends_with(".cmd")) {
                return Err("melty.gg sent a program this client cannot start.".into());
            }
            if launch.args.len() > 32 || !launch.args.iter().all(|a| plain(a)) {
                return Err("melty.gg sent invalid program arguments.".into());
            }
            validate_launch_env(launch)?;
            if let Some(cwd) = &launch.cwd {
                if parse_target(cwd).is_none() {
                    return Err("melty.gg sent an unsafe working folder.".into());
                }
            }
            Ok(())
        }
        "uri" => {
            let uri = launch.uri.as_deref().unwrap_or("");
            let lower = uri.to_ascii_lowercase();
            if lower.starts_with("steam://") || lower.starts_with("com.epicgames.launcher://") || lower.starts_with("https://") {
                Ok(())
            } else {
                Err("melty.gg sent an unsupported launch link.".into())
            }
        }
        "html" => {
            if launch.component.as_deref().is_some_and(|c| components.contains(c))
                && launch.entry.as_deref().and_then(normalize_entry).is_some_and(|e| e.to_ascii_lowercase().ends_with(".html") || e.to_ascii_lowercase().ends_with(".htm"))
            {
                Ok(())
            } else {
                Err("melty.gg sent an invalid HTML launch.".into())
            }
        }
        _ => Err("melty.gg sent an unknown launch kind.".into()),
    }
}

fn validate_launch_env(launch: &Launch) -> Result<(), String> {
    let plain = |s: &str| !s.is_empty() && s.len() <= 400 && !s.contains(['\r', '\n', '\0']);
    if launch.env.len() > 8 {
        return Err("melty.gg sent too many environment variables.".into());
    }
    for (key, value) in &launch.env {
        if !key.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || key.len() > 64
            || !plain(value)
        {
            return Err("melty.gg sent an invalid environment variable.".into());
        }
    }
    Ok(())
}

/// Plan the files that a recipe maps into the installation.
///
/// `entries` are the files of one component (relative to its staging directory); the returned sources are
/// relative to that staging directory, because each component is extracted into its own folder.
pub fn plan_files(recipe: &Recipe, entries: &BTreeMap<String, Vec<StagedEntry>>) -> Vec<PlannedFile> {
    let mut planned = Vec::new();
    for (component, files) in entries {
        let paths: HashSet<String> = files.iter().filter_map(|e| normalize_entry(&e.path)).collect();
        let own: Vec<(&Mapping, String, Target)> = recipe
            .mappings
            .iter()
            .filter(|m| &m.component == component)
            .filter_map(|m| parse_target(&m.to).map(|t| (m, source_of(&m.from, &paths), t)))
            .collect();
        for file in files {
            let Some(path) = normalize_entry(&file.path) else { continue };
            if is_junk_path(&path) {
                continue;
            }
            for (_, from, target) in &own {
                if let Some(subpath) = placed_at(from, &path, &target.subpath) {
                    let mut target = target.clone();
                    target.subpath = subpath;
                    planned.push(PlannedFile {
                        component: component.clone(),
                        source: path.clone(),
                        target,
                        size: file.size,
                    });
                    break;
                }
            }
        }
    }
    planned
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    pub path: String,
    pub size: u64,
}

fn source_of(from: &str, paths: &HashSet<String>) -> String {
    if from.is_empty() || from.ends_with('/') || paths.contains(from) {
        from.to_string()
    } else if paths.iter().any(|p| p.starts_with(&format!("{from}/"))) {
        format!("{from}/")
    } else {
        from.to_string()
    }
}

fn placed_at(from: &str, path: &str, to: &str) -> Option<String> {
    if from.is_empty() {
        return join_path(to, path);
    }
    if from.ends_with('/') {
        return path.strip_prefix(from).and_then(|rest| join_path(to, rest));
    }
    if path != from {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if to.rsplit('/').next() == Some(name) {
        Some(to.to_string())
    } else {
        join_path(to, name)
    }
}

fn join_path(a: &str, b: &str) -> Option<String> {
    if b.is_empty() {
        return Some(a.to_string());
    }
    if a.is_empty() {
        return normalize_entry(b);
    }
    normalize_entry(&format!("{a}/{b}"))
}

/// A resolved destination. `root` is the trusted base; `rel` is always a safe relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub root: PathBuf,
    pub rel: String,
    pub token: String,
    pub game_slug: Option<String>,
}

impl ResolvedTarget {
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_are_strict() {
        assert_eq!(normalize_entry("a\\b/./c").as_deref(), Some("a/b/c"));
        for bad in ["", "/etc/passwd", "C:\\Windows", "../x", "a/../b", "a//../../b", "a\0b", "a:b"] {
            assert!(normalize_entry(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn targets_parse_like_the_site() {
        assert_eq!(
            parse_target("{game}/BepInEx/plugins"),
            Some(Target { root: "game".into(), game_slug: None, subpath: "BepInEx/plugins".into() })
        );
        assert_eq!(
            parse_target("{game:peak}/BepInEx"),
            Some(Target { root: "game".into(), game_slug: Some("peak".into()), subpath: "BepInEx".into() })
        );
        assert_eq!(
            parse_target("{managed}"),
            Some(Target { root: "managed".into(), game_slug: None, subpath: String::new() })
        );
        for bad in ["{game}/../x", "game", "{game:peak}/../x", "{game:PEAK}", "{managed:x}/a", "{unknown}/a", "{game}/"] {
            assert!(parse_target(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn planning_matches_original_rules() {
        let recipe = Recipe {
            mappings: vec![
                Mapping { component: "main".into(), from: "Mod/".into(), to: "{game}/Mods".into() },
                Mapping { component: "single".into(), from: "".into(), to: "{managed}".into() },
            ],
            ..Default::default()
        };
        let mut entries = BTreeMap::new();
        entries.insert(
            "main".to_string(),
            vec![
                StagedEntry { path: "Mod/a.dll".into(), size: 3 },
                StagedEntry { path: "Mod/__MACOSX/x".into(), size: 1 },
                StagedEntry { path: "Other/b.dll".into(), size: 2 },
            ],
        );
        entries.insert("single".to_string(), vec![StagedEntry { path: "run.cmd".into(), size: 9 }]);
        let planned = plan_files(&recipe, &entries);
        assert_eq!(planned.len(), 2);
        assert!(planned.iter().any(|p| p.source == "Mod/a.dll" && p.target.subpath == "Mods/a.dll"));
        assert!(planned.iter().any(|p| p.source == "run.cmd" && p.target.subpath == "run.cmd"));
    }

    #[test]
    fn setup_marker_and_launch_are_validated() {
        let mut ticket = Ticket {
            ticket_version: 1,
            mashup: TicketMod { id: "abc".into(), slug: "my-mod".into(), title: "My mod".into() },
            release: Release { id: "r1".into(), version: "1.0".into() },
            recipe: Recipe {
                components: vec![Component { id: "main".into(), file_name: "a.zip".into(), kind: "main".into(), required: true, ..Default::default() }],
                mappings: vec![Mapping { component: "main".into(), from: "".into(), to: "{game}/Mod".into() }],
                setup: Some(Setup {
                    label: "Minecraft".into(),
                    done: Some(SetupDone { file: "{localappdata}/Game/log.txt".into(), contains: Some("ready".into()) }),
                    launch: Some(Launch {
                        kind: "exe".into(),
                        path: Some("{localappdata}/Game/setup.exe".into()),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            },
            files: vec![TicketFile { file_name: "a.zip".into(), size: 3, sha256: "a".repeat(64), url: "https://example.com/a.zip".into() }],
            depends_on: vec![],
        };
        assert!(validate_ticket(ticket.clone()).is_ok());
        ticket.recipe.setup.as_mut().unwrap().done.as_mut().unwrap().file = "{localappdata}/../etc".into();
        assert!(validate_ticket(ticket).is_err());
    }

    #[test]
    fn together_companions_are_parsed_and_validated() {
        let recipe: Recipe = serde_json::from_str(
            r#"{
                "components": [{"id":"main","fileName":"a.zip","kind":"main","required":true}],
                "games": [{"slug":"peak","role":"primary"},{"slug":"minecraft-java","role":"companion"}],
                "launch": {"kind":"game"},
                "together": [
                    {"launch":{"kind":"exe","path":"{localappdata}/Prism/prismlauncher.exe","args":["--launch","PeakCraft"]},"startFirst":true,"waitSeconds":20},
                    {"launch":{"kind":"game","game":"minecraft-java"},"startFirst":null}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(recipe.together.len(), 2);
        assert_eq!(recipe.together[0].start_first, Some(true));
        assert_eq!(recipe.together[0].wait_seconds, Some(20));
        assert_eq!(recipe.together[0].launch.as_ref().unwrap().kind, "exe");
        assert_eq!(recipe.together[1].start_first, None);
        assert_eq!(recipe.together[1].launch.as_ref().unwrap().game.as_deref(), Some("minecraft-java"));

        let mut ticket = Ticket {
            ticket_version: 1,
            mashup: TicketMod { id: "abc".into(), slug: "my-mod".into(), title: "My mod".into() },
            release: Release { id: "r1".into(), version: "1.0".into() },
            recipe,
            files: vec![TicketFile { file_name: "a.zip".into(), size: 3, sha256: "a".repeat(64), url: "https://example.com/a.zip".into() }],
            depends_on: vec![],
        };
        assert!(validate_ticket(ticket.clone()).is_ok());
        ticket.recipe.together[0].wait_seconds = Some(301);
        assert!(validate_ticket(ticket.clone()).is_err());
        ticket.recipe.together[0].wait_seconds = Some(20);
        ticket.recipe.together[1].launch.as_mut().unwrap().game = Some("unknown-game".into());
        assert!(validate_ticket(ticket).is_err());
    }

    #[test]
    fn ticket_validation_rejects_traversal_and_plain_http() {
        let mut ticket = Ticket {
            ticket_version: 1,
            mashup: TicketMod { id: "abc".into(), slug: "my-mod".into(), title: "My mod".into() },
            release: Release { id: "r1".into(), version: "1.0".into() },
            recipe: Recipe {
                components: vec![Component { id: "main".into(), file_name: "a.zip".into(), kind: "main".into(), required: true, ..Default::default() }],
                mappings: vec![Mapping { component: "main".into(), from: "".into(), to: "{game}/Mod".into() }],
                ..Default::default()
            },
            files: vec![TicketFile { file_name: "a.zip".into(), size: 3, sha256: "a".repeat(64), url: "https://example.com/a.zip".into() }],
            depends_on: vec![],
        };
        assert!(validate_ticket(ticket.clone()).is_ok());
        ticket.files[0].url = "http://example.com/a.zip".into();
        assert!(validate_ticket(ticket.clone()).is_err());
        ticket.files[0].url = "https://example.com/a.zip".into();
        ticket.recipe.mappings[0].to = "{game}/../../etc".into();
        assert!(validate_ticket(ticket).is_err());
    }
}
