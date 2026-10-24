# ARCHITECTURE.md – Melty-Linux (Rust/Tauri)

> **Implementation status 22.10.2026:** The real client has **no** melty.gg WebView anymore. The only window is
> the native panel (`ui/panel.html`) with Library, Mashups, Settings, and Diagnostics; Mashups run via the
> public JSON endpoints (see `SPIKE.md`). The bridge/WebView layer described here is the target image for
> later phases (Mod Player, Agents) and is currently parked (`src-tauri/src/bridge_webview.rs.parked`,
> `capabilities/` only contains `panel.json`).

Basis: `ANALYSIS.md`. Goal: self-contained Linux client that runs against the same website `https://melty.gg`
and implements the bridge `window.meltyDesktop` natively. No copy of the original code, but a
reimplementation according to the documented interface.

## 1. Two Paths

| | A: Original app under Linux-Electron | B: Own client (Tauri) |
|---|---|---|
| Effort | low: unpack `app.asar`, patch Windows parts, start Linux-Electron | high |
| Self-contained | no (runs on foreign code, updater/signature checks interfere) | yes |
| Distributable | problematic | yes |
| Suitability | **quick feasibility test** for phase 0 | **the actual goal** |

Recommendation: A only as a throwaway experiment to see early if melty.gg accepts a Linux client
(login, `device-token`, install tickets). Then B.

## 2. Layers (B)

```
┌─────────────────────────────────────────────┐
│ WebView  →  https://melty.gg  (UI, remote)  │
│   initialization_script: window.meltyDesktop│
└───────────────▲─────────────────────────────┘
                │ invoke / emit  (Origin-allowlist: only melty.gg)
┌───────────────┴─────────────────────────────┐
│ Rust core (Tauri)                           │
│  bridge/   98 channels → handlers (bridge-channels.json)
│  auth/     handoff login, device token, keyring
│  games/    Steam/Heroic/Lutris/Faugus/Prism scanner, inotify
│  mods/     tickets, downloads, archive, loader, journal, launch
│  agents/   process execution claude/codex, chats, overlay
│  tunnel/   WebRTC data channels (multiplayer)
│  platform/ Wayland/X11, tray, autostart, deep link melty://
└─────────────────────────────────────────────┘
```

**Bridge injection:** `window.meltyDesktop` is set via initialization script before page code. Every method
calls `invoke("<channel>", args)`, every `on…` registers an event listener. Take names and argument forms from
`src/preload/preload.cjs` (including `isDesktop: true`, `platform: "linux"`, `appVersion`, `agents.version`,
`agentRuns.version`, `mods.version`, `waitsForClose`, `keepsChats`, `checksBriefs`, `setsUp`). The page
branches after these flags; `platform` and missing features show the page what the app can do.

**WebView risk:** Tauri uses WebKitGTK under Linux. melty.gg is a Next.js app; whether it runs completely there
(fonts, streams, `webUtils` replacement for file paths), is untested. Fallback: Chromium-based WebView/CEF.
Check in phase 0, not later.

**File paths:** Electron provides paths via `webUtils.getPathForFile`. In Tauri, use drag-and-drop events
or native file dialog instead; as in the original, only one path may be accepted that the user has chosen themselves.

## 3. Security Invariants (adopt from original, don't weaken)

1. Bridge only for pages with origin `https://melty.gg` (block navigation to foreign origins in the same window/open externally).
2. Agent starts and messages only with **signed brief** from melty.gg, typed by user or after
   explicit yes in native dialog (`agents/agent-gate`). Signature check belongs in the Rust core, not in the page.
3. PC uploads: don't read or send anything before the user presses "Allow"; path comes from server, destination only
   defined by Melty hosts (`pc-uploads.js` validates plan and destination).
4. Downloads check against pinned SHA-256 (`LOADER_PINS`), unpack archives only into the intended folder
   (`fs-safety.js`: prevent path traversal).
5. Tokens in secret service/keyring (libsecret), not in plaintext.

## 4. Phase Plan with Acceptance Criteria

**Phase 0 – Shell & Login** (Feasibility)
- Tauri window loads melty.gg, bridge stub with all 98 channels (returns empty/"not supported" responses).
- Login via handoff (ANALYSIS §3), device token in keyring, logout.
- Deep link `melty://` registered (`.desktop` file, single-instance).
- ✔ Logged in, page renders, `isDesktop` active. ✔ Decision basis: server accepts the client? WebKitGTK ok?

**Phase 1 – Detect games** (`games:list|refresh|changed`)
- Steam: `libraryfolders.vdf` + `appmanifest_*.acf` (Native, Flatpak `~/.var/app/com.valvesoftware.Steam`, Proton prefix `compatdata`).
- Heroic, Lutris, Faugus (prefix paths), Prism instances; inotify instead of Windows watchers.
- Assignment via game catalog (`lib/mods/kit/games.ts`, ~250 entries) and aliases (`game-aliases.js`).
- Process detection via `/proc` instead of `windows-inspector.exe`.
- ✔ "My Games" shows installed games correctly.

**Phase 2 – Agents & Overlay** (`agents:*`, `agent-runs:*`)
- Claude Code/Codex as child process, chats locally, events to the page; login/setup of agents.
- Overlay over the game: on Wayland without always-on-top guarantee, solve with layer-shell or as own window.
- ✔ Chat starts from page, answers flow back.

**Phase 3 – Minecraft mashups** (`mods:minecraft-*`)
- Prism Launcher natively (Flatpak/AppImage), instances, Microsoft login flow, demo mode, name selection.
- ✔ Install and start one Minecraft mashup.

**Phase 4 – Mods for Windows games via Proton** (main effort, `mods:install|update|launch|…`)
- Ticket → download → checksum → unpack → loader installation → journal/rollback.
- Start via Steam (`steam://rungameid/…` with startup options/`WINEDLLOVERRIDES`) or Proton directly.
- Address loaders by groups: BepInEx/MelonLoader (Unity), REFramework, UE4SS, Source (SourceMod/Metamod), rest.
- ✔ Reference game for each loader group runs.

**Phase 5 – Tunnel/Multiplayer** (`launch({joinCode})`)
- WebRTC data channels in Rust; derive protocol from `tunnel-worker.cjs` + `tunnel-api.js`.

**Phase 6 – Daily use:** tray, autostart (`settings:*`), notifications, Discord rich presence, updates (package way), error reports (optional).

## 5. Working with Small Context Window (e.g. Qwen 64K)

- Never load the whole bundle. `node split-bundle.mjs <main.bundle.mjs> modules/` generates one file per module plus `INDEX.json`.
- Read one module per task; split large modules (`mod-manager`: 7 037 lines) into 600-line sections beforehand
  and write a 15-line summary per section in `notes/<module>.md`.
- Write a specification document per phase (`spec/phase-N.md`: channels, data forms, error cases) before creating code.
- Observe the bridge data forms from the page (DevTools in WebView), don't guess: the return objects are only indirectly
  in the original code, but the page expects them exactly.

## 6. Non-goals (for now)

macOS/Windows support, original updater, telemetry (Sentry/PostHog), `elevate.exe` functions (UAC doesn't exist on Linux).
