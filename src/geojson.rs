// =============================================================================
// Orbis — GeoJSON Parser + Data Model (M11)
// =============================================================================
// Parses GeoJSON files (RFC 7946) into an internal representation
// suitable for rendering as markers, lines, and polygons on the globe.
//
// Design decisions:
// - No external geojson crate — the format is simple enough for serde_json
// - Multi-geometries (MultiPoint, etc.) are flattened into individual features
// - Style properties extracted from GeoJSON "properties" where available
// - Coordinates always stored as (longitude, latitude) per GeoJSON spec
// =============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

// =============================================================================
// Core Data Types
// =============================================================================

/// A geographic coordinate (WGS 84).
///
/// GeoJSON stores coordinates as [longitude, latitude, altitude?].
/// We keep that order internally but name the fields clearly.
#[derive(Debug, Clone, Copy)]
pub struct GeoCoord {
    /// Longitude in degrees (-180..+180)
    pub lon: f64,
    /// Latitude in degrees (-90..+90)
    pub lat: f64,
    /// Optional altitude in meters (rarely used)
    pub alt: Option<f64>,
}

impl GeoCoord {
    pub fn new(lon: f64, lat: f64) -> Self {
        Self { lon, lat, alt: None }
    }

    pub fn with_alt(lon: f64, lat: f64, alt: f64) -> Self {
        Self {
            lon,
            lat,
            alt: Some(alt),
        }
    }

    /// Converts to a unit-sphere 3D position (for globe rendering).
    ///
    /// Uses standard spherical coordinates:
    /// - x = cos(lat) * cos(lon)
    /// - y = sin(lat)
    /// - z = cos(lat) * sin(lon)
    pub fn to_unit_sphere(&self) -> [f32; 3] {
        let lat_rad = (self.lat as f32).to_radians();
        let lon_rad = (self.lon as f32).to_radians();
        [
            lat_rad.cos() * lon_rad.cos(),
            lat_rad.sin(),
            lat_rad.cos() * lon_rad.sin(),
        ]
    }

    /// Converts to 2D map position (equirectangular projection).
    ///
    /// Returns (u, v) in [0, 1] range matching our UV mapping:
    /// - u = (lon + 180) / 360
    /// - v = (90 - lat) / 180
    pub fn to_uv(&self) -> [f32; 2] {
        let u = ((self.lon + 180.0) / 360.0) as f32;
        let v = ((90.0 - self.lat) / 180.0) as f32;
        [u, v]
    }
}

/// Geometry variants supported by Orbis.
///
/// Multi-types (MultiPoint, MultiLineString, MultiPolygon) are
/// flattened into multiple features during parsing.
#[derive(Debug, Clone)]
pub enum GeoGeometry {
    /// A single point (marker, city, event, etc.)
    Point(GeoCoord),

    /// An ordered sequence of coordinates forming a path.
    LineString(Vec<GeoCoord>),

    /// A closed area. First ring is the outer boundary,
    /// subsequent rings are holes (if any).
    Polygon(Vec<Vec<GeoCoord>>),
}

/// Visual style for rendering a feature.
///
/// Extracted from GeoJSON `properties` where available,
/// otherwise uses sensible defaults per geometry type.
#[derive(Debug, Clone)]
pub struct FeatureStyle {
    /// Fill/marker color as RGBA (0.0–1.0)
    pub color: [f32; 4],
    /// Stroke/outline color as RGBA
    pub stroke_color: [f32; 4],
    /// Marker radius in pixels (points only)
    pub marker_size: f32,
    /// Line width in pixels (lines and polygon outlines)
    pub line_width: f32,
    /// Optional text label
    pub label: Option<String>,
    /// Whether the fill color was explicitly set in properties
    pub has_explicit_color: bool,
    /// Whether the stroke color was explicitly set in properties
    pub has_explicit_stroke: bool,
}

impl Default for FeatureStyle {
    fn default() -> Self {
        Self {
            color: [1.0, 0.3, 0.1, 0.8],        // Orange-red
            stroke_color: [1.0, 1.0, 1.0, 1.0],  // White outline
            marker_size: 6.0,
            line_width: 2.0,
            label: None,
            has_explicit_color: false,
            has_explicit_stroke: false,
        }
    }
}

/// A single geographic feature with geometry, style, and metadata.
///
/// This is the fundamental unit that gets rendered on the globe/map.
#[derive(Debug, Clone)]
pub struct GeoFeature {
    /// The shape to render
    pub geometry: GeoGeometry,
    /// Visual appearance
    pub style: FeatureStyle,
    /// Raw properties from the GeoJSON (for tooltips, filtering, etc.)
    pub properties: HashMap<String, Value>,
}

/// A named collection of features loaded from a single source.
///
/// Corresponds roughly to a GeoJSON FeatureCollection,
/// but can also be built programmatically (e.g. from API data).
#[derive(Debug, Clone)]
pub struct GeoLayer {
    /// Display name (filename, API source name, etc.)
    pub name: String,
    /// All features in this layer
    pub features: Vec<GeoFeature>,
    /// Whether this layer is currently visible
    pub visible: bool,
    /// Attribution text from the data source (shown in GUI footer)
    pub attribution: Option<String>,
}

impl GeoLayer {
    /// Creates a new empty layer.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            features: Vec::new(),
            visible: true,
            attribution: None,
        }
    }

    /// Number of features.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Is this layer empty?
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Iterator over features of a specific geometry type.
    pub fn points(&self) -> impl Iterator<Item = &GeoFeature> {
        self.features
            .iter()
            .filter(|f| matches!(f.geometry, GeoGeometry::Point(_)))
    }

    pub fn lines(&self) -> impl Iterator<Item = &GeoFeature> {
        self.features
            .iter()
            .filter(|f| matches!(f.geometry, GeoGeometry::LineString(_)))
    }

    pub fn polygons(&self) -> impl Iterator<Item = &GeoFeature> {
        self.features
            .iter()
            .filter(|f| matches!(f.geometry, GeoGeometry::Polygon(_)))
    }
}

// =============================================================================
// GeoJSON Parsing
// =============================================================================

/// Parses a GeoJSON string into a `GeoLayer`.
///
/// Supports:
/// - FeatureCollection (most common)
/// - Single Feature
/// - Bare Geometry (wrapped in a default feature)
///
/// Multi-geometries are flattened: a MultiPoint with 5 points
/// becomes 5 individual Point features (same properties each).
pub fn parse_geojson(json_str: &str, layer_name: &str) -> Result<GeoLayer, String> {
    let root: Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {}", e))?;

    let geojson_type = root
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'type' field — not a valid GeoJSON document")?;

    let mut layer = GeoLayer::new(layer_name);

    match geojson_type {
        "FeatureCollection" => {
            // Use collection-level "name" property as layer name (if present)
            if let Some(name) = root.get("name").and_then(|v| v.as_str()) {
                layer.name = name.to_string();
            }

            // Extract attribution from the FeatureCollection root
            if let Some(attr) = root.get("attribution").and_then(|v| v.as_str()) {
                layer.attribution = Some(attr.to_string());
            }

            let features = root
                .get("features")
                .and_then(|v| v.as_array())
                .ok_or("FeatureCollection missing 'features' array")?;

            for feature_val in features {
                match parse_feature(feature_val) {
                    Ok(mut feats) => layer.features.append(&mut feats),
                    Err(e) => {
                        log::warn!("Skipping invalid feature: {}", e);
                    }
                }
            }
        }
        "Feature" => {
            let feats = parse_feature(&root)?;
            layer.features.extend(feats);
        }
        // Bare geometry (no Feature wrapper)
        "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString"
        | "MultiPolygon" => {
            let geometries = parse_geometry(&root)?;
            for geom in geometries {
                let mut style = FeatureStyle::default();
                apply_geometry_defaults(&mut style, &geom);
                layer.features.push(GeoFeature {
                    geometry: geom,
                    style,
                    properties: HashMap::new(),
                });
            }
        }
        other => {
            return Err(format!("Unsupported GeoJSON type: '{}'", other));
        }
    }

    log::info!(
        "GeoJSON '{}': {} features ({} points, {} lines, {} polygons)",
        layer.name,
        layer.features.len(),
        layer.points().count(),
        layer.lines().count(),
        layer.polygons().count(),
    );

    Ok(layer)
}

/// Loads and parses a GeoJSON file from disk.
pub fn load_geojson_file(path: &Path) -> Result<GeoLayer, String> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let content = fs::read_to_string(path)
        .map_err(|e| format!("Could not read '{}': {}", path.display(), e))?;

    parse_geojson(&content, name)
}

// =============================================================================
// Internal Parsing Helpers
// =============================================================================

/// Parses a single GeoJSON Feature object.
///
/// Returns a Vec because Multi-geometries produce multiple features.
fn parse_feature(val: &Value) -> Result<Vec<GeoFeature>, String> {
    let geom_val = val
        .get("geometry")
        .ok_or("Feature missing 'geometry'")?;

    // Null geometry is allowed per spec (feature with no location)
    if geom_val.is_null() {
        return Ok(Vec::new());
    }

    let properties = parse_properties(val.get("properties"));
    let style = extract_style(&properties);

    let geometries = parse_geometry(geom_val)?;

    Ok(geometries
        .into_iter()
        .map(|geom| {
            let mut feat_style = style.clone();
            apply_geometry_defaults(&mut feat_style, &geom);
            GeoFeature {
                geometry: geom,
                style: feat_style,
                properties: properties.clone(),
            }
        })
        .collect())
}

/// Parses a GeoJSON Geometry object into one or more geometries.
///
/// Multi-types are split into individual geometries.
fn parse_geometry(val: &Value) -> Result<Vec<GeoGeometry>, String> {
    let geo_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or("Geometry missing 'type'")?;

    let coords = val.get("coordinates");

    match geo_type {
        "Point" => {
            let c = coords.ok_or("Point missing 'coordinates'")?;
            let point = parse_coord(c)?;
            Ok(vec![GeoGeometry::Point(point)])
        }
        "LineString" => {
            let c = coords.ok_or("LineString missing 'coordinates'")?;
            let points = parse_coord_array(c)?;
            if points.len() < 2 {
                return Err("LineString needs at least 2 coordinates".into());
            }
            Ok(vec![GeoGeometry::LineString(points)])
        }
        "Polygon" => {
            let c = coords.ok_or("Polygon missing 'coordinates'")?;
            let rings = parse_ring_array(c)?;
            if rings.is_empty() {
                return Err("Polygon needs at least one ring".into());
            }
            Ok(vec![GeoGeometry::Polygon(rings)])
        }
        "MultiPoint" => {
            let c = coords.ok_or("MultiPoint missing 'coordinates'")?;
            let points = parse_coord_array(c)?;
            Ok(points
                .into_iter()
                .map(|p| GeoGeometry::Point(p))
                .collect())
        }
        "MultiLineString" => {
            let c = coords.ok_or("MultiLineString missing 'coordinates'")?;
            let arr = c.as_array().ok_or("Expected array of line arrays")?;
            let mut result = Vec::new();
            for line_val in arr {
                let points = parse_coord_array(line_val)?;
                if points.len() >= 2 {
                    result.push(GeoGeometry::LineString(points));
                }
            }
            Ok(result)
        }
        "MultiPolygon" => {
            let c = coords.ok_or("MultiPolygon missing 'coordinates'")?;
            let arr = c.as_array().ok_or("Expected array of polygon arrays")?;
            let mut result = Vec::new();
            for poly_val in arr {
                let rings = parse_ring_array(poly_val)?;
                if !rings.is_empty() {
                    result.push(GeoGeometry::Polygon(rings));
                }
            }
            Ok(result)
        }
        "GeometryCollection" => {
            let geoms = val
                .get("geometries")
                .and_then(|v| v.as_array())
                .ok_or("GeometryCollection missing 'geometries'")?;
            let mut result = Vec::new();
            for g in geoms {
                result.extend(parse_geometry(g)?);
            }
            Ok(result)
        }
        other => Err(format!("Unknown geometry type: '{}'", other)),
    }
}

/// Parses a single coordinate: [lon, lat] or [lon, lat, alt]
fn parse_coord(val: &Value) -> Result<GeoCoord, String> {
    let arr = val.as_array().ok_or("Coordinate is not an array")?;
    if arr.len() < 2 {
        return Err(format!("Coordinate needs at least 2 values, got {}", arr.len()));
    }

    let lon = arr[0]
        .as_f64()
        .ok_or("Longitude is not a number")?;
    let lat = arr[1]
        .as_f64()
        .ok_or("Latitude is not a number")?;
    let alt = arr.get(2).and_then(|v| v.as_f64());

    Ok(GeoCoord { lon, lat, alt })
}

/// Parses an array of coordinates: [[lon,lat], [lon,lat], ...]
fn parse_coord_array(val: &Value) -> Result<Vec<GeoCoord>, String> {
    let arr = val.as_array().ok_or("Expected array of coordinates")?;
    arr.iter().map(|v| parse_coord(v)).collect()
}

/// Parses an array of rings (for Polygon): [[[lon,lat],...], ...]
fn parse_ring_array(val: &Value) -> Result<Vec<Vec<GeoCoord>>, String> {
    let arr = val.as_array().ok_or("Expected array of rings")?;
    arr.iter().map(|v| parse_coord_array(v)).collect()
}

/// Extracts properties from the GeoJSON "properties" object.
fn parse_properties(val: Option<&Value>) -> HashMap<String, Value> {
    match val {
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => HashMap::new(),
    }
}

// =============================================================================
// Style Extraction
// =============================================================================

/// Extracts visual style from GeoJSON properties.
///
/// Recognizes common conventions used by various GeoJSON producers:
/// - `marker-color` / `fill` / `color` → fill color
/// - `stroke` → outline color
/// - `marker-size` → point size ("small"=4, "medium"=6, "large"=10)
/// - `stroke-width` → line width
/// - `fill-opacity` / `stroke-opacity` → alpha
/// - `name` / `title` / `label` → text label
fn extract_style(props: &HashMap<String, Value>) -> FeatureStyle {
    let mut style = FeatureStyle::default();

    // Color (fill)
    if let Some(color) = props
        .get("marker-color")
        .or_else(|| props.get("fill"))
        .or_else(|| props.get("color"))
        .and_then(|v| v.as_str())
    {
        if let Some(rgba) = parse_css_color(color) {
            style.color = rgba;
            style.has_explicit_color = true;
        }
    }

    // Fill opacity
    if let Some(opacity) = props
        .get("fill-opacity")
        .and_then(|v| v.as_f64())
    {
        style.color[3] = opacity as f32;
    }

    // Stroke color
    if let Some(color) = props
        .get("stroke")
        .and_then(|v| v.as_str())
    {
        if let Some(rgba) = parse_css_color(color) {
            style.stroke_color = rgba;
            style.has_explicit_stroke = true;
        }
    }

    // Stroke opacity
    if let Some(opacity) = props
        .get("stroke-opacity")
        .and_then(|v| v.as_f64())
    {
        style.stroke_color[3] = opacity as f32;
    }

    // Marker size
    if let Some(size) = props.get("marker-size") {
        style.marker_size = match size.as_str() {
            Some("small") => 4.0,
            Some("medium") => 6.0,
            Some("large") => 10.0,
            _ => size.as_f64().unwrap_or(6.0) as f32,
        };
    }

    // Line width
    if let Some(width) = props
        .get("stroke-width")
        .and_then(|v| v.as_f64())
    {
        style.line_width = width as f32;
    }

    // Label
    style.label = props
        .get("name")
        .or_else(|| props.get("title"))
        .or_else(|| props.get("label"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    style
}

/// Applies geometry-specific default colors when no explicit style was set.
///
/// This makes features distinguishable by type when loaded without styling:
/// - Points:   red markers with white outline
/// - Lines:    dodger blue strokes
/// - Polygons: teal fill with lighter teal outline
fn apply_geometry_defaults(style: &mut FeatureStyle, geom: &GeoGeometry) {
    match geom {
        GeoGeometry::Point(_) => {
            if !style.has_explicit_color {
                style.color = [0.9, 0.2, 0.15, 0.9];       // Red
            }
            if !style.has_explicit_stroke {
                style.stroke_color = [1.0, 1.0, 1.0, 1.0]; // White outline
            }
        }
        GeoGeometry::LineString(_) => {
            if !style.has_explicit_color {
                style.color = [0.12, 0.56, 1.0, 0.9];      // Dodger blue
            }
            if !style.has_explicit_stroke {
                style.stroke_color = [0.12, 0.56, 1.0, 0.9];
            }
        }
        GeoGeometry::Polygon(_) => {
            if !style.has_explicit_color {
                style.color = [0.0, 0.5, 0.5, 0.3];        // Teal, semi-transparent
            }
            if !style.has_explicit_stroke {
                style.stroke_color = [0.0, 0.7, 0.7, 0.8]; // Lighter teal
            }
        }
    }
}

/// Parses a CSS color string into RGBA floats.
///
/// Supports:
/// - Hex: #RGB, #RRGGBB, #RRGGBBAA
/// - Functional: rgb(r, g, b), rgba(r, g, b, a)
/// - Named CSS colors: "red", "blue", "steelblue", etc.
///
/// Returns None for unparseable strings.
fn parse_css_color(s: &str) -> Option<[f32; 4]> {
    let trimmed = s.trim();

    // Try hex first
    if let Some(hex) = trimmed.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0])
            }
            _ => None,
        };
    }

    // Try rgb()/rgba() syntax
    if let Some(rgba) = parse_rgb_function(trimmed) {
        return Some(rgba);
    }

    // Named CSS colors (most common subset)
    let (r, g, b) = match trimmed.to_lowercase().as_str() {
        "black"       => (0, 0, 0),
        "white"       => (255, 255, 255),
        "red"         => (255, 0, 0),
        "green"       => (0, 128, 0),
        "blue"        => (0, 0, 255),
        "yellow"      => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "orange"      => (255, 165, 0),
        "purple"      => (128, 0, 128),
        "pink"        => (255, 192, 203),
        "brown"       => (165, 42, 42),
        "gray" | "grey" => (128, 128, 128),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "darkgray" | "darkgrey"   => (169, 169, 169),
        "lime"        => (0, 255, 0),
        "navy"        => (0, 0, 128),
        "teal"        => (0, 128, 128),
        "olive"       => (128, 128, 0),
        "maroon"      => (128, 0, 0),
        "silver"      => (192, 192, 192),
        "gold"        => (255, 215, 0),
        "coral"       => (255, 127, 80),
        "salmon"      => (250, 128, 114),
        "tomato"      => (255, 99, 71),
        "crimson"     => (220, 20, 60),
        "darkred"     => (139, 0, 0),
        "darkgreen"   => (0, 100, 0),
        "darkblue"    => (0, 0, 139),
        "steelblue"   => (70, 130, 180),
        "dodgerblue"  => (30, 144, 255),
        "skyblue"     => (135, 206, 235),
        "indigo"      => (75, 0, 130),
        "violet"      => (238, 130, 238),
        "turquoise"   => (64, 224, 208),
        "khaki"       => (240, 230, 140),
        "tan"         => (210, 180, 140),
        "sienna"      => (160, 82, 45),
        "chocolate"   => (210, 105, 30),
        "forestgreen" => (34, 139, 34),
        "seagreen"    => (46, 139, 87),
        "limegreen"   => (50, 205, 50),
        "orangered"   => (255, 69, 0),
        "firebrick"   => (178, 34, 34),
        "royalblue"   => (65, 105, 225),
        "slategray" | "slategrey" => (112, 128, 144),
        "transparent" => return Some([0.0, 0.0, 0.0, 0.0]),
        _ => return None,
    };

    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
}

/// Parses `rgb(r, g, b)` or `rgba(r, g, b, a)` CSS color syntax.
///
/// Accepts integer values 0-255 for RGB and 0.0-1.0 for alpha.
fn parse_rgb_function(s: &str) -> Option<[f32; 4]> {
    let s = s.trim();

    let (inner, has_alpha) = if let Some(inner) = s.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        (inner, true)
    } else if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        (inner, false)
    } else {
        return None;
    };

    let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();

    if has_alpha && parts.len() != 4 {
        return None;
    }
    if !has_alpha && parts.len() != 3 {
        return None;
    }

    let r = parts[0].parse::<f32>().ok()? / 255.0;
    let g = parts[1].parse::<f32>().ok()? / 255.0;
    let b = parts[2].parse::<f32>().ok()? / 255.0;
    let a = if has_alpha {
        parts[3].parse::<f32>().ok()?
    } else {
        1.0
    };

    Some([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), a.clamp(0.0, 1.0)])
}

// =============================================================================
// Great-Circle Interpolation
// =============================================================================

impl GeoCoord {
    /// Converts to a unit vector on the sphere (f64 precision for interpolation).
    fn to_unit_vec(&self) -> (f64, f64, f64) {
        let lat = self.lat.to_radians();
        let lon = self.lon.to_radians();
        (lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin())
    }

    /// Creates a GeoCoord from a unit vector.
    fn from_unit_vec(x: f64, y: f64, z: f64) -> Self {
        let lat = y.asin().to_degrees();
        let lon = z.atan2(x).to_degrees();
        Self { lon, lat, alt: None }
    }
}

/// Subdivides a great-circle arc between two points.
///
/// Inserts intermediate points every `max_angle_deg` degrees along
/// the shortest path on the sphere (SLERP). Returns all points
/// including start and end.
///
/// This ensures lines follow the Earth's curvature rather than
/// cutting straight through in lat/lon space.
pub fn great_circle_subdivide(
    a: &GeoCoord,
    b: &GeoCoord,
    max_angle_deg: f64,
) -> Vec<GeoCoord> {
    let (ax, ay, az) = a.to_unit_vec();
    let (bx, by, bz) = b.to_unit_vec();

    // Angle between the two points
    let dot = (ax * bx + ay * by + az * bz).clamp(-1.0, 1.0);
    let angle = dot.acos(); // radians
    let angle_deg = angle.to_degrees();

    // If points are very close, no subdivision needed
    if angle_deg < max_angle_deg || angle < 1e-10 {
        return vec![*a, *b];
    }

    let n_segments = (angle_deg / max_angle_deg).ceil() as usize;
    let sin_angle = angle.sin();

    let mut result = Vec::with_capacity(n_segments + 1);

    for i in 0..=n_segments {
        let t = i as f64 / n_segments as f64;
        let sa = ((1.0 - t) * angle).sin() / sin_angle;
        let sb = (t * angle).sin() / sin_angle;

        let x = sa * ax + sb * bx;
        let y = sa * ay + sb * by;
        let z = sa * az + sb * bz;

        result.push(GeoCoord::from_unit_vec(x, y, z));
    }

    result
}

/// Subdivides an entire LineString along great circles.
///
/// Each segment between consecutive coordinates is interpolated
/// independently, with deduplication at segment joins.
pub fn subdivide_linestring(
    coords: &[GeoCoord],
    max_angle_deg: f64,
) -> Vec<GeoCoord> {
    if coords.len() < 2 {
        return coords.to_vec();
    }

    let mut result = Vec::new();

    for i in 0..coords.len() - 1 {
        let sub = great_circle_subdivide(&coords[i], &coords[i + 1], max_angle_deg);
        if i == 0 {
            result.extend(sub);
        } else {
            // Skip first point (duplicate of previous segment's end)
            result.extend(sub.into_iter().skip(1));
        }
    }

    result
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_point() {
        let json = r#"{
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [13.405, 52.52] },
            "properties": { "name": "Berlin" }
        }"#;
        let layer = parse_geojson(json, "test").unwrap();
        assert_eq!(layer.len(), 1);
        match &layer.features[0].geometry {
            GeoGeometry::Point(c) => {
                assert!((c.lon - 13.405).abs() < 1e-6);
                assert!((c.lat - 52.52).abs() < 1e-6);
            }
            _ => panic!("Expected Point"),
        }
        assert_eq!(layer.features[0].style.label.as_deref(), Some("Berlin"));
    }

    #[test]
    fn test_parse_linestring() {
        let json = r#"{
            "type": "Feature",
            "geometry": {
                "type": "LineString",
                "coordinates": [[0,0], [10,10], [20,20]]
            },
            "properties": {}
        }"#;
        let layer = parse_geojson(json, "test").unwrap();
        assert_eq!(layer.len(), 1);
        match &layer.features[0].geometry {
            GeoGeometry::LineString(coords) => assert_eq!(coords.len(), 3),
            _ => panic!("Expected LineString"),
        }
    }

    #[test]
    fn test_parse_feature_collection() {
        let json = r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": { "type": "Point", "coordinates": [0, 0] },
                    "properties": { "name": "Null Island" }
                },
                {
                    "type": "Feature",
                    "geometry": { "type": "Point", "coordinates": [13.405, 52.52] },
                    "properties": { "name": "Berlin" }
                }
            ]
        }"#;
        let layer = parse_geojson(json, "cities").unwrap();
        assert_eq!(layer.len(), 2);
        assert_eq!(layer.name, "cities");
    }

    #[test]
    fn test_multipoint_flattening() {
        let json = r#"{
            "type": "Feature",
            "geometry": {
                "type": "MultiPoint",
                "coordinates": [[0,0], [1,1], [2,2]]
            },
            "properties": { "name": "Triplet" }
        }"#;
        let layer = parse_geojson(json, "test").unwrap();
        // MultiPoint with 3 coords → 3 individual Point features
        assert_eq!(layer.len(), 3);
        assert!(layer.features.iter().all(|f| matches!(f.geometry, GeoGeometry::Point(_))));
        // All share the same properties
        assert!(layer.features.iter().all(|f| f.style.label.as_deref() == Some("Triplet")));
    }

    #[test]
    fn test_css_color_hex() {
        assert_eq!(parse_css_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_css_color("#f00"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_css_color("#00ff0080"), Some([0.0, 1.0, 0.0, 128.0 / 255.0]));
        assert_eq!(parse_css_color("invalid"), None);
        assert_eq!(parse_css_color("#xyz"), None);
    }

    #[test]
    fn test_css_color_rgb_function() {
        let c = parse_css_color("rgb(255, 0, 0)").unwrap();
        assert!((c[0] - 1.0).abs() < 0.01);
        assert!(c[1].abs() < 0.01);

        let c = parse_css_color("rgba(0, 128, 255, 0.5)").unwrap();
        assert!((c[2] - 1.0).abs() < 0.01);
        assert!((c[3] - 0.5).abs() < 0.01);

        assert_eq!(parse_css_color("rgb(0, 0)"), None); // too few args
    }

    #[test]
    fn test_style_extraction() {
        let json = r##"{
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [0, 0] },
            "properties": {
                "marker-color": "#3366ff",
                "marker-size": "large",
                "name": "Test Point"
            }
        }"##;
        let layer = parse_geojson(json, "test").unwrap();
        let style = &layer.features[0].style;
        assert!((style.color[2] - 1.0).abs() < 0.01); // Blue channel ≈ 1.0
        assert_eq!(style.marker_size, 10.0); // "large"
        assert_eq!(style.label.as_deref(), Some("Test Point"));
    }

    #[test]
    fn test_coord_to_unit_sphere() {
        // North pole: lat=90 → y=1
        let pole = GeoCoord::new(0.0, 90.0);
        let [x, y, z] = pole.to_unit_sphere();
        assert!((y - 1.0).abs() < 1e-5);
        assert!(x.abs() < 1e-5);
        assert!(z.abs() < 1e-5);

        // Equator at prime meridian: lat=0, lon=0 → (1, 0, 0)
        let eq = GeoCoord::new(0.0, 0.0);
        let [x, y, z] = eq.to_unit_sphere();
        assert!((x - 1.0).abs() < 1e-5);
        assert!(y.abs() < 1e-5);
        assert!(z.abs() < 1e-5);
    }

    #[test]
    fn test_coord_to_uv() {
        // Top-left corner of equirectangular: lon=-180, lat=90 → (0, 0)
        let tl = GeoCoord::new(-180.0, 90.0);
        assert_eq!(tl.to_uv(), [0.0, 0.0]);

        // Bottom-right: lon=180, lat=-90 → (1, 1)
        let br = GeoCoord::new(180.0, -90.0);
        assert_eq!(br.to_uv(), [1.0, 1.0]);

        // Center: lon=0, lat=0 → (0.5, 0.5)
        let center = GeoCoord::new(0.0, 0.0);
        assert_eq!(center.to_uv(), [0.5, 0.5]);
    }

    #[test]
    fn test_null_geometry_skipped() {
        let json = r#"{
            "type": "Feature",
            "geometry": null,
            "properties": { "name": "No location" }
        }"#;
        let layer = parse_geojson(json, "test").unwrap();
        assert_eq!(layer.len(), 0);
    }

    #[test]
    fn test_polygon() {
        let json = r#"{
            "type": "Feature",
            "geometry": {
                "type": "Polygon",
                "coordinates": [
                    [[0,0], [10,0], [10,10], [0,10], [0,0]]
                ]
            },
            "properties": {}
        }"#;
        let layer = parse_geojson(json, "test").unwrap();
        assert_eq!(layer.len(), 1);
        match &layer.features[0].geometry {
            GeoGeometry::Polygon(rings) => {
                assert_eq!(rings.len(), 1);
                assert_eq!(rings[0].len(), 5); // Closed ring
            }
            _ => panic!("Expected Polygon"),
        }
    }

    #[test]
    fn test_great_circle_subdivide() {
        // Berlin → New York: ~6,400 km ≈ ~57° arc
        let berlin = GeoCoord::new(13.405, 52.52);
        let nyc = GeoCoord::new(-73.9857, 40.7484);
        let points = great_circle_subdivide(&berlin, &nyc, 2.0);

        // At 2° max angle, ~57° arc → ~29 segments → ~30 points
        assert!(points.len() >= 25, "Expected ~30 points, got {}", points.len());
        assert!(points.len() <= 40, "Expected ~30 points, got {}", points.len());

        // First and last should match originals
        assert!((points[0].lon - berlin.lon).abs() < 0.001);
        assert!((points[0].lat - berlin.lat).abs() < 0.001);
        assert!((points.last().unwrap().lon - nyc.lon).abs() < 0.001);
        assert!((points.last().unwrap().lat - nyc.lat).abs() < 0.001);
    }

    #[test]
    fn test_great_circle_short_segment() {
        // Two very close points → no subdivision needed
        let a = GeoCoord::new(0.0, 0.0);
        let b = GeoCoord::new(0.5, 0.5);
        let points = great_circle_subdivide(&a, &b, 2.0);
        assert_eq!(points.len(), 2); // Just start and end
    }

    #[test]
    fn test_subdivide_linestring() {
        let coords = vec![
            GeoCoord::new(0.0, 0.0),
            GeoCoord::new(30.0, 0.0),
            GeoCoord::new(60.0, 0.0),
        ];
        let result = subdivide_linestring(&coords, 5.0);

        // 30° at 5° steps = 6 segments per → 7 points per
        // Two segments: 7 + 6 (first of second segment shared) = 13
        assert!(result.len() >= 10, "Expected ~13 points, got {}", result.len());

        // No duplicate at the join
        let mid_idx = result.len() / 2;
        assert!((result[mid_idx].lon - result[mid_idx - 1].lon).abs() > 0.01);
    }

    #[test]
    fn test_named_css_colors() {
        // Basic named colors
        assert_eq!(parse_css_color("red"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_css_color("blue"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(parse_css_color("green"), Some([0.0, 128.0 / 255.0, 0.0, 1.0]));

        // Case insensitive
        assert_eq!(parse_css_color("Red"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_css_color("STEELBLUE"), parse_css_color("steelblue"));

        // Whitespace trimming
        assert_eq!(parse_css_color(" teal "), Some([0.0, 128.0 / 255.0, 128.0 / 255.0, 1.0]));

        // Transparent
        assert_eq!(parse_css_color("transparent"), Some([0.0, 0.0, 0.0, 0.0]));

        // Unknown name
        assert_eq!(parse_css_color("nonexistent"), None);

        // Aliases
        assert_eq!(parse_css_color("cyan"), parse_css_color("aqua"));
        assert_eq!(parse_css_color("gray"), parse_css_color("grey"));
    }

    #[test]
    fn test_geometry_default_colors() {
        // Features without explicit colors get geometry-specific defaults
        let json = r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": { "type": "Point", "coordinates": [0, 0] },
                    "properties": { "name": "Uncolored Point" }
                },
                {
                    "type": "Feature",
                    "geometry": { "type": "LineString", "coordinates": [[0,0],[1,1]] },
                    "properties": { "name": "Uncolored Line" }
                },
                {
                    "type": "Feature",
                    "geometry": { "type": "Polygon", "coordinates": [[[0,0],[1,0],[1,1],[0,0]]] },
                    "properties": { "name": "Uncolored Polygon" }
                }
            ]
        }"#;

        let layer = parse_geojson(json, "test").unwrap();
        let point_color = layer.features[0].style.color;
        let line_color = layer.features[1].style.color;
        let poly_color = layer.features[2].style.color;

        // All three should have different colors
        assert_ne!(point_color, line_color, "Point and Line should differ");
        assert_ne!(line_color, poly_color, "Line and Polygon should differ");
        assert_ne!(point_color, poly_color, "Point and Polygon should differ");
    }

    #[test]
    fn test_explicit_color_preserved() {
        // Feature with explicit color should NOT get geometry defaults
        let json = r##"{
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [0, 0] },
            "properties": { "marker-color": "#00ff00" }
        }"##;

        let layer = parse_geojson(json, "test").unwrap();
        let color = layer.features[0].style.color;
        assert!((color[1] - 1.0).abs() < 0.01, "Green channel should be ~1.0");
        assert!(layer.features[0].style.has_explicit_color);
    }

    #[test]
    fn test_named_color_in_feature() {
        let json = r#"{
            "type": "Feature",
            "geometry": { "type": "LineString", "coordinates": [[0,0],[1,1]] },
            "properties": { "stroke": "coral" }
        }"#;

        let layer = parse_geojson(json, "test").unwrap();
        let stroke = layer.features[0].style.stroke_color;
        // Coral = (255, 127, 80)
        assert!((stroke[0] - 1.0).abs() < 0.01, "Red channel should be ~1.0");
        assert!(layer.features[0].style.has_explicit_stroke);
    }
}
