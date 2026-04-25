// Items here are exercised by unit tests but only wired into the providers in
// commit 3c — keep the transitional dead-code lint quiet during the split.
#![allow(dead_code)]

// =============================================================================
// Self-consistency probe — catches servers that lie about 4326.
// =============================================================================
//
// Belt-and-suspenders for the case the structural CRS preference can't reach:
// a server that declares EPSG:4326 (but not 3857), or that we for some
// reason picked 4326 from, while actually delivering Mercator-shaped pixels.
//
// Method: fetch a tiny world-extent image in both the declared CRS AND in
// EPSG:3857. Bring both into the same equirect frame — the Mercator one
// through our trusted local reprojection, the declared one through whatever
// path the caller picked. If the declared CRS is honest, they agree to
// within resampling tolerance. If the declared CRS is a lie, the geometry
// is wrong and the comparison diverges.
//
// We compare in pixel-space (mean absolute RGB difference) rather than at
// landmarks. That's robust across very different layer styles (DWD's
// rainbow temperature scale, OSM's pale roads, GEBCO's blue bathymetry):
// the geometry of the data dominates the comparison either way.
// =============================================================================

use crate::provider::LayerImage;
use crate::wms::crs::Crs;
use crate::wms::reproject;

/// Outcome of a self-consistency check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// The declared response matches the Mercator reference once both are
    /// brought into the same frame. The server is honest about its CRS.
    Consistent,
    /// The declared response disagrees with the Mercator reference. The
    /// declared CRS label is wrong — the server is delivering data in a
    /// different projection than it claims (Terrestris-class lie).
    Inconsistent,
    /// One or both inputs were unusable, or the difference fell in the
    /// gray zone. Caller should default to trusting the declared CRS.
    Inconclusive,
}

/// Pixel-difference threshold below which we declare consistency.
/// Honest servers can still vary in JPEG quality and minor resampling, so
/// "below 25" is a generous tolerance.
const CONSISTENT_BELOW: f64 = 25.0;
/// Threshold above which we declare inconsistency. Wider gray zone reduces
/// false positives — wrongly switching is worse than wrongly trusting.
const INCONSISTENT_ABOVE: f64 = 70.0;

/// Probes the consistency of `declared_image` (returned under `declared_crs`)
/// against `reference_image` (returned under EPSG:3857, our trusted anchor).
/// Both must cover the whole globe at the canonical world bbox for their CRS.
pub fn check_consistency(
    declared_image: &LayerImage,
    declared_crs: Crs,
    reference_image: &LayerImage,
    target_w: u32,
    target_h: u32,
) -> ProbeVerdict {
    if declared_image.width < 2
        || declared_image.height < 2
        || reference_image.width < 2
        || reference_image.height < 2
    {
        return ProbeVerdict::Inconclusive;
    }

    // Bring both into the same equirect frame.
    let declared_eq = match reproject::to_equirect(
        declared_image,
        declared_crs,
        declared_crs.world_bbox(),
        target_w,
        target_h,
    ) {
        Ok(img) => img,
        Err(_) => return ProbeVerdict::Inconclusive,
    };
    let reference_eq = match reproject::to_equirect(
        reference_image,
        Crs::WebMercator,
        Crs::WebMercator.world_bbox(),
        target_w,
        target_h,
    ) {
        Ok(img) => img,
        Err(_) => return ProbeVerdict::Inconclusive,
    };

    let diff = mean_rgb_diff(&declared_eq, &reference_eq);
    log::debug!(
        "wms probe: mean RGB diff = {:.2} (consistent < {}, inconsistent > {})",
        diff,
        CONSISTENT_BELOW,
        INCONSISTENT_ABOVE,
    );

    if diff < CONSISTENT_BELOW {
        ProbeVerdict::Consistent
    } else if diff > INCONSISTENT_ABOVE {
        ProbeVerdict::Inconsistent
    } else {
        ProbeVerdict::Inconclusive
    }
}

/// Mean absolute RGB difference across non-transparent pixels.
///
/// Alpha is excluded for two reasons: (1) Mercator-reprojected images have
/// transparent polar caps (latitudes beyond ±85°), which would skew an
/// alpha-aware comparison; (2) the Terrestris bug shows up in pixel
/// geometry, not in transparency.
fn mean_rgb_diff(a: &LayerImage, b: &LayerImage) -> f64 {
    if a.width != b.width || a.height != b.height {
        return f64::MAX;
    }
    let n = (a.width as usize) * (a.height as usize);
    let mut sum: u64 = 0;
    let mut counted: u64 = 0;
    for i in 0..n {
        let off = i * 4;
        if a.rgba[off + 3] < 16 || b.rgba[off + 3] < 16 {
            continue; // skip transparent pixels (polar caps)
        }
        for c in 0..3 {
            let d = a.rgba[off + c] as i32 - b.rgba[off + c] as i32;
            sum += d.unsigned_abs() as u64;
        }
        counted += 3;
    }
    if counted == 0 {
        f64::MAX
    } else {
        sum as f64 / counted as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// High-contrast latitude band pattern: alternating black/white every
    /// 15° of latitude. The sharp transitions amplify any geometric
    /// misalignment, which is what the probe needs to detect.
    fn lat_band_brightness(lat_deg: f64) -> u8 {
        let band = (lat_deg.abs() / 15.0).floor() as u32;
        if band % 2 == 0 { 0 } else { 255 }
    }

    /// Equirect-domain image with brightness driven by lat-band pattern.
    /// In equirect, row y maps linearly to lat via `lat = 90 - y/h * 180`.
    fn equirect_with_lat_bands(w: u32, h: u32) -> LayerImage {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let lat = 90.0 - (y as f64 + 0.5) / h as f64 * 180.0;
            let v = lat_band_brightness(lat);
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = v;
                rgba[i + 1] = v;
                rgba[i + 2] = v;
                rgba[i + 3] = 255;
            }
        }
        LayerImage { rgba, width: w, height: h }
    }

    /// Mercator-domain image with the SAME lat-band pattern. Rows are placed
    /// at Mercator y, not lat, so band edges land at different rows than in
    /// the equirect version — but both encode the same content. Reprojecting
    /// either to equirect should produce visually identical bands.
    fn mercator_with_lat_bands(w: u32, h: u32) -> LayerImage {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for ym in 0..h {
            let merc_y_norm = 1.0 - 2.0 * (ym as f64 + 0.5) / h as f64;
            let merc_y = merc_y_norm * std::f64::consts::PI;
            let lat_deg = merc_y.sinh().atan().to_degrees();
            let v = lat_band_brightness(lat_deg);
            for x in 0..w {
                let i = ((ym * w + x) * 4) as usize;
                rgba[i] = v;
                rgba[i + 1] = v;
                rgba[i + 2] = v;
                rgba[i + 3] = 255;
            }
        }
        LayerImage { rgba, width: w, height: h }
    }

    #[test]
    fn honest_pair_compares_as_consistent() {
        // Honest case: declared and reference encode the same lat-band
        // content in their respective CRS frames. Reprojecting both to
        // equirect should yield essentially the same image.
        let declared = equirect_with_lat_bands(256, 128);
        let reference = mercator_with_lat_bands(256, 256);
        let v = check_consistency(&declared, Crs::EquirectWgs84, &reference, 256, 128);
        assert_eq!(v, ProbeVerdict::Consistent,
                   "honest equirect ≈ honest Mercator-then-reprojected");
    }

    #[test]
    fn lying_4326_pair_compares_as_inconsistent() {
        // Terrestris simulation: server declares EPSG:4326 but actually
        // returns Mercator-shaped pixels. The "reference" 3857 fetch returns
        // the same bytes (server's internal Mercator cache).
        //
        // - Declared treated as equirect: row y → lat = 90-y/h*180 (linear),
        //   sampling Mercator-encoded data at the wrong row. Bands at
        //   60°N appear in the output near row h*0.17.
        // - Reference (correctly reprojected from Mercator): bands at
        //   60°N appear at row h*0.17 too, BUT sampled from the Mercator-
        //   correct row of the source. With sharp band transitions, the
        //   lat-misalignment shows up as flipped pixel values along the
        //   transition strips.
        let mercator_bytes = mercator_with_lat_bands(256, 256);
        let declared_lying = LayerImage {
            rgba: mercator_bytes.rgba.clone(),
            width: mercator_bytes.width,
            height: mercator_bytes.height,
        };
        let v = check_consistency(&declared_lying, Crs::EquirectWgs84, &mercator_bytes, 256, 128);
        assert_eq!(v, ProbeVerdict::Inconsistent,
                   "lying 4326 (=Mercator content) must NOT match reprojected reference");
    }

    #[test]
    fn empty_image_returns_inconclusive() {
        let empty = LayerImage { rgba: vec![], width: 0, height: 0 };
        let normal = equirect_with_lat_bands(64, 32);
        assert_eq!(
            check_consistency(&empty, Crs::EquirectWgs84, &normal, 64, 32),
            ProbeVerdict::Inconclusive,
        );
        assert_eq!(
            check_consistency(&normal, Crs::EquirectWgs84, &empty, 64, 32),
            ProbeVerdict::Inconclusive,
        );
    }

    #[test]
    fn mean_rgb_diff_skips_transparent_pixels() {
        // Build two images that are identical in opaque pixels but differ
        // wildly in transparent ones — diff should be 0.
        let mut a = vec![0u8; 4 * 4 * 4];
        let mut b = vec![0u8; 4 * 4 * 4];
        for i in 0..16 {
            let off = i * 4;
            // First 8 pixels opaque and identical
            if i < 8 {
                a[off] = 100; a[off+1] = 100; a[off+2] = 100; a[off+3] = 255;
                b[off] = 100; b[off+1] = 100; b[off+2] = 100; b[off+3] = 255;
            } else {
                // Last 8 transparent and different
                a[off] = 0;   a[off+1] = 0;   a[off+2] = 0;   a[off+3] = 0;
                b[off] = 200; b[off+1] = 200; b[off+2] = 200; b[off+3] = 0;
            }
        }
        let img_a = LayerImage { rgba: a, width: 4, height: 4 };
        let img_b = LayerImage { rgba: b, width: 4, height: 4 };
        let diff = mean_rgb_diff(&img_a, &img_b);
        assert_eq!(diff, 0.0, "transparent pixels should not contribute to diff");
    }

    #[test]
    fn mean_rgb_diff_max_on_size_mismatch() {
        let small = LayerImage { rgba: vec![0u8; 16], width: 2, height: 2 };
        let large = LayerImage { rgba: vec![0u8; 64], width: 4, height: 4 };
        assert_eq!(mean_rgb_diff(&small, &large), f64::MAX);
    }
}
