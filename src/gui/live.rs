// =============================================================================
// Orbis — GUI Live Data Sources Panel
// =============================================================================

use crate::i18n;
use super::state::GuiState;

pub(super) fn draw_live_sources(ui: &mut egui::Ui, gui_state: &mut GuiState) {
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
