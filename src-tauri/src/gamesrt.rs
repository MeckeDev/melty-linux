//! Laufzeit-Anbindung der Spiele-Erkennung: Pfade, Liste, Beobachter für `games:changed`.

use crate::AppState;
use melty_games::PageGame;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub fn home() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// `/proc`, überschreibbar für Tests.
pub fn proc_root() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from("C:\\")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("MELTY_PROC_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/proc"))
    }
}

pub fn list(revision: u64) -> Vec<PageGame> {
    melty_games::page_games(&home(), &proc_root(), revision)
}

fn relevant(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "libraryfolders.vdf" || (n.starts_with("appmanifest_") && n.ends_with(".acf")))
}

/// Beobachtet die Steam-Bibliotheken (Datei-Ereignisse) und fragt alle 4 s den Laufzustand ab; meldet Änderungen an die Seite.
pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = {
            let tx = tx.clone();
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if ev.paths.iter().any(|p| relevant(p)) {
                        let _ = tx.send(());
                    }
                }
            })
            .ok()
        };
        let mut watched: HashSet<PathBuf> = HashSet::new();
        let mut last = list(0);
        loop {
            if let Some(w) = watcher.as_mut() {
                use notify::Watcher;
                for lib in melty_games::steam::scan(&home()).libraries {
                    let dir = lib.join("steamapps");
                    if watched.insert(dir.clone()) && w.watch(&dir, notify::RecursiveMode::NonRecursive).is_err() {
                        watched.remove(&dir);
                    }
                }
            }
            match rx.recv_timeout(Duration::from_secs(4)) {
                Ok(()) => while rx.recv_timeout(Duration::from_millis(800)).is_ok() {}, // entprellen
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let now = list(0);
            if now != last {
                last = now;
                // Das Panel fragt den Stand regelmäßig selbst ab; hier nur die Revision fortschreiben.
                let state = app.state::<AppState>();
                state.revision.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });
}
