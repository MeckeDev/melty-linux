# melty-games – Phase 1 (Game detection)

Replaces `windows-inspector.exe` for Linux and provides exactly the data form of `meltyDesktop.listInstalledGames()`.
Self-contained library without Tauri dependency (only `serde`, `serde_json`, `sha2`), 14 tests.

```
cargo test                      # Unit tests with simulated Steam trees and fake /proc
cargo run --example crosscheck  # checks slug_for/installation_id against values from real JS formula
cargo run -- page               # JSON like games:list
cargo run -- scan               # Steam details: roots, libraries, Proton prefixes, problems
cargo run -- procs              # Process list, Wine/Proton processes with Windows names
```

## What is covered

| Inspector method | Linux replacement | Status |
|---|---|---|
| `processes` | `procs::scan` (/proc, Windows name from Wine command line) | done, tested only with fake /proc |
| `commands` | `procs::commands` (form `[{ProcessId, CommandLine}]`) | done |
| `steamRoots` | `steam::find_roots` (native, Flatpak, Snap, symlink deduplication) | done |
| `steamFolders` | `steam::libraries` (new + old `libraryfolders.vdf`) | done |
| `modules` | – (`legacyModules`, form not yet read) | open |
| `gamepads` | – (idle time; evdev/`/dev/input`) | open |
| `uninstall`, `rockstar` | – (Windows registry; on Linux only via Wine prefix `*.reg`) | open, later |

Additionally: Proton/Steam Linux Runtime/Redistributables are filtered out (appear as manifests on Linux),
unmounted libraries result in `detectionState: "unavailable"` instead of "no games", manifest without folder doesn't count
as installed.

## Integration into the Tauri app

In `src-tauri/Cargo.toml` (adjust path to your layout):
```toml
melty-games = { path = "../crates/melty-games" }
```
```rust
#[tauri::command]
async fn games_list(rev: tauri::State<'_, std::sync::atomic::AtomicU64>) -> Result<Vec<melty_games::PageGame>, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("HOME missing")?;
    let r = rev.load(std::sync::atomic::Ordering::Relaxed);
    tauri::async_runtime::spawn_blocking(move || melty_games::page_games(&home, std::path::Path::new("/proc"), r))
        .await.map_err(|e| e.to_string())
}
```
In the bridge (initialization script): `listInstalledGames: () => invoke("games_list")`,
`refreshInstalledGames` similarly (increment revision), `onInstalledGamesChanged` as event listener on `games:changed`.
For `games:changed` watch the `steamapps/` folders of each library with the `notify` crate (relevant are
`libraryfolders.vdf` and `appmanifest_*.acf`), debounce 1 s, then rescan and send event.

## Assumptions that can only be verified against a real system

1. **Wine command line:** I assume that `/proc/<pid>/cmdline` of Proton games contains the Windows command line
   (`Z:\home\…\game.exe`). Verified only against simulated data. Please run `cargo run -- procs` with a running Proton game
   and check/submit the line of the game.
2. **Page form:** Fields and `installationId` formula come from the original code (byte-identical against the JS formula verified);
   whether the page expects additional fields (`compatibility`) for Linux games is only shown by the live test.
3. **Only Steam:** Epic/GOG/Heroic/Lutris/Faugus and Prism instances are still missing. The catalog match (`gameId` → catalog slug via
   `game-aliases.js`) as well: `gameId` here is only `slug_for(name)`, as the raw version in the original.
4. Only installed games are given to the page; the original also internally uses `known_missing_files`.
