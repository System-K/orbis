// =============================================================================
// Orbis — GUI Satellite Panel + Sky Overlays (Planets, Satellites)
// =============================================================================

use crate::i18n;
use super::state::GuiState;

pub(super) fn draw_satellite_panel(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing(i18n::t("sat_heading"), |ui| {
        ui.checkbox(&mut gui_state.satellites_visible, i18n::t("sat_show"));

        if gui_state.satellite_downloading {
            ui.spinner();
            ui.weak(i18n::t("sat_downloading"));
        } else if gui_state.satellite_count == 0 {
            ui.weak(i18n::t("sat_no_data"));
        } else {
            let enabled = gui_state.enabled_satellites.len();
            let total = gui_state.all_satellites.len();
            ui.weak(format!("{}/{} {}", enabled, total, i18n::t("sat_tracked")));
        }

        if gui_state.satellites_visible && !gui_state.all_satellites.is_empty() {
            ui.add_space(2.0);

            // Toggle all / none
            ui.horizontal(|ui| {
                if ui.small_button(i18n::t("sat_all")).clicked() {
                    for (id, _) in &gui_state.all_satellites {
                        gui_state.enabled_satellites.insert(*id);
                    }
                }
                if ui.small_button(i18n::t("sat_none")).clicked() {
                    gui_state.enabled_satellites.clear();
                    gui_state.follow_satellite = None;
                }
            });

            ui.add_space(2.0);

            // Per-satellite toggles
            let mut toggle_id: Option<u32> = None;
            let mut follow_click: Option<u32> = None;

            for (norad_id, name) in &gui_state.all_satellites {
                let is_enabled = gui_state.enabled_satellites.contains(norad_id);
                let is_followed = gui_state.follow_satellite == Some(*norad_id);

                ui.horizontal(|ui| {
                    // Checkbox to toggle visibility
                    let mut checked = is_enabled;
                    if ui.checkbox(&mut checked, "").changed() {
                        toggle_id = Some(*norad_id);
                    }

                    // Satellite name + telemetry (clickable for follow)
                    let color = if is_followed {
                        egui::Color32::from_rgb(255, 255, 120)
                    } else if is_enabled {
                        egui::Color32::from_rgb(255, 220, 50)
                    } else {
                        egui::Color32::from_rgb(140, 140, 140)
                    };

                    // Find telemetry from markers (if satellite is projected)
                    let telemetry = gui_state.satellite_markers.iter()
                        .find(|m| m.norad_id == *norad_id);

                    let text = if let Some(sat) = telemetry {
                        format!("{} — {:.0} km, {:.1} km/s",
                            name, sat.altitude_km, sat.velocity_km_s)
                    } else {
                        name.clone()
                    };

                    let follow_icon = if is_followed { "◉ " } else { "" };
                    let label = egui::Label::new(
                        egui::RichText::new(format!("{}{}", follow_icon, text)).color(color),
                    ).sense(egui::Sense::click());

                    let resp = ui.add(label);
                    if resp.clicked() && is_enabled {
                        follow_click = Some(*norad_id);
                    }
                    resp.on_hover_text(if is_followed {
                        i18n::t("sat_unfollow_tooltip")
                    } else if is_enabled {
                        i18n::t("sat_follow_tooltip")
                    } else {
                        i18n::t("sat_enable_first")
                    });
                });
            }

            // Apply toggle
            if let Some(id) = toggle_id {
                if gui_state.enabled_satellites.contains(&id) {
                    gui_state.enabled_satellites.remove(&id);
                    if gui_state.follow_satellite == Some(id) {
                        gui_state.follow_satellite = None;
                    }
                } else {
                    gui_state.enabled_satellites.insert(id);
                }
            }

            // Apply follow
            if let Some(id) = follow_click {
                if gui_state.follow_satellite == Some(id) {
                    gui_state.follow_satellite = None;
                } else {
                    gui_state.follow_satellite = Some(id);
                }
            }

            if gui_state.follow_satellite.is_some() {
                ui.add_space(2.0);
                ui.weak(i18n::t("sat_follow_hint"));
            }
        }
    });
}

/// Renders planet markers on the sky sphere (M14b).
pub fn draw_planets(ctx: &egui::Context, gui_state: &GuiState) {
    if gui_state.planet_markers.is_empty() || gui_state.custom_source_dialog_open {
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
        painter.circle_filled(center, p.radius + 2.0,
            egui::Color32::from_rgba_unmultiplied(
                (p.color[0] * 200.0) as u8,
                (p.color[1] * 200.0) as u8,
                (p.color[2] * 200.0) as u8, 40));
        painter.circle_filled(center, p.radius, c);
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
    if !gui_state.satellites_visible || gui_state.satellite_markers.is_empty()
        || gui_state.custom_source_dialog_open
    {
        return;
    }

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("satellite_markers"),
    ));

    let panel_w = gui_state.panel_width;

    // Ground tracks
    for track in &gui_state.satellite_tracks {
        let past_stroke = egui::Stroke::new(
            2.0,
            egui::Color32::from_rgba_unmultiplied(255, 240, 140, 160),
        );
        for seg in &track.past_segments {
            let clipped: Vec<egui::Pos2> = seg.iter()
                .filter(|p| p.x >= panel_w)
                .copied()
                .collect();
            if clipped.len() >= 2 {
                painter.add(egui::Shape::line(clipped, past_stroke));
            }
        }
        let future_stroke = egui::Stroke::new(
            1.8,
            egui::Color32::from_rgba_unmultiplied(220, 120, 255, 130),
        );
        for seg in &track.future_segments {
            let clipped: Vec<egui::Pos2> = seg.iter()
                .filter(|p| p.x >= panel_w)
                .copied()
                .collect();
            if clipped.len() >= 2 {
                painter.add(egui::Shape::line(clipped, future_stroke));
            }
        }
    }

    // Satellite dots + labels
    for sat in &gui_state.satellite_markers {
        if sat.x < panel_w || !sat.visible { continue; }
        let alpha: u8 = 255;
        let dot_color = egui::Color32::from_rgba_unmultiplied(255, 220, 50, alpha);
        let outline_color = egui::Color32::from_rgba_unmultiplied(200, 100, 0, alpha);
        let text_color = egui::Color32::from_rgba_unmultiplied(255, 255, 200, alpha);

        let center = egui::pos2(sat.x, sat.y);

        painter.circle_filled(center, 7.0, egui::Color32::from_rgba_unmultiplied(255, 180, 0, alpha / 4));
        painter.circle_filled(center, 4.0, dot_color);
        painter.circle_stroke(center, 4.0, egui::Stroke::new(1.0, outline_color));

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
