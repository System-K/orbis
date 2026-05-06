// =============================================================================
// Orbis — GUI Request Handlers
// =============================================================================
// Consumes GUI-initiated request queues (layer add/remove, date changes,
// settings sync, catalog reload, GeoJSON load, live-source toggles) and
// mutates GpuState accordingly. Extracted from main.rs to keep main.rs under
// the 45 KB refactor budget (< 50 KB hard limit).
// =============================================================================

use crate::{GpuState, gui, i18n, layer, live_source, geojson, provider, tile};

impl GpuState {
    /// Handles all GUI-initiated requests: add/remove layers, date changes,
    /// settings sync, and catalog reload.
    pub(crate) fn handle_gui_requests(&mut self) {
        // Add layer
        if let Some(provider_id) = self.gui_state.add_provider_request.take() {
            if !self.layer_stack.has_provider(&provider_id) {
                if provider_id == "builtin:grid" {
                    // Grid is procedurally generated, not downloaded
                    let grid_texture = layer::generate_grid_texture(
                        &self.device, &self.queue, 2048, 1024,
                    );
                    let grid_layer = layer::Layer::new(
                        "grid",
                        &i18n::t("layer_grid"),
                        "builtin:grid",
                        grid_texture,
                        0.3,
                        &self.overlay_layer_bind_group_layout,
                        &self.overlay_settings_bind_group_layout,
                        &self.device,
                    );
                    self.layer_stack.add(grid_layer);
                    self.gui_state.layers_changed = true;
                } else {
                    let opacity = self.catalog.find(&provider_id)
                        .map(|p| p.info().default_opacity).unwrap_or(0.5);
                    self.gui_state.set_download_status(
                        &provider_id, gui::DownloadStatus::Downloading,
                    );
                    self.download_manager.start_download(
                        &self.catalog, &provider_id, None, opacity, true,
                    );
                }
            }
        }

        // Remove layer
        if let Some(layer_id) = self.gui_state.remove_layer_request.take() {
            self.layer_stack.remove(&layer_id);
            self.gui_state.download_status.retain(|e| e.provider_id != layer_id);
            self.gui_state.layers_changed = true;
        }

        // Save layer config
        if self.gui_state.layers_changed {
            self.gui_state.layers_changed = false;
            let layer_configs: Vec<(String, f32, bool)> = self.layer_stack.layers.iter()
                .map(|l| (l.provider_id.clone(), l.opacity, l.enabled))
                .collect();
            self.gui_state.settings.sync_layers(&layer_configs);
            self.gui_state.settings.save();
        }

        // Date change → re-download layers
        if self.gui_state.date_changed {
            self.gui_state.date_changed = false;
            if let Some(date) = self.gui_state.selected_date() {
                let download_date = if self.gui_state.time_live {
                    date - chrono::Days::new(1)
                } else {
                    date
                };
                let providers_to_reload: Vec<(String, f32, bool)> = self.layer_stack.layers.iter()
                    .filter(|l| l.provider_id != "builtin:grid")
                    .map(|l| (l.provider_id.clone(), l.opacity, l.enabled))
                    .collect();
                for (pid, opacity, enabled) in providers_to_reload {
                    self.gui_state.set_download_status(&pid, gui::DownloadStatus::Downloading);
                    self.download_manager.start_download(
                        &self.catalog, &pid, Some(download_date), opacity, enabled,
                    );
                    self.layer_stack.layers.retain(|l| l.provider_id != pid);
                }
            }
        }

        // Settings sync (FOV, projection, tile cache)
        if self.gui_state.settings_dirty {
            let new_fov = self.gui_state.settings.compute_fov(
                self.camera.distance, self.camera.distance_min, self.camera.distance_max,
            );
            // Only adjust distance if FOV actually changed (prevents drift on
            // unrelated settings changes like projection toggle or mouse invert)
            if (new_fov - self.camera.fov_y).abs() > 0.001 {
                let ratio = (self.camera.fov_y / 2.0).tan() / (new_fov / 2.0).tan();
                self.camera.distance = (self.camera.distance * ratio)
                    .clamp(self.camera.distance_min, self.camera.distance_max);
            }
            self.camera.fov_y = new_fov;
            let tile_settings = tile::TileSettings {
                source_id: self.gui_state.settings.tile_source.clone(),
                cache_max_mb: self.gui_state.settings.tile_cache_max_mb,
                cache_max_age: if self.gui_state.settings.tile_cache_max_days == 0 {
                    None // 0 = no age limit
                } else {
                    Some(std::time::Duration::from_secs(
                        self.gui_state.settings.tile_cache_max_days as u64 * 24 * 3600,
                    ))
                },
                zoom_bias: self.gui_state.settings.tile_zoom_bias,
            };
            self.tile_manager.apply_settings(&tile_settings);
            self.gui_state.settings_dirty = false;
            self.gui_state.settings.save();
        }

        // Reload provider catalog
        if self.gui_state.reload_catalog_request {
            self.gui_state.reload_catalog_request = false;
            self.catalog = provider::build_default_catalog();
            log::info!("Provider catalog reloaded ({} providers)", self.catalog.count());

            // Remove overlay layers whose provider no longer exists in catalog
            let removed_layers: Vec<String> = self.layer_stack.layers.iter()
                .filter(|l| l.provider_id.starts_with("user_"))
                .filter(|l| self.catalog.find(&l.provider_id).is_none())
                .map(|l| l.provider_id.clone())
                .collect();
            for pid in &removed_layers {
                self.layer_stack.remove(pid);
                self.gui_state.download_status.retain(|e| e.provider_id != *pid);
                log::info!("Removed overlay layer for deleted custom source '{}'", pid);
            }
            if !removed_layers.is_empty() {
                self.gui_state.layers_changed = true;
            }

            // Sync REST feed manager — remove deactivated feeds + their GeoLayers
            let removed_feeds = self.rest_feed_manager.sync_config(&self.gui_state.custom_sources_config);
            for name in &removed_feeds {
                if self.marker_system.remove_layer(name) {
                    log::info!("Removed GeoLayer for deactivated REST feed '{}'", name);
                }
            }

            // M17h: Sync Shapefile source manager — load new sources, drop
            // deactivated ones, reload sources whose path changed.
            let shp_sync = self.shapefile_source_manager.sync_config(&self.gui_state.custom_sources_config);
            let shp_changed = !shp_sync.is_noop();
            for name in &shp_sync.removed {
                if self.marker_system.remove_layer(name) {
                    log::info!("Removed GeoLayer for deactivated Shapefile '{}'", name);
                }
            }
            for layer in shp_sync.added {
                log::info!("Added GeoLayer from Shapefile source '{}'", layer.name);
                self.marker_system.add_layer(layer);
            }

            if !removed_feeds.is_empty() || shp_changed {
                self.polygon_system.rebuild_from_layers(
                    self.marker_system.geo_layers(), &self.device,
                );
                self.line_system.rebuild_from_layers(
                    self.marker_system.geo_layers(),
                    self.polygon_system.outline_segments(),
                    &self.device,
                );
            }
        }
    }

    pub(crate) fn handle_geojson_requests(&mut self) {
        let mut geo_changed = false;

        // File dialog request
        if self.gui_state.load_geojson_request {
            self.gui_state.load_geojson_request = false;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Vector data", &["geojson", "json", "shp"])
                .add_filter("GeoJSON", &["geojson", "json"])
                .add_filter("Shapefile", &["shp"])
                .pick_file()
            {
                self.load_vector_file(&path);
                geo_changed = true;
            }
        }

        // Drag & drop
        let dropped: Vec<_> = self.gui_state.dropped_files.drain(..).collect();
        for path in dropped {
            self.load_vector_file(&path);
            geo_changed = true;
        }

        // Toggle layer visibility
        if let Some(name) = self.gui_state.toggle_geo_layer_request.take() {
            self.marker_system.toggle_layer(&name);
            geo_changed = true;
        }

        // Remove layer
        if let Some(name) = self.gui_state.remove_geo_layer_request.take() {
            self.marker_system.remove_layer(&name);
            geo_changed = true;
        }

        // Rebuild line + polygon buffers if anything changed
        if geo_changed {
            self.polygon_system.rebuild_from_layers(
                self.marker_system.geo_layers(),
                &self.device,
            );
            self.line_system.rebuild_from_layers(
                self.marker_system.geo_layers(),
                self.polygon_system.outline_segments(),
                &self.device,
            );
        }
    }

    /// Loads a vector file (GeoJSON or Shapefile) and adds it as a layer.
    /// Dispatch is by file extension — every supported format produces a
    /// `GeoLayer`, so the rest of the pipeline doesn't care which one it
    /// came from.
    fn load_vector_file(&mut self, path: &std::path::Path) {
        let name = path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());

        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let result = match ext.as_str() {
            "shp" => crate::shp::load_shapefile(path),
            // Default to GeoJSON for .geojson, .json, and anything unknown
            // (the legacy load_geojson_file emitted its own error message
            // for files it couldn't parse, so behaviour is preserved).
            _ => geojson::load_geojson_file(path),
        };

        match result {
            Ok(layer) => {
                let count = layer.len();
                log::info!("Loaded '{}' ({}): {} features", layer.name, ext, count);
                self.marker_system.add_layer(layer);
            }
            Err(e) => {
                log::error!("Failed to load '{}' ({}): {}", name, ext, e);
            }
        }
    }

    /// Handles activate/deactivate requests for live data sources.
    pub(crate) fn handle_live_source_requests(&mut self) {
        // Activate
        if let Some(id) = self.gui_state.activate_live_source.take() {
            if let Some(def) = live_source::all_sources().into_iter().find(|s| s.id == id) {
                self.live_source_manager.activate(def);
            }
        }

        // Deactivate
        if let Some(id) = self.gui_state.deactivate_live_source.take() {
            // Find label before deactivating (used as layer name)
            let label = live_source::all_sources()
                .into_iter()
                .find(|s| s.id == id)
                .map(|s| s.label);
            self.live_source_manager.deactivate(&id);
            // Remove the layer from marker system (keyed by label)
            if let Some(label) = label {
                if self.marker_system.remove_layer(label) {
                    self.polygon_system.rebuild_from_layers(
                        self.marker_system.geo_layers(),
                        &self.device,
                    );
                    self.line_system.rebuild_from_layers(
                        self.marker_system.geo_layers(),
                        self.polygon_system.outline_segments(),
                        &self.device,
                    );
                }
            }
        }

        // Sync active source IDs to GUI
        self.gui_state.active_live_sources = self
            .live_source_manager
            .active_ids()
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
}
