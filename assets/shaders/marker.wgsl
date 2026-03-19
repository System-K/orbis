// =============================================================================
// Orbis — Marker Shader (M11b) — Instanced Billboard Circles
// =============================================================================
// Renders geographic point markers as screen-aligned circles.
//
// Each marker is an instance with lat/lon, color, and size.
// The vertex shader computes the 3D world position from lat/lon,
// choosing between globe (sphere surface) or map (flat quad) mode
// based on camera.view_mode.
//
// Bind Groups:
// - @group(0): Camera uniform (view_proj, view_mode)
// =============================================================================

struct CameraUniform {
    view_proj: mat4x4<f32>,
    eye_pos: vec3<f32>,
    _pad1: f32,
    sun_dir: vec3<f32>,
    view_mode: f32,  // 0.0 = Globe3D, 1.0 = Map2D
}

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// Per-instance data
struct MarkerInstance {
    @location(0) lon_lat: vec2<f32>,   // longitude, latitude (degrees)
    @location(1) color: vec4<f32>,     // RGBA
    @location(2) size: f32,            // radius in clip-space units
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,        // (-1,-1) to (1,1) within quad
}

// Map quad half-width (must match QUAD_HALF_WIDTH in main.rs)
const QUAD_HW: f32 = 4.0;

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: MarkerInstance,
) -> VertexOutput {
    // 6 vertices = 2 triangles = 1 quad (same pattern as stars)
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let corner = corners[vertex_index];

    // Convert lat/lon (degrees) to radians
    let lon_rad = radians(instance.lon_lat.x);
    let lat_rad = radians(instance.lon_lat.y);

    // Compute world position based on view mode
    var world_pos: vec3<f32>;

    if camera.view_mode < 0.5 {
        // Globe3D: position on unit sphere surface (+ small offset to avoid z-fighting)
        // Orbis convention (negated x/z vs standard):
        //   x = -cos(lat) * cos(lon)
        //   y =  sin(lat)
        //   z = -cos(lat) * sin(lon)
        let r = 1.002;
        world_pos = vec3<f32>(
            -r * cos(lat_rad) * cos(lon_rad),
             r * sin(lat_rad),
            -r * cos(lat_rad) * sin(lon_rad),
        );
    } else {
        // Map2D: position on flat quad
        let u = (instance.lon_lat.x + 180.0) / 360.0;
        let v = (90.0 - instance.lon_lat.y) / 180.0;
        let x = (u * 2.0 - 1.0) * QUAD_HW;
        let y = (1.0 - v * 2.0) * QUAD_HW * 0.5;
        world_pos = vec3<f32>(x, y, 0.01);
    }

    // Project to clip space
    let clip_center = camera.view_proj * vec4<f32>(world_pos, 1.0);

    // Globe occlusion: collapse billboard to zero size for far-side markers.
    // In Orbis coords, the outward normal at a point is normalize(world_pos).
    // Due to the negated x/z convention, a VISIBLE marker has its normal
    // pointing AWAY from the camera → dot(normal, to_camera) < 0.
    // A far-side marker has dot > 0.
    var visibility = 1.0;
    if camera.view_mode < 0.5 {
        let to_cam = normalize(camera.eye_pos - world_pos);
        let normal = normalize(world_pos);
        let d = dot(normal, to_cam);
        // Hide when d > 0 (far side). Small positive threshold hides horizon-edge markers.
        visibility = select(0.0, 1.0, d < 0.05);
    }

    // Billboard size in clip space (perspective-correct)
    let base_size = instance.size * 0.0006 * visibility;

    var out: VertexOutput;
    out.clip_position = vec4<f32>(
        clip_center.xy + corner * base_size * clip_center.w,
        clip_center.z,
        clip_center.w,
    );
    out.color = instance.color;
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Soft circle with anti-aliased edge
    let dist = length(in.uv);

    if dist > 1.0 {
        discard;
    }

    // Smooth edge (anti-aliasing in the last 20% of radius)
    let edge_softness = 1.0 - smoothstep(0.7, 1.0, dist);

    // Subtle highlight in the center (3D look)
    let highlight = 1.0 + 0.3 * (1.0 - dist * dist);

    let color = in.color.rgb * highlight;
    let alpha = in.color.a * edge_softness;

    return vec4<f32>(color, alpha);
}
