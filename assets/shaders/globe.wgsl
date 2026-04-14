// =============================================================================
// Orbis — Globe Shader (M8b: Atmosphären-Glow)
// =============================================================================
// Rendert die Erde mit realistischem Tag/Nacht-Übergang:
// - Tag-Textur (Blue Marble) auf der sonnenzugewandten Seite
// - Nacht-Textur (Black Marble Stadtlichter) auf der Schattenseite
// - Weicher Terminator: ~6° breite Übergangszone (Dämmerung)
// - Fresnel-basierter Atmosphären-Glow am Kugelrand
//
// Bind Groups:
// - @group(0): Kamera + Sonne (Uniform Buffer)
// - @group(1): Tag-Textur + Nacht-Textur + Sampler
// =============================================================================

// --- Kamera- und Sonnendaten ---
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
// Tag-Textur: NASA Blue Marble (Farbe bei Tageslicht)
@group(1) @binding(0)
var t_day: texture_2d<f32>;

// Nacht-Textur: NASA Black Marble (Stadtlichter bei Nacht)
@group(1) @binding(1)
var t_night: texture_2d<f32>;

// Gemeinsamer Sampler für beide Texturen
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
    @location(0) world_normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world_pos: vec3<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.world_normal = in.normal;
    out.uv = in.uv;
    out.world_pos = in.position;  // Kein Model-Transform → Position = Weltkoordinaten
    return out;
}

// --- Fragment Shader ---
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);
    let light_dir = normalize(camera.sun_dir);

    // Beide Texturen am gleichen UV-Punkt samplen
    let day_color = textureSample(t_day, s_earth, in.uv).rgb;
    let night_color = textureSample(t_night, s_earth, in.uv).rgb;

    // === Tag/Nacht-Blending ===
    //
    // dot(normal, light_dir):
    //   +1.0 = Punkt zeigt direkt zur Sonne (Mittag)
    //    0.0 = Punkt liegt auf dem Terminator (Sonnenauf-/-untergang)
    //   -1.0 = Punkt zeigt weg von der Sonne (Mitternacht)
    //
    // smoothstep(-0.1, 0.1, x) erzeugt eine S-Kurve:
    //   x < -0.1 → 0.0 (volle Nacht)
    //   x >  0.1 → 1.0 (voller Tag)
    //   dazwischen → sanfter Übergang
    //
    // ±0.1 entspricht ca. ±6° um den Terminator — eine realistische
    // Dämmerungszone (zivile Dämmerung geht bis ~6° unter dem Horizont).
    let ndot = dot(normal, light_dir);
    let day_factor = smoothstep(-0.1, 0.1, ndot);

    // Tagseite: Texturfarbe × diffuse Beleuchtung
    let diffuse = max(ndot, 0.0);
    let day_lit = day_color * (0.15 + 0.85 * diffuse);

    // Nachtseite: Stadtlichter (emissiv, leichter Boost)
    let night_lit = night_color * 1.5;

    // Mischen: day_factor=1 → Tag, day_factor=0 → Nacht
    let surface_color = mix(night_lit, day_lit, vec3<f32>(day_factor));

    // === Atmosphären-Glow (Fresnel) ===
    //
    // Blickrichtung: vom Fragment zur Kamera
    let view_dir = normalize(camera.eye_pos - in.world_pos);
    //
    // Fresnel-Term: dot(view, normal) = 1.0 in der Mitte, ~0.0 am Rand
    // Invertiert: rim = 1 - dot → 0 in der Mitte, 1 am Rand
    // abs() instead of max() because the inside-out globe has normals
    // facing away from the camera — dot is negative for the visible side.
    // abs() makes rim=0 at center (facing camera) and rim=1 at edges.
    let rim = 1.0 - abs(dot(view_dir, normal));
    //
    // Potenz steuert die Breite des Glows:
    //   Exponent 3 → schmaler, dezenter Rand (realistisch)
    //   Exponent 2 → breiter, auffälliger (künstlerisch)
    let fresnel = pow(rim, 3.0);
    //
    // Atmosphärenfarbe: helles Blau, Stärke durch fresnel moduliert
    // Auf der Tagseite stärker (Sonne beleuchtet die Atmosphäre),
    // auf der Nachtseite nur ein Hauch sichtbar.
    let atmo_color = vec3<f32>(0.3, 0.6, 1.0);
    let atmo_day_strength = 0.6;    // Tagseite: deutlich sichtbar
    let atmo_night_strength = 0.08; // Nachtseite: dezenter Schimmer
    let atmo_strength = mix(atmo_night_strength, atmo_day_strength, day_factor);
    let atmosphere = atmo_color * fresnel * atmo_strength;

    // Atmosphäre additiv über die Oberfläche legen
    let final_color = surface_color + atmosphere;

    return vec4<f32>(final_color, 1.0);
}
