// =============================================================================
// Orbis — Real-time Earth Visualization
// =============================================================================
// M3: Day/night cycle with real sun position + soft terminator
// M4: Generic overlay layer system (multi-pass rendering)
// M5: NASA GIBS — real-time satellite imagery as overlay
// M6: egui GUI + time control + shortcuts + dynamic FOV
// M7: 2D map projection (equirectangular) + GUI toggle
// M8a: Procedural starfield (3000 billboard quads, instanced rendering)
// M8b: Atmospheric glow (Fresnel effect in globe.wgsl)
// M8c: Error handling (texture fallback, GIBS status in GUI)
// M8d: Cross-compile + packaging (app_path, scripts, GitHub Actions)
// M8e: README + GPL-3.0 license
// M8f: i18n system (8 languages, auto-detect system locale)
// M9:  Provider catalog + scalable layer infrastructure
// M10: External WMS sources (DWD, OpenStreetMap, OpenTopoMap)
// =============================================================================

use std::sync::Arc;
use std::path::PathBuf;

/// Resolve a path relative to the binary location.
///
/// Makes Orbis work regardless of where it's launched from.
/// Priority: (1) next to the executable, (2) current working directory.
pub fn app_path(relative: &str) -> PathBuf {
    // Attempt 1: Next to the binary (release package)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(relative);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    // Attempt 2: Working directory (development with cargo run)
    PathBuf::from(relative)
}

use winit::window::Window;

// Module declarations
mod camera;
mod crs;
mod csv_import;
mod gibs;
mod gpx_import;
mod shp;
mod gui;
mod i18n;
mod layer;
mod mesh;
mod provider;
mod settings;
mod sun;
mod texture;
mod wms;
#[allow(dead_code)] // M11: items used progressively across M11b-M11e
mod geojson;
#[allow(dead_code)]
mod marker;
#[allow(dead_code)]
mod line;
#[allow(dead_code)]
mod polygon;
#[allow(dead_code)]
mod label;

mod live_source;

mod satellite;

mod planets;

mod tile;

mod custom_source;
mod wms_caps;
mod download;
mod gpu_init;
mod app;
mod overlay_project;
mod gui_requests;

use camera::Camera;
use layer::LayerStack;
use provider::ProviderCatalog;
use texture::GpuTexture;

/// Display mode: 3D globe or 2D map
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Globe3D,
    Map2D,
}

/// Half-width of the map quad (height = QUAD_HALF_WIDTH / 2 due to 2:1 ratio)
pub(crate) const QUAD_HALF_WIDTH: f32 = 4.0;

/// Overlay settings passed per-layer to the shader.
///
/// Written into each layer's own GPU buffer at creation time.
/// Padded to 16 bytes (GPU alignment for uniform buffers).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OverlaySettings {
    pub opacity: f32,
    pub _pad: [f32; 3],
}

// --- GPU State ---
pub(crate) struct GpuState {
    pub(crate) window: Arc<Window>,
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) config: wgpu::SurfaceConfiguration,

    pub(crate) render_pipeline: wgpu::RenderPipeline,
    pub(crate) vertex_buffer: wgpu::Buffer,
    pub(crate) index_buffer: wgpu::Buffer,
    pub(crate) num_indices: u32,

    pub(crate) camera: Camera,
    pub(crate) camera_uniform_buffer: wgpu::Buffer,
    pub(crate) camera_bind_group: wgpu::BindGroup,

    // Texture resources (day + night)
    #[allow(dead_code)]
    pub(crate) earth_day_texture: GpuTexture,
    #[allow(dead_code)]
    pub(crate) earth_night_texture: GpuTexture,
    pub(crate) texture_bind_group: wgpu::BindGroup,

    // M4: Overlay layer system
    pub(crate) overlay_pipeline: wgpu::RenderPipeline,
    pub(crate) overlay_layer_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) overlay_settings_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) layer_stack: LayerStack,

    // M9: Provider catalog + download manager
    pub(crate) catalog: ProviderCatalog,
    pub(crate) download_manager: download::DownloadManager,

    // M6: GUI
    pub(crate) gui: gui::Gui,
    pub(crate) gui_state: gui::GuiState,
    pub(crate) last_frame_time: std::time::Instant,

    // M8a: Starfield
    pub(crate) star_pipeline: wgpu::RenderPipeline,
    pub(crate) star_vertex_buffer: wgpu::Buffer,
    pub(crate) star_count: u32,

    // M7: 2D map projection
    pub(crate) view_mode: ViewMode,
    pub(crate) map_pipeline: wgpu::RenderPipeline,
    pub(crate) map_vertex_buffer: wgpu::Buffer,
    pub(crate) map_index_buffer: wgpu::Buffer,
    pub(crate) map_num_indices: u32,
    pub(crate) map_zoom: f32,
    pub(crate) map_pan: (f32, f32),

    // M11: GeoJSON marker + line + polygon systems
    pub(crate) marker_system: marker::MarkerSystem,
    pub(crate) line_system: line::LineSystem,
    pub(crate) polygon_system: polygon::PolygonSystem,

    // M12: Live data sources (REST API → GeoJSON)
    pub(crate) live_source_manager: live_source::LiveSourceManager,
    // M17d: Custom REST/GeoJSON feed polling
    pub(crate) rest_feed_manager: custom_source::RestFeedManager,
    // M17h: Custom Shapefile sources (file-based, loaded once per add)
    pub(crate) shapefile_source_manager: custom_source::ShapefileSourceManager,
    // M17i: Custom CSV sources (file-based, lat/lon point clouds)
    pub(crate) csv_source_manager: custom_source::CsvSourceManager,
    // M17i (GPX flavour): Custom GPX sources (waypoints / routes / tracks)
    pub(crate) gpx_source_manager: custom_source::GpxSourceManager,
    pub(crate) satellite_tracker: satellite::SatelliteTracker,

    /// M16 Phase 4: Single owner of cache, worker pool, compositor, state.
    pub(crate) tile_manager: tile::TileManager,
    /// GPU texture for the composited tile overlay
    pub(crate) tile_overlay_texture: Option<wgpu::Texture>,
    /// Bind group for tile overlay rendering (@group(1): texture)
    pub(crate) tile_overlay_bind_group: Option<wgpu::BindGroup>,
    /// Settings bind group for tile overlay (@group(2): opacity)
    pub(crate) tile_overlay_settings_bind_group: wgpu::BindGroup,
    /// Settings buffer for tile overlay (updated each frame with dynamic opacity)
    pub(crate) tile_overlay_settings_buffer: wgpu::Buffer,

    // Mouse controls
    pub(crate) mouse_pressed: bool,
    pub(crate) last_mouse_pos: Option<(f64, f64)>,
}

impl GpuState {
    // GpuState::new() is in gpu_init.rs

    fn resize(&mut self, new_width: u32, new_height: u32) {
        if new_width > 0 && new_height > 0 {
            self.config.width = new_width;
            self.config.height = new_height;
            self.surface.configure(&self.device, &self.config);
            self.camera.set_aspect(new_width, new_height);
        }
    }


    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        // --- Update phase ---
        self.update_frame_state();
        self.process_downloads();
        let sat_utc = self.update_data_sources();
        self.handle_gui_requests();
        self.update_tiles();
        self.update_camera_follow();

        // --- Camera uniform → GPU ---
        // Sun direction (reuse sat_utc for consistent time across all systems)
        let sun_dir = {
            use chrono::{Datelike, Timelike};
            sun::sun_direction_at(
                sat_utc.year(), sat_utc.month(), sat_utc.day(),
                sat_utc.hour(), sat_utc.minute(), sat_utc.second(),
            )
        };

        let camera_uniform = match self.view_mode {
            ViewMode::Globe3D => self.camera.to_uniform(sun_dir),
            ViewMode::Map2D => self.camera.to_map_uniform(
                sun_dir,
                self.map_zoom,
                self.map_pan,
                QUAD_HALF_WIDTH,
            ),
        };
        self.queue.write_buffer(
            &self.camera_uniform_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniform]),
        );

        // --- Render passes ---
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            // M11: Ensure marker buffer is ready before render pass begins
            self.marker_system.ensure_buffer(&self.device);

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.005,
                            g: 0.005,
                            b: 0.02,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            match self.view_mode {
                ViewMode::Globe3D => {
                    // Stars first
                    render_pass.set_pipeline(&self.star_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, self.star_vertex_buffer.slice(..));
                    render_pass.draw(0..6, 0..self.star_count);

                    // Globe
                    render_pass.set_pipeline(&self.render_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    render_pass.set_bind_group(1, &self.texture_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(
                        self.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(0..self.num_indices, 0, 0..1);

                    // M16: Tile zoom overlay (rendered BEFORE regular layers so clouds appear on top)
                    render_pass.set_pipeline(&self.overlay_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    if let Some(tile_bg) = &self.tile_overlay_bind_group {
                        if self.tile_manager.has_content() {
                            render_pass.set_bind_group(1, tile_bg, &[]);
                            render_pass.set_bind_group(2, &self.tile_overlay_settings_bind_group, &[]);
                            render_pass.draw_indexed(0..self.num_indices, 0, 0..1);
                        }
                    }

                    // Overlay layers (clouds, grid, etc. — on top of tiles)
                    for layer in self.layer_stack.enabled_layers() {
                        render_pass.set_bind_group(1, &layer.texture_bind_group, &[]);
                        render_pass.set_bind_group(2, &layer.settings_bind_group, &[]);
                        render_pass.draw_indexed(0..self.num_indices, 0, 0..1);
                    }

                    // M11: GeoJSON polygons (under lines and markers)
                    self.polygon_system.render(&mut render_pass, &self.camera_bind_group);

                    // M11: GeoJSON lines
                    self.line_system.render(&mut render_pass, &self.camera_bind_group);

                    // M11: GeoJSON markers (on top)
                    self.marker_system.render(&mut render_pass, &self.camera_bind_group);
                }
                ViewMode::Map2D => {
                    // Map
                    render_pass.set_pipeline(&self.map_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    render_pass.set_bind_group(1, &self.texture_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, self.map_vertex_buffer.slice(..));
                    render_pass.set_index_buffer(
                        self.map_index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(0..self.map_num_indices, 0, 0..1);

                    // M16: Tile zoom overlay on map (BEFORE regular layers)
                    render_pass.set_pipeline(&self.overlay_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    if let Some(tile_bg) = &self.tile_overlay_bind_group {
                        if self.tile_manager.has_content() {
                            render_pass.set_bind_group(1, tile_bg, &[]);
                            render_pass.set_bind_group(2, &self.tile_overlay_settings_bind_group, &[]);
                            render_pass.draw_indexed(0..self.map_num_indices, 0, 0..1);
                        }
                    }

                    // Overlay layers on map (clouds, grid — on top of tiles)
                    for layer in self.layer_stack.enabled_layers() {
                        render_pass.set_bind_group(1, &layer.texture_bind_group, &[]);
                        render_pass.set_bind_group(2, &layer.settings_bind_group, &[]);
                        render_pass.draw_indexed(0..self.map_num_indices, 0, 0..1);
                    }

                    // M11: GeoJSON polygons (under lines and markers)
                    self.polygon_system.render(&mut render_pass, &self.camera_bind_group);

                    // M11: GeoJSON lines
                    self.line_system.render(&mut render_pass, &self.camera_bind_group);

                    // M11: GeoJSON markers (on top)
                    self.marker_system.render(&mut render_pass, &self.camera_bind_group);
                }
            }
        }

        // --- Screen-space overlays ---
        self.project_screen_overlays(sat_utc);

        // --- egui ---
        // egui render
        // =============================================================
        let raw_input = self.gui.state.take_egui_input(&self.window);
        let full_output = self.gui.ctx.run(raw_input, |ctx| {
            gui::draw_ui(ctx, &mut self.gui_state, &self.catalog);
            gui::draw_labels(ctx, &mut self.gui_state);
            gui::draw_satellites(ctx, &self.gui_state);
            gui::draw_planets(ctx, &self.gui_state);
        });

        self.gui.state.handle_platform_output(&self.window, full_output.platform_output);

        let pixels_per_point = self.gui.ctx.pixels_per_point();
        let clipped_primitives = self.gui.ctx.tessellate(full_output.shapes, pixels_per_point);

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };

        for (id, image_delta) in &full_output.textures_delta.set {
            self.gui.renderer.update_texture(
                &self.device,
                &self.queue,
                *id,
                image_delta,
            );
        }
        self.gui.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            let egui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            let mut egui_pass = egui_pass.forget_lifetime();
            self.gui.renderer.render(&mut egui_pass, &clipped_primitives, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            self.gui.renderer.free_texture(id);
        }

        // --- Post-render sync ---
        self.sync_post_render();

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    // =========================================================================
    // Extracted update methods (Phase 2 refactoring)
    // =========================================================================

    /// Updates FPS counter, syncs view mode, and refreshes layer GUI state.
    fn update_frame_state(&mut self) {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        if dt > 0.0 {
            self.gui_state.fps = self.gui_state.fps * 0.9 + (1.0 / dt) * 0.1;
        }

        // Sync GUI → ViewMode
        let gui_wants_map = self.gui_state.view_mode_map;
        let current_is_map = self.view_mode == ViewMode::Map2D;
        if gui_wants_map != current_is_map {
            if gui_wants_map {
                log::info!("GUI: Switching to 2D map view");
                self.map_zoom = 1.0;
                self.map_pan = (0.0, 0.0);
                self.view_mode = ViewMode::Map2D;
            } else {
                log::info!("GUI: Switching to 3D globe");
                self.view_mode = ViewMode::Globe3D;
            }
        }

        // Sync layer state → GUI
        if self.gui_state.layers.len() != self.layer_stack.layers.len() {
            self.gui_state.layers = self.layer_stack.layers.iter().map(|l| {
                gui::LayerGuiEntry {
                    id: l.id.clone(),
                    label: l.label.clone(),
                    provider_id: l.provider_id.clone(),
                    enabled: l.enabled,
                    opacity: l.opacity,
                }
            }).collect();
        }
    }

    /// Polls completed downloads and creates GPU textures for finished layers.
    fn process_downloads(&mut self) {
        let completed = self.download_manager.poll();
        for download in completed {
            match download.result {
                Ok(img) => {
                    log::info!(
                        "Download complete: '{}' ({}×{})",
                        download.provider_id, img.width, img.height,
                    );
                    self.layer_stack.layers.retain(|l| l.provider_id != download.provider_id);

                    let tex = match texture::GpuTexture::from_rgba(
                        &self.device, &self.queue,
                        &img.rgba, img.width, img.height, &download.label,
                    ) {
                        Ok(t) => t,
                        Err(e) => { log::error!("Texture creation failed: {}", e); continue; }
                    };

                    let mut new_layer = layer::Layer::new(
                        &download.provider_id, &download.label, &download.provider_id,
                        tex, download.opacity,
                        &self.overlay_layer_bind_group_layout,
                        &self.overlay_settings_bind_group_layout,
                        &self.device,
                    );
                    new_layer.enabled = download.enabled;
                    self.layer_stack.add(new_layer);

                    self.gui_state.set_download_status(
                        &download.provider_id, gui::DownloadStatus::Ready,
                    );
                    self.gui_state.layers_changed = true;
                }
                Err(e) => {
                    log::error!("Download failed for '{}': {}", download.provider_id, e);
                    self.gui_state.set_download_status(
                        &download.provider_id, gui::DownloadStatus::Error(e),
                    );
                }
            }
        }
    }

    /// Polls live data sources, propagates satellites, returns current UTC time.
    fn update_data_sources(&mut self) -> chrono::DateTime<chrono::Utc> {
        // Live data sources
        let live_results = self.live_source_manager.poll();
        if !live_results.is_empty() {
            for result in live_results {
                self.marker_system.replace_layer(result.layer);
            }
            self.polygon_system.rebuild_from_layers(
                self.marker_system.geo_layers(), &self.device,
            );
            self.line_system.rebuild_from_layers(
                self.marker_system.geo_layers(),
                self.polygon_system.outline_segments(),
                &self.device,
            );
        }

        // M17d: Poll custom REST/GeoJSON feeds
        let rest_results = self.rest_feed_manager.poll();
        if !rest_results.is_empty() {
            for result in rest_results {
                self.marker_system.replace_layer(result.layer);
            }
            self.polygon_system.rebuild_from_layers(
                self.marker_system.geo_layers(), &self.device,
            );
            self.line_system.rebuild_from_layers(
                self.marker_system.geo_layers(),
                self.polygon_system.outline_segments(),
                &self.device,
            );
        }

        // Satellite tracking
        self.satellite_tracker.poll_downloads();
        self.gui_state.satellite_count = self.satellite_tracker.count();
        self.gui_state.satellite_downloading = self.satellite_tracker.is_downloading();
        let utc = if self.gui_state.time_live {
            chrono::Utc::now()
        } else {
            use chrono::TimeZone;
            chrono::Utc
                .with_ymd_and_hms(
                    self.gui_state.selected_year,
                    self.gui_state.selected_month,
                    self.gui_state.selected_day,
                    self.gui_state.selected_hour,
                    self.gui_state.selected_minute, 0,
                )
                .single()
                .unwrap_or_else(chrono::Utc::now)
        };
        self.satellite_tracker.propagate(&utc);
        utc
    }

    // handle_gui_requests() is in gui_requests.rs

    /// Drives the TileManager state machine each frame, then uploads any
    /// dirty compositor buffer to the GPU.
    fn update_tiles(&mut self) {
        fn age_from_days(days: u32) -> Option<std::time::Duration> {
            if days == 0 {
                None // 0 = no age limit
            } else {
                Some(std::time::Duration::from_secs(days as u64 * 24 * 3600))
            }
        }

        // Cache-clear button: delegate to the manager (bumps gen, clears
        // active source only, resets compositor). Drop GPU texture so the
        // old pixels stop rendering until new tiles come in.
        if self.gui_state.cache_clear_request {
            self.gui_state.cache_clear_request = false;
            self.tile_manager.clear_cache(tile::ClearScope::ActiveSource);
            self.tile_overlay_texture = None;
            self.tile_overlay_bind_group = None;
            self.gui_state.cache_usage_mb = self.tile_manager.cache_size_mb();
        }

        // Build view + settings snapshots, run the manager
        let view = tile::ViewState {
            yaw: self.camera.yaw,
            pitch: self.camera.pitch,
            distance: self.camera.distance,
            fov_y: self.camera.fov_y,
            aspect: self.camera.aspect,
        };
        let tile_settings = tile::TileSettings {
            source_id: self.gui_state.settings.tile_source.clone(),
            cache_max_mb: self.gui_state.settings.tile_cache_max_mb,
            cache_max_age: age_from_days(self.gui_state.settings.tile_cache_max_days),
            zoom_bias: self.gui_state.settings.tile_zoom_bias,
        };
        let frame = self.tile_manager.update(view, &tile_settings);

        // Source-change reset: drop the GPU texture so the previous
        // source's pixels don't keep rendering. (Zoom changes intentionally
        // do NOT reset — old pixels stay visible until new-zoom tiles
        // overwrite them, which keeps the transition seamless.)
        if frame.reset {
            self.tile_overlay_texture = None;
            self.tile_overlay_bind_group = None;
        }

        // Dynamic opacity crossfade (close-up zoom reveals tiles)
        let tile_opacity = self.tile_manager.opacity_for_distance(self.camera.distance);
        let settings = OverlaySettings { opacity: tile_opacity, _pad: [0.0; 3] };
        self.queue.write_buffer(
            &self.tile_overlay_settings_buffer, 0, bytemuck::bytes_of(&settings),
        );

        // Upload any dirty compositor sub-region to the GPU. Phase D:
        // only the dirty rectangle is sent — a single tile at z=3 is
        // ~512×512 = 1 MB instead of the full 33 MB buffer.
        if let Some(upload) = self.tile_manager.take_upload() {
            if self.tile_overlay_texture.is_none() {
                let size = wgpu::Extent3d {
                    width: upload.buffer_width,
                    height: upload.buffer_height,
                    depth_or_array_layers: 1,
                };
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Tile Overlay Texture"),
                    size, mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("Tile Overlay Sampler"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    address_mode_u: wgpu::AddressMode::Repeat,
                    address_mode_v: wgpu::AddressMode::ClampToEdge,
                    ..Default::default()
                });
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Tile Overlay Bind Group"),
                    layout: &self.overlay_layer_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                    ],
                });
                self.tile_overlay_texture = Some(tex);
                self.tile_overlay_bind_group = Some(bind_group);
            }
            if let Some(tex) = &self.tile_overlay_texture {
                // The source data is the full compositor buffer; we tell
                // wgpu to treat rows as `buffer_width * 4` bytes wide and
                // start at the offset corresponding to the rect's top-left.
                let row_stride_bytes = upload.buffer_width * 4;
                let offset = (upload.origin_y as u64) * (row_stride_bytes as u64)
                    + (upload.origin_x as u64) * 4;
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: upload.origin_x,
                            y: upload.origin_y,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    upload.data,
                    wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(row_stride_bytes),
                        rows_per_image: Some(upload.height),
                    },
                    wgpu::Extent3d {
                        width: upload.width,
                        height: upload.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // Periodic refresh of cache-usage + metrics for the GUI status line
        self.gui_state.cache_usage_mb = self.tile_manager.cache_size_mb();
        self.gui_state.tile_metrics = self.tile_manager.metrics().clone();

        // Compute meters-per-pixel (vertical) for the scale HUD.
        // Differs between view modes:
        //   - 3D globe: perspective frustum at the visible globe surface.
        //               Near-surface depth ≈ distance - 1 (Earth radius = 1
        //               world unit). visible world height = 2·(d−1)·tan(fov/2).
        //               One world unit == Earth equatorial radius.
        //   - 2D map: orthographic. Visible world height = QUAD_HALF_WIDTH /
        //              map_zoom. The quad (height 4.0 world units) represents
        //              180° of latitude, so one world unit ≈ πR/4 ≈ 5 009 377 m.
        // A 0.0 result (screen too small etc.) tells the HUD to hide.
        const EARTH_RADIUS_M: f32 = 6_378_137.0;
        const MAP_METERS_PER_WORLD_UNIT: f32 =
            std::f32::consts::PI * EARTH_RADIUS_M / 4.0;
        let screen_h = self.config.height as f32;
        self.gui_state.scale_meters_per_pixel = if screen_h <= 0.0 {
            0.0
        } else {
            match self.view_mode {
                ViewMode::Globe3D => {
                    let near = (self.camera.distance - 1.0).max(0.1);
                    let visible_world_h =
                        2.0 * near * (self.camera.fov_y / 2.0).tan();
                    visible_world_h * EARTH_RADIUS_M / screen_h
                }
                ViewMode::Map2D => {
                    let visible_world_h = QUAD_HALF_WIDTH / self.map_zoom;
                    visible_world_h * MAP_METERS_PER_WORLD_UNIT / screen_h
                }
            }
        };
    }

    /// Smoothly tracks a satellite with the camera (M13c).
    fn update_camera_follow(&mut self) {
        if let Some(norad_id) = self.gui_state.follow_satellite {
            if self.view_mode == ViewMode::Globe3D {
                if let Some(sat) = self.satellite_tracker.states().iter()
                    .find(|s| s.norad_id == norad_id)
                {
                    let lat_rad = (sat.latitude as f32).to_radians();
                    let lon_rad = (sat.longitude as f32).to_radians();
                    let target_yaw = std::f32::consts::FRAC_PI_2 - lon_rad;
                    let target_pitch = -lat_rad;

                    // Normalize yaw
                    let mut dy = target_yaw - self.camera.yaw;
                    if dy > std::f32::consts::PI { dy -= 2.0 * std::f32::consts::PI; }
                    if dy < -std::f32::consts::PI { dy += 2.0 * std::f32::consts::PI; }

                    let t = 0.08;
                    self.camera.yaw += dy * t;
                    self.camera.pitch += (target_pitch - self.camera.pitch) * t;

                    // Normalize yaw to [-π, π]
                    if self.camera.yaw > std::f32::consts::PI {
                        self.camera.yaw -= 2.0 * std::f32::consts::PI;
                    }
                    if self.camera.yaw < -std::f32::consts::PI {
                        self.camera.yaw += 2.0 * std::f32::consts::PI;
                    }
                }
            }
        }
    }

    /// Projects satellites, ground tracks, and planets to screen space.
    // project_screen_overlays() is in overlay_project.rs

    /// Syncs GUI changes back to layer state and handles post-frame requests.
    fn sync_post_render(&mut self) {
        for gui_entry in &self.gui_state.layers {
            if let Some(layer) = self.layer_stack.layers.iter_mut().find(|l| l.id == gui_entry.id) {
                let enabled_changed = layer.enabled != gui_entry.enabled;
                let opacity_changed = (layer.opacity - gui_entry.opacity).abs() > 0.001;
                layer.enabled = gui_entry.enabled;
                if opacity_changed {
                    layer.opacity = gui_entry.opacity;
                    let settings = OverlaySettings { opacity: gui_entry.opacity, _pad: [0.0; 3] };
                    self.queue.write_buffer(
                        &layer.settings_buffer, 0, bytemuck::cast_slice(&[settings]),
                    );
                }
                if enabled_changed || opacity_changed {
                    self.gui_state.layers_changed = true;
                }
            }
        }
        self.handle_geojson_requests();
        self.handle_live_source_requests();
    }


    // handle_geojson_requests(), load_geojson_file(), handle_live_source_requests()
    // are in gui_requests.rs
}

fn main() {
    app::run();
}

