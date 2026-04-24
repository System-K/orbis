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

pub mod capabilities;
pub mod crs;
pub mod reproject;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::provider::{LayerImage, LayerProvider, ProviderCategory, ProviderInfo};
use crs::Crs;

/// Default resolution for WMS downloads (same as GIBS).
const DEFAULT_WIDTH: u32 = 2048;
const DEFAULT_HEIGHT: u32 = 1024;

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
    /// If true, the source image is assumed to be Web Mercator
    /// and will be reprojected to equirectangular after download.
    /// Needed for tile-based services (OSM, OpenTopoMap) that render
    /// in Mercator internally even when EPSG:4326 is requested.
    reproject_mercator: bool,
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
        reproject_mercator: true,
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
        reproject_mercator: true,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
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
        reproject_mercator: false,
    },
];

// =============================================================================
// WmsProvider
// =============================================================================

/// A generic WMS-backed layer provider.
///
/// Fetches equirectangular images from any OGC WMS 1.3.0 server.
/// Supports both time-varying (forecasts, observations) and
/// static (basemaps) layers.
pub struct WmsProvider {
    info: ProviderInfo,
    base_url: String,
    layer_name: String,
    format: String,
    extension: String,
    uses_time: bool,
    transparent: bool,
    reproject_mercator: bool,
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
            reproject_mercator: def.reproject_mercator,
        }
    }

    /// Builds the WMS GetMap URL.
    ///
    /// For Mercator-native sources (OSM, OpenTopoMap), requests in EPSG:3857
    /// to get clean native-projection pixels. We reproject to equirectangular
    /// ourselves, because Terrestris' server-side reprojection is broken.
    fn build_url(&self, date: Option<&NaiveDate>) -> String {
        let mut url = if self.reproject_mercator {
            // Request in native Web Mercator (square image)
            let extent = 20037508.3427892_f64;
            format!(
                "{}?SERVICE=WMS&VERSION=1.1.1&REQUEST=GetMap&LAYERS={}&FORMAT={}&WIDTH={}&HEIGHT={}&SRS=EPSG:3857&BBOX={},{},{},{}&STYLES=",
                self.base_url,
                self.layer_name,
                self.format,
                DEFAULT_WIDTH,
                DEFAULT_WIDTH, // Square! Mercator world map is square
                -extent, -extent, extent, extent,
            )
        } else {
            // Standard EPSG:4326 equirectangular request
            format!(
                "{}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS={}&FORMAT={}&WIDTH={}&HEIGHT={}&BBOX=-90,-180,90,180&CRS=EPSG:4326&STYLES=",
                self.base_url,
                self.layer_name,
                self.format,
                DEFAULT_WIDTH,
                DEFAULT_HEIGHT,
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

impl LayerProvider for WmsProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn fetch(&self, date: &NaiveDate, cache_dir: &Path) -> Result<LayerImage, String> {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Could not create cache directory: {}", e))?;

        // For timeless layers (basemaps): cache without date in filename
        let cached = if self.uses_time {
            cache_dir.join(format!(
                "{}_{}.{}",
                self.info.id,
                date.format("%Y-%m-%d"),
                self.extension,
            ))
        } else {
            cache_dir.join(format!(
                "{}.{}",
                self.info.id,
                self.extension,
            ))
        };

        // 1. Cache hit?
        if cached.exists() {
            // For timeless layers, check if cache is older than 24h
            let use_cache = if self.uses_time {
                true // Date-based layers are immutable per date
            } else {
                is_cache_fresh(&cached, 24 * 3600) // Basemaps: refresh daily
            };

            if use_cache {
                log::info!(
                    "WMS cache hit: {} ({})",
                    self.info.label,
                    cached.display()
                );
                let raw_bytes = fs::read(&cached)
                    .map_err(|e| format!("Could not read cache file: {}", e))?;
                let mut image = decode_image(&raw_bytes, &self.info.label)?;
                if self.reproject_mercator {
                    image = reproject_mercator_to_equirect(image)?;
                }
                return Ok(image);
            }
        }

        // 2. Download from WMS
        let time_param = if self.uses_time { Some(date) } else { None };
        let url = self.build_url(time_param);
        log::info!("WMS download: {} → {}", self.info.label, url);

        let response = ureq::get(&url)
            .call()
            .map_err(|e| format!(
                "WMS download failed ({}): {}",
                self.info.id, e
            ))?;

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

        // Read image bytes
        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|e| format!("Error reading image data: {}", e))?;

        log::info!(
            "WMS download complete: {} ({} KB)",
            self.info.label,
            bytes.len() / 1024
        );

        // 3. Save to cache
        if let Err(e) = fs::write(&cached, &bytes) {
            log::warn!("WMS cache save failed: {}", e);
        }

        // 4. Decode image → RGBA
        let mut layer_image = decode_image(&bytes, &self.info.label)?;

        // 5. Reproject Mercator → Equirectangular if needed
        if self.reproject_mercator {
            layer_image = reproject_mercator_to_equirect(layer_image)?;
            log::info!(
                "WMS reprojected Mercator → Equirectangular: {}",
                self.info.label
            );
        }

        Ok(layer_image)
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
// Helper functions
// =============================================================================

/// Reprojects a Web Mercator image (square, EPSG:3857) to equirectangular.
///
/// Input:  2048×2048 (or any square) Mercator image covering ±85.0511° latitude
/// Output: 2048×1024 equirectangular image covering ±90° latitude
///
/// Web Mercator maps latitude with: y = ln(tan(π/4 + φ/2))
/// This function inverts that mapping for each output row.
/// Poles beyond ±85.05° are filled with transparent pixels.
///
/// Public wrapper for use by custom_source.rs.
pub fn reproject_mercator_pub(src: LayerImage) -> Result<LayerImage, String> {
    reproject_mercator_to_equirect(src)
}

fn reproject_mercator_to_equirect(src: LayerImage) -> Result<LayerImage, String> {
    reproject::to_equirect(
        &src,
        Crs::WebMercator,
        Crs::WebMercator.world_bbox(),
        DEFAULT_WIDTH,
        DEFAULT_HEIGHT,
    )
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
