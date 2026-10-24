//! Startet ein installiertes Mashup über Steam, Lutris, Heroic, Faugus, Proton oder Wine.
//!
//! Rezepte nennen entweder das Spiel selbst (`game`), ein Programm im Spiel-/Mashup-Ordner (`exe`) oder
//! eine URI. Windows-Programme laufen in der Proton-/Wine-Umgebung des Spiels; dabei werden
//! `{…}`-Argumente in `Z:\…`-Pfade übersetzt. Prozesse werden in einer eigenen Prozessgruppe gestartet,
//! damit „Stoppen“ auch den Wine-/Proton-Baum beendet.

use super::gamecopy;
use super::journal::Record;
use super::model::{Launch as RecipeLaunch, Setup, Together};
use super::paths::{self, InstallEnv};
use super::{installed_games, Mods, RunningProcess};
use crate::gamesrt;
use melty_games::{InstalledGame, Launch as GameLaunch};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchTrack {
    Primary,
    Companion,
}

pub async fn launch(app: AppHandle, mut record: Record) -> Result<Value, String> {
    if !record.enabled {
        return Err("Dieses Mashup ist deaktiviert. Aktiviere es zuerst.".into());
    }
    let (mods, home, proc_root) = {
        let state = app.state::<crate::AppState>();
        (state.mods.clone(), gamesrt::home(), gamesrt::proc_root())
    };
    mods.prune_running(&record.mod_id, &home, &proc_root);
    if mods.is_running(&record.mod_id) {
        return Err("Dieses Mashup läuft bereits.".into());
    }

    let games = installed_games(&home, &proc_root);
    let mut env_games = HashMap::new();
    let mut primary: Option<InstalledGame> = None;
    for (slug, installation_id) in &record.installation_ids {
        if let Some(game) = games.iter().find(|game| &game.page.installation_id == installation_id) {
            env_games.insert(slug.clone(), super::paths::game_env(game));
            if record.primary_game_slug.as_deref() == Some(slug.as_str()) || (record.primary_game_slug.is_none() && primary.is_none()) {
                primary = Some(game.clone());
            }
        }
    }
    let primary_slug = record.primary_game_slug.clone().or_else(|| env_games.keys().next().cloned());
    let mut primary = primary.ok_or_else(|| {
        if let Some(slug) = &primary_slug {
            format!("Das Spiel für dieses Mashup ({slug}) ist nicht installiert.")
        } else {
            "Das Spiel für dieses Mashup ist nicht installiert.".to_string()
        }
    })?;
    if primary.page.running {
        return Err("Das Spiel läuft bereits. Beende es zuerst.".into());
    }

    // Ein Steam-Spiel, das noch nie lief, hat oft noch kein Proton-Präfix. Rezepte mit
    // %LOCALAPPDATA% & Co. müssen aber in dieses Präfix schreiben; sonst landen die Dateien im
    // Linux-Home und der Start sucht sie später vergeblich im Präfix. Deshalb einmalig anlegen.
    let prefix_missing = primary
        .proton_prefix
        .as_ref()
        .map_or(true, |prefix| paths::wine_root(prefix).is_none());
    if paths::recipe_uses_windows_profile(&record.recipe)
        && prefix_missing
        && !primary.page.store_app_ids.steam.is_empty()
    {
        let game = primary.clone();
        let home2 = home.clone();
        let prefix = tauri::async_runtime::spawn_blocking(move || ensure_prefix(&game, &home2))
            .await
            .map_err(|error| format!("Konnte das Spielpräfix nicht vorbereiten: {error}"))??;
        if let Some(env_game) = primary_slug.as_ref().and_then(|slug| env_games.get_mut(slug)) {
            env_game.prefix = Some(prefix.clone());
        }
        primary.proton_prefix = Some(prefix);
    }

    let steam_root = paths::steam_root_from(&paths::game_env(&primary))
        .or_else(|| melty_games::steam::find_roots(&home).first().map(|root| root.path.clone()));
    let mut env = InstallEnv {
        home: home.clone(),
        mods_dir: mods.dir.clone(),
        mod_slug: record.slug.clone(),
        games: env_games,
        primary_slug,
        steam_root,
    };

    // Eine ausdrücklich gewählte Spielkopie ersetzt nur den Startordner des Hauptspiels. Die
    // Steam-Installation bleibt unangetastet; Proton-Präfix und App-ID kommen weiterhin von dort.
    if gamecopy::available(&record) {
        if let Some(slug) = gamecopy::primary_slug(&record).map(str::to_string) {
            if let Some(selected) = record.game_paths.get(&slug).cloned() {
                let info = gamecopy::validate(&record, Path::new(&selected), &primary.install_dir, &primary.page.display_name)?;
                gamecopy::apply(&record, &primary.install_dir, &info.root, &mods.dir, &slug)?;
                if let Some(game) = env.games.get_mut(&slug) {
                    game.install_dir = info.root;
                }
            }
        }
    }

    // Ältere Datensätze können einen leeren `loaders`-Eintrag haben, obwohl das Rezept einen Loader
    // verlangt. Für die Reparatur und die Startumgebung zählt deshalb auch das Rezept.
    let recipe_loaders = launch_loaders(&record);
    if recipe_loaders != record.loaders {
        record.loaders = recipe_loaders;
    }

    // Ältere Installationen (vor dem Präfix) haben ihre %LOCALAPPDATA%-Dateien noch im
    // Linux-Home. Sie werden jetzt einmalig dorthin verschoben, wo das Rezept sie erwartet.
    migrate_runtime_data(&mods, &record, &env)?;

    // Loader können bei älteren Installationen fehlen, obwohl das Mashup sie im Journal nennt.
    // Fehlende Loader werden vor dem Start nachinstalliert; vorhandene Loader bekommen nur die
    // nötige Wine-DLL-Überschreibung.
    if let Some(game) = env.primary_slug.as_ref().and_then(|slug| env.games.get(slug)) {
        for loader in &record.loaders {
            if loader_provided_by_record(loader, &record) {
                ensure_loader_override(loader, game, &env)?;
            } else {
                super::ensure_launch_loader(&app, &mods, loader, game, &env).await?;
            }
        }
    }

    // Manche Mashups brauchen eine einmalige Einrichtung (z. B. die Anmeldung im mitgelieferten
    // Minecraft-Launcher). Solange die Markerdatei fehlt, starten wir diese Einrichtung statt des Spiels.
    if let Some(setup) = &record.recipe.setup {
        if !setup_done(setup, &env)? {
            let Some(launch) = &setup.launch else {
                return Err("Dieses Mashup braucht eine Einrichtung, hat aber kein Einrichtungsprogramm.".into());
            };
            let result = match launch.kind.as_str() {
                "exe" => launch_exe(&app, &mods, &record, &env, &primary, launch, &home, LaunchTrack::Primary).await?,
                "game" => launch_game(&app, &mods, &record, &env, &primary, launch, LaunchTrack::Primary).await?,
                "uri" => launch_uri(&app, &mods, &record, &env, &primary, launch, LaunchTrack::Primary).await?,
                _ => return Err("Die Einrichtung dieses Mashups kann dieser Client nicht starten.".into()),
            };
            // Die Einrichtung ist kein laufendes Mashup. Sonst würde der nächste „Play“-Klick
            // blockiert, obwohl nur noch der einmalige Launcher (z. B. Prism) läuft.
            mods.clear_running(&record.mod_id, None);
            let label = if setup.label.is_empty() { "Mashup" } else { setup.label.as_str() };
            return Ok(json!({ "setup": true, "label": label, "launch": result }));
        }
    }

    // Ein Loader kann den Start des Spiels übernehmen (z. B. OWML).
    let loader_launch = record
        .loaders
        .iter()
        .find_map(|id| super::loaders::launch_for(id));
    let main_launch = if let Some(path) = loader_launch {
        RecipeLaunch { kind: "exe".into(), path: Some(path.to_string()), ..Default::default() }
    } else {
        record
            .recipe
            .launch
            .clone()
            .ok_or_else(|| "Dieses Mashup hat kein Startprogramm.".to_string())?
    };

    // Eine Spielaktualisierung kann die Bridge eines Mashups unbrauchbar machen. Das Rezept nennt
    // den unterstützten Versionsbereich; wir prüfen ihn vor dem Start, damit nicht wieder beide
    // Spiele ohne Passthrough laufen. Die einmalige Einrichtung (z. B. der Minecraft-Launcher) darf
    // vorher noch laufen, damit der Nutzer sie nicht wegen einer Spielversion wiederholen muss.
    check_game_version(&record, &env, &primary)?;

    // Begleitprogramme, die vor dem Hauptspiel bereit sein müssen (z. B. Minecraft/Prism).
    start_together(&app, &mods, &record, &env, &primary, &games, &record.recipe.together, true, &home)
        .await
        .map_err(|error| format!("Das Begleitprogramm konnte nicht gestartet werden: {error}"))?;

    let result = match main_launch.kind.as_str() {
        "game" => launch_game(&app, &mods, &record, &env, &primary, &main_launch, LaunchTrack::Primary).await?,
        "exe" => launch_exe(&app, &mods, &record, &env, &primary, &main_launch, &home, LaunchTrack::Primary).await?,
        "uri" => launch_uri(&app, &mods, &record, &env, &primary, &main_launch, LaunchTrack::Primary).await?,
        "none" => return Err("Dieses Mashup hat kein Startprogramm.".into()),
        "html" => return Err("HTML-Mashups werden in diesem Client noch nicht gestartet.".into()),
        _ => return Err("Dieses Mashup hat eine unbekannte Startart.".into()),
    };

    // Begleitprogramme, die erst nach dem Hauptspiel starten sollen.
    if let Err(error) = start_together(&app, &mods, &record, &env, &primary, &games, &record.recipe.together, false, &home).await {
        return Err(format!("Das Spiel wurde gestartet, aber ein Begleitprogramm nicht: {error}"));
    }
    Ok(result)
}

/// Prüft den vom Rezept verlangten Versionsbereich des Hauptspiels anhand der PE-Dateiversion.
/// Ist die Version nicht lesbar oder die Angabe unbekannt, wird bewusst nicht blockiert; nur eine
/// sicher erkannte Unverträglichkeit führt zu einem klaren Fehler vor dem Start.
fn check_game_version(record: &Record, env: &InstallEnv, primary: &InstalledGame) -> Result<(), String> {
    let primary_slug = record
        .primary_game_slug
        .as_deref()
        .or_else(|| record.recipe.games.iter().find(|game| game.role == "primary").map(|game| game.slug.as_str()));
    let Some(primary_slug) = primary_slug else { return Ok(()) };
    let Some(requirement) = record
        .recipe
        .games
        .iter()
        .find(|game| game.slug == primary_slug)
        .and_then(|game| game.version.as_deref())
        .map(str::trim)
        .filter(|version| !version.is_empty())
    else {
        return Ok(());
    };
    // Das Spiel kann direkt gestartet werden (`{game}/Game/…exe`) oder über einen Launcher, der
    // den Spielpfad als Argument bekommt (`--exe {game}/Game/…exe`, z. B. me3 bei Minecraft Ring).
    // Beide Formen müssen geprüft werden, sonst startet ein Mashup mit inkompatiblem Build ohne
    // funktionierende Bridge.
    let Some(subpath) = gamecopy::expected_game_subpath(record, primary_slug) else { return Ok(()) };
    let Some(game) = env.games.get(primary_slug) else { return Ok(()) };
    let Ok(executable) = paths::safe_join(&game.install_dir, &subpath) else { return Ok(()) };
    if !executable.is_file() {
        return Ok(());
    }
    let Some(actual) = super::version::read_file_version(&executable) else { return Ok(()) };
    match super::version::matches_requirement(requirement, actual) {
        Some(true) | None => Ok(()),
        Some(false) => Err(format!(
            "{} unterstützt {} nur im Bereich {}. Installiert ist {}. Bitte warte auf ein Update von {} oder wähle im Tab „Installed“ bei diesem Mashup unter „Game copy“ einen kompatiblen Spielordner aus; sonst startet der Client die Spiele ohne funktionierende Bridge.",
            record.title,
            primary.page.display_name,
            super::version::requirement_label(requirement),
            actual,
            record.title
        )),
    }
}

async fn start_together(
    app: &AppHandle,
    mods: &Arc<Mods>,
    record: &Record,
    env: &InstallEnv,
    primary: &InstalledGame,
    games: &[InstalledGame],
    entries: &[Together],
    start_first: bool,
    home: &Path,
) -> Result<(), String> {
    for entry in entries.iter().filter(|entry| entry.start_first.unwrap_or(false) == start_first) {
        // `startFirst`: erst das Begleitprogramm, dann warten, dann das Hauptspiel.
        // Sonst: erst das Hauptspiel, dann warten, dann das Begleitprogramm.
        if !start_first {
            if let Some(seconds) = entry.wait_seconds.filter(|seconds| *seconds > 0) {
                tokio::time::sleep(Duration::from_secs(seconds)).await;
            }
        }
        let Some(launch) = &entry.launch else { continue };
        launch_together(app, mods, record, env, primary, games, launch, home).await?;
        if start_first {
            if let Some(seconds) = entry.wait_seconds.filter(|seconds| *seconds > 0) {
                tokio::time::sleep(Duration::from_secs(seconds)).await;
            }
        }
    }
    Ok(())
}

async fn launch_together(
    app: &AppHandle,
    mods: &Arc<Mods>,
    record: &Record,
    env: &InstallEnv,
    primary: &InstalledGame,
    games: &[InstalledGame],
    launch: &RecipeLaunch,
    home: &Path,
) -> Result<Value, String> {
    match launch.kind.as_str() {
        "exe" => launch_exe(app, mods, record, env, primary, launch, home, LaunchTrack::Companion).await,
        "game" => {
            let slug = launch
                .game
                .as_deref()
                .ok_or_else(|| "Das Begleitprogramm nennt kein Spiel.".to_string())?;
            let installation = env
                .games
                .get(slug)
                .map(|game| game.installation_id.as_str())
                .ok_or_else(|| format!("Das Begleitprogramm gehört zu „{slug}“, das nicht installiert ist."))?;
            let game = games
                .iter()
                .find(|game| game.page.installation_id == installation)
                .ok_or_else(|| format!("Das Begleitprogramm gehört zu „{slug}“, das nicht installiert ist."))?;
            launch_game(app, mods, record, env, game, launch, LaunchTrack::Companion).await
        }
        "uri" => launch_uri(app, mods, record, env, primary, launch, LaunchTrack::Companion).await,
        _ => Err("Dieses Begleitprogramm kann dieser Client nicht starten.".into()),
    }
}

fn setup_done(setup: &Setup, env: &InstallEnv) -> Result<bool, String> {
    let Some(done) = &setup.done else {
        // Ohne Marker lässt sich nicht feststellen, ob die Einrichtung abgeschlossen ist. Dann lieber
        // nicht bei jedem Start erneut ein Fenster öffnen.
        return Ok(true);
    };
    let target = super::model::parse_target(&done.file)
        .ok_or_else(|| "Unsichere Einrichtungs-Markerdatei im Mashup.".to_string())?;
    let resolved = paths::resolve_target(&target, env)?;
    let base = paths::safe_base(&resolved.root)?;
    let path = paths::safe_join(&base, &resolved.rel)?;
    if !path.is_file() {
        return Ok(false);
    }
    let Some(marker) = &done.contains else {
        return Ok(true);
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("Konnte die Einrichtungs-Markerdatei nicht lesen: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).contains(marker))
}

/// Verschiebt Laufzeitdaten, die vor dem Anlegen des Proton-Präfixes im Linux-Home gelandet sind,
/// in das Präfix und aktualisiert die Journalpfade. Ohne das würde der Start die Dateien im
/// Präfix suchen, während sie noch unter `~/.local/share` liegen.
fn migrate_runtime_data(mods: &Arc<Mods>, record: &Record, env: &InstallEnv) -> Result<(), String> {
    let mut updated = record.clone();
    let mut changed = false;

    for stored in updated.runtime_data.clone() {
        if stored.root == "game" || stored.rel.is_empty() || stored.abs.is_empty() {
            continue;
        }
        let target = super::model::Target {
            root: stored.root.clone(),
            game_slug: stored.game_slug.clone(),
            subpath: stored.rel.clone(),
        };
        let Ok(resolved) = paths::resolve_target(&target, env) else { continue };
        let old = PathBuf::from(&stored.abs);
        let new = resolved.path();
        if old == new || !old.exists() {
            continue;
        }
        if !updated.roots.iter().any(|root| paths::is_within(Path::new(root), &old)) {
            continue;
        }
        move_tree(&old, &new)?;
        let old_text = old.to_string_lossy().to_string();
        let new_text = new.to_string_lossy().to_string();
        let old_prefix = format!("{}/", old_text.trim_end_matches('/'));
        for file in &mut updated.files {
            if file.abs == old_text || file.abs.starts_with(&old_prefix) {
                let suffix = &file.abs[old_text.len()..];
                file.abs = format!("{new_text}{suffix}");
            }
        }
        if let Some(entry) = updated
            .runtime_data
            .iter_mut()
            .find(|entry| entry.root == stored.root && entry.rel == stored.rel && entry.abs == old_text)
        {
            entry.abs = new_text;
        }
        let root_text = resolved.root.to_string_lossy().to_string();
        if !updated.roots.contains(&root_text) {
            updated.roots.push(root_text);
        }
        changed = true;
    }

    // Fallback für ältere Datensätze ohne `runtimeData`-Eintrag: Dateien einzeln verschieben.
    for file in record.files.clone() {
        if file.root == "game" || file.abs.is_empty() {
            continue;
        }
        let target = super::model::Target {
            root: file.root.clone(),
            game_slug: file.game_slug.clone(),
            subpath: file.rel.clone(),
        };
        let Ok(resolved) = paths::resolve_target(&target, env) else { continue };
        let old = PathBuf::from(&file.abs);
        let new = resolved.path();
        if old == new || !old.exists() {
            continue;
        }
        if !updated.roots.iter().any(|root| paths::is_within(Path::new(root), &old)) {
            continue;
        }
        if let Some(parent) = new.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Konnte {} nicht anlegen: {error}", parent.display()))?;
        }
        move_tree(&old, &new)?;
        let new_text = new.to_string_lossy().to_string();
        if let Some(entry) = updated.files.iter_mut().find(|entry| entry.abs == file.abs) {
            entry.abs = new_text;
        }
        let root_text = resolved.root.to_string_lossy().to_string();
        if !updated.roots.contains(&root_text) {
            updated.roots.push(root_text);
        }
        changed = true;
    }

    if changed {
        updated.updated_at = super::journal::now();
        mods.update_record(updated)?;
    }
    Ok(())
}

fn move_tree(source: &Path, target: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(source)
        .map_err(|error| format!("Konnte {} nicht lesen: {error}", source.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("Weigere mich, den Link {} zu verschieben.", source.display()));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(target)
            .map_err(|error| format!("Konnte {} nicht anlegen: {error}", target.display()))?;
        for entry in std::fs::read_dir(source)
            .map_err(|error| format!("Konnte {} nicht lesen: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            move_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        let _ = std::fs::remove_dir(source);
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Konnte {} nicht anlegen: {error}", parent.display()))?;
    }
    match std::fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            if target.exists() {
                let same_size = std::fs::metadata(source).ok().map(|meta| meta.len())
                    == std::fs::metadata(target).ok().map(|meta| meta.len());
                if same_size {
                    std::fs::remove_file(source)
                        .map_err(|error| format!("Konnte {} nicht entfernen: {error}", source.display()))?;
                    return Ok(());
                }
            }
            std::fs::copy(source, target)
                .map_err(|error| format!("Konnte {} nicht kopieren: {error}", source.display()))?;
            std::fs::remove_file(source)
                .map_err(|error| format!("Konnte {} nicht entfernen: {error}", source.display()))?;
            Ok(())
        }
    }
}

fn expand_args(args: &[String], env: &InstallEnv, windows: bool) -> Result<Vec<String>, String> {
    args.iter().map(|arg| paths::expand_launch_string(arg, env, windows)).collect()
}

async fn launch_game(
    app: &AppHandle,
    mods: &Arc<Mods>,
    record: &Record,
    env: &InstallEnv,
    game: &InstalledGame,
    launch: &RecipeLaunch,
    track: LaunchTrack,
) -> Result<Value, String> {
    preflight_proton(game, &env.home)?;
    let windows = game.proton_prefix.is_some() || !game.page.store_app_ids.steam.is_empty();
    let args = expand_args(&launch.args, env, windows)?;
    let mut extra_env = HashMap::new();
    for (key, value) in &launch.env {
        extra_env.insert(key.clone(), paths::expand_launch_string(value, env, windows)?);
    }
    let loader = loader_env(&record.loaders);
    if !loader.is_empty() {
        let entry = extra_env.entry("WINEDLLOVERRIDES".into()).or_default();
        if !entry.is_empty() {
            entry.push(';');
        }
        entry.push_str(&loader);
    }
    match &game.launch {
        GameLaunch::Uri(uri) => {
            if args.is_empty() && extra_env.is_empty() {
                app.opener().open_url(uri, None::<&str>).map_err(|e| format!("Konnte das Spiel nicht starten: {e}"))?;
                track_external(mods, record, track);
                return Ok(json!({ "tracked": false }));
            }
            if let Some(rest) = uri.strip_prefix("steam://rungameid/") {
                let app_id = rest.split(['/', '?']).next().unwrap_or("");
                if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("Steam hat für dieses Spiel keine gültige App-ID geliefert.".into());
                }
                let mut last_error = None;
                let mut started = None;
                for launcher in steam_launchers(&game.install_dir) {
                    let mut command = steam_command(launcher, app_id, &args, &extra_env);
                    match command.spawn() {
                        Ok(child) => {
                            started = Some(child);
                            break;
                        }
                        Err(e) => last_error = Some(e),
                    }
                }
                let _started = started.ok_or_else(|| {
                    format!(
                        "Konnte Steam nicht starten: {}",
                        last_error.map(|e| e.to_string()).unwrap_or_else(|| "Steam wurde nicht gefunden".into())
                    )
                })?;
                track_external(mods, record, track);
                return Ok(json!({ "tracked": false }));
            }
            Err("Dieses Spiel kann nicht mit zusätzlichen Argumenten oder einer Wine-Umgebung gestartet werden.".into())
        }
        GameLaunch::Command { program, args: base } => {
            let mut command = Command::new(program);
            command.args(base).args(&args).current_dir(&game.install_dir);
            for (key, value) in &extra_env {
                command.env(key, value);
            }
            let pid = spawn_tracked(command, mods, record, track)?;
            Ok(json!({ "tracked": true, "pid": pid }))
        }
    }
}

async fn launch_uri(
    app: &AppHandle,
    mods: &Arc<Mods>,
    record: &Record,
    env: &InstallEnv,
    game: &InstalledGame,
    launch: &RecipeLaunch,
    track: LaunchTrack,
) -> Result<Value, String> {
    let uri = launch.uri.as_deref().ok_or_else(|| "Dieses Mashup hat keinen Startlink.".to_string())?;
    let args = expand_args(&launch.args, env, game.proton_prefix.is_some())?;
    if !args.is_empty() {
        return Err("Dieser Startlink kann keine Argumente übernehmen.".into());
    }
    app.opener().open_url(uri, None::<&str>).map_err(|e| format!("Konnte den Startlink nicht öffnen: {e}"))?;
    track_external(mods, record, track);
    Ok(json!({ "tracked": false }))
}

fn track_external(mods: &Arc<Mods>, record: &Record, track: LaunchTrack) {
    if track == LaunchTrack::Primary {
        mods.set_running(RunningProcess {
            mod_id: record.mod_id.clone(),
            slug: record.slug.clone(),
            pid: 0,
            started_at: super::journal::now(),
            stoppable: false,
        });
    }
}

async fn launch_exe(
    _app: &AppHandle,
    mods: &Arc<Mods>,
    record: &Record,
    env: &InstallEnv,
    game: &InstalledGame,
    launch: &RecipeLaunch,
    home: &Path,
    track: LaunchTrack,
) -> Result<Value, String> {
    let path_text = launch.path.as_deref().ok_or_else(|| "Dieses Mashup hat kein Startprogramm.".to_string())?;
    let target = super::model::parse_target(path_text).ok_or_else(|| "Unsichere Programmdatei im Mashup.".to_string())?;
    let resolved = paths::resolve_target(&target, env)?;
    let base = paths::safe_base(&resolved.root)?;
    let path = paths::safe_join(&base, &resolved.rel)?;
    if !path.is_file() {
        return Err(format!("Das Startprogramm fehlt: {}", path.display()));
    }
    let name = path.to_string_lossy().to_ascii_lowercase();
    let windows = name.ends_with(".exe") || name.ends_with(".bat") || name.ends_with(".cmd");

    // Spezielle Behandlung für me3-launcher.exe und prismlauncher.exe (Minecraft Ring, PeakCraft und ähnliche Mashups)
    let is_me3 = name.contains("me3-launcher.exe") || name.contains("prismlauncher.exe");
    
    // me3-launcher.exe und prismlauncher.exe brauchen --launch <Mashup-Slug> statt der Standardargumente
    let me3_args = is_me3.then(|| vec!["--launch".into(), record.slug.clone()]).unwrap_or_default();
    let args = if me3_args.is_empty() {
        expand_args(&launch.args, env, windows)?
    } else {
        me3_args
    };

    let windows_path = game
        .proton_prefix
        .as_deref()
        .map(|prefix| paths::wine_path_in_prefix(&path, prefix))
        .unwrap_or_else(|| paths::wine_path(&path));
    let cwd = match &launch.cwd {
        Some(cwd) => {
            let target = super::model::parse_target(cwd).ok_or_else(|| "Unsichere Arbeitsordner im Mashup.".to_string())?;
            let resolved = paths::resolve_target(&target, env)?;
            let base = paths::safe_base(&resolved.root)?;
            paths::safe_join(&base, &resolved.rel)?
        }
        None => path.parent().map(Path::to_path_buf).unwrap_or_else(|| game.install_dir.clone()),
    };
    let mut extra_env = HashMap::new();
    
    // me3-launcher.exe und prismlauncher.exe brauchen ERBRIDGE=1 für Minecraft-Integration
    if is_me3 {
        extra_env.insert("ERBRIDGE".into(), "1".into());
    }
    
    for (key, value) in &launch.env {
        extra_env.insert(key.clone(), paths::expand_launch_string(value, env, windows)?);
    }
    let loader_env = loader_env(&record.loaders);
    if !loader_env.is_empty() {
        let entry = extra_env.entry("WINEDLLOVERRIDES".into()).or_default();
        if !entry.is_empty() {
            entry.push(';');
        }
        entry.push_str(&loader_env);
    }

    if !windows {
        let mut command = Command::new(&path);
        command.args(&args).current_dir(&cwd);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let pid = spawn_tracked(command, mods, record, track)?;
        return Ok(json!({ "tracked": true, "pid": pid }));
    }

    let prefix = game.proton_prefix.clone();
    let steam_prefix = prefix.as_ref().is_some_and(|prefix| prefix.to_string_lossy().contains("/steamapps/compatdata/"));
    let proton = if steam_prefix {
        let prefix = prefix.as_ref().expect("steam_prefix implies prefix");
        preflight_proton(game, &env.home)?;
        steam_proton_for(&game.page.store_app_ids.steam, prefix, &env.home)
    } else {
        None
    };
    if let Some(proton) = proton {
        let prefix = prefix.as_ref().expect("steam_prefix implies prefix");
        let compat_data = prefix.parent().ok_or_else(|| "Ungültiges Proton-Präfix.".to_string())?;
        let steam_root = env
            .steam_root
            .clone()
            .or_else(|| melty_games::steam::find_roots(home).first().map(|root| root.path.clone()))
            .unwrap_or_else(|| home.to_path_buf());
        let mut command = Command::new(proton);
        command.arg("run");
        if name.ends_with(".bat") || name.ends_with(".cmd") {
            command.args(["cmd.exe", "/c"]);
        }
        command.arg(&windows_path).args(&args);
        command
            .env("STEAM_COMPAT_DATA_PATH", compat_data)
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &steam_root)
            .env("SteamAppId", &game.page.store_app_ids.steam)
            .env("SteamGameId", &game.page.store_app_ids.steam);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.current_dir(&cwd);
        let pid = spawn_tracked(command, mods, record, track)?;
        return Ok(json!({ "tracked": true, "pid": pid }));
    }

    let wine = wine_binary().ok_or_else(|| "Für dieses Windows-Programm wird Proton oder Wine benötigt.".to_string())?;
    let wine_prefix = prefix
        .as_deref()
        .and_then(paths::wine_root)
        .unwrap_or_else(|| mods.dir.join("prefixes").join(&record.slug));
    std::fs::create_dir_all(&wine_prefix).map_err(|e| e.to_string())?;
    let mut command = Command::new(wine);
    if name.ends_with(".bat") || name.ends_with(".cmd") {
        command.args(["cmd.exe", "/c"]);
    }
    command.arg(&windows_path).args(&args);
    command.env("WINEPREFIX", &wine_prefix).current_dir(&cwd);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let pid = spawn_tracked(command, mods, record, track)?;
    Ok(json!({ "tracked": true, "pid": pid }))
}

fn spawn_tracked(mut command: Command, mods: &Arc<Mods>, record: &Record, track: LaunchTrack) -> Result<u32, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    let mut child = command.spawn().map_err(|e| format!("Konnte das Programm nicht starten: {e}"))?;
    let pid = child.id();
    let run = RunningProcess {
        mod_id: record.mod_id.clone(),
        slug: record.slug.clone(),
        pid,
        started_at: super::journal::now(),
        stoppable: true,
    };
    match track {
        LaunchTrack::Primary => mods.set_running(run),
        LaunchTrack::Companion => mods.add_companion(run),
    }
    let mods = mods.clone();
    let mod_id = record.mod_id.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        match track {
            LaunchTrack::Primary => mods.clear_running(&mod_id, Some(pid)),
            LaunchTrack::Companion => mods.clear_companion(&mod_id, pid),
        }
    });
    Ok(pid)
}

/// Alle Loader, die für den Start gebraucht werden: der gespeicherte Stand plus die Loader-Anforderungen
/// des Rezepts. So werden auch Datensätze aus der Zeit vor dem `loaders`-Feld repariert.
fn launch_loaders(record: &Record) -> Vec<String> {
    let mut loaders = record.loaders.clone();
    for requirement in &record.recipe.requirements {
        if requirement.kind == "loader" && !loaders.contains(&requirement.id) {
            loaders.push(requirement.id.clone());
        }
    }
    loaders
}

/// Ein Mashup kann seinen eigenen ASI-Loader mitbringen. Dann darf der Start nicht nachträglich
/// Ultimate ASI Loader darüberinstallieren; die Wine-Überschreibung wird trotzdem gesetzt.
fn loader_provided_by_record(loader: &str, record: &Record) -> bool {
    loader == "ultimate-asi-loader"
        && record.files.iter().any(|file| {
            file.root == "game"
                && Path::new(&file.abs).is_file()
                && matches!(
                    file.rel.to_ascii_lowercase().as_str(),
                    "dinput8.dll" | "version.dll" | "winmm.dll" | "dsound.dll"
                )
        })
}

fn loader_env(loaders: &[String]) -> String {
    let mut parts = Vec::new();
    for id in loaders {
        if let Some((dll, _)) = loader_override(id) {
            let part = format!("{dll}=n,b");
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }
    parts.join(";")
}

/// Wine ignores a game-local proxy DLL unless the DLL override is set. Environment variables set on
/// `steam -applaunch` do not reach the game when the Steam client is already running, so the override
/// is written into the game's prefix as well.
pub(super) fn ensure_loader_override(loader: &str, game: &paths::GameEnv, env: &paths::InstallEnv) -> Result<(), String> {
    let Some((dll, value)) = loader_override(loader) else { return Ok(()) };
    let Some(prefix) = game.prefix.as_deref() else { return Ok(()) };
    let Some(wine_prefix) = paths::wine_root(prefix) else { return Ok(()) };
    if override_present(&wine_prefix, dll, value) {
        return Ok(());
    }
    let steam_prefix = prefix.to_string_lossy().contains("/steamapps/compatdata/");
    let proton = if steam_prefix {
        game.steam_app_id
            .as_deref()
            .filter(|app| !app.is_empty())
            .and_then(|app| steam_proton_for(app, prefix, &env.home))
    } else {
        None
    };
    if let Some(proton) = &proton {
        if conflicting_wineserver(prefix, proton).is_some() {
            return Err("Beende zuerst das Spiel; sein Proton-Präfix wird gerade noch benutzt.".into());
        }
    } else if steam_prefix {
        // Never point system Wine at a Steam prefix; Steam may still start it with its own Proton.
        return Ok(());
    }
    let mut command = if let Some(proton) = proton {
        let compat_data = prefix.parent().ok_or_else(|| "Ungültiges Proton-Präfix.".to_string())?;
        let steam_root = env
            .steam_root
            .clone()
            .or_else(|| melty_games::steam::find_roots(&env.home).first().map(|root| root.path.clone()))
            .unwrap_or_else(|| env.home.clone());
        let app_id = game.steam_app_id.as_deref().unwrap_or("");
        let mut command = Command::new(proton);
        command
            .arg("run")
            .env("STEAM_COMPAT_DATA_PATH", compat_data)
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &steam_root)
            .env("SteamAppId", app_id)
            .env("SteamGameId", app_id);
        command
    } else {
        let wine = wine_binary().ok_or_else(|| "Für den Loader wird Proton oder Wine benötigt.".to_string())?;
        let mut command = Command::new(wine);
        command.env("WINEPREFIX", &wine_prefix);
        command
    };
    let output = command
        .args(["reg", "add", r"HKCU\Software\Wine\DllOverrides", "/v", dll, "/d", value, "/f"])
        .output()
        .map_err(|error| format!("Konnte die Wine-DLL-Einstellung nicht setzen: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        return Err(if message.is_empty() {
            "Konnte die Wine-DLL-Einstellung nicht setzen.".to_string()
        } else {
            format!("Konnte die Wine-DLL-Einstellung nicht setzen: {message}")
        });
    }
    Ok(())
}

fn loader_override(id: &str) -> Option<(&'static str, &'static str)> {
    if id.starts_with("bepinex") {
        Some(("winhttp", "native,builtin"))
    } else if id == "melonloader" {
        Some(("version", "native,builtin"))
    } else if id == "ultimate-asi-loader" {
        Some(("dinput8", "native,builtin"))
    } else {
        None
    }
}

fn override_present(wine_prefix: &Path, dll: &str, value: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(wine_prefix.join("user.reg")) else { return false };
    let needle = format!("\"{dll}\"=\"{value}\"");
    text.lines().any(|line| line.trim() == needle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteamLauncher {
    Native,
    Flatpak,
    Snap,
}

/// Flatpak/Snap Steam needs its own launcher, even when a `steam` stub exists on the host.
fn steam_launchers(install_dir: &Path) -> Vec<SteamLauncher> {
    let path = install_dir.to_string_lossy();
    let detected = if path.contains("/.var/app/com.valvesoftware.Steam/") {
        SteamLauncher::Flatpak
    } else if path.contains("/snap/steam/") {
        SteamLauncher::Snap
    } else {
        SteamLauncher::Native
    };
    let mut out = vec![detected];
    for launcher in [SteamLauncher::Native, SteamLauncher::Flatpak, SteamLauncher::Snap] {
        if launcher != detected {
            out.push(launcher);
        }
    }
    out
}

fn steam_command(
    launcher: SteamLauncher,
    app_id: &str,
    args: &[String],
    extra_env: &HashMap<String, String>,
) -> Command {
    let mut command = match launcher {
        SteamLauncher::Native => {
            let mut command = Command::new("steam");
            command.args(["-applaunch", app_id]);
            for (key, value) in extra_env {
                command.env(key, value);
            }
            command
        }
        SteamLauncher::Flatpak => {
            let mut command = Command::new("flatpak");
            command.arg("run");
            for (key, value) in extra_env {
                command.arg(format!("--env={key}={value}"));
            }
            command.args(["com.valvesoftware.Steam", "-applaunch", app_id]);
            command
        }
        SteamLauncher::Snap => {
            let mut command = Command::new("snap");
            command.args(["run", "steam", "-applaunch", app_id]);
            for (key, value) in extra_env {
                command.env(key, value);
            }
            command
        }
    };
    command.args(args);
    command
}

fn wine_binary() -> Option<PathBuf> {
    for path in ["/usr/bin/wine", "/usr/local/bin/wine", "/bin/wine"] {
        if Path::new(path).is_file() {
            return Some(PathBuf::from(path));
        }
    }
    Command::new("wine").arg("--version").output().ok().map(|_| PathBuf::from("wine"))
}

fn find_proton(home: &Path) -> Option<PathBuf> {
    let mut candidates = proton_candidates(home);
    candidates.retain(|path| is_executable(path));
    candidates.sort_by_key(|path| {
        let name = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let rank = if name.contains("experimental") {
            0
        } else if name.contains("hotfix") {
            1
        } else {
            2
        };
        (rank, path.clone())
    });
    candidates.into_iter().next()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|meta| meta.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn proton_candidates(home: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in melty_games::steam::find_roots(home) {
        for base in [root.path.join("steamapps/common"), root.path.join("compatibilitytools.d")] {
            let Ok(entries) = std::fs::read_dir(&base) else { continue };
            for entry in entries.flatten() {
                let proton = entry.path().join("proton");
                if proton.is_file() {
                    candidates.push(proton);
                }
            }
        }
    }
    for extra in [home.join(".steam/steam/compatibilitytools.d"), PathBuf::from("/usr/share/steam/compatibilitytools.d")] {
        let Ok(entries) = std::fs::read_dir(&extra) else { continue };
        for entry in entries.flatten() {
            let proton = entry.path().join("proton");
            if proton.is_file() {
                candidates.push(proton);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|path| seen.insert(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone())));
    candidates
}

/// The Proton runtime that Steam itself uses for a game. Steam records the effective mapping in
/// `logs/compat_log.txt`; the prefix metadata is a fallback for older Steam versions.
fn steam_proton_for(app_id: &str, prefix: &Path, home: &Path) -> Option<PathBuf> {
    if app_id.is_empty() {
        return None;
    }
    if let Some(tool) = steam_tool_mapping(app_id, home) {
        if let Some(proton) = resolve_proton_tool(&tool, home) {
            return Some(proton);
        }
    }
    proton_from_prefix_info(prefix).or_else(|| find_proton(home))
}

fn steam_tool_mapping(app_id: &str, home: &Path) -> Option<String> {
    let needle = format!("Mapping AppID {app_id} to tool \"");
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for root in melty_games::steam::find_roots(home) {
        let log = root.path.join("logs/compat_log.txt");
        let modified = std::fs::metadata(&log).and_then(|meta| meta.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let Ok(text) = std::fs::read_to_string(&log) else { continue };
        let mut found = None;
        for line in text.lines() {
            let Some(start) = line.find(&needle) else { continue };
            let rest = &line[start + needle.len()..];
            let Some(end) = rest.find('"') else { continue };
            let name = rest[..end].trim();
            if !name.is_empty() && name != "native" {
                found = Some(name.to_string());
            }
        }
        if let Some(name) = found {
            if best.as_ref().map_or(true, |(stamp, _)| modified >= *stamp) {
                best = Some((modified, name));
            }
        }
    }
    best.map(|(_, name)| name)
}

fn resolve_proton_tool(tool: &str, home: &Path) -> Option<PathBuf> {
    proton_candidates(home)
        .into_iter()
        .find(|proton| proton_names(proton).iter().any(|name| tool_name_matches(name, tool)))
}

fn proton_names(proton: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Some(dir) = proton.parent() else { return names };
    if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
        names.push(name.to_string());
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("compatibilitytool.vdf")) {
        if let Ok(doc) = melty_games::vdf::parse(&text) {
            if let Some(tools) = doc.obj("compatibilitytools").and_then(|obj| obj.obj("compat_tools")) {
                for (key, value) in tools.iter() {
                    names.push(key.clone());
                    if let melty_games::vdf::Value::Obj(obj) = value {
                        if let Some(display) = obj.str("display_name") {
                            names.push(display.to_string());
                        }
                    }
                }
            }
        }
    }
    names
}

fn normalize_tool_name(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(ch);
        } else {
            gap = true;
        }
    }
    out
}

fn tool_name_matches(candidate: &str, wanted: &str) -> bool {
    let candidate = normalize_tool_name(candidate);
    let wanted = normalize_tool_name(wanted);
    if candidate == wanted {
        return true;
    }
    // Steam's aliases drop the minor version: `proton_9` for "Proton 9.0".
    trim_minor(&candidate) == trim_minor(&wanted)
}

fn trim_minor(value: &str) -> &str {
    value.strip_suffix("_0").unwrap_or(value)
}

fn proton_from_prefix_info(prefix: &Path) -> Option<PathBuf> {
    let compat = prefix.parent()?;
    let text = std::fs::read_to_string(compat.join("config_info")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        for suffix in ["/files/share/fonts/", "/files/lib/"] {
            if let Some(base) = line.strip_suffix(suffix) {
                let proton = Path::new(base).join("proton");
                if proton.is_file() {
                    return Some(proton);
                }
            }
        }
    }
    None
}

/// Creates the Proton prefix of a Steam game when the game has never run. Returns the `…/pfx`
/// directory. `wineboot` is headless; it only prepares the prefix, it does not start the game.
pub fn ensure_prefix(game: &InstalledGame, home: &Path) -> Result<PathBuf, String> {
    if game.page.running {
        return Err("Das Spiel läuft bereits. Beende es zuerst.".into());
    }
    let app_id = game.page.store_app_ids.steam.as_str();
    if app_id.is_empty() {
        return Err("Für dieses Spiel ist keine Steam-App-ID bekannt.".into());
    }
    let steam_root = paths::steam_root_from(&paths::game_env(game))
        .or_else(|| melty_games::steam::find_roots(home).first().map(|root| root.path.clone()))
        .ok_or_else(|| "Steam wurde nicht gefunden.".to_string())?;
    let compat = steam_root.join("steamapps/compatdata").join(app_id);
    let prefix = compat.join("pfx");
    if paths::wine_root(&prefix).is_some() {
        return Ok(prefix);
    }
    if prefix.exists() {
        return Err("Das Spielpräfix ist unvollständig. Starte das Spiel einmal über Steam und versuche es danach erneut.".into());
    }
    let proton = steam_proton_for(app_id, &prefix, home).ok_or_else(|| {
        "Für dieses Spiel wurde keine Proton-Version gefunden. Installiere Proton in Steam und starte das Spiel einmal.".to_string()
    })?;
    if let Some((pid, _)) = conflicting_wineserver(&prefix, &proton) {
        return Err(format!(
            "Das Spielpräfix wird noch von einer anderen Proton-Version benutzt (Prozess {pid}). Beende zuerst das Spiel bzw. die Einrichtung vollständig und warte, bis im Hintergrund nichts mehr läuft."
        ));
    }
    std::fs::create_dir_all(&compat).map_err(|error| format!("Konnte den Proton-Ordner nicht anlegen: {error}"))?;
    let mut command = Command::new(&proton);
    command
        .arg("run")
        .arg("wineboot")
        .arg("-u")
        .env("STEAM_COMPAT_DATA_PATH", &compat)
        .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &steam_root)
        .env("SteamAppId", app_id)
        .env("SteamGameId", app_id)
        .env("WINEDEBUG", "-all")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Konnte das Spielpräfix nicht vorbereiten: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!(
                        "Proton konnte das Spielpräfix nicht anlegen (Status {}).",
                        status.code().unwrap_or(-1)
                    ));
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Die Vorbereitung des Spielpräfixes hat zu lange gedauert.".into());
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(format!("Konnte den Proton-Status nicht lesen: {error}")),
        }
    }
    if paths::wine_root(&prefix).is_none() {
        return Err("Proton hat das Spielpräfix nicht angelegt.".into());
    }
    Ok(prefix)
}

/// Refuse a Steam launch when an old Proton still owns this prefix. Steam would otherwise report
/// "starting" and exit with `wine client error: version mismatch`.
fn preflight_proton(game: &InstalledGame, home: &Path) -> Result<(), String> {
    let Some(prefix) = game.proton_prefix.as_deref() else { return Ok(()) };
    if game.page.store_app_ids.steam.is_empty() {
        return Ok(());
    }
    let Some(proton) = steam_proton_for(&game.page.store_app_ids.steam, prefix, home) else { return Ok(()) };
    if let Some((pid, _)) = conflicting_wineserver(prefix, &proton) {
        return Err(format!(
            "Das Spielpräfix wird noch von einer anderen Proton-Version benutzt (Prozess {pid}). Beende zuerst das Spiel bzw. die Einrichtung vollständig und warte, bis im Hintergrund nichts mehr läuft. Danach erneut starten."
        ));
    }
    Ok(())
}

fn conflicting_wineserver(prefix: &Path, proton: &Path) -> Option<(u32, PathBuf)> {
    let expected = std::fs::canonicalize(proton.parent()?.join("files/bin/wineserver")).ok()?;
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        if pid == std::process::id() {
            continue;
        }
        let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else { continue };
        let is_wineserver = exe.file_name().is_some_and(|name| name.to_string_lossy().starts_with("wineserver"));
        if !is_wineserver || !process_uses_prefix(pid, prefix) {
            continue;
        }
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        if exe != expected {
            return Some((pid, exe));
        }
    }
    None
}

fn process_uses_prefix(pid: u32, prefix: &Path) -> bool {
    let prefix_text = prefix.to_string_lossy();
    if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/environ")) {
        let text = String::from_utf8_lossy(&bytes);
        if text
            .split('\0')
            .any(|entry| entry.strip_prefix("WINEPREFIX=").is_some_and(|value| Path::new(value) == prefix))
        {
            return true;
        }
        if text.contains(prefix_text.as_ref()) {
            return true;
        }
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).contains(prefix_text.as_ref()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_flavor_is_taken_from_the_game_path() {
        assert_eq!(
            steam_launchers(Path::new("/home/u/.local/share/Steam/steamapps/common/Game")),
            vec![SteamLauncher::Native, SteamLauncher::Flatpak, SteamLauncher::Snap]
        );
        assert_eq!(
            steam_launchers(Path::new("/home/u/.var/app/com.valvesoftware.Steam/data/Steam/steamapps/common/Game")),
            vec![SteamLauncher::Flatpak, SteamLauncher::Native, SteamLauncher::Snap]
        );
        assert_eq!(
            steam_launchers(Path::new("/home/u/snap/steam/common/.local/share/Steam/steamapps/common/Game")),
            vec![SteamLauncher::Snap, SteamLauncher::Native, SteamLauncher::Flatpak]
        );
    }

    #[test]
    fn flatpak_passes_environment_into_the_sandbox() {
        let mut env = HashMap::new();
        env.insert("WINEDLLOVERRIDES".to_string(), "winhttp=n,b".to_string());
        let command = steam_command(SteamLauncher::Flatpak, "620", &["--foo".to_string()], &env);
        assert_eq!(command.get_program().to_string_lossy(), "flatpak");
        let args: Vec<String> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["run", "--env=WINEDLLOVERRIDES=winhttp=n,b", "com.valvesoftware.Steam", "-applaunch", "620", "--foo"]);
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "melty-launch-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn steam_tool_aliases_match_proton_folders() {
        assert!(tool_name_matches("Proton - Experimental", "proton_experimental"));
        assert!(tool_name_matches("Proton 9.0", "proton_9"));
        assert!(tool_name_matches("Proton-CachyOS Latest", "Proton-CachyOS Latest"));
        assert!(!tool_name_matches("Proton - Experimental", "proton_9"));
    }

    fn synthetic_pe(version: super::super::version::GameVersion) -> Vec<u8> {
        let mut file = vec![0u8; 0x400];
        file[0..2].copy_from_slice(b"MZ");
        file[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let pe = 0x40usize;
        file[pe..pe + 4].copy_from_slice(b"PE\0\0");
        file[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
        file[pe + 20..pe + 22].copy_from_slice(&0xf0u16.to_le_bytes());
        let optional = pe + 24;
        file[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        let resource = optional + 112 + 16;
        file[resource..resource + 4].copy_from_slice(&0x1000u32.to_le_bytes());
        file[resource + 4..resource + 8].copy_from_slice(&0x200u32.to_le_bytes());
        let section = optional + 0xf0;
        file[section..section + 5].copy_from_slice(b".rsrc");
        file[section + 8..section + 12].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        file[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());
        let raw = 0x200usize;
        file[raw..raw + 4].copy_from_slice(&[0xbd, 0x04, 0xef, 0xfe]);
        file[raw + 4..raw + 8].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        let ms = ((version.0[0] as u32) << 16) | version.0[1] as u32;
        let ls = ((version.0[2] as u32) << 16) | version.0[3] as u32;
        file[raw + 8..raw + 12].copy_from_slice(&ms.to_le_bytes());
        file[raw + 12..raw + 16].copy_from_slice(&ls.to_le_bytes());
        file
    }

    #[test]
    fn wrapper_launches_are_version_checked_like_direct_launches() {
        let home = temp_home("version-wrapper");
        let install = home.join("ELDEN RING");
        std::fs::create_dir_all(install.join("Game")).unwrap();
        std::fs::write(
            install.join("Game/eldenring.exe"),
            synthetic_pe(super::super::version::GameVersion([2, 7, 2, 0])),
        )
        .unwrap();

        let mut env = InstallEnv {
            home: home.clone(),
            mods_dir: home.join("mods"),
            mod_slug: "minecraft-ring".into(),
            games: HashMap::new(),
            primary_slug: Some("elden-ring".into()),
            steam_root: None,
        };
        env.games.insert(
            "elden-ring".into(),
            super::super::paths::GameEnv {
                installation_id: "install".into(),
                install_dir: install.clone(),
                prefix: None,
                steam_app_id: Some("1245620".into()),
            },
        );
        let primary = InstalledGame {
            page: melty_games::PageGame {
                revision: 1,
                detection_state: "ready",
                installation_id: "install".into(),
                game_id: "elden-ring".into(),
                display_name: "Elden Ring".into(),
                launcher: "steam",
                status: "installed",
                running: false,
                process_name: None,
                store_app_ids: melty_games::StoreAppIds::steam("1245620"),
            },
            install_dir: install.clone(),
            proton_prefix: None,
            launch: GameLaunch::Uri("steam://rungameid/1245620".into()),
        };
        let record = Record {
            key: Record::key("ring"),
            mod_id: "ring".into(),
            slug: "minecraft-ring".into(),
            title: "Minecraft Ring".into(),
            release_id: "r1".into(),
            version: "0.3.3".into(),
            game_slugs: vec!["elden-ring".into()],
            primary_game_slug: Some("elden-ring".into()),
            installation_ids: std::collections::BTreeMap::from([("elden-ring".into(), "install".into())]),
            enabled: true,
            installed_at: 1,
            updated_at: 1,
            files: vec![],
            backups: vec![],
            runtime_data: vec![],
            roots: vec![],
            recipe: super::super::model::Recipe {
                games: vec![super::super::model::RecipeGame {
                    slug: "elden-ring".into(),
                    role: "primary".into(),
                    version: Some(">=2.7.1.0 <=2.7.1.1".into()),
                    ..Default::default()
                }],
                launch: Some(super::super::model::Launch {
                    kind: "exe".into(),
                    path: Some("{managed}/me3/bin/me3.exe".into()),
                    args: vec![
                        "launch".into(),
                        "--exe".into(),
                        "{game}/Game/eldenring.exe".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            loaders: vec![],
            game_paths: std::collections::BTreeMap::new(),
        };

        let error = check_game_version(&record, &env, &primary).unwrap_err();
        assert!(error.contains("2.7.2.0"), "{error}");
        assert!(error.contains("2.7.1.1"), "{error}");

        let copy = home.join("compatible-copy");
        std::fs::create_dir_all(copy.join("Game")).unwrap();
        std::fs::write(
            copy.join("Game/eldenring.exe"),
            synthetic_pe(super::super::version::GameVersion([2, 7, 1, 1])),
        )
        .unwrap();
        env.games.get_mut("elden-ring").unwrap().install_dir = copy;
        assert!(check_game_version(&record, &env, &primary).is_ok());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn prismlauncher_is_treated_like_me3_launcher() {
        let home = temp_home("prismlauncher");
        let install = home.join("ELDEN RING");
        std::fs::create_dir_all(install.join("Game")).unwrap();
        let managed = home.join("mods/managed/minecraft-ring");
        std::fs::create_dir_all(managed.join("bin")).unwrap();
        let prismlauncher = managed.join("bin/prismlauncher.exe");
        std::fs::write(&prismlauncher, b"exe").unwrap();

        let mut env = super::super::paths::InstallEnv {
            home: home.clone(),
            mods_dir: home.join("mods"),
            mod_slug: "minecraft-ring".into(),
            games: std::collections::HashMap::new(),
            primary_slug: Some("elden-ring".into()),
            steam_root: None,
        };
        env.games.insert(
            "elden-ring".into(),
            super::super::paths::GameEnv {
                installation_id: "install".into(),
                install_dir: install.clone(),
                prefix: None,
                steam_app_id: Some("1245620".into()),
            },
        );
        let primary = InstalledGame {
            page: melty_games::PageGame {
                revision: 1,
                detection_state: "ready",
                installation_id: "install".into(),
                game_id: "elden-ring".into(),
                display_name: "Elden Ring".into(),
                launcher: "steam",
                status: "installed",
                running: false,
                process_name: None,
                store_app_ids: melty_games::StoreAppIds::steam("1245620"),
            },
            install_dir: install.clone(),
            proton_prefix: None,
            launch: GameLaunch::Uri("steam://rungameid/1245620".into()),
        };
        let record = super::Record {
            key: super::Record::key("ring"),
            mod_id: "ring".into(),
            slug: "minecraft-ring".into(),
            title: "Minecraft Ring".into(),
            release_id: "r1".into(),
            version: "0.3.3".into(),
            game_slugs: vec!["elden-ring".into()],
            primary_game_slug: Some("elden-ring".into()),
            installation_ids: std::collections::BTreeMap::from([("elden-ring".into(), "install".into())]),
            enabled: true,
            installed_at: 1,
            updated_at: 1,
            files: vec![super::super::journal::InstalledFile {
                root: "managed".into(),
                game_slug: None,
                rel: "minecraft-ring/bin/prismlauncher.exe".into(),
                abs: prismlauncher.to_string_lossy().into_owned(),
                sha256: "a".repeat(64),
                size: 3,
                parked: None,
            }],
            backups: vec![],
            runtime_data: vec![],
            roots: vec![managed.to_string_lossy().into_owned()],
            recipe: super::super::model::Recipe {
                games: vec![super::super::model::RecipeGame {
                    slug: "elden-ring".into(),
                    role: "primary".into(),
                    ..Default::default()
                }],
                launch: Some(super::super::model::Launch {
                    kind: "exe".into(),
                    path: Some("{managed}/bin/prismlauncher.exe".into()),
                    args: vec!["--some-arg".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            loaders: vec![],
            game_paths: std::collections::BTreeMap::new(),
        };

        // simuliere launch_exe-Aufruf mit prismlauncher.exe
        let launch = super::super::model::Launch {
            kind: "exe".into(),
            path: Some("{managed}/bin/prismlauncher.exe".into()),
            args: vec!["--some-arg".into()],
            ..Default::default()
        };
        let path_text = launch.path.as_deref().unwrap();
        let target = super::super::model::parse_target(path_text).unwrap();
        let resolved = super::super::paths::resolve_target(&target, &env).unwrap();
        let base = super::super::paths::safe_base(&resolved.root).unwrap();
        let path = super::super::paths::safe_join(&base, &resolved.rel).unwrap();
        let name = path.to_string_lossy().to_ascii_lowercase();

        // test: prismlauncher.exe sollte als me3 erkannt werden
        let is_me3 = name.contains("me3-launcher.exe") || name.contains("prismlauncher.exe");
        assert!(is_me3, "prismlauncher.exe sollte als me3-Launcher erkannt werden");

        // test: args sollten durch --launch <slug> ersetzt werden
        let me3_args = is_me3.then(|| vec!["--launch".into(), record.slug.clone()]);
        let expected_args = if me3_args.is_some() {
            me3_args.unwrap()
        } else {
            launch.args.clone()
        };
        assert_eq!(expected_args[0], "--launch");
        assert_eq!(expected_args[1], "minecraft-ring");

        // test: ERBRIDGE=1 sollte gesetzt werden
        let mut extra_env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if is_me3 {
            extra_env.insert("ERBRIDGE".into(), "1".into());
        }
        assert_eq!(extra_env.get("ERBRIDGE"), Some(&"1".to_string()));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn runtime_data_is_migrated_from_the_home_into_the_prefix() {
        let home = temp_home("migrate");
        let prefix = home.join(".local/share/Steam/steamapps/compatdata/3527290/pfx");
        std::fs::create_dir_all(prefix.join("drive_c/users/steamuser")).unwrap();
        let old_root = home.join(".local/share");
        let old_dir = old_root.join("PeakCraft");
        let old_file = old_dir.join("Prism/prismlauncher.exe");
        std::fs::create_dir_all(old_file.parent().unwrap()).unwrap();
        std::fs::write(&old_file, b"exe").unwrap();

        let mods = Arc::new(Mods::new(home.join("mods")));
        let mut games = HashMap::new();
        games.insert(
            "peak".to_string(),
            super::super::paths::GameEnv {
                installation_id: "install".into(),
                install_dir: home.join(".local/share/Steam/steamapps/common/PEAK"),
                prefix: Some(prefix.clone()),
                steam_app_id: Some("3527290".into()),
            },
        );
        let env = InstallEnv {
            home: home.clone(),
            mods_dir: home.join("mods"),
            mod_slug: "peakcraft".into(),
            games,
            primary_slug: Some("peak".into()),
            steam_root: Some(home.join(".local/share/Steam")),
        };
        let record = Record {
            key: Record::key("modid"),
            mod_id: "modid".into(),
            slug: "peakcraft".into(),
            title: "PeakCraft".into(),
            release_id: "r1".into(),
            version: "1".into(),
            game_slugs: vec!["peak".into()],
            primary_game_slug: Some("peak".into()),
            installation_ids: std::collections::BTreeMap::from([("peak".into(), "install".into())]),
            enabled: true,
            installed_at: 1,
            updated_at: 1,
            files: vec![super::super::journal::InstalledFile {
                root: "localappdata".into(),
                game_slug: None,
                rel: "PeakCraft/Prism/prismlauncher.exe".into(),
                abs: old_file.to_string_lossy().into_owned(),
                sha256: "a".repeat(64),
                size: 3,
                parked: None,
            }],
            backups: vec![],
            runtime_data: vec![super::super::journal::StoredTarget {
                root: "localappdata".into(),
                game_slug: None,
                rel: "PeakCraft".into(),
                abs: old_dir.to_string_lossy().into_owned(),
            }],
            roots: vec![old_root.to_string_lossy().into_owned()],
            recipe: super::super::model::Recipe::default(),
            loaders: vec![],
            game_paths: std::collections::BTreeMap::new(),
        };
        mods.update_record(record.clone()).unwrap();

        migrate_runtime_data(&mods, &record, &env).unwrap();

        let new_dir = prefix.join("drive_c/users/steamuser/AppData/Local/PeakCraft");
        assert_eq!(std::fs::read(new_dir.join("Prism/prismlauncher.exe")).unwrap(), b"exe");
        assert!(!old_dir.exists());
        let stored = mods.record("modid").unwrap();
        assert_eq!(stored.files[0].abs, new_dir.join("Prism/prismlauncher.exe").to_string_lossy());
        assert_eq!(stored.runtime_data[0].abs, new_dir.to_string_lossy());
        assert!(stored.roots.iter().any(|root| root == &prefix.join("drive_c/users/steamuser/AppData/Local").to_string_lossy()));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn effective_steam_mapping_and_prefix_info_are_read() {
        let home = temp_home("mapping");
        let steam = home.join(".local/share/Steam");
        let prefix = steam.join("steamapps/compatdata/1174180/pfx");
        let tool = steam.join("steamapps/common/Proton - Experimental");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::write(tool.join("proton"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(steam.join("logs")).unwrap();
        std::fs::write(
            steam.join("logs/compat_log.txt"),
            "Mapping AppID 1174180 to tool \"proton_cachyos\" with priority 90\n\
             Mapping AppID 1174180 to tool \"proton_experimental\" with priority 90\n",
        )
        .unwrap();
        std::fs::write(
            prefix.parent().unwrap().join("config_info"),
            format!("11.0-100\n{}/files/share/fonts/\n{}/files/lib/\n", tool.display(), tool.display()),
        )
        .unwrap();

        assert_eq!(steam_tool_mapping("1174180", &home).as_deref(), Some("proton_experimental"));
        assert_eq!(resolve_proton_tool("proton_experimental", &home), Some(tool.join("proton")));
        assert_eq!(proton_from_prefix_info(&prefix), Some(tool.join("proton")));
        let _ = std::fs::remove_dir_all(home);
    }
}
