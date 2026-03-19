// =============================================================================
// Orbis — Layer Provider System (M9)
// =============================================================================
// Abstracts how layer images are obtained from various data sources.
//
// Architecture:
// - `LayerProvider` trait: defines how to fetch an image for a given date
// - `ProviderInfo`: metadata for the catalog UI (name, category, attribution)
// - `ProviderCatalog`: registry of all available providers (built-in + custom)
// - `LayerImage`: raw RGBA pixel data ready for GPU upload
//
// Each provider knows how to fetch a single equirectangular image
// (or a set of tiles composited into one). The layer system doesn't
// care about the source — it just needs RGBA pixels.
//
// Built-in providers:
// - GIBS (NASA Global Imagery Browse Services) — satellite imagery
// - Generic WMS (future: NOAA, DWD, Copernicus, etc.)
// - XYZ tiles (future: OpenStreetMap, etc.)
// =============================================================================

use chrono::NaiveDate;
use std::path::Path;

// =============================================================================
// Types
// =============================================================================

/// Raw image data ready for GPU upload.
///
/// This is the universal output format of all providers.
/// Regardless of source (GIBS, WMS, XYZ, static file),
/// every provider ultimately produces RGBA pixels.
pub struct LayerImage {
    /// RGBA pixel data (4 bytes per pixel, row-major)
    pub rgba: Vec<u8>,
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
}

/// Category for organizing providers in the catalog UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderCategory {
    /// True color, corrected reflectance (what Earth looks like)
    Satellite,
    /// Clouds, aerosols, ozone, air quality
    Atmosphere,
    /// Sea surface temperature, currents, waves, salinity
    Ocean,
    /// Vegetation (NDVI), land use, fires, deforestation
    Land,
    /// Temperature, precipitation, climate anomalies
    Climate,
    /// Snow cover, sea ice extent, glaciers
    Ice,
    /// OpenStreetMap, Natural Earth, borders, coastlines
    Basemap,
    /// Radar, forecasts, wind, pressure
    Weather,
    /// Soil types, geological maps, bathymetry
    Geology,
}

impl ProviderCategory {
    /// Returns all categories in display order.
    pub fn all() -> &'static [ProviderCategory] {
        &[
            ProviderCategory::Satellite,
            ProviderCategory::Atmosphere,
            ProviderCategory::Ocean,
            ProviderCategory::Land,
            ProviderCategory::Climate,
            ProviderCategory::Ice,
            ProviderCategory::Basemap,
            ProviderCategory::Weather,
            ProviderCategory::Geology,
        ]
    }

    /// Emoji + short label for the UI.
    pub fn label(&self) -> &'static str {
        match self {
            ProviderCategory::Satellite => "🛰 Satellite",
            ProviderCategory::Atmosphere => "🌫 Atmosphere",
            ProviderCategory::Ocean => "🌊 Ocean",
            ProviderCategory::Land => "🌿 Land",
            ProviderCategory::Climate => "🌡 Climate",
            ProviderCategory::Ice => "❄ Ice",
            ProviderCategory::Basemap => "🗺 Basemap",
            ProviderCategory::Weather => "⛅ Weather",
            ProviderCategory::Geology => "🪨 Geology",
        }
    }

    /// i18n key for the category label.
    #[allow(dead_code)]
    pub fn i18n_key(&self) -> &'static str {
        match self {
            ProviderCategory::Satellite => "cat_satellite",
            ProviderCategory::Atmosphere => "cat_atmosphere",
            ProviderCategory::Ocean => "cat_ocean",
            ProviderCategory::Land => "cat_land",
            ProviderCategory::Climate => "cat_climate",
            ProviderCategory::Ice => "cat_ice",
            ProviderCategory::Basemap => "cat_basemap",
            ProviderCategory::Weather => "cat_weather",
            ProviderCategory::Geology => "cat_geology",
        }
    }
}

/// Metadata about a layer provider (used in the catalog UI).
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Unique identifier (e.g. "gibs_viirs_true_color", "osm_standard")
    pub id: String,
    /// Display name (e.g. "VIIRS True Color")
    pub label: String,
    /// Short description (e.g. "Daily satellite imagery from Suomi-NPP")
    pub description: String,
    /// Category for grouping in the catalog
    pub category: ProviderCategory,
    /// Attribution text (required by data license)
    #[allow(dead_code)]
    pub attribution: String,
    /// Whether this provider supports date-based queries
    #[allow(dead_code)]
    pub supports_date: bool,
    /// Default opacity when adding this layer (0.0–1.0)
    pub default_opacity: f32,
    /// URL to a pre-generated legend image (if available).
    pub legend_url: Option<String>,
}

// =============================================================================
// Provider trait
// =============================================================================

/// Trait for all layer data sources.
///
/// Implementors fetch a single equirectangular image for a given date.
/// The image is then uploaded to the GPU as a texture.
///
/// All methods must be safe to call from a background thread
/// (hence `Send + Sync`).
pub trait LayerProvider: Send + Sync {
    /// Returns metadata about this provider.
    fn info(&self) -> &ProviderInfo;

    /// Fetches the layer image for a specific date.
    ///
    /// This method is **blocking** and should be called from a
    /// background thread to avoid blocking the render loop.
    ///
    /// The `cache_dir` is provided for disk caching — providers
    /// should cache aggressively to avoid redundant downloads.
    fn fetch(&self, date: &NaiveDate, cache_dir: &Path) -> Result<LayerImage, String>;

    /// Fetches the layer with automatic date fallback.
    ///
    /// Tries yesterday, then day-before-yesterday, etc.
    /// Default implementation tries 3 dates. Providers can override
    /// for custom fallback behavior.
    fn fetch_with_fallback(&self, cache_dir: &Path) -> Result<(LayerImage, NaiveDate), String> {
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
                        "Provider '{}' fetch for {} failed: {}",
                        self.info().id,
                        date.format("%Y-%m-%d"),
                        e
                    );
                }
            }
        }

        Err(format!(
            "Provider '{}': all fallback dates failed",
            self.info().id
        ))
    }
}

// =============================================================================
// Provider Catalog
// =============================================================================

/// Registry of all available layer providers.
///
/// Built-in providers are registered at startup. Custom providers
/// (user-defined WMS/WMTS/XYZ URLs) can be added at runtime.
pub struct ProviderCatalog {
    providers: Vec<Box<dyn LayerProvider>>,
}

impl ProviderCatalog {
    /// Creates a new empty catalog.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Registers a provider in the catalog.
    pub fn register(&mut self, provider: Box<dyn LayerProvider>) {
        log::info!(
            "Provider registered: '{}' ({})",
            provider.info().id,
            provider.info().label,
        );
        self.providers.push(provider);
    }

    /// Returns all providers in the catalog.
    #[allow(dead_code)]
    pub fn all(&self) -> &[Box<dyn LayerProvider>] {
        &self.providers
    }

    /// Returns providers filtered by category.
    pub fn by_category(&self, category: ProviderCategory) -> Vec<&dyn LayerProvider> {
        self.providers
            .iter()
            .filter(|p| p.info().category == category)
            .map(|p| p.as_ref())
            .collect()
    }

    /// Finds a provider by its unique ID.
    pub fn find(&self, id: &str) -> Option<&dyn LayerProvider> {
        self.providers
            .iter()
            .find(|p| p.info().id == id)
            .map(|p| p.as_ref())
    }

    /// Returns the number of registered providers.
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.providers.len()
    }

    /// Returns all categories that have at least one provider.
    pub fn active_categories(&self) -> Vec<ProviderCategory> {
        let mut cats: Vec<ProviderCategory> = ProviderCategory::all()
            .iter()
            .copied()
            .filter(|cat| self.providers.iter().any(|p| p.info().category == *cat))
            .collect();
        cats.sort_by_key(|c| {
            ProviderCategory::all()
                .iter()
                .position(|x| x == c)
                .unwrap_or(99)
        });
        cats
    }
}

/// Builds the default catalog with all built-in providers.
///
/// Called once at startup. This is the single place where all
/// built-in data sources are registered.
pub fn build_default_catalog() -> ProviderCatalog {
    let mut catalog = ProviderCatalog::new();

    // Register all GIBS providers
    for provider in crate::gibs::all_gibs_providers() {
        catalog.register(provider);
    }

    // Register all external WMS providers (M10)
    for provider in crate::wms::all_wms_providers() {
        catalog.register(provider);
    }

    log::info!(
        "Provider catalog ready: {} providers in {} categories",
        catalog.count(),
        catalog.active_categories().len(),
    );

    catalog
}
