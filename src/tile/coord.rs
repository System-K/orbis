// =============================================================================
// Tile Coordinates
// =============================================================================

/// A tile address in the XYZ grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub z: u32,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    /// Converts geographic coordinates to a tile coordinate at a given zoom level.
    ///
    /// Uses the Web Mercator (EPSG:3857) projection, the de facto standard
    /// for slippy map tiles (OSM, Google Maps, etc.).
    ///
    /// lat: -85.0511..+85.0511 (Mercator limit)
    /// lon: -180..+180
    /// zoom: 0..19
    pub fn from_lat_lon(lat_deg: f64, lon_deg: f64, zoom: u32) -> Self {
        let n = (1u64 << zoom) as f64;
        let lat_rad = lat_deg.to_radians();

        let x = ((lon_deg + 180.0) / 360.0 * n).floor() as u32;
        let y = ((1.0 - lat_rad.tan().asinh() / std::f64::consts::PI) / 2.0 * n).floor() as u32;

        let max = (1u32 << zoom).saturating_sub(1);
        TileCoord {
            z: zoom,
            x: x.min(max),
            y: y.min(max),
        }
    }

    /// Returns the geographic bounding box of this tile as (north, south, east, west).
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let n = (1u64 << self.z) as f64;

        let lon_west = self.x as f64 / n * 360.0 - 180.0;
        let lon_east = (self.x + 1) as f64 / n * 360.0 - 180.0;

        let lat_north = (std::f64::consts::PI * (1.0 - 2.0 * self.y as f64 / n))
            .sinh()
            .atan()
            .to_degrees();
        let lat_south = (std::f64::consts::PI * (1.0 - 2.0 * (self.y + 1) as f64 / n))
            .sinh()
            .atan()
            .to_degrees();

        (lat_north, lat_south, lon_east, lon_west)
    }

    /// Returns the center lat/lon of this tile.
    #[allow(dead_code)] // Utility for future tile prioritization
    pub fn center(&self) -> (f64, f64) {
        let (n, s, e, w) = self.bounds();
        ((n + s) / 2.0, (e + w) / 2.0)
    }

    /// Returns all tiles visible in a bounding box at a given zoom level.
    pub fn tiles_in_view(
        lat_north: f64, lat_south: f64,
        lon_west: f64, lon_east: f64,
        zoom: u32,
    ) -> Vec<TileCoord> {
        let tl = TileCoord::from_lat_lon(lat_north, lon_west, zoom);
        let br = TileCoord::from_lat_lon(lat_south, lon_east, zoom);

        let mut tiles = Vec::new();
        let max = (1u32 << zoom).saturating_sub(1);

        // Handle date-line wraparound
        let (x_start, x_end) = if tl.x <= br.x {
            (tl.x, br.x)
        } else {
            // Wraps around date line — simplified: just use full range
            (0, max)
        };

        for y in tl.y..=br.y.min(max) {
            for x in x_start..=x_end.min(max) {
                tiles.push(TileCoord { z: zoom, x, y });
            }
        }
        tiles
    }
}

impl std::fmt::Display for TileCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_coord_london() {
        // London (51.5N, -0.1W) at zoom 10
        let tile = TileCoord::from_lat_lon(51.5, -0.1, 10);
        assert_eq!(tile.z, 10);
        // Known: London at z10 is approximately x=511, y=340
        assert!((tile.x as i32 - 511).abs() <= 2, "x={}", tile.x);
        assert!((tile.y as i32 - 340).abs() <= 2, "y={}", tile.y);
    }

    #[test]
    fn test_tile_coord_zero_zero() {
        // Null Island (0N, 0E) at zoom 0
        let tile = TileCoord::from_lat_lon(0.0, 0.0, 0);
        assert_eq!(tile.z, 0);
        assert_eq!(tile.x, 0);
        assert_eq!(tile.y, 0);
    }

    #[test]
    fn test_tile_coord_bounds_roundtrip() {
        let tile = TileCoord { z: 5, x: 16, y: 11 };
        let (n, s, e, w) = tile.bounds();
        // Center should be within bounds
        let (clat, clon) = tile.center();
        assert!(clat >= s && clat <= n, "center lat {} not in [{}, {}]", clat, s, n);
        assert!(clon >= w && clon <= e, "center lon {} not in [{}, {}]", clon, w, e);
    }

    #[test]
    fn test_tiles_in_view() {
        // Small area at zoom 3 — should return a handful of tiles
        let tiles = TileCoord::tiles_in_view(55.0, 45.0, 5.0, 15.0, 3);
        assert!(!tiles.is_empty());
        assert!(tiles.len() < 20, "too many tiles: {}", tiles.len());
        for t in &tiles {
            assert_eq!(t.z, 3);
        }
    }
}
