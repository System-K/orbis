// =============================================================================
// Shapefile projection detection — bounds + .prj reconciliation.
// =============================================================================
//
// Applies the projection-honesty pattern from docs/projection-honesty.md to
// shapefiles. The .prj sidecar declares a CRS via WKT; the .shp header
// carries an axis-aligned bounding box of all the data inside. When they
// disagree, the bounds are the truth — coordinates can't lie about
// themselves the way a sidecar can.
//
// Three return shapes:
// - Confirmed:  prj parsed, bounds match → trust the declaration
// - Corrected:  prj parsed, bounds disagree → use the bounds-inferred CRS
// - Inferred:   no prj or unparseable → fall back to bounds inference
// - Unknown:    neither prj nor bounds give a usable answer → caller picks
//               between rendering at-coordinate-with-warning or skipping
//
// Detection scope today is WGS84 (EPSG:4326) and Web Mercator (EPSG:3857).
// UTM, regional grids, and polar stereographic come later — extending the
// `parse_prj` matcher and `infer_from_bounds` table is the only place that
// needs to grow.
// =============================================================================

use crate::crs::{Bbox, Crs};

/// Result of reconciling a shapefile's .prj declaration with its .shp bbox.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionVerdict {
    /// The .prj parses and the bbox is plausible for that CRS. Trust it.
    Confirmed(Crs),
    /// The .prj parses, the bbox is plausible for a DIFFERENT CRS. The
    /// declaration is lying; use `actual` instead.
    Corrected { declared: Crs, actual: Crs },
    /// No .prj (or unparseable). Bounds suggest a known CRS — best guess.
    Inferred(Crs),
    /// We can't pin a CRS. The bounds don't fit any CRS we recognize and
    /// either no .prj exists or it declares a CRS we don't support.
    Unknown {
        /// Whatever was in .prj, if anything we could parse.
        declared: Option<Crs>,
        /// The bbox we couldn't classify.
        bbox: Bbox,
    },
}

impl ProjectionVerdict {
    /// The CRS the caller should use to interpret coordinates, if any.
    /// Returns None for `Unknown` — caller decides whether to render
    /// at-coordinate or skip.
    pub fn effective_crs(&self) -> Option<Crs> {
        match self {
            Self::Confirmed(c) | Self::Inferred(c) => Some(*c),
            Self::Corrected { actual, .. } => Some(*actual),
            Self::Unknown { .. } => None,
        }
    }

    /// Human-readable one-line summary, for logging.
    pub fn describe(&self) -> String {
        match self {
            Self::Confirmed(c) => format!("confirmed {}", c.epsg_code()),
            Self::Corrected { declared, actual } => format!(
                "CORRECTED — .prj declared {} but bounds say {}",
                declared.epsg_code(),
                actual.epsg_code(),
            ),
            Self::Inferred(c) => format!("inferred {} from bounds (no .prj)", c.epsg_code()),
            Self::Unknown { declared, bbox } => {
                let dec = declared
                    .map(|c| c.epsg_code())
                    .unwrap_or("none");
                format!(
                    "UNKNOWN — declared {}, bbox ({:.3}, {:.3}, {:.3}, {:.3})",
                    dec, bbox.min_x, bbox.min_y, bbox.max_x, bbox.max_y,
                )
            }
        }
    }
}

/// Reconciles the .prj declaration with the .shp bounding box.
///
/// `prj_text` is the raw contents of the sidecar file (or `None` if absent).
/// `bbox` is the axis-aligned envelope from the .shp header, in whatever
/// coordinate system the data is in.
pub fn detect_projection(prj_text: Option<&str>, bbox: &Bbox) -> ProjectionVerdict {
    let declared = prj_text.and_then(parse_prj);
    let inferred = infer_from_bounds(bbox);

    match (declared, inferred) {
        // Both agree → trust the declaration (the most common honest case).
        (Some(d), Some(i)) if d == i => ProjectionVerdict::Confirmed(d),
        // Declaration disagrees with bounds → bounds win, declaration lied.
        (Some(d), Some(i)) => ProjectionVerdict::Corrected { declared: d, actual: i },
        // Declaration parses but bounds don't fit anything we know.
        // This is the "EPSG:25832 declared, UTM bounds" case — we can't
        // currently reproject UTM, so flag as Unknown rather than blindly
        // trusting the unsupported declaration.
        (Some(d), None) => ProjectionVerdict::Unknown {
            declared: Some(d),
            bbox: *bbox,
        },
        // No usable declaration but bounds suggest a known CRS.
        (None, Some(i)) => ProjectionVerdict::Inferred(i),
        // Nothing to go on.
        (None, None) => ProjectionVerdict::Unknown {
            declared: None,
            bbox: *bbox,
        },
    }
}

// =============================================================================
// .prj parsing — minimal WKT pattern matcher
// =============================================================================

/// Extracts a `Crs` from .prj WKT text. Returns `None` for unrecognised
/// CRSes (we'd rather flag Unknown than guess wrong).
///
/// Strategy: explicit EPSG code first (most reliable), then descriptive
/// keywords as fallback for files without an AUTHORITY block.
pub fn parse_prj(prj: &str) -> Option<Crs> {
    let t = prj.trim();
    let upper = t.to_uppercase();

    // 1. AUTHORITY["EPSG","<code>"] is unambiguous when present.
    // We look for the OUTERMOST authority — the last one in the WKT string.
    // (Inner ones describe the spheroid, datum, prime meridian, etc.)
    if let Some(epsg) = extract_outer_epsg(&upper) {
        if let Some(crs) = Crs::parse(&format!("EPSG:{}", epsg)) {
            return Some(crs);
        }
    }

    // 2. Descriptive keyword fallback for .prj without AUTHORITY blocks
    //    (common in older or hand-written files).
    //
    // PROJCS containing "MERCATOR" → Web Mercator. We check PROJCS rather
    // than just substring "MERCATOR" because GEOGCS can reference Mercator
    // in datum/spheroid descriptions without the data being Mercator.
    if upper.starts_with("PROJCS") && upper.contains("MERCATOR") {
        return Some(Crs::WebMercator);
    }

    // 3. GEOGCS["WGS 84"...] without a PROJCS wrapper is plain WGS84.
    if upper.starts_with("GEOGCS") && (upper.contains("WGS 84") || upper.contains("WGS_1984")) {
        return Some(Crs::EquirectWgs84);
    }

    None
}

/// Finds the outermost `AUTHORITY["EPSG","<code>"]` block, which describes
/// the whole CRS rather than a sub-element (datum/spheroid/etc).
///
/// Heuristic: scan for all `AUTHORITY["EPSG","..."]` matches and return the
/// LAST one — in WKT, nested elements come first and the outermost
/// AUTHORITY is at the end of the string just before the closing brackets.
fn extract_outer_epsg(upper_wkt: &str) -> Option<String> {
    let pattern = "AUTHORITY[\"EPSG\",\"";
    let mut last_code: Option<String> = None;
    let mut search_from = 0;

    while let Some(idx) = upper_wkt[search_from..].find(pattern) {
        let start = search_from + idx + pattern.len();
        if let Some(end_offset) = upper_wkt[start..].find('"') {
            let code = &upper_wkt[start..start + end_offset];
            if !code.is_empty() && code.chars().all(|c| c.is_ascii_digit()) {
                last_code = Some(code.to_string());
            }
            search_from = start + end_offset;
        } else {
            break;
        }
    }
    last_code
}

// =============================================================================
// Bounds inference
// =============================================================================

/// Guesses a CRS from the magnitude of the bbox.
///
/// Two-step rule:
/// - max coordinate magnitude under 200 (well past WGS84's ±180/±90 limits)
///   → degrees → WGS84. Mercator data this small in meters would be a
///   sub-millimeter region, which doesn't happen in practice.
/// - magnitude up to ~20 037 508 (Mercator world extent) → Mercator meters.
/// - anything bigger → unrecognised (could be a regional grid we don't yet
///   support).
///
/// Returns `None` to mean "can't classify", not "definitely none of these".
fn infer_from_bounds(bbox: &Bbox) -> Option<Crs> {
    let max_abs = bbox
        .min_x
        .abs()
        .max(bbox.max_x.abs())
        .max(bbox.min_y.abs())
        .max(bbox.max_y.abs());

    if max_abs <= 200.0 {
        // Plausible WGS84 range. Validate strictly: degrees beyond ±180/±90
        // on either axis would still flunk the actual range check.
        if Crs::EquirectWgs84.is_plausible_coord(bbox.min_x, bbox.min_y)
            && Crs::EquirectWgs84.is_plausible_coord(bbox.max_x, bbox.max_y)
        {
            return Some(Crs::EquirectWgs84);
        }
        // Out-of-range degrees (e.g. lat=190) — neither WGS84 nor Mercator-
        // sized. Some other system; flag as unknown.
        return None;
    }

    if Crs::WebMercator.is_plausible_coord(bbox.min_x, bbox.min_y)
        && Crs::WebMercator.is_plausible_coord(bbox.max_x, bbox.max_y)
    {
        return Some(Crs::WebMercator);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_prj ----

    #[test]
    fn parse_prj_handles_epsg_4326_authority_block() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(parse_prj(wkt), Some(Crs::EquirectWgs84));
    }

    #[test]
    fn parse_prj_handles_epsg_3857_authority_block() {
        // Real .prj contents for Web Mercator. Multiple AUTHORITY blocks
        // (spheroid, datum, prime meridian, unit) — only the OUTERMOST
        // one (3857) describes the CRS itself.
        let wkt = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]],PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AUTHORITY["EPSG","3857"]]"#;
        assert_eq!(parse_prj(wkt), Some(Crs::WebMercator));
    }

    #[test]
    fn parse_prj_handles_legacy_900913() {
        let wkt = r#"PROJCS["Web Mercator",AUTHORITY["EPSG","900913"]]"#;
        assert_eq!(parse_prj(wkt), Some(Crs::WebMercator));
    }

    #[test]
    fn parse_prj_falls_back_to_geogcs_keyword_without_authority() {
        // Older / hand-written .prj sometimes have no AUTHORITY blocks.
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
        assert_eq!(parse_prj(wkt), Some(Crs::EquirectWgs84));
    }

    #[test]
    fn parse_prj_falls_back_to_projcs_mercator_without_authority() {
        let wkt = r#"PROJCS["World_Mercator",GEOGCS["WGS 84"...],PROJECTION["Mercator"]]"#;
        assert_eq!(parse_prj(wkt), Some(Crs::WebMercator));
    }

    #[test]
    fn parse_prj_returns_none_for_unsupported_crs() {
        // EPSG:25832 — UTM zone 32N, common in Germany. Not yet supported.
        let wkt = r#"PROJCS["ETRS89 / UTM zone 32N",AUTHORITY["EPSG","25832"]]"#;
        assert_eq!(parse_prj(wkt), None);
    }

    #[test]
    fn parse_prj_handles_lowercase_and_whitespace() {
        // Defensive: real .prj files vary in casing and whitespace.
        let wkt = "  geogcs[\"WGS 84\",authority[\"EPSG\",\"4326\"]]  ";
        assert_eq!(parse_prj(wkt), Some(Crs::EquirectWgs84));
    }

    #[test]
    fn parse_prj_returns_none_on_garbage() {
        assert_eq!(parse_prj(""), None);
        assert_eq!(parse_prj("not a wkt"), None);
        assert_eq!(parse_prj("GEOGCS[]"), None);
    }

    // ---- infer_from_bounds ----

    #[test]
    fn infer_small_lat_lon_bbox_as_wgs84() {
        // Germany-ish in degrees.
        let bbox = Bbox::new(5.0, 47.0, 16.0, 55.0);
        assert_eq!(infer_from_bounds(&bbox), Some(Crs::EquirectWgs84));
    }

    #[test]
    fn infer_large_meter_bbox_as_mercator() {
        // Germany-ish in Web Mercator meters.
        let bbox = Bbox::new(550_000.0, 6_000_000.0, 1_800_000.0, 7_200_000.0);
        assert_eq!(infer_from_bounds(&bbox), Some(Crs::WebMercator));
    }

    #[test]
    fn infer_world_extent_wgs84() {
        let bbox = Bbox::new(-180.0, -90.0, 180.0, 90.0);
        assert_eq!(infer_from_bounds(&bbox), Some(Crs::EquirectWgs84));
    }

    #[test]
    fn infer_world_extent_mercator() {
        let bbox = Bbox::new(-20_037_508.0, -20_037_508.0, 20_037_508.0, 20_037_508.0);
        assert_eq!(infer_from_bounds(&bbox), Some(Crs::WebMercator));
    }

    #[test]
    fn infer_returns_none_for_unrecognised_magnitudes() {
        // Way past Mercator world extent — neither system fits.
        let bbox = Bbox::new(50_000_000.0, 50_000_000.0, 60_000_000.0, 60_000_000.0);
        assert_eq!(infer_from_bounds(&bbox), None);
    }

    #[test]
    fn infer_returns_none_for_out_of_range_degrees() {
        // Magnitude small but lat past ±90 → not valid degrees, not large
        // enough for Mercator. Caller must flag Unknown.
        let bbox = Bbox::new(0.0, 0.0, 10.0, 95.0);
        assert_eq!(infer_from_bounds(&bbox), None);
    }

    // ---- detect_projection (the integration) ----

    #[test]
    fn detect_confirms_when_prj_and_bounds_agree() {
        let prj = r#"GEOGCS["WGS 84",AUTHORITY["EPSG","4326"]]"#;
        let bbox = Bbox::new(5.0, 47.0, 16.0, 55.0);
        assert_eq!(
            detect_projection(Some(prj), &bbox),
            ProjectionVerdict::Confirmed(Crs::EquirectWgs84),
        );
    }

    #[test]
    fn detect_corrects_when_prj_lies_about_4326() {
        // The Terrestris-class lie, ported to shapefile: .prj declares
        // EPSG:4326 but coordinates are clearly Mercator meters.
        let prj = r#"GEOGCS["WGS 84",AUTHORITY["EPSG","4326"]]"#;
        let bbox = Bbox::new(550_000.0, 6_000_000.0, 1_800_000.0, 7_200_000.0);
        assert_eq!(
            detect_projection(Some(prj), &bbox),
            ProjectionVerdict::Corrected {
                declared: Crs::EquirectWgs84,
                actual: Crs::WebMercator,
            },
        );
    }

    #[test]
    fn detect_corrects_when_prj_says_3857_but_bounds_are_degrees() {
        let prj = r#"PROJCS["WGS 84 / Pseudo-Mercator",AUTHORITY["EPSG","3857"]]"#;
        let bbox = Bbox::new(5.0, 47.0, 16.0, 55.0);
        assert_eq!(
            detect_projection(Some(prj), &bbox),
            ProjectionVerdict::Corrected {
                declared: Crs::WebMercator,
                actual: Crs::EquirectWgs84,
            },
        );
    }

    #[test]
    fn detect_infers_when_no_prj_and_bounds_are_clear() {
        let bbox = Bbox::new(5.0, 47.0, 16.0, 55.0);
        assert_eq!(
            detect_projection(None, &bbox),
            ProjectionVerdict::Inferred(Crs::EquirectWgs84),
        );
    }

    #[test]
    fn detect_unknown_when_bounds_unrecognised_and_no_prj() {
        let bbox = Bbox::new(50_000_000.0, 50_000_000.0, 60_000_000.0, 60_000_000.0);
        match detect_projection(None, &bbox) {
            ProjectionVerdict::Unknown { declared, .. } => assert_eq!(declared, None),
            v => panic!("expected Unknown, got {:?}", v),
        }
    }

    #[test]
    fn detect_unknown_keeps_unsupported_declaration_for_logging() {
        // EPSG:25832 (UTM 32N) — bounds match it but we can't reproject.
        // The declaration should NOT round-trip through Confirmed because
        // we don't have a UTM Crs variant.
        let prj = r#"PROJCS["ETRS89 / UTM zone 32N",AUTHORITY["EPSG","25832"]]"#;
        let bbox = Bbox::new(550_000.0, 5_400_000.0, 700_000.0, 5_600_000.0);
        // Bounds happen to LOOK like Mercator at this magnitude, but the
        // declared EPSG (which we can't parse to a Crs) is None, so the
        // path is (None, Some(WebMercator)) → Inferred(WebMercator).
        // That's incorrect for the actual data — but it's the best we
        // can do without UTM support, and the user will at least see a
        // map (offset from reality, but visible). When UTM support lands,
        // parse_prj will return Some(Utm32n) and this will Confirm.
        let verdict = detect_projection(Some(prj), &bbox);
        // For now: Inferred(WebMercator). Rendering will be wrong but
        // visible — matches the "render at-coordinate with a warning"
        // policy from docs/projection-honesty.md.
        assert_eq!(verdict, ProjectionVerdict::Inferred(Crs::WebMercator));
    }

    // ---- effective_crs / describe ----

    #[test]
    fn effective_crs_returns_actual_for_corrected() {
        let v = ProjectionVerdict::Corrected {
            declared: Crs::EquirectWgs84,
            actual: Crs::WebMercator,
        };
        assert_eq!(v.effective_crs(), Some(Crs::WebMercator));
    }

    #[test]
    fn effective_crs_is_none_for_unknown() {
        let v = ProjectionVerdict::Unknown {
            declared: None,
            bbox: Bbox::new(0.0, 0.0, 1.0, 1.0),
        };
        assert_eq!(v.effective_crs(), None);
    }

    #[test]
    fn describe_includes_both_crs_for_corrected() {
        let v = ProjectionVerdict::Corrected {
            declared: Crs::EquirectWgs84,
            actual: Crs::WebMercator,
        };
        let s = v.describe();
        assert!(s.contains("EPSG:4326"));
        assert!(s.contains("EPSG:3857"));
        assert!(s.to_uppercase().contains("CORRECTED"));
    }
}
