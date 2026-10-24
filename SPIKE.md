# SPIKE.md – Mashups and Login without Main Window

Status: 22.10.2026, checked against `https://melty.gg` (public, without login).

## 1. Found Endpoints

The Windows app itself contains **no** search endpoint for mashups – it loads the website in an
Electron window. The website, however, offers two public JSON endpoints that also work without login:

| Purpose | Endpoint | Example |
|---|---|---|
| Catalog (paginated) | `GET /api/mods?limit=60&offset=0&sort=top` | 60 entries, max. 60 per page |
| Full-text search | `GET /api/search?q=minecraft` | 12 hits + games + players |
| Details | `GET /api/mods/<slug>` | `{"mod": {…}}` |
| Deployment ID | `GET /api/mods/deployment` | `{"id":"dpl_…"}` |

**Sort values** (verified): `trending` (default), `new`, `top` ("Most played").
Further parameters (`game`, `games`, `genre`, `tag`, `q`, `search`, `query`, `cursor`) are **ignored** by the
catalog endpoint. There is no server-side filtering by game, genre, or tag, and no total count in the response. The catalog
comprised **2,584** entries on 22.10.2026 (44 pages à 60).

`/api/search` is limited to 12 mashups and only provides a short form:
`slug, title, coverUrl, games[], creator, stars, version`. It is the search from the header, not the
results page.

**Consequence for the client:** The complete catalog is loaded once (44 requests, parallel in
blocks), cached locally, and then filtered/sorted locally. This makes free text, game,
"only installed games", sorting, and pagination consistent. The cache is located in
`<config>/mashup-catalog.json` and is renewed in the background after 60 minutes.

### Data format of a catalog entry (shortened)

```json
{
  "id": "19abee01-dce4-4115-97ea-3e4be42a5ca7",
  "slug": "minecraft-ring",
  "title": "Minecraft Ring",
  "tagline": "Walk, build and fight with Minecraft in Elden Ring's Lands Between.",
  "creator": { "id": "melty", "name": "Melty", "avatarUrl": "…", "verified": true },
  "games": [
    { "slug": "elden-ring", "name": "Elden Ring", "shortName": "Elden Ring", "steamAppId": "1245620" },
    { "slug": "minecraft-java", "name": "Minecraft: Java Edition", "shortName": "Minecraft" }
  ],
  "pricing": "free", "priceCents": 0, "currency": "usd",
  "coverUrl": "/api/mods/media/…/poster",
  "previewMedia": [ { "url": "/api/mods/media/…", "kind": "video" } ],
  "madeBy": "siddoff", "version": "0.3.1", "status": "live",
  "gets": 14987, "installs": 8565,
  "publishedAt": "2026-10-07T00:31:22.178Z",
  "updatedAt": "2026-10-21T22:50:54.575Z",
  "stars": 4,
  "play": { "releaseId": "…", "needs": [ … ], "mode": "installed", "multiplayer": null }
}
```

The details additionally provide `descriptionMd`, `testUnderWay`, `awaitingPlay`, `playersGetEarlier`,
`goingLive`, `testRun`, `testedRelease`.

## 2. Login

From `capture/main/auth.mjs` (Windows app):

1. `openLogin(href)` appends `desktop_handoff` + `desktop_handoff_secret` to the Google/Supabase URL.
2. The system browser logs in; the app polls `GET /auth/desktop-handoff?…` (900 ms).
3. On `{"status":"ready","fragment":"…"}` the app loads `/auth/desktop-import#<fragment>` in its
   webview. This creates the cookie session.
4. Then `POST /api/desktop/device-token` with `{deviceName}` **and the cookies of the webview**.
   Response `{token}` in format `^[a-z]{2,8}_[A-Za-z0-9_-]{16,}$`.

**For mashup search/details, no login is needed.** The public endpoints provide everything.
A later login (mod player, own library) still needs a webview for step 3/4
or another cookie source; this is intentionally not part of this spike.

## 3. Decision

**Option A** for mashups: native Rust binding (`reqwest`), no website, no webview.
The bridge (`window.meltyDesktop`) is no longer loaded at startup. It remains as a module for
later phases in the repo, but is no longer registered.

## 4. Limitations / Open Points

- No server-side filter by game/genre/tag. The client loads the complete catalog and filters locally.
  This is acceptable at ~2,600 entries, but must be re-evaluated if growth is strong.
- Catalog endpoint provides no genres. The genre pages (`/discover/...`) are only HTML.
- No "Install/Play": The mod player (phase 4) is missing. Details view only links to melty.gg.
- No write endpoints, no HTML scraping.

## 5. Implementation (22.10.2026)

- `src-tauri/src/mashups.rs`: catalog cache (`<config>/mashup-catalog.json`, TTL 60 min), parallel loading in
  8er blocks, deduplication via slug, local filter/sorting/facets, detail fetch.
- Panel commands: `mashups_search`, `mashups_refresh`, `mashups_detail`, `mashups_open` (each only window
  "panel", slug whitelist, external URLs only http(s) on `web_base`).
- `ui/panel.html`: mashups tab with search, game dropdown, "only installed games", sorting, cards, details,
  "load more", error/empty state, and catalog age.
- "Only installed games" compares Melty game slugs and Steam app IDs with the local library. The
  complete alias assignment from `game-aliases.js` is not yet adopted; the filter may therefore be
  conservative with differing game names.
- Login remains parked: Without website webview, steps 3/4 of the handoff (cookie session, device token)
  cannot be completed. `melty://auth/success` is therefore only logged and discarded.
- Verification: `cargo test --workspace` (54 tests) and `tests/e2e/run.py` (20 checks against fake API and
  fixtures for Steam/Lutris/Heroic/Faugus). The real melty.gg API was spot-checked in the spike,
  not described in the automated test.
