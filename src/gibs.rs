// =============================================================================
// Orbis — NASA GIBS Providers (M5 + M9)
// =============================================================================
// Downloads satellite imagery from NASA's Global Imagery Browse Services (GIBS).
//
// GIBS offers >1000 layers with Earth observation data:
// - Corrected reflectance (True Color — Earth as seen from orbit)
// - Clouds, aerosols, temperature, vegetation, ...
// - Near-Real-Time: data ~3-5h after satellite overpass
//
// We use WMS (Web Map Service) instead of WMTS (tiles), because WMS
// can deliver a single image for the entire Earth.
// No tile stitching needed!
//
// Legal: NASA data is public domain. GIBS requires attribution:
//   "We acknowledge the use of imagery provided by services from NASA's
//    Global Imagery Browse Services (GIBS), part of NASA's Earth Science
//    Data and Information System (ESDIS)."
// =============================================================================

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::provider::{LayerImage, LayerProvider, ProviderCategory, ProviderInfo};

/// Base URL for GIBS WMS (EPSG:4326, best available data)
const GIBS_WMS_BASE: &str =
    "https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi";

/// Default resolution for downloaded images.
/// 2048×1024 is a good compromise: sharp enough for global view,
/// but only ~200–500 KB per download (JPEG).
const DEFAULT_WIDTH: u32 = 2048;
const DEFAULT_HEIGHT: u32 = 1024;

/// NASA GIBS attribution text (required by data license).
const GIBS_ATTRIBUTION: &str =
    "NASA GIBS / ESDIS";

// =============================================================================
// GIBS Layer Definition
// =============================================================================

/// Static definition of a GIBS layer.
///
/// These are compile-time constants describing each available GIBS layer.
/// At runtime, each definition becomes a `GibsProvider` instance.
#[derive(Debug, Clone)]
struct GibsLayerDef {
    /// Provider ID (used in settings, cache paths, etc.)
    id: &'static str,
    /// GIBS internal layer name (e.g. "VIIRS_SNPP_CorrectedReflectance_TrueColor")
    gibs_id: &'static str,
    /// Display name for the GUI
    label: &'static str,
    /// Short description
    description: &'static str,
    /// Image format: "image/jpeg" or "image/png"
    format: &'static str,
    /// File extension for cache
    extension: &'static str,
    /// Category for catalog grouping
    category: ProviderCategory,
    /// Default opacity (0.0–1.0)
    default_opacity: f32,
    /// URL to a pre-generated horizontal legend PNG (if available).
    /// None for layers without color scales (e.g. true-color imagery).
    legend_url: Option<&'static str>,
}

// =============================================================================
// All GIBS Layer Definitions
// =============================================================================

const GIBS_LAYERS: &[GibsLayerDef] = &[
    // --- Satellite (True Color) ---
    GibsLayerDef {
        id: "gibs_viirs_true_color",
        gibs_id: "VIIRS_SNPP_CorrectedReflectance_TrueColor",
        label: "VIIRS True Color (Suomi-NPP)",
        description: "Daily true-color satellite image from the VIIRS instrument on Suomi-NPP. Shows Earth as seen from orbit, including clouds.",
        format: "image/jpeg",
        extension: "jpg",
        category: ProviderCategory::Satellite,
        default_opacity: 0.45,
        legend_url: None, // True color = photo, no color scale
    },
    GibsLayerDef {
        id: "gibs_modis_terra_true_color",
        gibs_id: "MODIS_Terra_CorrectedReflectance_TrueColor",
        label: "MODIS Terra True Color",
        description: "Daily true-color image from MODIS on the Terra satellite. Older sensor, useful as alternative to VIIRS.",
        format: "image/jpeg",
        extension: "jpg",
        category: ProviderCategory::Satellite,
        default_opacity: 0.45,
        legend_url: None,
    },
    GibsLayerDef {
        id: "gibs_modis_aqua_true_color",
        gibs_id: "MODIS_Aqua_CorrectedReflectance_TrueColor",
        label: "MODIS Aqua True Color",
        description: "Daily true-color image from MODIS on the Aqua satellite. Afternoon overpass complements Terra's morning pass.",
        format: "image/jpeg",
        extension: "jpg",
        category: ProviderCategory::Satellite,
        default_opacity: 0.45,
        legend_url: None,
    },

    // --- Atmosphere ---
    GibsLayerDef {
        id: "gibs_modis_terra_aod",
        gibs_id: "MODIS_Terra_Aerosol_Optical_Depth_3km",
        label: "Aerosol Optical Depth (Terra)",
        description: "Atmospheric aerosol concentration measured by MODIS Terra. Useful for tracking dust storms, smoke, and pollution.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Atmosphere,
        default_opacity: 0.6,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_VIIRS_AOD_H.png"),
    },
    GibsLayerDef {
        id: "gibs_airs_ozone",
        gibs_id: "OMI_Ozone_TOMS_Total_Column",
        label: "Ozone Total Column (OMI)",
        description: "Total ozone column from the OMI instrument on Aura. Shows ozone layer thickness in Dobson units using the TOMS algorithm.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Atmosphere,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/OMI_Ozone_TOMS_Total_Column_H.png"),
    },
    GibsLayerDef {
        id: "gibs_airs_co_total",
        gibs_id: "AIRS_L3_Carbon_Monoxide_500hPa_Volume_Mixing_Ratio_Daily_Day",
        label: "Carbon Monoxide 500hPa (AIRS)",
        description: "Mid-troposphere carbon monoxide mixing ratio at 500 hPa from AIRS. Useful for tracking wildfire smoke and industrial emissions.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Atmosphere,
        default_opacity: 0.5,
        legend_url: None, // No pre-generated legend available
    },

    // --- Ocean ---
    GibsLayerDef {
        id: "gibs_modis_terra_sst",
        gibs_id: "MODIS_Terra_L2_Sea_Surface_Temp_Day",
        label: "Sea Surface Temperature (Terra)",
        description: "Daytime sea surface temperature from MODIS Terra. Color-mapped from cold (blue) to warm (red).",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Ocean,
        default_opacity: 0.6,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_Sea_Surface_Temperature_H.png"),
    },
    GibsLayerDef {
        id: "gibs_modis_aqua_sst",
        gibs_id: "MODIS_Aqua_L2_Sea_Surface_Temp_Day",
        label: "Sea Surface Temperature (Aqua)",
        description: "Daytime sea surface temperature from MODIS Aqua.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Ocean,
        default_opacity: 0.6,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_Sea_Surface_Temperature_H.png"),
    },
    GibsLayerDef {
        id: "gibs_modis_aqua_chlorophyll",
        gibs_id: "MODIS_Aqua_L2_Chlorophyll_A",
        label: "Chlorophyll-a (Aqua)",
        description: "Ocean chlorophyll concentration from MODIS Aqua. Indicates phytoplankton density and ocean productivity.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Ocean,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_Chlorophyll_H.png"),
    },

    // --- Land ---
    GibsLayerDef {
        id: "gibs_modis_terra_ndvi",
        gibs_id: "MODIS_Terra_NDVI_8Day",
        label: "Vegetation Index NDVI (Terra)",
        description: "8-day composite Normalized Difference Vegetation Index. Green = dense vegetation, brown = sparse.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Land,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_NDVI_H.png"),
    },
    GibsLayerDef {
        id: "gibs_modis_terra_lst_day",
        gibs_id: "MODIS_Terra_Land_Surface_Temp_Day",
        label: "Land Surface Temperature Day (Terra)",
        description: "Daytime land surface temperature from MODIS Terra. Not air temperature — measures actual ground radiative temperature.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Climate,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_Land_Surface_Temp_H.png"),
    },
    GibsLayerDef {
        id: "gibs_modis_terra_lst_night",
        gibs_id: "MODIS_Terra_Land_Surface_Temp_Night",
        label: "Land Surface Temperature Night (Terra)",
        description: "Nighttime land surface temperature from MODIS Terra.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Climate,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_Land_Surface_Temp_H.png"),
    },
    GibsLayerDef {
        id: "gibs_firms_modis",
        gibs_id: "MODIS_Terra_Thermal_Anomalies_Day",
        label: "Thermal Anomalies / Fires (Terra)",
        description: "Active fire detections from MODIS Terra. Shows wildfires, volcanic activity, and industrial heat sources.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Land,
        default_opacity: 0.7,
        legend_url: None, // Fire = binary points, no gradient
    },
    GibsLayerDef {
        id: "gibs_firms_viirs",
        gibs_id: "VIIRS_SNPP_Thermal_Anomalies_375m_Day",
        label: "Thermal Anomalies / Fires (VIIRS)",
        description: "Active fire detections from VIIRS at 375m resolution. Higher resolution than MODIS fire product.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Land,
        default_opacity: 0.7,
        legend_url: None,
    },

    // --- Ice ---
    GibsLayerDef {
        id: "gibs_modis_terra_snow",
        gibs_id: "MODIS_Terra_NDSI_Snow_Cover",
        label: "Snow Cover (Terra)",
        description: "Snow cover extent from MODIS Terra using the Normalized Difference Snow Index.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Ice,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/MODIS_NDSI_Snow_Cover_H.png"),
    },
    GibsLayerDef {
        id: "gibs_sea_ice_brightness",
        gibs_id: "AMSRU2_Sea_Ice_Concentration_12km",
        label: "Sea Ice Concentration (AMSR2)",
        description: "Sea ice concentration at 12 km resolution from AMSR2 on GCOM-W1. Shows ice extent in polar regions.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Ice,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/AMSR_Sea_Ice_Concentration_H.png"),
    },

    // --- Climate ---
    GibsLayerDef {
        id: "gibs_imerg_precipitation",
        gibs_id: "IMERG_Precipitation_Rate",
        label: "Precipitation Rate (IMERG)",
        description: "Global precipitation rate from the GPM/IMERG mission. Shows rainfall and snowfall intensity worldwide.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Climate,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/GPM_Precipitation_Rate_H.png"),
    },
    GibsLayerDef {
        id: "gibs_airs_surface_air_temp",
        gibs_id: "AIRS_L3_Surface_Air_Temperature_Daily_Day",
        label: "Surface Air Temperature (AIRS)",
        description: "Daily surface air temperature from the AIRS instrument on Aqua.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Climate,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/AIRS_Surface_Air_Temperature_Daily_Day_H.png"),
    },
    GibsLayerDef {
        id: "gibs_airs_relative_humidity",
        gibs_id: "AIRS_L2_RelativeHumidity_500hPa_Day",
        label: "Relative Humidity 500hPa (AIRS)",
        description: "Mid-troposphere relative humidity at 500 hPa from AIRS (Level 2, daytime swath).",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Atmosphere,
        default_opacity: 0.5,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/AIRS_RelativeHumidity_H.png"),
    },

    // --- Night Lights ---
    GibsLayerDef {
        id: "gibs_viirs_dnb",
        gibs_id: "VIIRS_SNPP_DayNightBand_At_Sensor_Radiance",
        label: "Night Lights / Day-Night Band (VIIRS)",
        description: "VIIRS Day-Night Band showing city lights, moonlit clouds, and auroras. Best viewed with low opacity over the night side.",
        format: "image/png",
        extension: "png",
        category: ProviderCategory::Satellite,
        default_opacity: 0.4,
        legend_url: Some("https://gibs.earthdata.nasa.gov/legends/VIIRS_DayNightBand_At_Sensor_Radiance_H.png"),
    },
];

// =============================================================================
// GibsProvider
// =============================================================================

/// A GIBS-backed layer provider.
///
/// Each instance represents one GIBS layer (e.g. VIIRS True Color,
/// MODIS SST, etc.). Implements the `LayerProvider` trait for
/// integration with the catalog system.
pub struct GibsProvider {
    info: ProviderInfo,
    gibs_id: String,
    format: String,
    extension: String,
}

impl GibsProvider {
    /// Creates a provider from a static layer definition.
    fn from_def(def: &GibsLayerDef) -> Self {
        Self {
            info: ProviderInfo {
                id: def.id.to_string(),
                label: def.label.to_string(),
                description: def.description.to_string(),
                category: def.category,
                attribution: GIBS_ATTRIBUTION.to_string(),
                supports_date: true,
                default_opacity: def.default_opacity,
                legend_url: def.legend_url.map(|s| s.to_string()),
            },
            gibs_id: def.gibs_id.to_string(),
            format: def.format.to_string(),
            extension: def.extension.to_string(),
        }
    }
}

impl LayerProvider for GibsProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    fn fetch(&self, date: &NaiveDate, cache_dir: &Path) -> Result<LayerImage, String> {
        // Create cache directory if needed
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Could not create cache directory: {}", e))?;

        let cached = cache_path(cache_dir, &self.gibs_id, &self.extension, date);

        // 1. Cache hit?
        let raw_bytes = if cached.exists() {
            log::info!(
                "GIBS cache hit: {} ({})",
                self.info.label,
                cached.display()
            );
            fs::read(&cached)
                .map_err(|e| format!("Could not read cache file: {}", e))?
        } else {
            // 2. Download from GIBS
            let url = build_wms_url(&self.gibs_id, &self.format, date);
            log::info!("GIBS download: {} → {}", self.info.label, url);

            let response = ureq::get(&url)
                .call()
                .map_err(|e| format!(
                    "GIBS download failed ({}): {}",
                    date.format("%Y-%m-%d"),
                    e
                ))?;

            // Check if the response is an image (not an XML error)
            let is_error = response
                .headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.contains("xml") || ct.contains("text"))
                .unwrap_or(false);

            if is_error {
                let body = response
                    .into_body()
                    .read_to_string()
                    .map_err(|e| format!("Error reading error response: {}", e))?;
                return Err(format!(
                    "GIBS returned an error (not an image): {}",
                    &body[..body.len().min(500)]
                ));
            }

            // Read image bytes
            let bytes = response
                .into_body()
                .read_to_vec()
                .map_err(|e| format!("Error reading image data: {}", e))?;

            log::info!(
                "GIBS download complete: {} ({} KB)",
                self.info.label,
                bytes.len() / 1024
            );

            // 3. Save to cache
            if let Err(e) = fs::write(&cached, &bytes) {
                log::warn!("Cache save failed: {}", e);
            }

            bytes
        };

        // 4. Decode image → RGBA
        let img = image::load_from_memory(&raw_bytes)
            .map_err(|e| format!("Image could not be decoded: {}", e))?;

        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();

        log::info!(
            "GIBS image ready: {} ({}×{}, {})",
            self.info.label,
            width,
            height,
            date.format("%Y-%m-%d"),
        );

        Ok(LayerImage {
            rgba: rgba.into_raw(),
            width,
            height,
        })
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Builds the WMS GetMap URL for a GIBS layer and date.
fn build_wms_url(gibs_id: &str, format: &str, date: &NaiveDate) -> String {
    format!(
        "{}?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS={}&FORMAT={}&WIDTH={}&HEIGHT={}&BBOX=-90,-180,90,180&CRS=EPSG:4326&TIME={}&STYLES=",
        GIBS_WMS_BASE,
        gibs_id,
        format,
        DEFAULT_WIDTH,
        DEFAULT_HEIGHT,
        date.format("%Y-%m-%d"),
    )
}

/// Path for the cached file.
///
/// Schema: `{cache_dir}/{gibs_id}_{YYYY-MM-DD}.{ext}`
fn cache_path(cache_dir: &Path, gibs_id: &str, extension: &str, date: &NaiveDate) -> PathBuf {
    cache_dir.join(format!(
        "{}_{}.{}",
        gibs_id,
        date.format("%Y-%m-%d"),
        extension,
    ))
}

// =============================================================================
// Public API
// =============================================================================

/// Returns all GIBS providers for registration in the catalog.
///
/// Called once at startup by `provider::build_default_catalog()`.
pub fn all_gibs_providers() -> Vec<Box<dyn LayerProvider>> {
    GIBS_LAYERS
        .iter()
        .map(|def| Box::new(GibsProvider::from_def(def)) as Box<dyn LayerProvider>)
        .collect()
}
