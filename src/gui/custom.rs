// =============================================================================
// Orbis — GUI Custom Sources Panel + Dialog (M17e)
// =============================================================================

use super::state::{GuiState, CustomSourceForm, SOURCE_CATEGORIES, SOURCE_TYPES};

pub(super) fn draw_custom_sources_panel(ui: &mut egui::Ui, gui_state: &mut GuiState) {
    ui.collapsing("🔌 Custom Sources", |ui| {
        // List existing custom sources
        if gui_state.custom_sources_config.sources.is_empty() {
            ui.weak("No custom sources defined");
        } else {
            let mut toggle_idx: Option<usize> = None;
            let mut remove_idx: Option<usize> = None;

            for (i, src) in gui_state.custom_sources_config.sources.iter().enumerate() {
                ui.horizontal(|ui| {
                    let icon = if src.enabled { "◉" } else { "○" };
                    if ui.button(icon).on_hover_text("Toggle").clicked() {
                        toggle_idx = Some(i);
                    }

                    let type_tag = match src.source_type {
                        crate::custom_source::SourceType::Wms => "WMS",
                        crate::custom_source::SourceType::Xyz => "XYZ",
                        crate::custom_source::SourceType::Rest => "REST",
                        crate::custom_source::SourceType::Shapefile => "SHP",
                        crate::custom_source::SourceType::Csv => "CSV",
                    };
                    ui.label(format!("[{}] {}", type_tag, src.name));

                    if ui.small_button("✖").on_hover_text("Remove").clicked() {
                        remove_idx = Some(i);
                    }
                });
            }

            // Handle toggle
            if let Some(i) = toggle_idx {
                gui_state.custom_sources_config.sources[i].enabled =
                    !gui_state.custom_sources_config.sources[i].enabled;
                crate::custom_source::save_config(&gui_state.custom_sources_config);
                gui_state.reload_catalog_request = true;
            }

            // Handle remove
            if let Some(i) = remove_idx {
                let name = gui_state.custom_sources_config.sources[i].name.clone();
                gui_state.custom_sources_config.sources.remove(i);
                crate::custom_source::save_config(&gui_state.custom_sources_config);
                gui_state.reload_catalog_request = true;
                gui_state.custom_source_status = Some((
                    format!("Removed: {}", name),
                    std::time::Instant::now(),
                    false,
                ));
            }
        }

        ui.add_space(4.0);

        // "Add Custom Source" button
        if ui.button("➕ Add Custom Source").clicked() {
            gui_state.custom_source_form = CustomSourceForm::default();
            gui_state.custom_source_dialog_open = true;
        }

        // Status message (auto-clears after 5s)
        let expired = gui_state.custom_source_status
            .as_ref()
            .map_or(false, |(_, when, _)| when.elapsed().as_secs() >= 5);
        if expired {
            gui_state.custom_source_status = None;
        }
        if let Some((msg, _, is_error)) = &gui_state.custom_source_status {
            let color = if *is_error {
                egui::Color32::from_rgb(255, 100, 100)
            } else {
                egui::Color32::from_rgb(100, 255, 100)
            };
            ui.colored_label(color, msg);
        }

        ui.add_space(2.0);
        ui.weak("Edit config/custom_sources.json for advanced options");
    });
}

/// Draws the floating "Add Custom Source" dialog window (M17e).
pub(super) fn draw_custom_source_dialog(ctx: &egui::Context, gui_state: &mut GuiState) {
    if !gui_state.custom_source_dialog_open {
        return;
    }

    let mut open = gui_state.custom_source_dialog_open;

    egui::Window::new("Add Custom Source")
        .id(egui::Id::new("custom_source_dialog"))
        .open(&mut open)
        .resizable(false)
        .default_width(400.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let form = &mut gui_state.custom_source_form;

            // --- Common fields ---
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut form.name);
            });

            ui.horizontal(|ui| {
                ui.label("Type:");
                egui::ComboBox::from_id_salt("src_type")
                    .selected_text(SOURCE_TYPES[form.source_type_idx])
                    .show_ui(ui, |ui| {
                        for (i, label) in SOURCE_TYPES.iter().enumerate() {
                            ui.selectable_value(&mut form.source_type_idx, i, *label);
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Category:");
                egui::ComboBox::from_id_salt("src_cat")
                    .selected_text(SOURCE_CATEGORIES[form.category_idx].1)
                    .show_ui(ui, |ui| {
                        for (i, (_, label)) in SOURCE_CATEGORIES.iter().enumerate() {
                            ui.selectable_value(&mut form.category_idx, i, *label);
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Attribution:");
                ui.text_edit_singleline(&mut form.attribution);
            });

            ui.add(
                egui::Slider::new(&mut form.opacity, 0.0..=1.0)
                    .text("Opacity")
                    .fixed_decimals(2),
            );

            ui.separator();

            // --- Type-specific fields ---
            match form.source_type_idx {
                0 => {
                    // WMS
                    ui.strong("WMS Configuration");
                    ui.add_space(2.0);

                    // --- Base URL + Detect button ---
                    ui.horizontal(|ui| {
                        ui.label("Base URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.wms_base_url)
                                .desired_width(220.0)
                                .hint_text("https://example.com/wms"),
                        );
                        let can_detect = !form.wms_base_url.is_empty()
                            && !matches!(form.wms_caps_status, crate::wms_caps::CapsStatus::Loading);
                        if ui.add_enabled(can_detect, egui::Button::new("\u{1f50d} Detect")).clicked() {
                            form.wms_caps_status = crate::wms_caps::CapsStatus::Loading;
                            form.wms_caps_rx = Some(crate::wms_caps::fetch_capabilities_async(
                                form.wms_base_url.clone(),
                                std::collections::HashMap::new(),
                            ));
                        }
                    });

                    // --- Poll background GetCapabilities result ---
                    {
                        let mut caps_result: Option<Result<crate::wms_caps::WmsCapabilities, String>> = None;
                        if let Some(rx) = &form.wms_caps_rx {
                            match rx.try_recv() {
                                Ok(result) => { caps_result = Some(result); }
                                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                                Err(_) => {
                                    caps_result = Some(Err("Connection lost".to_string()));
                                }
                            }
                        }
                        if let Some(result) = caps_result {
                            form.wms_caps_rx = None;
                            match result {
                                Ok(caps) => {
                                    log::info!("GetCapabilities: {} layers, version {}",
                                        caps.layers.len(), caps.version);
                                    form.wms_version_130 = caps.version.starts_with("1.3");
                                    if caps.formats.iter().any(|f| f == "image/png") {
                                        form.wms_format_png = true;
                                    } else if caps.formats.iter().any(|f| f.contains("jpeg")) {
                                        form.wms_format_png = false;
                                    }
                                    form.wms_caps_layer_idx = 0;
                                    // Auto-select first layer
                                    if let Some(first) = caps.layers.first() {
                                        form.wms_layer_name = first.name.clone();
                                        if form.name.is_empty() {
                                            form.name = first.title.clone();
                                        }
                                        form.wms_uses_time = first.has_time;
                                        // CRS is auto-discovered at fetch time via
                                        // GetCapabilities — no longer set per-source.
                                    }
                                    form.wms_caps_status = crate::wms_caps::CapsStatus::Ready(caps);
                                }
                                Err(e) => {
                                    form.wms_caps_status = crate::wms_caps::CapsStatus::Error(e);
                                }
                            }
                        }
                    }

                    // --- Status display ---
                    match &form.wms_caps_status {
                        crate::wms_caps::CapsStatus::Idle => {}
                        crate::wms_caps::CapsStatus::Loading => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak("Detecting WMS capabilities...");
                            });
                        }
                        crate::wms_caps::CapsStatus::Ready(caps) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(100, 220, 100),
                                format!("\u{2705} {} layers found (WMS {})",
                                    caps.layers.len(), caps.version),
                            );
                        }
                        crate::wms_caps::CapsStatus::Error(e) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 180, 80),
                                format!("\u{26a0} {}", e),
                            );
                            ui.weak("You can still configure manually below.");
                        }
                    }

                    // --- Layer selection: dropdown if caps available, text field as fallback ---
                    if let crate::wms_caps::CapsStatus::Ready(caps) = &form.wms_caps_status {
                        if !caps.layers.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label("Layer:");
                                let selected_title = caps.layers
                                    .get(form.wms_caps_layer_idx)
                                    .map(|l| l.title.as_str())
                                    .unwrap_or("(select)");
                                egui::ComboBox::from_id_salt("wms_layer_select")
                                    .selected_text(selected_title)
                                    .width(260.0)
                                    .show_ui(ui, |ui| {
                                        for (i, layer) in caps.layers.iter().enumerate() {
                                            let label = if layer.name == layer.title {
                                                layer.name.clone()
                                            } else {
                                                format!("{} ({})", layer.title, layer.name)
                                            };
                                            if ui.selectable_value(
                                                &mut form.wms_caps_layer_idx, i, &label
                                            ).changed() {
                                                // Auto-fill from selected layer
                                                form.wms_layer_name = layer.name.clone();
                                                if form.name.is_empty() {
                                                    form.name = layer.title.clone();
                                                }
                                                form.wms_uses_time = layer.has_time;
                                                // CRS picked at fetch time, not here.
                                            }
                                        }
                                    });
                            });

                            // Show CRS info for selected layer
                            if let Some(layer) = caps.layers.get(form.wms_caps_layer_idx) {
                                let strategy = crate::wms_caps::select_best_crs(&layer.supported_crs);
                                let crs_text = match &strategy {
                                    crate::wms_caps::CrsStrategy::WebMercator =>
                                        "\u{2705} EPSG:3857 (Web Mercator, auto-reprojection)".to_string(),
                                    crate::wms_caps::CrsStrategy::Equirectangular =>
                                        "\u{2705} EPSG:4326 (native equirectangular)".to_string(),
                                    crate::wms_caps::CrsStrategy::Crs84 =>
                                        "\u{2705} CRS:84 (native equirectangular)".to_string(),
                                    crate::wms_caps::CrsStrategy::Unsupported(code) =>
                                        format!("\u{26a0} Only {} available (may not display correctly)", code),
                                };
                                ui.weak(&crs_text);
                            }
                        }
                    } else {
                        // Fallback: manual layer name input
                        ui.horizontal(|ui| {
                            ui.label("Layer name:");
                            ui.add(
                                egui::TextEdit::singleline(&mut form.wms_layer_name)
                                    .desired_width(280.0)
                                    .hint_text("layer_name"),
                            );
                        });
                    }

                    // --- Remaining WMS settings (always visible, auto-filled) ---
                    ui.add_space(4.0);
                    ui.collapsing("Advanced settings", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Format:");
                            ui.selectable_value(&mut form.wms_format_png, true, "PNG");
                            ui.selectable_value(&mut form.wms_format_png, false, "JPEG");
                        });
                        ui.checkbox(&mut form.wms_transparent, "Transparent background");
                        ui.checkbox(&mut form.wms_uses_time, "Supports TIME parameter");
                        ui.horizontal(|ui| {
                            ui.label("WMS Version:");
                            ui.selectable_value(&mut form.wms_version_130, true, "1.3.0");
                            ui.selectable_value(&mut form.wms_version_130, false, "1.1.1");
                        });
                    });
                }
                1 => {
                    // XYZ Tiles
                    ui.strong("XYZ Tile Configuration");
                    ui.add_space(2.0);

                    ui.horizontal(|ui| {
                        ui.label("URL template:");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.xyz_url_template)
                                .desired_width(280.0)
                                .hint_text("https://tile.example.com/{z}/{x}/{y}.png"),
                        );
                    });

                    ui.add(
                        egui::Slider::new(&mut form.xyz_max_zoom, 1..=22)
                            .text("Max zoom"),
                    );

                    ui.weak("Not yet implemented (M17c)");
                }
                2 => {
                    // REST/GeoJSON
                    ui.strong("REST/GeoJSON Configuration");
                    ui.add_space(2.0);

                    ui.horizontal(|ui| {
                        ui.label("API URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.rest_url)
                                .desired_width(280.0)
                                .hint_text("https://api.example.com/data.geojson"),
                        );
                    });

                    ui.add(
                        egui::Slider::new(&mut form.rest_refresh_secs, 0..=3600)
                            .text("Refresh (seconds)")
                            .custom_formatter(|v, _| {
                                if v < 0.5 { "off".to_string() }
                                else { format!("{:.0}s", v) }
                            }),
                    );

                    ui.weak("Not yet implemented (M17d)");
                }
                3 => {
                    // Shapefile
                    ui.strong("Shapefile Configuration");
                    ui.add_space(2.0);

                    ui.horizontal(|ui| {
                        ui.label("File:");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.shp_path)
                                .desired_width(220.0)
                                .hint_text("/path/to/file.shp"),
                        );
                        if ui.small_button("📂 Browse...").clicked() {
                            if let Some(picked) = rfd::FileDialog::new()
                                .add_filter("Shapefile", &["shp"])
                                .pick_file()
                            {
                                form.shp_path = picked.to_string_lossy().to_string();
                            }
                        }
                    });

                    ui.weak(
                        "CRS is auto-detected from the .prj sidecar and the data \
                         bbox. WGS84 and Web Mercator are reprojected to the globe; \
                         other CRSes render at-coordinate with a warning.",
                    );
                }
                4 => {
                    // CSV
                    ui.strong("CSV Configuration");
                    ui.add_space(2.0);

                    ui.horizontal(|ui| {
                        ui.label("File:");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.csv_path)
                                .desired_width(220.0)
                                .hint_text("/path/to/points.csv"),
                        );
                        if ui.small_button("📂 Browse...").clicked() {
                            if let Some(picked) = rfd::FileDialog::new()
                                .add_filter("CSV / TSV", &["csv", "tsv"])
                                .pick_file()
                            {
                                form.csv_path = picked.to_string_lossy().to_string();
                            }
                        }
                    });

                    ui.weak(
                        "Required columns: a latitude column (lat / latitude / y / \
                         decimal_latitude) and a longitude column (lon / longitude / \
                         x / decimal_longitude). Optional name column becomes a \
                         visible label; optional description column lands in feature \
                         properties.",
                    );
                }
                _ => {}
            }

            ui.separator();

            // --- Buttons ---
            ui.horizontal(|ui| {
                let can_save = !form.name.trim().is_empty()
                    && match form.source_type_idx {
                        0 => !form.wms_base_url.trim().is_empty()
                            && !form.wms_layer_name.trim().is_empty(),
                        1 => !form.xyz_url_template.trim().is_empty(),
                        2 => !form.rest_url.trim().is_empty(),
                        3 => !form.shp_path.trim().is_empty(),
                        4 => !form.csv_path.trim().is_empty(),
                        _ => false,
                    };

                if ui
                    .add_enabled(can_save, egui::Button::new("💾 Save"))
                    .clicked()
                {
                    // Build config from form
                    let id = format!(
                        "user_{}",
                        form.name
                            .trim()
                            .to_lowercase()
                            .replace(|c: char| !c.is_alphanumeric(), "_")
                    );

                    let source_type = match form.source_type_idx {
                        0 => crate::custom_source::SourceType::Wms,
                        1 => crate::custom_source::SourceType::Xyz,
                        2 => crate::custom_source::SourceType::Rest,
                        3 => crate::custom_source::SourceType::Shapefile,
                        _ => crate::custom_source::SourceType::Csv,
                    };

                    let wms = if form.source_type_idx == 0 {
                        Some(crate::custom_source::WmsConfig {
                            base_url: form.wms_base_url.trim().to_string(),
                            layer_name: form.wms_layer_name.trim().to_string(),
                            format: if form.wms_format_png {
                                "image/png".to_string()
                            } else {
                                "image/jpeg".to_string()
                            },
                            transparent: form.wms_transparent,
                            uses_time: form.wms_uses_time,
                            // Auto-discovered at fetch time. Hand-edited JSON can
                            // still set this to `true` as a manual legacy override.
                            reproject_mercator: false,
                            wms_version: if form.wms_version_130 {
                                "1.3.0".to_string()
                            } else {
                                "1.1.1".to_string()
                            },
                        })
                    } else {
                        None
                    };

                    let xyz = if form.source_type_idx == 1 {
                        Some(crate::custom_source::XyzConfig {
                            url_template: form.xyz_url_template.trim().to_string(),
                            max_zoom: form.xyz_max_zoom,
                            subdomains: Vec::new(),
                            format: "png".to_string(),
                        })
                    } else {
                        None
                    };

                    let rest = if form.source_type_idx == 2 {
                        Some(crate::custom_source::RestConfig {
                            url: form.rest_url.trim().to_string(),
                            refresh_secs: form.rest_refresh_secs,
                            response_format: "geojson".to_string(),
                        })
                    } else {
                        None
                    };

                    let shp_cfg = if form.source_type_idx == 3 {
                        Some(crate::custom_source::ShapefileConfig {
                            path: form.shp_path.trim().to_string(),
                        })
                    } else {
                        None
                    };

                    let csv_cfg = if form.source_type_idx == 4 {
                        Some(crate::custom_source::CsvConfig {
                            path: form.csv_path.trim().to_string(),
                        })
                    } else {
                        None
                    };

                    let new_source = crate::custom_source::CustomSourceConfig {
                        id,
                        name: form.name.trim().to_string(),
                        source_type,
                        category: SOURCE_CATEGORIES[form.category_idx].0.to_string(),
                        attribution: form.attribution.trim().to_string(),
                        default_opacity: form.opacity,
                        enabled: true,
                        headers: std::collections::HashMap::new(),
                        wms,
                        xyz,
                        rest,
                        shapefile: shp_cfg,
                        csv: csv_cfg,
                    };

                    gui_state.custom_sources_config.sources.push(new_source);
                    crate::custom_source::save_config(&gui_state.custom_sources_config);
                    gui_state.reload_catalog_request = true;
                    gui_state.custom_source_dialog_open = false;
                    gui_state.custom_source_status = Some((
                        format!("Added: {}", form.name.trim()),
                        std::time::Instant::now(),
                        false,
                    ));
                }

                if ui.button("Cancel").clicked() {
                    gui_state.custom_source_dialog_open = false;
                    return;
                }
            });
        });

    // Sync X-button close (egui sets open=false when X is clicked)
    if !open {
        gui_state.custom_source_dialog_open = false;
    }
}
