// =============================================================================
// Orbis — GUI Display Settings + Tile Cache
// =============================================================================

use crate::i18n;
use super::state::GuiState;

pub(super) fn draw_display_settings(ui: &mut egui::Ui, gui_state: &mut GuiState) {
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

        // Tile-pipeline status (universal notation, no i18n keys).
        let m = &gui_state.tile_metrics;
        ui.label(format!(
            "z={}  tiles={}/{}  loading={}",
            m.current_zoom, m.composited_tiles, m.visible_tiles, m.in_flight,
        ));

        #[cfg(debug_assertions)]
        ui.label(format!(
            "hits={}  miss={}  gen={}",
            m.cache_hits, m.cache_misses, m.gen,
        ));
    });
}
