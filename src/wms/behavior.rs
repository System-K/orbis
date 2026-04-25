// =============================================================================
// SourceBehavior — what a WMS provider has learned about a remote layer.
// =============================================================================
//
// The Terrestris-class problem: a server can declare it serves EPSG:4326
// while actually delivering Mercator-shaped pixels. We can't trust
// declarations blindly. The defence is a layered decision:
//
//   1. Read GetCapabilities. Pick the most-preferred CRS we both support.
//   2. Prefer EPSG:3857 over EPSG:4326 — Mercator is structurally honest
//      because tile-backed servers ARE Mercator natively, and well-behaved
//      WMS servers implement it correctly through the same engine as 4326.
//      Asking for 3857 dodges the lie no matter who's serving.
//   3. (commit 3b) For the rare case where only 4326 is available, run a
//      self-consistency probe to detect Mercator-shaped output despite the
//      4326 declaration.
//
// The result of that decision is a `SourceBehavior` — what to put in the
// SRS/CRS parameter, what BBOX to use, and what CRS to TREAT the response
// as for the local reprojection step. We persist this per-source so we
// only run discovery once per server (capped by a TTL).
// =============================================================================

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::wms::capabilities::LayerCapabilities;
use crate::wms::crs::Crs;

/// How long a cached behaviour stays valid before we re-run discovery.
/// 30 days strikes a balance: server upgrades happen, but rarely; we don't
/// want to hit GetCapabilities on every cold-start.
const BEHAVIOR_TTL_SECS: u64 = 30 * 24 * 3600;

/// A discovered (or fallback) plan for talking to one WMS source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceBehavior {
    /// Goes in the SRS (1.1.x) / CRS (1.3.0) URL parameter.
    pub request_crs: Crs,
    /// World-extent bbox in `request_crs` units. Stored as four f64 because
    /// `Bbox` doesn't derive Serialize (it's in a different module that
    /// shouldn't depend on serde).
    pub request_bbox: [f64; 4],
    /// Pixel dimensions to request. Mercator world maps are square; equirect
    /// is 2:1. The reprojection engine still produces 2:1 output regardless.
    pub request_width: u32,
    pub request_height: u32,
    /// What CRS the response actually contains. May differ from request_crs
    /// if the probe (commit 3b) detected a server that lies about 4326.
    pub response_crs: Crs,
    /// How we arrived at this plan — for logging and cache freshness checks.
    pub discovery_method: DiscoveryMethod,
    /// UNIX seconds when this behaviour was decided (for TTL).
    pub decided_at_unix: u64,
}

/// Provenance for a SourceBehavior — drives logs and cache invalidation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiscoveryMethod {
    /// Server declares EPSG:3857 in its capabilities — picked structurally.
    PreferredMercator,
    /// Server declares only EPSG:4326; trusted (probe pending in commit 3b).
    DeclaredEquirect,
    /// Probe (commit 3b) confirmed 4326 actually contains 4326 pixels.
    ProbedHonestEquirect,
    /// Probe (commit 3b) caught the server returning Mercator under 4326.
    ProbedDishonestEquirect,
    /// GetCapabilities failed; using a hardcoded default.
    Fallback,
    /// Legacy `reproject_mercator: bool` from JSON config / static layer def.
    LegacyOverride,
}

impl SourceBehavior {
    /// True when the response needs a client-side reprojection pass.
    /// (False only when we're requesting and receiving plain equirect.)
    pub fn needs_reproject(&self) -> bool {
        self.response_crs != Crs::EquirectWgs84
    }

    /// True when this cached behaviour is still fresh.
    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.decided_at_unix) < BEHAVIOR_TTL_SECS
    }

    /// Builds a `SourceBehavior` from parsed capabilities. Returns `None` if
    /// the server declares no CRS we recognise — the caller should then
    /// fall back to a default plan.
    pub fn from_capabilities(caps: &LayerCapabilities) -> Option<Self> {
        let crs = pick_preferred_crs(&caps.supported_crs)?;
        let method = match crs {
            Crs::WebMercator => DiscoveryMethod::PreferredMercator,
            Crs::EquirectWgs84 => DiscoveryMethod::DeclaredEquirect,
        };
        Some(Self::for_crs(crs, method))
    }

    /// Constructs a behaviour for a specific CRS using its world bbox and
    /// the canonical request shape (square for Mercator, 2:1 for equirect).
    pub fn for_crs(crs: Crs, method: DiscoveryMethod) -> Self {
        let bbox = crs.world_bbox();
        // Mercator world is square (same extent both axes); equirect is 2:1.
        let (w, h) = match crs {
            Crs::WebMercator => (2048, 2048),
            Crs::EquirectWgs84 => (2048, 1024),
        };
        Self {
            request_crs: crs,
            request_bbox: [bbox.min_x, bbox.min_y, bbox.max_x, bbox.max_y],
            request_width: w,
            request_height: h,
            response_crs: crs, // updated by probe if it detects a lie
            discovery_method: method,
            decided_at_unix: now_unix(),
        }
    }

    /// Last-resort default: assume legacy equirect and trust the server.
    pub fn fallback_default() -> Self {
        Self::for_crs(Crs::EquirectWgs84, DiscoveryMethod::Fallback)
    }

    /// Backward-compat shim for the legacy `reproject_mercator: bool` field
    /// in static layer defs and user JSON configs. When the flag is set,
    /// we replicate the old behaviour: request 3857, treat as 3857.
    pub fn from_legacy_flag(reproject_mercator: bool) -> Self {
        let crs = if reproject_mercator {
            Crs::WebMercator
        } else {
            Crs::EquirectWgs84
        };
        Self::for_crs(crs, DiscoveryMethod::LegacyOverride)
    }
}

/// CRS preference order — higher rank wins.
///
/// Mercator beats Equirect because:
/// - Tile-backed "WMS" servers (Terrestris, OSM proxies) can't lie about
///   Mercator: their cache IS Mercator. Asking for it returns native pixels.
/// - Proper WMS servers (DWD, GEBCO, Geoserver) implement Mercator through
///   the same battle-tested reprojection engine as everything else, so if
///   they were going to be wrong, they'd already be wrong about 4326 too.
fn preference_rank(crs: Crs) -> u8 {
    match crs {
        Crs::WebMercator => 2,
        Crs::EquirectWgs84 => 1,
    }
}

/// Picks the most-preferred CRS from a server's declared list.
pub fn pick_preferred_crs(supported: &[Crs]) -> Option<Crs> {
    supported.iter().copied().max_by_key(|c| preference_rank(*c))
}

// =============================================================================
// Cache I/O — per-source JSON file alongside the image cache
// =============================================================================

/// Path to the cached behaviour file for a given source ID.
fn cache_path(cache_dir: &Path, source_id: &str) -> PathBuf {
    // Mirror the path-safe id used elsewhere (custom_source replaces ':').
    let safe_id = source_id.replace(':', "_");
    cache_dir.join(format!("{}.behavior.json", safe_id))
}

/// Reads a previously cached behaviour. Returns `None` on missing/malformed
/// cache or if the entry has expired — caller will re-run discovery.
pub fn load_behavior(cache_dir: &Path, source_id: &str) -> Option<SourceBehavior> {
    let path = cache_path(cache_dir, source_id);
    let raw = std::fs::read_to_string(&path).ok()?;
    let behavior: SourceBehavior = serde_json::from_str(&raw).ok()?;
    if !behavior.is_fresh() {
        log::debug!(
            "wms behaviour cache stale for '{}', will re-discover",
            source_id
        );
        return None;
    }
    Some(behavior)
}

/// Persists a discovered behaviour. Failures log a warning but don't error —
/// missing cache just means we re-discover next time.
pub fn save_behavior(cache_dir: &Path, source_id: &str, behavior: &SourceBehavior) {
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        log::warn!(
            "wms behaviour cache: cannot create dir {}: {}",
            cache_dir.display(),
            e
        );
        return;
    }
    let path = cache_path(cache_dir, source_id);
    match serde_json::to_string_pretty(behavior) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!(
                    "wms behaviour cache: write failed for {}: {}",
                    path.display(),
                    e
                );
            }
        }
        Err(e) => log::warn!("wms behaviour cache: serialize failed: {}", e),
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// =============================================================================
// URL building — shared between built-in WmsProvider and CustomWmsProvider
// =============================================================================

/// Builds a WMS GetMap URL using a discovered SourceBehavior.
///
/// Handles the WMS 1.1.x vs 1.3.0 differences:
/// - 1.1.x uses `SRS=`, BBOX always (minx, miny, maxx, maxy)
/// - 1.3.0 uses `CRS=`, BBOX axis order depends on the CRS:
///   - EPSG:4326 (geographic): (minlat, minlon, maxlat, maxlon)
///   - EPSG:3857 (projected):  (minx, miny, maxx, maxy)
///
/// Caller is responsible for appending the `&TIME=...` parameter when the
/// layer is time-aware — kept out of here so this module doesn't depend on
/// chrono.
pub fn build_get_map_url(
    base_url: &str,
    layer_name: &str,
    behavior: &SourceBehavior,
    wms_version: &str,
    format: &str,
    transparent: bool,
) -> String {
    let is_130 = wms_version.starts_with("1.3");
    let crs_param = if is_130 { "CRS" } else { "SRS" };

    let b = &behavior.request_bbox;
    let bbox_str = if is_130 && behavior.request_crs == Crs::EquirectWgs84 {
        // 1.3.0 + geographic CRS: lat-first axis order.
        format!("{},{},{},{}", b[1], b[0], b[3], b[2])
    } else {
        // 1.1.x always, or 1.3.0 + projected CRS: x,y axis order.
        format!("{},{},{},{}", b[0], b[1], b[2], b[3])
    };

    let mut url = format!(
        "{}?SERVICE=WMS&VERSION={}&REQUEST=GetMap&LAYERS={}&FORMAT={}\
         &WIDTH={}&HEIGHT={}&{}={}&BBOX={}&STYLES=",
        base_url,
        wms_version,
        layer_name,
        format,
        behavior.request_width,
        behavior.request_height,
        crs_param,
        behavior.request_crs.epsg_code(),
        bbox_str,
    );
    if transparent {
        url.push_str("&TRANSPARENT=true");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_beats_equirect_in_preference() {
        assert_eq!(
            pick_preferred_crs(&[Crs::EquirectWgs84, Crs::WebMercator]),
            Some(Crs::WebMercator),
        );
        assert_eq!(
            pick_preferred_crs(&[Crs::WebMercator, Crs::EquirectWgs84]),
            Some(Crs::WebMercator),
        );
    }

    #[test]
    fn equirect_only_picks_equirect() {
        assert_eq!(
            pick_preferred_crs(&[Crs::EquirectWgs84]),
            Some(Crs::EquirectWgs84),
        );
    }

    #[test]
    fn empty_list_returns_none() {
        assert_eq!(pick_preferred_crs(&[]), None);
    }

    #[test]
    fn from_capabilities_with_mercator_picks_mercator() {
        let caps = LayerCapabilities {
            supported_crs: vec![Crs::EquirectWgs84, Crs::WebMercator],
        };
        let b = SourceBehavior::from_capabilities(&caps).unwrap();
        assert_eq!(b.request_crs, Crs::WebMercator);
        assert_eq!(b.response_crs, Crs::WebMercator);
        assert_eq!(b.discovery_method, DiscoveryMethod::PreferredMercator);
        assert_eq!(b.request_width, 2048);
        assert_eq!(b.request_height, 2048); // square for Mercator
        assert!(b.needs_reproject());
    }

    #[test]
    fn from_capabilities_with_equirect_only_picks_equirect() {
        let caps = LayerCapabilities {
            supported_crs: vec![Crs::EquirectWgs84],
        };
        let b = SourceBehavior::from_capabilities(&caps).unwrap();
        assert_eq!(b.request_crs, Crs::EquirectWgs84);
        assert_eq!(b.response_crs, Crs::EquirectWgs84);
        assert_eq!(b.discovery_method, DiscoveryMethod::DeclaredEquirect);
        assert_eq!(b.request_height, 1024); // 2:1 for equirect
        assert!(!b.needs_reproject());
    }

    #[test]
    fn from_capabilities_returns_none_for_no_known_crs() {
        let caps = LayerCapabilities { supported_crs: vec![] };
        assert!(SourceBehavior::from_capabilities(&caps).is_none());
    }

    #[test]
    fn legacy_flag_true_maps_to_mercator() {
        let b = SourceBehavior::from_legacy_flag(true);
        assert_eq!(b.request_crs, Crs::WebMercator);
        assert_eq!(b.response_crs, Crs::WebMercator);
        assert_eq!(b.discovery_method, DiscoveryMethod::LegacyOverride);
    }

    #[test]
    fn legacy_flag_false_maps_to_equirect() {
        let b = SourceBehavior::from_legacy_flag(false);
        assert_eq!(b.request_crs, Crs::EquirectWgs84);
        assert_eq!(b.response_crs, Crs::EquirectWgs84);
        assert_eq!(b.discovery_method, DiscoveryMethod::LegacyOverride);
    }

    #[test]
    fn fallback_default_is_safe_equirect() {
        let b = SourceBehavior::fallback_default();
        assert_eq!(b.request_crs, Crs::EquirectWgs84);
        assert_eq!(b.discovery_method, DiscoveryMethod::Fallback);
        assert!(!b.needs_reproject());
    }

    #[test]
    fn fresh_behaviour_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "orbis_behavior_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let written = SourceBehavior::for_crs(Crs::WebMercator, DiscoveryMethod::PreferredMercator);
        save_behavior(&dir, "test:layer", &written);

        let read = load_behavior(&dir, "test:layer").expect("should load");
        assert_eq!(read, written);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_behaviour_is_dropped_on_load() {
        let dir = std::env::temp_dir().join(format!(
            "orbis_behavior_stale_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mut behavior = SourceBehavior::fallback_default();
        // Pretend it was decided long before the TTL.
        behavior.decided_at_unix = now_unix().saturating_sub(BEHAVIOR_TTL_SECS + 100);
        save_behavior(&dir, "stale:layer", &behavior);

        assert!(load_behavior(&dir, "stale:layer").is_none(),
                "stale entry should not load");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_cache_is_dropped_silently() {
        let dir = std::env::temp_dir().join(format!(
            "orbis_behavior_bad_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(cache_path(&dir, "bad:layer"), "not json").unwrap();

        assert!(load_behavior(&dir, "bad:layer").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_builder_uses_crs_param_and_xy_axis_for_3857() {
        let b = SourceBehavior::for_crs(Crs::WebMercator, DiscoveryMethod::PreferredMercator);
        let url = build_get_map_url(
            "https://example.com/wms",
            "OSM-WMS",
            &b,
            "1.3.0",
            "image/png",
            false,
        );
        assert!(url.contains("VERSION=1.3.0"));
        assert!(url.contains("CRS=EPSG:3857"));
        // Mercator BBOX is x,y axis order even in 1.3.0 (projected CRSes don't flip).
        assert!(url.contains("BBOX=-20037508"), "url was: {}", url);
        assert!(!url.contains("&TRANSPARENT="));
    }

    #[test]
    fn url_builder_flips_axes_for_4326_in_130() {
        let b = SourceBehavior::for_crs(Crs::EquirectWgs84, DiscoveryMethod::DeclaredEquirect);
        let url = build_get_map_url(
            "https://example.com/wms",
            "dwd:T",
            &b,
            "1.3.0",
            "image/png",
            true,
        );
        assert!(url.contains("CRS=EPSG:4326"));
        // 1.3.0 + 4326: lat-first axis order. World bbox: minlat=-90, maxlat=90.
        assert!(url.contains("BBOX=-90,-180,90,180"), "url was: {}", url);
        assert!(url.contains("&TRANSPARENT=true"));
    }

    #[test]
    fn url_builder_uses_srs_and_xy_axis_for_4326_in_111() {
        let b = SourceBehavior::for_crs(Crs::EquirectWgs84, DiscoveryMethod::DeclaredEquirect);
        let url = build_get_map_url(
            "https://example.com/wms",
            "legacy_layer",
            &b,
            "1.1.1",
            "image/png",
            false,
        );
        assert!(url.contains("VERSION=1.1.1"));
        // 1.1.x: SRS, not CRS.
        assert!(url.contains("SRS=EPSG:4326"));
        // 1.1.x: always lon-first BBOX even for geographic.
        assert!(url.contains("BBOX=-180,-90,180,90"), "url was: {}", url);
    }

    #[test]
    fn cache_filename_strips_colons_for_windows_safety() {
        // Source IDs like "custom:foo" must not produce a filename containing
        // ':' (illegal on Windows, awkward elsewhere).
        let path = cache_path(Path::new("/tmp"), "custom:foo");
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!file.contains(':'), "filename '{}' contains ':'", file);
        assert_eq!(file, "custom_foo.behavior.json");
    }
}
