// =============================================================================
// Orbis — Overlay-Shader (M4)
// =============================================================================
// Rendert einen einzelnen Overlay-Layer auf dem Globus.
//
// Wird im Multi-Pass-Verfahren NACH dem Basis-Globus gezeichnet.
// Die Render-Pipeline nutzt Alpha-Blending, sodass transparente Bereiche
// den darunterliegenden Basis-Globus durchscheinen lassen.
//
// Bind Groups:
// - @group(0): Kamera (gleich wie beim Basis-Globus)
// - @group(1): Layer-Textur + Sampler
// - @group(2): Overlay-Einstellungen (Opacity)
// =============================================================================

// --- Kamera-Daten (identisch zum Basis-Shader) ---
struct CameraUniform {
    view_proj: mat4x4<f32>,
    eye_pos: vec3<f32>,
    // _pad1
    sun_dir: vec3<f32>,
    // _pad2
}

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// --- Layer-Textur ---
@group(1) @binding(0)
var t_overlay: texture_2d<f32>;

@group(1) @binding(1)
var s_overlay: sampler;

// --- Overlay-Einstellungen ---
struct OverlaySettings {
    opacity: f32,
    // 12 Bytes Padding (GPU-Alignment: Uniform Buffers auf 16-Byte-Grenzen)
}

@group(2) @binding(0)
var<uniform> overlay: OverlaySettings;

// --- Vertex Input/Output ---
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_overlay, s_overlay, in.uv);

    // Textur-Alpha × Layer-Opacity = finale Transparenz.
    // Die Render-Pipeline macht dann Alpha-Blending mit dem Framebuffer:
    // final = src.rgb × src.a + dst.rgb × (1 - src.a)
    return vec4<f32>(color.rgb, color.a * overlay.opacity);
}
