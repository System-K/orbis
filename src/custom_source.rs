// =============================================================================
// Orbis — Custom Data Sources (M17)
// =============================================================================
// Lets users define their own WMS, XYZ tile, and REST/GeoJSON data sources
// via a JSON configuration file (config/custom_sources.json).
//
// Architecture:
// - `CustomSourceConfig`: serde-driven definition of a custom data source
// - `SourceType`: enum discriminator (WMS, XYZ, REST)
// - `load_custom_sources()`: reads config, creates real providers
// - Sources are registered in the ProviderCatalog alongside built-in ones
//
// The config file is separate from settings.json so users can easily
// share, copy, or version-control their source definitions.
// =============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::geojson::GeoLayer;
use crate::provider::{LayerProvider, ProviderCategory, ProviderInfo};

/// Path to the custom sources configuration file.
const CUSTOM_SOURCES_FILE: &str = "config/custom_sources.json";

// =============================================================================
// Configuration Data Model
// =============================================================================

/// Root structure of the custom_sources.json file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomSourcesConfig {
    /// Schema version (for future migrations)
    pub version: u32,
    /// List of user-defined data sources
    pub sources: Vec<CustomSourceConfig>,
}

impl Default for CustomSourcesConfig {
    fn default() -> Self {
        Self {
            version: 1,
            sources: Vec::new(),
        }
    }
}

/// A single user-defined data source.
///
/// JSON structure uses type-specific nested objects:
/// ```json
/// {
///   "id": "my_wms",
///   "name": "My Layer",
///   "type": "wms",
///   "category": "weather",
///   "wms": { "base_url": "...", "layer_name": "..." }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomSourceConfig {
    /// Unique identifier (auto-generated or user-chosen)
    pub id: String,
    /// Display name shown in the catalog
    pub name: String,
    /// Source type determines which pipeline handles this source
    #[serde(rename = "type")]
    pub source_type: SourceType,
    /// Category for catalog grouping
    #[serde(default = "default_category")]
    pub category: String,
    /// Attribution text (required for proper data crediting)
    #[serde(default)]
    pub attribution: String,
    /// Default opacity when adding as layer (0.0–1.0)
    #[serde(default = "default_opacity")]
    pub default_opacity: f32,
    /// Whether this source is enabled (shown in catalog)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional HTTP headers for authentication (API-Key, Bearer, etc.)
    /// Example: {"Authorization": "Bearer abc123"} or {"X-Api-Key": "mykey"}
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// WMS-specific config (present when type = "wms")
    #[serde(default)]
    pub wms: Option<WmsConfig>,
    /// XYZ tile config (present when type = "xyz")
    #[serde(default)]
    pub xyz: Option<XyzConfig>,
    /// REST/GeoJSON config (present when type = "rest")
    #[serde(default)]
    pub rest: Option<RestConfig>,
    /// Shapefile config (present when type = "shapefile")
    #[serde(default)]
    pub shapefile: Option<ShapefileConfig>,
    /// CSV config (present when type = "csv")
    #[serde(default)]
    pub csv: Option<CsvConfig>,
}

/// Source type discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    /// OGC Web Map Service — fetches equirectangular images
    Wms,
    /// XYZ slippy map tiles — fetches individual tiles
    Xyz,
    /// REST API returning GeoJSON — fetches point/line/polygon data
    Rest,
    /// Local Esri Shapefile bundle (.shp + .shx + .dbf + optional .prj)
    Shapefile,
    /// Local CSV/TSV file with lat/lon point coordinates
    Csv,
}

/// WMS-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WmsConfig {
    /// WMS base URL (e.g. "https://maps.dwd.de/geoserver/wms")
    pub base_url: String,
    /// WMS layer name (LAYERS parameter)
    pub layer_name: String,
    /// Image format: "image/png" or "image/jpeg"
    #[serde(default = "default_png_format")]
    pub format: String,
    /// Whether to request transparent background
    #[serde(default)]
    pub transparent: bool,
    /// Whether this layer supports TIME parameter
    #[serde(default)]
    pub uses_time: bool,
    /// Whether the source returns Web Mercator that needs reprojection
    #[serde(default)]
    pub reproject_mercator: bool,
    /// WMS version (default: "1.3.0")
    #[serde(default = "default_wms_version")]
    pub wms_version: String,
}

/// XYZ tile source configuration (M17c — placeholder for now).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XyzConfig {
    /// URL template with {z}, {x}, {y} placeholders
    pub url_template: String,
    /// Maximum zoom level
    #[serde(default = "default_max_zoom")]
    pub max_zoom: u32,
    /// Subdomains for load balancing (e.g. ["a", "b", "c"])
    #[serde(default)]
    pub subdomains: Vec<String>,
    /// Tile format: "png" or "jpg"
    #[serde(default = "default_png_ext")]
    pub format: String,
}

/// REST/GeoJSON feed configuration (M17d — placeholder for now).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestConfig {
    /// API endpoint URL
    pub url: String,
    /// Auto-refresh interval in seconds (0 = no refresh)
    #[serde(default = "default_refresh")]
    pub refresh_secs: u64,
    /// Expected response format
    #[serde(default = "default_geojson")]
    pub response_format: String,
}

/// Shapefile source configuration. Points at a local .shp file; the loader
/// reads the matching .shx/.dbf/.prj sidecars from the same directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapefileConfig {
    /// Absolute or relative path to the .shp file on disk.
    pub path: String,
}

/// CSV source configuration. Points at a local .csv (or .tsv) file with
/// lat/lon columns; the loader auto-detects delimiter and column roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsvConfig {
    /// Absolute or relative path to the .csv file on disk.
    pub path: String,
}

// --- Serde defaults ---

fn default_category() -> String { "basemap".to_string() }
fn default_opacity() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_png_format() -> String { "image/png".to_string() }
fn default_wms_version() -> String { "1.3.0".to_string() }
fn default_max_zoom() -> u32 { 18 }
fn default_png_ext() -> String { "png".to_string() }
fn default_refresh() -> u64 { 300 }
fn default_geojson() -> String { "geojson".to_string() }

// =============================================================================
// Category Mapping
// =============================================================================

/// Maps a category string from JSON to the internal ProviderCategory enum.
///
/// Accepts both the enum name and i18n key form (e.g. "weather" or "cat_weather").
pub fn parse_category(s: &str) -> ProviderCategory {
    match s.to_lowercase().trim_start_matches("cat_").as_ref() {
        "satellite" => ProviderCategory::Satellite,
        "atmosphere" => ProviderCategory::Atmosphere,
        "ocean" => ProviderCategory::Ocean,
        "land" => ProviderCategory::Land,
        "climate" => ProviderCategory::Climate,
        "ice" => ProviderCategory::Ice,
        "basemap" => ProviderCategory::Basemap,
        "weather" => ProviderCategory::Weather,
        "geology" => ProviderCategory::Geology,
        _ => {
            log::warn!("Unknown category '{}', defaulting to Basemap", s);
            ProviderCategory::Basemap
        }
    }
}

// =============================================================================
// Loading + Saving
// =============================================================================

/// Loads custom sources from the config file.
///
/// Returns an empty config if the file doesn't exist (first run).
/// Malformed entries are logged and skipped — never crashes.
pub fn load_config() -> CustomSourcesConfig {
    let path = crate::app_path(CUSTOM_SOURCES_FILE);
    if !path.exists() {
        log::info!("No custom sources config found, using empty list");
        return CustomSourcesConfig::default();
    }

    match fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(config) => {
                let config: CustomSourcesConfig = config;
                log::info!(
                    "Custom sources loaded: {} sources from {}",
                    config.sources.len(),
                    path.display()
                );
                config
            }
            Err(e) => {
                log::warn!(
                    "Custom sources config malformed ({}): {}",
                    path.display(),
                    e
                );
                CustomSourcesConfig::default()
            }
        },
        Err(e) => {
            log::warn!("Could not read custom sources config: {}", e);
            CustomSourcesConfig::default()
        }
    }
}

/// Saves the custom sources config to disk.
pub fn save_config(config: &CustomSourcesConfig) {
    let dir = crate::app_path("config");
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("Could not create config directory: {}", e);
        return;
    }

    let path = crate::app_path(CUSTOM_SOURCES_FILE);
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, &json) {
                log::warn!("Could not save custom sources config: {}", e);
            } else {
                log::info!("Custom sources saved: {} sources", config.sources.len());
            }
        }
        Err(e) => {
            log::warn!("Custom sources serialization failed: {}", e);
        }
    }
}

// =============================================================================
// Provider Creation (M17b: WMS)
// =============================================================================

/// Creates provider instances from all enabled custom source configs.
///
/// Each config is converted into the appropriate provider type and
/// returned for registration in the ProviderCatalog.
pub fn create_providers(config: &CustomSourcesConfig) -> Vec<Box<dyn LayerProvider>> {
    let mut providers: Vec<Box<dyn LayerProvider>> = Vec::new();

    for source in &config.sources {
        if !source.enabled {
            log::debug!("Custom source '{}' is disabled, skipping", source.id);
            continue;
        }

        match source.source_type {
            SourceType::Wms => {
                match create_wms_provider(source) {
                    Ok(provider) => {
                        log::info!(
                            "Custom WMS source registered: '{}' ({})",
                            source.name,
                            source.id
                        );
                        providers.push(provider);
                    }
                    Err(e) => {
                        log::warn!(
                            "Custom WMS source '{}' failed to create: {}",
                            source.id,
                            e
                        );
                    }
                }
            }
            SourceType::Xyz => {
                log::info!("Custom XYZ source '{}' — not yet implemented (M17c)", source.id);
                // TODO: M17c
            }
            SourceType::Rest => {
                // REST sources are handled by RestFeedManager, not the provider catalog.
                log::info!("Custom REST source '{}' registered (handled by RestFeedManager)", source.id);
            }
            SourceType::Shapefile => {
                // Shapefiles produce vector GeoLayers, not raster providers.
                // Loaded by ShapefileSourceManager → marker_system.
                log::info!("Custom Shapefile source '{}' registered (handled by ShapefileSourceManager)", source.id);
            }
            SourceType::Csv => {
                // Same lifecycle story as Shapefile — file-based, sync-loaded
                // GeoLayer of points. Owned by CsvSourceManager.
                log::info!("Custom CSV source '{}' registered (handled by CsvSourceManager)", source.id);
            }
        }
    }

    providers
}

/// Creates a WmsProvider from a custom source config.
fn create_wms_provider(source: &CustomSourceConfig) -> Result<Box<dyn LayerProvider>, String> {
    let wms_cfg = source.wms.as_ref()
        .ok_or_else(|| format!("Source '{}' has type=wms but no 'wms' config block", source.id))?;

    let ext = if wms_cfg.format.contains("jpeg") || wms_cfg.format.contains("jpg") {
        "jpg"
    } else {
        "png"
    };

    // Migration of the legacy `reproject_mercator: bool` field:
    // - `true`  → force the legacy Mercator-request path (back-compat for
    //            existing user configs and GUI-created sources where
    //            wms_caps detected Mercator-only behaviour). Logs a
    //            deprecation notice nudging users toward auto-discovery.
    // - `false` (or absent) → no override, run auto-discovery via
    //            GetCapabilities on first fetch (the new default).
    let legacy_reproject_mercator = if wms_cfg.reproject_mercator {
        Some(true)
    } else {
        None
    };

    let provider = CustomWmsProvider {
        info: ProviderInfo {
            id: format!("custom:{}", source.id),
            label: source.name.clone(),
            description: format!("Custom WMS: {}", wms_cfg.base_url),
            category: parse_category(&source.category),
            attribution: source.attribution.clone(),
            supports_date: wms_cfg.uses_time,
            default_opacity: source.default_opacity,
            legend_url: None,
        },
        base_url: wms_cfg.base_url.clone(),
        layer_name: wms_cfg.layer_name.clone(),
        format: wms_cfg.format.clone(),
        extension: ext.to_string(),
        uses_time: wms_cfg.uses_time,
        transparent: wms_cfg.transparent,
        legacy_reproject_mercator,
        wms_version: wms_cfg.wms_version.clone(),
        headers: source.headers.clone(),
    };

    Ok(Box::new(provider))
}

// =============================================================================
// CustomWmsProvider (runtime instance)
// =============================================================================

/// WMS provider created from a user's custom source config.
///
/// On first fetch, runs CRS auto-discovery via GetCapabilities (in
/// `crate::wms::resolve_behavior`) and persists the result. The legacy
/// `reproject_mercator` flag is still accepted from JSON for backward
/// compatibility, but its presence forces the legacy behaviour and skips
/// auto-discovery — users are nudged to remove it via a deprecation log.
struct CustomWmsProvider {
    info: ProviderInfo,
    base_url: String,
    layer_name: String,
    format: String,
    extension: String,
    uses_time: bool,
    transparent: bool,
    /// If `Some`, the legacy `reproject_mercator: bool` was set in JSON and
    /// its value is forwarded to `resolve_behavior` as a manual override.
    /// If `None`, we run normal auto-discovery.
    legacy_reproject_mercator: Option<bool>,
    wms_version: String,
    headers: HashMap<String, String>,
}

impl LayerProvider for CustomWmsProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn fetch(
        &self,
        date: &chrono::NaiveDate,
        cache_dir: &Path,
    ) -> Result<crate::provider::LayerImage, String> {
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Could not create cache directory: {}", e))?;

        // Resolve discovered behaviour (or legacy override). Cached on disk
        // after first call, so subsequent fetches skip GetCapabilities.
        let behavior = crate::wms::resolve_behavior(
            cache_dir,
            &self.info.id,
            &self.base_url,
            &self.layer_name,
            &self.wms_version,
            self.legacy_reproject_mercator,
        );

        // Cache path
        let cached = if self.uses_time {
            cache_dir.join(format!(
                "{}_{}.{}",
                self.info.id.replace(':', "_"),
                date.format("%Y-%m-%d"),
                self.extension,
            ))
        } else {
            cache_dir.join(format!(
                "{}.{}",
                self.info.id.replace(':', "_"),
                self.extension,
            ))
        };

        if cached.exists() {
            let use_cache = if self.uses_time {
                true
            } else {
                crate::wms::is_cache_fresh_pub(&cached, 24 * 3600)
            };

            if use_cache {
                log::info!("Custom WMS cache hit: {}", self.info.label);
                let raw = std::fs::read(&cached)
                    .map_err(|e| format!("Cache read failed: {}", e))?;
                let image = crate::wms::decode_image_pub(&raw, &self.info.label)?;
                return crate::wms::apply_behavior_reproject(image, &behavior, &self.info.label);
            }
        }

        // Build URL via the shared helper.
        let mut url = crate::wms::behavior::build_get_map_url(
            &self.base_url,
            &self.layer_name,
            &behavior,
            &self.wms_version,
            &self.format,
            self.transparent,
        );
        if self.uses_time {
            url.push_str(&format!("&TIME={}", date.format("%Y-%m-%d")));
        }
        log::info!("Custom WMS download: {} → {}", self.info.label, url);

        let mut request = ureq::get(&url);
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }

        let response = request
            .call()
            .map_err(|e| format!("Custom WMS download failed ({}): {}", self.info.id, e))?;

        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|e| format!("Error reading response: {}", e))?;

        log::info!(
            "Custom WMS download complete: {} ({} KB)",
            self.info.label,
            bytes.len() / 1024
        );

        if let Err(e) = std::fs::write(&cached, &bytes) {
            log::warn!("Custom WMS cache write failed: {}", e);
        }

        let image = crate::wms::decode_image_pub(&bytes, &self.info.label)?;
        crate::wms::apply_behavior_reproject(image, &behavior, &self.info.label)
    }

    fn fetch_with_fallback(
        &self,
        cache_dir: &Path,
    ) -> Result<(crate::provider::LayerImage, chrono::NaiveDate), String> {
        let today = chrono::Utc::now().date_naive();

        if self.uses_time {
            let dates = vec![
                today - chrono::Days::new(1),
                today - chrono::Days::new(2),
                today - chrono::Days::new(3),
            ];
            for date in &dates {
                match self.fetch(date, cache_dir) {
                    Ok(img) => return Ok((img, *date)),
                    Err(e) => {
                        log::warn!("Custom WMS '{}' fetch for {} failed: {}", self.info.id, date, e);
                    }
                }
            }
            Err(format!("Custom WMS '{}': all fallback dates failed", self.info.id))
        } else {
            let img = self.fetch(&today, cache_dir)?;
            Ok((img, today))
        }
    }
}

// =============================================================================
// Example Config (for documentation / first-run generation)
// =============================================================================

/// Generates an example custom_sources.json with commented examples.
#[allow(dead_code)]
pub fn generate_example_config() -> CustomSourcesConfig {
    CustomSourcesConfig {
        version: 1,
        sources: vec![
            CustomSourceConfig {
                id: "example_copernicus".to_string(),
                name: "Copernicus DEM (example)".to_string(),
                source_type: SourceType::Wms,
                category: "geology".to_string(),
                attribution: "© Copernicus".to_string(),
                default_opacity: 0.4,
                enabled: false,
                headers: HashMap::new(),
                wms: Some(WmsConfig {
                    base_url: "https://services.sentinel-hub.com/ogc/wms/YOUR_INSTANCE_ID".to_string(),
                    layer_name: "DEM".to_string(),
                    format: "image/png".to_string(),
                    transparent: true,
                    uses_time: false,
                    reproject_mercator: false,
                    wms_version: "1.3.0".to_string(),
                }),
                xyz: None,
                rest: None,
                shapefile: None,
                csv: None,
            },
        ],
    }
}

// =============================================================================
// REST Feed Manager (M17d)
// =============================================================================
// Periodically polls custom REST/GeoJSON endpoints and delivers parsed
// GeoLayers to the main loop, analogous to LiveSourceManager but with
// dynamic configuration from custom_sources.json.
// =============================================================================

/// Tracks one active REST feed.
struct ActiveRestFeed {
    id: String,
    name: String,
    url: String,
    attribution: String,
    headers: HashMap<String, String>,
    refresh_secs: u64,
    last_fetch: Option<Instant>,
    pending: Option<mpsc::Receiver<Result<GeoLayer, String>>>,
}

/// Result of a completed REST feed fetch.
pub struct RestFeedResult {
    /// Source ID for logging
    #[allow(dead_code)]
    pub source_id: String,
    /// Parsed GeoJSON layer ready for MarkerSystem
    pub layer: GeoLayer,
}

/// Manages periodic polling of custom REST/GeoJSON feeds.
pub struct RestFeedManager {
    feeds: Vec<ActiveRestFeed>,
}

impl RestFeedManager {
    pub fn new() -> Self {
        Self { feeds: Vec::new() }
    }

    /// Synchronizes active feeds with the current config.
    ///
    /// Adds new enabled REST sources, removes disabled/deleted ones.
    /// Returns names of removed feeds (so caller can clean up GeoLayers).
    pub fn sync_config(&mut self, config: &CustomSourcesConfig) -> Vec<String> {
        let rest_sources: Vec<&CustomSourceConfig> = config.sources.iter()
            .filter(|s| matches!(s.source_type, SourceType::Rest) && s.enabled)
            .filter(|s| s.rest.is_some())
            .collect();

        // Collect names of feeds being removed
        let removed: Vec<String> = self.feeds.iter()
            .filter(|f| !rest_sources.iter().any(|s| s.id == f.id))
            .map(|f| f.name.clone())
            .collect();

        // Remove feeds whose source was disabled or deleted
        self.feeds.retain(|f| rest_sources.iter().any(|s| s.id == f.id));

        // Add new feeds
        for source in &rest_sources {
            if self.feeds.iter().any(|f| f.id == source.id) {
                continue; // Already active
            }
            let rest_cfg = source.rest.as_ref().unwrap();
            log::info!("RestFeed: activating '{}' ({})", source.name, rest_cfg.url);
            let mut feed = ActiveRestFeed {
                id: source.id.clone(),
                name: source.name.clone(),
                url: rest_cfg.url.clone(),
                attribution: source.attribution.clone(),
                headers: source.headers.clone(),
                refresh_secs: rest_cfg.refresh_secs,
                last_fetch: None,
                pending: None,
            };
            // Start first fetch immediately
            Self::start_fetch(&mut feed);
            self.feeds.push(feed);
        }

        removed
    }

    /// Polls for completed fetches and triggers auto-refreshes.
    pub fn poll(&mut self) -> Vec<RestFeedResult> {
        let mut results = Vec::new();

        for feed in &mut self.feeds {
            // Check pending fetch
            if let Some(rx) = &feed.pending {
                match rx.try_recv() {
                    Ok(Ok(layer)) => {
                        log::info!("RestFeed '{}': {} features",
                            feed.name, layer.len());
                        results.push(RestFeedResult {
                            source_id: feed.id.clone(),
                            layer,
                        });
                        feed.pending = None;
                        feed.last_fetch = Some(Instant::now());
                    }
                    Ok(Err(e)) => {
                        log::warn!("RestFeed '{}' failed: {}", feed.name, e);
                        feed.pending = None;
                        feed.last_fetch = Some(Instant::now());
                    }
                    Err(mpsc::TryRecvError::Empty) => {} // Still downloading
                    Err(mpsc::TryRecvError::Disconnected) => {
                        log::warn!("RestFeed '{}': download thread disconnected", feed.name);
                        feed.pending = None;
                    }
                }
            }

            // Auto-refresh if no fetch pending and interval elapsed
            if feed.pending.is_none() && feed.refresh_secs > 0 {
                let should_refresh = feed.last_fetch
                    .map_or(true, |t| t.elapsed().as_secs() >= feed.refresh_secs);
                if should_refresh {
                    Self::start_fetch(feed);
                }
            }
        }

        results
    }

    /// Launches a background thread to fetch and parse a GeoJSON feed.
    fn start_fetch(feed: &mut ActiveRestFeed) {
        let url = feed.url.clone();
        let name = feed.name.clone();
        let attribution = feed.attribution.clone();
        let headers = feed.headers.clone();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let result = fetch_rest_geojson(&url, &name, &attribution, &headers);
            let _ = tx.send(result);
        });

        feed.pending = Some(rx);
    }
}

/// Fetches GeoJSON from a REST endpoint with optional custom headers.
fn fetch_rest_geojson(
    url: &str,
    layer_name: &str,
    attribution: &str,
    headers: &HashMap<String, String>,
) -> Result<GeoLayer, String> {
    let mut request = ureq::get(url);
    for (key, value) in headers {
        request = request.header(key, value);
    }

    let response = request
        .call()
        .map_err(|e| format!("REST fetch failed for '{}': {}", url, e))?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {}", e))?;

    let mut layer = crate::geojson::parse_geojson(&body, layer_name)?;
    layer.name = layer_name.to_string();

    if !attribution.is_empty() {
        layer.attribution = Some(attribution.to_string());
    }

    Ok(layer)
}

// =============================================================================
// Shapefile Source Manager
// =============================================================================
// Owns the lifetime of GeoLayers loaded from custom Shapefile sources. Mirrors
// RestFeedManager's pattern but simpler: shapefiles are local files, not
// polled URLs, so no background threads — load is synchronous on add.
// =============================================================================

/// One shapefile source the manager has actively loaded.
struct ActiveShapefile {
    /// User-chosen source ID (custom_source.id, e.g. "user_world_borders").
    id: String,
    /// Display name — also used as the GeoLayer name. Tracking it here lets
    /// the manager remove the right layer when the source is disabled.
    name: String,
    /// Path the source pointed at when last loaded. If the user edits the
    /// path, the manager will reload (the source-id stays the same).
    loaded_from: String,
}

/// Result of one `sync_config` call.
pub struct ShapefileSyncResult {
    /// New GeoLayers the manager just loaded (caller adds them to MarkerSystem).
    pub added: Vec<crate::geojson::GeoLayer>,
    /// Layer names whose source was removed/disabled (caller removes them
    /// from MarkerSystem).
    pub removed: Vec<String>,
}

impl ShapefileSyncResult {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Loads + tracks GeoLayers from custom Shapefile sources.
pub struct ShapefileSourceManager {
    active: Vec<ActiveShapefile>,
}

impl ShapefileSourceManager {
    pub fn new() -> Self {
        Self { active: Vec::new() }
    }

    /// Reconciles the manager with the current config. Returns:
    /// - `added`:  newly-loaded GeoLayers (caller passes to MarkerSystem)
    /// - `removed`: layer names of sources that were disabled or whose
    ///              path changed (caller removes from MarkerSystem before
    ///              applying `added`, since path-change is a remove+re-add)
    pub fn sync_config(&mut self, config: &CustomSourcesConfig) -> ShapefileSyncResult {
        // 1. Desired state: every enabled Shapefile source with a path.
        let wanted: Vec<&CustomSourceConfig> = config
            .sources
            .iter()
            .filter(|s| matches!(s.source_type, SourceType::Shapefile) && s.enabled)
            .filter(|s| s.shapefile.as_ref().is_some_and(|sc| !sc.path.trim().is_empty()))
            .collect();

        // 2. Removals = previously active sources that are no longer wanted,
        //    OR are still wanted but with a different path (reload semantics).
        let mut removed: Vec<String> = Vec::new();
        self.active.retain(|active| {
            let still_wanted = wanted.iter().find(|w| w.id == active.id);
            match still_wanted {
                None => {
                    removed.push(active.name.clone());
                    false // drop
                }
                Some(w) => {
                    let new_path = w
                        .shapefile
                        .as_ref()
                        .map(|sc| sc.path.trim().to_string())
                        .unwrap_or_default();
                    if new_path != active.loaded_from {
                        // Path changed → remove + queue for re-add below.
                        removed.push(active.name.clone());
                        false
                    } else {
                        true // unchanged, keep
                    }
                }
            }
        });

        // 3. Additions = wanted sources that aren't currently active.
        let mut added: Vec<crate::geojson::GeoLayer> = Vec::new();
        for source in &wanted {
            if self.active.iter().any(|a| a.id == source.id) {
                continue;
            }
            let cfg = source.shapefile.as_ref().unwrap();
            let path = std::path::Path::new(&cfg.path);
            log::info!(
                "Shapefile source '{}': loading from {}",
                source.name,
                cfg.path,
            );
            match crate::shp::load_shapefile(path) {
                Ok(mut layer) => {
                    // Prefer the user-chosen name over the file stem.
                    layer.name = source.name.clone();
                    if !source.attribution.is_empty() {
                        layer.attribution = Some(source.attribution.clone());
                    }
                    self.active.push(ActiveShapefile {
                        id: source.id.clone(),
                        name: source.name.clone(),
                        loaded_from: cfg.path.trim().to_string(),
                    });
                    added.push(layer);
                }
                Err(e) => {
                    log::error!(
                        "Shapefile source '{}' failed to load from '{}': {}",
                        source.name,
                        cfg.path,
                        e,
                    );
                }
            }
        }

        ShapefileSyncResult { added, removed }
    }
}

impl Default for ShapefileSourceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod shapefile_manager_tests {
    use super::*;

    fn cfg_with(sources: Vec<CustomSourceConfig>) -> CustomSourcesConfig {
        CustomSourcesConfig { version: 1, sources }
    }

    fn shp_source(id: &str, path: &str, enabled: bool) -> CustomSourceConfig {
        CustomSourceConfig {
            id: id.to_string(),
            name: id.to_string(),
            source_type: SourceType::Shapefile,
            category: "basemap".to_string(),
            attribution: String::new(),
            default_opacity: 0.5,
            enabled,
            headers: HashMap::new(),
            wms: None,
            xyz: None,
            rest: None,
            shapefile: Some(ShapefileConfig { path: path.to_string() }),
            csv: None,
        }
    }

    #[test]
    fn empty_config_is_noop() {
        let mut mgr = ShapefileSourceManager::new();
        let r = mgr.sync_config(&cfg_with(vec![]));
        assert!(r.is_noop());
    }

    #[test]
    fn missing_path_does_not_attempt_load() {
        // A source with an empty path is filtered before load_shapefile is
        // called — would produce a useless error otherwise.
        let mut mgr = ShapefileSourceManager::new();
        let r = mgr.sync_config(&cfg_with(vec![shp_source("user_x", "", true)]));
        assert!(r.is_noop());
    }

    #[test]
    fn nonexistent_path_logs_and_skips() {
        // Unloadable file → error logged, no layer added, manager doesn't
        // record it as active so a retry on next sync_config will try again.
        let mut mgr = ShapefileSourceManager::new();
        let r = mgr.sync_config(&cfg_with(vec![shp_source(
            "user_x",
            "/no/such/file.shp",
            true,
        )]));
        assert!(r.added.is_empty());
        assert!(mgr.active.is_empty());

        // Re-syncing should attempt again, not silently skip.
        let r2 = mgr.sync_config(&cfg_with(vec![shp_source(
            "user_x",
            "/no/such/file.shp",
            true,
        )]));
        assert!(r2.added.is_empty());
    }

    #[test]
    fn disabling_source_emits_removal() {
        // Set up a fake "active" entry by hand (shortcut around needing a
        // real shapefile on disk for this test).
        let mut mgr = ShapefileSourceManager::new();
        mgr.active.push(ActiveShapefile {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/some/path.shp".to_string(),
        });

        let r = mgr.sync_config(&cfg_with(vec![shp_source(
            "user_x",
            "/some/path.shp",
            false, // disabled
        )]));
        assert_eq!(r.removed, vec!["user_x".to_string()]);
        assert!(mgr.active.is_empty());
    }

    #[test]
    fn path_change_triggers_remove_then_readd_attempt() {
        // Active entry with old path; config now has same id but new path.
        // Expect the old layer to be removed (and re-add will be attempted,
        // failing here since the file doesn't exist — but that's the
        // load_shapefile failure path, not the manager's responsibility).
        let mut mgr = ShapefileSourceManager::new();
        mgr.active.push(ActiveShapefile {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/old/path.shp".to_string(),
        });

        let r = mgr.sync_config(&cfg_with(vec![shp_source(
            "user_x",
            "/new/path.shp",
            true,
        )]));
        assert_eq!(r.removed, vec!["user_x".to_string()]);
        assert!(mgr.active.is_empty());
    }

    #[test]
    fn unchanged_active_source_is_skipped() {
        let mut mgr = ShapefileSourceManager::new();
        mgr.active.push(ActiveShapefile {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/some/path.shp".to_string(),
        });

        let r = mgr.sync_config(&cfg_with(vec![shp_source(
            "user_x",
            "/some/path.shp",
            true,
        )]));
        assert!(r.is_noop());
        assert_eq!(mgr.active.len(), 1);
    }
}

// =============================================================================
// CSV Source Manager
// =============================================================================
// Same lifecycle shape as ShapefileSourceManager: local file, synchronous
// load, no polling. Cloned rather than generalised over a `LocalFileSourceKind`
// trait — when a third file format lands (KML?), it's worth pulling both
// into a generic. Two parallel copies of ~80 lines is cheaper than premature
// abstraction for now.
// =============================================================================

struct ActiveCsvSource {
    id: String,
    name: String,
    loaded_from: String,
}

pub struct CsvSyncResult {
    pub added: Vec<crate::geojson::GeoLayer>,
    pub removed: Vec<String>,
}

impl CsvSyncResult {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

pub struct CsvSourceManager {
    active: Vec<ActiveCsvSource>,
}

impl CsvSourceManager {
    pub fn new() -> Self {
        Self { active: Vec::new() }
    }

    /// Reconciles the manager with the current config — same contract as
    /// `ShapefileSourceManager::sync_config`.
    pub fn sync_config(&mut self, config: &CustomSourcesConfig) -> CsvSyncResult {
        let wanted: Vec<&CustomSourceConfig> = config
            .sources
            .iter()
            .filter(|s| matches!(s.source_type, SourceType::Csv) && s.enabled)
            .filter(|s| s.csv.as_ref().is_some_and(|cc| !cc.path.trim().is_empty()))
            .collect();

        let mut removed: Vec<String> = Vec::new();
        self.active.retain(|active| {
            let still_wanted = wanted.iter().find(|w| w.id == active.id);
            match still_wanted {
                None => {
                    removed.push(active.name.clone());
                    false
                }
                Some(w) => {
                    let new_path = w
                        .csv
                        .as_ref()
                        .map(|cc| cc.path.trim().to_string())
                        .unwrap_or_default();
                    if new_path != active.loaded_from {
                        removed.push(active.name.clone());
                        false
                    } else {
                        true
                    }
                }
            }
        });

        let mut added: Vec<crate::geojson::GeoLayer> = Vec::new();
        for source in &wanted {
            if self.active.iter().any(|a| a.id == source.id) {
                continue;
            }
            let cfg = source.csv.as_ref().unwrap();
            let path = std::path::Path::new(&cfg.path);
            log::info!(
                "CSV source '{}': loading from {}",
                source.name,
                cfg.path,
            );
            match crate::csv_import::load_csv_file(path) {
                Ok(mut layer) => {
                    layer.name = source.name.clone();
                    if !source.attribution.is_empty() {
                        layer.attribution = Some(source.attribution.clone());
                    }
                    self.active.push(ActiveCsvSource {
                        id: source.id.clone(),
                        name: source.name.clone(),
                        loaded_from: cfg.path.trim().to_string(),
                    });
                    added.push(layer);
                }
                Err(e) => {
                    log::error!(
                        "CSV source '{}' failed to load from '{}': {}",
                        source.name,
                        cfg.path,
                        e,
                    );
                }
            }
        }

        CsvSyncResult { added, removed }
    }
}

impl Default for CsvSourceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod csv_manager_tests {
    use super::*;

    fn cfg_with(sources: Vec<CustomSourceConfig>) -> CustomSourcesConfig {
        CustomSourcesConfig { version: 1, sources }
    }

    fn csv_source(id: &str, path: &str, enabled: bool) -> CustomSourceConfig {
        CustomSourceConfig {
            id: id.to_string(),
            name: id.to_string(),
            source_type: SourceType::Csv,
            category: "basemap".to_string(),
            attribution: String::new(),
            default_opacity: 0.5,
            enabled,
            headers: HashMap::new(),
            wms: None,
            xyz: None,
            rest: None,
            shapefile: None,
            csv: Some(CsvConfig { path: path.to_string() }),
        }
    }

    #[test]
    fn empty_config_is_noop() {
        let mut mgr = CsvSourceManager::new();
        assert!(mgr.sync_config(&cfg_with(vec![])).is_noop());
    }

    #[test]
    fn missing_path_does_not_attempt_load() {
        let mut mgr = CsvSourceManager::new();
        let r = mgr.sync_config(&cfg_with(vec![csv_source("user_x", "", true)]));
        assert!(r.is_noop());
    }

    #[test]
    fn nonexistent_path_logs_and_skips() {
        let mut mgr = CsvSourceManager::new();
        let r = mgr.sync_config(&cfg_with(vec![csv_source(
            "user_x",
            "/no/such/file.csv",
            true,
        )]));
        assert!(r.added.is_empty());
        assert!(mgr.active.is_empty());
    }

    #[test]
    fn disabling_source_emits_removal() {
        let mut mgr = CsvSourceManager::new();
        mgr.active.push(ActiveCsvSource {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/some/path.csv".to_string(),
        });
        let r = mgr.sync_config(&cfg_with(vec![csv_source(
            "user_x",
            "/some/path.csv",
            false,
        )]));
        assert_eq!(r.removed, vec!["user_x".to_string()]);
        assert!(mgr.active.is_empty());
    }

    #[test]
    fn path_change_triggers_remove_then_readd_attempt() {
        let mut mgr = CsvSourceManager::new();
        mgr.active.push(ActiveCsvSource {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/old/path.csv".to_string(),
        });
        let r = mgr.sync_config(&cfg_with(vec![csv_source(
            "user_x",
            "/new/path.csv",
            true,
        )]));
        assert_eq!(r.removed, vec!["user_x".to_string()]);
        assert!(mgr.active.is_empty());
    }

    #[test]
    fn unchanged_active_source_is_skipped() {
        let mut mgr = CsvSourceManager::new();
        mgr.active.push(ActiveCsvSource {
            id: "user_x".to_string(),
            name: "user_x".to_string(),
            loaded_from: "/some/path.csv".to_string(),
        });
        let r = mgr.sync_config(&cfg_with(vec![csv_source(
            "user_x",
            "/some/path.csv",
            true,
        )]));
        assert!(r.is_noop());
        assert_eq!(mgr.active.len(), 1);
    }
}
