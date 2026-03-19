// =============================================================================
// Orbis — Polygon Shader (M11d) — Filled Geographic Areas
// =============================================================================
// Renders triangulated polygons from GeoJSON. Each vertex carries
// lon/lat coordinates, converted to world position in the vertex shader.
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

struct VertexInput {
    @location(0) lon_lat: vec2<f32>,
    @location(1) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

const QUAD_HW: f32 = 4.0;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let lon_rad = radians(in.lon_lat.x);
    let lat_rad = radians(in.lon_lat.y);

    var world_pos: vec3<f32>;

    if camera.view_mode < 0.5 {
        // Globe: Orbis convention (negated x/z)
        let r = 1.001;
        world_pos = vec3<f32>(
            -r * cos(lat_rad) * cos(lon_rad),
             r * sin(lat_rad),
            -r * cos(lat_rad) * sin(lon_rad),
        );
    } else {
        // Map2D
        let u = (in.lon_lat.x + 180.0) / 360.0;
        let v = (90.0 - in.lon_lat.y) / 180.0;
        let x = (u * 2.0 - 1.0) * QUAD_HW;
        let y = (1.0 - v * 2.0) * QUAD_HW * 0.5;
        world_pos = vec3<f32>(x, y, 0.002);
    }

    // Globe occlusion: fade out vertices on the far side
    var alpha_mul = 1.0;
    if camera.view_mode < 0.5 {
        let normal = normalize(world_pos);
        let to_cam = normalize(camera.eye_pos - world_pos);
        let d = dot(normal, to_cam);
        // Smooth fade near the horizon to avoid hard polygon edges
        alpha_mul = select(0.0, 1.0, d < 0.1);
    }

    let clip = camera.view_proj * vec4<f32>(world_pos, 1.0);

    var out: VertexOutput;
    out.clip_position = clip;
    out.color = vec4<f32>(in.color.rgb, in.color.a * alpha_mul);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.color.a < 0.01 {
        discard;
    }
    return in.color;
}
