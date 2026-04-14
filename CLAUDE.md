# Orbis — Project Context for Claude Code

## What is Orbis?
A real-time 3D Earth viewer desktop app inspired by xPlanet, written in Rust. Licensed GPL-3.0.
Targets Windows 10/11 and Linux (CachyOS). Code and comments in English.
GitHub: System-K/orbis

## Developer
Yves (GitHub: System-K). Solo developer. Communicates in German during sessions.
Red-green colorblind — use yellow/magenta/blue palette, never red/green distinction.
Attribution for all data sources is non-negotiable (credited in GUI footer).

## Hard Rules — ALWAYS FOLLOW
1. **50KB file size limit**: No `.rs` source file may EVER exceed 50KB. If a file approaches this limit, STOP and refactor/split immediately. No exceptions. Check with `wc -c src/*.rs` before finishing work.
2. **i18n completeness**: Every new user-facing string requires keys in ALL 27 language files under `assets/lang/`. Never add a key to just one file.
3. **Inside-out globe convention**: The globe renders inside-out. See "Coordinate System" below.
4. **ureq v3 API**: Use `.into_body().read_to_vec().ok()` for binary, `.into_body().read_to_string()` for text. No `.into_string()`.
5. **Do NOT modify `<name>` tag in Cargo.toml** — it triggers an infinite fix loop due to a known glitch.

## Build & Run
```bash
cd G:\Orbis\orbis   # or wherever the repo lives
cargo build         # dev build
cargo run           # run
cargo build --release  # release build
```

## Current File Sizes (WATCH THESE)
```
main.rs           49.2 KB  ← CRITICAL! Near 50KB limit, needs split soon
geojson.rs        37.2 KB
live_source.rs    32.3 KB
gpu_init.rs       29.8 KB
tile.rs           29.5 KB
custom_source.rs  26.3 KB
wms.rs            22.4 KB
```

## Architecture Overview

### Rendering Pipeline
- `wgpu 27` for GPU rendering, `winit 0.30` for windowing, `egui 0.33` for GUI
- Globe rendered as inside-out sphere mesh
- Overlay layers composited as equirectangular textures on second render pass
- GeoJSON features rendered as egui painter shapes at screen-space positions

### Module Map (25 .rs files + gui/ submodule)
```
main.rs          — GpuState struct, render(), update_*, handle_gui_requests()
gpu_init.rs      — GpuState::new() (pipeline/texture/buffer creation)
app.rs           — App struct + ApplicationHandler + fn main() via run()
download.rs      — DownloadManager (background thread downloads)
camera.rs        — Camera (orbital, FOV, inside-out conventions)
mesh.rs          — Globe/quad mesh generation
texture.rs       — GpuTexture helpers
sun.rs           — Solar position calculation (Meeus)
layer.rs         — Layer + LayerStack + grid texture generation
provider.rs      — ProviderCatalog + LayerProvider trait + GridProvider
gibs.rs          — NASA GIBS providers
wms.rs           — Built-in WMS providers + Mercator reprojection
wms_caps.rs      — WMS GetCapabilities XML parser + CRS strategy
custom_source.rs — Custom WMS/REST config + RestFeedManager (M17)
geojson.rs       — GeoJSON parser + GeoLayer data model
marker.rs        — MarkerSystem (point rendering)
line.rs          — LineSystem (line rendering)
polygon.rs       — PolygonSystem (polygon triangulation + rendering)
label.rs         — LabelSystem (text labels + clustering)
live_source.rs   — LiveSourceManager (USGS earthquakes, OpenSky, GVP)
satellite.rs     — SatelliteTracker (SGP4, CelesTrak OMM)
planets.rs       — Planet positions (Meeus algorithms)
tile.rs          — Tile cache, download queue, compositor
i18n.rs          — 27-language runtime i18n system
settings.rs      — Persistent settings (JSON)
gui/             — 11-file GUI module:
  mod.rs         — Gui struct, draw_ui orchestrator
  state.rs       — GuiState, CustomSourceForm, marker/track structs
  custom.rs      — Custom source dialog + panel
  legend.rs      — Legend panel
  satellites.rs  — Satellite panel + overlay rendering
  settings.rs    — Display settings panel
  labels.rs      — Label overlay
  panels.rs      — Active layers + catalog
  geojson_panel.rs — GeoJSON layer panel
  time.rs        — Time control panel
  live.rs        — Live data sources panel
```

### Inside-Out Globe Coordinate System (CRITICAL)
- Mesh: `+X=180°W`, `-X=0°E`, `+Y=NorthPole`, `+Z=90°W`
- Camera: `eye = (d·cos(pitch)·sin(yaw), d·sin(pitch), d·cos(pitch)·cos(yaw))`
- Default `yaw=π/2` shows 0°E
- Labels/markers use negated x/z: `world = (-cos(lat)·cos(lon), sin(lat), -cos(lat)·sin(lon))`
- Camera follow: `target_yaw = π/2 - lon_rad`, `target_pitch = -lat_rad`
- Fresnel shader: Must use `abs(dot(view_dir, normal))` — dot is always ≤0 on inside-out geometry

### WMS Projection Handling
- CRS priority: EPSG:3857 (preferred, our reprojection is correct) > EPSG:4326 > CRS:84
- WMS 1.1.1: `SRS=EPSG:4326`, BBOX=`-180,-90,180,90` (lon,lat order)
- WMS 1.3.0: `CRS=EPSG:4326`, BBOX=`-90,-180,90,180` (lat,lon order)
- Cache stores raw server bytes; reprojection applied on EVERY load (including cache hits!)
- GetCapabilities auto-detection via `wms_caps.rs` (quick-xml streaming parser)

### Key Patterns
- egui 0.33 `ColorImage` requires `from_rgba_unmultiplied()` factory (not struct literal)
- Star shader: instanced billboard quads (6 vertices/star)
- Overlay markers/satellites/planets rendered as egui painter circles at screen-space positions
- Satellite tracking: parallel downloads (8 threads), SGP4, TEME→ECEF→geodetic (WGS84)
- Ground tracks: ±90min at 2min steps, stored as `Vec<Vec<Pos2>>` segments (occluded sections clipped)
- Planet accuracy target: ≤2° vs Astro-Seek. Sky sphere radius=50.0

## Dependencies (Cargo.toml)
wgpu 27, winit 0.30, egui 0.33, chrono 0.4, ureq 3, serde/serde_json,
glam 0.29, bytemuck, sgp4 2 (with serde feature), image 0.25,
rand 0.9, earcutr 0.4, rfd 0.15, quick-xml 0.37. Edition 2021.

## Data Assets
- `assets/data/stars.bin` — HYG v3.7, binary: `"STAR"` + u32 count + N×32-byte StarVertex
- `assets/geojson/` — 3 demo files (nuclear plants, tectonic plates, submarine cables)
- `assets/lang/` — 27 JSON language files
- `assets/shaders/` — globe.wgsl, star.wgsl
- `config/custom_sources.json` — User-defined WMS/REST sources

## Current Milestone: M17 (Custom Data Sources)
See PLAN.md for full roadmap. Key open items:
- M17d: REST→GeoJSON feeds ✅ (RestFeedManager done)
- M17p: WMS GetCapabilities auto-detection ✅ (layer dropdown + CRS auto-detect)
- M17p-3: Generic CRS reprojection via `proj4rs` (planned, not started)
- M17h-o: Format imports (KML, GPX, CSV, Shapefile, etc.)
- M17c: Custom XYZ tiles (deferred)
- M17g: Auth headers support

## Known Issues
- egui text fields have no right-click context menu (egui limitation). Ctrl+V works.
- main.rs at 49.2KB — needs refactoring split ASAP
- Regional WMS sources (e.g. Brandenburg ALKIS) need BBOX from GetCapabilities, not global
- Some WMS servers (Terrestris OSM) report EPSG:4326 but deliver Mercator-distorted images
