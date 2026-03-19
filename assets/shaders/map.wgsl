// =============================================================================
// Orbis — Map Shader (M7b)
// =============================================================================
// Rendert die 2D-Equirectangular-Kartenprojektion mit Tag/Nacht-Zyklus.
//
// Problem: Auf dem flachen Quad zeigen alle Normalen in +Z.
// Lösung: Die "virtuelle Kugel-Normale" wird aus den UV-Koordinaten
// rekonstruiert. Da UV = Equirectangular-Projektion der Kugel gilt:
//
//   theta = v * PI       (0 = Nordpol, PI = Südpol)
//   phi   = u * 2*PI     (0 = 180°W, PI = 0°E, 2*PI = 180°E)
//
//   normal = (sin(theta)*cos(phi), cos(theta), sin(theta)*sin(phi))
//
// Das ist exakt die gleiche Formel wie in mesh.rs für die Sphere-Vertices.
// Damit funktioniert dot(normal, sun_dir) identisch zum 3D-Globus.
//
// Bind Groups:
// - @group(0): Kamera (Orthographische View-Projection-Matrix)
// - @group(1): Tag-Textur + Nacht-Textur + Sampler
// =============================================================================

const PI: f32 = 3.14159265358979323846;

// --- Kamera-Daten ---
struct CameraUniform {
    view_proj: mat4x4<f32>,
    eye_pos: vec3<f32>,
    // _pad1
    sun_dir: vec3<f32>,
    // _pad2
}

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

// --- Texturen ---
@group(1) @binding(0)
var t_day: texture_2d<f32>;

@group(1) @binding(1)
var t_night: texture_2d<f32>;

@group(1) @binding(2)
var s_earth: sampler;

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

// --- Fragment Shader ---
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Kugel-Normale aus UV rekonstruieren (Equirectangular → Kugelkoordinaten)
    let theta = in.uv.y * PI;          // v: 0=Nordpol, 1=Südpol
    let phi = in.uv.x * 2.0 * PI;     // u: 0=180°W, 1=180°E

    let sin_theta = sin(theta);
    let cos_theta = cos(theta);

    let normal = vec3<f32>(
        sin_theta * cos(phi),   // x
        cos_theta,              // y (oben)
        sin_theta * sin(phi),   // z
    );

    let light_dir = normalize(camera.sun_dir);

    // Texturen samplen
    let day_color = textureSample(t_day, s_earth, in.uv).rgb;
    let night_color = textureSample(t_night, s_earth, in.uv).rgb;

    // === Tag/Nacht-Blending (identisch zu globe.wgsl) ===
    let ndot = dot(normal, light_dir);
    let day_factor = smoothstep(-0.1, 0.1, ndot);

    // Tagseite: diffuse Beleuchtung
    let diffuse = max(ndot, 0.0);
    let day_lit = day_color * (0.15 + 0.85 * diffuse);

    // Nachtseite: Stadtlichter (emissiv)
    let night_lit = night_color * 1.5;

    let final_color = mix(night_lit, day_lit, vec3<f32>(day_factor));

    return vec4<f32>(final_color, 1.0);
}
