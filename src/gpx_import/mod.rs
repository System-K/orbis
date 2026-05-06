// =============================================================================
// Orbis — GPX Import (waypoints, routes, tracks)
// =============================================================================
// Drop-target for .gpx files: the user drags one in (or adds it via
// Add-Custom-Source) and we produce a `GeoLayer` of point + line features.
//
// What GPX has and how it maps to GeoFeature:
// - <wpt>     standalone point of interest    → GeoGeometry::Point
// - <rte>     ordered list of route points    → GeoGeometry::LineString
// - <trk>     a multi-segment track recording → one LineString per segment
//
// GPX is always WGS84 by spec — there's no projection metadata to detect.
// Out-of-range coordinates (|lat| > 90 or |lon| > 180) are dropped with a
// summary warning, matching the CSV import policy.
//
// Per-point metadata (elevation, time, source, etc.) is preserved in the
// feature's `properties` map. Names go to `style.label`. Descriptions to
// `properties["description"]`.
// =============================================================================

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value as JsonValue;

use crate::geojson::{FeatureStyle, GeoCoord, GeoFeature, GeoGeometry, GeoLayer};

/// Loads a GPX file into a `GeoLayer`. Each waypoint becomes one Point
/// feature; each track segment becomes one LineString feature; each route
/// becomes one LineString feature.
pub fn load_gpx_file(path: &Path) -> Result<GeoLayer, String> {
    let layer_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gpx")
        .to_string();

    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open GPX '{}': {}", path.display(), e))?;
    let reader = std::io::BufReader::new(file);

    let parsed = gpx::read(reader)
        .map_err(|e| format!("GPX parse failed for '{}': {}", layer_name, e))?;

    let mut layer = GeoLayer::new(&layer_name);
    let mut dropped_points = 0usize;
    let mut empty_segments = 0usize;

    // ----- Waypoints (<wpt>) → Point features
    for wp in &parsed.waypoints {
        if let Some(feature) = waypoint_to_point_feature(wp) {
            layer.features.push(feature);
        } else {
            dropped_points += 1;
        }
    }

    // ----- Routes (<rte>) → LineString features
    for route in &parsed.routes {
        let mut coords = Vec::with_capacity(route.points.len());
        for wp in &route.points {
            match waypoint_to_geocoord(wp) {
                Some(c) => coords.push(c),
                None => dropped_points += 1,
            }
        }
        if coords.len() >= 2 {
            layer.features.push(GeoFeature {
                geometry: GeoGeometry::LineString(coords),
                style: FeatureStyle {
                    label: route.name.clone(),
                    ..FeatureStyle::default()
                },
                properties: route_properties(route),
            });
        } else {
            empty_segments += 1;
        }
    }

    // ----- Tracks (<trk>): each <trkseg> → its own LineString feature.
    // Track-level metadata (name, description) repeats on every segment so
    // the user can see which track a segment belongs to even after the
    // track→segments fan-out.
    for track in &parsed.tracks {
        let track_props = track_properties(track);
        for segment in &track.segments {
            let mut coords = Vec::with_capacity(segment.points.len());
            for wp in &segment.points {
                match waypoint_to_geocoord(wp) {
                    Some(c) => coords.push(c),
                    None => dropped_points += 1,
                }
            }
            if coords.len() >= 2 {
                layer.features.push(GeoFeature {
                    geometry: GeoGeometry::LineString(coords),
                    style: FeatureStyle {
                        label: track.name.clone(),
                        ..FeatureStyle::default()
                    },
                    properties: track_props.clone(),
                });
            } else {
                empty_segments += 1;
            }
        }
    }

    if dropped_points > 0 {
        log::warn!(
            "GPX '{}': dropped {} point(s) outside WGS84 valid range",
            layer_name,
            dropped_points,
        );
    }
    if empty_segments > 0 {
        log::warn!(
            "GPX '{}': skipped {} empty track segment(s) / degenerate route(s)",
            layer_name,
            empty_segments,
        );
    }

    log::info!(
        "GPX '{}': {} waypoint(s), {} route(s), {} track(s) → {} feature(s)",
        layer_name,
        parsed.waypoints.len(),
        parsed.routes.len(),
        parsed.tracks.len(),
        layer.features.len(),
    );

    Ok(layer)
}

// =============================================================================
// Conversion helpers
// =============================================================================

/// Extracts (lon, lat) from a `gpx::Waypoint`'s underlying point. GPX
/// stores points as `(x, y)` where `x = lon`, `y = lat`. Returns None for
/// out-of-range coordinates.
fn waypoint_to_geocoord(wp: &gpx::Waypoint) -> Option<GeoCoord> {
    let point = wp.point();
    let lon = point.x();
    let lat = point.y();
    if lon.abs() > 180.0 || lat.abs() > 90.0 {
        return None;
    }
    Some(GeoCoord::new(lon, lat))
}

/// Builds a Point GeoFeature from a `<wpt>`. Drops any point with
/// out-of-range coordinates.
fn waypoint_to_point_feature(wp: &gpx::Waypoint) -> Option<GeoFeature> {
    let coord = waypoint_to_geocoord(wp)?;
    Some(GeoFeature {
        geometry: GeoGeometry::Point(coord),
        style: FeatureStyle {
            label: wp.name.clone(),
            ..FeatureStyle::default()
        },
        properties: waypoint_properties(wp),
    })
}

/// Per-waypoint metadata. Includes description (visible to GUI tooltip
/// pipelines) plus elevation, time, source, type — useful for biology /
/// hiking / GPS-recording workflows.
fn waypoint_properties(wp: &gpx::Waypoint) -> HashMap<String, JsonValue> {
    let mut props = HashMap::new();
    if let Some(d) = &wp.description {
        if !d.is_empty() {
            props.insert("description".to_string(), JsonValue::String(d.clone()));
        }
    }
    if let Some(c) = &wp.comment {
        if !c.is_empty() {
            props.insert("comment".to_string(), JsonValue::String(c.clone()));
        }
    }
    if let Some(elev) = wp.elevation {
        if let Some(num) = serde_json::Number::from_f64(elev) {
            props.insert("elevation_m".to_string(), JsonValue::Number(num));
        }
    }
    if let Some(t) = &wp.time {
        // Time::format() returns Result; fall back to debug if it fails.
        let s = t.format().unwrap_or_else(|_| format!("{:?}", t));
        props.insert("time".to_string(), JsonValue::String(s));
    }
    if let Some(src) = &wp.source {
        if !src.is_empty() {
            props.insert("source".to_string(), JsonValue::String(src.clone()));
        }
    }
    if let Some(t) = &wp.type_ {
        if !t.is_empty() {
            props.insert("type".to_string(), JsonValue::String(t.clone()));
        }
    }
    if let Some(sym) = &wp.symbol {
        if !sym.is_empty() {
            props.insert("symbol".to_string(), JsonValue::String(sym.clone()));
        }
    }
    props
}

fn route_properties(route: &gpx::Route) -> HashMap<String, JsonValue> {
    let mut props = HashMap::new();
    if let Some(d) = &route.description {
        if !d.is_empty() {
            props.insert("description".to_string(), JsonValue::String(d.clone()));
        }
    }
    if let Some(c) = &route.comment {
        if !c.is_empty() {
            props.insert("comment".to_string(), JsonValue::String(c.clone()));
        }
    }
    if let Some(t) = &route.type_ {
        if !t.is_empty() {
            props.insert("type".to_string(), JsonValue::String(t.clone()));
        }
    }
    props.insert("gpx_kind".to_string(), JsonValue::String("route".to_string()));
    props
}

fn track_properties(track: &gpx::Track) -> HashMap<String, JsonValue> {
    let mut props = HashMap::new();
    if let Some(d) = &track.description {
        if !d.is_empty() {
            props.insert("description".to_string(), JsonValue::String(d.clone()));
        }
    }
    if let Some(c) = &track.comment {
        if !c.is_empty() {
            props.insert("comment".to_string(), JsonValue::String(c.clone()));
        }
    }
    if let Some(t) = &track.type_ {
        if !t.is_empty() {
            props.insert("type".to_string(), JsonValue::String(t.clone()));
        }
    }
    props.insert("gpx_kind".to_string(), JsonValue::String("track".to_string()));
    props
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_gpx(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "orbis_gpx_test_{}_{}.gpx",
            std::process::id(),
            name,
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    const WAYPOINT_ONLY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-test" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="52.5" lon="13.4">
    <name>Berlin</name>
    <desc>The capital</desc>
    <ele>34</ele>
  </wpt>
</gpx>"#;

    const TRACK_TWO_SEGMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-test" xmlns="http://www.topografix.com/GPX/1/1">
  <trk>
    <name>Sunday hike</name>
    <desc>Up the hill</desc>
    <trkseg>
      <trkpt lat="48.8" lon="2.35"></trkpt>
      <trkpt lat="48.81" lon="2.36"></trkpt>
      <trkpt lat="48.82" lon="2.37"></trkpt>
    </trkseg>
    <trkseg>
      <trkpt lat="48.83" lon="2.38"></trkpt>
      <trkpt lat="48.84" lon="2.39"></trkpt>
    </trkseg>
  </trk>
</gpx>"#;

    const ROUTE_AND_WAYPOINT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-test" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="35.7" lon="139.7"><name>Tokyo</name></wpt>
  <rte>
    <name>To the harbour</name>
    <rtept lat="35.7" lon="139.7"></rtept>
    <rtept lat="35.6" lon="139.7"></rtept>
    <rtept lat="35.5" lon="139.8"></rtept>
  </rte>
</gpx>"#;

    #[test]
    fn waypoint_becomes_point_with_label() {
        let path = write_temp_gpx("waypoint", WAYPOINT_ONLY);
        let layer = load_gpx_file(&path).unwrap();
        assert_eq!(layer.features.len(), 1);
        let f = &layer.features[0];
        match &f.geometry {
            GeoGeometry::Point(c) => {
                assert!((c.lat - 52.5).abs() < 1e-9);
                assert!((c.lon - 13.4).abs() < 1e-9);
            }
            other => panic!("expected Point, got {:?}", other),
        }
        assert_eq!(f.style.label.as_deref(), Some("Berlin"));
        assert_eq!(
            f.properties.get("description"),
            Some(&JsonValue::String("The capital".to_string())),
        );
        // Elevation makes it through as a number property.
        match f.properties.get("elevation_m") {
            Some(JsonValue::Number(n)) => assert_eq!(n.as_f64(), Some(34.0)),
            other => panic!("expected number, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn track_with_two_segments_yields_two_features() {
        let path = write_temp_gpx("track", TRACK_TWO_SEGMENTS);
        let layer = load_gpx_file(&path).unwrap();
        // Two LineString features (one per <trkseg>), both labelled with
        // the track name "Sunday hike".
        assert_eq!(layer.features.len(), 2);
        for f in &layer.features {
            assert!(matches!(f.geometry, GeoGeometry::LineString(_)));
            assert_eq!(f.style.label.as_deref(), Some("Sunday hike"));
            assert_eq!(
                f.properties.get("gpx_kind"),
                Some(&JsonValue::String("track".to_string())),
            );
        }
        // First segment: 3 points. Second: 2 points.
        if let GeoGeometry::LineString(pts) = &layer.features[0].geometry {
            assert_eq!(pts.len(), 3);
        }
        if let GeoGeometry::LineString(pts) = &layer.features[1].geometry {
            assert_eq!(pts.len(), 2);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn route_becomes_linestring_with_route_kind() {
        let path = write_temp_gpx("route", ROUTE_AND_WAYPOINT);
        let layer = load_gpx_file(&path).unwrap();
        // 1 waypoint feature + 1 route feature.
        assert_eq!(layer.features.len(), 2);

        let route = layer
            .features
            .iter()
            .find(|f| matches!(f.geometry, GeoGeometry::LineString(_)))
            .expect("route feature missing");
        assert_eq!(route.style.label.as_deref(), Some("To the harbour"));
        assert_eq!(
            route.properties.get("gpx_kind"),
            Some(&JsonValue::String("route".to_string())),
        );

        let waypoint = layer
            .features
            .iter()
            .find(|f| matches!(f.geometry, GeoGeometry::Point(_)))
            .expect("waypoint feature missing");
        assert_eq!(waypoint.style.label.as_deref(), Some("Tokyo"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_gpx_produces_empty_layer() {
        let path = write_temp_gpx(
            "empty",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-test" xmlns="http://www.topografix.com/GPX/1/1">
</gpx>"#,
        );
        let layer = load_gpx_file(&path).unwrap();
        assert_eq!(layer.features.len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_xml_returns_error() {
        let path = write_temp_gpx("malformed", "<gpx version=\"1.1\"><wpt lat=");
        let err = load_gpx_file(&path).unwrap_err();
        assert!(err.to_lowercase().contains("gpx"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nonexistent_file_returns_error() {
        let result = load_gpx_file(std::path::Path::new("/no/such/file.gpx"));
        assert!(result.is_err());
    }

    #[test]
    fn single_point_segment_is_skipped_as_degenerate() {
        // A track segment with only one point can't form a LineString
        // (need >= 2 vertices). The loader should skip it without panicking.
        let path = write_temp_gpx(
            "single_point_seg",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-test" xmlns="http://www.topografix.com/GPX/1/1">
  <trk>
    <trkseg>
      <trkpt lat="0" lon="0"></trkpt>
    </trkseg>
  </trk>
</gpx>"#,
        );
        let layer = load_gpx_file(&path).unwrap();
        assert_eq!(layer.features.len(), 0);
        let _ = std::fs::remove_file(&path);
    }
}
