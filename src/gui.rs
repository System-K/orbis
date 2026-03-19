// =============================================================================
// Orbis — GUI (egui Overlay) (M6 + M9)
// =============================================================================
// Immediate-mode GUI rendered over the 3D globe. Uses egui + egui-wgpu.
//
// M9 changes:
// - Layer panel now shows active layers with remove buttons
// - "Add Layer" button opens a categorized catalog browser
// - Provider descriptions and attribution visible in catalog
// - Download status shown per layer
// =============================================================================

use std::collections::HashMap;
use std::sync::mpsc;

use chrono::{Datelike, NaiveDate, Timelike, Utc};

use crate::i18n;
use crate::provider::ProviderCatalog;
use crate::settings::Settings;

/// Encapsulates all egui resources.
pub struct Gui {
    pub ctx: egui::Context,
    pub state: egui_winit::State,
    pub renderer: egui_wgpu::Renderer,
}

impl Gui {
    /// Creates the GUI infrastructure.
    pub fn new(
        window: &winit::window::Window,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let ctx = egui::Context::default();

        // M15c: Load Noto Sans fonts for international script support
        load_fonts(&ctx);

        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            None,
            None,
            None,
        );

        let renderer = egui_wgpu::Renderer::new(
            device,
            surface_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: None,
                ..Default::default()
            },
        );

        Self {
            ctx,
            state,
            renderer,
        }
    }

    /// Forwards a winit event to egui.
    ///
    /// Returns `true` if egui consumed the event.
    pub fn handle_event(
        &mut self,
        window: &winit::window::Window,
        event: &winit::event::WindowEvent,
    ) -> bool {
        self.state.on_window_event(window, event).consumed
    }
}

/// Loads Noto Sans font family for international script support (M15c).
///
/// Adds fonts as fallbacks to egui's default proportional font family.
/// Order matters: first match wins for each glyph.
fn load_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let font_files: &[(&str, &str)] = &[
        ("noto-sans",       "assets/fonts/NotoSans-Regular.ttf"),
        ("noto-cjk",        "assets/fonts/NotoSansSC-Regular.otf"),
        ("noto-kr",         "assets/fonts/NotoSansKR-Regular.otf"),
        ("noto-arabic",     "assets/fonts/NotoSansArabic-Regular.ttf"),
        ("noto-devanagari", "assets/fonts/NotoSansDevanagari-Regular.ttf"),
    ];

    for (name, path) in font_files {
        let full_path = crate::app_path(path);
        match std::fs::read(&full_path) {
            Ok(data) => {
                fonts.font_data.insert(
                    name.to_string(),
                    egui::FontData::from_owned(data).into(),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push(name.to_string());
                log::info!("Font loaded: {} ({:.0} KB)", name,
                    full_path.metadata().map(|m| m.len() as f64 / 1024.0).unwrap_or(0.0));
            }
            Err(e) => {
                log::warn!("Font not found: {} ({}) — some scripts may not render", name, e);
            }
        }
    }

    ctx.set_fonts(fonts);
}

// =============================================================================
// GUI State
// =============================================================================

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
pub struct SatelliteTrack {
    /// Past orbit path screen points
    pub past: Vec<egui::Pos2>,
    /// Future orbit path screen points
    pub future: Vec<egui::Pos2>,
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
        }
    }

    /// Returns the currently selected date (for downloads).
    pub fn selected_date(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.selected_year, self.selected_month, self.selected_day)
    }

    /// Maximum days in the selected month (accounts for leap years).
    fn max_day(&self) -> u32 {
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

// =============================================================================
// Draw functions
// =============================================================================

/// Draws the GUI (called every frame).
pub fn draw_ui(ctx: &egui::Context, gui_state: &mut GuiState, catalog: &ProviderCatalog) {
    // ─── Layer panel (left side) ───────────────────────────────────────
    egui::SidePanel::left("layer_panel")
        .resizable(true)
        .default_width(300.0)
        .show_animated(ctx, gui_state.panel_open, |ui| {
            // Header
            ui.horizontal(|ui| {
                if ui.button("◀").clicked() {
                    gui_state.panel_open = false;
                }
                ui.heading(&i18n::t("app_title"));
            });
            ui.separator();

            // ─── View mode toggle ────────────────────
            ui.horizontal(|ui| {
                let globe_label = if gui_state.view_mode_map {
                    i18n::t("view_globe")
                } else {
                    i18n::t("view_globe_active")
                };
                let map_label = if gui_state.view_mode_map {
                    i18n::t("view_map_active")
                } else {
                    i18n::t("view_map")
                };
                if ui
                    .selectable_label(!gui_state.view_mode_map, globe_label)
                    .clicked()
                {
                    gui_state.view_mode_map = false;
                }
                if ui
                    .selectable_label(gui_state.view_mode_map, map_label)
                    .clicked()
                {
                    gui_state.view_mode_map = true;
                }
            });

            ui.separator();

            // ─── Scrollable content ────────────────────────────
            egui::ScrollArea::vertical().show(ui, |ui| {
                // ─── Active layer list ──────────────────────────
                ui.collapsing(i18n::t("layers_heading"), |ui| {
                    if gui_state.catalog_open {
                        // === Catalog browser mode ===
                        draw_catalog(ui, gui_state, catalog);
                    } else {
                        // === Active layers mode ===
                        draw_active_layers(ui, gui_state);

                        ui.add_space(6.0);

                        // "Add Layer" button
                        if ui
                            .button(format!("➕ {}", i18n::t("layer_add")))
                            .clicked()
                        {
                            gui_state.catalog_open = true;
                        }
                    }
                });

                ui.separator();

                // ─── Time control ─────────────────────────────
                draw_time_control(ui, gui_state);

                ui.separator();

                // ─── Display settings ────────────────────────
                draw_display_settings(ui, gui_state);

                ui.separator();

                // ─── GeoJSON layers (M11e) ─────────────────────
                draw_geojson_layers(ui, gui_state);

                ui.separator();

                // ─── Live data sources (M12) ───────────────────
                draw_live_sources(ui, gui_state);

                ui.separator();

                draw_satellite_panel(ui, gui_state);

                ui.separator();

                // ─── Info section ──────────────────────────────
                ui.label(format!("{:.0} FPS", gui_state.fps));

                // Download status summary
                for entry in &gui_state.download_status {
                    match &entry.status {
                        DownloadStatus::Downloading => {
                            ui.small(format!("⏳ {}...", entry.provider_id));
                        }
                        DownloadStatus::Error(msg) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 100, 100),
                                format!("❌ {}: {}", entry.provider_id, msg),
                            );
                        }
                        DownloadStatus::Ready => {} // Don't show ready status
                    }
                }

                ui.separator();

                // --- Data attributions (dynamic, based on active layers) ---
                let mut attributions: Vec<&str> = Vec::new();

                // Raster layer attributions (GIBS, WMS, etc.)
                for entry in &gui_state.layers {
                    if let Some(provider) = catalog.find(&entry.provider_id) {
                        let attr = &provider.info().attribution;
                        if !attr.is_empty() && !attributions.contains(&attr.as_str()) {
                            attributions.push(attr.as_str());
                        }
                    }
                }
                // Always show GIBS attribution if any GIBS layer is present
                if gui_state.layers.iter().any(|l| l.provider_id.starts_with("gibs_")) {
                    let gibs = "NASA GIBS / ESDIS";
                    if !attributions.contains(&gibs) {
                        attributions.push(gibs);
                    }
                }

                // Live source attributions (only for active sources)
                let sources = crate::live_source::all_sources();
                for active_id in &gui_state.active_live_sources {
                    if let Some(src) = sources.iter().find(|s| s.id == *active_id) {
                        if !attributions.contains(&src.attribution) {
                            attributions.push(src.attribution);
                        }
                    }
                }

                // GeoJSON layer attributions
                for attr in &gui_state.geo_attributions {
                    if !attributions.contains(&attr.as_str()) {
                        attributions.push(attr.as_str());
                    }
                }

                for attr in &attributions {
                    ui.small(*attr);
                }

                ui.separator();
                ui.small(&i18n::t("shortcuts"));

            });
        });

    // Track panel width for label occlusion.
    // available_rect().left() gives the x where the remaining area starts
    // (after all left panels have been drawn).
    gui_state.panel_width = ctx.available_rect().left();

    // ─── Toggle button (always visible when panel is closed) ──────────
    if !gui_state.panel_open {
        egui::Area::new(egui::Id::new("panel_toggle"))
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(4.0, 4.0))
            .show(ctx, |ui| {
                let button = egui::Button::new("▶").min_size(egui::vec2(28.0, 28.0));
                if ui
                    .add(button)
                    .on_hover_text(&i18n::t("panel_tooltip"))
                    .clicked()
                {
                    gui_state.panel_open = true;
                }
            });
    }

    // ─── Legend panel (bottom-right, floating) ────────────────────
    draw_legend(ctx, gui_state, catalog);
}

/// Draws the active layer list with controls.
fn draw_active_layers(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    if gui_state.layers.is_empty() {
        ui.weak(&i18n::t("layers_none"));
        return;
    }

    let mut remove_id: Option<String> = None;

    for entry in gui_state.layers.iter_mut() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut entry.enabled, "");
            ui.label(&entry.label);

            // Remove button (right-aligned)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("🗑")
                    .on_hover_text(&i18n::t("layer_remove"))
                    .clicked()
                {
                    remove_id = Some(entry.id.clone());
                }
            });
        });

        ui.add(
            egui::Slider::new(&mut entry.opacity, 0.0..=1.0)
                .text(&i18n::t("layers_opacity"))
                .fixed_decimals(2),
        );

        ui.add_space(4.0);
    }

    if let Some(id) = remove_id {
        gui_state.remove_layer_request = Some(id);
        gui_state.layers_changed = true;
    }
}

/// Draws the catalog browser for adding new layers.
fn draw_catalog(
    ui: &mut egui::Ui,
    gui_state: &mut GuiState,
    catalog: &ProviderCatalog,
) {
    // Back button
    ui.horizontal(|ui| {
        if ui.button(format!("◀ {}", i18n::t("catalog_back"))).clicked() {
            gui_state.catalog_open = false;
        }
        ui.strong(&i18n::t("catalog_title"));
    });

    ui.separator();
    ui.small(&i18n::t("catalog_description"));
    ui.add_space(4.0);

    // List providers grouped by category
    for category in catalog.active_categories() {
        let providers = catalog.by_category(category);
        if providers.is_empty() {
            continue;
        }

        ui.collapsing(category.label(), |ui| {
            for provider in &providers {
                let info = provider.info();

                // Check if already added
                let already_active = gui_state
                    .layers
                    .iter()
                    .any(|l| l.provider_id == info.id);

                // Check if currently downloading
                let is_downloading = matches!(
                    gui_state.get_download_status(&info.id),
                    Some(DownloadStatus::Downloading)
                );

                ui.horizontal(|ui| {
                    let enabled = !already_active && !is_downloading;
                    let button_label = if already_active {
                        format!("✅ {}", info.label)
                    } else if is_downloading {
                        format!("⏳ {}", info.label)
                    } else {
                        format!("➕ {}", info.label)
                    };

                    if ui
                        .add_enabled(enabled, egui::Button::new(&button_label))
                        .on_hover_text(&info.description)
                        .clicked()
                    {
                        gui_state.add_provider_request = Some(info.id.clone());
                    }
                });
            }
        });
    }
}

/// Draws the time control section.
fn draw_time_control(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("time_heading"), |ui| {
        // Live toggle
        let live_label = if gui_state.time_live {
            i18n::t("time_live")
        } else {
            i18n::t("time_manual")
        };
        if ui
            .selectable_label(gui_state.time_live, live_label)
            .clicked()
        {
            gui_state.time_live = !gui_state.time_live;
            if gui_state.time_live {
                let now = Utc::now();
                gui_state.selected_year = now.year();
                gui_state.selected_month = now.month();
                gui_state.selected_day = now.day();
                gui_state.selected_hour = now.hour();
                gui_state.selected_minute = now.minute();
                gui_state.date_changed = true;
            }
        }

        if !gui_state.time_live {
            ui.add_space(4.0);

            let old_date = (
                gui_state.selected_year,
                gui_state.selected_month,
                gui_state.selected_day,
            );

            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut gui_state.selected_year)
                        .range(2012..=2026)
                        .speed(0.1)
                        .prefix("Y: "),
                );
                ui.add(
                    egui::DragValue::new(&mut gui_state.selected_month)
                        .range(1..=12)
                        .speed(0.05)
                        .prefix("M: "),
                );
            });

            let max_day = gui_state.max_day();
            if gui_state.selected_day > max_day {
                gui_state.selected_day = max_day;
            }

            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut gui_state.selected_day)
                        .range(1..=max_day)
                        .speed(0.1)
                        .prefix("D: "),
                );
            });

            let new_date = (
                gui_state.selected_year,
                gui_state.selected_month,
                gui_state.selected_day,
            );
            if new_date != old_date {
                gui_state.date_changed = true;
            }

            ui.add_space(4.0);
            ui.label(&i18n::t("time_utc_label"));
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut gui_state.selected_hour)
                        .range(0..=23)
                        .speed(0.1)
                        .prefix("H: "),
                );
                ui.add(
                    egui::DragValue::new(&mut gui_state.selected_minute)
                        .range(0..=59)
                        .speed(0.1)
                        .prefix("M: "),
                );
            });
        }
    });
}

/// Draws the display/control settings section.
fn draw_display_settings(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("settings_heading"), |ui| {
        // --- Projection mode ---
        ui.horizontal(|ui| {
            ui.label(i18n::t("settings_projection"));
            let is_ortho = gui_state.settings.globe_projection
                == crate::camera::GlobeProjection::Orthographic;
            if ui
                .selectable_label(is_ortho, i18n::t("settings_proj_ortho"))
                .clicked()
            {
                gui_state.settings.globe_projection =
                    crate::camera::GlobeProjection::Orthographic;
                gui_state.settings_dirty = true;
            }
            if ui
                .selectable_label(!is_ortho, i18n::t("settings_proj_persp"))
                .clicked()
            {
                gui_state.settings.globe_projection =
                    crate::camera::GlobeProjection::Perspective;
                gui_state.settings_dirty = true;
            }
        });

        ui.add_space(4.0);

        // --- Mouse axis inversion ---
        if ui
            .checkbox(
                &mut gui_state.settings.invert_mouse_x,
                i18n::t("settings_invert_x"),
            )
            .changed()
        {
            gui_state.settings_dirty = true;
        }
        if ui
            .checkbox(
                &mut gui_state.settings.invert_mouse_y,
                i18n::t("settings_invert_y"),
            )
            .changed()
        {
            gui_state.settings_dirty = true;
        }

        ui.add_space(4.0);

        // --- Language selector (M15b) ---
        let current_code = i18n::current_language();
        let current_label = gui_state.available_languages.iter()
            .find(|(c, _)| *c == current_code)
            .map(|(_, name)| name.as_str())
            .unwrap_or("English");

        ui.horizontal(|ui| {
            ui.label("\u{1F310}"); // 🌐
            egui::ComboBox::from_id_salt("lang_selector")
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    for (code, name) in &gui_state.available_languages {
                        let is_selected = *code == current_code;
                        if ui.selectable_label(is_selected, name).clicked() && !is_selected {
                            i18n::set_language(code);
                            gui_state.settings.language = Some(code.clone());
                            gui_state.settings_dirty = true;
                        }
                    }
                });
        });

        ui.add_space(6.0);
        ui.separator();

        // --- Tile settings (M16) ---
        ui.label(i18n::t("cache_heading"));
        ui.add_space(2.0);

        // Tile source selector (M16f)
        let current_source = &gui_state.settings.tile_source;
        let current_source_label = gui_state.tile_sources.iter()
            .find(|(id, _)| id == current_source)
            .map(|(_, name)| name.as_str())
            .unwrap_or("Sentinel-2 Cloudless");

        ui.horizontal(|ui| {
            ui.label(i18n::t("tile_source_label"));
            egui::ComboBox::from_id_salt("tile_source_selector")
                .selected_text(current_source_label)
                .show_ui(ui, |ui| {
                    for (id, name) in &gui_state.tile_sources {
                        let selected = *id == gui_state.settings.tile_source;
                        if ui.selectable_label(selected, name).clicked() && !selected {
                            gui_state.settings.tile_source = id.clone();
                            gui_state.settings_dirty = true;
                        }
                    }
                });
        });

        ui.add_space(2.0);

        // Max cache size slider (100 MB – 5000 MB)
        ui.horizontal(|ui| {
            ui.label(i18n::t("cache_max_size"));
            let mut mb = gui_state.settings.tile_cache_max_mb as f32;
            if ui.add(egui::Slider::new(&mut mb, 100.0..=5000.0)
                .step_by(100.0)
                .suffix(" MB")
            ).changed() {
                gui_state.settings.tile_cache_max_mb = mb as u32;
                gui_state.settings_dirty = true;
            }
        });

        // Max tile age slider (0 = forever, 1–90 days)
        ui.horizontal(|ui| {
            ui.label(i18n::t("cache_max_age"));
            let mut days = gui_state.settings.tile_cache_max_days as f32;
            if ui.add(egui::Slider::new(&mut days, 0.0..=90.0)
                .step_by(1.0)
                .custom_formatter(|v, _| {
                    if v < 0.5 { "\u{221e}".to_string() } // ∞
                    else { format!("{:.0}", v) }
                })
            ).changed() {
                gui_state.settings.tile_cache_max_days = days as u32;
                gui_state.settings_dirty = true;
            }
        });

        // Current usage display + clear button
        ui.horizontal(|ui| {
            ui.label(format!("{} {:.1} / {} MB",
                i18n::t("cache_usage"),
                gui_state.cache_usage_mb,
                gui_state.settings.tile_cache_max_mb,
            ));
            if ui.small_button(i18n::t("cache_clear")).clicked() {
                gui_state.cache_clear_request = true;
            }
        });
    });
}

/// Draws the GeoJSON layer management section.
fn draw_geojson_layers(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("geojson_heading"), |ui| {
        // Labels toggle
        ui.checkbox(&mut gui_state.labels_visible, i18n::t("geojson_show_labels"));

        ui.add_space(4.0);

        // Loaded layers
        if gui_state.geo_layer_info.is_empty() {
            ui.weak(i18n::t("geojson_no_layers"));
        } else {
            let mut toggle_name = None;
            let mut remove_name = None;

            // Feature count summary
            let total_p: usize = gui_state.geo_layer_info.iter().map(|i| i.point_count).sum();
            let total_l: usize = gui_state.geo_layer_info.iter().map(|i| i.line_count).sum();
            let total_g: usize = gui_state.geo_layer_info.iter().map(|i| i.polygon_count).sum();
            let total = total_p + total_l + total_g;
            ui.weak(format!(
                "{} features (P:{} L:{} Poly:{})",
                total, total_p, total_l, total_g,
            ));

            ui.add_space(2.0);

            for info in &gui_state.geo_layer_info {
                ui.horizontal(|ui| {
                    let icon = if info.visible { "◉" } else { "○" };
                    if ui.button(icon)
                        .on_hover_text(i18n::t("geojson_toggle"))
                        .clicked()
                    {
                        toggle_name = Some(info.name.clone());
                    }

                    ui.label(&info.name);

                    let count = info.point_count + info.line_count + info.polygon_count;
                    ui.weak(format!("({})", count));

                    if ui.small_button("✖")
                        .on_hover_text(i18n::t("geojson_remove"))
                        .clicked()
                    {
                        remove_name = Some(info.name.clone());
                    }
                });
            }

            gui_state.toggle_geo_layer_request = toggle_name;
            gui_state.remove_geo_layer_request = remove_name;
        }

        ui.add_space(4.0);

        // Load button + drag & drop hint
        if ui.button(i18n::t("geojson_load")).clicked() {
            gui_state.load_geojson_request = true;
        }
        ui.weak(i18n::t("geojson_dragdrop"));

        ui.add_space(4.0);

        // URL loading
        ui.horizontal(|ui| {
            ui.label("URL:");
            let response = ui.add(
                egui::TextEdit::singleline(&mut gui_state.geojson_url_input)
                    .desired_width(160.0)
                    .hint_text("https://..."),
            );
            let enter_pressed = response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            if (ui.small_button("Load").clicked() || enter_pressed)
                && !gui_state.geojson_url_input.trim().is_empty()
            {
                gui_state.load_geojson_url_request =
                    Some(gui_state.geojson_url_input.trim().to_string());
                gui_state.geojson_url_input.clear();
            }
        });

        // Status message (auto-clears after 5 seconds)
        let status_expired = gui_state.geojson_status
            .as_ref()
            .map_or(false, |(_, when, _)| when.elapsed().as_secs() >= 5);
        if status_expired {
            gui_state.geojson_status = None;
        }
        if let Some((msg, _, is_error)) = &gui_state.geojson_status {
            let color = if *is_error {
                egui::Color32::from_rgb(255, 100, 100)
            } else {
                egui::Color32::from_rgb(100, 255, 100)
            };
            ui.colored_label(color, msg);
        }
    });
}

/// Draws the live data source management section.
fn draw_live_sources(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("live_heading"), |ui| {
        let sources = crate::live_source::all_sources();

        // Group by category
        for category in crate::live_source::LiveSourceCategory::all() {
            let cat_sources: Vec<_> = sources
                .iter()
                .filter(|s| s.category == *category)
                .collect();

            if cat_sources.is_empty() {
                continue;
            }

            ui.weak(category.label());

            for src in &cat_sources {
                let is_active = gui_state
                    .active_live_sources
                    .iter()
                    .any(|id| id == src.id);

                ui.horizontal(|ui| {
                    if is_active {
                        if ui.small_button("⏹")
                            .on_hover_text(i18n::t("live_stop"))
                            .clicked()
                        {
                            gui_state.deactivate_live_source =
                                Some(src.id.to_string());
                        }
                        ui.label(format!("\u{1f7e2} {}", src.label));
                    } else {
                        if ui.small_button("▶")
                            .on_hover_text(i18n::t("live_start"))
                            .clicked()
                        {
                            gui_state.activate_live_source =
                                Some(src.id.to_string());
                        }
                        ui.weak(src.label);
                    }
                });
            }

            ui.add_space(2.0);
        }

        if gui_state.active_live_sources.is_empty() {
            ui.weak(i18n::t("live_none_active"));
        }

        // Show metered warning for OpenSky feeds
        let has_opensky = gui_state
            .active_live_sources
            .iter()
            .any(|id| id.starts_with("opensky_"));
        if has_opensky {
            ui.add_space(2.0);
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 80),
                i18n::t("live_opensky_metered"),
            );
        }
    });
}

// =============================================================================
// Legend image download
// =============================================================================

/// Downloads a legend PNG from a URL and decodes it to an egui ColorImage.
///
/// Returns None on any error (network, decode). Called from background thread.
fn download_legend_image(url: &str) -> Option<egui::ColorImage> {
    let response = ureq::get(url).call().ok()?;
    let bytes = response.into_body().read_to_vec().ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<u8> = rgba.into_raw();
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels))
}

// =============================================================================
// Satellite tracking panel (M13)
// =============================================================================

/// Draws the satellite tracking section in the side panel.
fn draw_satellite_panel(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("sat_heading"), |ui| {
        ui.checkbox(&mut gui_state.satellites_visible, i18n::t("sat_show"));

        if gui_state.satellite_downloading {
            ui.spinner();
            ui.weak(i18n::t("sat_downloading"));
        } else if gui_state.satellite_count == 0 {
            ui.weak(i18n::t("sat_no_data"));
        } else {
            ui.weak(format!(
                "{} {}",
                gui_state.satellite_count,
                i18n::t("sat_tracked"),
            ));
        }

        if !gui_state.satellite_markers.is_empty() && gui_state.satellites_visible {
            ui.add_space(2.0);
            ui.weak(i18n::t("sat_click_hint"));
            ui.add_space(2.0);
            for sat in &gui_state.satellite_markers {
                let is_followed = gui_state.follow_satellite == Some(sat.norad_id);
                let icon = if is_followed { "◉" } else { "○" };
                let color = if is_followed {
                    egui::Color32::from_rgb(255, 255, 120)
                } else {
                    egui::Color32::from_rgb(255, 220, 50)
                };
                let text = format!(
                    "{} {} — {:.0} km, {:.1} km/s",
                    icon, sat.name, sat.altitude_km, sat.velocity_km_s,
                );
                let label = egui::Label::new(
                    egui::RichText::new(&text).color(color),
                ).sense(egui::Sense::click());
                let response = ui.add(label);
                if response.clicked() {
                    if is_followed {
                        gui_state.follow_satellite = None;
                    } else {
                        gui_state.follow_satellite = Some(sat.norad_id);
                    }
                }
                response.on_hover_text(if is_followed {
                    i18n::t("sat_unfollow_tooltip")
                } else {
                    i18n::t("sat_follow_tooltip")
                });
            }
            if gui_state.follow_satellite.is_some() {
                ui.add_space(2.0);
                ui.weak(i18n::t("sat_follow_hint"));
            }
        }
    });
}

/// Draws the legend panel (M12d).
///
/// Floating egui window at the bottom-right showing color scales
/// for active live data sources and GIBS raster layers.
/// Only visible when toggled (K shortcut).
fn draw_legend(
    ctx: &egui::Context,
    gui_state: &mut GuiState,
    catalog: &ProviderCatalog,
) {
    if !gui_state.legend_open {
        return;
    }

    // Poll pending legend image downloads
    gui_state.poll_legend_downloads(ctx);

    // --- Live source legends ---
    let has_quakes = gui_state
        .active_live_sources
        .iter()
        .any(|id| id.starts_with("usgs_"));
    let has_aircraft = gui_state
        .active_live_sources
        .iter()
        .any(|id| id.starts_with("opensky_"));
    let has_volcanoes = gui_state
        .active_live_sources
        .iter()
        .any(|id| id.starts_with("gvp_"));

    // --- GIBS raster legends: collect (provider_id, label) for layers with legend_url ---
    let mut raster_legends: Vec<(String, String, Option<String>)> = Vec::new();
    for entry in &gui_state.layers {
        if !entry.enabled {
            continue;
        }
        if let Some(provider) = catalog.find(&entry.provider_id) {
            let info = provider.info();
            if let Some(ref url) = info.legend_url {
                raster_legends.push((
                    entry.provider_id.clone(),
                    info.label.clone(),
                    Some(url.clone()),
                ));
            }
        }
    }

    // --- GeoJSON layer legends ---
    let has_nuclear = gui_state.geo_layer_info.iter().any(|l| l.visible && l.name == "Nuclear Power Plants");
    let has_plates = gui_state.geo_layer_info.iter().any(|l| l.visible && l.name == "Tectonic Plates");
    let has_satellites = gui_state.satellites_visible && !gui_state.satellite_markers.is_empty();

    let has_any = has_quakes || has_aircraft || has_volcanoes
        || !raster_legends.is_empty() || has_nuclear || has_plates || has_satellites;
    if !has_any {
        return;
    }

    // Trigger downloads for raster legends that haven't been requested yet
    for (pid, _label, url) in &raster_legends {
        if let Some(url) = url {
            gui_state.request_legend_download(pid, url);
        }
    }

    egui::Window::new(i18n::t("legend_title"))
        .id(egui::Id::new("legend_panel"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
        .collapsible(true)
        .resizable(false)
        .default_width(200.0)
        .max_height(500.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut need_sep = false;

                // Live source legends (hand-drawn swatches)
                if has_quakes {
                    draw_legend_earthquakes(ui);
                    need_sep = true;
                }
                if has_aircraft {
                    if need_sep { ui.separator(); }
                    draw_legend_aircraft(ui);
                    need_sep = true;
                }
                if has_volcanoes {
                    if need_sep { ui.separator(); }
                    draw_legend_volcanoes(ui);
                    need_sep = true;
                }

                // GeoJSON layer legends (hand-drawn)
                if has_nuclear {
                    if need_sep { ui.separator(); }
                    draw_legend_nuclear(ui);
                    need_sep = true;
                }
                if has_plates {
                    if need_sep { ui.separator(); }
                    draw_legend_tectonic(ui);
                    need_sep = true;
                }
                if has_satellites {
                    if need_sep { ui.separator(); }
                    draw_legend_satellites(ui);
                    need_sep = true;
                }

                // GIBS raster legends (downloaded PNG images)
                for (pid, label, _url) in &raster_legends {
                    if let Some(tex) = gui_state.legend_textures.get(pid) {
                        if need_sep { ui.separator(); }
                        ui.strong(label);
                        let size = tex.size_vec2();
                        // Scale to fit panel width (~190px), maintain aspect ratio
                        let max_w = 190.0;
                        let scale = (max_w / size.x).min(1.0);
                        ui.image(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(size.x * scale, size.y * scale),
                        ));
                        need_sep = true;
                    }
                }
            });
        });
}

/// Helper: draws a colored rectangle swatch.
fn color_swatch(ui: &mut egui::Ui, rgba: [f32; 4], size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let c = egui::Color32::from_rgba_unmultiplied(
        (rgba[0] * 255.0) as u8,
        (rgba[1] * 255.0) as u8,
        (rgba[2] * 255.0) as u8,
        (rgba[3] * 255.0) as u8,
    );
    ui.painter().rect_filled(rect, 2.0, c);
}

/// Earthquake legend: magnitude → color.
fn draw_legend_earthquakes(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_earthquakes"));
    let steps: &[(f64, &str)] = &[
        (1.0, "< M2"),
        (3.0, "M2–M4"),
        (4.5, "M4–M5.5"),
        (6.0, "M5.5–M7"),
        (7.5, "M7+"),
    ];
    for (mag, label) in steps {
        ui.horizontal(|ui| {
            color_swatch(ui, crate::live_source::magnitude_color(*mag), egui::vec2(14.0, 14.0));
            ui.label(*label);
        });
    }
}

/// Aircraft legend: altitude → color.
fn draw_legend_aircraft(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_aircraft"));
    let steps: &[(f64, &str)] = &[
        (-1.0, "Ground"),   // Will show gray via special case below
        (1500.0, "0–3 km"),
        (5500.0, "3–8 km"),
        (10000.0, "8–12 km"),
        (13000.0, "12+ km"),
    ];
    for (alt, label) in steps {
        let color = if *alt < 0.0 {
            [0.5, 0.5, 0.5, 0.7] // Ground: gray
        } else {
            crate::live_source::altitude_color(*alt)
        };
        ui.horizontal(|ui| {
            color_swatch(ui, color, egui::vec2(14.0, 14.0));
            ui.label(*label);
        });
    }
}

/// Volcano legend: last eruption year → color.
fn draw_legend_volcanoes(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_volcanoes"));
    let steps: &[(Option<i64>, &str)] = &[
        (Some(2000), "≥ 1900"),
        (Some(1600), "1500–1900"),
        (Some(500),  "0–1500 CE"),
        (Some(-2000), "Mid-Holocene"),
        (Some(-8000), "Early Holocene"),
        (None,        "Unknown"),
    ];
    for (year, label) in steps {
        ui.horizontal(|ui| {
            color_swatch(ui, crate::live_source::eruption_year_color(*year), egui::vec2(14.0, 14.0));
            ui.label(*label);
        });
    }
}

/// Legend for nuclear power plants: color by commissioning age.
fn draw_legend_nuclear(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_nuclear"));
    let steps: &[([f32; 4], &str)] = &[
        ([0.133, 0.8, 0.267, 1.0],   "≤ 10 y"),    // #22cc44
        ([0.533, 0.8, 0.133, 1.0],   "11–25 y"),   // #88cc22
        ([0.867, 0.667, 0.0, 1.0],   "26–40 y"),   // #ddaa00
        ([0.933, 0.4, 0.0, 1.0],     "41–50 y"),   // #ee6600
        ([0.867, 0.133, 0.0, 1.0],   "50+ y"),     // #dd2200
        ([0.8, 0.4, 0.0, 1.0],       "Unknown"),   // #cc6600
        ([0.533, 0.533, 0.533, 1.0], "Shutdown"),   // #888888
    ];
    for (color, label) in steps {
        ui.horizontal(|ui| {
            color_swatch(ui, *color, egui::vec2(14.0, 14.0));
            ui.label(*label);
        });
    }
}

/// Legend for tectonic plates: fill + stroke preview.
fn draw_legend_tectonic(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_tectonic"));
    ui.horizontal(|ui| {
        color_swatch(ui, [0.0, 0.667, 0.8, 0.15], egui::vec2(14.0, 14.0));
        ui.label(i18n::t("legend_plate_fill"));
    });
    ui.horizontal(|ui| {
        color_swatch(ui, [1.0, 0.267, 0.267, 1.0], egui::vec2(14.0, 14.0));
        ui.label(i18n::t("legend_plate_boundary"));
    });
}

/// Legend for satellite tracking: marker + track colors.
fn draw_legend_satellites(ui: &mut egui::Ui) {
    ui.strong(i18n::t("legend_satellites"));
    ui.horizontal(|ui| {
        color_swatch(ui, [1.0, 0.863, 0.196, 1.0], egui::vec2(14.0, 14.0));
        ui.label(i18n::t("legend_sat_position"));
    });
    ui.horizontal(|ui| {
        color_swatch(ui, [1.0, 0.941, 0.549, 0.63], egui::vec2(14.0, 14.0));
        ui.label(i18n::t("legend_sat_past"));
    });
    ui.horizontal(|ui| {
        color_swatch(ui, [0.863, 0.471, 1.0, 0.51], egui::vec2(14.0, 14.0));
        ui.label(i18n::t("legend_sat_future"));
    });
}

/// Renders planet markers on the sky sphere (M14b).
pub fn draw_planets(ctx: &egui::Context, gui_state: &GuiState) {
    if gui_state.planet_markers.is_empty() {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("planet_markers"),
    ));
    let panel_w = gui_state.panel_width;
    for p in &gui_state.planet_markers {
        if p.x < panel_w || !p.visible { continue; }
        let center = egui::pos2(p.x, p.y);
        let c = egui::Color32::from_rgb(
            (p.color[0] * 255.0) as u8,
            (p.color[1] * 255.0) as u8,
            (p.color[2] * 255.0) as u8,
        );
        // Outer glow
        painter.circle_filled(center, p.radius + 2.0,
            egui::Color32::from_rgba_unmultiplied(
                (p.color[0] * 200.0) as u8,
                (p.color[1] * 200.0) as u8,
                (p.color[2] * 200.0) as u8, 40));
        // Main disc
        painter.circle_filled(center, p.radius, c);
        // Name label
        painter.text(
            egui::pos2(p.x + p.radius + 4.0, p.y - 5.0),
            egui::Align2::LEFT_CENTER,
            p.name,
            egui::FontId::proportional(11.0),
            c,
        );
    }
}

/// Renders satellite markers as painted circles on the globe (M13).
pub fn draw_satellites(ctx: &egui::Context, gui_state: &GuiState) {
    if !gui_state.satellites_visible || gui_state.satellite_markers.is_empty() {
        return;
    }

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("satellite_markers"),
    ));

    let panel_w = gui_state.panel_width;

    // Ground tracks (behind satellite dots)
    // Colors chosen for red-green colorblind accessibility:
    // Past = warm white/yellow (high contrast on dark Earth)
    // Future = bright magenta/pink (distinct from yellow for deuteranopia)
    for track in &gui_state.satellite_tracks {
        if track.past.len() >= 2 {
            let clipped: Vec<egui::Pos2> = track.past.iter()
                .filter(|p| p.x >= panel_w)
                .copied()
                .collect();
            if clipped.len() >= 2 {
                let stroke = egui::Stroke::new(
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(255, 240, 140, 160),
                );
                painter.add(egui::Shape::line(clipped, stroke));
            }
        }
        if track.future.len() >= 2 {
            let clipped: Vec<egui::Pos2> = track.future.iter()
                .filter(|p| p.x >= panel_w)
                .copied()
                .collect();
            if clipped.len() >= 2 {
                let stroke = egui::Stroke::new(
                    1.8,
                    egui::Color32::from_rgba_unmultiplied(220, 120, 255, 130),
                );
                painter.add(egui::Shape::line(clipped, stroke));
            }
        }
    }

    // Satellite dots + labels
    for sat in &gui_state.satellite_markers {
        if sat.x < panel_w { continue; }
        let alpha = if sat.visible { 255 } else { 60 };
        let dot_color = egui::Color32::from_rgba_unmultiplied(255, 220, 50, alpha);
        let outline_color = egui::Color32::from_rgba_unmultiplied(200, 100, 0, alpha);
        let text_color = egui::Color32::from_rgba_unmultiplied(255, 255, 200, alpha);

        let center = egui::pos2(sat.x, sat.y);

        // Outer glow
        painter.circle_filled(center, 7.0, egui::Color32::from_rgba_unmultiplied(255, 180, 0, alpha / 4));
        // Main dot
        painter.circle_filled(center, 4.0, dot_color);
        painter.circle_stroke(center, 4.0, egui::Stroke::new(1.0, outline_color));

        // Name label (offset right)
        let text_pos = egui::pos2(sat.x + 8.0, sat.y - 6.0);
        painter.text(
            text_pos,
            egui::Align2::LEFT_CENTER,
            &sat.name,
            egui::FontId::proportional(11.0),
            text_color,
        );
    }
}

/// Renders floating labels at screen positions.
///
/// Features:
/// - Leader lines for displaced labels
/// - Left-click on cluster: expand/collapse grouped items
/// - Middle-click on any label: copy text to clipboard
/// - Labels rendered at default order; those overlapping the panel are hidden
pub fn draw_labels(ctx: &egui::Context, gui_state: &mut GuiState) {
    if !gui_state.labels_visible {
        return;
    }

    // Leader lines at Middle order (same as labels) so they stay together
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("label_leaders"),
    ));

    let panel_w = gui_state.panel_width;

    // Collect deferred state changes
    let mut toggle_expand: Option<String> = None;
    let mut copy_text: Option<String> = None;

    for (i, label) in gui_state.geo_labels.iter().enumerate() {
        // Skip labels that start inside the side panel area.
        // This prevents labels from rendering on top of the panel.
        if label.x < panel_w {
            continue;
        }

        let r = (label.color[0] * 255.0) as u8;
        let g = (label.color[1] * 255.0) as u8;
        let b = (label.color[2] * 255.0) as u8;
        let label_color = egui::Color32::from_rgb(r, g, b);

        // Leader line from anchor to displaced label
        if label.is_displaced() {
            let anchor = egui::pos2(label.anchor_x, label.anchor_y);
            let label_pos = egui::pos2(label.x, label.y + label.height * 0.5);
            let line_color = egui::Color32::from_rgba_unmultiplied(r, g, b, 120);
            painter.line_segment([anchor, label_pos], egui::Stroke::new(1.0, line_color));
            painter.circle_filled(anchor, 2.5, line_color);
        }

        let is_cluster = !label.clustered_texts.is_empty();
        let is_expanded = gui_state.expanded_labels.contains(&label.text);

        // Label rendering — Order::Middle so egui’s hit-test blocks globe drag.
        // We capture click events from the inner interactive rect.
        let mut left_clicked = false;
        let mut middle_clicked = false;

        egui::Area::new(egui::Id::new("geo_label").with(i))
            .fixed_pos(egui::pos2(label.x, label.y))
            .interactable(true)
            .show(ctx, |ui| {
                let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
                let frame_resp = egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .corner_radius(3.0)
                    .fill(bg)
                    .show(ui, |ui| {
                        ui.set_min_width(60.0);

                        // Main label — sense(click) so it responds to
                        // both left and middle mouse buttons.
                        // Show "+N" suffix only when collapsed.
                        let display_text = if is_cluster && !is_expanded {
                            format!("{} +{}", label.text, label.clustered_texts.len())
                        } else {
                            label.text.clone()
                        };
                        let label_widget = egui::Label::new(
                            egui::RichText::new(display_text).color(label_color),
                        )
                        .sense(egui::Sense::click());
                        let resp = ui.add(label_widget);

                        // Tooltip: hint about interactions
                        if is_cluster {
                            resp.on_hover_text(
                                i18n::t("label_click_expand"),
                            );
                        }

                        // Expanded cluster: show all merged texts in scrollable area
                        if is_expanded && is_cluster {
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .max_height(200.0)
                                .show(ui, |ui| {
                                    for text in &label.clustered_texts {
                                        ui.colored_label(label_color, text);
                                    }
                                });
                        }
                    });

                // Interact with the full frame rect so the entire box
                // is clickable, not just the text.
                let full_resp = ui.interact(
                    frame_resp.response.rect,
                    egui::Id::new("geo_label_click").with(i),
                    egui::Sense::click(),
                );
                if full_resp.clicked() {
                    left_clicked = true;
                }
                if full_resp.clicked_by(egui::PointerButton::Middle) {
                    middle_clicked = true;
                }
            });

        if left_clicked && is_cluster {
            toggle_expand = Some(label.text.clone());
        }
        if middle_clicked {
            let mut full = label.text.clone();
            for text in &label.clustered_texts {
                full.push('\n');
                full.push_str(text);
            }
            copy_text = Some(full);
        }
    }

    // Apply deferred state changes
    if let Some(key) = toggle_expand {
        if !gui_state.expanded_labels.remove(&key) {
            gui_state.expanded_labels.insert(key);
        }
        ctx.request_repaint();
    }
    if let Some(text) = copy_text {
        ctx.copy_text(text);
    }
}
