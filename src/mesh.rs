// =============================================================================
// Orbis — Mesh Generation
// =============================================================================
// Vertex definition and functions for generating 3D geometry.
// Currently: UV sphere for the Earth globe.
// =============================================================================

use bytemuck::{Pod, Zeroable};
use rand::Rng;

/// A single vertex (corner point) in 3D space.
///
/// Each vertex has three attributes:
/// - `position`: Where it lies in 3D space (x, y, z)
/// - `normal`: Which direction the surface faces at this point
///   (used for lighting calculations)
/// - `uv`: Where on the texture this point maps to (u = horizontal, v = vertical)
///
/// `#[repr(C)]` + `Pod` + `Zeroable`: Guarantees a fixed memory layout
/// that the GPU can read directly. Without this, Rust might reorder fields.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl Vertex {
    /// Describes for wgpu how vertex data is arranged in the buffer.
    /// This is the bridge between the Rust struct and the WGSL shader.
    ///
    /// Total size per vertex: 3+3+2 = 8 floats × 4 bytes = 32 bytes.
    pub fn buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                // position: @location(0) in shader
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // normal: @location(1) in shader
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // uv: @location(2) in shader
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress, // 24
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }
    }
}

/// Generates a UV sphere.
///
/// A UV sphere is created by dividing the sphere into "rings" (stacks, like
/// latitudes) and "segments" (sectors, like longitudes). Each quad in the
/// grid is split into two triangles.
///
/// Parameters:
/// - `sectors`: Number of longitude subdivisions (more = smoother)
/// - `stacks`: Number of latitude subdivisions
/// - `radius`: Sphere radius
///
/// Returns:
/// - `Vec<Vertex>`: All vertices of the sphere
/// - `Vec<u32>`: Indices indicating which vertices form triangles
///
/// Typical values: sectors=64, stacks=32 → ~4000 triangles, looks perfectly
/// smooth and is trivial for the GPU.
pub fn generate_sphere(sectors: u32, stacks: u32, radius: f32) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let sector_step = 2.0 * std::f32::consts::PI / sectors as f32;
    let stack_step = std::f32::consts::PI / stacks as f32;

    // --- Generate vertices ---
    // We iterate from north pole (theta=0) to south pole (theta=PI).
    // For each latitude, we iterate once around the sphere (phi=0..2PI).
    for i in 0..=stacks {
        // Theta: angle from north pole. 0 = north pole, PI = south pole.
        let theta = i as f32 * stack_step;
        let sin_theta = theta.sin();
        let cos_theta = theta.cos();

        for j in 0..=sectors {
            // Phi: angle around the sphere. 0 = start, 2*PI = full circle.
            let phi = j as f32 * sector_step;
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();

            // Spherical → Cartesian coordinates
            // Y is up (standard in 3D graphics)
            let x = radius * sin_theta * cos_phi;
            let y = radius * cos_theta;
            let z = radius * sin_theta * sin_phi;

            // Normal = normalized position (for a sphere at the origin,
            // the normal vector always points outward from the center,
            // i.e. exactly in the direction of the position)
            let nx = sin_theta * cos_phi;
            let ny = cos_theta;
            let nz = sin_theta * sin_phi;

            // UV coordinates: u = longitude (0..1), v = latitude (0..1)
            // This matches the equirectangular projection of Blue Marble perfectly.
            let u = j as f32 / sectors as f32;
            let v = i as f32 / stacks as f32;

            vertices.push(Vertex {
                position: [x, y, z],
                normal: [nx, ny, nz],
                uv: [u, v],
            });
        }
    }

    // --- Generate indices ---
    // Each quad in the grid is split into two triangles.
    // At the poles, quads degenerate to triangles (one edge has length 0),
    // so we skip one triangle there.
    for i in 0..stacks {
        for j in 0..sectors {
            // The four corners of the current quad:
            //   first ---- first+1
            //     |    \     |
            //   second -- second+1
            let first = i * (sectors + 1) + j;
            let second = first + sectors + 1;

            // Upper triangle (not at north pole, where it degenerates)
            if i != 0 {
                indices.push(first);
                indices.push(second);
                indices.push(first + 1);
            }

            // Lower triangle (not at south pole, where it degenerates)
            if i != stacks - 1 {
                indices.push(first + 1);
                indices.push(second);
                indices.push(second + 1);
            }
        }
    }

    log::info!(
        "Sphere generated: {} vertices, {} indices ({} triangles)",
        vertices.len(),
        indices.len(),
        indices.len() / 3
    );

    (vertices, indices)
}

/// Generates a flat rectangle (quad) for the 2D map projection.
///
/// The quad has a 2:1 aspect ratio (like an equirectangular projection:
/// 360° width × 180° height). It lies in the XY plane, centered at origin.
///
/// UV coordinates are identical to the sphere: u=0..1 (left→right = 180°W→180°E),
/// v=0..1 (top→bottom = north pole→south pole). This means all textures
/// (Blue Marble, GIBS overlays) work without modification.
///
/// Normals all point in +Z (toward camera), but are not used for lighting
/// in the 2D shader (M7b reconstructs sphere normals from UV).
///
/// Parameters:
/// - `half_width`: Half-width of the quad (height = half_width, due to 2:1)
///
/// Returns:
/// - `Vec<Vertex>`: 4 corner vertices
/// - `Vec<u32>`: 6 indices (2 triangles)
pub fn generate_quad(half_width: f32) -> (Vec<Vertex>, Vec<u32>) {
    let half_height = half_width / 2.0; // 2:1 aspect ratio

    //  TL (0) ---- TR (1)
    //   |    \       |
    //   |     \      |
    //  BL (2) ---- BR (3)
    //
    // UV: (0,0) = top-left = northwest (180°W, 90°N)
    //     (1,1) = bottom-right = southeast (180°E, 90°S)
    let vertices = vec![
        // Top-left
        Vertex {
            position: [-half_width, half_height, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
        },
        // Top-right
        Vertex {
            position: [half_width, half_height, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [1.0, 0.0],
        },
        // Bottom-left
        Vertex {
            position: [-half_width, -half_height, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 1.0],
        },
        // Bottom-right
        Vertex {
            position: [half_width, -half_height, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [1.0, 1.0],
        },
    ];

    // Two triangles: TL→BL→TR and TR→BL→BR
    // Counter-clockwise winding (front face = CCW)
    let indices = vec![
        0, 2, 1, // Triangle 1: TL → BL → TR
        1, 2, 3, // Triangle 2: TR → BL → BR
    ];

    log::info!("Map quad generated: {}x{} units", half_width * 2.0, half_height * 2.0);

    (vertices, indices)
}

// =============================================================================
// Star Vertex (M14a)
// =============================================================================

/// A star vertex: position, brightness, and color.
///
/// 32 bytes per star. Rendered as billboard quads via instancing.
/// In M14, this is loaded from the HYG star catalog binary.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct StarVertex {
    pub position: [f32; 3],
    pub brightness: f32,
    pub color: [f32; 3],
    pub _pad: f32,
}

impl StarVertex {
    /// GPU layout for the star shader (instance buffer).
    /// Total size: 8 floats × 4 bytes = 32 bytes per instance.
    /// Each instance is rendered as a billboard quad (6 vertices in shader).
    pub fn buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<StarVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                // position: @location(0)
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // brightness: @location(1)
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32,
                },
                // color: @location(2)
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x3,
                },
            ],
        }
    }
}

// =============================================================================
// Star Catalog (M14a) — replaces procedural starfield (M8a)
// =============================================================================

/// Loads the HYG star catalog from a compact binary file.
///
/// Binary format (little-endian):
///   4 bytes: magic "STAR"
///   4 bytes: u32 star count
///   N x 32 bytes: StarVertex (position, brightness, color, _pad)
///
/// Falls back to a small procedural starfield if the file is missing.
pub fn load_star_catalog(path: &std::path::Path) -> Vec<StarVertex> {
    match std::fs::read(path) {
        Ok(data) => {
            if data.len() < 8 || &data[0..4] != b"STAR" {
                log::warn!("Invalid star catalog header, using fallback");
                return generate_starfield_fallback(3000, 50.0);
            }
            let count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
            let expected = 8 + count * 32;
            if data.len() < expected {
                log::warn!("Star catalog truncated ({} < {}), using fallback", data.len(), expected);
                return generate_starfield_fallback(3000, 50.0);
            }

            let star_bytes = &data[8..8 + count * 32];
            let stars: Vec<StarVertex> = bytemuck::cast_slice(star_bytes).to_vec();

            log::info!("Star catalog loaded: {} stars from {:?}", stars.len(), path);
            stars
        }
        Err(e) => {
            log::warn!("Could not load star catalog {:?}: {}, using fallback", path, e);
            generate_starfield_fallback(3000, 50.0)
        }
    }
}

/// Procedural fallback starfield (used when star catalog binary is missing).
fn generate_starfield_fallback(count: u32, radius: f32) -> Vec<StarVertex> {
    let mut rng = rand::rng();
    let mut stars = Vec::with_capacity(count as usize);

    for _ in 0..count {
        let z: f32 = rng.random_range(-1.0..1.0);
        let phi: f32 = rng.random_range(0.0..std::f32::consts::TAU);
        let r_xy = (1.0 - z * z).sqrt();

        let x = radius * r_xy * phi.cos();
        let y = radius * z;
        let z_pos = radius * r_xy * phi.sin();

        let raw: f32 = rng.random();
        let brightness = 0.1 + raw * raw * raw * 0.9;
        let warm = 0.7 + raw * 0.3;

        stars.push(StarVertex {
            position: [x, y, z_pos],
            brightness,
            color: [1.0, warm, warm * 0.85],
            _pad: 0.0,
        });
    }

    log::info!("Fallback starfield: {} stars", count);
    stars
}
