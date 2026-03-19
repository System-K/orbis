// =============================================================================
// Orbis — Star Shader (M14a) — Billboard-Quads with Real Colors
// =============================================================================
// Each star is rendered as a small screen-aligned quad (instancing).
// The vertex shader generates quad corners from vertex_index (0..5),
// the fragment shader paints a soft glow circle with falloff.
//
// M14a update: star colors now come from the HYG catalog (B-V index),
// replacing the old brightness-based warm/cool heuristic.
// Blue O/B stars, white A stars, yellow G stars, orange K, red M stars.
//
// Bind Groups:
// - @group(0): Camera (view_proj)
// =============================================================================

struct CameraUniform {
    view_proj: mat4x4<f32>,
    eye_pos: vec3<f32>,
    // _pad1
    sun_dir: vec3<f32>,
    // _pad2
}

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// Instance data (per star) — 32 bytes, matches StarVertex in mesh.rs
struct StarInstance {
    @location(0) position: vec3<f32>,
    @location(1) brightness: f32,
    @location(2) color: vec3<f32>,
    // _pad (not mapped — implicit from stride)
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) brightness: f32,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec3<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: StarInstance,
) -> VertexOutput {
    // 6 vertices = 2 triangles = 1 quad
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );

    let corner = corners[vertex_index];

    // Project star position to clip space
    let clip_center = camera.view_proj * vec4<f32>(instance.position, 1.0);

    // Star size in clip space (pixel-independent):
    // Bright stars (brightness~1.0): ~6px, faint (~0.15): ~2px
    let base_size = 0.004;
    let size = base_size * (0.5 + instance.brightness * 2.5);

    var out: VertexOutput;
    out.clip_position = vec4<f32>(
        clip_center.xy + corner * size * clip_center.w,
        clip_center.z,
        clip_center.w,
    );
    out.brightness = instance.brightness;
    out.uv = corner;
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Soft circular glow
    let dist = length(in.uv);

    if dist > 1.0 {
        discard;
    }

    // Smooth falloff: bright core, transparent edge
    let glow = 1.0 - dist * dist;
    // pow(0.4) brightens faint stars aggressively so mag 7 is clearly visible
    let alpha = glow * pow(in.brightness, 0.4);

    // Use real star color from HYG catalog (B-V color index)
    // Bright core stays close to catalog color,
    // faint glow at edges goes slightly whiter
    let core_color = in.color;
    let edge_color = mix(in.color, vec3<f32>(1.0, 1.0, 1.0), 0.3);
    let color = mix(edge_color, core_color, glow);

    return vec4<f32>(color * glow, alpha);
}
