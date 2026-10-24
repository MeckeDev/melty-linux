#!/usr/bin/env python3
"""E2E-Test für den Panel-only-Client.

Startet das echte Binary gegen einen lokalen Fake von melty.gg und prüft:
  - Spiele-Erkennung (Steam, Lutris, Heroic/GOG, Faugus) samt installationsId
  - Mashup-Katalog, lokale Filter/Sortierung und Detail über /api/mods
  - Deep Links: Panel-Tabs und melty://m/<slug> (öffnet das Detail, kein Website-Fenster)
  - dass keine melty.gg-Website geladen und keine Bridge mehr aufgerufen wird

Braucht eine X-Sitzung (oder xvfb-run). Aufruf:
  python3 tests/e2e/run.py [pfad/zur/melty-linux]
"""
import json, os, re, sqlite3, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target/debug/melty-linux")
PORT = int(os.environ.get("E2E_PORT", "18765"))
TMP = tempfile.mkdtemp(prefix="melty-e2e-")
HOME, PROC, BINDIR = f"{TMP}/home", f"{TMP}/proc", f"{TMP}/bin"
STEAM = f"{HOME}/.local/share/Steam"
REPORT = f"{TMP}/report.json"
requests, agents, lock = [], set(), threading.Lock()

MODS = [
    {
        "id": "m1", "slug": "minecraft-ring", "title": "Minecraft Ring",
        "tagline": "Two worlds, one ring.", "creator": {"id": "c1", "name": "Mecke", "verified": True},
        "games": [{"slug": "elden-ring", "name": "Elden Ring", "shortName": "Elden Ring", "steamAppId": "1245620"}],
        "installs": 10, "gets": 120, "stars": 4.5, "pricing": "free", "priceCents": 0,
        "coverUrl": "/api/mods/media/minecraft-ring.png", "version": "1.2.0", "status": "published",
        "publishedAt": "2026-10-01T10:00:00Z", "updatedAt": "2026-10-02T10:00:00Z",
    },
    {
        "id": "m2", "slug": "gta-craft", "title": "GTA Craft",
        "tagline": "Blocks in Los Santos.", "creator": {"id": "c2", "name": "Someone"},
        "games": [{"slug": "gta-v", "name": "Grand Theft Auto V", "shortName": "GTA V", "steamAppId": "999999"}],
        "installs": 50, "gets": 400, "stars": 3.9, "pricing": "paid", "priceCents": 299,
        "coverUrl": "https://cdn.example/gta-craft.png", "version": "0.9.0", "status": "published",
        "publishedAt": "2026-09-01T10:00:00Z", "updatedAt": "2026-09-20T10:00:00Z",
    },
]


def manifest(appid, name, install):
    os.makedirs(f"{STEAM}/steamapps/common/{install}", exist_ok=True)
    with open(f"{STEAM}/steamapps/appmanifest_{appid}.acf", "w") as f:
        f.write(f'"AppState"\n{{\n\t"appid"\t"{appid}"\n\t"name"\t"{name}"\n\t"StateFlags"\t"4"\n\t"installdir"\t"{install}"\n}}\n')


def fixtures():
    manifest(620, "Portal 2", "Portal 2")
    manifest(271590, "Grand Theft Auto V", "Grand Theft Auto V")

    # GTA läuft (Fake-/proc), damit die Prozess-Erkennung geprüft wird.
    os.makedirs(f"{PROC}/4242", exist_ok=True)
    with open(f"{PROC}/4242/comm", "w") as f:
        f.write("GrandTheftAuto\n")
    gta = f"Z:{STEAM}/steamapps/common/Grand Theft Auto V/GTA5.exe".replace("/", "\\")
    with open(f"{PROC}/4242/cmdline", "wb") as f:
        f.write(gta.encode() + b"\0")

    # Lutris (SQLite + installiertes Wine-Spiel)
    elden = f"{HOME}/Games/Elden"
    os.makedirs(elden, exist_ok=True)
    db = f"{HOME}/.local/share/lutris/pga.db"
    os.makedirs(os.path.dirname(db), exist_ok=True)
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE games (id INTEGER PRIMARY KEY, name TEXT, slug TEXT, runner TEXT, directory TEXT, installed INTEGER, configpath TEXT)")
    con.execute("INSERT INTO games (name, slug, runner, directory, installed) VALUES (?,?,?,?,1)", ("Elden Ring", "elden-ring", "wine", elden))
    con.commit()
    con.close()

    # Heroic (GOG)
    witcher = f"{HOME}/Games/Witcher3"
    os.makedirs(witcher, exist_ok=True)
    heroic = f"{HOME}/.config/heroic/gog_store/installed.json"
    os.makedirs(os.path.dirname(heroic), exist_ok=True)
    with open(heroic, "w") as f:
        json.dump({"installed": [{"appName": "1207658924", "title": "The Witcher 3", "install_path": witcher}]}, f)

    # Faugus Launcher
    cyber = f"{HOME}/Games/Cyberpunk/bin/x64"
    os.makedirs(cyber, exist_ok=True)
    faugus = f"{HOME}/.local/share/faugus-launcher/games.json"
    os.makedirs(os.path.dirname(faugus), exist_ok=True)
    with open(faugus, "w") as f:
        json.dump([{"gameid": "Cyberpunk2077", "title": "Cyberpunk 2077", "path": f"{cyber}/Cyberpunk2077.exe", "prefix": "/prefixes/cp", "hidden": False}], f)


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code, body=b"", ctype="application/json"):
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self._send(200)

    def do_GET(self):
        with lock:
            requests.append(self.path)
            agents.add(self.headers.get("user-agent", ""))
        path = self.path.split("?", 1)[0]
        if path == "/api/mods":
            return self._send(200, json.dumps({"mods": MODS}).encode())
        if path.startswith("/api/mods/"):
            slug = path[len("/api/mods/"):]
            mod = next((m for m in MODS if m["slug"] == slug), None)
            if mod is None:
                return self._send(404, b"{}")
            detail = dict(mod, description=f"Description of {mod['title']}.")
            return self._send(200, json.dumps({"mod": detail}).encode())
        return self._send(404, b"{}")

    def do_POST(self):
        with lock:
            requests.append("POST " + self.path)
        self._send(404, b"{}")


def wait_for(check, timeout=60, interval=0.3):
    end = time.time() + timeout
    while time.time() < end:
        try:
            value = check()
            if value:
                return value
        except Exception:
            pass
        time.sleep(interval)
    return None


def shot(name):
    path = f"{TMP}/{name}.png"
    subprocess.run(["import", "-window", "root", path], timeout=20, capture_output=True)
    return path


fixtures()
srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()

if not os.environ.get("DISPLAY"):
    sys.exit("Kein DISPLAY. Nutze z. B.: xvfb-run -a python3 tests/e2e/run.py")

env = dict(
    os.environ, HOME=HOME, XDG_CONFIG_HOME=f"{HOME}/.config", MELTY_DEV_DESKTOP="1",
    MELTY_WEB_URL=f"http://127.0.0.1:{PORT}", MELTY_PROC_ROOT=PROC, MELTY_E2E_REPORT=REPORT,
    PATH=f"{BINDIR}:{os.environ['PATH']}", GDK_BACKEND="x11", LIBGL_ALWAYS_SOFTWARE="1",
    WEBKIT_DISABLE_COMPOSITING_MODE="1", WEBKIT_DISABLE_DMABUF_RENDERER="1",
    WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS="1", NO_AT_BRIDGE="1",
)
log_path = f"{TMP}/app.log"
log = open(log_path, "w")
app = subprocess.Popen([BIN], env=env, stdout=log, stderr=subprocess.STDOUT, preexec_fn=os.setsid)

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))


try:
    report = wait_for(lambda: json.load(open(REPORT)) if os.path.exists(REPORT) else None)
    check("Debug-Bericht geschrieben (App startet)", report is not None, log_path)
    report = report or {}

    games = report.get("games") or []
    names = [g.get("displayName") or g.get("name") for g in games]
    check(
        "Alle Launcher erkannt (Steam, Lutris, Heroic, Faugus)",
        sorted(names) == ["Cyberpunk 2077", "Elden Ring", "Grand Theft Auto V", "Portal 2", "The Witcher 3"],
        names,
    )
    launchers = {g.get("launcher") for g in games}
    check("Launcher-Feld gesetzt", launchers == {"Steam", "Lutris", "Heroic", "Faugus"}, launchers)

    gta_game = next((g for g in games if g.get("appId") == "271590"), {})
    check("Laufendes Spiel erkannt (Fake-/proc)", gta_game.get("running") is True and gta_game.get("processName") == "GTA5.exe", gta_game)
    check(
        "installationId: 24 Hex, eindeutig; launchKind korrekt",
        len(games) == 5 and len({g.get("installationId") for g in games}) == 5
        and all(re.fullmatch(r"[0-9a-f]{24}", g.get("installationId") or "") for g in games)
        and all(g.get("launchKind") in ("uri", "command") for g in games)
        and next((g for g in games if g.get("launcher") == "Faugus"), {}).get("launchKind") == "command",
        [(g.get("launcher"), g.get("launchKind"), g.get("installationId")) for g in games],
    )
    check("Erkennung ohne Fehler", (report.get("detection") or {}).get("state") == "ready", report.get("detection"))

    top = report.get("top") or {}
    check("Katalog geladen (2 Mashups)", top.get("total") == 2 and len(top.get("items") or []) == 2, top.get("total"))
    check("Sortierung 'top' nach Installationen", [m["slug"] for m in top.get("items") or []] == ["gta-craft", "minecraft-ring"], [m.get("slug") for m in top.get("items") or []])
    check("Spiel-Facetten vorhanden", {f["slug"] for f in top.get("games") or []} == {"elden-ring", "gta-v"}, top.get("games"))

    inst = report.get("installed") or {}
    check("Filter 'nur installierte Spiele' (Elden Ring)", [m["slug"] for m in inst.get("items") or []] == ["minecraft-ring"], inst.get("items"))

    text = report.get("text") or {}
    check("Textsuche lokal", [m["slug"] for m in text.get("items") or []] == ["gta-craft"], text.get("items"))

    gf = report.get("gameFilter") or {}
    check("Filter nach Spiel", gf.get("game") == "elden-ring" and [m["slug"] for m in (gf.get("result") or {}).get("items") or []] == ["minecraft-ring"], gf)

    det = report.get("detail") or {}
    check("Detail über /api/mods/<slug>", (det.get("mod") or {}).get("title") == "GTA Craft", det)

    with lock:
        api = [r for r in requests if r.startswith("/api/mods")]
        bad = [r for r in requests if not r.startswith("/api/mods")]
        uas = list(agents)
    check("Nur Mashup-API aufgerufen (keine Website, keine Bridge)", bad == [], bad)
    check("Katalog mit limit=60&sort=top geladen", any(r.startswith("/api/mods?") and "limit=60" in r and "sort=top" in r for r in api), api[:5])
    check("User-Agent enthält Melty/1.0.132 MeltyLinux", any("Melty/1.0.132 MeltyLinux/" in ua for ua in uas), uas[:2])

    # Deep Links an die laufende Instanz (Zweitstart wird weitergeleitet).
    for tab in ("games", "mashups", "settings", "diagnostics"):
        subprocess.run([BIN, f"melty://desktop/panel/{tab}"], env=env, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.8)
    before = sum(1 for r in requests if r.startswith("/api/mods/minecraft-ring"))
    subprocess.run([BIN, "melty://m/minecraft-ring"], env=env, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    detail_calls = wait_for(lambda: sum(1 for r in requests if r.startswith("/api/mods/minecraft-ring")) > before, timeout=20)
    check("melty://m/<slug> öffnet Detail (API-Aufruf)", detail_calls, before)
    check("Prozess lebt nach Deep Links (ein Fenster)", app.poll() is None, app.poll())

    shot("after-links")
    log.flush()
    log_text = open(log_path, errors="replace").read()
    check("Panel-Fenster ohne Fehler geöffnet", "[panel] konnte nicht geöffnet werden" not in log_text and "Fehler beim Start" not in log_text, log_text[-400:])
    check("Kein Website-/Bridge-Login mehr", "Login-Import" not in log_text, log_text[-400:])
finally:
    try:
        os.killpg(os.getpgid(app.pid), 15)
    except Exception:
        pass
    time.sleep(1)
    log.close()
    srv.shutdown()

width = max((len(n) for n, _, _ in checks), default=10)
for name, ok, detail in checks:
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n       → {detail}"))
print(f"\n{sum(ok for _, ok, _ in checks)}/{len(checks)} bestanden; Logs: {TMP}")
print("--- App-Log (letzte Zeilen) ---")
print("".join(open(log_path, errors="replace").readlines()[-15:]))
sys.exit(0 if checks and all(ok for _, ok, _ in checks) else 1)
