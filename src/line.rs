// =============================================================================
// Orbis — Line Rendering System (M11c)
// =============================================================================
// Renders GeoJSON LineString features as screen-space widened segments
// on the globe (3D) and map (2D).
//
// Architecture:
// - LineStrings are subdivided along great circles for correct curvature
// - Each sub-segment becomes one GPU instance (rendered as a quad)
// - Screen-space widening gives constant visual thickness
// - Buffer is rebuilt when layers change
// =============================================================================

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::geojson::{GeoGeometry, GeoLayer, subdivide_linestring, great_circle_subdivide};
use crate::polygon::OutlineSegment;

// =============================================================================
// GPU Instance Data
// =============================================================================

/// Per-segment instance data sent to the GPU.
///
/// Memory layout (48 bytes per instance):
///   start_lon_lat:   0..8    (vec2<f32>)
///   end_lon_lat:     8..16   (vec2<f32>)
///   color:          16..32   (vec4<f32>)
///   width:          32..36   (f32)
///   _pad:           36..48   (3 × f32, alignment to 16-byte boundary)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct LineSegment {
    pub start_lon_lat: [f32; 2],
    pub end_lon_lat: [f32; 2],
    pub color: [f32; 4],
    pub width: f32,
    pub _pad: [f32; 3],
}

impl LineSegment {
    pub fn buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineSegment>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                // start_lon_lat: @location(0)
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // end_lon_lat: @location(1)
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // color: @location(2)
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                // width: @location(3)
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32,
                },
            ],
        }
    }
}

// =============================================================================
// Line System
// =============================================================================

/// Maximum angle (degrees) between great-circle subdivision points.
/// Smaller = smoother curves, more GPU segments.
/// 2° gives ~180 segments for a half-globe arc — good balance.
const SUBDIVISION_ANGLE: f64 = 2.0;

/// Manages GeoJSON line rendering.
pub struct LineSystem {
    pipeline: wgpu::RenderPipeline,
    instance_buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    dirty: bool,
}

impl LineSystem {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        camera_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Line Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/line.wgsl").into(),
            ),
        });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Line Pipeline Layout"),
                bind_group_layouts: &[camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Line Render Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[LineSegment::buffer_layout()],
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
            dirty: false,
        }
    }

    /// Rebuilds the GPU segment buffer from all visible line features
    /// and polygon outlines.
    ///
    /// Lines are subdivided along great circles for correct globe curvature.
    pub fn rebuild_from_layers(
        &mut self,
        geo_layers: &[GeoLayer],
        outline_segments: &[OutlineSegment],
        device: &wgpu::Device,
    ) {
        let mut segments: Vec<LineSegment> = Vec::new();

        // LineString features
        for layer in geo_layers.iter().filter(|l| l.visible) {
            for feature in layer.lines() {
                if let GeoGeometry::LineString(coords) = &feature.geometry {
                    let subdivided = subdivide_linestring(coords, SUBDIVISION_ANGLE);
                    for pair in subdivided.windows(2) {
                        segments.push(LineSegment {
                            start_lon_lat: [pair[0].lon as f32, pair[0].lat as f32],
                            end_lon_lat: [pair[1].lon as f32, pair[1].lat as f32],
                            color: feature.style.stroke_color,
                            width: feature.style.line_width,
                            _pad: [0.0; 3],
                        });
                    }
                }
            }
        }

        // Polygon outline segments (subdivided along great circles)
        for outline in outline_segments {
            let sub = great_circle_subdivide(&outline.start, &outline.end, SUBDIVISION_ANGLE);
            for pair in sub.windows(2) {
                segments.push(LineSegment {
                    start_lon_lat: [pair[0].lon as f32, pair[0].lat as f32],
                    end_lon_lat: [pair[1].lon as f32, pair[1].lat as f32],
                    color: outline.color,
                    width: outline.width,
                    _pad: [0.0; 3],
                });
            }
        }

        self.instance_count = segments.len() as u32;

        if segments.is_empty() {
            self.instance_buffer = None;
        } else {
            self.instance_buffer =
                Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Line Segment Buffer"),
                    contents: bytemuck::cast_slice(&segments),
                    usage: wgpu::BufferUsages::VERTEX,
                }));

            log::info!(
                "LineSystem: rebuilt buffer ({} segments)",
                self.instance_count,
            );
        }

        self.dirty = false;
    }

    /// Marks the buffer as needing a rebuild.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Returns whether the buffer needs rebuilding.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Renders all line segments in the current render pass.
    ///
    /// Must be called AFTER base geometry and overlays, BEFORE markers
    /// (so markers appear on top of lines).
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
}
