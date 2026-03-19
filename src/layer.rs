// =============================================================================
// Orbis — Layer System (M4 + M9)
// =============================================================================
// Manages any number of overlay textures laid over the base globe
// (day/night).
//
// Architecture:
// - Each layer has its own GPU texture + bind group
// - Each layer has its own settings buffer (opacity)
// - Rendering: multi-pass (base globe → layer 1 → layer 2 → ...)
// - Each layer pass uses alpha blending on the GPU
// - Opacity per layer controllable (0.0 = invisible, 1.0 = fully opaque)
//
// M9 additions:
// - Each layer references its provider ID (for persistence + re-download)
// - Layers can be added and removed dynamically at runtime
// - The base globe (day/night + sun lighting) is NOT a layer —
//   it is the foundation over which layers are placed.
// =============================================================================

use wgpu::util::DeviceExt;

use crate::texture::GpuTexture;
use crate::OverlaySettings;

/// A single overlay layer on the globe.
///
/// Each layer contains an equirectangular-projected texture
/// mapped onto the same UV sphere as the base textures.
/// Each layer owns its own GPU resources (bind groups + buffer),
/// so no buffer updates between draw calls are needed during rendering.
pub struct Layer {
    /// Unique ID matching the provider (e.g. "gibs_viirs_true_color", "grid")
    pub id: String,

    /// Display name for the GUI (e.g. "VIIRS True Color (2026-02-28)")
    pub label: String,

    /// Provider ID that created this layer (for persistence + re-download).
    /// "builtin:grid" for the coordinate grid, provider IDs for data layers.
    pub provider_id: String,

    /// GPU texture with the layer data
    #[allow(dead_code)]
    pub texture: GpuTexture,

    /// Opacity: 0.0 = invisible, 1.0 = fully opaque
    pub opacity: f32,

    /// Layer on/off (without removing it)
    pub enabled: bool,

    /// Bind group for layer texture + sampler → @group(1)
    pub texture_bind_group: wgpu::BindGroup,

    /// GPU buffer for overlay settings (opacity) → @group(2)
    pub settings_buffer: wgpu::Buffer,

    /// Bind group for overlay settings → @group(2)
    pub settings_bind_group: wgpu::BindGroup,
}

impl Layer {
    /// Creates a new layer from a ready-made GPU texture.
    ///
    /// Two bind groups are automatically created:
    /// - texture_bind_group (@group(1)): texture view + sampler
    /// - settings_bind_group (@group(2)): opacity uniform
    pub fn new(
        id: &str,
        label: &str,
        provider_id: &str,
        texture: GpuTexture,
        opacity: f32,
        texture_layout: &wgpu::BindGroupLayout,
        settings_layout: &wgpu::BindGroupLayout,
        device: &wgpu::Device,
    ) -> Self {
        // @group(1): Texture + Sampler
        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("Layer Texture Bind Group: {}", id)),
            layout: texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
            ],
        });

        // @group(2): Overlay settings (own buffer per layer)
        let settings = OverlaySettings {
            opacity,
            _pad: [0.0; 3],
        };
        let settings_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("Layer Settings Buffer: {}", id)),
            contents: bytemuck::cast_slice(&[settings]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let settings_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("Layer Settings Bind Group: {}", id)),
            layout: settings_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: settings_buffer.as_entire_binding(),
            }],
        });

        Self {
            id: id.to_string(),
            label: label.to_string(),
            provider_id: provider_id.to_string(),
            texture,
            opacity,
            enabled: true,
            texture_bind_group,
            settings_buffer,
            settings_bind_group,
        }
    }
}

/// Manages an ordered list of overlay layers.
///
/// The order determines the draw order:
/// Layer 0 is drawn first (directly over the base globe),
/// the last layer is on top.
pub struct LayerStack {
    pub layers: Vec<Layer>,
}

impl LayerStack {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Adds a layer on top of the stack.
    pub fn add(&mut self, layer: Layer) {
        log::info!(
            "Layer added: '{}' (provider={}, opacity={:.0}%)",
            layer.label,
            layer.provider_id,
            layer.opacity * 100.0,
        );
        self.layers.push(layer);
    }

    /// Removes a layer by its ID. Returns true if found and removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| l.id != id);
        let removed = self.layers.len() < before;
        if removed {
            log::info!("Layer removed: '{}'", id);
        }
        removed
    }

    /// Checks if a layer with the given provider_id already exists.
    pub fn has_provider(&self, provider_id: &str) -> bool {
        self.layers.iter().any(|l| l.provider_id == provider_id)
    }

    /// Iterator over all enabled layers (in draw order).
    pub fn enabled_layers(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.enabled)
    }

    /// Count of all layers (including disabled ones).
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.layers.len()
    }
}

// =============================================================================
// Test textures (procedurally generated)
// =============================================================================

/// Generates a coordinate grid texture (latitude/longitude lines).
///
/// - Yellow lines every 30° (main grid)
/// - Orange lines for equator and prime meridian (thicker)
/// - All semi-transparent over black background
///
/// Perfect for testing the layer system: you can immediately see
/// whether UV mapping is correct and whether alpha blending works.
pub fn generate_grid_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
) -> GpuTexture {
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            // UV → geographic coordinates
            let u = x as f32 / width as f32;
            let v = y as f32 / height as f32;
            let lon = u * 360.0 - 180.0; // -180..+180
            let lat = 90.0 - v * 180.0; //  +90..-90

            // Line thickness in degrees (depends on resolution)
            let thin = 180.0 / height as f32 * 1.5; // ~1.5 pixels wide
            let thick = thin * 2.5;

            // Equator (lat=0) and prime meridian (lon=0) — thick, orange
            let on_major = lon.abs() < thick || lat.abs() < thick;

            // Grid lines every 30° — thin, yellow
            let on_grid_30 = (lon % 30.0).abs() < thin
                || ((-lon) % 30.0).abs() < thin
                || (lat % 30.0).abs() < thin
                || ((-lat) % 30.0).abs() < thin;

            let idx = ((y * width + x) * 4) as usize;

            if on_major {
                // Orange, high opacity
                pixels[idx] = 255; // R
                pixels[idx + 1] = 165; // G
                pixels[idx + 2] = 0; // B
                pixels[idx + 3] = 220; // A
            } else if on_grid_30 {
                // Yellow, medium opacity
                pixels[idx] = 255; // R
                pixels[idx + 1] = 255; // G
                pixels[idx + 2] = 0; // B
                pixels[idx + 3] = 140; // A
            }
            // else: stays [0, 0, 0, 0] = fully transparent
        }
    }

    // Size is guaranteed correct — we just built the pixel array above
    GpuTexture::from_rgba(device, queue, &pixels, width, height, "Grid Overlay")
        .expect("Grid pixel buffer size mismatch (bug)")
}
