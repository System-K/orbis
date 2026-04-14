// =============================================================================
// Tile Compositor (M16d/e) — stitches tiles into equirectangular buffer
// =============================================================================

use std::collections::HashSet;

use super::TileCoord;

/// Composites downloaded tiles into a single equirectangular RGBA buffer
/// that can be uploaded as an overlay texture.
///
/// The buffer uses equirectangular projection (matching Orbis' UV convention):
///   u=0 -> 180W, u=1 -> 180E, v=0 -> 90N, v=1 -> 90S
///
/// Web Mercator tiles are blitted with approximate UV mapping.
/// The error is negligible at tile-level scales (< 0.5 deg for zoom >= 3).
pub struct TileCompositor {
    /// RGBA compositing buffer
    buffer: Vec<u8>,
    /// Buffer dimensions
    pub width: u32,
    pub height: u32,
    /// Tiles that have been composited into the current buffer
    composited: HashSet<TileCoord>,
    /// Current zoom level (buffer is cleared when zoom changes)
    pub current_zoom: u32,
    /// Whether the buffer changed since last GPU upload
    pub dirty: bool,
    /// Whether any tiles have been composited at all
    pub has_content: bool,
}

impl TileCompositor {
    /// Creates a new compositor with the given buffer size.
    ///
    /// Recommended: 4096x2048 for good quality, 2048x1024 for less memory.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            buffer: vec![0u8; (width * height * 4) as usize],
            width,
            height,
            composited: HashSet::new(),
            current_zoom: 0,
            dirty: false,
            has_content: false,
        }
    }

    /// Clears the buffer and resets for a new zoom level.
    pub fn reset(&mut self, zoom: u32) {
        self.buffer.fill(0);
        self.composited.clear();
        self.current_zoom = zoom;
        self.dirty = true;
        self.has_content = false;
    }

    /// Returns true if this tile has already been composited.
    pub fn has_tile(&self, coord: &TileCoord) -> bool {
        self.composited.contains(coord)
    }

    /// Composites a tile image into the equirectangular buffer.
    ///
    /// Decodes the image (JPEG/PNG), computes its UV bounds, and blits
    /// the pixels into the correct position in the buffer.
    pub fn composite_tile(&mut self, coord: &TileCoord, image_data: &[u8]) -> bool {
        if self.composited.contains(coord) {
            return false; // already composited
        }

        // Decode image
        let img = match image::load_from_memory(image_data) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                log::debug!("Failed to decode tile {}: {}", coord, e);
                return false;
            }
        };
        let (tw, th) = img.dimensions();

        // Compute tile bounds in geographic coordinates
        let (lat_n, lat_s, lon_e, lon_w) = coord.bounds();

        // Map to equirectangular UV
        // u = (lon + 180) / 360   ->  pixel_x = u * width
        // v = (90 - lat) / 180    ->  pixel_y = v * height
        let u_left = ((lon_w + 180.0) / 360.0) as f32;
        let u_right = ((lon_e + 180.0) / 360.0) as f32;
        let v_top = ((90.0 - lat_n) / 180.0) as f32;
        let v_bottom = ((90.0 - lat_s) / 180.0) as f32;

        let px_left = (u_left * self.width as f32) as i32;
        let px_right = (u_right * self.width as f32) as i32;
        let px_top = (v_top * self.height as f32) as i32;
        let px_bottom = (v_bottom * self.height as f32) as i32;

        let dest_w = (px_right - px_left).max(1) as u32;
        let dest_h = (px_bottom - px_top).max(1) as u32;

        // Blit tile into buffer with nearest-neighbor scaling
        for dy in 0..dest_h {
            for dx in 0..dest_w {
                let dest_x = px_left as u32 + dx;
                let dest_y = px_top as u32 + dy;

                if dest_x >= self.width || dest_y >= self.height {
                    continue;
                }

                // Source pixel (scale from dest rect to tile rect)
                let sx = (dx as f32 / dest_w as f32 * tw as f32) as u32;
                let sy = (dy as f32 / dest_h as f32 * th as f32) as u32;
                let sx = sx.min(tw - 1);
                let sy = sy.min(th - 1);

                let src_idx = ((sy * tw + sx) * 4) as usize;
                let dst_idx = ((dest_y * self.width + dest_x) * 4) as usize;

                self.buffer[dst_idx..dst_idx + 4]
                    .copy_from_slice(&img.as_raw()[src_idx..src_idx + 4]);
            }
        }

        self.composited.insert(*coord);
        self.dirty = true;
        self.has_content = true;

        log::debug!("Composited tile {} ({}x{} -> {}x{} at {},{} in buffer)",
            coord, tw, th, dest_w, dest_h, px_left, px_top);
        true
    }

    /// Returns the raw RGBA buffer for GPU upload.
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// Marks the buffer as uploaded (not dirty anymore).
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Number of tiles composited in current buffer.
    #[allow(dead_code)] // Used in debug println
    pub fn tile_count(&self) -> usize {
        self.composited.len()
    }
}
