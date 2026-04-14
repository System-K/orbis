// =============================================================================
// Orbis — GUI Label Overlay Rendering
// =============================================================================

use crate::i18n;
use super::state::GuiState;

pub fn draw_labels(ctx: &egui::Context, gui_state: &mut GuiState) {
    if !gui_state.labels_visible || gui_state.custom_source_dialog_open {
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
