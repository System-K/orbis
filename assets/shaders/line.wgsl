// =============================================================================
// Orbis — Line Shader (M11c) — Screen-Space Widened Segments
// =============================================================================
// Each line segment is an instance rendered as a quad (6 vertices).
// The vertex shader projects both endpoints to clip space, computes
// the perpendicular direction in screen space, and offsets vertices
// to create a line with configurable pixel width.
//
// This gives constant visual width regardless of distance/zoom.
//
// Bind Groups:
// - @group(0): Camera uniform (view_proj, view_mode)
// =============================================================================

struct CameraUniform {
    view_proj: mat4x4<f32>,
    eye_pos: vec3<f32>,
    _pad1: f32,
    sun_dir: vec3<f32>,
    view_mode: f32,
}

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// Per-instance data (one per line segment)
struct LineSegment {
    @location(0) start_lon_lat: vec2<f32>,
    @location(1) end_lon_lat: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) width: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

const QUAD_HW: f32 = 4.0;

/// Convert lon/lat (degrees) to world-space position.
fn geo_to_world(lon_deg: f32, lat_deg: f32) -> vec3<f32> {
    let lon_rad = radians(lon_deg);
    let lat_rad = radians(lat_deg);

    if camera.view_mode < 0.5 {
        // Globe: Orbis convention (negated x/z)
        let r = 1.003;
        return vec3<f32>(
            -r * cos(lat_rad) * cos(lon_rad),
             r * sin(lat_rad),
            -r * cos(lat_rad) * sin(lon_rad),
        );
    } else {
        // Map2D: equirectangular on flat quad
        let u = (lon_deg + 180.0) / 360.0;
        let v = (90.0 - lat_deg) / 180.0;
        let x = (u * 2.0 - 1.0) * QUAD_HW;
        let y = (1.0 - v * 2.0) * QUAD_HW * 0.5;
        return vec3<f32>(x, y, 0.005);
    }
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    seg: LineSegment,
) -> VertexOutput {
    // Quad corners: 6 vertices, 2 triangles
    // Encode: which endpoint (0=start, 1=end) and which side (-1 or +1)
    //   v0: start, left    v1: end, left     v2: start, right
    //   v3: start, right   v4: end, left     v5: end, right
    var endpoint = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
    var side     = array<f32, 6>(-1.0, -1.0, 1.0, 1.0, -1.0, 1.0);

    let ep = endpoint[vertex_index];
    let sd = side[vertex_index];

    // Compute world positions
    let world_start = geo_to_world(seg.start_lon_lat.x, seg.start_lon_lat.y);
    let world_end   = geo_to_world(seg.end_lon_lat.x, seg.end_lon_lat.y);

    // Globe occlusion: collapse segment to zero width if both endpoints
    // are on the far side of the globe (same approach as marker.wgsl)
    var visibility = 1.0;
    if camera.view_mode < 0.5 {
        let ns = normalize(world_start);
        let ne = normalize(world_end);
        let cam_dir_s = normalize(camera.eye_pos - world_start);
        let cam_dir_e = normalize(camera.eye_pos - world_end);
        let ds = dot(ns, cam_dir_s);
        let de = dot(ne, cam_dir_e);
        // Hide when both endpoints face away from camera
        visibility = select(0.0, 1.0, ds < 0.05 || de < 0.05);
    }

    // Project both endpoints to clip space
    let clip_start = camera.view_proj * vec4<f32>(world_start, 1.0);
    let clip_end   = camera.view_proj * vec4<f32>(world_end, 1.0);

    // Convert to NDC (normalized device coordinates)
    let ndc_start = clip_start.xy / clip_start.w;
    let ndc_end   = clip_end.xy / clip_end.w;

    // Line direction and perpendicular in screen space
    var dir = ndc_end - ndc_start;
    let len = length(dir);
    if len < 0.00001 {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = dir / len;
    }
    let perp = vec2<f32>(-dir.y, dir.x);

    // Pick the endpoint for this vertex
    var clip_pos: vec4<f32>;
    if ep < 0.5 {
        clip_pos = clip_start;
    } else {
        clip_pos = clip_end;
    }

    // Offset in clip space (width in pixels → NDC units)
    // seg.width is in pixels. Typical screen ~1000px wide → NDC width = 2.0
    // So 1 pixel ≈ 2.0/screen_width ≈ 0.002 NDC units
    let pixel_scale = 0.0015;
    let offset = perp * sd * seg.width * pixel_scale * clip_pos.w * visibility;

    var out: VertexOutput;
    out.clip_position = vec4<f32>(
        clip_pos.xy + offset,
        clip_pos.z,
        clip_pos.w,
    );
    out.color = seg.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
