// =============================================================================
// Zoom Level Mapping
// =============================================================================

use super::TileSource;

/// Computes an appropriate tile zoom level for a given tile source.
///
/// Uses a logarithmic mapping calibrated to Orbis' camera distance range
/// (`2.0..15.0`), combined with the source's per-source bias and the
/// user's optional zoom bias from settings.
///
/// Base mapping (at `recommended_zoom_bias = 0`):
/// ```text
/// distance=15 (max out)  -> base zoom 1
/// distance=8  (default)  -> base zoom 4
/// distance=4             -> base zoom 8
/// distance=2.5           -> base zoom 10
/// distance=2  (max in)   -> base zoom 11
/// ```
///
/// The source's `max_zoom` is always respected as a hard upper clamp, so
/// e.g. GIBS (max 9) never requests a z=11 tile.
pub fn level_for(source: &TileSource, distance: f32, user_bias: i32) -> u32 {
    // Guard against distance < 1.0 (should not happen but prevents NaN/negative log)
    let ratio = (15.0_f32 / distance.max(1.0)).max(1.0);
    let z_raw = ratio.log2() * 3.44 + 1.0;
    let z = (z_raw.round() as i32 + source.recommended_zoom_bias + user_bias)
        .clamp(0, source.max_zoom as i32);
    z as u32
}

/// Computes the visible lat/lon bounding box from camera yaw/pitch/distance.
///
/// Returns (lat_north, lat_south, lon_west, lon_east).
/// Uses a simplified FOV-based estimate.
pub fn visible_bounds(
    yaw: f32, pitch: f32, distance: f32, fov_y: f32, aspect: f32,
) -> (f64, f64, f64, f64) {
    // Orbis convention: yaw=pi/2 -> looking at 0E, pitch=0 -> equator
    // Camera sees the FAR side (inside-out), so center = opposite of eye
    let center_lon = (std::f32::consts::FRAC_PI_2 - yaw).to_degrees() as f64;
    let center_lat = (-pitch).to_degrees() as f64; // negated for inside-out

    // Angular extent based on distance and FOV
    // At distance=2 (close), ~30 deg visible. At distance=8, ~90 deg visible.
    let half_fov_deg = (fov_y / 2.0).to_degrees() as f64;
    let angular_radius = (half_fov_deg * distance as f64 / 2.0).min(90.0);
    let angular_width = angular_radius * aspect as f64;

    let lat_n = (center_lat + angular_radius).min(85.05);
    let lat_s = (center_lat - angular_radius).max(-85.05);
    let lon_w = center_lon - angular_width;
    let lon_e = center_lon + angular_width;

    // Normalize longitude to -180..180
    let normalize_lon = |l: f64| -> f64 {
        let mut l = l % 360.0;
        if l > 180.0 { l -= 360.0; }
        if l < -180.0 { l += 360.0; }
        l
    };

    (lat_n, lat_s, normalize_lon(lon_w), normalize_lon(lon_e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{builtin_tile_sources, TileSource};

    fn find_src(id: &str) -> TileSource {
        builtin_tile_sources().into_iter().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn test_level_for_sentinel2_close_zoom_is_detailed() {
        // Primary target: at max-in distance, Sentinel-2 must request z >= 11
        // so that ~150 m/px tiles replace the previous ~5 km/px z=6 tiles.
        let s2 = find_src("sentinel2");
        assert!(level_for(&s2, 2.0, 0) >= 11,
                "sentinel2 at distance=2 should be >= 11, got {}", level_for(&s2, 2.0, 0));
    }

    #[test]
    fn test_level_for_sentinel2_far_zoom_is_coarse() {
        // At max-out distance the whole globe is visible; tiles should be coarse.
        let s2 = find_src("sentinel2");
        assert!(level_for(&s2, 15.0, 0) <= 3,
                "sentinel2 at distance=15 should be <= 3, got {}", level_for(&s2, 15.0, 0));
    }

    #[test]
    fn test_level_for_osm_respects_range() {
        let osm = find_src("osm");
        let z_close = level_for(&osm, 2.0, 0);
        let z_far = level_for(&osm, 15.0, 0);
        assert!(z_close >= 10, "osm at distance=2 should be >= 10, got {}", z_close);
        assert!(z_close <= 19, "osm must not exceed max_zoom, got {}", z_close);
        assert!(z_far <= 3, "osm at distance=15 should be <= 3, got {}", z_far);
    }

    #[test]
    fn test_level_for_gibs_clamps_to_max() {
        // GIBS maxes at z=9 — at close distance must clamp, not overshoot.
        let gibs = find_src("gibs_truecolor");
        let z = level_for(&gibs, 2.0, 0);
        assert!(z <= 9, "gibs must respect max_zoom=9, got {}", z);
    }

    #[test]
    fn test_level_for_user_bias_shifts() {
        // User bias adds on top of the source bias.
        let s2 = find_src("sentinel2");
        let z_base = level_for(&s2, 4.0, 0) as i32;
        let z_plus = level_for(&s2, 4.0, 2) as i32;
        // Increase unless clamped by max_zoom
        assert!(z_plus >= z_base, "positive user bias should not decrease zoom");
        if z_base + 2 <= s2.max_zoom as i32 {
            assert_eq!(z_plus, z_base + 2, "bias should add when not clamped");
        }
    }
}
