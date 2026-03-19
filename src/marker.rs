// =============================================================================
// Orbis — Marker Rendering System (M11b)
// =============================================================================
// Renders GeoJSON point features as instanced billboard circles on the
// globe (3D) and map (2D). Uses the same instancing technique as stars:
// 6 vertices per quad generated in the vertex shader, one instance per marker.
//
// Architecture:
// - GeoLayers are stored and can be added/removed at runtime
// - All point features across all visible layers are flattened into a
//   single GPU instance buffer for efficient batched rendering
// - Buffer is rebuilt when layers change (add/remove/toggle visibility)
// =============================================================================

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::geojson::{GeoGeometry, GeoLayer};

// =============================================================================
// GPU Instance Data
// =============================================================================

/// Per-marker instance data sent to the GPU.
///
/// Memory layout (32 bytes per instance):
///   lon_lat:   0..8    (vec2<f32>)
///   color:     8..24   (vec4<f32>)
///   size:     24..28   (f32)
///   _pad:     28..32   (f32, alignment padding)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct MarkerInstance {
    pub lon_lat: [f32; 2],
    pub color: [f32; 4],
    pub size: f32,
    pub _pad: f32,
}

impl MarkerInstance {
    /// Vertex buffer layout for instanced rendering.
    pub fn buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                // lon_lat: @location(0)
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // color: @location(1)
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                // size: @location(2)
                wgpu::VertexAttribute {
                    offset: 24,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32,
                },
            ],
        }
    }
}

// =============================================================================
// Marker System
// =============================================================================

/// Manages GeoJSON point rendering.
///
/// Holds the GPU pipeline, instance buffer, and a collection of GeoLayers.
/// Call `add_layer()` / `remove_layer()` to manage data, then `render()`
/// each frame. The instance buffer is rebuilt automatically when layers change.
pub struct MarkerSystem {
    pipeline: wgpu::RenderPipeline,
    instance_buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    geo_layers: Vec<GeoLayer>,
    dirty: bool, // Instance buffer needs rebuild
}

impl MarkerSystem {
    /// Creates the marker rendering system.
    ///
    /// Sets up the render pipeline (shader, blending, vertex layout).
    /// No instance buffer is allocated until layers are added.
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        camera_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Marker Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/marker.wgsl").into(),
            ),
        });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Marker Pipeline Layout"),
                bind_group_layouts: &[camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Marker Render Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[MarkerInstance::buffer_layout()],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
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
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        Self {
            pipeline,
            instance_buffer: None,
            instance_count: 0,
            geo_layers: Vec::new(),
            dirty: false,
        }
    }

    /// Adds a GeoJSON layer. Only point features will be rendered.
    pub fn add_layer(&mut self, layer: GeoLayer) {
        let point_count = layer.points().count();
        log::info!(
            "MarkerSystem: added layer '{}' ({} points)",
            layer.name,
            point_count,
        );
        self.geo_layers.push(layer);
        self.dirty = true;
    }

    /// Replaces an existing layer by name, or adds it if not found.
    ///
    /// Used by live data sources to update their layer on refresh.
    pub fn replace_layer(&mut self, layer: GeoLayer) {
        if let Some(existing) = self.geo_layers.iter_mut().find(|l| l.name == layer.name) {
            let vis = existing.visible; // Preserve user toggle
            *existing = layer;
            existing.visible = vis;
        } else {
            self.geo_layers.push(layer);
        }
        self.dirty = true;
    }

    /// Removes a layer by name. Returns true if found.
    pub fn remove_layer(&mut self, name: &str) -> bool {
        let before = self.geo_layers.len();
        self.geo_layers.retain(|l| l.name != name);
        let removed = self.geo_layers.len() < before;
        if removed {
            log::info!("MarkerSystem: removed layer '{}'", name);
            self.dirty = true;
        }
        removed
    }

    /// Toggles visibility of a layer by name.
    pub fn toggle_layer(&mut self, name: &str) {
        if let Some(layer) = self.geo_layers.iter_mut().find(|l| l.name == name) {
            layer.visible = !layer.visible;
            self.dirty = true;
        }
    }

    /// Returns the names of all loaded GeoJSON layers.
    pub fn layer_names(&self) -> Vec<(&str, bool, usize)> {
        self.geo_layers
            .iter()
            .map(|l| (l.name.as_str(), l.visible, l.points().count()))
            .collect()
    }

    /// Ensures the GPU instance buffer is up-to-date.
    ///
    /// Must be called BEFORE the render pass begins, because it needs
    /// mutable access to self (which conflicts with render pass borrows).
    pub fn ensure_buffer(&mut self, device: &wgpu::Device) {
        if self.dirty {
            self.rebuild_instances(device);
        }
    }

    /// Rebuilds the GPU instance buffer from all visible point features.
    fn rebuild_instances(&mut self, device: &wgpu::Device) {
        let instances: Vec<MarkerInstance> = self
            .geo_layers
            .iter()
            .filter(|l| l.visible)
            .flat_map(|l| l.points())
            .filter_map(|feature| {
                if let GeoGeometry::Point(coord) = &feature.geometry {
                    Some(MarkerInstance {
                        lon_lat: [coord.lon as f32, coord.lat as f32],
                        color: feature.style.color,
                        size: feature.style.marker_size,
                        _pad: 0.0,
                    })
                } else {
                    None
                }
            })
            .collect();

        self.instance_count = instances.len() as u32;

        if instances.is_empty() {
            self.instance_buffer = None;
        } else {
            self.instance_buffer =
                Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Marker Instance Buffer"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX,
                }));

            log::info!(
                "MarkerSystem: rebuilt instance buffer ({} markers)",
                self.instance_count,
            );
        }

        self.dirty = false;
    }

    /// Renders all visible markers in the current render pass.
    ///
    /// Must be called AFTER the base geometry (globe/map + overlay layers),
    /// so markers appear on top.
    ///
    /// IMPORTANT: Call `ensure_buffer()` before beginning the render pass!
    /// This method takes `&self` (not `&mut`) to avoid borrow conflicts
    /// with the render pass holding references to other GpuState fields.
    pub fn render<'a>(
        &'a self,
        render_pass: &mut wgpu::RenderPass<'a>,
        camera_bind_group: &'a wgpu::BindGroup,
    ) {
        if self.instance_count == 0 {
            return;
        }

        if let Some(buffer) = &self.instance_buffer {
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(0..6, 0..self.instance_count);
        }
    }

    /// Total number of points across all visible layers.
    pub fn visible_point_count(&self) -> usize {
        self.geo_layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.points().count())
            .sum()
    }

    /// Whether any GeoJSON layers are loaded.
    pub fn has_layers(&self) -> bool {
        !self.geo_layers.is_empty()
    }

    /// Read-only access to the GeoJSON layers.
    /// Used by LineSystem (and later PolygonSystem) to rebuild their buffers.
    pub fn geo_layers(&self) -> &[GeoLayer] {
        &self.geo_layers
    }
}
