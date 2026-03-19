# Orbis — Milestone Plan

> Last updated: 2026-03-16

## Completed Milestones

### M1a: Window + Background ✅
Empty window with dark blue space background.

### M1b: Colored Triangle ✅
First GPU render test with a colored triangle.

### M2a: White Lit Sphere ✅
UV sphere with diffuse lighting (camera-based).

### M2b: Textured Sphere (Blue Marble) ✅
NASA Blue Marble day texture on the sphere.

### M2c: Mouse Controls (Orbit + Zoom) ✅
Left mouse = rotate, scroll wheel = zoom.

### M3a: Real-time Sun Position ✅
Astronomical sun position calculation from UTC time (Jean Meeus).
Lighting follows the real sun instead of the camera.

### M3b: Night Texture Loading ✅
NASA Black Marble (city lights) as second GPU texture.

### M3c: Day/Night Blending ✅
Soft terminator (~6° twilight zone) with smoothstep blending
between day and night textures. Real-time movement.

### M4: Generic Layer System ✅
Multi-pass rendering with alpha blending. Unlimited overlay layers
over the base globe. Each layer: own GPU resources, opacity control.
Test: Procedural coordinate grid (lat/lon lines).

### M5: GIBS Integration (NASA Satellite Data) ✅
WMS download from NASA GIBS. VIIRS True Color as first layer.
Background thread for non-blocking download, disk cache,
automatic date fallback (yesterday → day before → ...).
Real cloud formations visible on the globe.

### M6: GUI Overlay (egui) ✅
egui integration into the wgpu render loop. Layer panel with
on/off toggle and opacity slider per layer. Time control:
live mode (real-time) or manual (pick date/time,
triggers new GIBS download, sun follows chosen time).
Persistent settings (config/settings.json). Keyboard shortcuts:
L=Panel, R=Reset, T=Time mode, M=Map, Esc=Exit.
Startup fix: window invisible until GPU init completes.

### M7: 2D Map Projection (Equirectangular) ✅
Switchable 2D map view alongside the 3D globe.
Flat quad (2:1) with identical UV coordinates → all textures
and layers work unchanged on both views.
Day/night terminator: shader reconstructs sphere normals from UV
(theta = v*PI, phi = u*2*PI) for correct lighting incl. seasons.
Orthographic camera with pan (mouse drag) and zoom (scroll wheel).
GUI toggle (🌍 Globe / 🗺 Map) + shortcut M.
Overlays render on quad geometry in multi-pass.

### M8: Polish + Packaging ✅
Release-ready version.

- **M8a:** Procedural starfield — 3,000 billboard quads with instanced
  rendering, size/color by brightness, additive blending, soft glow.
- **M8b:** Atmospheric glow — Fresnel effect in globe shader. Cubic
  falloff, day/night modulated (stronger on sun side).
- **M8c:** Error handling — Texture fallback (magenta checkerboard),
  GIBS status in GUI (⏳/✅/❌), software renderer fallback (WARP/llvmpipe).
- **M8d:** Cross-compile + packaging — `app_path()` for portable paths,
  `scripts/package.sh` + `package.ps1`, GitHub Actions CI/CD.
- **M8e:** README.md + GPL-3.0 license, project documentation.
- **M8f:** i18n system (8 languages, auto-detect system locale).
- **M8g:** English translation of all code comments + README.
- **M8h:** FOV simplification — removed UI controls, fixed low-distortion
  defaults (15°/10°), values still tweakable via settings.json.

---

## Current Roadmap

### M9: Layer Infrastructure ✅
**Goal:** Scalable foundation for dozens of data sources.

**Contents:**
- **Provider abstraction** — `LayerProvider` trait with GIBS implementation
- **Provider registry** — Catalog of 21 GIBS providers across 6 categories
- **Download manager** — Concurrent background downloads with mpsc channels
- **Catalog browser** — GUI with categorized provider list, add/remove
- **Layer persistence** — Active layers + settings saved to config/settings.json
- **Date fallback** — Automatic 3-day fallback for satellite imagery

### M10: External WMS Sources ✅
**Goal:** Integrate non-GIBS data sources via generic WMS provider.

**Contents:**
- **Generic WMS provider** (`wms.rs`) — configurable base URL, optional TIME,
  transparent background, smart caching (timeless layers cached 24h)
- **9 providers across 3 new categories:**
  - Basemap: OpenStreetMap, Topographic Map (Terrestris WMS)
  - Weather: DWD ICON Temperature/Precipitation/Wind/Pressure + Warnings
  - Geology: GEBCO Bathymetry, GEBCO Shaded Relief
- **Mercator reprojection** — OSM/Topo requested in native EPSG:3857,
  reprojected to equirectangular with bilinear interpolation
- **Cache path separation** — GIBS → `cache/gibs/`, WMS → `cache/wms/`
- **Download manager updated** — background threads find both GIBS + WMS providers

**Future candidates (require auth or different protocol):**
- Copernicus Marine Service (CMEMS — requires API key)
- EMODnet (European marine data, bathymetry)
- NOAA Environmental Data (different WMS format)
- Natural Earth (bundled vector data: borders, coastlines)

### M11: GeoJSON / Marker System ✅
**Goal:** Points, lines, and polygons on globe + 2D map.

**Contents:**
- **M11a:** GeoJSON parser — serde_json-based, RFC 7946 compliant.
  Supports FeatureCollection, Feature, bare Geometry, GeometryCollection.
  Multi-geometries flattened. Style extraction from properties
  (marker-color, fill, stroke, stroke-width, marker-size, fill-opacity).
- **M11b:** Point rendering — MarkerSystem with instanced billboard quads,
  screen-space sizing, colored circles at lat/lon. Label system with
  collision avoidance (5-pass greedy push), leader lines for displaced
  labels, panel clipping, DPI-aware sizing.
- **M11c:** Line rendering — LineSystem with great-circle subdivision
  (SLERP, 2° max arc), cylindrical billboards for constant-width lines.
  Shared vertex buffer rebuilt on layer changes.
- **M11d:** Polygon rendering — PolygonSystem with earcut triangulation,
  ring subdivision for globe curvature, semi-transparent fills,
  outline segments fed into LineSystem. Hole support.
- **M11e:** File loading — rfd file dialog + drag & drop (.geojson files).
  Layer management GUI: toggle visibility, remove, feature counts.
  FeatureCollection-level "name" property as layer name.
- **M11f:** Color system — CSS hex (#RGB, #RRGGBB, #RRGGBBAA),
  rgb()/rgba() functional syntax, 50+ named CSS colors.
  Geometry-type defaults (red points, blue lines, teal polygons).
- **M11g:** i18n — 7 GeoJSON keys in all 8 languages.
  Keyboard shortcut G = toggle labels.

### M12: API-based Live Sources ✅
**Goal:** Real-time data from REST APIs, rendered as markers/overlays.

Each source: adapter module, configurable refresh interval,
attribution display, graceful offline handling.

**Implementation order (by API accessibility + visual impact):**

- **M12a:** USGS Earthquakes ✅ — Native GeoJSON from
  `earthquake.usgs.gov/earthquakes/feed/`. No auth needed.
  Magnitude → marker size + color (green/yellow/orange/red).
  Feeds: past hour, past day, past 7 days, significant month.
  Auto-refresh configurable. Label clustering with expand/collapse.

- **M12b:** OpenSky Network ✅ — Custom JSON parser for aircraft
  state vectors from `opensky-network.org/api/states/all`.
  Anonymous access (400 credits/day, 10s resolution).
  Altitude → color (cyan/green/yellow/orange), callsign labels.
  Feeds: global, Europe, North America (bounding box).

- **M12c:** Smithsonian / GVP Volcanoes ✅ — WFS GeoJSON from
  `webservices.volcano.si.edu`. All ~1,222 Holocene volcanoes.
  Last eruption year → color (red=recent, gray=ancient).
  Labels: name + elevation + type. Refresh: once per day.

- **M12d:** Legend Panel ✅ — Collapsible right-side panel showing
  color scales for active data layers. Per-source legends:
  earthquakes (magnitude → color), aircraft (altitude → color),
  volcanoes (eruption year → color), nuclear plants (age → color),
  tectonic plates (fill + boundary). GIBS raster legends as
  downloaded PNG images. Shortcut: K = toggle legend.
  Adapts dynamically: only shows legends for active sources.

- **M12e:** Demo GeoJSON Files ✅ — Bundled example datasets in
  `assets/geojson/` to showcase all geometry types:
  - `nuclear_power_plants.geojson` (Points, 195 plants, age-colored,
    13 known shutdowns marked, WRI v1.3 CC BY 4.0)
  - `submarine_cables.geojson` (Lines, 708 cables, TeleGeography)
  - `tectonic_plates.geojson` (Polygons, 54 plates, Peter Bird 2003)
  Attribution included per file at FeatureCollection level.
  Parser extracts attribution into GeoLayer; GUI footer shows it.

**Deferred to M12d/M12e:**
- World Air Quality Index (waqi.info, requires API key)
- ACLED Conflict Events (requires free registration)

**Deferred to M13:**
- CelesTrak TLEs → needs SGP4 propagator (fits satellite tracking)

**Dropped:**
- EMSA ship positions — free tier too limited for useful display

### M13: Satellite Perspective + Tracking 🔧
**Goal:** xPlanet-style camera rides from orbit.

**Contents:**
- **M13a:** SGP4 core ✅ — `satellite.rs` module with sgp4 crate (v2).
  CelesTrak OMM JSON download (background thread, 200ms courtesy delay).
  SGP4 propagation → TEME → ECEF → geodetic (WGS84, Bowring's method).
  8 built-in satellites: ISS, CSS Tianhe, Hubble, Landsat 8/9,
  Sentinel-2A, NOAA-20, Terra. Auto-refresh at startup.
  Rendered as golden egui circles with name labels.

- **M13b:** Ground tracks + GUI panel ✅ — Past (90 min, orange) and
  future (90 min, cyan) orbit paths projected as screen-space lines.
  Date-line wrap detection. GUI panel with satellite list showing
  live altitude + velocity. Toggle visibility checkbox.
  i18n: `sat_heading`, `sat_show`, `sat_downloading`, `sat_no_data`,
  `sat_tracked` in all 8 languages.

- **M13c:** Camera follow mode ✅ — Click satellite name in panel to
  follow. Camera smoothly lerps to satellite position (yaw/pitch).
  Orbis inside-out convention: pitch = -lat, yaw = π/2 - lon.
  Yaw normalization prevents angular drift. User keeps full zoom
  control. Globe drag breaks follow. Colorblind-accessible track
  colors (yellow past, magenta future). Panel occlusion for tracks
  and markers. CelesTrak parallel download (~2-3s instead of 60s).

- **M13d:** Satellite info panel — detailed view with orbital parameters,
  next pass prediction, TLE age indicator. Manual TLE refresh button.

### M14: Real Star Catalog + Planets ✅
**Goal:** Replace procedural stars with astronomically correct sky.

**Contents:**
- **M14a:** HYG Star Catalog ✅ — 15,598 real stars (HYG v3.7, CC BY-SA 4.0,
  magnitude < 7). Compact binary format (487 KB). B-V color index →
  RGB via Ballesteros 2012 / Tanner Helland. Linear brightness mapping
  with sqrt alpha boost in shader. Graceful fallback to 3,000 procedural
  stars if binary is missing.

- **M14b:** Planet positions ✅ — `planets.rs` module. Simplified Jean Meeus
  algorithms for Mercury, Venus, Mars, Jupiter, Saturn + Moon.
  Heliocentric orbital elements → geocentric equatorial (RA/Dec).
  Verified against Astro-Seek ephemeris (≤2° error). Rendered as
  colored discs with glow + name labels. Moon via Meeus Ch.47.

- **M14c:** Constellation lines (optional) — IAU constellation
  boundaries as line overlays on the sky sphere.

- **Deferred:** JPL Horizons integration, exposure model, constellation lines (M14c)

### M15: Languages + Locale ✅
**Goal:** Comprehensive i18n with in-app language selection.

**27 active languages** (76 keys each):
German, English, French, Spanish, Portuguese, Italian, Dutch,
Swedish, Norwegian, Danish, Finnish, Polish, Czech, Hungarian,
Romanian, Greek, Russian, Ukrainian, Turkish, Hindi,
Japanese, Korean, Chinese (Simplified), Chinese (Traditional),
Indonesian, Vietnamese, Catalan

**Deferred (RTL):** Arabic + Hebrew (ar.json present, he.json planned —
both excluded until egui gains bidirectional text support, see emilk/egui#1016)

**Completed:**
- **M15a:** 20 new language files (added to existing 8) ✅
- **M15b:** Runtime language switching ✅ — `i18n.rs` refactored from
  `OnceLock` to `RwLock`. `set_language()` for runtime switching.
  `available_languages()` scans `assets/lang/` directory.
  ComboBox selector in Settings panel. Persisted in `settings.json`.
- **M15c:** Font support ✅ — Noto Sans font family bundled (~14 MB):
  NotoSans-Regular (Latin/Cyrillic/Greek/Vietnamese),
  NotoSansSC-Regular (CJK Chinese/Japanese),
  NotoSansKR-Regular (Korean Hangul),
  NotoSansArabic-Regular (reserved for future RTL),
  NotoSansDevanagari-Regular (Hindi).
  Loaded as egui font fallbacks at startup.

### M16: Tile-based High-Resolution Zoom ✅
**Goal:** Progressive detail when zooming in, Google Earth-style exploration.

**Progress:**
- **M16a:** Tile infrastructure ✅ — `tile.rs` module: TileCoord (lat/lon→z/x/y),
  TileSource (URL templates for Sentinel-2, OSM, GIBS), TileCache
  (LRU disk cache with configurable size/age), fetch_tile (cache→download).
  6 unit tests passing.
- **M16b:** Cache settings ✅ — Settings fields (tile_cache_max_mb,
  tile_cache_max_days). GUI sliders (100–5000 MB, 0–90 days with ∞).
  Usage display + clear button. Periodic eviction check (~2s).
- **M16c:** Download queue ✅ — TileDownloadQueue with deduplication,
  8 concurrent threads, Arc<TileCache> shared between main + workers.
- **M16d:** Globe-mode rendering ✅ — TileCompositor stitches tiles into
  4096×2048 equirectangular RGBA buffer. Mercator→equirectangular UV mapping.
  Dynamic GPU texture upload via queue.write_texture().
- **M16e:** Map-mode rendering ✅ — Same compositor buffer rendered as overlay
  on the 2D map quad. Tiles rendered BEFORE clouds/grid so overlays stay on top.
- **M16f:** Tile source selector ✅ — ComboBox in Settings. Source change
  resets compositor + invalidates GPU texture. Smooth crossfade opacity
  (smoothstep from distance 4.0→2.5).

**Known limitations (future improvement):**
- Tile resolution: current compositor buffer (4096×2048) limits effective
  detail. Higher zoom levels need multi-resolution atlas or per-tile quads.
- Sentinel-2 Cloudless max zoom is 14; for street-level detail, OSM tiles
  go to zoom 19 but are vector/label maps, not satellite imagery.
- Mercator→equirectangular reprojection is approximate (nearest-neighbor
  scaling). Bilinear interpolation would improve quality.
- No tile prefetching (only visible tiles are requested).

**Tile Sources (all free, all attributed):**
- NASA GIBS WMTS (already integrated): up to 250m resolution (~zoom 8-9),
  XYZ-style: `gibs.earthdata.nasa.gov/wmts/epsg4326/best/{Layer}/default/{Date}/{TileMatrixSet}/{z}/{y}/{x}.jpg`
  Domain sharding via gibs-a/b/c subdomains for parallel downloads.
- OpenStreetMap tiles (zoom 0–19): roads, labels, boundaries.
  `tile.openstreetmap.org/{z}/{x}/{y}.png` — strict usage policy,
  must set User-Agent, max 2 req/s. For heavy use, self-host or use
  OpenFreeMap alternative.
- Sentinel-2 Cloudless (EOX, CC BY-NC-SA 4.0): beautiful cloud-free
  satellite mosaic up to zoom 14.
  `tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2021_3857/default/GoogleMapsCompatible/{z}/{y}/{x}.jpg`
- Stadia/Stamen terrain tiles: terrain visualization with hillshading.

**Architecture:**
- `tile.rs` module: generic XYZ tile fetcher with pluggable URL templates
- Lat/lon + zoom → tile coordinates (Mercator projection math)
- Hybrid approach: globe mode assembles visible tiles into a texture
  atlas; map mode renders tiles directly as quads
- Background download queue with priority (center of view first)
- Progressive LOD: low zoom tiles shown immediately while higher
  zoom tiles download

**Cache Management (critical for Google Earth-style usage):**
- LRU disk cache in `cache/tiles/{source}/{z}/{x}/{y}.png`
- Configurable max cache size (Settings: default 500 MB, range 100 MB – 5 GB)
- Configurable max tile age (Settings: default 7 days, range 1 day – forever)
- Cache size display in Settings panel (current usage / limit)
- Manual “Clear cache” button in Settings
- LRU eviction: oldest-accessed tiles removed first when limit reached
- Per-source cache quotas (optional, to prevent one source filling cache)
- In-memory tile texture cache: ~100 most recent tiles kept as GPU textures

**GUI additions:**
- Zoom level indicator (bottom bar)
- Cache settings in Settings panel (size limit slider, age slider, clear button)
- Tile source selector (which base imagery to use when zoomed in)

### M17: Custom Layer UI
**Goal:** User-defined data sources without touching code.

**Contents:**
- "Add Custom Layer" dialog in GUI:
  - URL template (with `{z}/{x}/{y}` or WMS parameters)
  - Type selector: WMS / WMTS / XYZ / GeoJSON
  - Name, attribution, refresh interval
  - Preview / test connection
- Import/export layer configurations (TOML/JSON)
- GeoJSON file import (drag & drop or file picker)
- Community layer sharing (curated list on GitHub)

### M18: Timelapse / Playback
**Goal:** Animate changes over time.

**Contents:**
- Play button in time control: auto-advance by configurable interval
- Speed control: hours/days/weeks per second
- Pre-fetch GIBS tiles for date range (background download)
- Use cases: cloud movement, seasonal vegetation, ice melt,
  fire progression, deforestation over years

### M19: Desktop Wallpaper / Screensaver
**Goal:** xPlanet-style live desktop integration.

Periodic screenshot of the current globe view, set as desktop wallpaper.
App minimizes to system tray when wallpaper mode is active.

**Approach:** Render frame → read pixels → save PNG → `wallpaper::set_from_path()`
Refresh interval: configurable (30s–10min), default 60s.

**Windows:**
- `SystemParametersInfo(SPI_SETDESKWALLPAPER)` via `wallpaper` crate
- System tray via `tray-icon` crate + winit integration
- Windows 10/11: full support (DirectX 12 via wgpu)
- Windows 8.1: may work via Vulkan backend (not guaranteed)
- Windows 7: Vulkan only (if GPU driver supports it), not advertised

**Linux (via `wallpaper` crate):**
- GNOME, KDE, Cinnamon, Unity, Budgie, XFCE, LXDE, MATE, Deepin
- Wayland compositors: via `swaybg` (Sway, Hyprland, etc.)
- i3/X11: via `feh`
- No single API — crate detects environment and delegates

**Screensaver mode:**
- Fullscreen borderless window on inactivity (simpler than OS integration)
- Mouse movement / key press exits screensaver back to normal view
- Optional: register as XScreenSaver module on Linux (future)

**Tray integration:**
- Activate wallpaper mode → window minimizes to system tray
- Tray icon shows Orbis logo ⬧
- Tray menu: "Show Window", "Wallpaper: ON/OFF", "Quit"
- "Quit" from tray = real exit; window close = minimize to tray

### M20: Easter Eggs + Fun Features
**Goal:** Delight and surprise.

**Contents:**
- ASCII art mode: real-time rendering of globe using ASCII characters
  derived from Blue/Black Marble brightness values
  (` .:-=+*#%@` mapping, as fragment shader or post-process)
  - Triggered by hidden shortcut or settings flag
  - Updates when textures change
- Additional fun ideas TBD

---

## Future Ideas (post-roadmap)

### Map Projections & Display
- **Continent-centered 2D map:** Configurable longitude offset so
  Africa, the Americas, or Oceania can be centered instead of Europe.
  Simple parameter shift in the map shader, no new projection math.
- **Flipped map orientation:** Option to rotate the 2D map 180°
  (South Pole at top). Useful for southern-hemisphere users and
  as an educational tool to challenge Eurocentric map conventions.
- **Earth rotation mode ("Kiosk"):** Auto-rotate globe/map at 15°/hour
  (real-time Earth rotation). Ideal for lobby displays, waiting rooms,
  and infotainment screens. Timer-driven, toggleable in Time Control.
- **Fuller / Dymaxion projection:** Icosahedral unfolding of the globe.
  Visually striking, minimal area distortion. Very complex to implement
  — requires rewriting the entire layer pipeline. Post-v2.0 at earliest.

### Analysis & Interaction
- **Spatial query / filter builder:** UI for cross-layer queries like
  "earthquakes > M5 within 500km of active volcanoes, last 30 days".
  Pure geometry + property filtering, no AI required. High utility.
- **AI-assisted geospatial exploration (optional):** Natural-language
  queries over datasets, automated pattern detection across layers.
  Would require LLM integration (local via llama.cpp or remote API).
  Candidate for an optional plugin rather than core feature.
- **Location search:** Type a place name → camera flies there (geocoding API).
- **Measure tool:** Distance between two points on the globe.

### Gamification
- **Geography quiz mode (Seterra-style):** Interactive quiz on the globe:
  "Click on Brazil", "Where is Mount Fuji?", timed rounds, scoring.
  Requires country boundary polygons (Natural Earth GeoJSON) +
  point-in-polygon hit testing. Custom quiz editor for user-created
  quizzes (place a marker + type the answer). Good fit for educational
  use and the "accessible beyond nerds" goal.

### Rendering & Data
- **Other celestial bodies:** Mars, Jupiter, Saturn, Moon with NASA textures.
  Requires different textures + adjusted size/rotation. Rings and
  atmospheres are additional complexity.
- **Terrain / elevation:** DEM-based relief (mountains as actual elevations).
  Displacement mapping or tessellation. GPU-intensive but spectacular.
- **Screenshot / export:** Save current view as PNG, or record a flyover video.
- **OSINT dashboard:** Aggregated conflict, disaster, and crisis data.
- **Plugin system:** Lua or WASM-based plugins for community extensions.

---

## Data Source Status

### ⚠️ Not Recommended as Built-in

| Source | Reason |
|--------|--------|
| DeepStateMap.Live | No public API. Tile scraping is legally/technically fragile. Use ACLED for conflict data instead. |
| GHGSat | Commercial, very limited public data. |
| IAEA | No tile/WMS service. Nuclear facility positions could be added as static GeoJSON. |

These can potentially be used via the Custom Layer system (M17) by
technically skilled users, but should not be official built-in sources.
