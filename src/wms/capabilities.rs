// =============================================================================
// WMS GetCapabilities parsing — minimal, just enough to learn which CRSes a
// layer actually declares support for.
// =============================================================================
//
// Scope: we stream the XML with quick-xml and, for a given target layer
// name, collect every <CRS>/<SRS> declared on that layer and all its
// ancestors (the OGC spec says child layers inherit parent CRSes).
//
// We intentionally do NOT build a full capabilities AST. The only downstream
// consumer is the source-behavior decision — "can I ask for EPSG:3857?" —
// and that answer needs nothing else.
// =============================================================================

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::wms::crs::Crs;

/// What we need from a server's GetCapabilities doc for a specific layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerCapabilities {
    /// Supported CRSes for the layer (own + inherited). Order preserved as
    /// seen in the document — a server usually lists its preferred CRS first.
    pub supported_crs: Vec<Crs>,
}

impl LayerCapabilities {
    #[allow(dead_code)] // consumed by provider wiring (commit 3)
    pub fn supports(&self, crs: Crs) -> bool {
        self.supported_crs.contains(&crs)
    }

    /// Was the layer found in the document at all? An empty result means
    /// either "layer not found" or "layer found but declares no CRSes" —
    /// callers should treat both as "no capability info available".
    #[allow(dead_code)] // consumed by provider wiring (commit 3)
    pub fn is_empty(&self) -> bool {
        self.supported_crs.is_empty()
    }
}

/// Builds the GetCapabilities URL for a WMS endpoint.
#[allow(dead_code)] // consumed by provider wiring (commit 3)
pub fn capabilities_url(base_url: &str, wms_version: &str) -> String {
    format!(
        "{}?SERVICE=WMS&VERSION={}&REQUEST=GetCapabilities",
        base_url, wms_version,
    )
}

/// Parses a Capabilities XML document and returns the CRS list for
/// `target_layer`. Unknown CRSes are silently skipped (we can't use them
/// anyway). Returns an empty result if the layer isn't present.
#[allow(dead_code)] // consumed by provider wiring (commit 3c)
pub fn parse_capabilities(xml: &str, target_layer: &str) -> Result<LayerCapabilities, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    // Stack of CRS lists, one per currently-open <Layer>. The outermost
    // entry is the root <Layer> (if any); the innermost is the layer
    // currently being streamed.
    let mut layer_stack: Vec<Vec<Crs>> = Vec::new();

    // Reading position within the tag we last opened.
    enum Cursor { None, InName, InCrs }
    let mut cursor = Cursor::None;

    // Result set once we see <Name>target_layer</Name>.
    let mut captured: Option<Vec<Crs>> = None;
    // Name of the current (innermost) layer, if we've read it yet. Used to
    // decide "is this the target?" retroactively when the layer closes.
    let mut current_name: Option<String> = None;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(format!("WMS capabilities parse error: {}", e)),
            Ok(Event::Eof) => break,

            Ok(Event::Start(e)) => {
                match e.local_name().as_ref() {
                    b"Layer" => {
                        layer_stack.push(Vec::new());
                        current_name = None;
                    }
                    b"CRS" | b"SRS" => cursor = Cursor::InCrs,
                    b"Name" => cursor = Cursor::InName,
                    _ => {}
                }
            }

            Ok(Event::Text(t)) => match cursor {
                Cursor::InCrs => {
                    let text = t.unescape().map_err(|e| format!("XML text decode: {}", e))?;
                    if let Some(crs) = Crs::parse(&text) {
                        if let Some(top) = layer_stack.last_mut() {
                            if !top.contains(&crs) {
                                top.push(crs);
                            }
                        }
                    }
                }
                Cursor::InName => {
                    let text = t.unescape().map_err(|e| format!("XML text decode: {}", e))?;
                    current_name = Some(text.into_owned());
                }
                Cursor::None => {}
            },

            Ok(Event::End(e)) => {
                match e.local_name().as_ref() {
                    b"CRS" | b"SRS" | b"Name" => cursor = Cursor::None,
                    b"Layer" => {
                        let own = layer_stack.pop().unwrap_or_default();
                        // If this closing layer was our target, flatten the
                        // ancestor stack (outer → inner) plus our own CRSes
                        // into the result.
                        if captured.is_none()
                            && current_name.as_deref() == Some(target_layer)
                        {
                            let mut merged: Vec<Crs> = Vec::new();
                            for level in &layer_stack {
                                for c in level {
                                    if !merged.contains(c) {
                                        merged.push(*c);
                                    }
                                }
                            }
                            for c in &own {
                                if !merged.contains(c) {
                                    merged.push(*c);
                                }
                            }
                            captured = Some(merged);
                        }
                        current_name = None;
                    }
                    _ => {}
                }
            }

            _ => {}
        }
        buf.clear();
    }

    Ok(LayerCapabilities {
        supported_crs: captured.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_layer_lists_its_own_crses() {
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <Name>osm:basemap</Name>
              <CRS>EPSG:4326</CRS>
              <CRS>EPSG:3857</CRS>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps = parse_capabilities(xml, "osm:basemap").unwrap();
        assert_eq!(caps.supported_crs, vec![Crs::EquirectWgs84, Crs::WebMercator]);
        assert!(caps.supports(Crs::WebMercator));
    }

    #[test]
    fn child_layer_inherits_parent_crses() {
        // The OGC WMS spec: child layers inherit CRS declarations from ancestors.
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <CRS>EPSG:4326</CRS>
              <CRS>EPSG:3857</CRS>
              <Layer>
                <Name>child</Name>
              </Layer>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps = parse_capabilities(xml, "child").unwrap();
        assert_eq!(caps.supported_crs, vec![Crs::EquirectWgs84, Crs::WebMercator]);
    }

    #[test]
    fn own_crses_appear_after_inherited_without_duplication() {
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <CRS>EPSG:4326</CRS>
              <Layer>
                <Name>child</Name>
                <CRS>EPSG:3857</CRS>
                <CRS>EPSG:4326</CRS>
              </Layer>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps = parse_capabilities(xml, "child").unwrap();
        // 4326 comes first (inherited), then 3857 (own). Duplicate 4326 drops.
        assert_eq!(caps.supported_crs, vec![Crs::EquirectWgs84, Crs::WebMercator]);
    }

    #[test]
    fn wms_1_1_x_uses_srs_not_crs() {
        // WMS 1.1.x servers declare <SRS> instead of <CRS>. Both must work.
        let xml = r#"
        <WMT_MS_Capabilities version="1.1.1">
          <Capability>
            <Layer>
              <Name>legacy_layer</Name>
              <SRS>EPSG:4326</SRS>
              <SRS>EPSG:900913</SRS>
            </Layer>
          </Capability>
        </WMT_MS_Capabilities>"#;
        let caps = parse_capabilities(xml, "legacy_layer").unwrap();
        // EPSG:900913 is a Web Mercator alias — must be recognised.
        assert_eq!(caps.supported_crs, vec![Crs::EquirectWgs84, Crs::WebMercator]);
    }

    #[test]
    fn unknown_crses_are_skipped_not_erroring() {
        // Regional CRSes like UTM zones — not supported yet but must not
        // break parsing for the CRSes we DO understand.
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <Name>mixed</Name>
              <CRS>EPSG:25832</CRS>
              <CRS>EPSG:3857</CRS>
              <CRS>EPSG:31467</CRS>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps = parse_capabilities(xml, "mixed").unwrap();
        assert_eq!(caps.supported_crs, vec![Crs::WebMercator]);
    }

    #[test]
    fn missing_layer_returns_empty() {
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <Name>other</Name>
              <CRS>EPSG:4326</CRS>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps = parse_capabilities(xml, "not_there").unwrap();
        assert!(caps.is_empty());
    }

    #[test]
    fn sibling_layers_do_not_cross_contaminate() {
        // Two sibling layers under the same parent: the CRSes declared by
        // sibling A must NOT leak into sibling B's result.
        let xml = r#"
        <WMS_Capabilities>
          <Capability>
            <Layer>
              <CRS>EPSG:4326</CRS>
              <Layer>
                <Name>a</Name>
                <CRS>EPSG:3857</CRS>
              </Layer>
              <Layer>
                <Name>b</Name>
              </Layer>
            </Layer>
          </Capability>
        </WMS_Capabilities>"#;
        let caps_a = parse_capabilities(xml, "a").unwrap();
        let caps_b = parse_capabilities(xml, "b").unwrap();
        assert_eq!(caps_a.supported_crs, vec![Crs::EquirectWgs84, Crs::WebMercator]);
        assert_eq!(caps_b.supported_crs, vec![Crs::EquirectWgs84]);
    }

    #[test]
    fn malformed_xml_errors_cleanly() {
        let xml = "<WMS_Capabilities><Layer><Name>oops</Name></Lay";
        assert!(parse_capabilities(xml, "oops").is_err());
    }

    #[test]
    fn capabilities_url_builds_for_common_versions() {
        assert_eq!(
            capabilities_url("https://example.com/wms", "1.3.0"),
            "https://example.com/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetCapabilities",
        );
        assert_eq!(
            capabilities_url("https://example.com/wms", "1.1.1"),
            "https://example.com/wms?SERVICE=WMS&VERSION=1.1.1&REQUEST=GetCapabilities",
        );
    }
}
