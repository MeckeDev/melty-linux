// window.meltyDesktop für melty.gg (Nachbildung von src/preload/preload.cjs der Windows-App, Bridge-Version 1.0.132).
// Läuft vor dem Seitencode, ausschließlich auf der Origin von melty.gg. Jede Methode ruft einen Rust-Befehl, den
// capabilities/default.json für diese Origin freigibt; Rust prüft die Origin zusätzlich bei jedem Aufruf.
(() => {
  "use strict";
  const ORIGIN = "__MELTY_ORIGIN__";
  if (location.origin !== ORIGIN) return;
  const internals = window.__TAURI_INTERNALS__;
  if (!internals || window.meltyDesktop) return;

  const call = (command, args) => internals.invoke(command, args || {});

  // Ereignisse (games:changed, …): Rust ruft window.__meltyDispatch(kanal, nutzlast) auf.
  const subscribers = new Map();
  const listen = (channel, callback) => {
    if (typeof callback !== "function") return () => {};
    let set = subscribers.get(channel);
    if (!set) subscribers.set(channel, (set = new Set()));
    set.add(callback);
    return () => set.delete(callback);
  };
  Object.defineProperty(window, "__meltyDispatch", {
    value: (channel, payload) => {
      for (const callback of [...(subscribers.get(channel) || [])]) {
        try { callback(payload); } catch (error) { console.error(error); }
      }
    },
  });

  const api = {
    isDesktop: true,
    platform: "linux",
    appVersion: "__MELTY_VERSION__",
    usesWindowsTitleBarOverlay: false,
    openLogin: (href) => call("auth_open_login", { href }),
    listInstalledGames: () => call("games_list"),
    refreshInstalledGames: () => call("games_refresh"),
    onInstalledGamesChanged: (callback) => listen("games:changed", callback),
    getStartupBehavior: () => call("settings_get_startup"),
    setStartupBehavior: (behavior) => call("settings_set_startup", { behavior }),
    getDesktopNotificationsEnabled: () => call("settings_get_notifications"),
    setDesktopNotificationsEnabled: (enabled) => call("settings_set_notifications", { enabled: Boolean(enabled) }),
    // Updates kommen über den Paketweg, nicht über die App.
    getUpdateState: () => call("updater_state"),
    checkForUpdates: () => call("updater_check"),
    managesUpdateToast: true,
    onUpdateEvent: (callback) => listen("updater:event", callback),
    downloadUpdate: () => call("updater_download"),
    installUpdate: () => call("updater_install"),
    ackUpdateToast: () => Promise.resolve(),
    setModalChrome: () => Promise.resolve(),
    minimizeWindow: () => call("window_minimize"),
    toggleMaximizeWindow: () => call("window_toggle_maximize"),
    closeWindow: () => call("window_close"),
    // Bewusst nicht vorhanden (noch nicht umgesetzt): agents, agentRuns, mods. Die Seite erkennt Fähigkeiten an
    // `<bereich>.version`; fehlt der Bereich, bietet sie die Funktion nicht an.
  };
  Object.defineProperty(window, "meltyDesktop", { value: Object.freeze(api), enumerable: true });

  // ---- Seite ins Fenster einpassen (wie die Windows-App) ----
  const addStyle = (id, css) => {
    const add = () => {
      if (!document.head || document.getElementById(id)) return;
      const style = document.createElement("style");
      style.id = id;
      style.textContent = css;
      document.head.append(style);
    };
    if (document.readyState === "loading") window.addEventListener("DOMContentLoaded", add, { once: true });
    else add();
  };
  // Das Fenster hat die normale Titelleiste des Desktops: die eigenen Knöpfe der Seite entfallen.
  addStyle("melty-titlebar-style", ".desktop-window-controls { display: none !important; }");
  // Seitenleiste und Kopf bleiben stehen, nur der Inhalt scrollt.
  addStyle(
    "melty-shell-scroll-style",
    `
    html.is-desktop:has(.app-shell-layout) { height: 100% !important; overflow: hidden !important; scrollbar-gutter: auto !important; }
    html.is-desktop:has(.app-shell-layout) body { height: 100vh !important; overflow: hidden !important; }
    html.is-desktop:has(.app-shell-layout) .app-frame { height: 100vh !important; min-height: 100vh !important; overflow: hidden !important; }
    html.is-desktop:has(.app-shell-layout) .topbar { position: relative !important; }
    html.is-desktop:has(.app-shell-layout) .app-shell-layout { height: calc(100vh - 52px) !important; min-height: 0 !important; overflow: hidden !important; }
    html.is-desktop:has(.app-shell-layout) .app-shell-content { min-height: 0 !important; overflow-x: hidden !important; overflow-y: auto !important; }
    html.is-desktop:has(.app-shell-layout) .app-sidebar { position: relative !important; top: auto !important; height: 100% !important; }
  `
  );
})();
