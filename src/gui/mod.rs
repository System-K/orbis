// =============================================================================
// Orbis — GUI Module (egui Overlay)
// =============================================================================
// Split into sub-modules for maintainability:
//   state.rs       — GuiState, all marker/track/info structs
//   panels.rs      — Active layer list + catalog browser
//   time.rs        — Time control section
//   settings.rs    — Display settings + tile cache
//   custom.rs      — Custom sources panel + dialog (M17)
//   geojson_panel.rs — GeoJSON layer management
//   live.rs        — Live data source management
//   satellites.rs  — Satellite panel + sky overlays
//   legend.rs      — Legend panel + all legend helpers
//   labels.rs      — Floating label overlay rendering
// =============================================================================

pub mod state;
mod panels;
mod time;
mod settings;
mod custom;
mod geojson_panel;
mod live;
pub mod satellites;
pub mod legend;
pub mod labels;
mod scale;

// Re-export all public types used by main.rs and other modules
pub use state::{
    GuiState, SatelliteMarker, PlanetMarker, SatelliteTrack,
    GeoLayerInfo, DownloadStatus, LayerGuiEntry,
};

// Re-export overlay draw functions called directly from main.rs
pub use satellites::{draw_satellites, draw_planets};
pub use labels::draw_labels;

use crate::i18n;
use crate::provider::ProviderCatalog;

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
// Main draw entry point
// =============================================================================

/// Draws the complete GUI (called every frame).
pub fn draw_ui(ctx: &egui::Context, gui_state: &mut GuiState, catalog: &ProviderCatalog) {
    // Left side panel
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

            // View mode toggle
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

            // Scrollable content
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.collapsing(i18n::t("layers_heading"), |ui| {
                    if gui_state.catalog_open {
                        panels::draw_catalog(ui, gui_state, catalog);
                    } else {
                        panels::draw_active_layers(ui, gui_state);
                        ui.add_space(6.0);
                        if ui
                            .button(format!("➕ {}", i18n::t("layer_add")))
                            .clicked()
                        {
                            gui_state.catalog_open = true;
                        }
                    }
                });

                ui.separator();
                time::draw_time_control(ui, gui_state);

                ui.separator();
                settings::draw_display_settings(ui, gui_state);

                ui.separator();
                custom::draw_custom_sources_panel(ui, gui_state);

                ui.separator();
                geojson_panel::draw_geojson_layers(ui, gui_state);

                ui.separator();
                live::draw_live_sources(ui, gui_state);

                ui.separator();
                satellites::draw_satellite_panel(ui, gui_state);

                ui.separator();

                // About section
                ui.collapsing("ℹ About", |ui| {
                    ui.label("Orbis — Real-Time Earth Viewer");
                    ui.add_space(2.0);
                    ui.hyperlink_to("🌐 GitHub", "https://github.com/System-K/orbis");
                    ui.hyperlink_to("☕ Support on Ko-Fi", "https://ko-fi.com/yveskuehn");
                    ui.add_space(2.0);
                    ui.small("by Yves Kühn");
                    ui.small("Licensed under GPL-3.0-or-later");
                });

                ui.separator();

                // FPS + download status
                ui.label(format!("{:.0} FPS", gui_state.fps));
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
                        DownloadStatus::Ready => {}
                    }
                }

                ui.separator();

                // Data attributions
                let mut attributions: Vec<&str> = Vec::new();
                for entry in &gui_state.layers {
                    if let Some(provider) = catalog.find(&entry.provider_id) {
                        let attr = &provider.info().attribution;
                        if !attr.is_empty() && !attributions.contains(&attr.as_str()) {
                            attributions.push(attr.as_str());
                        }
                    }
                }
                if gui_state.layers.iter().any(|l| l.provider_id.starts_with("gibs_")) {
                    let gibs = "NASA GIBS / ESDIS";
                    if !attributions.contains(&gibs) {
                        attributions.push(gibs);
                    }
                }
                let sources = crate::live_source::all_sources();
                for active_id in &gui_state.active_live_sources {
                    if let Some(src) = sources.iter().find(|s| s.id == *active_id) {
                        if !attributions.contains(&src.attribution) {
                            attributions.push(src.attribution);
                        }
                    }
                }
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

    gui_state.panel_width = ctx.available_rect().left();

    // Toggle button when panel is closed
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

    // Floating panels
    legend::draw_legend(ctx, gui_state, catalog);
    custom::draw_custom_source_dialog(ctx, gui_state);

    // HUD: tile zoom + scale bar (lower-left of the viewport).
    // Drawn last so it sits on top and uses `available_rect()` from
    // after the panels above have claimed their space.
    scale::draw_scale_hud(ctx, gui_state);
}
