// =============================================================================
// Orbis — GPU State Initialization
// =============================================================================
// Creates all GPU resources: device, pipelines, textures, buffers.
// Separated from main.rs for readability.
// =============================================================================

use std::sync::Arc;
use winit::window::Window;
use wgpu::util::DeviceExt;

use crate::{
    app_path, QUAD_HALF_WIDTH, OverlaySettings, ViewMode,
    camera::Camera,
    gui, i18n, layer, mesh, provider, settings, sun, tile,
    download, satellite, live_source, marker, line, polygon, geojson, custom_source,
};
use crate::mesh::Vertex;
use crate::texture::GpuTexture;
use crate::layer::LayerStack;

impl crate::GpuState {
    pub(crate) async fn new(window: Arc<Window>) -> Self {
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
        let mut download_manager = download::DownloadManager::new();

        // Load settings and restore active layers
        let mut loaded_settings = settings::Settings::load();
        // Guard against corrupted/hand-edited settings.json: replace unknown
        // tile_source IDs with a valid fallback so the tile subsystem never
        // starts in an unresolvable state.
        loaded_settings.sanitize_tile_source(&tile::builtin_source_ids());
        let initial_tile_source = loaded_settings.tile_source.clone();

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

        // M17h: Initialize Shapefile source manager — load any enabled
        // .shp custom sources synchronously into marker_system before the
        // first frame.
        let mut shapefile_source_manager = custom_source::ShapefileSourceManager::new();
        let shp_sync = shapefile_source_manager.sync_config(&gui_state.custom_sources_config);
        for layer in shp_sync.added {
            marker_system.add_layer(layer);
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

        // M16: Create the tile manager (owns cache, worker pool, compositor).
        let tile_cache_max_age = if cache_max_days == 0 {
            None // 0 = no age limit
        } else {
            Some(std::time::Duration::from_secs(cache_max_days as u64 * 24 * 3600))
        };
        let tile_manager = tile::TileManager::new(
            app_path("cache/tiles"),
            cache_max_mb,
            tile_cache_max_age,
            initial_tile_source.clone(),
        );

        // M17d: Initialize REST feed manager from custom source config
        let mut rest_feed_manager = custom_source::RestFeedManager::new();
        rest_feed_manager.sync_config(&gui_state.custom_sources_config);
        // (Shapefile manager initialized earlier so its layers feed the
        // marker_system buffer rebuild that already ran above.)

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
            rest_feed_manager,
            shapefile_source_manager,
            satellite_tracker: {
                let mut tracker = satellite::SatelliteTracker::new();
                tracker.request_refresh(); // start downloading OMMs at startup
                tracker
            },
            tile_manager,
            tile_overlay_texture: None,
            tile_overlay_bind_group: None,
            tile_overlay_settings_bind_group,
            tile_overlay_settings_buffer,
            mouse_pressed: false,
            last_mouse_pos: None,
        }
    }


}
