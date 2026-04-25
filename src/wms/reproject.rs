// =============================================================================
// Generic WMS reprojection — any supported CRS → equirectangular.
// =============================================================================
//
// One loop, one dispatch: for each output pixel we know the lat/lon, we ask
// the source CRS where that lands in the source image, and bilinear-sample.
//
// This replaces the Mercator-specific inverse-formula loop from the old
// wms.rs. Adding a new CRS means extending `Crs::latlon_to_fracxy` in
// crs.rs — the loop here does not change.
// =============================================================================

use crate::provider::LayerImage;
use crate::crs::{Bbox, Crs};

/// Reprojects `src` (whose pixels cover `src_bbox` in `src_crs` units) into an
/// equirectangular WGS84 image of size `out_w × out_h` covering the whole globe.
///
/// Pixels outside `src_bbox` or outside the source CRS's valid domain are left
/// fully transparent.
pub fn to_equirect(
    src: &LayerImage,
    src_crs: Crs,
    src_bbox: Bbox,
    out_w: u32,
    out_h: u32,
) -> Result<LayerImage, String> {
    if src.width == 0 || src.height == 0 {
        return Err("reproject: source image is empty".to_string());
    }
    if (src.rgba.len() as u32) < src.width * src.height * 4 {
        return Err("reproject: source rgba buffer too small".to_string());
    }

    let src_w = src.width as usize;
    let src_h = src.height as usize;
    let ow = out_w as usize;
    let oh = out_h as usize;

    let mut out_rgba = vec![0u8; ow * oh * 4];

    for oy in 0..oh {
        // Use pixel centers for better interpolation: row 0 center is at y = 0.5 / oh.
        let lat = 90.0 - (oy as f64 + 0.5) / oh as f64 * 180.0;

        for ox in 0..ow {
            let lon = -180.0 + (ox as f64 + 0.5) / ow as f64 * 360.0;

            let Some((fx, fy)) = src_crs.latlon_to_fracxy(lat, lon, &src_bbox) else {
                continue; // transparent (buffer already zeroed)
            };

            let sx = fx * (src_w - 1) as f64;
            let sy = fy * (src_h - 1) as f64;

            // Clamp to valid bilinear-sample range.
            let x0 = (sx.floor() as usize).min(src_w - 2);
            let y0 = (sy.floor() as usize).min(src_h - 2);
            let x1 = x0 + 1;
            let y1 = y0 + 1;
            let tx = (sx - x0 as f64) as f32;
            let ty = (sy - y0 as f64) as f32;

            let i00 = (y0 * src_w + x0) * 4;
            let i10 = (y0 * src_w + x1) * 4;
            let i01 = (y1 * src_w + x0) * 4;
            let i11 = (y1 * src_w + x1) * 4;
            let out_idx = (oy * ow + ox) * 4;

            for c in 0..4 {
                let v00 = src.rgba[i00 + c] as f32;
                let v10 = src.rgba[i10 + c] as f32;
                let v01 = src.rgba[i01 + c] as f32;
                let v11 = src.rgba[i11 + c] as f32;
                let top = v00 + (v10 - v00) * tx;
                let bot = v01 + (v11 - v01) * tx;
                let v = top + (bot - top) * ty;
                out_rgba[out_idx + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    Ok(LayerImage {
        rgba: out_rgba,
        width: out_w,
        height: out_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small test image where each pixel stores its (col, row) in the
    /// R and G channels. Lets us assert where the reproject loop sampled.
    fn synthetic_image(w: u32, h: u32) -> LayerImage {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = (x * 255 / w.max(1)) as u8; // R encodes x
                rgba[i + 1] = (y * 255 / h.max(1)) as u8; // G encodes y
                rgba[i + 2] = 0;
                rgba[i + 3] = 255;
            }
        }
        LayerImage { rgba, width: w, height: h }
    }

    #[test]
    fn equirect_identity_roundtrips() {
        // An equirect source covering the whole globe, reprojected to equirect
        // at the same dims, should pass through unchanged (±rounding).
        let src = synthetic_image(64, 32);
        let out = to_equirect(&src, Crs::EquirectWgs84, Crs::EquirectWgs84.world_bbox(), 64, 32).unwrap();
        assert_eq!(out.width, 64);
        assert_eq!(out.height, 32);
        // Pick a few pixels — R/G channels should roughly match.
        for &(x, y) in &[(10u32, 8u32), (30, 16), (60, 28)] {
            let idx_src = ((y * 64 + x) * 4) as usize;
            let idx_out = ((y * 64 + x) * 4) as usize;
            let dr = src.rgba[idx_src] as i32 - out.rgba[idx_out] as i32;
            let dg = src.rgba[idx_src + 1] as i32 - out.rgba[idx_out + 1] as i32;
            assert!(dr.abs() <= 8, "R mismatch at ({x},{y}): {dr}");
            assert!(dg.abs() <= 8, "G mismatch at ({x},{y}): {dg}");
        }
    }

    #[test]
    fn mercator_to_equirect_stretches_poles() {
        // Start with a Mercator source image where row 0 is +85° and row h-1 is -85°.
        // In the output (equirect), the equator (row oh/2) must sample the
        // source's middle row, and high-latitude output rows must sample
        // rows close to the source top/bottom.
        let src = synthetic_image(64, 64);
        let out = to_equirect(&src, Crs::WebMercator, Crs::WebMercator.world_bbox(), 64, 32).unwrap();

        // Equator in output is row 16 (out of 32). Source middle row is 32 (out of 64).
        // The G channel at output row 16 should be ≈ 32 * 255 / 64 = 127.
        let g_equator = out.rgba[((16 * 64 + 32) * 4 + 1) as usize] as i32;
        assert!((g_equator - 127).abs() < 10, "equator G = {g_equator}, expected ~127");

        // Output row 0 = +90°, which is OUTSIDE the Mercator limit (±85°),
        // so it must be transparent.
        let a_pole = out.rgba[(0 * 64 * 4 + 3) as usize];
        assert_eq!(a_pole, 0, "top row should be transparent beyond Mercator limit");

        // Output at 60°N: equirect row ≈ (90-60)/180 * 32 = 5.33.
        // Sampled from Mercator row where fy ≈ 0.29 → source row ≈ 0.29 * 63 = 18.
        // G ≈ 18 * 255 / 64 ≈ 72. It MUST NOT equal 40 (which is what equirect
        // pass-through would give at row 5/6). That contrast is the whole
        // point of this reprojection.
        let g_60n = out.rgba[((5 * 64 + 32) * 4 + 1) as usize] as i32;
        assert!(g_60n > 50, "60°N output should sample lower (higher-number) source row, got G={g_60n}");
    }

    #[test]
    fn empty_source_errors_cleanly() {
        let src = LayerImage { rgba: vec![], width: 0, height: 0 };
        assert!(to_equirect(&src, Crs::EquirectWgs84, Crs::EquirectWgs84.world_bbox(), 8, 4).is_err());
    }

    #[test]
    fn truncated_buffer_errors_cleanly() {
        let src = LayerImage { rgba: vec![0u8; 10], width: 8, height: 4 };
        assert!(to_equirect(&src, Crs::EquirectWgs84, Crs::EquirectWgs84.world_bbox(), 8, 4).is_err());
    }
}
