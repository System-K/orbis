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
        reproject_mercator: wms_cfg.reproject_mercator,
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
/// Functionally identical to the built-in WmsProvider but constructed
/// from runtime JSON config instead of hardcoded definitions.
struct CustomWmsProvider {
    info: ProviderInfo,
    base_url: String,
    layer_name: String,
    format: String,
    extension: String,
    uses_time: bool,
    transparent: bool,
    reproject_mercator: bool,
    wms_version: String,
    headers: HashMap<String, String>,
}

/// Default image resolution for custom WMS downloads.
const DEFAULT_WIDTH: u32 = 2048;
const DEFAULT_HEIGHT: u32 = 1024;

impl CustomWmsProvider {
    /// Returns true if using WMS 1.3.0+ (which uses CRS and geographic axis order).
    fn is_version_130(&self) -> bool {
        self.wms_version.starts_with("1.3")
    }

    /// Builds the WMS GetMap URL.
    ///
    /// Handles differences between WMS 1.1.x and 1.3.0:
    /// - 1.1.x: SRS parameter, BBOX = minx,miny,maxx,maxy (always easting,northing)
    /// - 1.3.0: CRS parameter, BBOX axis order depends on CRS
    ///   (EPSG:4326 = lat,lon; EPSG:3857 = easting,northing)
    fn build_url(&self, date: Option<&chrono::NaiveDate>) -> String {
        let v130 = self.is_version_130();
        let crs_param = if v130 { "CRS" } else { "SRS" };

        let mut url = if self.reproject_mercator {
            let extent = 20037508.3427892_f64;
            format!(
                "{}?SERVICE=WMS&VERSION={}&REQUEST=GetMap&LAYERS={}&FORMAT={}\
                 &WIDTH={}&HEIGHT={}&{}=EPSG:3857&BBOX={},{},{},{}&STYLES=",
                self.base_url, self.wms_version, self.layer_name, self.format,
                DEFAULT_WIDTH, DEFAULT_WIDTH,
                crs_param,
                -extent, -extent, extent, extent,
            )
        } else if v130 {
            // WMS 1.3.0: EPSG:4326 BBOX = minlat,minlon,maxlat,maxlon
            format!(
                "{}?SERVICE=WMS&VERSION={}&REQUEST=GetMap&LAYERS={}&FORMAT={}\
                 &WIDTH={}&HEIGHT={}&CRS=EPSG:4326&BBOX=-90,-180,90,180&STYLES=",
                self.base_url, self.wms_version, self.layer_name, self.format,
                DEFAULT_WIDTH, DEFAULT_HEIGHT,
            )
        } else {
            // WMS 1.1.x: EPSG:4326 BBOX = minlon,minlat,maxlon,maxlat
            format!(
                "{}?SERVICE=WMS&VERSION={}&REQUEST=GetMap&LAYERS={}&FORMAT={}\
                 &WIDTH={}&HEIGHT={}&SRS=EPSG:4326&BBOX=-180,-90,180,90&STYLES=",
                self.base_url, self.wms_version, self.layer_name, self.format,
                DEFAULT_WIDTH, DEFAULT_HEIGHT,
            )
        };

        if self.transparent {
            url.push_str("&TRANSPARENT=true");
        }

        if let Some(d) = date {
            url.push_str(&format!("&TIME={}", d.format("%Y-%m-%d")));
        }

        url
    }
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

        // Cache hit?
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
                let mut image = crate::wms::decode_image_pub(&raw, &self.info.label)?;
                if self.reproject_mercator {
                    image = crate::wms::reproject_mercator_pub(image)?;
                }
                return Ok(image);
            }
        }

        // Download
        let time_param = if self.uses_time { Some(date) } else { None };
        let url = self.build_url(time_param);
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

        // Cache
        if let Err(e) = std::fs::write(&cached, &bytes) {
            log::warn!("Custom WMS cache write failed: {}", e);
        }

        // Decode
        let mut image = crate::wms::decode_image_pub(&bytes, &self.info.label)?;

        // Reproject if needed
        if self.reproject_mercator {
            image = crate::wms::reproject_mercator_pub(image)?;
        }

        Ok(image)
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
