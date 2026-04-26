// =============================================================================
// Orbis — Shapefile Loader (Esri Shapefile / .shp + .dbf + .prj)
// =============================================================================
// Drop-target for shapefile bundles: the user drags a .shp into Orbis and
// we produce a `GeoLayer` ready for rendering. The .prj sidecar is
// reconciled against the .shp bounding box via `projection::detect_projection`
// before we trust either, applying the projection-honesty pattern from
// docs/projection-honesty.md.
//
// Module layout:
// - projection.rs     pure CRS detection (no shapefile dep)
// - (mod.rs)          loader: file I/O, geometry → GeoFeature, reproject
//
// Supported geometry types: Point, Polyline, Polygon, Multipoint
// (plus their Z and M variants — the extra dimensions are dropped).
// Multipatch is skipped (TIN/triangle meshes don't fit our 2D feature
// model).
// =============================================================================

pub mod projection;

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value as JsonValue;
use shapefile::Shape;

use crate::crs::Crs;
use crate::geojson::{FeatureStyle, GeoCoord, GeoFeature, GeoGeometry, GeoLayer};
use projection::ProjectionVerdict;

// =============================================================================
// Public API
// =============================================================================

/// Loads a shapefile bundle (.shp + sidecars) into a `GeoLayer`.
///
/// Reads the .prj sidecar if present, reconciles it with the .shp header's
/// bounding box, and reprojects geometries to WGS84 if the source CRS is
/// not already WGS84. Attribute records from the .dbf file are preserved
/// in each feature's `properties` map.
pub fn load_shapefile(path: &Path) -> Result<GeoLayer, String> {
    let layer_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("shapefile")
        .to_string();

    // 1. Read .prj sidecar (best-effort; absence is normal).
    let prj_text = std::fs::read_to_string(path.with_extension("prj")).ok();

    // 2. Open the .shp + .dbf via the shapefile crate.
    let mut reader = shapefile::Reader::from_path(path)
        .map_err(|e| format!("Failed to open shapefile '{}': {}", path.display(), e))?;

    // 3. Header bbox — the structural ground truth for projection detection.
    let hdr = reader.header();
    let bbox = crate::crs::Bbox::new(hdr.bbox.min.x, hdr.bbox.min.y, hdr.bbox.max.x, hdr.bbox.max.y);

    // 4. Detect projection (the whole point of this whole exercise).
    let verdict = projection::detect_projection(prj_text.as_deref(), &bbox);
    log::info!(
        "Shapefile '{}': projection {}",
        layer_name,
        verdict.describe(),
    );

    // 5. Decide what CRS to TREAT the data as. None → render-at-coordinate
    //    fallback (assume WGS84, log loudly).
    let source_crs = match verdict.effective_crs() {
        Some(c) => c,
        None => {
            log::warn!(
                "Shapefile '{}': no usable CRS detected, rendering at-coordinate \
                 (data positions will likely be wrong; provide a .prj or \
                 reproject the file to EPSG:4326)",
                layer_name,
            );
            Crs::EquirectWgs84
        }
    };
    if let ProjectionVerdict::Corrected { declared, actual } = &verdict {
        log::warn!(
            "Shapefile '{}': .prj declared {} but coordinates are in {} \
             — using {} (the .prj is wrong)",
            layer_name,
            declared.epsg_code(),
            actual.epsg_code(),
            actual.epsg_code(),
        );
    }

    // 6. Iterate shapes + records, convert, collect.
    let mut layer = GeoLayer::new(&layer_name);
    let mut skipped = 0usize;
    let mut dropped_points = 0usize;
    for result in reader.iter_shapes_and_records() {
        let (shape, record) = result.map_err(|e| {
            format!("Error reading shapefile record in '{}': {}", layer_name, e)
        })?;
        let properties = record_to_properties(&record);
        match shape_to_features(shape, source_crs, &properties) {
            ShapeOutcome::Features(features) => {
                layer.features.extend(features);
            }
            ShapeOutcome::Skipped => skipped += 1,
            ShapeOutcome::PartiallyDropped { features, dropped } => {
                layer.features.extend(features);
                dropped_points += dropped;
            }
        }
    }

    if skipped > 0 {
        log::warn!(
            "Shapefile '{}': skipped {} unsupported shape(s) (Multipatch / Null)",
            layer_name,
            skipped,
        );
    }
    if dropped_points > 0 {
        log::warn!(
            "Shapefile '{}': dropped {} point(s) outside source CRS valid range",
            layer_name,
            dropped_points,
        );
    }

    log::info!(
        "Shapefile '{}': loaded {} feature(s)",
        layer_name,
        layer.features.len(),
    );

    Ok(layer)
}

// =============================================================================
// Shape conversion
// =============================================================================

/// What `shape_to_features` produced for one input shape.
enum ShapeOutcome {
    Features(Vec<GeoFeature>),
    Skipped,
    PartiallyDropped {
        features: Vec<GeoFeature>,
        dropped: usize,
    },
}

/// Converts one shapefile `Shape` into zero or more `GeoFeature`s, applying
/// the source-CRS inverse transform as it goes.
///
/// Multi-types fan out (Multipoint → N Point features), matching how
/// GeoJSON multi-geometries are flattened.
fn shape_to_features(
    shape: Shape,
    source_crs: Crs,
    properties: &HashMap<String, JsonValue>,
) -> ShapeOutcome {
    match shape {
        // ----- Points -----
        Shape::Point(p) => point_to_feature(p.x, p.y, source_crs, properties)
            .map(|f| ShapeOutcome::Features(vec![f]))
            .unwrap_or(ShapeOutcome::PartiallyDropped { features: vec![], dropped: 1 }),
        Shape::PointM(p) => point_to_feature(p.x, p.y, source_crs, properties)
            .map(|f| ShapeOutcome::Features(vec![f]))
            .unwrap_or(ShapeOutcome::PartiallyDropped { features: vec![], dropped: 1 }),
        Shape::PointZ(p) => point_to_feature(p.x, p.y, source_crs, properties)
            .map(|f| ShapeOutcome::Features(vec![f]))
            .unwrap_or(ShapeOutcome::PartiallyDropped { features: vec![], dropped: 1 }),

        // ----- Multipoints fan out into individual point features -----
        Shape::Multipoint(mp) => {
            let mut features = Vec::with_capacity(mp.points().len());
            let mut dropped = 0;
            for p in mp.points() {
                match point_to_feature(p.x, p.y, source_crs, properties) {
                    Some(f) => features.push(f),
                    None => dropped += 1,
                }
            }
            shape_outcome(features, dropped)
        }
        Shape::MultipointM(mp) => {
            let mut features = Vec::with_capacity(mp.points().len());
            let mut dropped = 0;
            for p in mp.points() {
                match point_to_feature(p.x, p.y, source_crs, properties) {
                    Some(f) => features.push(f),
                    None => dropped += 1,
                }
            }
            shape_outcome(features, dropped)
        }
        Shape::MultipointZ(mp) => {
            let mut features = Vec::with_capacity(mp.points().len());
            let mut dropped = 0;
            for p in mp.points() {
                match point_to_feature(p.x, p.y, source_crs, properties) {
                    Some(f) => features.push(f),
                    None => dropped += 1,
                }
            }
            shape_outcome(features, dropped)
        }

        // ----- Polylines: each part = one LineString feature -----
        Shape::Polyline(pl) => polyline_to_features(
            pl.parts().iter().map(|part| {
                part.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
            }),
            source_crs,
            properties,
        ),
        Shape::PolylineM(pl) => polyline_to_features(
            pl.parts().iter().map(|part| {
                part.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
            }),
            source_crs,
            properties,
        ),
        Shape::PolylineZ(pl) => polyline_to_features(
            pl.parts().iter().map(|part| {
                part.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
            }),
            source_crs,
            properties,
        ),

        // ----- Polygons: outer + hole rings → one Polygon feature per outer -----
        Shape::Polygon(pg) => polygon_to_features(
            pg.rings().iter().map(|ring| {
                use shapefile::record::polygon::PolygonRing;
                let (is_outer, points) = match ring {
                    PolygonRing::Outer(pts) => (true, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                    PolygonRing::Inner(pts) => (false, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                };
                (is_outer, points)
            }),
            source_crs,
            properties,
        ),
        Shape::PolygonM(pg) => polygon_to_features(
            pg.rings().iter().map(|ring| {
                use shapefile::record::polygon::PolygonRing;
                let (is_outer, points) = match ring {
                    PolygonRing::Outer(pts) => (true, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                    PolygonRing::Inner(pts) => (false, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                };
                (is_outer, points)
            }),
            source_crs,
            properties,
        ),
        Shape::PolygonZ(pg) => polygon_to_features(
            pg.rings().iter().map(|ring| {
                use shapefile::record::polygon::PolygonRing;
                let (is_outer, points) = match ring {
                    PolygonRing::Outer(pts) => (true, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                    PolygonRing::Inner(pts) => (false, pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                };
                (is_outer, points)
            }),
            source_crs,
            properties,
        ),

        // ----- Skipped types -----
        Shape::Multipatch(_) | Shape::NullShape => ShapeOutcome::Skipped,
    }
}

fn shape_outcome(features: Vec<GeoFeature>, dropped: usize) -> ShapeOutcome {
    if dropped == 0 {
        ShapeOutcome::Features(features)
    } else {
        ShapeOutcome::PartiallyDropped { features, dropped }
    }
}

/// Builds a single point GeoFeature from CRS-native coordinates.
/// Returns `None` if the inverse transform is undefined for those coords
/// (e.g. Mercator past the polar cutoff).
fn point_to_feature(
    x: f64,
    y: f64,
    source_crs: Crs,
    properties: &HashMap<String, JsonValue>,
) -> Option<GeoFeature> {
    let coord = project_to_geo(x, y, source_crs)?;
    Some(GeoFeature {
        geometry: GeoGeometry::Point(coord),
        style: FeatureStyle::default(),
        properties: properties.clone(),
    })
}

fn polyline_to_features<I, P>(parts: I, source_crs: Crs, properties: &HashMap<String, JsonValue>) -> ShapeOutcome
where
    I: Iterator<Item = P>,
    P: IntoIterator<Item = (f64, f64)>,
{
    let mut features = Vec::new();
    let mut dropped = 0;
    for part in parts {
        let mut coords = Vec::new();
        for (x, y) in part {
            match project_to_geo(x, y, source_crs) {
                Some(c) => coords.push(c),
                None => dropped += 1,
            }
        }
        if coords.len() >= 2 {
            features.push(GeoFeature {
                geometry: GeoGeometry::LineString(coords),
                style: FeatureStyle::default(),
                properties: properties.clone(),
            });
        }
    }
    shape_outcome(features, dropped)
}

/// Polygon parts come back as a sequence of (is_outer, points). One
/// shapefile polygon record can contain multiple outer rings (each its own
/// island); each gets a separate `GeoGeometry::Polygon` feature, with all
/// subsequent inner rings (until the next outer) attached as holes.
fn polygon_to_features<I>(rings: I, source_crs: Crs, properties: &HashMap<String, JsonValue>) -> ShapeOutcome
where
    I: Iterator<Item = (bool, Vec<(f64, f64)>)>,
{
    let mut features = Vec::new();
    let mut dropped = 0;
    let mut current_outer: Option<Vec<GeoCoord>> = None;
    let mut current_holes: Vec<Vec<GeoCoord>> = Vec::new();

    let flush = |outer: Vec<GeoCoord>,
                 holes: Vec<Vec<GeoCoord>>,
                 features: &mut Vec<GeoFeature>| {
        if outer.len() < 3 {
            return; // degenerate
        }
        let mut rings = vec![outer];
        rings.extend(holes);
        features.push(GeoFeature {
            geometry: GeoGeometry::Polygon(rings),
            style: FeatureStyle::default(),
            properties: properties.clone(),
        });
    };

    for (is_outer, raw_points) in rings {
        let mut coords = Vec::with_capacity(raw_points.len());
        for (x, y) in raw_points {
            match project_to_geo(x, y, source_crs) {
                Some(c) => coords.push(c),
                None => dropped += 1,
            }
        }
        if is_outer {
            // Flush the previous outer + its holes.
            if let Some(outer) = current_outer.take() {
                flush(outer, std::mem::take(&mut current_holes), &mut features);
            }
            current_outer = Some(coords);
        } else {
            // Hole — only meaningful if we have an outer to attach it to.
            if current_outer.is_some() && coords.len() >= 3 {
                current_holes.push(coords);
            }
        }
    }
    if let Some(outer) = current_outer.take() {
        flush(outer, current_holes, &mut features);
    }

    shape_outcome(features, dropped)
}

/// Converts one (x, y) in `source_crs` units to a `GeoCoord` (WGS84 lat/lon).
/// Identity for sources already in WGS84; runs the CRS inverse transform
/// otherwise.
fn project_to_geo(x: f64, y: f64, source_crs: Crs) -> Option<GeoCoord> {
    match source_crs {
        Crs::EquirectWgs84 => {
            // Validate range — out-of-range values mean the file's CRS
            // claim was wrong and bounds inference also failed; drop them
            // rather than render at impossible lat/lon.
            if x.abs() > 180.0 || y.abs() > 90.0 {
                return None;
            }
            Some(GeoCoord::new(x, y))
        }
        Crs::WebMercator => {
            let (lat, lon) = source_crs.xy_to_latlon(x, y)?;
            Some(GeoCoord::new(lon, lat))
        }
    }
}

// =============================================================================
// DBF record → GeoFeature.properties
// =============================================================================

/// Converts a dBase record into a GeoJSON-style properties map.
///
/// Field-value type mapping mirrors what real-world tools (QGIS, GDAL) do
/// when exporting shapefile attributes to GeoJSON: numerics become JSON
/// numbers, dates become ISO strings, missing values become null.
fn record_to_properties(record: &shapefile::dbase::Record) -> HashMap<String, JsonValue> {
    let map: &HashMap<String, shapefile::dbase::FieldValue> = record.as_ref();
    map.iter()
        .map(|(name, value)| (name.clone(), field_value_to_json(value)))
        .collect()
}

fn field_value_to_json(v: &shapefile::dbase::FieldValue) -> JsonValue {
    use shapefile::dbase::FieldValue::*;
    match v {

        Character(Some(s)) => JsonValue::String(s.clone()),
        Character(None) => JsonValue::Null,
        Numeric(Some(n)) => json_number_or_null(*n),
        Numeric(None) => JsonValue::Null,
        Logical(Some(b)) => JsonValue::Bool(*b),
        Logical(None) => JsonValue::Null,
        Date(Some(d)) => JsonValue::String(format!("{:?}", d)),
        Date(None) => JsonValue::Null,
        Float(Some(f)) => json_number_or_null(*f as f64),
        Float(None) => JsonValue::Null,
        Integer(i) => JsonValue::Number((*i).into()),
        Currency(c) => json_number_or_null(*c),
        DateTime(dt) => JsonValue::String(format!("{:?}", dt)),
        Double(d) => json_number_or_null(*d),
        Memo(s) => JsonValue::String(s.clone()),
    }
}

/// JSON numbers reject NaN/Infinity — fall back to null in those cases
/// rather than panicking on a malformed dBase record.
fn json_number_or_null(n: f64) -> JsonValue {
    serde_json::Number::from_f64(n)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_to_geo_passes_wgs84_through() {
        let c = project_to_geo(13.4, 52.5, Crs::EquirectWgs84).unwrap();
        assert!((c.lon - 13.4).abs() < 1e-12);
        assert!((c.lat - 52.5).abs() < 1e-12);
    }

    #[test]
    fn project_to_geo_drops_out_of_range_wgs84() {
        // 200° longitude is structurally impossible — drop rather than
        // wrap silently.
        assert!(project_to_geo(200.0, 0.0, Crs::EquirectWgs84).is_none());
        assert!(project_to_geo(0.0, 100.0, Crs::EquirectWgs84).is_none());
    }

    #[test]
    fn project_to_geo_inverse_transforms_mercator_meters() {
        // Berlin in Web Mercator meters → ~13.4°E, ~52.5°N.
        let c = project_to_geo(1_492_000.0, 6_894_000.0, Crs::WebMercator).unwrap();
        assert!((c.lon - 13.4).abs() < 0.1);
        assert!((c.lat - 52.5).abs() < 0.1);
    }

    #[test]
    fn json_number_or_null_handles_nan() {
        match json_number_or_null(f64::NAN) {
            JsonValue::Null => {}
            v => panic!("expected null for NaN, got {:?}", v),
        }
    }

    #[test]
    fn json_number_or_null_handles_normal_floats() {
        match json_number_or_null(3.14) {
            JsonValue::Number(_) => {}
            v => panic!("expected number for 3.14, got {:?}", v),
        }
    }
}
