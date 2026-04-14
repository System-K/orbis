// =============================================================================
// Orbis — GUI Legend Panel
// =============================================================================

use crate::i18n;
use crate::provider::ProviderCatalog;
use super::state::GuiState;

pub(super) fn draw_legend(
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
pub(super) fn color_swatch(ui: &mut egui::Ui, rgba: [f32; 4], size: egui::Vec2) {
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
pub(super) fn draw_legend_earthquakes(ui: &mut egui::Ui) {
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
pub(super) fn draw_legend_aircraft(ui: &mut egui::Ui) {
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
pub(super) fn draw_legend_volcanoes(ui: &mut egui::Ui) {
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
pub(super) fn draw_legend_nuclear(ui: &mut egui::Ui) {
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
pub(super) fn draw_legend_tectonic(ui: &mut egui::Ui) {
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
pub(super) fn draw_legend_satellites(ui: &mut egui::Ui) {
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
