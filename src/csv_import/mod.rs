// =============================================================================
// Orbis — CSV Import (lat/lon point clouds)
// =============================================================================
// Drop-target for CSV files that carry a spatial location per row plus an
// optional name/description. Produces a `GeoLayer` of point features —
// no line/polygon support (that's GeoJSON's domain).
//
// Module layout:
// - columns.rs     header → role mapping (pure, deterministic)
// - (mod.rs)       loader: file I/O, CSV parse, GeoFeature assembly
//
// What this loader does NOT do:
// - Reproject from non-WGS84 CRSes. CSV has no metadata sidecar; users
//   would have to manually declare the CRS, and that's a UI we don't yet
//   have. For now, lat/lon means WGS84 degrees. Out-of-range values get
//   dropped with a warning.
// - Multi-locale headers (`Breitengrad`, `経度`). English synonyms only.
//   Multi-locale heuristics are easy to get wrong; extend on demand.
// - Mixed lat/lon and projected coords in the same file. One unit per
//   column; the loader rejects anything outside ±90/±180.
// =============================================================================

pub mod columns;

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value as JsonValue;

use crate::geojson::{FeatureStyle, GeoCoord, GeoFeature, GeoGeometry, GeoLayer};
use columns::ColumnMapping;

/// Loads a CSV file into a `GeoLayer` of point features.
///
/// The first row is treated as the header. Columns are matched to roles
/// (lat / lon / name / description) by `columns::map_columns`. Each
/// subsequent row produces one `GeoFeature::Point` whose:
/// - geometry is `GeoGeometry::Point(lon, lat)`
/// - style.label is set when the row has a non-empty `name` cell
/// - properties contains the `description` cell (when present) plus every
///   other column as a JSON string (so users can still see the full row
///   data without needing a separate inspector)
pub fn load_csv_file(path: &Path) -> Result<GeoLayer, String> {
    let layer_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("csv")
        .to_string();

    // Auto-detect delimiter by sniffing the first line. Priority order:
    // tab (TSV is unambiguous), semicolon (German Excel exports), comma
    // (everyone else). The csv crate handles quoting once we pick.
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read CSV '{}': {}", path.display(), e))?;
    let delimiter = sniff_delimiter(&raw);

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true) // tolerate trailing-comma rows
        .from_reader(raw.as_bytes());

    // Snapshot headers as owned strings before we start iterating records,
    // because csv::Reader borrows them per-call.
    let header_owned: Vec<String> = reader
        .headers()
        .map_err(|e| format!("CSV header read failed: {}", e))?
        .iter()
        .map(|s| s.to_string())
        .collect();
    let header_refs: Vec<&str> = header_owned.iter().map(|s| s.as_str()).collect();
    let mapping = columns::map_columns(&header_refs)?;

    log::info!(
        "CSV '{}': mapped lat={}, lon={}, name={:?}, desc={:?}",
        layer_name,
        header_owned[mapping.lat_idx],
        header_owned[mapping.lon_idx],
        mapping.name_idx.map(|i| header_owned[i].as_str()),
        mapping.description_idx.map(|i| header_owned[i].as_str()),
    );

    let mut layer = GeoLayer::new(&layer_name);
    let mut row_no = 0usize;
    let mut dropped_unparseable = 0usize;
    let mut dropped_out_of_range = 0usize;

    for result in reader.records() {
        row_no += 1;
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                log::warn!("CSV '{}' row {} skipped: {}", layer_name, row_no, e);
                dropped_unparseable += 1;
                continue;
            }
        };

        match row_to_feature(&record, &header_owned, &mapping) {
            RowOutcome::Feature(f) => layer.features.push(f),
            RowOutcome::Unparseable => dropped_unparseable += 1,
            RowOutcome::OutOfRange => dropped_out_of_range += 1,
        }
    }

    if dropped_unparseable > 0 {
        log::warn!(
            "CSV '{}': dropped {} row(s) with unparseable lat/lon",
            layer_name,
            dropped_unparseable,
        );
    }
    if dropped_out_of_range > 0 {
        log::warn!(
            "CSV '{}': dropped {} row(s) with lat/lon outside ±90/±180 — \
             values likely projected coordinates rather than WGS84 degrees",
            layer_name,
            dropped_out_of_range,
        );
    }

    log::info!(
        "CSV '{}': loaded {} point(s)",
        layer_name,
        layer.features.len(),
    );

    Ok(layer)
}

enum RowOutcome {
    Feature(GeoFeature),
    Unparseable,
    OutOfRange,
}

fn row_to_feature(
    record: &csv::StringRecord,
    headers: &[String],
    mapping: &ColumnMapping,
) -> RowOutcome {
    let lat_str = match record.get(mapping.lat_idx) {
        Some(s) => s.trim(),
        None => return RowOutcome::Unparseable,
    };
    let lon_str = match record.get(mapping.lon_idx) {
        Some(s) => s.trim(),
        None => return RowOutcome::Unparseable,
    };

    let lat = match parse_decimal(lat_str) {
        Some(v) => v,
        None => return RowOutcome::Unparseable,
    };
    let lon = match parse_decimal(lon_str) {
        Some(v) => v,
        None => return RowOutcome::Unparseable,
    };

    if lat.abs() > 90.0 || lon.abs() > 180.0 {
        return RowOutcome::OutOfRange;
    }

    let mut style = FeatureStyle::default();
    if let Some(idx) = mapping.name_idx {
        if let Some(name) = record.get(idx) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                style.label = Some(trimmed.to_string());
            }
        }
    }

    // Build properties: include the description (named) plus every other
    // column verbatim, so users can hover/inspect to see the raw data.
    // Lat/lon themselves are excluded — the geometry already carries them.
    let mut properties = HashMap::new();
    if let Some(idx) = mapping.description_idx {
        if let Some(desc) = record.get(idx) {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                properties.insert("description".to_string(), JsonValue::String(trimmed.to_string()));
            }
        }
    }
    for (i, header) in headers.iter().enumerate() {
        if i == mapping.lat_idx || i == mapping.lon_idx {
            continue;
        }
        // Skip name and description — they have dedicated handling above.
        if mapping.name_idx == Some(i) || mapping.description_idx == Some(i) {
            continue;
        }
        if let Some(cell) = record.get(i) {
            let trimmed = cell.trim();
            if !trimmed.is_empty() {
                properties
                    .entry(header.clone())
                    .or_insert_with(|| JsonValue::String(trimmed.to_string()));
            }
        }
    }

    RowOutcome::Feature(GeoFeature {
        geometry: GeoGeometry::Point(GeoCoord::new(lon, lat)),
        style,
        properties,
    })
}

/// Best-effort decimal parse. Accepts both `.` and `,` as decimal separator
/// (German Excel writes `52,5` instead of `52.5`). Rejects strings with both,
/// or with thousands separators — those are too risky to disambiguate.
fn parse_decimal(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let dot_count = s.matches('.').count();
    let comma_count = s.matches(',').count();
    let normalised = match (dot_count, comma_count) {
        (_, 0) => s.to_string(),               // pure dot or no separator
        (0, 1) => s.replace(',', "."),         // single comma → decimal comma
        _ => return None,                      // ambiguous: mixed or thousand sep
    };
    normalised.parse::<f64>().ok()
}

/// Picks the most-likely delimiter by counting occurrences in the first
/// non-empty line. Priority is fixed: `\t` > `;` > `,`. Any line that
/// contains a tab is almost certainly a TSV; semicolons in the first line
/// signal German-style CSV; otherwise default to comma.
fn sniff_delimiter(text: &str) -> u8 {
    // Strip optional UTF-8 BOM so it doesn't end up in the first cell.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.contains('\t') {
            return b'\t';
        }
        if line.contains(';') && !line.contains(',') {
            return b';';
        }
        // Mixed `;` and `,` defaults to `,` — quoted values may legitimately
        // contain `;` even in comma-delimited files.
        return b',';
    }
    b','
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_csv(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("orbis_csv_test_{}_{}.csv", std::process::id(), name));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_minimal_csv() {
        let path = write_temp_csv(
            "minimal",
            "lat,lon\n52.5,13.4\n48.8,2.35\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 2);
        match &layer.features[0].geometry {
            GeoGeometry::Point(c) => {
                assert!((c.lat - 52.5).abs() < 1e-9);
                assert!((c.lon - 13.4).abs() < 1e-9);
            }
            other => panic!("expected Point, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn name_column_becomes_label() {
        let path = write_temp_csv(
            "named",
            "name,lat,lon\nBerlin,52.5,13.4\nParis,48.8,2.35\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 2);
        assert_eq!(layer.features[0].style.label.as_deref(), Some("Berlin"));
        assert_eq!(layer.features[1].style.label.as_deref(), Some("Paris"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn description_lands_in_properties() {
        let path = write_temp_csv(
            "described",
            "name,lat,lon,description\nA,52.5,13.4,The capital\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 1);
        let f = &layer.features[0];
        assert_eq!(f.style.label.as_deref(), Some("A"));
        assert_eq!(
            f.properties.get("description"),
            Some(&JsonValue::String("The capital".to_string())),
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extra_columns_become_properties() {
        let path = write_temp_csv(
            "extras",
            "lat,lon,country,population\n52.5,13.4,DE,3700000\n",
        );
        let layer = load_csv_file(&path).unwrap();
        let f = &layer.features[0];
        assert_eq!(f.properties.get("country"), Some(&JsonValue::String("DE".to_string())));
        assert_eq!(f.properties.get("population"), Some(&JsonValue::String("3700000".to_string())));
        // lat/lon are NOT duplicated as properties.
        assert!(!f.properties.contains_key("lat"));
        assert!(!f.properties.contains_key("lon"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn out_of_range_rows_are_dropped() {
        let path = write_temp_csv(
            "bad-range",
            "lat,lon\n52.5,13.4\n95.0,200.0\n0,0\n",
        );
        let layer = load_csv_file(&path).unwrap();
        // Two valid (52.5/13.4 and 0/0); one out of range.
        assert_eq!(layer.features.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unparseable_rows_are_dropped_not_fatal() {
        let path = write_temp_csv(
            "bad-numbers",
            "lat,lon\n52.5,13.4\nNaNly,oops\n48.8,2.35\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn semicolon_delimiter_auto_detected() {
        let path = write_temp_csv(
            "semicolon",
            "name;lat;lon\nBerlin;52.5;13.4\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 1);
        assert_eq!(layer.features[0].style.label.as_deref(), Some("Berlin"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tab_delimiter_auto_detected() {
        let path = write_temp_csv(
            "tab",
            "name\tlat\tlon\nBerlin\t52.5\t13.4\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn german_decimal_comma_with_semicolon_delimiter() {
        // The classic German Excel export.
        let path = write_temp_csv(
            "de-decimal",
            "name;lat;lon\nBerlin;52,5;13,4\n",
        );
        let layer = load_csv_file(&path).unwrap();
        match &layer.features[0].geometry {
            GeoGeometry::Point(c) => {
                assert!((c.lat - 52.5).abs() < 1e-9, "lat = {}", c.lat);
                assert!((c.lon - 13.4).abs() < 1e-9, "lon = {}", c.lon);
            }
            other => panic!("expected Point, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_lat_column_returns_clear_error() {
        let path = write_temp_csv(
            "no-lat",
            "lon,name\n13.4,Berlin\n",
        );
        let err = load_csv_file(&path).unwrap_err();
        assert!(err.contains("latitude"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_name_does_not_set_label() {
        let path = write_temp_csv(
            "empty-name",
            "name,lat,lon\n,52.5,13.4\n",
        );
        let layer = load_csv_file(&path).unwrap();
        assert_eq!(layer.features.len(), 1);
        assert_eq!(layer.features[0].style.label, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_decimal_accepts_dot_and_comma() {
        assert_eq!(parse_decimal("52.5"), Some(52.5));
        assert_eq!(parse_decimal("52,5"), Some(52.5));
        assert_eq!(parse_decimal("-13.4"), Some(-13.4));
        assert_eq!(parse_decimal("0"), Some(0.0));
    }

    #[test]
    fn parse_decimal_rejects_ambiguous() {
        // Mixed decimal and thousands separator is dangerous — refuse.
        assert_eq!(parse_decimal("1,234.5"), None);
        assert_eq!(parse_decimal("1.234,5"), None);
        // Multiple of the same kind = malformed
        assert_eq!(parse_decimal("1.2.3"), None);
        // Empty
        assert_eq!(parse_decimal(""), None);
        // Garbage
        assert_eq!(parse_decimal("abc"), None);
    }

    #[test]
    fn sniff_delimiter_prefers_tab() {
        assert_eq!(sniff_delimiter("a\tb\tc\n1\t2\t3\n"), b'\t');
    }

    #[test]
    fn sniff_delimiter_picks_semicolon_when_no_comma() {
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3\n"), b';');
    }

    #[test]
    fn sniff_delimiter_falls_back_to_comma() {
        assert_eq!(sniff_delimiter("a,b,c\n1,2,3\n"), b',');
    }

    #[test]
    fn sniff_delimiter_handles_bom() {
        assert_eq!(sniff_delimiter("\u{feff}a,b\n1,2\n"), b',');
    }

    #[test]
    fn sniff_delimiter_with_mixed_chars_prefers_comma() {
        // A comma-delimited file with quoted semicolons inside cells must
        // still be detected as comma.
        assert_eq!(sniff_delimiter("a,b,c\n\"1;2\",3,4\n"), b',');
    }
}
