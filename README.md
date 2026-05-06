# 🌍 Orbis

**Real-time 3D Earth visualization with satellite tracking, NASA data, and 27 languages.**

Orbis is a lightweight, GPU-accelerated Earth viewer written in Rust.
It displays our planet with real-time solar illumination, live satellite tracking,
earthquake & volcano monitoring, cloud formations from NASA GIBS, a real star catalog,
and high-resolution tile zoom — all in real time.

![Orbis Screenshot](docs/screenshot.png)

---

## ✨ Features

### Globe & Map
- **3D Globe + 2D Map** — Orthographic and perspective projection, switchable with `M`
- **Real-time day/night cycle** — Astronomically accurate sun position (Jean Meeus)
- **Smooth terminator** — 6° twilight zone with smoothstep blending
- **Atmospheric glow** — Fresnel effect at the globe's rim
- **High-resolution tile zoom** — Sentinel-2 Cloudless, OpenStreetMap, and NASA GIBS tiles with smooth crossfade. Cancellation on dwell-detection, polar Mercator zoom-down to keep arctic views light, decoupled image decode for steady frametimes during pans.

### Satellite Data
- **NASA Blue Marble** — Day texture (vegetation, landmass, ocean)
- **NASA Black Marble** — Night texture (city lights)
- **NASA GIBS** — 30+ satellite imagery layers (VIIRS, MODIS, etc.) with automatic download + cache
- **External WMS** — DWD weather radar, OpenStreetMap, OpenTopoMap, GEBCO bathymetry. CRS auto-discovery via `GetCapabilities` so dishonest server declarations (e.g. Terrestris-style) self-correct.

### Live Data
- **USGS Earthquakes** — Real-time seismic data with magnitude-based coloring
- **OpenSky Network** — Live aircraft tracking with altitude coloring
- **Smithsonian GVP Volcanoes** — Holocene volcano data with eruption-year color coding

### Satellite Tracking
- **8 built-in satellites** — ISS, CSS Tianhe, Hubble, Landsat 8/9, Sentinel-2A, NOAA-20, Terra
- **SGP4 orbit propagation** — Real-time position from CelesTrak OMM data
- **Ground tracks** — ±90 minute past/future orbital paths (colorblind-accessible: yellow/magenta)
- **Camera follow mode** — Click a satellite to track it across the sky

### Sky
- **HYG Star Catalog** — 15,598 real stars (HYG v3.7) with accurate B-V color and brightness
- **Planet positions** — Mercury, Venus, Mars, Jupiter, Saturn, Moon (simplified Meeus, ≤2° accuracy)

### Vector Data
- **GeoJSON** — Bundled datasets (nuclear plants, tectonic plates, submarine cables) plus drag-drop / file dialog / URL loading
- **Shapefile (`.shp`)** — Esri Shapefile bundle (`.shp` + `.shx` + `.dbf` + `.prj`) with automatic projection detection. WGS84 and Web Mercator data reprojects to the globe; the `.prj` declaration is reconciled against the data's bounding box so mislabeled files self-correct. Points / lines / polygons / multipoints all supported. DBF attributes preserved as feature properties.
- **CSV / TSV** — Lat/lon point clouds with optional name and description columns. Auto-detected delimiter (`\t` / `;` / `,`), accepts both `.` and `,` decimal separators (German Excel works), recognises Darwin Core (GBIF) headers.
- **Interactive labels** — Clustering, expand/collapse, clipboard copy. Toggle with `G`.

### Custom Data Sources
- **Add Custom Source dialog** — Five source types: WMS, XYZ tiles, REST/GeoJSON, Shapefile, CSV
- **WMS auto-discovery** — `GetCapabilities` driven CRS preference (Mercator → 4326), per-source behaviour cached
- **Custom XYZ tiles** — User-supplied URL templates appear in the tile-source dropdown alongside built-ins
- **REST/GeoJSON polling** — Configurable refresh interval, optional auth headers
- **Drag and drop** — `.geojson` / `.json` / `.shp` / `.csv` / `.tsv` files dropped onto the window load as ad-hoc layers
- **Persistence** — All custom sources saved to `config/custom_sources.json`, auto-loaded at startup

### Interface
- **27 languages** — Runtime switching, auto-detect system locale
- **CJK font support** — Noto Sans font family for Chinese, Japanese, Korean, Hindi, Vietnamese
- **Legend panel** — Dynamic color scales for all active data sources
- **Persistent settings** — Camera, layers, language, tile cache — all saved automatically
- **Tile cache management** — Configurable size limit (100 MB–5 GB), age limit, clear button

## 🖥️ System Requirements

- **GPU:** Vulkan, DirectX 12, or Metal (software fallback via WARP/llvmpipe)
- **OS:** Windows 10/11, Linux (X11/Wayland, tested on CachyOS)
- **RAM:** ~300 MB (+ ~14 MB for CJK fonts)
- **Disk:** ~50 MB (application) + configurable tile cache (default 500 MB)
- **Network:** Optional (for GIBS layers, live data, satellite tracking)

## 🚀 Installation

### Precompiled Binaries

Download from the [Releases page](https://github.com/System-K/orbis/releases):

| Platform | File |
|----------|------|
| Windows (64-bit) | `orbis-windows-x86_64.zip` |
| Linux (64-bit) | `orbis-linux-x86_64.tar.gz` |

### Building from Source

Prerequisites: [Rust](https://rustup.rs/) (stable, edition 2021)

```bash
git clone https://github.com/System-K/orbis.git
cd orbis
cargo run --release
```

**Linux dependencies** (Debian/Ubuntu):
```bash
sudo apt install libx11-dev libxi-dev libxcursor-dev libxrandr-dev \
                 libwayland-dev libxkbcommon-dev pkg-config
```

**Arch:**
```bash
sudo pacman -S libx11 libxi libxcursor libxrandr wayland libxkbcommon
```

## ⌨️ Controls

| Key / Action | Function |
|--------------|----------|
| **Left mouse + drag** | Rotate globe (3D) / Pan (2D) |
| **Scroll wheel** | Zoom |
| **M** | Toggle 🌍 Globe ↔ 🗺 Map |
| **L** | Toggle layer panel |
| **T** | Toggle Live ↔ Manual time |
| **G** | Toggle GeoJSON labels |
| **K** | Toggle legend panel |
| **R** | Reset camera |
| **Esc** | Quit |

## 📂 Project Structure

```
orbis/
├── src/
│   ├── main.rs           # GpuState, render loop, top-level dispatch
│   ├── gpu_init.rs       # GpuState::new (pipelines, textures, buffers)
│   ├── app.rs            # winit ApplicationHandler + run()
│   ├── download.rs       # Background image download manager
│   ├── camera.rs         # Orbit camera (3D) + orthographic (2D)
│   ├── crs.rs            # Coordinate reference systems (WGS84, Mercator)
│   ├── mesh.rs           # Sphere, quad, star vertex generation
│   ├── texture.rs        # GPU texture loading + fallback
│   ├── sun.rs            # Astronomical sun position (Jean Meeus)
│   ├── planets.rs        # Planet positions (simplified Meeus)
│   ├── gibs.rs           # NASA GIBS WMS client + disk cache
│   ├── wms/              # WMS providers + CRS auto-discovery
│   │   ├── capabilities.rs   # GetCapabilities XML parser
│   │   ├── behavior.rs       # SourceBehavior + per-source cache
│   │   ├── reproject.rs      # Generic Crs → equirect resampler
│   │   └── probe.rs          # Self-consistency check (Terrestris-class)
│   ├── wms_caps.rs       # Capabilities-driven layer dropdown for the GUI
│   ├── tile/             # XYZ tile system (worker pool, cache, compositor)
│   ├── shp/              # Shapefile loader + projection reconciliation
│   ├── csv_import/       # CSV/TSV loader + column heuristics
│   ├── gui/              # egui panels, custom-source dialog, settings
│   ├── i18n.rs           # Internationalization (27 languages)
│   ├── layer.rs          # Overlay layer management
│   ├── provider.rs       # Data provider catalog
│   ├── settings.rs       # Persistent settings (JSON)
│   ├── custom_source.rs  # Custom data sources + Shapefile/CSV managers
│   ├── geojson.rs        # GeoJSON parser + feature model
│   ├── marker.rs         # Point marker rendering
│   ├── line.rs           # Line/polyline rendering
│   ├── polygon.rs        # Polygon rendering (earcut triangulation)
│   ├── label.rs          # Label layout + clustering
│   ├── live_source.rs    # Live data feeds (USGS, OpenSky, GVP)
│   └── satellite.rs      # Satellite tracking (SGP4, CelesTrak)
├── assets/
│   ├── data/             # HYG v3.7 star catalog (15,598 stars)
│   ├── fonts/            # Noto Sans font family (CJK, Arabic, etc.)
│   ├── geojson/          # Bundled demo datasets
│   ├── icon/             # Application icon
│   ├── lang/             # 27 language files (JSON)
│   ├── shaders/          # WGSL shaders
│   └── textures/         # Blue Marble + Black Marble
├── docs/                 # Design notes
│   ├── projection-honesty.md  # Trust-the-metadata pattern (WMS, .prj, etc.)
│   ├── csv-import.md          # CSV column synonyms + delimiter detection
│   └── xyz-tiles.md           # Custom XYZ tile-source format
├── cache/                # Tile + GIBS cache (auto-created)
├── config/               # settings.json + custom_sources.json (auto-created)
├── Cargo.toml
├── LICENSE               # GPL-3.0-or-later
├── PLAN.md               # Internal roadmap
└── README.md
```

## 🛠️ Technology

| Area | Technology |
|------|------------|
| Language | Rust (edition 2021) |
| GPU API | wgpu 27 (Vulkan / DX12 / Metal) |
| Windowing | winit 0.30 |
| GUI | egui 0.33 |
| Satellite data | NASA GIBS (WMTS), Sentinel-2 (EOX) |
| Orbit propagation | sgp4 crate (CelesTrak OMM) |
| Star catalog | HYG v3.7 (CC BY-SA 4.0) |
| Vector formats | GeoJSON (serde_json), Shapefile (`shapefile`), CSV (`csv`) |
| WMS / OGC | quick-xml (GetCapabilities) |
| Shader language | WGSL |

### Rendering Pipeline

1. **Stars** — Instanced billboard quads from HYG catalog, B-V color mapped
2. **Planets** — Geocentric positions via simplified Meeus, rendered as egui circles
3. **Globe** — UV sphere with day/night blending + Fresnel atmosphere
4. **Tile zoom** — Sentinel-2/OSM tiles composited into equirectangular buffer
5. **Overlays** — Multi-pass alpha blending (GIBS, grid, clouds)
6. **GeoJSON** — Polygons, lines, markers rendered via custom pipelines
7. **Satellites** — Ground tracks + markers as egui painter shapes
8. **GUI** — egui rendered via wgpu

## 🌐 Data Sources & Attribution

| Source | Data | License |
|--------|------|---------|
| [NASA GIBS / ESDIS](https://earthdata.nasa.gov/gibs) | Satellite imagery (VIIRS, MODIS, etc.) | Public domain |
| [NASA Visible Earth](https://visibleearth.nasa.gov/) | Blue Marble (day texture) | Public domain |
| [NASA Earth Observatory](https://earthobservatory.nasa.gov/) | Black Marble (night lights) | Public domain |
| [Sentinel-2 Cloudless (EOX)](https://s2maps.eu/) | High-res satellite mosaic | CC BY-NC-SA 4.0 |
| [OpenStreetMap](https://www.openstreetmap.org/) | Map tiles | ODbL |
| [USGS Earthquake Hazards](https://earthquake.usgs.gov/) | Real-time seismic data | Public domain |
| [OpenSky Network](https://opensky-network.org/) | Live aircraft positions | CC BY-NC 4.0 |
| [Smithsonian GVP](https://volcano.si.edu/) | Holocene volcano database | — |
| [CelesTrak](https://celestrak.org/) | Satellite orbital elements (OMM) | — |
| [HYG Star Database](https://github.com/astronexus/HYG-Database) | Star catalog (v3.7) | CC BY-SA 4.0 |
| [GeoNuclearData](https://github.com/cristianst85/GeoNuclearData) | Nuclear power plants | CC BY 4.0 |
| [TeleGeography](https://github.com/telegeography/www.submarinecablemap.com) | Submarine cable map | — |
| [Peter Bird (2003)](https://github.com/fraxen/tectonicplates) | Tectonic plate boundaries | — |

## 🌍 Supported Languages

Catalan, Chinese (Simplified), Chinese (Traditional), Czech, Danish, Dutch,
English, Finnish, French, German, Greek, Hindi, Hungarian, Indonesian, Italian,
Japanese, Korean, Norwegian, Polish, Portuguese, Romanian, Russian, Spanish,
Swedish, Turkish, Ukrainian, Vietnamese

*Arabic and Hebrew planned (pending egui RTL support).*

## ☕ Support

If you find Orbis useful, consider supporting development:

- [Ko-Fi](https://ko-fi.com/yveskuehn)

## 📋 License

Orbis is free software under the **GNU General Public License v3.0 or later**.

See [LICENSE](LICENSE) for the full license text.

```
Copyright (C) 2026  Yves Kühn / System-K

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.
```

## 🤝 Contributing

Contributions are welcome! Please open an issue or a pull request
on [GitHub](https://github.com/System-K/orbis).

---

*Orbis — Because Earth is beautiful, and so is data.* 🌎
