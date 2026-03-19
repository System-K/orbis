// =============================================================================
// Orbis — Live Data Source System (M12)
// =============================================================================
// Fetches data from REST APIs on a configurable timer and feeds it
// into the GeoJSON/Marker pipeline.
//
// Architecture:
// - `LiveSourceDef`: static definition of an API endpoint
// - `ActiveLiveSource`: a running source with refresh timer
// - `LiveSourceManager`: manages active sources, background fetches, refresh
// - Results arrive as GeoLayers via mpsc channel, consumed by main loop
//
// Each source provides its own `parse_response` function that converts
// the raw HTTP response body into a GeoLayer. This allows sources with
// non-GeoJSON formats (like OpenSky state vectors) to integrate cleanly.
// =============================================================================

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::geojson::{self, GeoCoord, GeoFeature, GeoGeometry, GeoLayer, FeatureStyle};

// =============================================================================
// Source Definition
// =============================================================================

/// Static definition of a live data source.
///
/// Multiple feed variants can exist per source (e.g. USGS past-hour vs past-day).
#[derive(Debug, Clone)]
pub struct LiveSourceDef {
    /// Unique identifier (e.g. "usgs_earthquakes_day")
    pub id: &'static str,
    /// Display name (e.g. "USGS Earthquakes (past 24h)")
    pub label: &'static str,
    /// Short description (shown in catalog tooltip)
    #[allow(dead_code)]
    pub description: &'static str,
    /// Category for catalog UI
    pub category: LiveSourceCategory,
    /// Attribution text (shown in info panel)
    #[allow(dead_code)]
    pub attribution: &'static str,
    /// Feed URL
    pub url: &'static str,
    /// Auto-refresh interval
    pub refresh: Duration,
    /// Parses the raw HTTP response body into a GeoLayer.
    ///
    /// Arguments: (response_body, layer_name)
    /// Each source implements its own format-specific parsing.
    pub parse_response: fn(&str, &str) -> Result<GeoLayer, String>,
}

/// Categories for live data sources in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveSourceCategory {
    /// Earthquakes, volcanoes, geological events
    Seismic,
    /// Aircraft tracking, flight data
    Aviation,
    // Future: Conflicts, AirQuality, etc.
}

impl LiveSourceCategory {
    /// All categories in display order.
    pub fn all() -> &'static [LiveSourceCategory] {
        &[
            LiveSourceCategory::Seismic,
            LiveSourceCategory::Aviation,
        ]
    }

    /// Emoji + short label for the UI.
    pub fn label(&self) -> &'static str {
        match self {
            LiveSourceCategory::Seismic => "🌋 Seismic",
            LiveSourceCategory::Aviation => "✈ Aviation",
        }
    }

    /// i18n key for the category label.
    #[allow(dead_code)]
    pub fn i18n_key(&self) -> &'static str {
        match self {
            LiveSourceCategory::Seismic => "cat_seismic",
            LiveSourceCategory::Aviation => "cat_aviation",
        }
    }
}

// =============================================================================
// Active Source + Manager
// =============================================================================

/// A live source that is currently active (fetching data).
struct ActiveLiveSource {
    def: &'static LiveSourceDef,
    last_fetch: Option<Instant>,
    pending: Option<mpsc::Receiver<Result<GeoLayer, String>>>,
}

/// Result of a completed live source fetch, ready for the main loop.
pub struct LiveSourceResult {
    #[allow(dead_code)]
    pub source_id: String,
    pub layer: GeoLayer,
}

/// Manages all active live data sources.
///
/// Handles background fetching, auto-refresh timers, and delivers
/// completed GeoLayers to the main loop.
pub struct LiveSourceManager {
    /// Currently active sources
    active: Vec<ActiveLiveSource>,
}

impl LiveSourceManager {
    pub fn new() -> Self {
        Self {
            active: Vec::new(),
        }
    }

    /// Activates a live source. Starts the first fetch immediately.
    pub fn activate(&mut self, def: &'static LiveSourceDef) {
        // Don't add duplicates
        if self.active.iter().any(|a| a.def.id == def.id) {
            log::warn!("LiveSource '{}' is already active", def.id);
            return;
        }

        log::info!("LiveSource: activating '{}'", def.label);
        let mut source = ActiveLiveSource {
            def,
            last_fetch: None,
            pending: None,
        };

        // Start first fetch immediately
        Self::start_fetch(&mut source);
        self.active.push(source);
    }

    /// Deactivates a live source by ID.
    pub fn deactivate(&mut self, id: &str) {
        self.active.retain(|a| a.def.id != id);
        log::info!("LiveSource: deactivated '{}'", id);
    }

    /// Returns whether a source is currently active.
    #[allow(dead_code)]
    pub fn is_active(&self, id: &str) -> bool {
        self.active.iter().any(|a| a.def.id == id)
    }

    /// Returns the IDs of all active sources.
    pub fn active_ids(&self) -> Vec<&'static str> {
        self.active.iter().map(|a| a.def.id).collect()
    }

    /// Polls for completed fetches and triggers auto-refreshes.
    ///
    /// Call this every frame from the main loop.
    /// Returns completed GeoLayers ready to be fed into MarkerSystem.
    pub fn poll(&mut self) -> Vec<LiveSourceResult> {
        let mut results = Vec::new();

        for source in &mut self.active {
            // Check pending fetch
            if let Some(rx) = &source.pending {
                match rx.try_recv() {
                    Ok(Ok(layer)) => {
                        log::info!(
                            "LiveSource '{}': received {} features",
                            source.def.label,
                            layer.len(),
                        );
                        results.push(LiveSourceResult {
                            source_id: source.def.id.to_string(),
                            layer,
                        });
                        source.pending = None;
                        source.last_fetch = Some(Instant::now());
                    }
                    Ok(Err(e)) => {
                        log::error!(
                            "LiveSource '{}' fetch failed: {}",
                            source.def.label,
                            e,
                        );
                        source.pending = None;
                        source.last_fetch = Some(Instant::now()); // Don't retry immediately
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        // Still downloading, keep waiting
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        log::error!(
                            "LiveSource '{}': download thread disconnected",
                            source.def.label,
                        );
                        source.pending = None;
                    }
                }
            }

            // Auto-refresh: start new fetch if interval elapsed and no pending
            if source.pending.is_none() {
                if let Some(last) = source.last_fetch {
                    if last.elapsed() >= source.def.refresh {
                        Self::start_fetch(source);
                    }
                }
            }
        }

        results
    }

    /// Starts a background fetch for a source.
    fn start_fetch(source: &mut ActiveLiveSource) {
        let url = source.def.url.to_string();
        let label = source.def.label.to_string();
        let parse_response = source.def.parse_response;

        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            log::info!("LiveSource '{}': fetching from {}", label, url);
            let result = fetch_and_parse(&url, &label, parse_response);
            let _ = tx.send(result);
        });

        source.pending = Some(rx);
    }
}

// =============================================================================
// HTTP Fetch
// =============================================================================

/// Fetches data from a URL and parses it using the source-specific parser.
fn fetch_and_parse(
    url: &str,
    layer_name: &str,
    parse_response: fn(&str, &str) -> Result<GeoLayer, String>,
) -> Result<GeoLayer, String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let mut layer = parse_response(&body, layer_name)?;

    // Force layer name to source label (overrides FeatureCollection "name")
    // This ensures replace_layer() can match by name on refresh.
    layer.name = layer_name.to_string();

    Ok(layer)
}

// =============================================================================
// USGS Earthquake Feeds (M12a)
// =============================================================================

/// Parses USGS GeoJSON response and applies magnitude-based styling.
fn usgs_parse_response(body: &str, layer_name: &str) -> Result<GeoLayer, String> {
    let mut layer = geojson::parse_geojson(body, layer_name)?;
    usgs_earthquake_post_process(&mut layer);
    Ok(layer)
}

/// Maps earthquake magnitude to a marker color (green → yellow → orange → red).
pub fn magnitude_color(mag: f64) -> [f32; 4] {
    if mag < 2.0 {
        // Minor: green
        [0.2, 0.8, 0.2, 0.9]
    } else if mag < 4.0 {
        // Light: yellow-green to yellow
        let t = ((mag - 2.0) / 2.0) as f32;
        [0.2 + t * 0.8, 0.8, 0.2 * (1.0 - t), 0.9]
    } else if mag < 5.5 {
        // Moderate: yellow to orange
        let t = ((mag - 4.0) / 1.5) as f32;
        [1.0, 0.8 - t * 0.4, 0.0, 0.9]
    } else if mag < 7.0 {
        // Strong: orange to red
        let t = ((mag - 5.5) / 1.5) as f32;
        [1.0, 0.4 - t * 0.35, 0.0, 0.95]
    } else {
        // Major/Great: deep red
        [0.9, 0.05, 0.05, 1.0]
    }
}

/// Maps earthquake magnitude to marker radius in pixels.
fn magnitude_size(mag: f64) -> f32 {
    if mag < 0.0 {
        3.0
    } else {
        // Exponential scaling: M2→4px, M4→6px, M6→10px, M8→16px
        (3.0 + (mag as f32).powf(1.3)).min(24.0)
    }
}

/// Post-processes USGS earthquake GeoJSON features.
///
/// Maps `mag` property to marker size + color.
/// Uses `title` (e.g. "M 5.2 - 10km NW of Tokyo") as label for M4+.
fn usgs_earthquake_post_process(layer: &mut GeoLayer) {
    for feature in &mut layer.features {
        let mag = feature
            .properties
            .get("mag")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        feature.style.color = magnitude_color(mag);
        feature.style.stroke_color = [1.0, 1.0, 1.0, 0.6];
        feature.style.marker_size = magnitude_size(mag);
        feature.style.has_explicit_color = true;
        feature.style.has_explicit_stroke = true;

        // Show label only for significant quakes (M4.0+)
        if mag >= 4.0 {
            feature.style.label = feature
                .properties
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        } else {
            feature.style.label = None;
        }
    }
}

// =============================================================================
// OpenSky Network — Aircraft Tracking (M12b)
// =============================================================================
//
// OpenSky API returns a custom JSON format (not GeoJSON):
// {
//   "time": 1614842673,
//   "states": [
//     [icao24, callsign, origin_country, time_position, last_contact,
//      longitude, latitude, baro_altitude, on_ground, velocity,
//      true_track, vertical_rate, sensors, geo_altitude, squawk,
//      spi, position_source, ...],
//     ...
//   ]
// }
//
// State vector indices:
//  0: icao24         (string)  — ICAO 24-bit transponder address
//  1: callsign       (string)  — callsign (8 chars, may be null)
//  2: origin_country (string)  — country of registration
//  3: time_position  (int)     — Unix timestamp of last position update
//  4: last_contact   (int)     — Unix timestamp of last message
//  5: longitude      (float)   — WGS-84 longitude
//  6: latitude       (float)   — WGS-84 latitude
//  7: baro_altitude  (float)   — barometric altitude in meters (null if unknown)
//  8: on_ground      (bool)    — whether aircraft is on ground
//  9: velocity       (float)   — ground speed in m/s
// 10: true_track     (float)   — heading in degrees clockwise from north
// 11: vertical_rate  (float)   — vertical rate in m/s
// 12: sensors        (array)   — receiver IDs (not useful for us)
// 13: geo_altitude   (float)   — geometric altitude in meters
// 14: squawk         (string)  — transponder squawk code
// 15: spi            (bool)    — special purpose indicator
// 16: position_source (int)    — 0=ADS-B, 1=ASTERIX, 2=MLAT, 3=FLARM
// =============================================================================

/// Parses OpenSky Network JSON response into a GeoLayer.
fn opensky_parse_response(body: &str, layer_name: &str) -> Result<GeoLayer, String> {
    let root: Value =
        serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;

    let states = root
        .get("states")
        .and_then(|v| v.as_array())
        .ok_or("OpenSky response missing 'states' array")?;

    let mut layer = GeoLayer::new(layer_name);

    for state in states {
        let arr = match state.as_array() {
            Some(a) if a.len() >= 13 => a,
            _ => continue, // Skip malformed entries
        };

        // Longitude and latitude are required
        let lon = match arr[5].as_f64() {
            Some(v) => v,
            None => continue, // No position available
        };
        let lat = match arr[6].as_f64() {
            Some(v) => v,
            None => continue,
        };

        // Skip clearly invalid coordinates
        if lon < -180.0 || lon > 180.0 || lat < -90.0 || lat > 90.0 {
            continue;
        }

        let icao24 = arr[0].as_str().unwrap_or("???");
        let callsign = arr[1]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let origin_country = arr[2].as_str().unwrap_or("");
        let baro_altitude = arr[7].as_f64();
        let on_ground = arr[8].as_bool().unwrap_or(false);
        let velocity = arr[9].as_f64();                   // m/s
        let true_track = arr[10].as_f64();                 // degrees
        let vertical_rate = arr[11].as_f64();
        let geo_altitude = arr.get(13).and_then(|v| v.as_f64());
        let squawk = arr.get(14).and_then(|v| v.as_str());

        // Pick best altitude: prefer geometric, fall back to barometric
        let altitude_m = geo_altitude.or(baro_altitude);

        // Color by altitude (ground=gray, low=cyan, mid=green, high=yellow, very high=orange)
        let color = if on_ground {
            [0.5, 0.5, 0.5, 0.7] // Gray for aircraft on ground
        } else {
            altitude_color(altitude_m.unwrap_or(0.0))
        };

        // Marker size: on-ground smaller, airborne larger
        let marker_size = if on_ground { 3.0 } else { 5.0 };

        // Label: show callsign for airborne aircraft
        let label = if !on_ground {
            callsign.map(|s| s.to_string())
        } else {
            None
        };

        // Build properties map for tooltips
        let mut properties = HashMap::new();
        properties.insert("icao24".into(), Value::String(icao24.to_string()));
        if let Some(cs) = callsign {
            properties.insert("callsign".into(), Value::String(cs.to_string()));
        }
        properties.insert("origin_country".into(), Value::String(origin_country.to_string()));
        properties.insert("on_ground".into(), Value::Bool(on_ground));
        if let Some(alt) = altitude_m {
            properties.insert("altitude_m".into(), serde_json::json!(alt));
        }
        if let Some(vel) = velocity {
            // Convert m/s to km/h for readability
            properties.insert("speed_kmh".into(), serde_json::json!((vel * 3.6).round()));
        }
        if let Some(track) = true_track {
            properties.insert("heading".into(), serde_json::json!(track.round()));
        }
        if let Some(vr) = vertical_rate {
            properties.insert("vertical_rate".into(), serde_json::json!(vr));
        }
        if let Some(sq) = squawk {
            properties.insert("squawk".into(), Value::String(sq.to_string()));
        }

        let coord = if let Some(alt) = altitude_m {
            GeoCoord::with_alt(lon, lat, alt)
        } else {
            GeoCoord::new(lon, lat)
        };

        layer.features.push(GeoFeature {
            geometry: GeoGeometry::Point(coord),
            style: FeatureStyle {
                color,
                stroke_color: [1.0, 1.0, 1.0, 0.4],
                marker_size,
                line_width: 1.0,
                label,
                has_explicit_color: true,
                has_explicit_stroke: true,
            },
            properties,
        });
    }

    log::info!(
        "OpenSky '{}': parsed {} aircraft from {} states",
        layer_name,
        layer.features.len(),
        states.len(),
    );

    Ok(layer)
}

/// Maps altitude in meters to a color.
///
/// Gradient: low (cyan) → mid (green) → high (yellow) → very high (orange/red).
/// Uses flight levels as reference:
/// - 0–3,000m (~FL100):   cyan/teal — low altitude, approach/departure
/// - 3,000–8,000m (~FL260): green — medium altitude
/// - 8,000–12,000m (~FL400): yellow — cruise altitude
/// - >12,000m:             orange/red — high cruise (Concorde territory)
pub fn altitude_color(alt_m: f64) -> [f32; 4] {
    if alt_m < 3_000.0 {
        // Low: cyan → green
        let t = (alt_m / 3_000.0).clamp(0.0, 1.0) as f32;
        [0.0, 0.7 + t * 0.1, 0.9 - t * 0.4, 0.85]
    } else if alt_m < 8_000.0 {
        // Mid: green → yellow
        let t = ((alt_m - 3_000.0) / 5_000.0).clamp(0.0, 1.0) as f32;
        [t * 1.0, 0.8, 0.5 * (1.0 - t), 0.85]
    } else if alt_m < 12_000.0 {
        // High: yellow → orange
        let t = ((alt_m - 8_000.0) / 4_000.0).clamp(0.0, 1.0) as f32;
        [1.0, 0.8 - t * 0.3, 0.0, 0.9]
    } else {
        // Very high: orange-red
        [1.0, 0.4, 0.1, 0.9]
    }
}

// =============================================================================
// Smithsonian / GVP — Holocene Volcanoes (M12c)
// =============================================================================
//
// The Smithsonian Global Volcanism Program provides a WFS (Web Feature Service)
// that returns GeoJSON with Holocene volcano data (~1,222 volcanoes).
//
// Properties include: Volcano_Name, Volcano_Number, Primary_Volcano_Type,
// Last_Eruption_Year, Country, Region, Elevation, Tectonic_Setting.
//
// This is essentially a static dataset — volcanoes don't move. Refresh
// interval is set to 24 hours (the database updates a few times per year).
// =============================================================================

/// Parses GVP WFS GeoJSON and applies eruption-history-based styling.
fn gvp_parse_response(body: &str, layer_name: &str) -> Result<GeoLayer, String> {
    let mut layer = geojson::parse_geojson(body, layer_name)?;
    gvp_volcano_post_process(&mut layer);
    Ok(layer)
}

/// Maps last eruption year to a color.
///
/// Recent activity = warm/red, ancient = cool/gray.
pub fn eruption_year_color(year: Option<i64>) -> [f32; 4] {
    match year {
        Some(y) if y >= 1900 => [1.0, 0.2, 0.1, 0.95],     // Active recently: bright red
        Some(y) if y >= 1500 => [1.0, 0.5, 0.1, 0.9],       // Historical: orange
        Some(y) if y >= 0    => [1.0, 0.75, 0.2, 0.85],      // Common Era: yellow-orange
        Some(y) if y >= -5000 => [0.7, 0.75, 0.3, 0.8],     // Mid-Holocene: yellow-green
        Some(_)              => [0.5, 0.55, 0.6, 0.7],       // Early Holocene: gray-blue
        None                 => [0.4, 0.4, 0.45, 0.6],       // Unknown: dim gray
    }
}

/// Post-processes GVP volcano features.
///
/// Colors by last eruption year, labels all volcanoes by name,
/// and sets a uniform marker size (label clustering handles density).
fn gvp_volcano_post_process(layer: &mut GeoLayer) {
    for feature in &mut layer.features {
        let last_eruption = feature
            .properties
            .get("Last_Eruption_Year")
            .and_then(|v| v.as_i64());

        let name = feature
            .properties
            .get("Volcano_Name")
            .and_then(|v| v.as_str());

        let volcano_type = feature
            .properties
            .get("Primary_Volcano_Type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let elevation = feature
            .properties
            .get("Elevation")
            .and_then(|v| v.as_f64());

        feature.style.color = eruption_year_color(last_eruption);
        feature.style.stroke_color = [1.0, 1.0, 1.0, 0.5];
        feature.style.has_explicit_color = true;
        feature.style.has_explicit_stroke = true;

        // Marker size: slightly larger for historically active volcanoes
        feature.style.marker_size = match last_eruption {
            Some(y) if y >= 1900 => 6.0,
            Some(y) if y >= 0    => 5.0,
            _                    => 4.0,
        };

        // Build descriptive label
        if let Some(n) = name {
            let mut label_text = n.to_string();
            if let Some(elev) = elevation {
                label_text.push_str(&format!(" ({}m)", elev as i64));
            }
            if !volcano_type.is_empty() {
                label_text.push_str(&format!(" — {}", volcano_type));
            }
            feature.style.label = Some(label_text);
        }
    }
}

// =============================================================================
// Built-in Source Catalog
// =============================================================================

/// All USGS earthquake feeds.
pub static USGS_FEEDS: &[LiveSourceDef] = &[
    LiveSourceDef {
        id: "usgs_quakes_hour",
        label: "USGS Earthquakes (past hour)",
        description: "All earthquakes in the last 60 minutes",
        category: LiveSourceCategory::Seismic,
        attribution: "USGS Earthquake Hazards Program",
        url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
        refresh: Duration::from_secs(60),
        parse_response: usgs_parse_response,
    },
    LiveSourceDef {
        id: "usgs_quakes_day",
        label: "USGS Earthquakes (past 24h)",
        description: "All earthquakes in the last 24 hours",
        category: LiveSourceCategory::Seismic,
        attribution: "USGS Earthquake Hazards Program",
        url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_day.geojson",
        refresh: Duration::from_secs(300),
        parse_response: usgs_parse_response,
    },
    LiveSourceDef {
        id: "usgs_quakes_week",
        label: "USGS Earthquakes (past 7 days)",
        description: "All earthquakes in the last 7 days (can be many thousands)",
        category: LiveSourceCategory::Seismic,
        attribution: "USGS Earthquake Hazards Program",
        url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_week.geojson",
        refresh: Duration::from_secs(900),
        parse_response: usgs_parse_response,
    },
    LiveSourceDef {
        id: "usgs_quakes_significant",
        label: "USGS Significant Earthquakes (30 days)",
        description: "Only significant earthquakes in the past month",
        category: LiveSourceCategory::Seismic,
        attribution: "USGS Earthquake Hazards Program",
        url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/significant_month.geojson",
        refresh: Duration::from_secs(3600),
        parse_response: usgs_parse_response,
    },
];

/// All OpenSky Network feeds.
///
/// Anonymous access: 400 credits/day, 10s resolution.
/// Global request is expensive (~130 credits), so we default to
/// longer refresh intervals. Bounding box requests cost less.
pub static OPENSKY_FEEDS: &[LiveSourceDef] = &[
    LiveSourceDef {
        id: "opensky_all",
        label: "OpenSky Aircraft (global)",
        description: "All tracked aircraft worldwide (anonymous, ~6000+ planes)",
        category: LiveSourceCategory::Aviation,
        attribution: "The OpenSky Network (https://opensky-network.org)",
        url: "https://opensky-network.org/api/states/all",
        refresh: Duration::from_secs(15),
        parse_response: opensky_parse_response,
    },
    LiveSourceDef {
        id: "opensky_europe",
        label: "OpenSky Aircraft (Europe)",
        description: "Aircraft over Europe (35°N–72°N, 12°W–45°E)",
        category: LiveSourceCategory::Aviation,
        attribution: "The OpenSky Network (https://opensky-network.org)",
        url: "https://opensky-network.org/api/states/all?lamin=35&lamax=72&lomin=-12&lomax=45",
        refresh: Duration::from_secs(10),
        parse_response: opensky_parse_response,
    },
    LiveSourceDef {
        id: "opensky_north_america",
        label: "OpenSky Aircraft (North America)",
        description: "Aircraft over North America (15°N–72°N, 170°W–50°W)",
        category: LiveSourceCategory::Aviation,
        attribution: "The OpenSky Network (https://opensky-network.org)",
        url: "https://opensky-network.org/api/states/all?lamin=15&lamax=72&lomin=-170&lomax=-50",
        refresh: Duration::from_secs(10),
        parse_response: opensky_parse_response,
    },
];

/// Smithsonian / GVP Holocene Volcano feeds.
///
/// Static dataset (~1,222 volcanoes). WFS with GeoJSON output.
/// Refresh once per day since the database only updates a few times per year.
pub static GVP_FEEDS: &[LiveSourceDef] = &[
    LiveSourceDef {
        id: "gvp_holocene_volcanoes",
        label: "GVP Holocene Volcanoes",
        description: "All ~1,222 Holocene volcanoes (Smithsonian GVP database)",
        category: LiveSourceCategory::Seismic,
        attribution: "Smithsonian Institution, Global Volcanism Program (2025)",
        url: "https://webservices.volcano.si.edu/geoserver/GVP-VOTW/ows?service=WFS&version=1.0.0&request=GetFeature&typeName=GVP-VOTW:Smithsonian_VOTW_Holocene_Volcanoes&outputFormat=application/json&maxFeatures=2000",
        refresh: Duration::from_secs(86400),  // once per day
        parse_response: gvp_parse_response,
    },
];

/// Returns all available live source definitions (all categories).
pub fn all_sources() -> Vec<&'static LiveSourceDef> {
    let mut sources: Vec<&'static LiveSourceDef> = Vec::new();
    for s in USGS_FEEDS {
        sources.push(s);
    }
    for s in OPENSKY_FEEDS {
        sources.push(s);
    }
    for s in GVP_FEEDS {
        sources.push(s);
    }
    sources
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magnitude_color_scale() {
        let c_minor = magnitude_color(1.0);
        let c_moderate = magnitude_color(4.5);
        let c_major = magnitude_color(7.5);

        // Minor: green (high G, low R)
        assert!(c_minor[1] > c_minor[0], "Minor quake should be greenish");

        // Moderate: orange (high R, medium G)
        assert!(c_moderate[0] > 0.8, "Moderate quake should have high red");
        assert!(c_moderate[1] < c_moderate[0], "Moderate G < R");

        // Major: deep red
        assert!(c_major[0] > 0.8, "Major quake should be red");
        assert!(c_major[1] < 0.1, "Major quake green should be low");
    }

    #[test]
    fn test_magnitude_size_scale() {
        let s_tiny = magnitude_size(0.5);
        let s_big = magnitude_size(7.0);

        assert!(s_big > s_tiny * 2.0, "M7 should be much larger than M0.5");
        assert!(s_big <= 24.0, "Size should be capped at 24px");
    }

    #[test]
    fn test_negative_magnitude() {
        let s = magnitude_size(-1.0);
        assert_eq!(s, 3.0);

        let c = magnitude_color(-1.0);
        assert_eq!(c, [0.2, 0.8, 0.2, 0.9]);
    }

    #[test]
    fn test_altitude_color_ground() {
        let c = altitude_color(0.0);
        // Low altitude: should be cyan-ish (high B, high G)
        assert!(c[2] > 0.5, "Low altitude should have high blue");
    }

    #[test]
    fn test_altitude_color_cruise() {
        let c = altitude_color(10_000.0);
        // Cruise altitude: should be yellow-ish (high R, high G)
        assert!(c[0] > 0.5, "Cruise altitude should have high red");
        assert!(c[1] > 0.5, "Cruise altitude should have high green");
    }

    #[test]
    fn test_altitude_color_very_high() {
        let c = altitude_color(15_000.0);
        // Very high: orange-red
        assert!(c[0] > 0.8, "Very high should be orange-red");
        assert!(c[1] < 0.6, "Very high green should be moderate");
    }

    #[test]
    fn test_opensky_parse_valid() {
        let json = r#"{
            "time": 1614842673,
            "states": [
                ["abc123", "DLH123 ", "Germany", 1614842670, 1614842673,
                 13.405, 52.52, 10000.0, false, 230.0,
                 45.0, 0.5, null, 10050.0, "1234",
                 false, 0],
                ["def456", null, "France", 1614842670, 1614842673,
                 2.35, 48.86, null, true, 5.0,
                 180.0, 0.0, null, null, null,
                 false, 0]
            ]
        }"#;

        let layer = opensky_parse_response(json, "test").unwrap();
        assert_eq!(layer.len(), 2, "Should parse 2 aircraft");

        // First aircraft: airborne with callsign
        let f0 = &layer.features[0];
        match &f0.geometry {
            GeoGeometry::Point(c) => {
                assert!((c.lon - 13.405).abs() < 0.01);
                assert!((c.lat - 52.52).abs() < 0.01);
            }
            _ => panic!("Expected Point"),
        }
        assert_eq!(f0.style.label.as_deref(), Some("DLH123"));
        assert!(f0.style.marker_size > 3.0, "Airborne should be larger");

        // Second aircraft: on ground, no label
        let f1 = &layer.features[1];
        assert!(f1.style.label.is_none(), "Ground aircraft should have no label");
        assert_eq!(f1.style.marker_size, 3.0, "Ground should be small");
    }

    #[test]
    fn test_opensky_parse_skips_null_position() {
        let json = r#"{
            "time": 1614842673,
            "states": [
                ["abc123", "TEST ", "Germany", 1614842670, 1614842673,
                 null, null, null, false, null,
                 null, null, null, null, null,
                 false, 0]
            ]
        }"#;

        let layer = opensky_parse_response(json, "test").unwrap();
        assert_eq!(layer.len(), 0, "Should skip aircraft with no position");
    }

    #[test]
    fn test_opensky_parse_empty_states() {
        let json = r#"{ "time": 1614842673, "states": [] }"#;
        let layer = opensky_parse_response(json, "test").unwrap();
        assert!(layer.is_empty());
    }

    #[test]
    fn test_eruption_year_color() {
        let c_recent = eruption_year_color(Some(2024));
        let c_historic = eruption_year_color(Some(1600));
        let c_ancient = eruption_year_color(Some(-8000));
        let c_unknown = eruption_year_color(None);

        // Recent: bright red
        assert!(c_recent[0] > 0.9, "Recent should be red");
        assert!(c_recent[1] < 0.3, "Recent red channel dominates");

        // Historic: orange (high R, medium G)
        assert!(c_historic[0] > 0.8, "Historic should have high red");
        assert!(c_historic[1] > 0.3, "Historic should have some green");

        // Ancient: gray-blue (muted)
        assert!(c_ancient[0] < 0.6, "Ancient should be muted");

        // Unknown: dim
        assert!(c_unknown[3] < 0.7, "Unknown should be dim");
    }

    #[test]
    fn test_source_catalog_contains_all_categories() {
        let sources = all_sources();
        assert!(sources.iter().any(|s| s.id == "usgs_quakes_day"));
        assert!(sources.iter().any(|s| s.id == "opensky_all"));
        assert!(sources.iter().any(|s| s.id == "gvp_holocene_volcanoes"));
        assert!(sources.iter().any(|s| s.category == LiveSourceCategory::Aviation));
    }
}
