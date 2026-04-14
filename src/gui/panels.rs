// =============================================================================
// Orbis — GUI Layer Panels (Active Layers + Catalog Browser)
// =============================================================================

use crate::i18n;
use crate::provider::ProviderCatalog;
use super::state::{GuiState, DownloadStatus};

pub(super) fn draw_active_layers(ui: &mut egui::Ui, gui_state: &mut GuiState) {
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
pub(super) fn draw_catalog(
    ui: &mut egui::Ui,
    gui_state: &mut GuiState,
    catalog: &ProviderCatalog,
) {
    ui.horizontal(|ui| {
        if ui.button(format!("◀ {}", i18n::t("catalog_back"))).clicked() {
            gui_state.catalog_open = false;
        }
        ui.strong(&i18n::t("catalog_title"));
    });

    ui.separator();
    ui.small(&i18n::t("catalog_description"));
    ui.add_space(4.0);

    for category in catalog.active_categories() {
        let providers = catalog.by_category(category);
        if providers.is_empty() {
            continue;
        }

        ui.collapsing(category.label(), |ui| {
            for provider in &providers {
                let info = provider.info();

                let already_active = gui_state
                    .layers
                    .iter()
                    .any(|l| l.provider_id == info.id);

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
