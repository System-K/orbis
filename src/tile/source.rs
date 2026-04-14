// =============================================================================
// Tile Sources
// =============================================================================

use super::TileCoord;

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
            recommended_zoom_bias: -1,
        },
    ]
}

/// Returns the list of built-in tile source IDs with their display names.
///
/// Used by `Settings::sanitize_tile_source` to validate a persisted source ID
/// and fall back to the first built-in source if the stored ID is unknown.
pub fn builtin_source_ids() -> Vec<(String, String)> {
    builtin_tile_sources()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect()
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
}
