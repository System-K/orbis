// =============================================================================
// Orbis — Tile System (M16a)
// =============================================================================
// XYZ slippy map tile infrastructure for high-resolution zoom.
//
// Provides:
// - Tile coordinate math (lat/lon/zoom → tile x/y)
// - Pluggable tile sources with URL templates
// - LRU disk cache with configurable size and age limits
// - Background tile fetching
//
// Tile naming follows the OpenStreetMap / "Slippy Map" convention:
//   URL: .../{z}/{x}/{y}.png
//   z = zoom level (0 = whole world, 19 = street level)
//   x = column (0..2^z - 1, left to right)
//   y = row (0..2^z - 1, top to bottom)
//
// Reference: https://wiki.openstreetmap.org/wiki/Slippy_map_tilenames
// =============================================================================

use std::path::{Path, PathBuf};
use std::time::{SystemTime, Duration};

// =============================================================================
// Tile Coordinates
// =============================================================================

/// A tile address in the XYZ grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub z: u32,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    /// Converts geographic coordinates to a tile coordinate at a given zoom level.
    ///
    /// Uses the Web Mercator (EPSG:3857) projection, the de facto standard
    /// for slippy map tiles (OSM, Google Maps, etc.).
    ///
    /// lat: -85.0511..+85.0511 (Mercator limit)
    /// lon: -180..+180
    /// zoom: 0..19
    pub fn from_lat_lon(lat_deg: f64, lon_deg: f64, zoom: u32) -> Self {
        let n = (1u64 << zoom) as f64;
        let lat_rad = lat_deg.to_radians();

        let x = ((lon_deg + 180.0) / 360.0 * n).floor() as u32;
        let y = ((1.0 - lat_rad.tan().asinh() / std::f64::consts::PI) / 2.0 * n).floor() as u32;

        let max = (1u32 << zoom).saturating_sub(1);
        TileCoord {
            z: zoom,
            x: x.min(max),
            y: y.min(max),
        }
    }

    /// Returns the geographic bounding box of this tile as (north, south, east, west).
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let n = (1u64 << self.z) as f64;

        let lon_west = self.x as f64 / n * 360.0 - 180.0;
        let lon_east = (self.x + 1) as f64 / n * 360.0 - 180.0;

        let lat_north = (std::f64::consts::PI * (1.0 - 2.0 * self.y as f64 / n))
            .sinh()
            .atan()
            .to_degrees();
        let lat_south = (std::f64::consts::PI * (1.0 - 2.0 * (self.y + 1) as f64 / n))
            .sinh()
            .atan()
            .to_degrees();

        (lat_north, lat_south, lon_east, lon_west)
    }

    /// Returns the center lat/lon of this tile.
    #[allow(dead_code)] // Utility for future tile prioritization
    pub fn center(&self) -> (f64, f64) {
        let (n, s, e, w) = self.bounds();
        ((n + s) / 2.0, (e + w) / 2.0)
    }

    /// Returns all tiles visible in a bounding box at a given zoom level.
    pub fn tiles_in_view(
        lat_north: f64, lat_south: f64,
        lon_west: f64, lon_east: f64,
        zoom: u32,
    ) -> Vec<TileCoord> {
        let tl = TileCoord::from_lat_lon(lat_north, lon_west, zoom);
        let br = TileCoord::from_lat_lon(lat_south, lon_east, zoom);

        let mut tiles = Vec::new();
        let max = (1u32 << zoom).saturating_sub(1);

        // Handle date-line wraparound
        let (x_start, x_end) = if tl.x <= br.x {
            (tl.x, br.x)
        } else {
            // Wraps around date line — simplified: just use full range
            (0, max)
        };

        for y in tl.y..=br.y.min(max) {
            for x in x_start..=x_end.min(max) {
                tiles.push(TileCoord { z: zoom, x, y });
            }
        }
        tiles
    }
}

impl std::fmt::Display for TileCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

// =============================================================================
// Tile Sources
// =============================================================================

/// A tile source with URL template and metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used progressively across M16 substeps
pub struct TileSource {
    /// Unique identifier (e.g. "osm", "sentinel2", "gibs_truecolor")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// URL template with {z}, {x}, {y} placeholders.
    /// Optional: {s} for subdomain rotation, {date} for GIBS.
    pub url_template: String,
    /// Subdomains for load balancing (e.g. ["a", "b", "c"])
    pub subdomains: Vec<String>,
    /// Maximum zoom level supported by this source
    pub max_zoom: u32,
    /// Tile image format
    pub format: TileFormat,
    /// Attribution string (mandatory for display)
    pub attribution: String,
    /// Required HTTP User-Agent (some servers require identification)
    pub user_agent: Option<String>,
}

/// Tile image format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TileFormat {
    Png,
    Jpg,
}

impl TileSource {
    /// Builds the download URL for a specific tile.
    pub fn tile_url(&self, coord: &TileCoord, date: Option<&str>) -> String {
        let mut url = self.url_template
            .replace("{z}", &coord.z.to_string())
            .replace("{x}", &coord.x.to_string())
            .replace("{y}", &coord.y.to_string());

        // Subdomain rotation based on tile coordinates
        if !self.subdomains.is_empty() && url.contains("{s}") {
            let idx = (coord.x + coord.y) as usize % self.subdomains.len();
            url = url.replace("{s}", &self.subdomains[idx]);
        }

        // Date substitution (for GIBS)
        if let Some(d) = date {
            url = url.replace("{date}", d);
        }

        url
    }

    /// File extension for cached tiles.
    pub fn extension(&self) -> &str {
        match self.format {
            TileFormat::Png => "png",
            TileFormat::Jpg => "jpg",
        }
    }
}

/// Built-in tile sources.
pub fn builtin_tile_sources() -> Vec<TileSource> {
    vec![
        TileSource {
            id: "sentinel2".into(),
            name: "Sentinel-2 Cloudless".into(),
            url_template: "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2021_3857/default/GoogleMapsCompatible/{z}/{y}/{x}.jpg".into(),
            subdomains: vec![],
            max_zoom: 14,
            format: TileFormat::Jpg,
            attribution: "Sentinel-2 cloudless by EOX (CC BY-NC-SA 4.0)".into(),
            user_agent: None,
        },
        TileSource {
            id: "osm".into(),
            name: "OpenStreetMap".into(),
            url_template: "https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            subdomains: vec!["a".into(), "b".into(), "c".into()],
            max_zoom: 19,
            format: TileFormat::Png,
            attribution: "© OpenStreetMap contributors".into(),
            user_agent: Some("Orbis/0.1 (https://github.com/System-K/orbis)".into()),
        },
        TileSource {
            id: "gibs_truecolor".into(),
            name: "NASA GIBS True Color".into(),
            url_template: "https://gibs-{s}.earthdata.nasa.gov/wmts/epsg3857/best/VIIRS_SNPP_CorrectedReflectance_TrueColor/default/{date}/GoogleMapsCompatible_Level9/{z}/{y}/{x}.jpg".into(),
            subdomains: vec!["a".into(), "b".into(), "c".into()],
            max_zoom: 9,
            format: TileFormat::Jpg,
            attribution: "NASA GIBS / ESDIS".into(),
            user_agent: None,
        },
    ]
}

// =============================================================================
// Disk Tile Cache (LRU)
// =============================================================================

/// Configuration for the tile disk cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Root directory for cached tiles
    pub cache_dir: PathBuf,
    /// Maximum cache size in bytes (default: 500 MB)
    pub max_size_bytes: u64,
    /// Maximum age of cached tiles (default: 7 days)
    pub max_age: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cache_dir: crate::app_path("cache/tiles"),
            max_size_bytes: 500 * 1024 * 1024, // 500 MB
            max_age: Duration::from_secs(7 * 24 * 3600), // 7 days
        }
    }
}

/// On-disk tile cache with LRU eviction.
///
/// Thread-safe: config is behind a RwLock so it can be updated
/// while download threads hold a shared reference.
pub struct TileCache {
    config: std::sync::RwLock<CacheConfig>,
}

impl TileCache {
    pub fn new(config: CacheConfig) -> Self {
        if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
            log::warn!("Could not create tile cache directory: {}", e);
        }
        Self { config: std::sync::RwLock::new(config) }
    }

    /// Returns the file path for a cached tile.
    fn tile_path(&self, source_id: &str, coord: &TileCoord, ext: &str) -> PathBuf {
        let cfg = self.config.read().unwrap();
        cfg.cache_dir
            .join(source_id)
            .join(coord.z.to_string())
            .join(coord.x.to_string())
            .join(format!("{}.{}", coord.y, ext))
    }

    /// Tries to read a tile from the cache.
    ///
    /// Returns None if the tile is not cached or has expired.
    /// Updates the file's access time on hit (for LRU).
    pub fn get(&self, source_id: &str, coord: &TileCoord, ext: &str) -> Option<Vec<u8>> {
        let path = self.tile_path(source_id, coord, ext);
        if !path.exists() {
            return None;
        }

        // Check age
        let max_age = self.config.read().unwrap().max_age;
        if let Ok(metadata) = path.metadata() {
            if let Ok(modified) = metadata.modified() {
                if let Ok(age) = SystemTime::now().duration_since(modified) {
                    if age > max_age {
                        // Expired — remove and return miss
                        let _ = std::fs::remove_file(&path);
                        return None;
                    }
                }
            }
        }

        // Read and "touch" (update mtime for LRU ordering)
        match std::fs::read(&path) {
            Ok(data) => {
                // Touch the file to update mtime (for LRU ordering)
                if let Ok(file) = std::fs::File::open(&path) {
                    let times = std::fs::FileTimes::new()
                        .set_modified(SystemTime::now());
                    let _ = file.set_times(times);
                }
                Some(data)
            }
            Err(_) => None,
        }
    }

    /// Stores a tile in the cache.
    pub fn put(&self, source_id: &str, coord: &TileCoord, ext: &str, data: &[u8]) {
        let path = self.tile_path(source_id, coord, ext);

        // Create parent directories
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Err(e) = std::fs::write(&path, data) {
            log::warn!("Failed to cache tile {}/{}: {}", source_id, coord, e);
        }
    }

    /// Returns the total size of the cache in bytes.
    pub fn total_size_bytes(&self) -> u64 {
        let cfg = self.config.read().unwrap();
        dir_size_recursive(&cfg.cache_dir)
    }

    /// Runs LRU eviction if the cache exceeds max_size_bytes.
    ///
    /// Collects all cached files, sorts by modification time (oldest first),
    /// and deletes until total size is under the limit.
    pub fn evict_if_needed(&self) {
        let cfg = self.config.read().unwrap();
        let current_size = dir_size_recursive(&cfg.cache_dir);
        if current_size <= cfg.max_size_bytes {
            return;
        }

        log::info!(
            "Tile cache eviction: {:.1} MB / {:.1} MB limit",
            current_size as f64 / (1024.0 * 1024.0),
            cfg.max_size_bytes as f64 / (1024.0 * 1024.0),
        );

        // Collect all files with their size and mtime
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        collect_files_recursive(&cfg.cache_dir, &mut files);
        let max_size = cfg.max_size_bytes;
        drop(cfg); // release lock before doing I/O

        // Sort by mtime: oldest first (LRU eviction order)
        files.sort_by_key(|f| f.2);

        let mut freed: u64 = 0;
        let target = current_size - max_size;

        for (path, size, _) in &files {
            if freed >= target {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                freed += size;
            }
        }

        log::info!("Tile cache: freed {:.1} MB", freed as f64 / (1024.0 * 1024.0));
    }

    /// Removes all cached tiles.
    pub fn clear(&self) {
        let cache_dir = self.config.read().unwrap().cache_dir.clone();
        if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
            log::warn!("Failed to clear tile cache: {}", e);
        }
        let _ = std::fs::create_dir_all(&cache_dir);
        log::info!("Tile cache cleared");
    }

    /// Updates the cache configuration (e.g. after settings change).
    pub fn update_config(&self, config: CacheConfig) {
        if let Ok(mut cfg) = self.config.write() {
            *cfg = config;
        }
    }
}

/// Recursively computes total file size in a directory.
fn dir_size_recursive(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                total += dir_size_recursive(&entry_path);
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Recursively collects all files with size and mtime.
fn collect_files_recursive(path: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                collect_files_recursive(&entry_path, out);
            } else if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((entry_path, meta.len(), mtime));
            }
        }
    }
}

// =============================================================================
// Tile Fetcher (download with cache)
// =============================================================================

/// Fetches a tile: cache hit → return immediately, miss → download and cache.
///
/// This is a blocking call — intended to be run from a background thread.
pub fn fetch_tile(
    source: &TileSource,
    coord: &TileCoord,
    cache: &TileCache,
    date: Option<&str>,
) -> Result<Vec<u8>, String> {
    let ext = source.extension();

    // Cache hit?
    if let Some(data) = cache.get(&source.id, coord, ext) {
        return Ok(data);
    }

    // Cache miss → download
    let url = source.tile_url(coord, date);

    let mut request = ureq::get(&url);
    if let Some(ua) = &source.user_agent {
        request = request.header("User-Agent", ua);
    }

    let response = request
        .call()
        .map_err(|e| format!("Tile download failed {}: {}", url, e))?;

    let data = response
        .into_body()
        .read_to_vec()
        .map_err(|e| format!("Tile read failed {}: {}", url, e))?;

    if data.is_empty() {
        return Err(format!("Empty tile response from {}", url));
    }

    // Store in cache
    cache.put(&source.id, coord, ext, &data);

    Ok(data)
}

// =============================================================================
// Tile Compositor (M16d/e) — stitches tiles into equirectangular buffer
// =============================================================================

/// Composites downloaded tiles into a single equirectangular RGBA buffer
/// that can be uploaded as an overlay texture.
///
/// The buffer uses equirectangular projection (matching Orbis' UV convention):
///   u=0 → 180°W, u=1 → 180°E, v=0 → 90°N, v=1 → 90°S
///
/// Web Mercator tiles are blitted with approximate UV mapping.
/// The error is negligible at tile-level scales (< 0.5° for zoom ≥ 3).
pub struct TileCompositor {
    /// RGBA compositing buffer
    buffer: Vec<u8>,
    /// Buffer dimensions
    pub width: u32,
    pub height: u32,
    /// Tiles that have been composited into the current buffer
    composited: HashSet<TileCoord>,
    /// Current zoom level (buffer is cleared when zoom changes)
    pub current_zoom: u32,
    /// Whether the buffer changed since last GPU upload
    pub dirty: bool,
    /// Whether any tiles have been composited at all
    pub has_content: bool,
}

impl TileCompositor {
    /// Creates a new compositor with the given buffer size.
    ///
    /// Recommended: 4096x2048 for good quality, 2048x1024 for less memory.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            buffer: vec![0u8; (width * height * 4) as usize],
            width,
            height,
            composited: HashSet::new(),
            current_zoom: 0,
            dirty: false,
            has_content: false,
        }
    }

    /// Clears the buffer and resets for a new zoom level.
    pub fn reset(&mut self, zoom: u32) {
        self.buffer.fill(0);
        self.composited.clear();
        self.current_zoom = zoom;
        self.dirty = true;
        self.has_content = false;
    }

    /// Returns true if this tile has already been composited.
    pub fn has_tile(&self, coord: &TileCoord) -> bool {
        self.composited.contains(coord)
    }

    /// Composites a tile image into the equirectangular buffer.
    ///
    /// Decodes the image (JPEG/PNG), computes its UV bounds, and blits
    /// the pixels into the correct position in the buffer.
    pub fn composite_tile(&mut self, coord: &TileCoord, image_data: &[u8]) -> bool {
        if self.composited.contains(coord) {
            return false; // already composited
        }

        // Decode image
        let img = match image::load_from_memory(image_data) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                log::debug!("Failed to decode tile {}: {}", coord, e);
                return false;
            }
        };
        let (tw, th) = img.dimensions();

        // Compute tile bounds in geographic coordinates
        let (lat_n, lat_s, lon_e, lon_w) = coord.bounds();

        // Map to equirectangular UV
        // u = (lon + 180) / 360   →  pixel_x = u * width
        // v = (90 - lat) / 180    →  pixel_y = v * height
        let u_left = ((lon_w + 180.0) / 360.0) as f32;
        let u_right = ((lon_e + 180.0) / 360.0) as f32;
        let v_top = ((90.0 - lat_n) / 180.0) as f32;
        let v_bottom = ((90.0 - lat_s) / 180.0) as f32;

        let px_left = (u_left * self.width as f32) as i32;
        let px_right = (u_right * self.width as f32) as i32;
        let px_top = (v_top * self.height as f32) as i32;
        let px_bottom = (v_bottom * self.height as f32) as i32;

        let dest_w = (px_right - px_left).max(1) as u32;
        let dest_h = (px_bottom - px_top).max(1) as u32;

        // Blit tile into buffer with nearest-neighbor scaling
        for dy in 0..dest_h {
            for dx in 0..dest_w {
                let dest_x = px_left as u32 + dx;
                let dest_y = px_top as u32 + dy;

                if dest_x >= self.width || dest_y >= self.height {
                    continue;
                }

                // Source pixel (scale from dest rect to tile rect)
                let sx = (dx as f32 / dest_w as f32 * tw as f32) as u32;
                let sy = (dy as f32 / dest_h as f32 * th as f32) as u32;
                let sx = sx.min(tw - 1);
                let sy = sy.min(th - 1);

                let src_idx = ((sy * tw + sx) * 4) as usize;
                let dst_idx = ((dest_y * self.width + dest_x) * 4) as usize;

                self.buffer[dst_idx..dst_idx + 4]
                    .copy_from_slice(&img.as_raw()[src_idx..src_idx + 4]);
            }
        }

        self.composited.insert(*coord);
        self.dirty = true;
        self.has_content = true;

        log::debug!("Composited tile {} ({}x{} → {}x{} at {},{} in buffer)",
            coord, tw, th, dest_w, dest_h, px_left, px_top);
        true
    }

    /// Returns the raw RGBA buffer for GPU upload.
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// Marks the buffer as uploaded (not dirty anymore).
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Number of tiles composited in current buffer.
    #[allow(dead_code)] // Used in debug println
    pub fn tile_count(&self) -> usize {
        self.composited.len()
    }
}

/// Computes an appropriate tile zoom level from camera distance.
///
/// Maps the Orbis camera distance range (2.0..15.0) to tile zoom levels.
/// Returns 0 if the camera is too far out for tiles to be useful.
pub fn zoom_from_distance(distance: f32) -> u32 {
    // Steeper mapping so close zoom gives usable tile detail:
    // distance=15 (max out):  zoom 1
    // distance=8  (default):  zoom 2
    // distance=4:             zoom 4
    // distance=3:             zoom 5
    // distance=2  (max in):   zoom 6
    let zoom = (15.0_f32 / distance).log2() * 1.8 + 1.0;
    (zoom.floor() as u32).clamp(0, 9)
}

/// Computes the visible lat/lon bounding box from camera yaw/pitch/distance.
///
/// Returns (lat_north, lat_south, lon_west, lon_east).
/// Uses a simplified FOV-based estimate.
pub fn visible_bounds(
    yaw: f32, pitch: f32, distance: f32, fov_y: f32, aspect: f32,
) -> (f64, f64, f64, f64) {
    // Orbis convention: yaw=π/2 → looking at 0°E, pitch=0 → equator
    // Camera sees the FAR side (inside-out), so center = opposite of eye
    let center_lon = (std::f32::consts::FRAC_PI_2 - yaw).to_degrees() as f64;
    let center_lat = (-pitch).to_degrees() as f64; // negated for inside-out

    // Angular extent based on distance and FOV
    // At distance=2 (close), ~30° visible. At distance=8, ~90° visible.
    let half_fov_deg = (fov_y / 2.0).to_degrees() as f64;
    let angular_radius = (half_fov_deg * distance as f64 / 2.0).min(90.0);
    let angular_width = angular_radius * aspect as f64;

    let lat_n = (center_lat + angular_radius).min(85.05);
    let lat_s = (center_lat - angular_radius).max(-85.05);
    let lon_w = center_lon - angular_width;
    let lon_e = center_lon + angular_width;

    // Normalize longitude to -180..180
    let normalize_lon = |l: f64| -> f64 {
        let mut l = l % 360.0;
        if l > 180.0 { l -= 360.0; }
        if l < -180.0 { l += 360.0; }
        l
    };

    (lat_n, lat_s, normalize_lon(lon_w), normalize_lon(lon_e))
}

// =============================================================================
// Background Tile Download Queue (M16c)
// =============================================================================

use std::collections::HashSet;
use std::sync::{mpsc, Arc};

/// A completed tile download ready for GPU upload.
pub struct TileReady {
    pub coord: TileCoord,
    pub source_id: String,
    pub data: Vec<u8>,
}

/// Manages background tile downloads with deduplication and priority.
///
/// Tiles are requested via `request()`, downloaded in a thread pool,
/// and completed tiles are polled via `poll()` each frame.
pub struct TileDownloadQueue {
    /// Receiver for completed downloads
    rx: mpsc::Receiver<TileReady>,
    /// Sender cloned into worker threads
    tx: mpsc::Sender<TileReady>,
    /// Tiles currently being downloaded (for deduplication)
    in_flight: HashSet<(String, TileCoord)>,
    /// Shared reference to the tile cache
    cache: Arc<TileCache>,
    /// Available tile sources
    sources: Vec<TileSource>,
    /// Maximum concurrent downloads
    max_concurrent: usize,
}

impl TileDownloadQueue {
    pub fn new(cache: Arc<TileCache>, sources: Vec<TileSource>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            rx,
            tx,
            in_flight: HashSet::new(),
            cache,
            sources,
            max_concurrent: 8,
        }
    }

    /// Request tiles for download. Duplicates and already-cached tiles are skipped.
    ///
    /// `requests` should be sorted by priority (center of view first).
    /// `date` is optional (used for GIBS date-dependent layers).
    pub fn request(&mut self, source_id: &str, tiles: &[TileCoord], date: Option<&str>) {
        let source = match self.sources.iter().find(|s| s.id == source_id) {
            Some(s) => s,
            None => {
                log::warn!("Unknown tile source: {}", source_id);
                return;
            }
        };

        let date_owned = date.map(|d| d.to_string());

        for &coord in tiles {
            // Skip if already in flight
            let key = (source_id.to_string(), coord);
            if self.in_flight.contains(&key) {
                continue;
            }

            // Skip if already cached
            if self.cache.get(&source.id, &coord, source.extension()).is_some() {
                continue;
            }

            // Respect concurrency limit
            if self.in_flight.len() >= self.max_concurrent {
                break;
            }

            self.in_flight.insert(key);

            // Spawn download thread
            let tx = self.tx.clone();
            let source_clone = source.clone();
            let cache_clone = Arc::clone(&self.cache);
            let date_clone = date_owned.clone();

            std::thread::spawn(move || {
                let result = fetch_tile(
                    &source_clone,
                    &coord,
                    &cache_clone,
                    date_clone.as_deref(),
                );

                if let Ok(data) = result {
                    let _ = tx.send(TileReady {
                        coord,
                        source_id: source_clone.id.clone(),
                        data,
                    });
                } else if let Err(e) = result {
                    println!("[TILE] Download FAILED {}/{}: {}", source_clone.id, coord, e);
                }
            });
        }
    }

    /// Polls for completed tile downloads. Call once per frame.
    ///
    /// Returns all tiles that finished downloading since the last poll.
    pub fn poll(&mut self) -> Vec<TileReady> {
        let mut ready = Vec::new();
        while let Ok(tile) = self.rx.try_recv() {
            self.in_flight.remove(&(tile.source_id.clone(), tile.coord));
            ready.push(tile);
        }
        ready
    }

    /// Number of tiles currently being downloaded.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Cancels all pending requests (tiles already in-flight will still complete).
    #[allow(dead_code)] // Used when switching tile sources
    pub fn clear_requests(&mut self) {
        self.in_flight.clear();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_coord_london() {
        // London (51.5°N, -0.1°W) at zoom 10
        let tile = TileCoord::from_lat_lon(51.5, -0.1, 10);
        assert_eq!(tile.z, 10);
        // Known: London at z10 is approximately x=511, y=340
        assert!((tile.x as i32 - 511).abs() <= 2, "x={}", tile.x);
        assert!((tile.y as i32 - 340).abs() <= 2, "y={}", tile.y);
    }

    #[test]
    fn test_tile_coord_zero_zero() {
        // Null Island (0°N, 0°E) at zoom 0
        let tile = TileCoord::from_lat_lon(0.0, 0.0, 0);
        assert_eq!(tile.z, 0);
        assert_eq!(tile.x, 0);
        assert_eq!(tile.y, 0);
    }

    #[test]
    fn test_tile_coord_bounds_roundtrip() {
        let tile = TileCoord { z: 5, x: 16, y: 11 };
        let (n, s, e, w) = tile.bounds();
        // Center should be within bounds
        let (clat, clon) = tile.center();
        assert!(clat >= s && clat <= n, "center lat {} not in [{}, {}]", clat, s, n);
        assert!(clon >= w && clon <= e, "center lon {} not in [{}, {}]", clon, w, e);
    }

    #[test]
    fn test_tiles_in_view() {
        // Small area at zoom 3 — should return a handful of tiles
        let tiles = TileCoord::tiles_in_view(55.0, 45.0, 5.0, 15.0, 3);
        assert!(!tiles.is_empty());
        assert!(tiles.len() < 20, "too many tiles: {}", tiles.len());
        for t in &tiles {
            assert_eq!(t.z, 3);
        }
    }

    #[test]
    fn test_tile_url_osm() {
        let sources = builtin_tile_sources();
        let osm = sources.iter().find(|s| s.id == "osm").unwrap();
        let coord = TileCoord { z: 10, x: 511, y: 340 };
        let url = osm.tile_url(&coord, None);
        assert!(url.contains("/10/511/340.png"), "url={}", url);
        assert!(url.contains("tile.openstreetmap.org"), "url={}", url);
    }

    #[test]
    fn test_tile_url_sentinel2() {
        let sources = builtin_tile_sources();
        let s2 = sources.iter().find(|s| s.id == "sentinel2").unwrap();
        let coord = TileCoord { z: 8, x: 134, y: 86 };
        let url = s2.tile_url(&coord, None);
        assert!(url.contains("/8/86/134.jpg"), "url={}", url);
    }

    #[test]
    fn test_tile_url_gibs() {
        let sources = builtin_tile_sources();
        let gibs = sources.iter().find(|s| s.id == "gibs_truecolor").unwrap();
        let coord = TileCoord { z: 5, x: 16, y: 11 };
        let url = gibs.tile_url(&coord, Some("2026-03-17"));
        assert!(url.contains("2026-03-17"), "url={}", url);
        assert!(url.contains("/5/11/16.jpg"), "url={}", url);
    }
}
