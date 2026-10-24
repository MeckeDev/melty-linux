fn main() {
    // Nur diese Befehle darf das Panel aufrufen (siehe capabilities/panel.json): kein Dateizugriff,
    // kein Shell, keine Core-Rechte. Die melty.gg-Seite erhält keine Capability mehr.
    const COMMANDS: &[&str] = &[
        "panel_state",
        "panel_set",
        "panel_launch",
        "panel_reveal",
        "panel_open_site",
        "panel_restart",
        "mashups_search",
        "mashups_refresh",
        "mashups_detail",
        "mashups_open",
        "account_status",
        "account_login",
        "account_logout",
        "mods_state",
        "mods_library",
        "mods_install",
        "mods_uninstall",
        "mods_set_enabled",
        "mods_set_game_copy",
        "mods_pick_game_folder",
        "mods_launch",
        "mods_stop",
        "mods_open_folder",
        "mods_clear_jobs",
    ];
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)))
        .expect("tauri-build");
}
