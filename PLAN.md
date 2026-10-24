# 🚀 Melty-Linux – Roadmap to Perfection
*Compare the Windows client with Linux advantages: native performance, customizability, security*

---

## 🎯 Phase 0: Basics & Feasibility (✅ **DONE**)

| Status | Feature | Description |
|---|---|---|
| ✅ | Panel-only client | No WebView for website – pure native panel (Library, Mashups, Settings, Diagnostics) |
| ✅ | Deep links | `melty://desktop/panel/...` and `melty://m/<slug>` (opens details, no 2nd window) |
| ✅ | Tray menu | Start, login, settings |
| ✅ | Login (browser handoff) | Secure OAuth without password entry, token with `0600` permissions |
| ✅ | E2E tests | Python test suite against fake API and fixtures for Steam/Lutris/Heroic/Faugus |

> 🚀 **Linux advantage**: Through the panel-only architecture **no security holes** through WebView scraping, **faster loading times** and **better resource usage**.

---

## 🎯 Phase 1: Game Integration & Discovery (✅ **DONE**)

| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| ✅ | Library (Steam) | 🟢 | 🟢 | Native, Flatpak, Snap |
| ✅ | Library (Lutris) | 🟢 | 🟢 | Wine/Proton |
| ✅ | Library (Heroic) | 🟢 | 🟢 | Epic, GOG, Amazon |
| ✅ | Library (Faugus) | 🟢 | 🟢 | New native discovery |
| ✅ | Proton prefixes | 🟢 | 🟢 | Detection of running games via `/proc` |
| ✅ | Real-time changes | 🟢 | 🟢 | `notify` crate instead of Windows worker |

> 🚀 **Linux advantage**: **Direct system integration** (no `windows-inspector.exe` needed), **no unnecessary process overhead**, **better process detection** via `/proc/cmdline`.

---

## 🎯 Phase 2: Mashups & Mod Management (✅ **DONE**)

| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| ✅ | Catalog from melty.gg | 🟢 | 🟢 | `/api/mods?limit=60&sort=top` |
| ✅ | Local caching | 🟢 | 🟢 | `mashup-catalog.json`, TTL 60 min |
| ✅ | Free text search | 🟢 | 🟢 | 300 ms debounce (client-side) |
| ✅ | Filter | 🟢 | 🟢 | Game, installed, sorting |
| ✅ | ZIP unpacking | 🟢 | 🟢 | Rust crate without system package |
| ✅ | Install/remove | 🟢 | 🟢 | Journal/rollback, enable/disable |
| ✅ | Open folder | 🟢 | 🟢 | `mods:open-folder` |
| ✅ | Start/stop | 🟢 | 🟢 | `mods:launch`, `mods:stop` (Proton/Wine) |

> 🚀 **Linux advantage**: **Faster ZIP unpacking** (Rust vs. 7-Zip), **no dependency on external binaries**, **safer path access** (no `fs-safety.js` needed).

---

## 🎯 Phase 3: Features for Advanced Users (⚠️ **IN WORK**)

### 🔧 3.1 Tunnel & Multiplayer
| Status | Feature | Windows | Linux | Plan |
|---|---|---|---|---|
| 🚧 | WebRTC tunnel | `tunnel-worker.cjs` + `node_datachannel.node` | Rust implementation | 1. Integrate `webrtc-rs`/`datachannel-rs`<br>2. Derive protocol from `tunnel-worker.cjs`<br>3. Relay page as in-app panel |

### 🧠 3.2 Agents (Coding CLIs)
| Status | Feature | Windows | Linux | Plan |
|---|---|---|---|---|
| 🚧 | Claude Code / Codex | `agent-runs:*` (31 channels) | Rust child processes | 1. Implement `agent-runs:*` commands<br>2. Brief signature check<br>3. Store chat history locally<br>4. Overlay over game (Wayland layer-shell) |

### ☁️ 3.3 PC Uploads
| Status | Feature | Windows | Linux | Plan |
|---|---|---|---|---|
| 🚧 | Creator uploads | `mods:pc-upload-*` (6 channels) | Rust upload handler | 1. `mods:pc-upload-*` commands<br>2. `notify` for file events<br>3. Upload status in panel |

---

## 🎯 Phase 4: Advanced UI & Customization 🎨 (🎯 **EXCLUSIVE FOR LINUX**)

### 🔨 4.1 Theme System & Colors
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟢 | **Dark/light theme** | 🟢 | 🟢 | CSS variables (`--theme-bg`, `--theme-accent`) |
| 🟡 | **Custom colors** | ❌ | 🟢 | Color palette in settings menu |
| 🟡 | **Theme export/import** | ❌ | 🟢 | JSON export for community themes |

---

## 🎯 Phase 5: Platform-Specific Features (🚀 **EXCLUSIVE FOR LINUX**)

### 🐧 5.1 Desktop Integration
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟢 | **System tray** | 🟢 | 🟢 | Wayland/X11 support |
| 🟢 | **Deep links** | 🟢 | 🟢 | `melty://` as default |
| 🟡 | **Desktop notifications** | 🟢 | 🟢 | `libnotify` with actions |
| 🟡 | **D-Bus support** | ❌ | 🟡 | `org.freedesktop.Notifications`, `org.freedesktop.FileManager1` |

### 🧪 5.2 Debugging & Diagnostics
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟢 | **System report** | 🟢 | 🟢 | `Diagnostics` tab with `copy report` |
| 🟡 | **Profile export** | ❌ | 🟡 | JSON export for bug reports |
| 🟡 | **Debug overlay** | ❌ | 🟡 | `Alt+D` for FPS, RAM, CPU |
| 🟡 | **Network inspector** | ❌ | 🟡 | HTTP requests with headers/status |

### 🎮 5.3 Game-Specific Features
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟢 | **SteamDeck optimization** | 🟢 | 🟡 | Touchpad control for navigation |
| 🟡 | **Controller feedback** | ❌ | 🟡 | Haptic feedback during mod installation |
| 🟡 | **Proton configurator** | ❌ | 🟡 | Winetricks suggestions for games |

---

## 🎯 Phase 6: Developer Tools & SDK (🎯 **EXCLUSIVE FOR LINUX**)

### 🛠️ 6.1 Mod Development
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟡 | **Mod boilerplate** | ❌ | 🟡 | `melty create mod <name>` CLI |
| 🟡 | **Dev server** | ❌ | 🟡 | `MELTY_DEV_DESKTOP=1` with hot reload |
| 🟡 | **Local mod library** | ❌ | 🟡 | Drag-and-drop of `.zip` directly into library |

### 🌐 6.2 Web Integration
| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟡 | **Native WebView fallback** | 🟢 | 🟢 | Panel tab with website view |
| 🟡 | **Custom CSS editor** | ❌ | 🟢 | Code editor in settings (Vim/VSCode keybindings) |
| 🟡 | **Extension API** | ❌ | 🟡 | Plugin system for JS/WASM |

---

## 🎯 Phase 7: Performance & Optimization (🚀 **EXCLUSIVE LINUX ADVANTAGES**)

| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟢 | **Zero-dependency ZIP** | 🟢 | 🟢 | Rust crate without `7z` |
| 🟢 | **Asynchronous storage** | 🟢 | 🟢 | SQLite/LevelDB without blocking |
| 🟡 | **GPU acceleration** | 🟢 | 🟢 | Vulkan/OpenGL for canvas |
| 🟡 | **Offline-first** | 🟢 | 🟡 | Caching strategy (SW-like) |
| 🟡 | **AppImage compression** | ❌ | 🟡 | UPX compression for <100MB |

---

## 🎯 Phase 8: Community & Ecosystem (🎯 **EXCLUSIVE FOR LINUX**)

| Status | Feature | Windows | Linux | Note |
|---|---|---|---|---|
| 🟡 | **Theme repository** | ❌ | 🟡 | GitHub repo with community themes |
| 🟡 | **Mod database export** | ❌ | 🟡 | CSV/JSON for third-party tools |
| 🟡 | **CLI tool** | ❌ | 🟡 | `melty` as Bash/Zsh command |
| 🟡 | **Webhook integration** | ❌ | 🟡 | Notify during mod update |

---

## 🎯 Phase 9: Production-Ready (🚀 **PLANNED FOR 1.0**)

| Status | Feature | Description |
|---|---|---|
| 🟡 | **Packaging** | `deb`, `rpm`, `appimage`, `flatpak`, `snap` |
| 🟡 | **Auto-updates** | Via package manager + fallback to `app-update.yml` |
| 🟡 | **Telemetry (opt-in)** | Anonymous metrics (Sentry/PostHog alternative) |
| 🟡 | **Dark mode auto** | System theme detection |
| 🟡 | **Accessibility** | Screen reader, keyboard navigation |

---

## 📊 Linux vs. Windows – The Advantages at a Glance

| Category | Windows | Linux | Winner |
|---|---|---|---|
| **Performance** | Electron (Chromium) | Rust (native) | 🚀 Linux |
| **Security** | WebView bridges | Panel-only + command filtering | 🚀 Linux |
| **Customizability** | Dark/light theme | Themes + backgrounds + CSS | 🚀 Linux |
| **Integration** | Taskbar | D-Bus, Wayland, layer-shell | 🚀 Linux |
| **Developer-friendliness** | VSCode extensions | CLI, extension API | 🚀 Linux |
| **Resource usage** | ~500MB RAM | <100MB RAM (estimated) | 🚀 Linux |

---

## 🎨 Bonus: "Linux-only" Features for Envy
- **Custom CSS**: `~/.config/gg.melty.linux-client/custom.css` for extreme customization
- **Wayland layer-shell**: Battery-optimized blur effect (`acrylic` for GNOME/KDE)
- **System monitor overlay**: `Alt+M` shows FPS, RAM, CPU in panel
- **Proton configurator**: One-click Wine settings for each game
- **Touchpad support**: Swipe gestures for navigation on SteamDeck
- **Multi-monitor optimization**: Each panel on its own monitor

---

## 📌 Next Steps (Recommended Order)
1. **Phase 3.1 (Tunnel)** → Minecraft mashups without WebView
2. **Phase 4.1 (Themes)** → "Envy feature" for Windows users
3. **Phase 3.2 (Agents)** → Mod developer flow
4. **Phase 5.1 (D-Bus)** → System integration
5. **Phase 4.2 (Backgrounds)** → Visual highlight

---

*„Linux isn’t just a better OS, it’s a better platform. Let’s build the best mod client the world has ever seen.“*  
— Melty Linux Team
