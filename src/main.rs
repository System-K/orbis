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
use std::sync::mpsc;
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

use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes},
};

// Module declarations
mod camera;
mod gibs;
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

use camera::Camera;
use layer::LayerStack;
use mesh::Vertex;
use provider::{LayerImage, ProviderCatalog};
use texture::GpuTexture;

/// Display mode: 3D globe or 2D map
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Globe3D,
    Map2D,
}

/// Half-width of the map quad (height = QUAD_HALF_WIDTH / 2 due to 2:1 ratio)
const QUAD_HALF_WIDTH: f32 = 4.0;

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

// =============================================================================
// Download Manager (M9)
// =============================================================================
// Manages background downloads for multiple providers concurrently.
// Each download runs in its own thread and sends results back via channel.

/// A completed download result from a background thread.
struct DownloadResult {
    /// Which provider produced this image
    provider_id: String,
    /// The label for the layer (includes date)
    label: String,
    /// Opacity to use
    opacity: f32,
    /// Whether the layer should be enabled
    enabled: bool,
    /// The downloaded image (or error)
    result: Result<LayerImage, String>,
}

/// Manages pending download receivers.
struct DownloadManager {
    /// Active download channels
    pending: Vec<mpsc::Receiver<DownloadResult>>,
}

impl DownloadManager {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Starts a download for a provider.
    ///
    /// The download runs in a background thread. Results are collected
    /// via `poll()` on the next frame(s).
    fn start_download(
        &mut self,
        catalog: &ProviderCatalog,
        provider_id: &str,
        date: Option<chrono::NaiveDate>,
        opacity: f32,
        enabled: bool,
    ) {
        let provider = match catalog.find(provider_id) {
            Some(p) => p,
            None => {
                log::warn!("Provider not found: '{}'", provider_id);
                return;
            }
        };

        let info = provider.info().clone();
        let pid = provider_id.to_string();

        // Clone data needed by the background thread
        // We need to re-find the provider inside the thread since
        // trait objects aren't easily Send across thread boundaries.
        // Instead, we use gibs providers directly.
        let (tx, rx) = mpsc::channel();

        // Build a closure that captures what we need
        // Use separate cache directories for GIBS vs external WMS
        let cache_dir_path = if pid.starts_with("gibs_") || pid == "builtin:grid" {
            PathBuf::from("cache/gibs")
        } else {
            PathBuf::from("cache/wms")
        };
        let provider_id_clone = pid.clone();
        let label = info.label.clone();

        std::thread::spawn(move || {
            // Re-create the provider in the thread
            // (providers are lightweight and stateless)
            let mut providers = crate::gibs::all_gibs_providers();
            providers.extend(crate::wms::all_wms_providers());
            let provider = providers
                .iter()
                .find(|p| p.info().id == provider_id_clone);

            let result = match provider {
                Some(p) => {
                    if let Some(d) = date {
                        p.fetch(&d, &cache_dir_path)
                    } else {
                        // Use fallback (try yesterday, day before, etc.)
                        match p.fetch_with_fallback(&cache_dir_path) {
                            Ok((img, _date)) => Ok(img),
                            Err(e) => Err(e),
                        }
                    }
                }
                None => Err(format!("Provider '{}' not found in thread", provider_id_clone)),
            };

            let download_label = if let Some(d) = date {
                format!("{} ({})", label, d.format("%Y-%m-%d"))
            } else {
                label
            };

            let _ = tx.send(DownloadResult {
                provider_id: provider_id_clone,
                label: download_label,
                opacity,
                enabled,
                result,
            });
        });

        self.pending.push(rx);
        log::info!("Download started for provider '{}'", pid);
    }

    /// Polls all pending downloads for completed results.
    ///
    /// Returns a list of completed downloads. Incomplete downloads
    /// remain in the pending list for the next poll.
    fn poll(&mut self) -> Vec<DownloadResult> {
        let mut completed = Vec::new();
        let mut still_pending = Vec::new();

        for rx in self.pending.drain(..) {
            match rx.try_recv() {
                Ok(result) => completed.push(result),
                Err(mpsc::TryRecvError::Empty) => still_pending.push(rx),
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Download thread disconnected unexpectedly");
                }
            }
        }

        self.pending = still_pending;
        completed
    }
}

// --- GPU State ---
struct GpuState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    render_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,

    camera: Camera,
    camera_uniform_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    // Texture resources (day + night)
    #[allow(dead_code)]
    earth_day_texture: GpuTexture,
    #[allow(dead_code)]
    earth_night_texture: GpuTexture,
    texture_bind_group: wgpu::BindGroup,

    // M4: Overlay layer system
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_layer_bind_group_layout: wgpu::BindGroupLayout,
    overlay_settings_bind_group_layout: wgpu::BindGroupLayout,
    layer_stack: LayerStack,

    // M9: Provider catalog + download manager
    catalog: ProviderCatalog,
    download_manager: DownloadManager,

    // M6: GUI
    gui: gui::Gui,
    gui_state: gui::GuiState,
    last_frame_time: std::time::Instant,

    // M8a: Starfield
    star_pipeline: wgpu::RenderPipeline,
    star_vertex_buffer: wgpu::Buffer,
    star_count: u32,

    // M7: 2D map projection
    view_mode: ViewMode,
    map_pipeline: wgpu::RenderPipeline,
    map_vertex_buffer: wgpu::Buffer,
    map_index_buffer: wgpu::Buffer,
    map_num_indices: u32,
    map_zoom: f32,
    map_pan: (f32, f32),

    // M11: GeoJSON marker + line + polygon systems
    marker_system: marker::MarkerSystem,
    line_system: line::LineSystem,
    polygon_system: polygon::PolygonSystem,

    // M12: Live data sources (REST API → GeoJSON)
    live_source_manager: live_source::LiveSourceManager,
    satellite_tracker: satellite::SatelliteTracker,

    /// M16: Tile disk cache with LRU eviction (shared with download queue)
    tile_cache: std::sync::Arc<tile::TileCache>,
    /// Background tile download queue
    tile_download_queue: tile::TileDownloadQueue,
    /// Tile compositor (stitches tiles into equirectangular buffer)
    tile_compositor: tile::TileCompositor,
    /// GPU texture for the composited tile overlay
    tile_overlay_texture: Option<wgpu::Texture>,
    /// Bind group for tile overlay rendering (@group(1): texture)
    tile_overlay_bind_group: Option<wgpu::BindGroup>,
    /// Settings bind group for tile overlay (@group(2): opacity)
    tile_overlay_settings_bind_group: wgpu::BindGroup,
    /// Settings buffer for tile overlay (updated each frame with dynamic opacity)
    tile_overlay_settings_buffer: wgpu::Buffer,
    /// Frame counter for periodic cache size updates (not every frame)
    tile_cache_check_counter: u32,
    /// Previous tile zoom level (to detect changes)
    tile_prev_zoom: u32,
    /// Previous tile source ID (to detect source switches)
    tile_prev_source: String,

    // Mouse controls
    mouse_pressed: bool,
    last_mouse_pos: Option<(f64, f64)>,
}

impl GpuState {
    async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        // --- Instance, Surface, Adapter, Device ---
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone()).unwrap();

        // Try hardware GPU first, then software fallback (WARP/llvmpipe)
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
        {
            Ok(adapter) => {
                log::info!("GPU (Hardware): {}", adapter.get_info().name);
                adapter
            }
            Err(e) => {
                log::warn!("No hardware GPU found ({}) — trying software renderer...", e);
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::default(),
                        compatible_surface: Some(&surface),
                        force_fallback_adapter: true,
                    })
                    .await
                    .expect("Neither hardware GPU nor software renderer available!")
            }
        };

        let adapter_info = adapter.get_info();
        log::info!(
            "Adapter: {} ({:?})",
            adapter_info.name,
            adapter_info.backend,
        );

        let (device, queue): (wgpu::Device, wgpu::Queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Orbis Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .expect("Failed to create GPU device!");

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: if surface_caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
                wgpu::PresentMode::Fifo // VSync — caps to display refresh rate
            } else {
                surface_caps.present_modes[0]
            },
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        // =================================================================
        // Sphere mesh generation
        // =================================================================
        let (vertices, indices) = mesh::generate_sphere(128, 64, 1.0);

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Sphere Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Sphere Index Buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let num_indices = indices.len() as u32;

        // =================================================================
        // Camera + Uniform Buffer
        // =================================================================
        let camera = Camera::new(size.width as f32 / size.height as f32);
        let sun_dir = sun::sun_direction_now();
        let camera_uniform = camera.to_uniform(sun_dir);

        let camera_uniform_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Uniform Buffer"),
                contents: bytemuck::cast_slice(&[camera_uniform]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        // =================================================================
        // Bind Group Layouts
        // =================================================================
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Camera Bind Group Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera Bind Group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_uniform_buffer.as_entire_binding(),
            }],
        });

        // =================================================================
        // Textures: Day (Blue Marble) + Night (Black Marble)
        // =================================================================
        let earth_day_texture = GpuTexture::from_file(
            &device,
            &queue,
            &app_path("assets/textures/earth_day.png"),
            "Earth Day Texture",
        ).unwrap_or_else(|e| {
            log::error!("{}", e);
            GpuTexture::fallback(&device, &queue, "Earth Day Fallback")
        });

        let earth_night_texture = GpuTexture::from_file(
            &device,
            &queue,
            &app_path("assets/textures/earth_night.jpg"),
            "Earth Night Texture",
        ).unwrap_or_else(|e| {
            log::error!("{}", e);
            GpuTexture::fallback(&device, &queue, "Earth Night Fallback")
        });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Texture Bind Group Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Earth Texture Bind Group"),
            layout: &texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&earth_day_texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&earth_night_texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&earth_day_texture.sampler),
                },
            ],
        });

        // =================================================================
        // Globe shader + pipeline
        // =================================================================
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Globe Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/globe.wgsl").into(),
            ),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Render Pipeline Layout"),
            bind_group_layouts: &[&camera_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Globe Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::buffer_layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        // =================================================================
        // Overlay pipeline + layer system
        // =================================================================
        let overlay_layer_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Overlay Layer Bind Group Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let overlay_settings_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Overlay Settings Bind Group Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Overlay Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/overlay.wgsl").into(),
            ),
        });

        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Overlay Pipeline Layout"),
                bind_group_layouts: &[
                    &camera_bind_group_layout,
                    &overlay_layer_bind_group_layout,
                    &overlay_settings_bind_group_layout,
                ],
                push_constant_ranges: &[],
            });

        let overlay_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Overlay Render Pipeline"),
                layout: Some(&overlay_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::buffer_layout()],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &overlay_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
                cache: None,
            });

        // =================================================================
        // M9: Provider catalog + layer initialization from settings
        // =================================================================
        let catalog = provider::build_default_catalog();
        let mut layer_stack = LayerStack::new();
        let mut download_manager = DownloadManager::new();

        // Load settings and restore active layers
        let loaded_settings = settings::Settings::load();

        // Add built-in layers immediately (grid), queue downloads for others
        for layer_cfg in &loaded_settings.active_layers {
            if layer_cfg.provider_id == "builtin:grid" {
                let grid_texture = layer::generate_grid_texture(&device, &queue, 2048, 1024);
                let grid_layer = layer::Layer::new(
                    "grid",
                    &i18n::t("layer_grid"),
                    "builtin:grid",
                    grid_texture,
                    layer_cfg.opacity,
                    &overlay_layer_bind_group_layout,
                    &overlay_settings_bind_group_layout,
                    &device,
                );
                layer_stack.add(grid_layer);
            } else {
                // Queue download for this provider
                download_manager.start_download(
                    &catalog,
                    &layer_cfg.provider_id,
                    None, // Use fallback dates
                    layer_cfg.opacity,
                    layer_cfg.enabled,
                );
            }
        }

        let cache_max_mb = loaded_settings.tile_cache_max_mb;
        let cache_max_days = loaded_settings.tile_cache_max_days;
        let gui_state = gui::GuiState::new(loaded_settings);

        // =================================================================
        // M7: 2D map pipeline + quad mesh
        // =================================================================
        let (quad_vertices, quad_indices) = mesh::generate_quad(QUAD_HALF_WIDTH);

        let map_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Map Quad Vertex Buffer"),
            contents: bytemuck::cast_slice(&quad_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let map_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Map Quad Index Buffer"),
            contents: bytemuck::cast_slice(&quad_indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let map_num_indices = quad_indices.len() as u32;

        let map_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Map Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/map.wgsl").into(),
            ),
        });

        let map_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Map Pipeline Layout"),
            bind_group_layouts: &[&camera_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        let map_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Map Render Pipeline"),
            layout: Some(&map_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &map_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::buffer_layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &map_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        // =================================================================
        // M14a: Star catalog pipeline
        // =================================================================
        let stars = mesh::load_star_catalog(&app_path("assets/data/stars.bin"));
        let star_count = stars.len() as u32;

        let star_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Star Vertex Buffer"),
            contents: bytemuck::cast_slice(&stars),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let star_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Star Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/stars.wgsl").into(),
            ),
        });

        let star_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Star Pipeline Layout"),
            bind_group_layouts: &[&camera_bind_group_layout],
            push_constant_ranges: &[],
        });

        let star_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Star Render Pipeline"),
            layout: Some(&star_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &star_shader,
                entry_point: Some("vs_main"),
                buffers: &[mesh::StarVertex::buffer_layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &star_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::OVER,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        // =================================================================
        // GUI initialization
        // =================================================================
        let egui_gui = gui::Gui::new(&window, &device, surface_format);

        // =================================================================
        // M11: Marker system for GeoJSON point rendering
        // =================================================================
        let mut marker_system = marker::MarkerSystem::new(
            &device,
            surface_format,
            &camera_bind_group_layout,
        );
        let mut line_system = line::LineSystem::new(
            &device,
            surface_format,
            &camera_bind_group_layout,
        );
        let mut polygon_system = polygon::PolygonSystem::new(
            &device,
            surface_format,
            &camera_bind_group_layout,
        );

        // Auto-load bundled demo GeoJSON files from assets/geojson/
        let demo_dir = app_path("assets/geojson");
        if demo_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&demo_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("geojson") {
                        match geojson::load_geojson_file(&path) {
                            Ok(mut layer) => {
                                layer.visible = false; // loaded but hidden by default
                                log::info!("Bundled GeoJSON '{}': {} features (hidden)",
                                    layer.name, layer.len());
                                marker_system.add_layer(layer);
                            }
                            Err(e) => {
                                log::warn!("Could not load bundled GeoJSON {:?}: {}",
                                    path.file_name().unwrap_or_default(), e);
                            }
                        }
                    }
                }
            }
        }

        // Rebuild geometry buffers from loaded layers
        polygon_system.rebuild_from_layers(marker_system.geo_layers(), &device);
        line_system.rebuild_from_layers(
            marker_system.geo_layers(),
            polygon_system.outline_segments(),
            &device,
        );

        // M16: Tile overlay settings (dynamic opacity, created before Self moves device)
        let tile_overlay_settings_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Tile Overlay Settings Buffer"),
            contents: bytemuck::bytes_of(&OverlaySettings { opacity: 0.0, _pad: [0.0; 3] }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let tile_overlay_settings_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Tile Overlay Settings Bind Group"),
            layout: &overlay_settings_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: tile_overlay_settings_buffer.as_entire_binding(),
            }],
        });

        // M16: Create shared tile cache
        let tile_cache_arc = std::sync::Arc::new(tile::TileCache::new(tile::CacheConfig {
            cache_dir: app_path("cache/tiles"),
            max_size_bytes: (cache_max_mb as u64) * 1024 * 1024,
            max_age: if cache_max_days == 0 {
                std::time::Duration::from_secs(365 * 24 * 3600)
            } else {
                std::time::Duration::from_secs(cache_max_days as u64 * 24 * 3600)
            },
        }));

        Self {
            window,
            surface,
            device,
            queue,
            config,
            render_pipeline,
            vertex_buffer,
            index_buffer,
            num_indices,
            camera,
            camera_uniform_buffer,
            camera_bind_group,
            earth_day_texture,
            earth_night_texture,
            texture_bind_group,
            overlay_pipeline,
            overlay_layer_bind_group_layout,
            overlay_settings_bind_group_layout,
            layer_stack,
            catalog,
            download_manager,
            gui: egui_gui,
            gui_state,
            last_frame_time: std::time::Instant::now(),
            star_pipeline,
            star_vertex_buffer,
            star_count,
            view_mode: ViewMode::Globe3D,
            map_pipeline,
            map_vertex_buffer,
            map_index_buffer,
            map_num_indices,
            map_zoom: 1.0,
            map_pan: (0.0, 0.0),
            marker_system,
            line_system,
            polygon_system,
            live_source_manager: live_source::LiveSourceManager::new(),
            satellite_tracker: {
                let mut tracker = satellite::SatelliteTracker::new();
                tracker.request_refresh(); // start downloading OMMs at startup
                tracker
            },
            tile_cache: tile_cache_arc.clone(),
            tile_download_queue: tile::TileDownloadQueue::new(
                tile_cache_arc,
                tile::builtin_tile_sources(),
            ),
            tile_compositor: tile::TileCompositor::new(4096, 2048),
            tile_overlay_texture: None,
            tile_overlay_bind_group: None,
            tile_overlay_settings_bind_group,
            tile_overlay_settings_buffer,
            tile_cache_check_counter: 0,
            tile_prev_zoom: 0,
            tile_prev_source: String::new(),
            mouse_pressed: false,
            last_mouse_pos: None,
        }
    }

    fn resize(&mut self, new_width: u32, new_height: u32) {
        if new_width > 0 && new_height > 0 {
            self.config.width = new_width;
            self.config.height = new_height;
            self.surface.configure(&self.device, &self.config);
            self.camera.set_aspect(new_width, new_height);
        }
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        // =============================================================
        // FPS calculation
        // =============================================================
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        if dt > 0.0 {
            self.gui_state.fps = self.gui_state.fps * 0.9 + (1.0 / dt) * 0.1;
        }

        // =============================================================
        // M7: Sync GUI → ViewMode
        // =============================================================
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

        // =============================================================
        // Sync layer state → GUI
        // =============================================================
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

        // =============================================================
        // M9: Process completed downloads
        // =============================================================
        let completed = self.download_manager.poll();
        for download in completed {
            match download.result {
                Ok(img) => {
                    log::info!(
                        "Download complete: '{}' ({}×{})",
                        download.provider_id,
                        img.width,
                        img.height,
                    );

                    // Remove existing layer from same provider (replace)
                    self.layer_stack.layers.retain(|l| l.provider_id != download.provider_id);

                    let tex = match texture::GpuTexture::from_rgba(
                        &self.device,
                        &self.queue,
                        &img.rgba,
                        img.width,
                        img.height,
                        &download.label,
                    ) {
                        Ok(t) => t,
                        Err(e) => {
                            log::error!("Texture creation failed: {}", e);
                            continue;
                        }
                    };

                    let new_layer = layer::Layer::new(
                        &download.provider_id,
                        &download.label,
                        &download.provider_id,
                        tex,
                        download.opacity,
                        &self.overlay_layer_bind_group_layout,
                        &self.overlay_settings_bind_group_layout,
                        &self.device,
                    );

                    // Restore enabled state
                    let mut l = new_layer;
                    l.enabled = download.enabled;
                    self.layer_stack.add(l);

                    self.gui_state.set_download_status(
                        &download.provider_id,
                        gui::DownloadStatus::Ready,
                    );
                    self.gui_state.layers_changed = true;
                }
                Err(e) => {
                    log::error!(
                        "Download failed for '{}': {}",
                        download.provider_id,
                        e
                    );
                    self.gui_state.set_download_status(
                        &download.provider_id,
                        gui::DownloadStatus::Error(e),
                    );
                }
            }
        }

        // =============================================================
        // M12: Poll live data sources
        // =============================================================
        let live_results = self.live_source_manager.poll();
        if !live_results.is_empty() {
            for result in live_results {
                self.marker_system.replace_layer(result.layer);
            }
            // Rebuild line + polygon buffers for new data
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

        // =============================================================
        // M13: Satellite tracking
        // =============================================================
        self.satellite_tracker.poll_downloads();
        self.gui_state.satellite_count = self.satellite_tracker.count();
        self.gui_state.satellite_downloading = self.satellite_tracker.is_downloading();
        let sat_utc = {
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
                        self.gui_state.selected_minute,
                        0,
                    )
                    .single()
                    .unwrap_or_else(chrono::Utc::now)
            };
            self.satellite_tracker.propagate(&utc);
            utc
        };

        // =============================================================
        // M9: Handle "Add Layer" requests from GUI
        // =============================================================
        if let Some(provider_id) = self.gui_state.add_provider_request.take() {
            if !self.layer_stack.has_provider(&provider_id) {
                let opacity = self
                    .catalog
                    .find(&provider_id)
                    .map(|p| p.info().default_opacity)
                    .unwrap_or(0.5);

                self.gui_state.set_download_status(
                    &provider_id,
                    gui::DownloadStatus::Downloading,
                );

                self.download_manager.start_download(
                    &self.catalog,
                    &provider_id,
                    None,
                    opacity,
                    true,
                );
            }
        }

        // =============================================================
        // M9: Handle "Remove Layer" requests from GUI
        // =============================================================
        if let Some(layer_id) = self.gui_state.remove_layer_request.take() {
            self.layer_stack.remove(&layer_id);
            // Clean up download status
            self.gui_state.download_status.retain(|e| e.provider_id != layer_id);
            self.gui_state.layers_changed = true;
        }

        // =============================================================
        // M9: Save layer configuration when changed
        // =============================================================
        if self.gui_state.layers_changed {
            self.gui_state.layers_changed = false;
            let layer_configs: Vec<(String, f32, bool)> = self
                .layer_stack
                .layers
                .iter()
                .map(|l| (l.provider_id.clone(), l.opacity, l.enabled))
                .collect();
            self.gui_state.settings.sync_layers(&layer_configs);
            self.gui_state.settings.save();
        }

        // =============================================================
        // M6: Date change → re-download all date-based layers
        // =============================================================
        if self.gui_state.date_changed {
            self.gui_state.date_changed = false;

            if let Some(date) = self.gui_state.selected_date() {
                let download_date = if self.gui_state.time_live {
                    date - chrono::Days::new(1)
                } else {
                    date
                };

                // Re-download all layers that support dates
                let providers_to_reload: Vec<(String, f32, bool)> = self
                    .layer_stack
                    .layers
                    .iter()
                    .filter(|l| l.provider_id != "builtin:grid")
                    .map(|l| (l.provider_id.clone(), l.opacity, l.enabled))
                    .collect();

                for (pid, opacity, enabled) in providers_to_reload {
                    self.gui_state.set_download_status(
                        &pid,
                        gui::DownloadStatus::Downloading,
                    );

                    self.download_manager.start_download(
                        &self.catalog,
                        &pid,
                        Some(download_date),
                        opacity,
                        enabled,
                    );

                    // Remove old layer (will be replaced when download completes)
                    self.layer_stack.layers.retain(|l| l.provider_id != pid);
                }
            }
        }

        // =============================================================
        // FOV + projection + mouse settings update
        // =============================================================
        if self.gui_state.settings_dirty {
            let old_fov = self.camera.fov_y;
            let new_fov = self.gui_state.settings.compute_fov(
                self.camera.distance,
                self.camera.distance_min,
                self.camera.distance_max,
            );

            if (new_fov - old_fov).abs() > 0.001 {
                let ratio = (old_fov / 2.0).tan() / (new_fov / 2.0).tan();
                self.camera.distance = (self.camera.distance * ratio)
                    .clamp(self.camera.distance_min, self.camera.distance_max);
            }

            // Sync tile cache config from settings
            self.tile_cache.update_config(tile::CacheConfig {
                cache_dir: app_path("cache/tiles"),
                max_size_bytes: (self.gui_state.settings.tile_cache_max_mb as u64) * 1024 * 1024,
                max_age: if self.gui_state.settings.tile_cache_max_days == 0 {
                    std::time::Duration::from_secs(365 * 24 * 3600)
                } else {
                    std::time::Duration::from_secs(
                        self.gui_state.settings.tile_cache_max_days as u64 * 24 * 3600,
                    )
                },
            });

            self.gui_state.settings_dirty = false;
            self.gui_state.settings.save();
        }

        // M16: Handle tile cache requests
        if self.gui_state.cache_clear_request {
            self.gui_state.cache_clear_request = false;
            self.tile_cache.clear();
            self.gui_state.cache_usage_mb = 0.0;
        }

        // Update cache usage display every ~600 frames (~10 seconds at 60fps)
        // Infrequent because total_size_bytes() does a recursive dir walk
        self.tile_cache_check_counter += 1;
        if self.tile_cache_check_counter >= 600 {
            self.tile_cache_check_counter = 0;
            self.gui_state.cache_usage_mb =
                self.tile_cache.total_size_bytes() as f32 / (1024.0 * 1024.0);
            self.tile_cache.evict_if_needed();
        }

        // =============================================================
        // M16d/e: Tile zoom — compute, download, composite, upload
        // =============================================================
        {
            let zoom = tile::zoom_from_distance(self.camera.distance);
            let source_id = self.gui_state.settings.tile_source.clone();

            // Reset compositor when zoom level OR tile source changes
            let source_changed = source_id != self.tile_prev_source;
            if zoom != self.tile_prev_zoom || source_changed {
                if source_changed {
                    log::info!("Tile source changed: {} -> {}", self.tile_prev_source, source_id);
                    self.tile_prev_source = source_id.clone();
                }
                log::info!("Tile reset: zoom={}, distance={:.2}", zoom, self.camera.distance);
                self.tile_compositor.reset(zoom);
                self.tile_prev_zoom = zoom;
                // Invalidate GPU texture so stale content isn't shown
                self.tile_overlay_texture = None;
                self.tile_overlay_bind_group = None;
            }

            // Only fetch tiles when zoomed in enough (zoom >= 3)
            // Below zoom 3, Blue Marble base texture has better detail
            if zoom >= 3 {
                // Compute visible area
                let (lat_n, lat_s, lon_w, lon_e) = tile::visible_bounds(
                    self.camera.yaw,
                    self.camera.pitch,
                    self.camera.distance,
                    self.camera.fov_y,
                    self.camera.aspect,
                );

                // Get needed tiles (sorted by proximity to center)
                let needed = tile::TileCoord::tiles_in_view(
                    lat_n, lat_s, lon_w, lon_e, zoom,
                );

                // Filter to tiles not yet composited
                let missing: Vec<tile::TileCoord> = needed.iter()
                    .filter(|t| !self.tile_compositor.has_tile(t))
                    .copied()
                    .collect();

                // Request downloads for missing tiles
                if !missing.is_empty() {
                    log::debug!("Tile request: zoom={}, needed={}, missing={}, source={}",
                        zoom, needed.len(), missing.len(), source_id);
                    self.tile_download_queue.request(
                        &source_id,
                        &missing,
                        None, // TODO: GIBS date support
                    );
                }
            }

            // Poll completed downloads and composite
            let ready_tiles = self.tile_download_queue.poll();
            if !ready_tiles.is_empty() {
                log::debug!("Tiles ready: {} downloaded, in_flight={}",
                    ready_tiles.len(), self.tile_download_queue.in_flight_count());
            }
            for tile_ready in &ready_tiles {
                self.tile_compositor.composite_tile(
                    &tile_ready.coord,
                    &tile_ready.data,
                );
            }

            // Dynamic tile overlay opacity: smooth crossfade with Blue Marble
            // Ramps from 0.0 (distance >= 4.0) to 1.0 (distance <= 2.5)
            // Tiles only become fully visible when close enough for good detail
            {
                let t = ((4.0 - self.camera.distance) / 1.5).clamp(0.0, 1.0);
                let smooth_t = t * t * (3.0 - 2.0 * t); // smoothstep
                let tile_opacity = if self.tile_compositor.has_content { smooth_t } else { 0.0 };
                let settings = OverlaySettings { opacity: tile_opacity, _pad: [0.0; 3] };
                self.queue.write_buffer(
                    &self.tile_overlay_settings_buffer,
                    0,
                    bytemuck::bytes_of(&settings),
                );
            }

            // Upload compositor buffer to GPU if dirty
            if self.tile_compositor.dirty && self.tile_compositor.has_content {
                let w = self.tile_compositor.width;
                let h = self.tile_compositor.height;
                let data = self.tile_compositor.buffer();

                // Create texture on first use
                if self.tile_overlay_texture.is_none() {
                    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
                    let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("Tile Overlay Texture"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
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
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&sampler),
                            },
                        ],
                    });
                    self.tile_overlay_texture = Some(tex);
                    self.tile_overlay_bind_group = Some(bind_group);
                }

                // Upload buffer to texture
                if let Some(tex) = &self.tile_overlay_texture {
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(4 * w),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    );
                }
                self.tile_compositor.mark_clean();
            }
        }

        self.camera.fov_y = self.gui_state.settings.compute_fov(
            self.camera.distance,
            self.camera.distance_min,
            self.camera.distance_max,
        );

        // Sync projection mode and mouse inversion from settings
        self.camera.projection = self.gui_state.settings.globe_projection;
        self.camera.invert_x = self.gui_state.settings.invert_mouse_x;
        self.camera.invert_y = self.gui_state.settings.invert_mouse_y;

        // M13c: Camera follow mode — smoothly track a satellite
        if let Some(norad_id) = self.gui_state.follow_satellite {
            if self.view_mode == ViewMode::Globe3D {
                if let Some(sat) = self.satellite_tracker.states().iter()
                    .find(|s| s.norad_id == norad_id)
                {
                    let lat_rad = (sat.latitude as f32).to_radians();
                    let lon_rad = (sat.longitude as f32).to_radians();

                    // Target yaw/pitch to center camera on satellite
                    // Orbis renders the far side of the globe (inside-out mesh),
                    // so eye must be OPPOSITE the world position: eye = -k * W.
                    // This gives: yaw = π/2 - lon, pitch = -lat.
                    let target_pitch = -lat_rad;
                    let mut target_yaw = std::f32::consts::FRAC_PI_2 - lon_rad;
                    // Normalize target to [-π, π]
                    while target_yaw > std::f32::consts::PI { target_yaw -= std::f32::consts::TAU; }
                    while target_yaw < -std::f32::consts::PI { target_yaw += std::f32::consts::TAU; }

                    // Smooth lerp (0.08 = responsive but not jumpy)
                    let t = 0.08_f32;

                    // Normalize camera yaw to [-π, π] before computing delta
                    while self.camera.yaw > std::f32::consts::PI {
                        self.camera.yaw -= std::f32::consts::TAU;
                    }
                    while self.camera.yaw < -std::f32::consts::PI {
                        self.camera.yaw += std::f32::consts::TAU;
                    }

                    // Handle yaw wrapping (shortest angular path)
                    let mut dy = target_yaw - self.camera.yaw;
                    if dy > std::f32::consts::PI { dy -= std::f32::consts::TAU; }
                    if dy < -std::f32::consts::PI { dy += std::f32::consts::TAU; }
                    self.camera.yaw += dy * t;
                    self.camera.pitch += (target_pitch - self.camera.pitch) * t;

                    // Distance is NOT forced — user keeps full zoom control
                }
            }
        }

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
                        if self.tile_compositor.has_content {
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
                        if self.tile_compositor.has_content {
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

        // =============================================================
        // M11e: Generate labels + sync GeoJSON layer info
        // =============================================================
        {
            let vp = match self.view_mode {
                ViewMode::Globe3D => self.camera.build_view_projection_matrix(),
                ViewMode::Map2D => self.camera.build_ortho_view_projection(
                    self.map_zoom,
                    self.map_pan,
                    QUAD_HALF_WIDTH,
                ),
            };
            let label_config = label::LabelConfig {
                view_proj: vp,
                eye_pos: glam::Vec3::new(
                    camera_uniform.eye_pos[0],
                    camera_uniform.eye_pos[1],
                    camera_uniform.eye_pos[2],
                ),
                // egui uses logical points, wgpu uses physical pixels
                screen_width: self.config.width as f32 / self.window.scale_factor() as f32,
                screen_height: self.config.height as f32 / self.window.scale_factor() as f32,
                is_map: self.view_mode == ViewMode::Map2D,
                quad_hw: QUAD_HALF_WIDTH,
            };
            self.gui_state.geo_labels =
                label::generate_labels(self.marker_system.geo_layers(), &label_config);

            // Sync layer info for GUI display
            self.gui_state.geo_layer_info = self.marker_system.geo_layers().iter().map(|l| {
                gui::GeoLayerInfo {
                    name: l.name.clone(),
                    visible: l.visible,
                    point_count: l.points().count(),
                    line_count: l.lines().count(),
                    polygon_count: l.polygons().count(),
                }
            }).collect();

            // Sync GeoJSON attributions for GUI footer
            self.gui_state.geo_attributions = self.marker_system.geo_layers().iter()
                .filter(|l| l.visible)
                .filter_map(|l| l.attribution.clone())
                .collect();

            // M13: Project satellite positions to screen space
            self.gui_state.satellite_markers.clear();
            let eye = glam::Vec3::new(
                camera_uniform.eye_pos[0],
                camera_uniform.eye_pos[1],
                camera_uniform.eye_pos[2],
            );
            let sw = label_config.screen_width;
            let sh = label_config.screen_height;
            for sat in self.satellite_tracker.states() {
                let lat_rad = (sat.latitude as f32).to_radians();
                let lon_rad = (sat.longitude as f32).to_radians();
                let scale = 1.0 + (sat.altitude_km as f32 / 6378.137) * 0.15;
                let world_pos = if label_config.is_map {
                    let u = (sat.longitude as f32 + 180.0) / 360.0;
                    let v = (90.0 - sat.latitude as f32) / 180.0;
                    let x = (u * 2.0 - 1.0) * QUAD_HALF_WIDTH;
                    let y = (1.0 - v * 2.0) * QUAD_HALF_WIDTH * 0.5;
                    glam::Vec3::new(x, y, 0.02)
                } else {
                    glam::Vec3::new(
                        -scale * lat_rad.cos() * lon_rad.cos(),
                         scale * lat_rad.sin(),
                        -scale * lat_rad.cos() * lon_rad.sin(),
                    )
                };

                // Occlusion (globe only)
                let vis = if label_config.is_map {
                    true
                } else {
                    let normal = world_pos.normalize();
                    let to_cam = (eye - world_pos).normalize();
                    normal.dot(to_cam) < 0.1
                };

                let clip = vp * glam::Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
                if clip.w <= 0.0 { continue; }
                let ndc_x = clip.x / clip.w;
                let ndc_y = clip.y / clip.w;
                if ndc_x.abs() > 1.1 || ndc_y.abs() > 1.1 { continue; }

                let sx = (ndc_x + 1.0) * 0.5 * sw;
                let sy = (1.0 - ndc_y) * 0.5 * sh;

                self.gui_state.satellite_markers.push(gui::SatelliteMarker {
                    x: sx,
                    y: sy,
                    name: sat.name.clone(),
                    norad_id: sat.norad_id,
                    altitude_km: sat.altitude_km,
                    velocity_km_s: sat.velocity_km_s,
                    visible: vis,
                });
            }

            // Ground tracks: compute + project for each satellite
            self.gui_state.satellite_tracks.clear();
            // Reusable buffers to avoid per-satellite per-frame allocations
            let mut past_screen: Vec<egui::Pos2> = Vec::with_capacity(64);
            let mut future_screen: Vec<egui::Pos2> = Vec::with_capacity(64);
            for sat in self.satellite_tracker.states() {
                let track_pts = self.satellite_tracker.compute_ground_track(
                    sat.norad_id,
                    &sat_utc,
                    90.0,   // 90 min past (~ 1 full orbit for LEO)
                    90.0,   // 90 min future
                    2.0,    // 2-minute steps
                );

                past_screen.clear();
                future_screen.clear();
                let mut prev_sx = f32::NAN;

                for pt in &track_pts {
                    let lat_r = (pt.latitude as f32).to_radians();
                    let lon_r = (pt.longitude as f32).to_radians();
                    let wp = if label_config.is_map {
                        let u = (pt.longitude as f32 + 180.0) / 360.0;
                        let v = (90.0 - pt.latitude as f32) / 180.0;
                        glam::Vec3::new(
                            (u * 2.0 - 1.0) * QUAD_HALF_WIDTH,
                            (1.0 - v * 2.0) * QUAD_HALF_WIDTH * 0.5,
                            0.015,
                        )
                    } else {
                        glam::Vec3::new(
                            -1.002 * lat_r.cos() * lon_r.cos(),
                             1.002 * lat_r.sin(),
                            -1.002 * lat_r.cos() * lon_r.sin(),
                        )
                    };

                    // Occlusion for globe mode
                    if !label_config.is_map {
                        let n = wp.normalize();
                        let tc = (eye - wp).normalize();
                        if n.dot(tc) > 0.05 { continue; }
                    }

                    let c = vp * glam::Vec4::new(wp.x, wp.y, wp.z, 1.0);
                    if c.w <= 0.0 { continue; }
                    let nx = c.x / c.w;
                    let ny = c.y / c.w;
                    if nx.abs() > 1.2 || ny.abs() > 1.2 { continue; }

                    let scr_x = (nx + 1.0) * 0.5 * sw;
                    let scr_y = (1.0 - ny) * 0.5 * sh;

                    // Break line on large horizontal jumps (date-line wrap)
                    if (scr_x - prev_sx).abs() > sw * 0.4 && !prev_sx.is_nan() {
                        // Start a new segment — push current and start fresh
                        if pt.minutes_offset <= 0.0 && past_screen.len() >= 2 {
                            // Could split, but simpler: just skip this point
                            prev_sx = scr_x;
                            continue;
                        }
                    }
                    prev_sx = scr_x;

                    let pos = egui::pos2(scr_x, scr_y);
                    if pt.minutes_offset <= 0.0 {
                        past_screen.push(pos);
                    } else {
                        future_screen.push(pos);
                    }
                }

                self.gui_state.satellite_tracks.push(gui::SatelliteTrack {
                    past: past_screen.clone(),
                    future: future_screen.clone(),
                });
            }

            // M14b: Project planet positions to screen space
            // Reuse sat_utc (already handles time_live vs. selected time)
            let (py, pm, pd, ph, pmin, ps) = {
                use chrono::{Datelike, Timelike};
                (sat_utc.year(), sat_utc.month(), sat_utc.day(),
                 sat_utc.hour(), sat_utc.minute(), sat_utc.second())
            };
            let planet_states = planets::compute_planet_positions(py, pm, pd, ph, pmin, ps);
            self.gui_state.planet_markers.clear();
            for planet in &planet_states {
                let pos = planet.to_sky_position();
                let world = glam::Vec3::new(pos[0], pos[1], pos[2]);

                let clip = vp * glam::Vec4::new(world.x, world.y, world.z, 1.0);
                if clip.w <= 0.0 { continue; }
                let nx = clip.x / clip.w;
                let ny = clip.y / clip.w;
                if nx.abs() > 1.1 || ny.abs() > 1.1 { continue; }

                let scr_x = (nx + 1.0) * 0.5 * sw;
                let scr_y = (1.0 - ny) * 0.5 * sh;

                self.gui_state.planet_markers.push(gui::PlanetMarker {
                    x: scr_x,
                    y: scr_y,
                    name: planet.name,
                    color: planet.color,
                    radius: planet.marker_radius(),
                    visible: true, // clip.w check above already handles off-screen
                });
            }
        }

        // =============================================================
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

        // Sync GUI → layer state (opacity/enabled changes)
        for gui_entry in &self.gui_state.layers {
            if let Some(layer) = self.layer_stack.layers.iter_mut().find(|l| l.id == gui_entry.id) {
                let enabled_changed = layer.enabled != gui_entry.enabled;
                let opacity_changed = (layer.opacity - gui_entry.opacity).abs() > 0.001;

                layer.enabled = gui_entry.enabled;

                if opacity_changed {
                    layer.opacity = gui_entry.opacity;
                    let settings = OverlaySettings {
                        opacity: gui_entry.opacity,
                        _pad: [0.0; 3],
                    };
                    self.queue.write_buffer(
                        &layer.settings_buffer,
                        0,
                        bytemuck::cast_slice(&[settings]),
                    );
                }

                if enabled_changed || opacity_changed {
                    self.gui_state.layers_changed = true;
                }
            }
        }

        // M11e: Handle GeoJSON requests from GUI
        self.handle_geojson_requests();

        // M12: Handle live source activate/deactivate requests
        self.handle_live_source_requests();

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    /// Handles GeoJSON file loading, layer toggling, and removal.
    fn handle_geojson_requests(&mut self) {
        let mut geo_changed = false;

        // File dialog request
        if self.gui_state.load_geojson_request {
            self.gui_state.load_geojson_request = false;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("GeoJSON", &["geojson", "json"])
                .pick_file()
            {
                self.load_geojson_file(&path);
                geo_changed = true;
            }
        }

        // Drag & drop
        let dropped: Vec<_> = self.gui_state.dropped_files.drain(..).collect();
        for path in dropped {
            self.load_geojson_file(&path);
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

    /// Loads a GeoJSON file and adds it as a layer.
    fn load_geojson_file(&mut self, path: &std::path::Path) {
        let name = path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());

        match geojson::load_geojson_file(path) {
            Ok(layer) => {
                let count = layer.len();
                log::info!("Loaded GeoJSON '{}': {} features", layer.name, count);
                self.marker_system.add_layer(layer);
            }
            Err(e) => {
                log::error!("Failed to load GeoJSON '{}': {}", name, e);
            }
        }
    }

    /// Handles activate/deactivate requests for live data sources.
    fn handle_live_source_requests(&mut self) {
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

// --- Application Handler ---
struct App {
    gpu: Option<GpuState>,
}

impl App {
    fn new() -> Self {
        Self { gpu: None }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        // Load window icon from assets/icon/icon.png (resized to 64x64)
        let window_icon = {
            let icon_path = crate::app_path("assets/icon/icon.png");
            match image::open(&icon_path) {
                Ok(img) => {
                    let resized = img.resize_exact(64, 64, image::imageops::FilterType::Lanczos3);
                    let rgba = resized.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    winit::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
                }
                Err(e) => {
                    log::warn!("Could not load window icon: {}", e);
                    None
                }
            }
        };

        let mut window_attrs = WindowAttributes::default()
            .with_title("Orbis — Real-Time Earth Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .with_maximized(true)
            .with_visible(false);
        if let Some(icon) = window_icon {
            window_attrs = window_attrs.with_window_icon(Some(icon));
        }

        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("Failed to create window!"),
        );

        self.gpu = Some(pollster::block_on(GpuState::new(window)));

        if let Some(gpu) = &self.gpu {
            gpu.window.set_visible(true);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => return,
        };

        let egui_consumed = gpu.gui.handle_event(&gpu.window, &event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        physical_key: PhysicalKey::Code(key_code),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } if !egui_consumed => match key_code {
                KeyCode::Escape => event_loop.exit(),

                KeyCode::KeyL => {
                    gpu.gui_state.panel_open = !gpu.gui_state.panel_open;
                }

                KeyCode::KeyR => {
                    match gpu.view_mode {
                        ViewMode::Globe3D => gpu.camera.reset(),
                        ViewMode::Map2D => {
                            gpu.map_zoom = 1.0;
                            gpu.map_pan = (0.0, 0.0);
                        }
                    }
                    gpu.window.request_redraw();
                }

                KeyCode::KeyT => {
                    gpu.gui_state.time_live = !gpu.gui_state.time_live;
                    if gpu.gui_state.time_live {
                        let now = chrono::Utc::now();
                        gpu.gui_state.selected_year = chrono::Datelike::year(&now);
                        gpu.gui_state.selected_month = chrono::Datelike::month(&now);
                        gpu.gui_state.selected_day = chrono::Datelike::day(&now);
                        gpu.gui_state.selected_hour = chrono::Timelike::hour(&now);
                        gpu.gui_state.selected_minute = chrono::Timelike::minute(&now);
                        gpu.gui_state.date_changed = true;
                    }
                }

                KeyCode::KeyG => {
                    gpu.gui_state.labels_visible = !gpu.gui_state.labels_visible;
                }

                KeyCode::KeyK => {
                    gpu.gui_state.legend_open = !gpu.gui_state.legend_open;
                }

                KeyCode::KeyM => {
                    gpu.view_mode = match gpu.view_mode {
                        ViewMode::Globe3D => {
                            log::info!("Switching to 2D map view");
                            gpu.map_zoom = 1.0;
                            gpu.map_pan = (0.0, 0.0);
                            ViewMode::Map2D
                        }
                        ViewMode::Map2D => {
                            log::info!("Switching to 3D globe");
                            ViewMode::Globe3D
                        }
                    };
                    gpu.gui_state.view_mode_map = gpu.view_mode == ViewMode::Map2D;
                    gpu.window.request_redraw();
                }

                _ => {}
            },

            WindowEvent::Resized(physical_size) => {
                gpu.resize(physical_size.width, physical_size.height);
            }

            WindowEvent::DroppedFile(path) => {
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "geojson" || ext == "json" {
                    log::info!("File dropped: {:?}", path);
                    gpu.gui_state.dropped_files.push(path);
                }
            }

            WindowEvent::MouseInput { state, button, .. } if !egui_consumed => {
                if button == winit::event::MouseButton::Left {
                    gpu.mouse_pressed = state == winit::event::ElementState::Pressed;
                    if !gpu.mouse_pressed {
                        gpu.last_mouse_pos = None;
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } if !egui_consumed => {
                if gpu.mouse_pressed {
                    if let Some((last_x, last_y)) = gpu.last_mouse_pos {
                        let dx = position.x - last_x;
                        let dy = position.y - last_y;

                        match gpu.view_mode {
                            ViewMode::Globe3D => {
                                gpu.camera.orbit(dx as f32, dy as f32);
                                // Break satellite follow on manual orbit
                                if gpu.gui_state.follow_satellite.is_some() {
                                    gpu.gui_state.follow_satellite = None;
                                }
                            }
                            ViewMode::Map2D => {
                                let visible_half_h = QUAD_HALF_WIDTH / (2.0 * gpu.map_zoom);
                                let visible_half_w = visible_half_h * gpu.camera.aspect;
                                let window_w = gpu.config.width as f32;
                                let window_h = gpu.config.height as f32;

                                gpu.map_pan.0 -= (dx as f32 / window_w) * visible_half_w * 2.0;
                                gpu.map_pan.1 += (dy as f32 / window_h) * visible_half_h * 2.0;
                            }
                        }
                        gpu.window.request_redraw();
                    }
                    gpu.last_mouse_pos = Some((position.x, position.y));
                }
            }

            WindowEvent::MouseWheel { delta, .. } if !egui_consumed => {
                let scroll = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0,
                };

                match gpu.view_mode {
                    ViewMode::Globe3D => {
                        gpu.camera.zoom(scroll);
                    }
                    ViewMode::Map2D => {
                        let zoom_speed = 0.1;
                        gpu.map_zoom *= 1.0 + scroll * zoom_speed;
                        gpu.map_zoom = gpu.map_zoom.clamp(0.5, 20.0);
                    }
                }
                gpu.window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                match gpu.render() {
                    Ok(_) => {}
                    Err(wgpu::SurfaceError::Lost) => {
                        gpu.resize(gpu.config.width, gpu.config.height);
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                    Err(e) => log::warn!("Render error: {:?}", e),
                }
                // Frame rate capped by VSync (PresentMode::Fifo)
                gpu.window.request_redraw();
            }

            _ => {}
        }
    }
}

fn main() {
    env_logger::init();
    // Load settings early to get language preference
    let settings = settings::Settings::load();
    i18n::init(settings.language.as_deref());
    let event_loop = EventLoop::new().expect("Failed to create event loop!");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("Event loop crashed!");
}
