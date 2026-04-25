// =============================================================================
// Orbis — Generic WMS Providers (M10)
// =============================================================================
// Downloads imagery from external WMS (Web Map Service) endpoints.
//
// Unlike GIBS (which is NASA-specific), these providers support any
// OGC-compliant WMS server: DWD, Terrestris/OSM, Copernicus, etc.
//
// Module layout:
// - `crs`        — supported CRSes and the lat/lon → pixel forward transform
// - `reproject`  — generic source-CRS → equirectangular resampler
// - (this file)  — static layer list, WmsProvider, URL building, fetch/cache
//
// Key differences from GIBS:
// - Configurable base URL per provider
// - Optional TIME parameter (basemaps don't need it)
// - Different caching strategy (timeless layers cached once)
// - Each service has its own attribution requirements
// =============================================================================

pub mod behavior;
pub mod capabilities;
pub mod probe;
pub mod reproject;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::provider::{LayerImage, LayerProvider, ProviderCategory, ProviderInfo};
use behavior::SourceBehavior;

/// Default resolution for WMS downloads (same as GIBS).
const DEFAULT_WIDTH: u32 = 2048;
const DEFAULT_HEIGHT: u32 = 1024;

/// WMS protocol version Orbis sends for built-in layers. 1.3.0 is the modern
/// standard and is supported by every server we ship configs for; the URL
/// builder handles axis-order quirks for both 1.3.0 and 1.1.x for custom
/// sources that need the older protocol.
const BUILTIN_WMS_VERSION: &str = "1.3.0";

// =============================================================================
// WMS Layer Definition
// =============================================================================

/// Static definition of an external WMS layer.
///
/// Each definition describes one layer from a third-party WMS server.
/// At runtime, each becomes a `WmsProvider` instance.
#[derive(Debug, Clone)]
struct WmsLayerDef {
    /// Provider ID (used in settings, cache paths, etc.)
    id: &'static str,
    /// WMS base URL (everything before the query string)
    base_url: &'static str,
    /// WMS layer name (LAYERS parameter)
    layer_name: &'static str,
    /// Display name for the GUI
    label: &'static str,
    /// Short description
    description: &'static str,
    /// Image format: "image/jpeg" or "image/png"
    format: &'static str,
    /// File extension for cache
    extension: &'static str,
    /// Whether this layer uses the TIME parameter
    uses_time: bool,
    /// Whether to request transparent background (TRANSPARENT=true)
    transparent: bool,
    /// Category for catalog grouping
    category: ProviderCategory,
    /// Attribution text
    attribution: &'static str,
    /// Default opacity (0.0–1.0)
    default_opacity: f32,
}

// =============================================================================
// All External WMS Layer Definitions
// =============================================================================

const WMS_LAYERS: &[WmsLayerDef] = &[
    // =========================================================================
    // Basemaps (no TIME parameter, cached permanently)
    // =========================================================================
    WmsLayerDef {
        id: "osm_standard",
        base_url: "https://ows.terrestris.de/osm/service",
        layer_name: "OSM-WMS",
        label: "OpenStreetMap",
        description: "OpenStreetMap rendered basemap via Terrestris WMS. Shows roads, borders, cities, and landmarks worldwide.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: false,
        category: ProviderCategory::Basemap,
        attribution: "© OpenStreetMap contributors / Terrestris",
        default_opacity: 0.35,
    },
    WmsLayerDef {
        id: "osm_topo",
        base_url: "https://ows.terrestris.de/osm/service",
        layer_name: "TOPO-WMS",
        label: "Topographic Map",
        description: "OpenTopoMap-style topographic basemap via Terrestris WMS. Shows terrain contours, elevation shading, and geographic features.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: false,
        category: ProviderCategory::Basemap,
        attribution: "© OpenStreetMap contributors / OpenTopoMap / Terrestris",
        default_opacity: 0.35,
    },

    // =========================================================================
    // DWD — Deutscher Wetterdienst (German Weather Service)
    // =========================================================================
    // Free, no authentication required.
    // ICON model: global weather forecast (0.25° resolution).
    // Data license: DL-DE/BY-2.0 (free with attribution).
    // =========================================================================
    WmsLayerDef {
        id: "dwd_icon_temperature",
        base_url: "https://maps.dwd.de/geoserver/wms",
        layer_name: "dwd:Aicon_reg025_fd_sl_T",
        label: "Temperature Forecast (DWD ICON)",
        description: "Global temperature forecast from the DWD ICON model (0.25° resolution). Shows 2m air temperature with color scale.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: true,
        category: ProviderCategory::Weather,
        attribution: "© DWD (Deutscher Wetterdienst)",
        default_opacity: 0.5,
    },
    WmsLayerDef {
        id: "dwd_icon_precipitation",
        base_url: "https://maps.dwd.de/geoserver/wms",
        layer_name: "dwd:Aicon_reg025_fd_sl_TOTPREC",
        label: "Precipitation Forecast (DWD ICON)",
        description: "Global total precipitation forecast from DWD ICON model. Shows rain and snow accumulation.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: true,
        category: ProviderCategory::Weather,
        attribution: "© DWD (Deutscher Wetterdienst)",
        default_opacity: 0.5,
    },
    WmsLayerDef {
        id: "dwd_icon_wind",
        base_url: "https://maps.dwd.de/geoserver/wms",
        layer_name: "dwd:Aicon_reg025_fd_sl_UV10M",
        label: "Wind Forecast (DWD ICON)",
        description: "Global 10m wind speed and direction forecast from DWD ICON model.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: true,
        category: ProviderCategory::Weather,
        attribution: "© DWD (Deutscher Wetterdienst)",
        default_opacity: 0.5,
    },
    WmsLayerDef {
        id: "dwd_icon_pressure",
        base_url: "https://maps.dwd.de/geoserver/wms",
        layer_name: "dwd:Aicon_reg025_fd_sl_PMSL",
        label: "Pressure Forecast (DWD ICON)",
        description: "Mean sea level pressure forecast from DWD ICON model. Shows pressure isolines (isobars).",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: true,
        category: ProviderCategory::Weather,
        attribution: "© DWD (Deutscher Wetterdienst)",
        default_opacity: 0.5,
    },
    WmsLayerDef {
        id: "dwd_warnings",
        base_url: "https://maps.dwd.de/geoserver/wms",
        layer_name: "dwd:Autowarn_Analyse",
        label: "Weather Warnings (DWD)",
        description: "Current DWD weather warnings for Germany. Color-coded by severity (yellow → orange → red → violet → dark red).",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: true,
        category: ProviderCategory::Weather,
        attribution: "© DWD (Deutscher Wetterdienst)",
        default_opacity: 0.6,
    },

    // =========================================================================
    // GEBCO — General Bathymetric Chart of the Oceans
    // =========================================================================
    // Free, no authentication required.
    // GEBCO_2024 Grid: global terrain model (15 arc-second resolution).
    // Data license: public domain / open access.
    // =========================================================================
    WmsLayerDef {
        id: "gebco_bathymetry",
        base_url: "https://wms.gebco.net/mapserv",
        layer_name: "GEBCO_LATEST",
        label: "Bathymetry (GEBCO)",
        description: "Global ocean bathymetry and land elevation from the GEBCO grid. Color-coded depth/height map at 15 arc-second resolution.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: false,
        category: ProviderCategory::Geology,
        attribution: "© GEBCO / Nippon Foundation–GEBCO Seabed 2030 Project",
        default_opacity: 0.4,
    },
    WmsLayerDef {
        id: "gebco_shaded_relief",
        base_url: "https://wms.gebco.net/mapserv",
        layer_name: "GEBCO_LATEST_2",
        label: "Shaded Relief (GEBCO)",
        description: "Global shaded relief image from the GEBCO grid. Shows ocean floor and land topography with hillshading.",
        format: "image/png",
        extension: "png",
        uses_time: false,
        transparent: false,
        category: ProviderCategory::Geology,
        attribution: "© GEBCO / Nippon Foundation–GEBCO Seabed 2030 Project",
        default_opacity: 0.4,
    },
];

// =============================================================================
// WmsProvider
// =============================================================================

/// A generic WMS-backed layer provider.
///
/// On first fetch the provider runs `resolve_behavior` (capabilities-driven
/// CRS discovery) and persists the result. Subsequent fetches reuse the
/// cached behaviour until it ages out.
pub struct WmsProvider {
    info: ProviderInfo,
    base_url: String,
    layer_name: String,
    format: String,
    extension: String,
    uses_time: bool,
    transparent: bool,
}

impl WmsProvider {
    /// Creates a provider from a static layer definition.
    fn from_def(def: &WmsLayerDef) -> Self {
        Self {
            info: ProviderInfo {
                id: def.id.to_string(),
                label: def.label.to_string(),
                description: def.description.to_string(),
                category: def.category,
                attribution: def.attribution.to_string(),
                supports_date: def.uses_time,
                default_opacity: def.default_opacity,
                legend_url: None, // WMS legends could be added later via GetLegendGraphic
            },
            base_url: def.base_url.to_string(),
            layer_name: def.layer_name.to_string(),
            format: def.format.to_string(),
            extension: def.extension.to_string(),
            uses_time: def.uses_time,
            transparent: def.transparent,
        }
    }
}

impl LayerProvider for WmsProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn fetch(&self, date: &NaiveDate, cache_dir: &Path) -> Result<LayerImage, String> {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Could not create cache directory: {}", e))?;

        // Resolve the source's discovered behaviour (cached after first call).
        let behavior = resolve_behavior(
            cache_dir,
            &self.info.id,
            &self.base_url,
            &self.layer_name,
            BUILTIN_WMS_VERSION,
            None, // built-in layers never use the legacy flag
        );

        // For timeless layers (basemaps): cache without date in filename
        let cached = if self.uses_time {
            cache_dir.join(format!(
                "{}_{}.{}",
                self.info.id,
                date.format("%Y-%m-%d"),
                self.extension,
            ))
        } else {
            cache_dir.join(format!("{}.{}", self.info.id, self.extension,))
        };

        // 1. Cache hit?
        if cached.exists() {
            let use_cache = if self.uses_time {
                true // Date-based layers are immutable per date
            } else {
                is_cache_fresh(&cached, 24 * 3600) // Basemaps: refresh daily
            };

            if use_cache {
                log::info!("WMS cache hit: {} ({})", self.info.label, cached.display());
                let raw_bytes = fs::read(&cached)
                    .map_err(|e| format!("Could not read cache file: {}", e))?;
                let image = decode_image(&raw_bytes, &self.info.label)?;
                return apply_behavior_reproject(image, &behavior, &self.info.label);
            }
        }

        // 2. Download from WMS
        let mut url = behavior::build_get_map_url(
            &self.base_url,
            &self.layer_name,
            &behavior,
            BUILTIN_WMS_VERSION,
            &self.format,
            self.transparent,
        );
        if self.uses_time {
            url.push_str(&format!("&TIME={}", date.format("%Y-%m-%d")));
        }
        log::info!("WMS download: {} → {}", self.info.label, url);

        let response = ureq::get(&url)
            .call()
            .map_err(|e| format!("WMS download failed ({}): {}", self.info.id, e))?;

        // Check if the response is an image (not an error page)
        let is_error = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("xml") || ct.contains("text/html"))
            .unwrap_or(false);

        if is_error {
            let body = response
                .into_body()
                .read_to_string()
                .map_err(|e| format!("Error reading error response: {}", e))?;
            return Err(format!(
                "WMS returned an error (not an image): {}",
                &body[..body.len().min(500)]
            ));
        }

        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|e| format!("Error reading image data: {}", e))?;

        log::info!(
            "WMS download complete: {} ({} KB)",
            self.info.label,
            bytes.len() / 1024
        );

        if let Err(e) = fs::write(&cached, &bytes) {
            log::warn!("WMS cache save failed: {}", e);
        }

        let layer_image = decode_image(&bytes, &self.info.label)?;
        apply_behavior_reproject(layer_image, &behavior, &self.info.label)
    }

    fn fetch_with_fallback(&self, cache_dir: &Path) -> Result<(LayerImage, NaiveDate), String> {
        if self.uses_time {
            // Time-based layers: try yesterday, day before, etc.
            // (same as default behavior from trait)
            let today = chrono::Utc::now().date_naive();
            let dates = vec![
                today - chrono::Days::new(1),
                today - chrono::Days::new(2),
                today - chrono::Days::new(3),
            ];

            for date in &dates {
                match self.fetch(date, cache_dir) {
                    Ok(img) => return Ok((img, *date)),
                    Err(e) => {
                        log::warn!(
                            "WMS '{}' fetch for {} failed: {}",
                            self.info.id,
                            date.format("%Y-%m-%d"),
                            e
                        );
                    }
                }
            }

            Err(format!(
                "WMS '{}': all fallback dates failed",
                self.info.id
            ))
        } else {
            // Timeless layers (basemaps): just use today's date as key
            let today = chrono::Utc::now().date_naive();
            let img = self.fetch(&today, cache_dir)?;
            Ok((img, today))
        }
    }
}

// =============================================================================
// Behaviour resolution + reprojection — shared between built-in WmsProvider
// and CustomWmsProvider.
// =============================================================================

/// Loads a cached behaviour or, on miss, runs discovery against the server.
/// Always returns *some* behaviour — discovery failures fall back to a safe
/// default (assume EPSG:4326, trust the server). The result is persisted so
/// the next fetch hits the cache.
///
/// `legacy_reproject_mercator` is a back-compat hook for the old per-source
/// `reproject_mercator: bool` flag in user JSON configs. When set, we skip
/// discovery and use that flag's intent — but log a deprecation notice
/// because the auto-discovery path is now the supported one.
pub fn resolve_behavior(
    cache_dir: &Path,
    source_id: &str,
    base_url: &str,
    layer_name: &str,
    wms_version: &str,
    legacy_reproject_mercator: Option<bool>,
) -> SourceBehavior {
    if let Some(flag) = legacy_reproject_mercator {
        log::warn!(
            "WMS source '{}' uses the deprecated `reproject_mercator` flag. \
             Remove it from your config to enable automatic CRS discovery.",
            source_id
        );
        let b = SourceBehavior::from_legacy_flag(flag);
        behavior::save_behavior(cache_dir, source_id, &b);
        return b;
    }

    if let Some(cached) = behavior::load_behavior(cache_dir, source_id) {
        log::debug!(
            "WMS '{}': behaviour cache hit ({:?}, request {})",
            source_id,
            cached.discovery_method,
            cached.request_crs.epsg_code(),
        );
        return cached;
    }

    let discovered = discover_behavior_via_capabilities(base_url, layer_name, wms_version);
    log::info!(
        "WMS '{}': discovered behaviour {:?}, will request {}",
        source_id,
        discovered.discovery_method,
        discovered.request_crs.epsg_code(),
    );
    behavior::save_behavior(cache_dir, source_id, &discovered);
    discovered
}

/// Fetches GetCapabilities and picks the most-preferred CRS the server
/// declares. On any failure (network, parse, no usable CRS) returns a
/// fallback default — the layer will still try to load, just without the
/// benefit of the discovery's CRS preference.
fn discover_behavior_via_capabilities(
    base_url: &str,
    layer_name: &str,
    wms_version: &str,
) -> SourceBehavior {
    let url = capabilities::capabilities_url(base_url, wms_version);
    log::info!("WMS capabilities: {}", url);

    let xml = match ureq::get(&url).call() {
        Ok(resp) => match resp.into_body().read_to_string() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("WMS capabilities body read failed ({}): {}", base_url, e);
                return SourceBehavior::fallback_default();
            }
        },
        Err(e) => {
            log::warn!("WMS capabilities fetch failed ({}): {}", base_url, e);
            return SourceBehavior::fallback_default();
        }
    };

    match capabilities::parse_capabilities(&xml, layer_name) {
        Ok(caps) => match SourceBehavior::from_capabilities(&caps) {
            Some(b) => b,
            None => {
                log::warn!(
                    "WMS layer '{}' declares no CRS we recognise; using fallback",
                    layer_name
                );
                SourceBehavior::fallback_default()
            }
        },
        Err(e) => {
            log::warn!("WMS '{}' capabilities parse failed: {}", layer_name, e);
            SourceBehavior::fallback_default()
        }
    }
}

/// Reprojects a freshly decoded image into equirectangular if the discovered
/// behaviour calls for it. Pass-through for honest equirect sources.
pub fn apply_behavior_reproject(
    image: LayerImage,
    behavior: &SourceBehavior,
    label: &str,
) -> Result<LayerImage, String> {
    if !behavior.needs_reproject() {
        return Ok(image);
    }
    let reprojected = reproject::to_equirect(
        &image,
        behavior.response_crs,
        behavior.response_crs.world_bbox(),
        DEFAULT_WIDTH,
        DEFAULT_HEIGHT,
    )?;
    log::info!(
        "WMS reprojected {} → equirectangular: {}",
        behavior.response_crs.epsg_code(),
        label,
    );
    Ok(reprojected)
}

/// Decodes raw image bytes into RGBA pixel data.
///
/// Public wrapper for use by custom_source.rs.
pub fn decode_image_pub(raw_bytes: &[u8], label: &str) -> Result<LayerImage, String> {
    decode_image(raw_bytes, label)
}

/// Decodes raw image bytes into RGBA pixel data.
fn decode_image(raw_bytes: &[u8], label: &str) -> Result<LayerImage, String> {
    let img = image::load_from_memory(raw_bytes)
        .map_err(|e| format!("Image could not be decoded: {}", e))?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    log::info!(
        "WMS image ready: {} ({}×{})",
        label, width, height,
    );

    Ok(LayerImage {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

/// Public wrapper for cache freshness check (used by custom_source.rs).
pub fn is_cache_fresh_pub(path: &PathBuf, max_age_secs: u64) -> bool {
    is_cache_fresh(path, max_age_secs)
}

/// Checks if a cache file is fresh (younger than `max_age_secs`).
fn is_cache_fresh(path: &PathBuf, max_age_secs: u64) -> bool {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age.as_secs() < max_age_secs)
        .unwrap_or(false) // If metadata fails, re-download
}

// =============================================================================
// Public API
// =============================================================================

/// Returns all external WMS providers for registration in the catalog.
///
/// Called once at startup by `provider::build_default_catalog()`.
pub fn all_wms_providers() -> Vec<Box<dyn LayerProvider>> {
    WMS_LAYERS
        .iter()
        .map(|def| Box::new(WmsProvider::from_def(def)) as Box<dyn LayerProvider>)
        .collect()
}
