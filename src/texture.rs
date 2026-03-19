// =============================================================================
// Orbis — Texture Management
// =============================================================================
// Loads image files or raw pixel arrays and creates wgpu textures from them.
//
// Three paths to a GPU texture:
// - from_file():     Load image from disk (PNG, JPEG, etc.)
// - from_rgba():     Pass raw RGBA data directly (e.g. procedural)
// - fallback():      Magenta checkerboard as placeholder on errors
//
// from_file() returns Result — the caller decides whether an error
// causes a crash or a fallback is used.
// =============================================================================

use std::path::Path;

/// Everything needed to use a texture in a shader:
/// - `texture`: The pixel data on the GPU
/// - `view`: A "view" of the texture (tells the GPU how to interpret it)
/// - `sampler`: Configuration for texture sampling (filtering, wrap mode)
pub struct GpuTexture {
    #[allow(dead_code)]
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

impl GpuTexture {
    /// Loads an image file and creates a ready-to-use GPU texture.
    ///
    /// Returns `Err` if the file doesn't exist, isn't readable,
    /// or isn't a valid image format. The caller can then use
    /// `fallback()` or handle the error.
    pub fn from_file(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &Path,
        label: &str,
    ) -> Result<Self, String> {
        let img = image::open(path)
            .map_err(|e| format!("Cannot load texture '{}': {}", path.display(), e))?;

        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        log::info!("Texture loaded: {} ({}×{})", label, width, height);

        Ok(Self::create_from_rgba(device, queue, &rgba, width, height, label))
    }

    /// Creates a GPU texture from raw RGBA pixel data.
    ///
    /// Useful for procedurally generated textures (e.g. grid lines,
    /// test patterns) or data already in RAM (e.g. after tile assembly).
    ///
    /// Returns `Err` if the data size doesn't match width × height × 4.
    pub fn from_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[u8],
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<Self, String> {
        let expected = (width * height * 4) as usize;
        if data.len() != expected {
            return Err(format!(
                "RGBA data size mismatch for '{}': expected {} bytes ({}×{}×4), got {}",
                label, expected, width, height, data.len()
            ));
        }
        log::info!("Texture created: {} ({}×{})", label, width, height);

        Ok(Self::create_from_rgba(device, queue, data, width, height, label))
    }

    /// Creates a magenta checkerboard fallback texture (64×64).
    ///
    /// Displayed when a texture could not be loaded.
    /// Magenta-black checkerboard is instantly recognizable as "missing"
    /// (convention from game development).
    pub fn fallback(device: &wgpu::Device, queue: &wgpu::Queue, label: &str) -> Self {
        const SIZE: u32 = 64;
        const CELL: u32 = 8; // 8×8 pixels per checker cell
        let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];

        for y in 0..SIZE {
            for x in 0..SIZE {
                let idx = ((y * SIZE + x) * 4) as usize;
                let checker = ((x / CELL) + (y / CELL)) % 2 == 0;
                if checker {
                    // Magenta: instantly recognizable as error
                    data[idx] = 255;     // R
                    data[idx + 1] = 0;   // G
                    data[idx + 2] = 255; // B
                    data[idx + 3] = 255; // A
                } else {
                    // Black
                    data[idx] = 0;
                    data[idx + 1] = 0;
                    data[idx + 2] = 0;
                    data[idx + 3] = 255;
                }
            }
        }

        log::warn!("Fallback texture created: {} ({}×{} checkerboard)", label, SIZE, SIZE);
        Self::create_from_rgba(device, queue, &data, SIZE, SIZE, label)
    }

    /// Internal function: upload RGBA data to the GPU.
    ///
    /// Shared by `from_file()`, `from_rgba()` and `fallback()`.
    fn create_from_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgba_data: &[u8],
        width: u32,
        height: u32,
        label: &str,
    ) -> Self {
        // Rgba8UnormSrgb = 4 channels, 8 bits per channel, sRGB color space.
        // sRGB is important: image files store colors in sRGB (non-linear),
        // and the GPU automatically converts them to linear color space when reading.
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{} Sampler", label)),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            texture,
            view,
            sampler,
        }
    }
}
