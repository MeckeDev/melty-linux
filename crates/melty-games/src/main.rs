//! CLI zum Ausprobieren und für Fehlerberichte:
//!   melty-games page   – JSON wie `meltyDesktop.listInstalledGames()`
//!   melty-games scan   – Steam-Details (Wurzeln, Bibliotheken, Proton-Prefixe, Probleme)
//!   melty-games procs  – Prozessliste (Wine/Proton-Prozesse mit Windows-Namen)
use std::path::PathBuf;

fn main() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/root"));
    let proc_root = PathBuf::from(std::env::var_os("MELTY_PROC_ROOT").unwrap_or_else(|| "/proc".into()));
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "page".into());
    match cmd.as_str() {
        "page" => println!("{}", serde_json::to_string_pretty(&melty_games::page_games(&home, &proc_root, 1)).unwrap()),
        "procs" => println!("{}", serde_json::to_string_pretty(&melty_games::procs::scan(&proc_root)).unwrap()),
        "scan" => {
            let inv = melty_games::inventory(&home, &proc_root, 1);
            let steam = melty_games::steam::scan(&home);
            let v = serde_json::json!({
                "detectionState": inv.detection_state,
                "roots": steam.roots.iter().map(|r| serde_json::json!({"path": r.path, "flavor": r.flavor})).collect::<Vec<_>>(),
                "libraries": steam.libraries,
                "skippedTools": steam.skipped_tools,
                "problems": inv.problems,
                "games": inv.games.iter().map(|g| serde_json::json!({
                    "name": g.page.display_name, "launcher": g.page.launcher,
                    "appIds": g.page.store_app_ids, "installDir": g.install_dir,
                    "protonPrefix": g.proton_prefix, "launch": g.launch.label(), "running": g.page.running,
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        _ => {
            eprintln!("Nutzung: melty-games [page|scan|procs]");
            std::process::exit(2);
        }
    }
}
