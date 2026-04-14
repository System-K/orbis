// =============================================================================
// Orbis — GUI Time Control
// =============================================================================

use chrono::{Datelike, Timelike, Utc};
use crate::i18n;
use super::state::GuiState;

pub(super) fn draw_time_control(ui: &mut egui::Ui, gui_state: &mut GuiState) {
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
