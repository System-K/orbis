// =============================================================================
// Tile Sources
// =============================================================================

use std::collections::HashMap;

use super::TileCoord;
use crate::custom_source::{CustomSourceConfig, CustomSourcesConfig, SourceType};

/// A tile source with URL template and metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used progressively across M16 substeps
pub struct TileSource {
    /// Unique identifier (e.g. "osm", "sentinel2", "gibs_truecolor")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// URL template with {z}, {x}, {y} placeholders.
    /// Optional: {s} for subdomain rotation, {date} for GIBS.
    pub url_template: String,
    /// Subdomains for load balancing (e.g. ["a", "b", "c"])
    pub subdomains: Vec<String>,
    /// Maximum zoom level supported by this source
    pub max_zoom: u32,
    /// Tile image format
    pub format: TileFormat,
    /// Attribution string (mandatory for display)
    pub attribution: String,
    /// Required HTTP User-Agent (some servers require identification)
    pub user_agent: Option<String>,
    /// Extra HTTP headers to send on every tile request (Authorization,
    /// X-API-Key, Referer, etc.). Empty for built-in sources; populated
    /// from `CustomSourceConfig.headers` for user XYZ entries. A
    /// `User-Agent` entry here overrides the `user_agent` field above.
    pub headers: HashMap<String, String>,
    /// Per-source zoom offset applied by `level_for()`. Higher-resolution sources
    /// (e.g. Sentinel-2 with 10 m/px native) get a positive bias so tiles match
    /// their visual detail; coarse sources (e.g. GIBS daily imagery) use a
    /// negative bias to avoid requesting tiles above their useful resolution.
    pub recommended_zoom_bias: i32,
}

/// Tile image format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TileFormat {
    Png,
    Jpg,
}

impl TileSource {
    /// Builds the download URL for a specific tile.
    pub fn tile_url(&self, coord: &TileCoord, date: Option<&str>) -> String {
        let mut url = self.url_template
            .replace("{z}", &coord.z.to_string())
            .replace("{x}", &coord.x.to_string())
            .replace("{y}", &coord.y.to_string());

        // Subdomain rotation based on tile coordinates
        if !self.subdomains.is_empty() && url.contains("{s}") {
            let idx = (coord.x + coord.y) as usize % self.subdomains.len();
            url = url.replace("{s}", &self.subdomains[idx]);
        }

        // Date substitution (for GIBS)
        if let Some(d) = date {
            url = url.replace("{date}", d);
        }

        url
    }

    /// File extension for cached tiles.
    pub fn extension(&self) -> &str {
        match self.format {
            TileFormat::Png => "png",
            TileFormat::Jpg => "jpg",
        }
    }
}

/// Built-in tile sources.
pub fn builtin_tile_sources() -> Vec<TileSource> {
    vec![
        TileSource {
            id: "sentinel2".into(),
            name: "Sentinel-2 Cloudless".into(),
            url_template: "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2021_3857/default/GoogleMapsCompatible/{z}/{y}/{x}.jpg".into(),
            subdomains: vec![],
            max_zoom: 14,
            format: TileFormat::Jpg,
            attribution: "Sentinel-2 cloudless by EOX (CC BY-NC-SA 4.0)".into(),
            user_agent: None,
            headers: HashMap::new(),
            recommended_zoom_bias: 1,
        },
        TileSource {
            id: "osm".into(),
            name: "OpenStreetMap".into(),
            url_template: "https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            subdomains: vec!["a".into(), "b".into(), "c".into()],
            max_zoom: 19,
            format: TileFormat::Png,
            attribution: "\u{00a9} OpenStreetMap contributors".into(),
            user_agent: Some("Orbis/0.1 (https://github.com/System-K/orbis)".into()),
            headers: HashMap::new(),
            recommended_zoom_bias: 0,
        },
        TileSource {
            id: "gibs_truecolor".into(),
            name: "NASA GIBS True Color".into(),
            url_template: "https://gibs-{s}.earthdata.nasa.gov/wmts/epsg3857/best/VIIRS_SNPP_CorrectedReflectance_TrueColor/default/{date}/GoogleMapsCompatible_Level9/{z}/{y}/{x}.jpg".into(),
            subdomains: vec!["a".into(), "b".into(), "c".into()],
            max_zoom: 9,
            format: TileFormat::Jpg,
            attribution: "NASA GIBS / ESDIS".into(),
            user_agent: None,
            headers: HashMap::new(),
            recommended_zoom_bias: -1,
        },
    ]
}

/// Returns the list of built-in tile source IDs with their display names.
///
/// Settings sanitization now goes through `all_tile_sources` so it can
/// see user-defined XYZ entries; this helper is kept for tests and any
/// callsite that genuinely needs the built-in subset only.
#[allow(dead_code)]
pub fn builtin_source_ids() -> Vec<(String, String)> {
    builtin_tile_sources()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect()
}

// =============================================================================
// Custom XYZ tile sources (user-defined, from custom_sources.json)
// =============================================================================

/// Builds a `TileSource` from a user's custom XYZ config. Returns `None` for
/// configs that aren't XYZ, are disabled, or have a missing/garbled URL
/// template (warns to the log so the user notices).
///
/// The TileSource ID is `custom:<source_id>` so it never collides with a
/// built-in source ID. Display name comes from the user's chosen source name.
pub fn tile_source_from_custom(source: &CustomSourceConfig) -> Option<TileSource> {
    if !matches!(source.source_type, SourceType::Xyz) {
        return None;
    }
    if !source.enabled {
        return None;
    }

    let xyz = source.xyz.as_ref()?;
    let template = xyz.url_template.trim();
    if template.is_empty() {
        log::warn!(
            "Custom XYZ source '{}' has empty url_template — skipping",
            source.id,
        );
        return None;
    }
    if !template.contains("{z}") || !template.contains("{x}") || !template.contains("{y}") {
        log::warn!(
            "Custom XYZ source '{}' url_template is missing one of {{z}}, {{x}}, {{y}}: {} \
             — tiles may fail to download",
            source.id,
            template,
        );
        // Don't reject — some users may have unusual placeholders we can't
        // anticipate. Warn and proceed.
    }

    // Format inference: prefer the explicit field; fall back to URL extension.
    let format = match xyz.format.to_ascii_lowercase().as_str() {
        "png" => TileFormat::Png,
        "jpg" | "jpeg" => TileFormat::Jpg,
        "" => infer_format_from_url(template),
        other => {
            log::warn!(
                "Custom XYZ source '{}' has unrecognised format '{}' — defaulting to PNG",
                source.id,
                other,
            );
            TileFormat::Png
        }
    };

    let max_zoom = xyz.max_zoom.clamp(0, 22);

    Some(TileSource {
        id: format!("custom:{}", source.id),
        name: source.name.clone(),
        url_template: template.to_string(),
        subdomains: xyz.subdomains.clone(),
        max_zoom,
        format,
        attribution: source.attribution.clone(),
        // Conservatively spoof a User-Agent — many tile servers reject bare
        // ureq UA strings (OSM in particular). Users with private servers
        // who need a specific UA can override by adding `User-Agent` to
        // the source's `headers` field (which takes precedence — see the
        // fetcher).
        user_agent: Some("Orbis/0.1 (https://github.com/System-K/orbis)".to_string()),
        headers: source.headers.clone(),
        recommended_zoom_bias: 0,
    })
}

/// Sniffs `.png` vs `.jpg` from the URL template's tail. Default PNG when
/// neither is present (most XYZ servers serve PNG).
fn infer_format_from_url(template: &str) -> TileFormat {
    let lower = template.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        TileFormat::Jpg
    } else {
        TileFormat::Png
    }
}

/// Returns the union of built-in tile sources and user's custom XYZ sources.
///
/// Built-in sources come first (so they're the default selections); custom
/// sources append in config order. Disabled or malformed customs are
/// silently filtered (with a warning logged from `tile_source_from_custom`).
pub fn all_tile_sources(custom: &CustomSourcesConfig) -> Vec<TileSource> {
    let mut sources = builtin_tile_sources();
    for source in &custom.sources {
        if let Some(ts) = tile_source_from_custom(source) {
            sources.push(ts);
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_url_osm() {
        let sources = builtin_tile_sources();
        let osm = sources.iter().find(|s| s.id == "osm").unwrap();
        let coord = TileCoord { z: 10, x: 511, y: 340 };
        let url = osm.tile_url(&coord, None);
        assert!(url.contains("/10/511/340.png"), "url={}", url);
        assert!(url.contains("tile.openstreetmap.org"), "url={}", url);
    }

    #[test]
    fn test_tile_url_sentinel2() {
        let sources = builtin_tile_sources();
        let s2 = sources.iter().find(|s| s.id == "sentinel2").unwrap();
        let coord = TileCoord { z: 8, x: 134, y: 86 };
        let url = s2.tile_url(&coord, None);
        assert!(url.contains("/8/86/134.jpg"), "url={}", url);
    }

    #[test]
    fn test_tile_url_gibs() {
        let sources = builtin_tile_sources();
        let gibs = sources.iter().find(|s| s.id == "gibs_truecolor").unwrap();
        let coord = TileCoord { z: 5, x: 16, y: 11 };
        let url = gibs.tile_url(&coord, Some("2026-03-17"));
        assert!(url.contains("2026-03-17"), "url={}", url);
        assert!(url.contains("/5/11/16.jpg"), "url={}", url);
    }

    #[test]
    fn test_builtin_source_ids_contains_defaults() {
        let ids = builtin_source_ids();
        let id_set: Vec<&str> = ids.iter().map(|(i, _)| i.as_str()).collect();
        assert!(id_set.contains(&"sentinel2"));
        assert!(id_set.contains(&"osm"));
        assert!(id_set.contains(&"gibs_truecolor"));
    }

    // ---- tile_source_from_custom ----

    use crate::custom_source::{CustomSourceConfig, CustomSourcesConfig, SourceType, XyzConfig};
    use std::collections::HashMap;

    fn make_xyz_source(
        id: &str,
        name: &str,
        url: &str,
        format: &str,
        enabled: bool,
    ) -> CustomSourceConfig {
        CustomSourceConfig {
            id: id.to_string(),
            name: name.to_string(),
            source_type: SourceType::Xyz,
            category: "basemap".to_string(),
            attribution: format!("© {}", name),
            default_opacity: 0.5,
            enabled,
            headers: HashMap::new(),
            wms: None,
            xyz: Some(XyzConfig {
                url_template: url.to_string(),
                max_zoom: 18,
                subdomains: Vec::new(),
                format: format.to_string(),
            }),
            rest: None,
            shapefile: None,
            csv: None,
            gpx: None,
        }
    }

    #[test]
    fn xyz_config_converts_to_tile_source() {
        let src = make_xyz_source(
            "user_topo",
            "OpenTopoMap",
            "https://a.tile.opentopomap.org/{z}/{x}/{y}.png",
            "png",
            true,
        );
        let ts = tile_source_from_custom(&src).expect("should convert");
        assert_eq!(ts.id, "custom:user_topo");
        assert_eq!(ts.name, "OpenTopoMap");
        assert_eq!(ts.format, TileFormat::Png);
        assert_eq!(ts.max_zoom, 18);
        assert!(ts.attribution.contains("OpenTopoMap"));
        assert!(ts.user_agent.is_some(), "should send a User-Agent");
    }

    #[test]
    fn id_namespacing_prevents_builtin_collision() {
        // Even if a user names their source "osm", the TileSource id gets
        // "custom:" prefix so it can't displace the built-in "osm".
        let src = make_xyz_source("osm", "My OSM", "https://x.example/{z}/{x}/{y}.png", "png", true);
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.id, "custom:osm");
    }

    #[test]
    fn disabled_source_returns_none() {
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            false,
        );
        assert!(tile_source_from_custom(&src).is_none());
    }

    #[test]
    fn non_xyz_source_returns_none() {
        let mut src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        src.source_type = SourceType::Wms;
        assert!(tile_source_from_custom(&src).is_none());
    }

    #[test]
    fn empty_url_template_returns_none() {
        let src = make_xyz_source("user_x", "X", "", "png", true);
        assert!(tile_source_from_custom(&src).is_none());
    }

    #[test]
    fn missing_xyz_block_returns_none() {
        let mut src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        src.xyz = None;
        assert!(tile_source_from_custom(&src).is_none());
    }

    #[test]
    fn url_without_xyz_placeholders_warns_but_proceeds() {
        // Warn-not-reject: some private servers may use unusual templates.
        let src = make_xyz_source(
            "user_weird",
            "Weird",
            "https://x.example/static.png",
            "png",
            true,
        );
        let ts = tile_source_from_custom(&src).expect("warn but proceed");
        assert_eq!(ts.url_template, "https://x.example/static.png");
    }

    #[test]
    fn jpeg_format_alias_recognised() {
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.jpg",
            "jpeg",
            true,
        );
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.format, TileFormat::Jpg);
    }

    #[test]
    fn empty_format_falls_back_to_url_extension_jpg() {
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.jpg",
            "",
            true,
        );
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.format, TileFormat::Jpg);
    }

    #[test]
    fn empty_format_with_no_extension_defaults_to_png() {
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}",
            "",
            true,
        );
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.format, TileFormat::Png);
    }

    #[test]
    fn unrecognised_format_defaults_to_png_with_warning() {
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.webp",
            "webp",
            true,
        );
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.format, TileFormat::Png);
    }

    #[test]
    fn max_zoom_clamps_to_22() {
        let mut src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        src.xyz.as_mut().unwrap().max_zoom = 99;
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.max_zoom, 22);
    }

    #[test]
    fn custom_headers_pass_through_to_tile_source() {
        // M17g: arbitrary headers (Authorization, X-API-Key, etc.) on the
        // CustomSourceConfig must reach the TileSource so the fetcher can
        // apply them per-request.
        let mut src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        src.headers
            .insert("Authorization".to_string(), "Bearer abc123".to_string());
        src.headers
            .insert("X-API-Key".to_string(), "secret".to_string());

        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.headers.get("Authorization").map(String::as_str), Some("Bearer abc123"));
        assert_eq!(ts.headers.get("X-API-Key").map(String::as_str), Some("secret"));
    }

    #[test]
    fn empty_custom_headers_yield_empty_map_not_default_ua() {
        // The default User-Agent lives in the user_agent field, not in
        // headers, so an empty headers map is the expected default.
        let src = make_xyz_source(
            "user_x",
            "X",
            "https://x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        let ts = tile_source_from_custom(&src).unwrap();
        assert!(ts.headers.is_empty());
        assert!(ts.user_agent.is_some());
    }

    #[test]
    fn subdomains_pass_through() {
        let mut src = make_xyz_source(
            "user_x",
            "X",
            "https://{s}.x.example/{z}/{x}/{y}.png",
            "png",
            true,
        );
        src.xyz.as_mut().unwrap().subdomains =
            vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let ts = tile_source_from_custom(&src).unwrap();
        assert_eq!(ts.subdomains, vec!["a", "b", "c"]);
        // Sanity-check the URL build uses them.
        let coord = TileCoord { z: 5, x: 10, y: 12 };
        let url = ts.tile_url(&coord, None);
        assert!(url.starts_with("https://"));
        // Subdomain index = (10+12) % 3 = 1 → "b"
        assert!(url.contains("://b.x.example/"), "url={}", url);
    }

    // ---- all_tile_sources ----

    fn empty_config() -> CustomSourcesConfig {
        CustomSourcesConfig::default()
    }

    #[test]
    fn all_tile_sources_returns_only_builtins_for_empty_config() {
        let all = all_tile_sources(&empty_config());
        let builtin_count = builtin_tile_sources().len();
        assert_eq!(all.len(), builtin_count);
    }

    #[test]
    fn all_tile_sources_appends_custom_xyz() {
        let mut cfg = empty_config();
        cfg.sources.push(make_xyz_source(
            "user_topo",
            "OpenTopoMap",
            "https://a.tile.opentopomap.org/{z}/{x}/{y}.png",
            "png",
            true,
        ));
        let all = all_tile_sources(&cfg);
        let builtin_count = builtin_tile_sources().len();
        assert_eq!(all.len(), builtin_count + 1);
        assert!(all.iter().any(|s| s.id == "custom:user_topo"));
    }

    #[test]
    fn all_tile_sources_filters_disabled() {
        let mut cfg = empty_config();
        cfg.sources.push(make_xyz_source(
            "user_topo",
            "OpenTopoMap",
            "https://a.tile.opentopomap.org/{z}/{x}/{y}.png",
            "png",
            false, // disabled
        ));
        let all = all_tile_sources(&cfg);
        assert_eq!(all.len(), builtin_tile_sources().len());
    }

    #[test]
    fn all_tile_sources_skips_non_xyz_types() {
        // A WMS source in the custom config doesn't appear in the tile list.
        let mut cfg = empty_config();
        let mut wms_src = make_xyz_source("user_wms", "W", "https://x.example", "png", true);
        wms_src.source_type = SourceType::Wms;
        cfg.sources.push(wms_src);
        let all = all_tile_sources(&cfg);
        assert_eq!(all.len(), builtin_tile_sources().len());
    }
}
