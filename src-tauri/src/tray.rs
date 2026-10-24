//! Tray-Symbol: "Open Melty" (Panel), "Quit Melty".

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

pub fn create(app: &AppHandle) -> bool {
    let build = || -> tauri::Result<()> {
        let open = MenuItem::with_id(app, "open", "Open Melty", true, None::<&str>)?;
        let panel = MenuItem::with_id(app, "panel", "Library & Settings", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit Melty", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&open, &panel, &PredefinedMenuItem::separator(app)?, &quit])?;
        let mut builder = TrayIconBuilder::with_id("main").tooltip(crate::config::TITLE).menu(&menu).show_menu_on_left_click(false);
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        builder
            .on_menu_event(|app, event| match event.id().as_ref() {
                "open" => crate::show_main(app),
                "panel" => crate::panel::open(app, "games"),
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    crate::show_main(tray.app_handle());
                }
            })
            .build(app)?;
        Ok(())
    };
    match build() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[tray] nicht verfügbar: {e}");
            false
        }
    }
}
