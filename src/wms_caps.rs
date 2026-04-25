// =============================================================================
// Orbis — WMS GetCapabilities Parser (M17p)
// =============================================================================
// Fetches and parses WMS GetCapabilities XML to auto-detect:
// - WMS version (1.1.1 / 1.3.0)
// - Available layers (name, title, supported CRS)
// - Supported image formats
//
// Uses quick-xml streaming parser — no DOM tree, low memory footprint.
// Handles both WMS 1.1.1 (<WMT_MS_Capabilities>) and 1.3.0 (<WMS_Capabilities>).
// =============================================================================

use std::collections::HashMap;
use std::sync::mpsc;

// =============================================================================
// Data Structures
// =============================================================================

/// Parsed result of a WMS GetCapabilities response.
#[derive(Debug, Clone)]
pub struct WmsCapabilities {
    /// WMS version reported by the server ("1.1.1" or "1.3.0")
    pub version: String,
    /// All available layers (only those with a <Name> are requestable)
    pub layers: Vec<WmsCapLayer>,
    /// Supported GetMap image formats (e.g. "image/png", "image/jpeg")
    pub formats: Vec<String>,
}

/// A single layer from the capabilities document.
#[derive(Debug, Clone)]
pub struct WmsCapLayer {
    /// Layer identifier used in LAYERS= parameter (empty for group layers)
    pub name: String,
    /// Human-readable title for GUI display
    pub title: String,
    /// All supported CRS/SRS codes (including inherited from parent)
    pub supported_crs: Vec<String>,
    /// Whether this layer supports TIME parameter
    pub has_time: bool,
}

/// CRS selection strategy for Orbis rendering.
#[derive(Debug, Clone)]
pub enum CrsStrategy {
    /// EPSG:4326 — native equirectangular, no reprojection needed
    Equirectangular,
    /// CRS:84 — same as 4326 but guaranteed lon/lat axis order
    Crs84,
    /// EPSG:3857 / 900913 — Web Mercator, needs our existing reprojection
    WebMercator,
    /// Unsupported CRS — show warning, store the first available code
    Unsupported(String),
}

/// Status of a GetCapabilities fetch operation.
#[derive(Debug, Clone)]
pub enum CapsStatus {
    /// Not yet requested
    Idle,
    /// Request in progress
    Loading,
    /// Successfully parsed
    Ready(WmsCapabilities),
    /// Failed with error message
    Error(String),
}

// =============================================================================
// CRS Selection Logic
// =============================================================================

/// Selects the best CRS from a layer's supported list.
///
/// Priority: EPSG:3857 > EPSG:4326 > CRS:84 > unsupported
///
/// We prefer requesting in EPSG:3857 and reprojecting ourselves because:
/// - Our Mercator→Equirectangular reprojection is mathematically correct
/// - Many servers (especially OSM-based) have broken server-side 4326 output
/// - EPSG:3857 images are always square → predictable geometry
/// Only falls back to 4326 when 3857 is not available.
pub fn select_best_crs(supported: &[String]) -> CrsStrategy {
    let has = |code: &str| supported.iter().any(|c| c.eq_ignore_ascii_case(code));

    if has("EPSG:3857") || has("EPSG:900913") {
        CrsStrategy::WebMercator
    } else if has("EPSG:4326") {
        CrsStrategy::Equirectangular
    } else if has("CRS:84") {
        CrsStrategy::Crs84
    } else if let Some(first) = supported.first() {
        CrsStrategy::Unsupported(first.clone())
    } else {
        CrsStrategy::Unsupported("(none)".to_string())
    }
}

/// Returns the CRS string to use in the GetMap request.
#[allow(dead_code)]
pub fn crs_for_request(strategy: &CrsStrategy) -> &str {
    match strategy {
        CrsStrategy::Equirectangular => "EPSG:4326",
        CrsStrategy::Crs84 => "CRS:84",
        CrsStrategy::WebMercator => "EPSG:3857",
        CrsStrategy::Unsupported(code) => code,
    }
}

// =============================================================================
// Background Fetch
// =============================================================================

/// Starts a background thread to fetch and parse WMS GetCapabilities.
///
/// Returns a channel receiver that will deliver the result.
pub fn fetch_capabilities_async(
    base_url: String,
    headers: HashMap<String, String>,
) -> mpsc::Receiver<Result<WmsCapabilities, String>> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = fetch_and_parse(&base_url, &headers);
        let _ = tx.send(result);
    });

    rx
}

/// Fetches GetCapabilities XML and parses it.
fn fetch_and_parse(
    base_url: &str,
    headers: &HashMap<String, String>,
) -> Result<WmsCapabilities, String> {
    // Detect WMTS URLs (different protocol, not supported)
    let url_lower = base_url.to_lowercase();
    if url_lower.contains("wmts") || url_lower.contains("request=getcapabilities&service=wmts") {
        return Err("This appears to be a WMTS (Web Map Tile Service), not a WMS. \
                    Orbis currently only supports WMS sources.".to_string());
    }

    // Build GetCapabilities URL
    let sep = if base_url.contains('?') { "&" } else { "?" };
    let url = format!(
        "{}{}SERVICE=WMS&REQUEST=GetCapabilities",
        base_url, sep,
    );

    log::info!("WMS GetCapabilities: fetching {}", url);

    let mut request = ureq::get(&url);
    for (key, value) in headers {
        request = request.header(key, value);
    }

    let response = request
        .call()
        .map_err(|e| format!("GetCapabilities request failed: {}", e))?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {}", e))?;

    if body.len() < 50 || (!body.contains("Capabilities") && !body.contains("capabilities")) {
        return Err("Response does not appear to be a WMS Capabilities document".to_string());
    }

    log::info!("WMS GetCapabilities: received {} bytes, parsing...", body.len());

    parse_capabilities_xml(&body)
}

// =============================================================================
// XML Parser (quick-xml streaming)
// =============================================================================

/// Parses a WMS GetCapabilities XML document.
fn parse_capabilities_xml(xml: &str) -> Result<WmsCapabilities, String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);

    let mut version = String::new();
    let mut formats: Vec<String> = Vec::new();
    let mut layers: Vec<WmsCapLayer> = Vec::new();

    // Stack to track CRS inheritance through nested <Layer> elements
    let mut crs_stack: Vec<Vec<String>> = Vec::new();
    let mut current_name = String::new();
    let mut current_title = String::new();
    let mut current_crs: Vec<String> = Vec::new();
    let mut current_has_time = false;
    let mut in_layer = false;
    let mut layer_depth: usize = 0;

    // Track which element we're reading text from
    let mut reading_element = ReadingElement::None;
    let mut in_getmap = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();

                match local_name.as_str() {
                    // Root element — extract version
                    "WMS_Capabilities" | "WMT_MS_Capabilities" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"version" {
                                version = String::from_utf8_lossy(&attr.value).to_string();
                            }
                        }
                    }

                    "GetMap" => {
                        in_getmap = true;
                    }

                    // Format inside GetMap → image format
                    "Format" if in_getmap => {
                        reading_element = ReadingElement::Format;
                    }

                    // Layer start — push CRS context
                    "Layer" => {
                        if in_layer {
                            // Entering a child layer — save parent CRS to stack
                            crs_stack.push(current_crs.clone());
                        }
                        in_layer = true;
                        layer_depth += 1;
                        current_name.clear();
                        current_title.clear();
                        // Inherit CRS from parent (if any)
                        current_crs = crs_stack.last().cloned().unwrap_or_default();
                        current_has_time = false;
                    }

                    // Layer child elements
                    "Name" if in_layer => {
                        reading_element = ReadingElement::LayerName;
                    }
                    "Title" if in_layer => {
                        reading_element = ReadingElement::LayerTitle;
                    }
                    // CRS tags (both versions)
                    "CRS" | "SRS" if in_layer => {
                        reading_element = ReadingElement::LayerCrs;
                    }

                    // Dimension element with name="time" → supports TIME
                    "Dimension" if in_layer => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                let val = String::from_utf8_lossy(&attr.value).to_lowercase();
                                if val == "time" {
                                    current_has_time = true;
                                }
                            }
                        }
                    }

                    _ => {}
                }
            }

            Ok(Event::Text(ref e)) => {
                let text = e.unescape()
                    .unwrap_or_default()
                    .trim()
                    .to_string();

                if text.is_empty() {
                    buf.clear();
                    continue;
                }

                match reading_element {
                    ReadingElement::Format => {
                        if text.starts_with("image/") {
                            if !formats.contains(&text) {
                                formats.push(text);
                            }
                        }
                    }
                    ReadingElement::LayerName => {
                        current_name = text;
                    }
                    ReadingElement::LayerTitle => {
                        current_title = text;
                    }
                    ReadingElement::LayerCrs => {
                        if !current_crs.contains(&text) {
                            current_crs.push(text);
                        }
                    }
                    ReadingElement::None => {}
                }

                reading_element = ReadingElement::None;
            }

            Ok(Event::End(ref e)) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();

                match local_name.as_str() {
                    "GetMap" => {
                        in_getmap = false;
                    }

                    "Layer" if in_layer => {
                        // Only add layers that have a <Name> (group layers don't)
                        if !current_name.is_empty() {
                            layers.push(WmsCapLayer {
                                name: current_name.clone(),
                                title: if current_title.is_empty() {
                                    current_name.clone()
                                } else {
                                    current_title.clone()
                                },
                                supported_crs: current_crs.clone(),
                                has_time: current_has_time,
                            });
                        }

                        layer_depth -= 1;
                        if layer_depth == 0 {
                            in_layer = false;
                            crs_stack.clear();
                        } else {
                            // Pop back to parent CRS context
                            current_crs = crs_stack.pop().unwrap_or_default();
                            current_name.clear();
                            current_title.clear();
                            current_has_time = false;
                        }
                    }

                    _ => {}
                }

                reading_element = ReadingElement::None;
            }

            Ok(Event::Eof) => break,

            Err(e) => {
                return Err(format!("XML parse error at position {}: {}", reader.error_position(), e));
            }

            _ => {}
        }

        buf.clear();
    }

    // Fallback version detection from XML content
    if version.is_empty() {
        if xml.contains("version=\"1.3") {
            version = "1.3.0".to_string();
        } else if xml.contains("version=\"1.1") {
            version = "1.1.1".to_string();
        } else {
            version = "unknown".to_string();
        }
    }

    log::info!(
        "WMS GetCapabilities parsed: version={}, {} layers, {} formats",
        version, layers.len(), formats.len()
    );

    Ok(WmsCapabilities {
        version,
        layers,
        formats,
    })
}

/// Tracks which XML element we're currently reading text content from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReadingElement {
    None,
    Format,
    LayerName,
    LayerTitle,
    LayerCrs,
}
