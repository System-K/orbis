// =============================================================================
// CRS registry — coordinate reference systems used by WMS servers.
// =============================================================================
//
// Generic reprojection needs one thing from each CRS: given (lat, lon), what
// fractional image-pixel coordinates (fx, fy) does that point land at, in a
// source image whose bbox is known in the CRS's native units?
//
// Today we support EPSG:4326 (equirectangular WGS84) and EPSG:3857 (Web
// Mercator). Adding a new CRS means adding a variant plus its forward
// transform — no changes to the reproject loop or the providers.
// =============================================================================

/// Axis-aligned bounding box in a CRS's native units.
///
/// For EPSG:4326 this is degrees (min_x = west lon, min_y = south lat).
/// For EPSG:3857 this is meters in Web Mercator (spherical) projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bbox {
    pub const fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self { min_x, min_y, max_x, max_y }
    }
}

/// A coordinate reference system Orbis can consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Crs {
    /// EPSG:4326 — geographic lat/lon on WGS84, plate-carrée when rendered 2:1.
    EquirectWgs84,
    /// EPSG:3857 — spherical Web Mercator, the tile grid used by OSM/Google/Bing.
    WebMercator,
}

/// Web Mercator's polar cutoff — the latitude at which the projection goes to infinity.
/// ≈ 85.05112878°, i.e. `atan(sinh(π))` in radians.
const MERCATOR_LAT_LIMIT_RAD: f64 = 1.4844222297453324;

/// Spherical-Mercator earth radius (the constant WMS servers use).
const MERCATOR_EARTH_RADIUS: f64 = 6_378_137.0;

impl Crs {
    /// Returns the canonical EPSG string (for building WMS URLs).
    #[allow(dead_code)] // consumed by capabilities + provider wiring in later commits
    pub fn epsg_code(&self) -> &'static str {
        match self {
            Crs::EquirectWgs84 => "EPSG:4326",
            Crs::WebMercator => "EPSG:3857",
        }
    }

    /// Parses an EPSG string (case-insensitive, accepts a few aliases).
    /// Returns `None` for unrecognised CRSes.
    #[allow(dead_code)] // consumed by capabilities parser (commit 2)
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_uppercase();
        match s.as_str() {
            "EPSG:4326" | "CRS:84" | "OGC:CRS84" => Some(Crs::EquirectWgs84),
            "EPSG:3857" | "EPSG:900913" | "EPSG:102100" | "EPSG:102113" => Some(Crs::WebMercator),
            _ => None,
        }
    }

    /// The default world-covering bbox in this CRS's native units.
    pub fn world_bbox(&self) -> Bbox {
        match self {
            Crs::EquirectWgs84 => Bbox::new(-180.0, -90.0, 180.0, 90.0),
            Crs::WebMercator => {
                // EPSG:3857 world extent, in meters.
                const EXTENT: f64 = 20_037_508.342_789_244;
                Bbox::new(-EXTENT, -EXTENT, EXTENT, EXTENT)
            }
        }
    }

    /// Forward transform: geographic (lat°, lon°) → fractional image pixel coords.
    ///
    /// The image is assumed to cover `src_bbox` in this CRS's units, with
    /// (fx=0, fy=0) at the top-left corner and (fx=1, fy=1) at bottom-right.
    ///
    /// Returns `None` when the point falls outside the CRS's valid domain
    /// (e.g. polar cutoff for Web Mercator) or outside `src_bbox`.
    pub fn latlon_to_fracxy(&self, lat_deg: f64, lon_deg: f64, src_bbox: &Bbox) -> Option<(f64, f64)> {
        match self {
            Crs::EquirectWgs84 => {
                let x = lon_deg;
                let y = lat_deg;
                Self::bbox_frac(x, y, src_bbox)
            }
            Crs::WebMercator => {
                let lat_rad = lat_deg.to_radians();
                if lat_rad.abs() > MERCATOR_LAT_LIMIT_RAD {
                    return None;
                }
                let x = MERCATOR_EARTH_RADIUS * lon_deg.to_radians();
                // Mercator forward: y = R * ln(tan(π/4 + φ/2))
                let y = MERCATOR_EARTH_RADIUS
                    * (std::f64::consts::FRAC_PI_4 + lat_rad / 2.0).tan().ln();
                Self::bbox_frac(x, y, src_bbox)
            }
        }
    }

    fn bbox_frac(x: f64, y: f64, b: &Bbox) -> Option<(f64, f64)> {
        if x < b.min_x || x > b.max_x || y < b.min_y || y > b.max_y {
            return None;
        }
        let fx = (x - b.min_x) / (b.max_x - b.min_x);
        // y axis is flipped (top of image = max_y).
        let fy = (b.max_y - y) / (b.max_y - b.min_y);
        Some((fx, fy))
    }

    /// Inverse transform: CRS-native (x, y) → geographic (lat°, lon°).
    ///
    /// This is the direction needed for **vector** reprojection: each point
    /// in the source data is in CRS units (meters for Mercator, degrees for
    /// EPSG:4326). To draw it on a WGS84 globe, convert each point through
    /// this function.
    ///
    /// (The pixel-sampling pipeline in `wms::reproject` uses the *forward*
    /// transform `latlon_to_fracxy` — different direction, different use.)
    ///
    /// Returns `None` for points outside the CRS's valid domain (e.g. Web
    /// Mercator past the polar cutoff).
    pub fn xy_to_latlon(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        match self {
            Crs::EquirectWgs84 => {
                // x is lon, y is lat — identity.
                if x.abs() > 180.0 || y.abs() > 90.0 {
                    return None;
                }
                Some((y, x))
            }
            Crs::WebMercator => {
                // Inverse Mercator: φ = atan(sinh(y / R)), λ = x / R
                let lon_rad = x / MERCATOR_EARTH_RADIUS;
                let lat_rad = (y / MERCATOR_EARTH_RADIUS).sinh().atan();
                if lat_rad.is_nan() || lon_rad.is_nan() {
                    return None;
                }
                let lat_deg = lat_rad.to_degrees();
                let lon_deg = lon_rad.to_degrees();
                if lat_deg.abs() > 90.0 || lon_deg.abs() > 180.0 {
                    return None;
                }
                Some((lat_deg, lon_deg))
            }
        }
    }

    /// Returns true when `(x, y)` falls inside the typical inhabited range
    /// of this CRS's coordinates. Used by projection detection to check
    /// whether a declared CRS is consistent with the data's actual numbers.
    ///
    /// "Plausible" is intentionally loose — we want to catch obvious lies
    /// (lon=600000 in a "WGS84" file), not flag genuine edge cases (a
    /// research dataset at the South Pole). The bounds are roughly the
    /// CRS's valid mathematical domain, not the human-occupied subset.
    pub fn is_plausible_coord(&self, x: f64, y: f64) -> bool {
        match self {
            Crs::EquirectWgs84 => x.abs() <= 180.0 && y.abs() <= 90.0,
            Crs::WebMercator => {
                // World Mercator extent (±20037508 m). Allow slight slop.
                const LIMIT: f64 = 20_100_000.0;
                x.abs() <= LIMIT && y.abs() <= LIMIT
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_handles_case_and_aliases() {
        assert_eq!(Crs::parse("EPSG:4326"), Some(Crs::EquirectWgs84));
        assert_eq!(Crs::parse("epsg:4326"), Some(Crs::EquirectWgs84));
        assert_eq!(Crs::parse("CRS:84"), Some(Crs::EquirectWgs84));
        assert_eq!(Crs::parse("EPSG:3857"), Some(Crs::WebMercator));
        assert_eq!(Crs::parse("EPSG:900913"), Some(Crs::WebMercator));
        assert_eq!(Crs::parse("EPSG:25832"), None);
    }

    #[test]
    fn equirect_corners_map_to_bbox_corners() {
        let bbox = Crs::EquirectWgs84.world_bbox();
        // Top-left corner: lat=+90, lon=-180 → (0, 0)
        let (fx, fy) = Crs::EquirectWgs84.latlon_to_fracxy(90.0, -180.0, &bbox).unwrap();
        assert!((fx - 0.0).abs() < 1e-12 && (fy - 0.0).abs() < 1e-12);
        // Bottom-right corner: lat=-90, lon=+180 → (1, 1)
        let (fx, fy) = Crs::EquirectWgs84.latlon_to_fracxy(-90.0, 180.0, &bbox).unwrap();
        assert!((fx - 1.0).abs() < 1e-12 && (fy - 1.0).abs() < 1e-12);
    }

    #[test]
    fn equirect_equator_is_mid_row() {
        let bbox = Crs::EquirectWgs84.world_bbox();
        let (_, fy) = Crs::EquirectWgs84.latlon_to_fracxy(0.0, 0.0, &bbox).unwrap();
        assert!((fy - 0.5).abs() < 1e-12);
    }

    #[test]
    fn mercator_equator_is_mid_row() {
        let bbox = Crs::WebMercator.world_bbox();
        let (fx, fy) = Crs::WebMercator.latlon_to_fracxy(0.0, 0.0, &bbox).unwrap();
        assert!((fx - 0.5).abs() < 1e-12);
        assert!((fy - 0.5).abs() < 1e-12);
    }

    #[test]
    fn mercator_rejects_beyond_pole_cutoff() {
        let bbox = Crs::WebMercator.world_bbox();
        // Past the Mercator limit — projection blows up, must return None.
        assert!(Crs::WebMercator.latlon_to_fracxy(87.0, 0.0, &bbox).is_none());
        assert!(Crs::WebMercator.latlon_to_fracxy(-87.0, 0.0, &bbox).is_none());
        // Right at the limit is still acceptable.
        assert!(Crs::WebMercator.latlon_to_fracxy(85.0, 0.0, &bbox).is_some());
    }

    #[test]
    fn mercator_vs_equirect_row_differs_at_60n() {
        // Classic Mercator stretch: 60° N is NOT at fy=1/6 (as in equirect),
        // it's significantly lower in the image because Mercator stretches
        // higher latitudes upward.
        let bbox_m = Crs::WebMercator.world_bbox();
        let bbox_e = Crs::EquirectWgs84.world_bbox();
        let (_, fy_m) = Crs::WebMercator.latlon_to_fracxy(60.0, 0.0, &bbox_m).unwrap();
        let (_, fy_e) = Crs::EquirectWgs84.latlon_to_fracxy(60.0, 0.0, &bbox_e).unwrap();
        // Equirect: 60°N is at fy = (90-60)/180 = 0.1667
        assert!((fy_e - 1.0 / 6.0).abs() < 1e-6);
        // Mercator: 60°N is much higher in fy (further from top).
        // Known value: ln(tan(π/4 + 30°)) / π ≈ 0.42 from center → fy ≈ 0.29.
        assert!(fy_m > 0.25 && fy_m < 0.35, "got fy_m = {fy_m}");
        // And the two must disagree — this is the Terrestris bug visualised.
        assert!((fy_m - fy_e).abs() > 0.1);
    }

    #[test]
    fn bbox_out_of_range_returns_none() {
        let small_bbox = Bbox::new(0.0, 0.0, 10.0, 10.0);
        // (lat=50, lon=50) is outside
        assert!(Crs::EquirectWgs84.latlon_to_fracxy(50.0, 50.0, &small_bbox).is_none());
    }

    // -- Inverse transform (xy_to_latlon) -- needed for vector reprojection.

    #[test]
    fn equirect_inverse_is_identity_swap() {
        // In equirect (WGS84), x=lon, y=lat. xy_to_latlon returns (lat, lon).
        let (lat, lon) = Crs::EquirectWgs84.xy_to_latlon(10.0, 50.0).unwrap();
        assert!((lat - 50.0).abs() < 1e-12);
        assert!((lon - 10.0).abs() < 1e-12);
    }

    #[test]
    fn equirect_inverse_rejects_out_of_range() {
        // Anything past the valid lat/lon domain → None.
        assert!(Crs::EquirectWgs84.xy_to_latlon(200.0, 0.0).is_none());
        assert!(Crs::EquirectWgs84.xy_to_latlon(0.0, 100.0).is_none());
    }

    #[test]
    fn mercator_inverse_recovers_origin() {
        // (0, 0) in Mercator meters is (lat=0, lon=0).
        let (lat, lon) = Crs::WebMercator.xy_to_latlon(0.0, 0.0).unwrap();
        assert!((lat).abs() < 1e-12);
        assert!((lon).abs() < 1e-12);
    }

    #[test]
    fn mercator_inverse_known_point_berlin() {
        // Berlin: ~13.4°E, 52.5°N. Mercator forward gives ~(1492000, 6894000).
        // Inverse should recover the lat/lon within reasonable tolerance.
        let x = 1_492_000.0;
        let y = 6_894_000.0;
        let (lat, lon) = Crs::WebMercator.xy_to_latlon(x, y).unwrap();
        assert!((lat - 52.5).abs() < 0.1, "lat = {lat}");
        assert!((lon - 13.4).abs() < 0.1, "lon = {lon}");
    }

    #[test]
    fn mercator_inverse_round_trips_through_forward() {
        // Pick a point, forward-transform, inverse-transform, expect to land
        // back on the original within numerical tolerance.
        let bbox = Crs::WebMercator.world_bbox();
        let original_lat = 45.0;
        let original_lon = -73.0;
        let (fx, fy) = Crs::WebMercator
            .latlon_to_fracxy(original_lat, original_lon, &bbox)
            .unwrap();
        // Convert back to absolute Mercator meters from fractional bbox.
        let x = bbox.min_x + fx * (bbox.max_x - bbox.min_x);
        let y = bbox.max_y - fy * (bbox.max_y - bbox.min_y);
        let (lat, lon) = Crs::WebMercator.xy_to_latlon(x, y).unwrap();
        assert!((lat - original_lat).abs() < 1e-9);
        assert!((lon - original_lon).abs() < 1e-9);
    }

    // -- is_plausible_coord -- structural detection of mislabeled data.

    #[test]
    fn plausible_wgs84_accepts_lat_lon_range() {
        assert!(Crs::EquirectWgs84.is_plausible_coord(13.4, 52.5)); // Berlin
        assert!(Crs::EquirectWgs84.is_plausible_coord(-180.0, -90.0)); // edges
        assert!(Crs::EquirectWgs84.is_plausible_coord(180.0, 90.0));
    }

    #[test]
    fn plausible_wgs84_rejects_projected_meters() {
        // Classic Terrestris-class lie: file says EPSG:4326 but coords are
        // in meters. Easy structural check.
        assert!(!Crs::EquirectWgs84.is_plausible_coord(600_000.0, 5_500_000.0));
        assert!(!Crs::EquirectWgs84.is_plausible_coord(1_492_000.0, 6_894_000.0));
    }

    #[test]
    fn plausible_mercator_accepts_world_extent() {
        // Mercator world extent ≈ ±20037508 m.
        assert!(Crs::WebMercator.is_plausible_coord(0.0, 0.0));
        assert!(Crs::WebMercator.is_plausible_coord(20_037_000.0, 20_037_000.0));
        assert!(Crs::WebMercator.is_plausible_coord(-20_037_000.0, -20_037_000.0));
    }

    #[test]
    fn plausible_mercator_rejects_lat_lon_or_far_outside() {
        // Mercator coords aren't <1 — degree values can't be Mercator meters.
        // A coord like (500_000_000, 500_000_000) is way past the world extent.
        assert!(!Crs::WebMercator.is_plausible_coord(500_000_000.0, 500_000_000.0));
    }
}
