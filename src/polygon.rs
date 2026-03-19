// =============================================================================
// Orbis — Polygon Rendering System (M11d)
// =============================================================================
// Renders GeoJSON Polygon features as triangulated, semi-transparent fills
// on the globe (3D) and map (2D).
//
// Architecture:
// - Polygons are triangulated CPU-side using the earcut algorithm
// - Vertices carry lon/lat + fill color
// - Index buffer drives the GPU draw call
// - Polygon outlines are generated as line segments for the LineSystem
// - Buffer is rebuilt when layers change
// =============================================================================

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::geojson::{self, GeoCoord, GeoGeometry, GeoLayer};

// =============================================================================
// GPU Vertex Data
// =============================================================================

/// Per-vertex data for polygon rendering.
///
/// Memory layout (24 bytes):
///   lon_lat:  0..8   (vec2<f32>)
///   color:    8..24  (vec4<f32>)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct PolygonVertex {
    pub lon_lat: [f32; 2],
    pub color: [f32; 4],
}

impl PolygonVertex {
    pub fn buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PolygonVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
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
            ],
        }
    }
}

// =============================================================================
// Outline Segment (for feeding into LineSystem)
// =============================================================================

/// A line segment representing part of a polygon outline.
/// Used to generate LineSystem segments for polygon boundaries.
pub struct OutlineSegment {
    pub start: GeoCoord,
    pub end: GeoCoord,
    pub color: [f32; 4],
    pub width: f32,
}

// =============================================================================
// Polygon System
// =============================================================================

pub struct PolygonSystem {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    index_count: u32,
    dirty: bool,
    /// Outline segments generated during last rebuild,
    /// to be consumed by the LineSystem.
    outline_segments: Vec<OutlineSegment>,
}

impl PolygonSystem {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        camera_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Polygon Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../assets/shaders/polygon.wgsl").into(),
            ),
        });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Polygon Pipeline Layout"),
                bind_group_layouts: &[camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Polygon Render Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[PolygonVertex::buffer_layout()],
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
                    cull_mode: None, // Polygons visible from both sides
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
            vertex_buffer: None,
            index_buffer: None,
            index_count: 0,
            dirty: false,
            outline_segments: Vec::new(),
        }
    }

    /// Rebuilds the GPU buffers from all visible polygon features.
    ///
    /// Also generates outline segments for the LineSystem.
    pub fn rebuild_from_layers(
        &mut self,
        geo_layers: &[GeoLayer],
        device: &wgpu::Device,
    ) {
        let mut vertices: Vec<PolygonVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        self.outline_segments.clear();

        for layer in geo_layers.iter().filter(|l| l.visible) {
            for feature in layer.polygons() {
                if let GeoGeometry::Polygon(rings) = &feature.geometry {
                    let color = feature.style.color;
                    let stroke_color = feature.style.stroke_color;
                    let stroke_width = feature.style.line_width;

                    // Subdivide and close rings for globe curvature
                    let subdiv_rings: Vec<Vec<GeoCoord>> = rings.iter()
                        .map(|ring| {
                            let mut r = subdivide_ring(ring, SUBDIVISION_ANGLE);
                            ensure_closed(&mut r);
                            r
                        })
                        .collect();

                    // Triangulate the polygon (outer ring + holes)
                    if let Some(tri_indices) = triangulate_polygon(&subdiv_rings) {
                        let base_vertex = vertices.len() as u32;

                        // Add all vertices from all rings
                        for ring in &subdiv_rings {
                            for coord in ring {
                                vertices.push(PolygonVertex {
                                    lon_lat: [coord.lon as f32, coord.lat as f32],
                                    color,
                                });
                            }
                        }

                        // Add triangle indices (offset by base vertex)
                        for idx in tri_indices {
                            indices.push(base_vertex + idx as u32);
                        }
                    }

                    // Generate outline segments for each ring
                    for ring in &subdiv_rings {
                        for pair in ring.windows(2) {
                            self.outline_segments.push(OutlineSegment {
                                start: pair[0],
                                end: pair[1],
                                color: stroke_color,
                                width: stroke_width,
                            });
                        }
                    }
                }
            }
        }

        self.index_count = indices.len() as u32;

        if vertices.is_empty() || indices.is_empty() {
            self.vertex_buffer = None;
            self.index_buffer = None;
        } else {
            self.vertex_buffer =
                Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Polygon Vertex Buffer"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }));

            self.index_buffer =
                Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Polygon Index Buffer"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                }));

            log::info!(
                "PolygonSystem: rebuilt buffers ({} vertices, {} indices, {} outline segments)",
                vertices.len(),
                self.index_count,
                self.outline_segments.len(),
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

    /// Returns the outline segments generated during the last rebuild.
    /// These should be consumed by the LineSystem.
    pub fn outline_segments(&self) -> &[OutlineSegment] {
        &self.outline_segments
    }

    /// Renders all polygon fills in the current render pass.
    ///
    /// Should be rendered BEFORE lines and markers (polygons are the base).
    pub fn render<'a>(
        &'a self,
        render_pass: &mut wgpu::RenderPass<'a>,
        camera_bind_group: &'a wgpu::BindGroup,
    ) {
        if self.index_count == 0 {
            return;
        }

        if let (Some(vb), Some(ib)) = (&self.vertex_buffer, &self.index_buffer) {
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, vb.slice(..));
            render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
    }
}

// =============================================================================
// Ring Processing
// =============================================================================

/// Maximum arc angle (degrees) before a polygon edge gets subdivided.
/// Matches the LineSystem constant for visual consistency.
const SUBDIVISION_ANGLE: f64 = 2.0;

/// Subdivides a polygon ring along great-circle arcs.
///
/// Each edge longer than `max_angle_deg` gets interpolated with intermediate
/// points so the polygon hugs the globe's curvature.
fn subdivide_ring(ring: &[GeoCoord], max_angle_deg: f64) -> Vec<GeoCoord> {
    if ring.len() < 2 {
        return ring.to_vec();
    }

    let mut result = Vec::with_capacity(ring.len() * 2);

    for pair in ring.windows(2) {
        let sub = geojson::great_circle_subdivide(&pair[0], &pair[1], max_angle_deg);
        // Add all points except the last (it will be the start of the next edge)
        if sub.len() > 1 {
            result.extend_from_slice(&sub[..sub.len() - 1]);
        } else {
            result.push(pair[0]);
        }
    }

    // Add the final point
    if let Some(last) = ring.last() {
        result.push(*last);
    }

    result
}

/// Ensures a ring is closed (first == last coordinate).
///
/// Many GeoJSON files omit the closing vertex. This adds it if missing.
fn ensure_closed(ring: &mut Vec<GeoCoord>) {
    if ring.len() >= 2 {
        let first = ring[0];
        let last = ring[ring.len() - 1];
        let dlat = (first.lat - last.lat).abs();
        let dlon = (first.lon - last.lon).abs();
        if dlat > 1e-9 || dlon > 1e-9 {
            ring.push(first);
        }
    }
}

// =============================================================================
// Triangulation
// =============================================================================

/// Triangulates a polygon (outer ring + optional holes) using the earcut algorithm.
///
/// Returns triangle indices into the flattened vertex list (all rings concatenated).
/// Returns None if the polygon cannot be triangulated.
fn triangulate_polygon(rings: &[Vec<GeoCoord>]) -> Option<Vec<usize>> {
    if rings.is_empty() || rings[0].len() < 3 {
        return None;
    }

    // Flatten all coordinates into a single [x, y, x, y, ...] array
    let mut coords: Vec<f64> = Vec::new();
    let mut hole_indices: Vec<usize> = Vec::new();

    // Outer ring
    for coord in &rings[0] {
        coords.push(coord.lon);
        coords.push(coord.lat);
    }

    // Hole rings
    for ring in rings.iter().skip(1) {
        hole_indices.push(coords.len() / 2);
        for coord in ring {
            coords.push(coord.lon);
            coords.push(coord.lat);
        }
    }

    match earcutr::earcut(&coords, &hole_indices, 2) {
        Ok(result) if !result.is_empty() => Some(result),
        Ok(_) => {
            log::warn!("Polygon triangulation produced 0 triangles");
            None
        }
        Err(e) => {
            log::warn!("Polygon triangulation failed: {:?}", e);
            None
        }
    }
}
