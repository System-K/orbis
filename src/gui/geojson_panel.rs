// =============================================================================
// Orbis — GUI GeoJSON Layer Panel
// =============================================================================

use crate::i18n;
use super::state::GuiState;

pub(super) fn draw_geojson_layers(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("geojson_heading"), |ui| {
        ui.checkbox(&mut gui_state.labels_visible, i18n::t("geojson_show_labels"));
        ui.add_space(4.0);

        if gui_state.geo_layer_info.is_empty() {
            ui.weak(i18n::t("geojson_no_layers"));
        } else {
            let mut toggle_name = None;
            let mut remove_name = None;

            let total_p: usize = gui_state.geo_layer_info.iter().map(|i| i.point_count).sum();
            let total_l: usize = gui_state.geo_layer_info.iter().map(|i| i.line_count).sum();
            let total_g: usize = gui_state.geo_layer_info.iter().map(|i| i.polygon_count).sum();
            let total = total_p + total_l + total_g;
            ui.weak(format!("{} features (P:{} L:{} Poly:{})", total, total_p, total_l, total_g));
            ui.add_space(2.0);

            for info in &gui_state.geo_layer_info {
                ui.horizontal(|ui| {
                    let icon = if info.visible { "◉" } else { "○" };
                    if ui.button(icon).on_hover_text(i18n::t("geojson_toggle")).clicked() {
                        toggle_name = Some(info.name.clone());
                    }
                    ui.label(&info.name);
                    let count = info.point_count + info.line_count + info.polygon_count;
                    ui.weak(format!("({})", count));
                    if ui.small_button("✖").on_hover_text(i18n::t("geojson_remove")).clicked() {
                        remove_name = Some(info.name.clone());
                    }
                });
            }

            gui_state.toggle_geo_layer_request = toggle_name;
            gui_state.remove_geo_layer_request = remove_name;
        }

        ui.add_space(4.0);
        if ui.button(i18n::t("geojson_load")).clicked() {
            gui_state.load_geojson_request = true;
        }
        ui.weak(i18n::t("geojson_dragdrop"));
        ui.add_space(4.0);

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
