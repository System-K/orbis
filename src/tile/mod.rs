// =============================================================================
// Orbis — Tile System (M16a)
// =============================================================================
// XYZ slippy map tile infrastructure for high-resolution zoom.
//
// Provides:
// - Tile coordinate math (lat/lon/zoom -> tile x/y)
// - Pluggable tile sources with URL templates
// - LRU disk cache with configurable size and age limits
// - Worker pool with generation-based cancellation
// - High-level TileManager state machine (single owner for main.rs)
//
// Tile naming follows the OpenStreetMap / "Slippy Map" convention:
//   URL: .../{z}/{x}/{y}.png
//   z = zoom level (0 = whole world, 19 = street level)
//   x = column (0..2^z - 1, left to right)
//   y = row (0..2^z - 1, top to bottom)
//
// Reference: https://wiki.openstreetmap.org/wiki/Slippy_map_tilenames
// =============================================================================

mod coord;
mod source;
mod zoom;
mod cache;
mod fetcher;
mod compositor;
mod worker;
mod manager;

pub use coord::TileCoord;
#[allow(unused_imports)] // TileFormat re-exported for API completeness
pub use source::{TileSource, TileFormat, builtin_tile_sources, builtin_source_ids};
pub use zoom::{level_for, visible_bounds};
pub use cache::{TileCache, CacheConfig};
pub use fetcher::fetch_tile;
pub use compositor::TileCompositor;
#[allow(unused_imports)] // TileFrameResult/TileMetrics/TileUpload wired in via public API (main.rs / Phase 6)
pub use manager::{
    ClearScope, TileFrameResult, TileManager, TileMetrics, TileSettings, TileUpload, ViewState,
};
