# ANALYSIS.md – Melty Desktop (capture) v1.0.132

Source: `app.asar` + `app.asar.unpacked` files of the Windows installation. Only JavaScript was analyzed
(`dist/main.bundle.mjs`, `src/preload/preload.cjs`, Worker). The Windows binaries (`windows-inspector.exe`,
`window-tuck.exe`, `elevate.exe`, `node_datachannel.node`) were **not** disassembled; their function was deduced
from the JS counterpart (see §7a).

## 1. Core Finding

Melty Desktop is a **thin native wrapper around the website `https://melty.gg`**. The UI is not included in the package,
it is loaded remotely (`config.js`: `PRODUCTION_URL = "https://melty.gg"`; overrides via `MELTY_API_URL`,
`MELTY_WEB_URL`, `MELTY_DEV_DESKTOP=1`). The wrapper provides a bridge `window.meltyDesktop` to the page via preload,
which the page uses to trigger local things: find games, install/start mods, execute coding agents
(Claude Code, Codex) locally, overlay over the game.

Consequence for a Linux client: **The UI does not need to be reproduced.** What needs to be reproduced is the bridge
(98 IPC channels, see `bridge-channels.json`) and the native layer behind it.

## 2. Scope

| Part | Lines |
|---|---|
| Bundle total | 103 018 (4.3 MB, unminified, esbuild) |
| of which app-specific code | 53 602 in 202 modules (rest: node_modules, Sentry) |
| `capture/main/mods` | 24 389 (largest module `mod-manager.js`: 7 037) |
| `lib/mods` (shared kit: game catalog, loader, recipes) | 9 860 |
| `capture/main/agents` | 8 429 |
| `capture/main/games` | 6 620 |
| `capture/main` (window, auth, config, cleanup, Sentry connection) | 3 643 |
| `capture/main/discord` | 324 |

`split-bundle.mjs` decomposes the bundle into these modules (`INDEX.json` + one file per module), so that an agent can
work module by module with a small context window.

## 3. Login (everything in `auth.js`, approx. 280 lines)

1. The page calls `meltyDesktop.openLogin(href)`. Matches `href` on `/auth/google?desktop=1` or on a
   Supabase-`/auth/…`-URL with `redirect_to=/auth/callback?desktop=1`, appends `desktop_handoff` (18 random bytes)
   and `desktop_handoff_secret` (36 random bytes, both base64url).
2. Opens the URL in the **system browser** (`shell.openExternal`); the password is never seen by the app.
3. Polls `GET /auth/desktop-handoff?desktop_handoff=…&desktop_handoff_secret=…` every 900 ms, max 10 min,
   until `{status:"ready", fragment}` comes.
4. Loads `/auth/desktop-import#<fragment>` in the app window; there the cookie session is created.
   Alternatively, the deep link `melty://auth/success?…` provides the same fragment.
5. For local API calls, a **device token** is obtained: `POST /api/desktop/device-token`
   `{deviceName:"Melty desktop"}` with the session cookie. 401 = logged out. Token format
   `^[a-z]{2,8}_[A-Za-z0-9_-]{16,}$`, stored as `melty-token.bin` in `userData`
   (Electron `safeStorage`, otherwise plaintext).
6. Logout: navigation to `/auth/logout` deletes the token file.

This is a standard OAuth handoff and can be ported 1:1. Open: whether the server identifies the client in any way
(User-Agent, headers). I didn't find any hints for this in the client code; to be checked in phase 0.

## 4. What the Bridge Covers (Channels per Namespace)

`mods:*` 48, `agent-runs:*` 31, `updater:*` 5, `settings:*` 4, `games:*` 3, `window:*` 3, `agents:*` 3, `auth:*` 1.
Complete list with method names: `bridge-channels.json`. The method signatures are in `preload.cjs`.

The preload restricts file paths to those the user has chosen/dropped themselves (`filesGiven`), and only accepts
agent briefs if they are signed by melty.gg (`checksBriefs`). The main process rejects calls from other origins than
melty.gg.

## 5. Modules in Overview

- **games/** – detection of installed games. Steam (`libraryfolders.vdf`, `appmanifest_*.acf`), Epic
  (`launcherinstalled.dat`, `.item`), GOG (`product.db`), Prism/MultiMC (`mmc-pack.json`, `instance.cfg`),
  Minecraft (`launcher_profiles.json`). File watcher in `store-record-worker.cjs`, Steam install progress in
  `steam-progress-worker.cjs`. Process detection via `windows-inspector.exe` (JSON-lines over stdio:
  `{version:1, generation, id, method, params}`, ping every 5 s) plus PowerShell/`reg.exe` fallbacks.
- **mods/** – centerpiece. Installation ticket from server, download, archive unpacking (7-Zip, unrar, zip),
  loader installation, journal (rollback), start, multiplayer tunnel, Minecraft login, PC uploads, test control.
- **lib/mods/kit** – shared catalog: approx. 250 game entries (`games.ts`), loader definitions, pins
  (version, URL, SHA-256), recipes.
- **agents/** – executes the local CLIs (adapters for Claude Code, Codex; `opencode` is referenced),
  keeps chats locally, shows the overlay (`/overlay` page in a small panel over the game, Alt+M).
- **discord/** – Rich Presence via Discord-IPC.
- **main/** – window, tray, deep links `melty://…`, autostart, cleanup, error reports (Sentry; PostHog is mentioned
  in telemetry filters).

## 6. Mod Loader (35 pinned entries)

archivexl, bepinex-5, bepinex-6-il2cpp, bepinexpack-peak, bg3se, cet, everest, godot-mod-loader, gzdoom,
hk-modding-api, lovely, melonloader, metamod-source, modengine2, modthespire, nvse, owml, red4ext, redscript,
reframework (each game: DMC5, DD2, MH Rise, MH Wilds, RE2/3/4/7/Village, SF6), reloaded-ii, sourcemod, tweakxl,
ue4ss, ultimate-asi-loader, uzdoom; plus official pins for SKSE (Skyrim SE/VR).
The pins point to **Windows builds** (e.g. `BepInEx_win_x64_5.4.23.5.zip`). Under Linux, these
games only run via Proton/Wine; the loaders must be started there with appropriate DLL overrides.

## 7. Windows Binding (the real effort)

- 38 `win32`-switches in the app code, 22 `darwin`-mentions. The 5 `linux`-hits are in third-party libraries:
  **The app code contains no Linux path.**
- Windows-only: `windows-inspector.exe`, `window-tuck.exe`, `elevate.exe` (PE32, presumably UAC help, not analyzed),
  `reg.exe`, `powershell.exe`, VC runtime installer (`mods:install-vc-runtime`), paths under `%APPDATA%`.
- `node_datachannel.node` is a Windows DLL. For Linux, there is the npm package `node-datachannel` with
  Linux prebuilds; for Rust e.g. `datachannel-rs`/`webrtc-rs` (not verified).
- Prism Launcher and Minecraft Java run natively under Linux; Minecraft mashups are the easiest part of phase 3.

## 7a. The Two Add-on EXEs

**`windows-inspector.exe`** (C++, `inspector.cpp`; process/registry helper). Protocol completely from `inspector-supervisor.js`:
JSON-lines over stdio, version 1. App → helper: `{version:1, generation, id, method, params}`; helper → app:
`{kind:"ready"|"heartbeat"|"response", version, generation, id, result|error, nativeCode?}`. Heartbeat every 5 s, timeout 15 s
(start 30 s), restart with backoff (250 ms … 30 s). Methods: `processes`, `commands({pids})`, `modules({names})`,
`steamRoots`, `steamFolders`, `uninstall`, `rockstar`, `gamepads` (→ `{available, idleMs}`). For Linux, **no protocol needs to be reproduced**:
the methods are implemented directly in Rust (`crates/melty-games`). In case of failure, the original falls back to PowerShell/`tasklist`,
on Linux this completely disappears.

**`window-tuck.exe`** (Win32: `SetWindowTextW`, `SetLayeredWindowAttributes`, `ShowWindow`, `EnumWindows`). Purely cosmetic/test aid:
gives the game window the name of the running mashup (taskbar/Alt+Tab, e.g. "Minecraft Ring" instead of "ELDEN RING™") and places
in automated tests a game that has come to the front back to the back. **Not part of the core scope**; on Wayland anyway not
possible for external windows. Skip.

## 8. Tunnel

`mods/tunnel/*`, `relay-page.html/.js`, `relay-page-preload.cjs`, `tunnel-worker.cjs` (44 KB), `node_datachannel.node`.
According to code: WebRTC data channels for multiplayer (`launch({modId, joinCode})`); a hidden relay page passes
message ports to the worker. Protocol details are not yet worked out (task for phase 5).

## 9. Updates

`app-update.yml`: electron-updater, provider `generic`, feed `https://qfzkw7tov6060ah3.public.blob.vercel-storage.com/melty/`,
channel `latest`, `publisherName`: David Miller / Stella Montis Inc (signature check under Windows).
Irrelevant for Linux; own update path (e.g. package manager/AUR) necessary.

## 10. Open Questions / Assumptions

- Does melty.gg fully accept a non-Electron client (platform/UA-gates server-side)? Test in phase 0.
- Does the server provide install tickets platform-dependently? (`mods/ticket.js`, `tunnel-api.js` read.)
- Does the page work properly in WebKitGTK (Tauri default)? Not tested.
- ~~Inspector protocol~~: clarified (§7a).
- Return forms of `modules` and `gamepads` (inspector) not yet read out.

## 11. Legal in Brief

The analysis serves the interoperability for own use. Write own code against the documented bridge
instead of copying the original code, don't distribute the client with Melty code, and check the
terms of use of melty.gg beforehand.
