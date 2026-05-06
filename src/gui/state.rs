// =============================================================================
// Orbis — GUI State & Types
// =============================================================================

use std::collections::HashMap;
use std::sync::mpsc;

use chrono::{Datelike, NaiveDate, Timelike, Utc};

use crate::i18n;
use crate::settings::Settings;


/// Form state for the "Add Custom Source" dialog (M17e).
///
/// Holds all input fields. On submit, converted to a CustomSourceConfig
/// and appended to the config file.
pub struct CustomSourceForm {
    /// Display name
    pub name: String,
    /// Source type: 0=WMS, 1=XYZ, 2=REST
    pub source_type_idx: usize,
    /// Category index into CATEGORIES array
    pub category_idx: usize,
    /// Attribution text
    pub attribution: String,
    /// Default opacity
    pub opacity: f32,
    // --- WMS fields ---
    pub wms_base_url: String,
    pub wms_layer_name: String,
    pub wms_format_png: bool, // true=PNG, false=JPEG
    pub wms_transparent: bool,
    pub wms_uses_time: bool,
    /// WMS version: true = 1.3.0, false = 1.1.1
    pub wms_version_130: bool,
    /// GetCapabilities detection status
    pub wms_caps_status: crate::wms_caps::CapsStatus,
    /// Background receiver for GetCapabilities result
    pub wms_caps_rx: Option<std::sync::mpsc::Receiver<Result<crate::wms_caps::WmsCapabilities, String>>>,
    /// Selected layer index in the capabilities layer list
    pub wms_caps_layer_idx: usize,
    // --- XYZ fields (M17c) ---
    pub xyz_url_template: String,
    pub xyz_max_zoom: u32,
    // --- REST fields (M17d) ---
    pub rest_url: String,
    pub rest_refresh_secs: u64,
    // --- Shapefile fields (M17h) ---
    pub shp_path: String,
    // --- CSV fields (M17i) ---
    pub csv_path: String,
    // --- HTTP headers editor (M17g, applies to WMS/XYZ/REST) ---
    /// Vec rather than HashMap for stable row ordering in the UI; converted
    /// to HashMap on save (empty-key rows dropped).
    pub headers: Vec<(String, String)>,
}

impl Default for CustomSourceForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            source_type_idx: 0,
            category_idx: 0,
            attribution: String::new(),
            opacity: 0.5,
            wms_base_url: String::new(),
            wms_layer_name: String::new(),
            wms_format_png: true,
            wms_transparent: true,
            wms_uses_time: false,
            wms_version_130: true,
            wms_caps_status: crate::wms_caps::CapsStatus::Idle,
            wms_caps_rx: None,
            wms_caps_layer_idx: 0,
            xyz_url_template: String::new(),
            xyz_max_zoom: 18,
            rest_url: String::new(),
            rest_refresh_secs: 300,
            shp_path: String::new(),
            csv_path: String::new(),
            headers: Vec::new(),
        }
    }
}

/// Category labels for the combo box.
pub const SOURCE_CATEGORIES: &[(&str, &str)] = &[
    ("satellite", "Satellite"),
    ("atmosphere", "Atmosphere"),
    ("ocean", "Ocean"),
    ("land", "Land"),
    ("climate", "Climate"),
    ("ice", "Ice"),
    ("basemap", "Basemap"),
    ("weather", "Weather"),
    ("geology", "Geology"),
];

/// Source type labels for the combo box.
pub const SOURCE_TYPES: &[&str] = &["WMS", "XYZ Tiles", "REST/GeoJSON", "Shapefile", "CSV"];

/// Data that the GUI can display and modify.
pub struct GuiState {
    /// FPS display value
    pub fps: f32,
    /// Layer information (id, label, enabled, opacity, provider_id)
    pub layers: Vec<LayerGuiEntry>,
    /// Whether the layer panel is open
    pub panel_open: bool,
    /// View mode: false = 3D globe, true = 2D map
    pub view_mode_map: bool,

    // --- Catalog browser (M9) ---
    /// Whether the catalog browser is currently shown
    pub catalog_open: bool,
    /// Provider ID that the user just selected to add
    pub add_provider_request: Option<String>,
    /// Provider ID that the user wants to remove
    pub remove_layer_request: Option<String>,
    /// Download status per provider_id
    pub download_status: Vec<DownloadStatusEntry>,
    /// Flag: layer config changed → save settings
    pub layers_changed: bool,

    // --- Time control ---
    pub time_live: bool,
    pub selected_year: i32,
    pub selected_month: u32,
    pub selected_day: u32,
    pub selected_hour: u32,
    pub selected_minute: u32,
    pub date_changed: bool,

    // --- Settings (persistent) ---
    pub settings: Settings,
    pub settings_dirty: bool,

    // --- GeoJSON (M11e) ---
    /// Screen-space labels for current frame
    pub geo_labels: Vec<crate::label::ScreenLabel>,
    /// Whether labels are shown
    pub labels_visible: bool,
    /// Current panel width in logical pixels (0 if closed)
    pub panel_width: f32,
    /// Whether the user clicked "Load GeoJSON"
    pub load_geojson_request: bool,
    /// Paths dropped onto the window
    pub dropped_files: Vec<std::path::PathBuf>,
    /// GeoJSON layer info for the GUI (name, visible, point/line/polygon counts)
    pub geo_layer_info: Vec<GeoLayerInfo>,
    /// Attribution strings collected from active GeoJSON layers
    pub geo_attributions: Vec<String>,
    /// Request to remove a GeoJSON layer by name
    pub remove_geo_layer_request: Option<String>,
    /// Request to toggle a GeoJSON layer by name
    pub toggle_geo_layer_request: Option<String>,
    /// Status message after GeoJSON load (auto-clears after a few seconds)
    pub geojson_status: Option<(String, std::time::Instant, bool)>, // (msg, when, is_error)
    /// URL input for loading GeoJSON from web
    pub geojson_url_input: String,
    /// Request to load GeoJSON from URL
    pub load_geojson_url_request: Option<String>,

    // --- Live Sources (M12) ---
    /// Request to activate a live source by ID
    pub activate_live_source: Option<String>,
    /// Request to deactivate a live source by ID
    pub deactivate_live_source: Option<String>,
    /// Currently active live source IDs (synced from LiveSourceManager)
    pub active_live_sources: Vec<String>,

    // --- Label interaction (M12) ---
    /// Set of label texts whose cluster is currently expanded
    pub expanded_labels: std::collections::HashSet<String>,

    // --- Legend panel (M12d) ---
    /// Whether the legend panel is visible
    pub legend_open: bool,
    /// Cached egui textures for GIBS legend images (keyed by provider_id)
    pub legend_textures: HashMap<String, egui::TextureHandle>,
    /// Pending legend image downloads (provider_id, receiver)
    pub legend_downloads: Vec<(String, mpsc::Receiver<Option<egui::ColorImage>>)>,
    /// Provider IDs for which a legend download has already been requested
    pub legend_requested: std::collections::HashSet<String>,

    // --- Satellite tracking (M13) ---
    /// Whether satellite markers are shown
    pub satellites_visible: bool,
    /// Set of enabled satellite NORAD IDs (only these are rendered)
    pub enabled_satellites: std::collections::HashSet<u32>,
    /// All available satellites (norad_id, name) for GUI toggle list
    pub all_satellites: Vec<(u32, String)>,
    /// Satellite screen positions for rendering (updated each frame)
    pub satellite_markers: Vec<SatelliteMarker>,
    /// Ground track screen points per satellite (norad_id, past_points, future_points)
    pub satellite_tracks: Vec<SatelliteTrack>,
    /// Number of tracked satellites (for GUI status)
    pub satellite_count: usize,
    /// Whether OMM download is in progress
    pub satellite_downloading: bool,
    /// NORAD ID of satellite to follow (None = free camera)
    pub follow_satellite: Option<u32>,

    // --- Planets (M14b) ---
    /// Planet screen positions for rendering (updated each frame)
    pub planet_markers: Vec<PlanetMarker>,

    // --- Language (M15b) ---
    /// Cached list of available languages (code, display_name).
    pub available_languages: Vec<(String, String)>,

    // --- Tile Cache (M16b) ---
    /// Current cache usage in MB (updated periodically, not every frame)
    pub cache_usage_mb: f32,
    /// Request to clear tile cache (consumed by main.rs)
    pub cache_clear_request: bool,
    /// Available tile sources (id, display_name) — populated at startup
    pub tile_sources: Vec<(String, String)>,
    /// Latest TileManager metrics snapshot (refreshed each frame by main.rs)
    pub tile_metrics: crate::tile::TileMetrics,
    /// Meters of ground distance covered by one screen pixel (vertical).
    ///
    /// Set each frame by `main.rs::update_tiles` based on the active view
    /// mode + camera state. Consumed by `gui::scale::draw_scale_hud` to
    /// render the scale bar + distance label. 0.0 means "not yet computed";
    /// the HUD hides itself in that case.
    pub scale_meters_per_pixel: f32,

    // --- Custom Sources (M17e) ---
    /// Whether the "Add Custom Source" dialog window is open
    pub custom_source_dialog_open: bool,
    /// Form state for the dialog
    pub custom_source_form: CustomSourceForm,
    /// Loaded custom sources config (for display + management)
    pub custom_sources_config: crate::custom_source::CustomSourcesConfig,
    /// Request to reload the provider catalog (after config change)
    pub reload_catalog_request: bool,
    /// Status message for custom source operations
    pub custom_source_status: Option<(String, std::time::Instant, bool)>,
}

/// Screen-space satellite marker for rendering on the globe.
pub struct SatelliteMarker {
    /// Screen X position
    pub x: f32,
    /// Screen Y position
    pub y: f32,
    /// Display name
    pub name: String,
    /// NORAD catalog number
    pub norad_id: u32,
    /// Altitude in km (for tooltip)
    pub altitude_km: f64,
    /// Velocity in km/s (for tooltip)
    pub velocity_km_s: f64,
    /// Whether the satellite is on the visible side of the globe
    pub visible: bool,
}

/// Screen-space planet marker for rendering on the sky sphere.
pub struct PlanetMarker {
    pub x: f32,
    pub y: f32,
    pub name: &'static str,
    pub color: [f32; 3],
    pub radius: f32,
    pub visible: bool,
}

/// Screen-projected ground track for a satellite.
///
/// Tracks are stored as multiple line segments (not one continuous line)
/// because sections behind the globe must be clipped.
pub struct SatelliteTrack {
    /// NORAD catalog number (links track to satellite marker)
    #[allow(dead_code)]
    pub norad_id: u32,
    /// Past orbit path — list of visible line segments
    pub past_segments: Vec<Vec<egui::Pos2>>,
    /// Future orbit path — list of visible line segments
    pub future_segments: Vec<Vec<egui::Pos2>>,
}

/// Display info for a GeoJSON layer in the GUI.
pub struct GeoLayerInfo {
    pub name: String,
    pub visible: bool,
    pub point_count: usize,
    pub line_count: usize,
    pub polygon_count: usize,
}

/// Download status for a provider.
#[derive(Clone)]
pub struct DownloadStatusEntry {
    pub provider_id: String,
    pub status: DownloadStatus,
}

/// Download status for a single layer download.
#[derive(Clone)]
pub enum DownloadStatus {
    /// Download in progress
    Downloading,
    /// Successfully loaded
    Ready,
    /// Download failed
    Error(String),
}

/// A layer entry for the GUI.
pub struct LayerGuiEntry {
    pub id: String,
    pub label: String,
    pub provider_id: String,
    pub enabled: bool,
    pub opacity: f32,
}

impl GuiState {
    pub fn new(settings: Settings) -> Self {
        let now = Utc::now();
        Self {
            fps: 0.0,
            layers: Vec::new(),
            panel_open: true,
            view_mode_map: false,
            catalog_open: false,
            add_provider_request: None,
            remove_layer_request: None,
            download_status: Vec::new(),
            layers_changed: false,
            time_live: true,
            selected_year: now.year(),
            selected_month: now.month(),
            selected_day: now.day(),
            selected_hour: now.hour(),
            selected_minute: now.minute(),
            date_changed: false,
            settings,
            settings_dirty: false,
            geo_labels: Vec::new(),
            labels_visible: true,
            panel_width: 0.0,
            load_geojson_request: false,
            dropped_files: Vec::new(),
            geo_layer_info: Vec::new(),
            geo_attributions: Vec::new(),
            remove_geo_layer_request: None,
            toggle_geo_layer_request: None,
            geojson_status: None,
            geojson_url_input: String::new(),
            load_geojson_url_request: None,
            activate_live_source: None,
            deactivate_live_source: None,
            active_live_sources: Vec::new(),
            expanded_labels: std::collections::HashSet::new(),
            legend_open: false,
            legend_textures: HashMap::new(),
            legend_downloads: Vec::new(),
            legend_requested: std::collections::HashSet::new(),
            satellites_visible: true,
            enabled_satellites: crate::satellite::builtin_satellites()
                .iter().map(|s| s.norad_id).collect(),
            all_satellites: crate::satellite::builtin_satellites()
                .iter().map(|s| (s.norad_id, s.name.to_string())).collect(),
            satellite_markers: Vec::new(),
            satellite_tracks: Vec::new(),
            satellite_count: 0,
            satellite_downloading: false,
            follow_satellite: None,
            planet_markers: Vec::new(),
            available_languages: i18n::available_languages(),
            cache_usage_mb: 0.0,
            cache_clear_request: false,
            tile_sources: crate::tile::builtin_tile_sources()
                .iter()
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect(),
            tile_metrics: crate::tile::TileMetrics::default(),
            scale_meters_per_pixel: 0.0,
            custom_source_dialog_open: false,
            custom_source_form: CustomSourceForm::default(),
            custom_sources_config: crate::custom_source::load_config(),
            reload_catalog_request: false,
            custom_source_status: None,
        }
    }

    /// Returns the currently selected date (for downloads).
    pub fn selected_date(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.selected_year, self.selected_month, self.selected_day)
    }

    /// Maximum days in the selected month (accounts for leap years).
    pub(crate) fn max_day(&self) -> u32 {
        if self.selected_month == 12 {
            NaiveDate::from_ymd_opt(self.selected_year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(self.selected_year, self.selected_month + 1, 1)
        }
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
    }

    /// Returns the download status for a given provider.
    pub fn get_download_status(&self, provider_id: &str) -> Option<&DownloadStatus> {
        self.download_status
            .iter()
            .find(|e| e.provider_id == provider_id)
            .map(|e| &e.status)
    }

    /// Polls pending legend image downloads and loads completed ones as textures.
    pub fn poll_legend_downloads(&mut self, ctx: &egui::Context) {
        let mut still_pending = Vec::new();
        for (provider_id, rx) in self.legend_downloads.drain(..) {
            match rx.try_recv() {
                Ok(Some(image)) => {
                    let tex = ctx.load_texture(
                        format!("legend_{}", provider_id),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    log::info!("Legend loaded for '{}'", provider_id);
                    self.legend_textures.insert(provider_id, tex);
                }
                Ok(None) => {
                    log::warn!("Legend download failed for '{}'", provider_id);
                    // Don't retry — leave in requested set so we don't spam
                }
                Err(mpsc::TryRecvError::Empty) => {
                    still_pending.push((provider_id, rx));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Legend download thread disconnected for '{}'", provider_id);
                }
            }
        }
        self.legend_downloads = still_pending;
    }

    /// Requests a legend image download for a provider (if not already requested).
    pub fn request_legend_download(&mut self, provider_id: &str, url: &str) {
        if self.legend_textures.contains_key(provider_id) {
            return; // Already loaded
        }
        if self.legend_requested.contains(provider_id) {
            return; // Already downloading or failed
        }
        self.legend_requested.insert(provider_id.to_string());

        let url = url.to_string();
        let pid = provider_id.to_string();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            log::info!("Downloading legend for '{}': {}", pid, url);
            let result = download_legend_image(&url);
            let _ = tx.send(result);
        });

        self.legend_downloads.push((provider_id.to_string(), rx));
    }

    /// Sets the download status for a provider.
    pub fn set_download_status(&mut self, provider_id: &str, status: DownloadStatus) {
        if let Some(entry) = self
            .download_status
            .iter_mut()
            .find(|e| e.provider_id == provider_id)
        {
            entry.status = status;
        } else {
            self.download_status.push(DownloadStatusEntry {
                provider_id: provider_id.to_string(),
                status,
            });
        }
    }
}


/// Downloads a legend PNG from a URL and decodes it to an egui ColorImage.
fn download_legend_image(url: &str) -> Option<egui::ColorImage> {
    let response = ureq::get(url).call().ok()?;
    let bytes = response.into_body().read_to_vec().ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<u8> = rgba.into_raw();
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels))
}
