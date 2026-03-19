// =============================================================================
// Orbis — Basis-Shader (M1b)
// =============================================================================
// WGSL (WebGPU Shading Language) ist die Shader-Sprache von wgpu.
// Wenn du GLSL oder HLSL kennst: WGSL ist syntaktisch anders, aber
// konzeptionell identisch. Typen werden mit Rust-ähnlicher Syntax geschrieben.
// =============================================================================

// --- Datenstrukturen ---

// Das hier ist das "Ausgabeformat" des Vertex Shaders.
// Alles was der Vertex Shader berechnet und an den Fragment Shader
// weitergeben will, muss hier deklariert werden.
struct VertexOutput {
    // @builtin(position) ist ein Pflichtfeld — sagt der GPU, wo der Vertex
    // auf dem Bildschirm liegt. vec4<f32> = 4 Floats (x, y, z, w).
    @builtin(position) clip_position: vec4<f32>,

    // @location(0) ist ein benutzerdefiniertes Feld — wir nutzen es für die Farbe.
    // Die GPU interpoliert diesen Wert automatisch zwischen den drei Ecken
    // des Dreiecks. Das erzeugt den schönen Farbverlauf.
    @location(0) color: vec3<f32>,
};

// --- Vertex Shader ---
// Läuft einmal pro Vertex. Bekommt Position und Farbe als Input,
// gibt die transformierte Position und die Farbe als Output.
//
// @vertex          = "Das ist ein Vertex Shader"
// @location(0)     = Erste Eingabe aus dem Vertex Buffer (Position)
// @location(1)     = Zweite Eingabe aus dem Vertex Buffer (Farbe)
@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
) -> VertexOutput {
    var out: VertexOutput;

    // Position direkt durchreichen (noch keine Kamera-Transformation).
    // w = 1.0 ist Standard für Punkte (0.0 wäre für Richtungsvektoren).
    out.clip_position = vec4<f32>(position, 1.0);

    // Farbe durchreichen — wird zwischen Vertices interpoliert
    out.color = color;

    return out;
}

// --- Fragment Shader ---
// Läuft einmal pro Pixel innerhalb des Dreiecks.
// Bekommt die interpolierte Farbe vom Vertex Shader.
//
// @fragment         = "Das ist ein Fragment Shader"
// @location(0)      = Ausgabe in das erste Color Attachment (= unser Bildschirm)
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // RGB aus dem Vertex Shader + Alpha 1.0 (voll opak)
    return vec4<f32>(in.color, 1.0);
}
