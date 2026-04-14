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
    /// Bounding box of changed pixels since the last `mark_clean`
    /// (half-open: [x_min, x_max) × [y_min, y_max)). When `dirty` is
    /// false these are undefined and must not be read.
    dirty_x_min: u32,
    dirty_y_min: u32,
    dirty_x_max: u32,
    dirty_y_max: u32,
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
            dirty_x_min: 0,
            dirty_y_min: 0,
            dirty_x_max: 0,
            dirty_y_max: 0,
        }
    }

    /// Full reset — clears the buffer and tracking. Use only when the source
    /// changes or the cache is cleared, because it produces a one-frame
    /// black flash.
    pub fn reset(&mut self, zoom: u32) {
        self.buffer.fill(0);
        self.composited.clear();
        self.current_zoom = zoom;
        self.dirty = true;
        self.has_content = false;
        // The zero-fill dirtied the whole buffer — upload it all so no
        // stale pixels from the previous source remain on the GPU.
        self.dirty_x_min = 0;
        self.dirty_y_min = 0;
        self.dirty_x_max = self.width;
        self.dirty_y_max = self.height;
    }

    /// Transitions to a new zoom level WITHOUT clearing the pixel buffer.
    ///
    /// Coord tracking for the old zoom is dropped, so the manager will
    /// re-request the new zoom's tiles. The old pixels remain visible
    /// until new tiles overwrite their regions, avoiding the black flash
    /// during a zoom step.
    pub fn demote_to_zoom(&mut self, new_zoom: u32) {
        if new_zoom == self.current_zoom {
            return;
        }
        // Keep only entries at the new zoom level (normally none — callers
        // usually hit this path on an actual zoom change — but be defensive).
        self.composited.retain(|c| c.z == new_zoom);
        self.current_zoom = new_zoom;
        // Buffer unchanged, not dirty — nothing to re-upload this frame.
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

        // Expand dirty rect to the clipped destination region we actually
        // wrote. Use the same clamps as the blit loop so the upload region
        // never lies outside the buffer.
        let touched_x_min = px_left.max(0) as u32;
        let touched_y_min = px_top.max(0) as u32;
        let touched_x_max = (px_left.max(0) as u32 + dest_w).min(self.width);
        let touched_y_max = (px_top.max(0) as u32 + dest_h).min(self.height);
        if touched_x_max > touched_x_min && touched_y_max > touched_y_min {
            self.expand_dirty_rect(
                touched_x_min, touched_y_min, touched_x_max, touched_y_max,
            );
        }

        self.composited.insert(*coord);
        self.dirty = true;
        self.has_content = true;

        log::debug!("Composited tile {} ({}x{} -> {}x{} at {},{} in buffer)",
            coord, tw, th, dest_w, dest_h, px_left, px_top);
        true
    }

    /// Grows the dirty rect to include the given half-open rectangle.
    /// Assumes the caller already established `dirty=true` (or is about
    /// to) — the rect is reset by `mark_clean`, not by `dirty=false`
    /// alone.
    fn expand_dirty_rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        if !self.dirty {
            self.dirty_x_min = x0;
            self.dirty_y_min = y0;
            self.dirty_x_max = x1;
            self.dirty_y_max = y1;
        } else {
            self.dirty_x_min = self.dirty_x_min.min(x0);
            self.dirty_y_min = self.dirty_y_min.min(y0);
            self.dirty_x_max = self.dirty_x_max.max(x1);
            self.dirty_y_max = self.dirty_y_max.max(y1);
        }
    }

    /// Returns the current dirty rectangle as (x_min, y_min, x_max, y_max).
    /// Only valid when `dirty` is true.
    pub fn dirty_rect(&self) -> (u32, u32, u32, u32) {
        (self.dirty_x_min, self.dirty_y_min, self.dirty_x_max, self.dirty_y_max)
    }

    /// Returns the raw RGBA buffer for GPU upload.
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// Marks the buffer as uploaded (not dirty anymore) and resets the
    /// dirty rect so the next composite starts a fresh region.
    pub fn mark_clean(&mut self) {
        self.dirty = false;
        self.dirty_x_min = 0;
        self.dirty_y_min = 0;
        self.dirty_x_max = 0;
        self.dirty_y_max = 0;
    }

    /// Number of tiles composited in current buffer.
    #[allow(dead_code)] // Used in debug println
    pub fn tile_count(&self) -> usize {
        self.composited.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    /// Build a tiny 1x1 RGBA PNG so composite_tile actually decodes.
    fn minimal_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 128, 64, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode minimal png");
        out
    }

    #[test]
    fn test_demote_to_zoom_keeps_pixels() {
        // Composite a tile at z=3, then demote to z=4. The pixel buffer
        // must stay intact (has_content still true, buffer non-zero
        // somewhere), but coord-tracking for the old zoom must be gone so
        // the manager re-requests at the new zoom.
        let mut comp = TileCompositor::new(64, 32);
        comp.reset(3);
        let png = minimal_png();
        let coord = TileCoord { z: 3, x: 0, y: 0 };
        assert!(comp.composite_tile(&coord, &png));
        assert!(comp.has_content, "content must be flagged after composite");
        assert_eq!(comp.tile_count(), 1);

        comp.demote_to_zoom(4);

        assert_eq!(comp.current_zoom, 4);
        assert_eq!(comp.tile_count(), 0, "old-zoom entries must be dropped");
        assert!(comp.has_content, "pixels must remain — no black flash");
        assert!(
            comp.buffer().iter().any(|&b| b != 0),
            "buffer must still carry the old-zoom pixels until overwritten",
        );
    }

    #[test]
    fn test_dirty_rect_covers_composited_tile() {
        // A fresh compositor that composites one tile must report a
        // non-empty dirty rect inside the buffer bounds.
        let mut comp = TileCompositor::new(64, 32);
        comp.reset(3);
        comp.mark_clean(); // drop the reset's full-buffer dirty mark
        assert!(!comp.dirty);

        let coord = TileCoord { z: 3, x: 0, y: 0 };
        assert!(comp.composite_tile(&coord, &minimal_png()));
        assert!(comp.dirty, "composite must set dirty");

        let (x0, y0, x1, y1) = comp.dirty_rect();
        assert!(x1 > x0, "dirty rect must have positive width");
        assert!(y1 > y0, "dirty rect must have positive height");
        assert!(x1 <= comp.width, "dirty rect clamped to buffer width");
        assert!(y1 <= comp.height, "dirty rect clamped to buffer height");
    }

    #[test]
    fn test_dirty_rect_expands_across_multiple_tiles() {
        let mut comp = TileCompositor::new(256, 128);
        comp.reset(3);
        comp.mark_clean();

        // Composite two tiles at opposite corners of zoom-3 grid.
        let a = TileCoord { z: 3, x: 0, y: 0 };
        let b = TileCoord { z: 3, x: 7, y: 7 };
        assert!(comp.composite_tile(&a, &minimal_png()));
        let (ax0, ay0, ax1, ay1) = comp.dirty_rect();
        assert!(comp.composite_tile(&b, &minimal_png()));
        let (cx0, cy0, cx1, cy1) = comp.dirty_rect();

        assert!(cx0 <= ax0 && cy0 <= ay0, "dirty rect min shrinks or equal after expand");
        assert!(cx1 >= ax1 && cy1 >= ay1, "dirty rect max grows or equal after expand");
    }

    #[test]
    fn test_mark_clean_resets_dirty_rect() {
        let mut comp = TileCompositor::new(64, 32);
        comp.reset(3);
        comp.mark_clean();
        let coord = TileCoord { z: 3, x: 0, y: 0 };
        assert!(comp.composite_tile(&coord, &minimal_png()));
        assert!(comp.dirty);

        comp.mark_clean();
        assert!(!comp.dirty);
        let (x0, y0, x1, y1) = comp.dirty_rect();
        assert_eq!((x0, y0, x1, y1), (0, 0, 0, 0),
            "mark_clean must zero the dirty rect");
    }

    #[test]
    fn test_reset_marks_full_buffer_dirty() {
        // A source change (reset) writes zeros everywhere, so the dirty
        // rect must cover the whole buffer — otherwise GPU keeps stale
        // pixels from the previous source around the rect we do upload.
        let mut comp = TileCompositor::new(64, 32);
        comp.reset(3);
        assert!(comp.dirty);
        let (x0, y0, x1, y1) = comp.dirty_rect();
        assert_eq!((x0, y0, x1, y1), (0, 0, 64, 32),
            "reset must mark the whole buffer dirty");
    }

    #[test]
    fn test_reset_clears_everything() {
        // Source-change path: reset() must blank the buffer and clear state.
        let mut comp = TileCompositor::new(64, 32);
        comp.reset(3);
        let coord = TileCoord { z: 3, x: 0, y: 0 };
        assert!(comp.composite_tile(&coord, &minimal_png()));

        comp.reset(5);

        assert_eq!(comp.current_zoom, 5);
        assert_eq!(comp.tile_count(), 0);
        assert!(!comp.has_content);
        assert!(comp.buffer().iter().all(|&b| b == 0));
    }
}
