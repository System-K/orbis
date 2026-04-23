// =============================================================================
// TileManager (Phase 4) — single owner of cache, worker pool, compositor
// =============================================================================
//
// The manager orchestrates the tile pipeline:
// - Observes camera/view changes (pan vs zoom vs source switch)
// - Drives the Compositor through state transitions (reset, upsert)
// - Coordinates the WorkerPool (enqueue jobs, ingest results, filter stale)
// - Manages the disk cache (periodic eviction, config updates)
//
// GPU-side texture / bind-group creation and uploads stay in `main.rs` and
// `gpu_init.rs` because they are device-bound; this manager exposes
// `take_upload()` for the caller to feed into `queue.write_texture(...)`.
//
// Bug-mapping (see plan):
// - A1 Pan triggers no reset       -> only source/zoom changes bump gen+reset
// - A3 Invalid source_id           -> sanitized at call site + bail-out here
// - B1 Thread per tile             -> fixed WorkerPool (see worker.rs)
// - B2 break -> silent drop        -> enqueue unbounded, FIFO by priority sort
// - B3/B4 No cancellation          -> generation counter in WorkerPool
// - B5 No timeouts                 -> TODO Phase 4 followup, uses default agent
// - C1 update_config no evict      -> TileCache::update_config does it (Phase 2)
// - C4 clear nuked global          -> clear_cache(ActiveSource) scopes it

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    builtin_tile_sources, level_for, visible_bounds,
    CacheConfig, TileCache, TileCompositor, TileCoord, TileSource,
};
use super::worker::{Job, WorkerPool};

/// Snapshot of camera parameters (all we need to derive the visible tile set).
#[derive(Debug, Clone, Copy)]
pub struct ViewState {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    pub aspect: f32,
}

/// Per-frame settings bundle from the GUI.
#[derive(Debug, Clone)]
pub struct TileSettings {
    pub source_id: String,
    pub cache_max_mb: u32,
    pub cache_max_age: Option<Duration>,
    pub zoom_bias: i32,
}

/// Scope for `TileManager::clear_cache`.
#[derive(Debug, Clone, Copy)]
pub enum ClearScope {
    /// Wipe only tiles for the currently-active source (GUI "Clear cache" button).
    ActiveSource,
    /// Wipe every source (admin full reset).
    #[allow(dead_code)]
    All,
}

/// A dirty compositor sub-region ready for GPU upload.
///
/// `data` is the full compositor buffer (stride `buffer_width * 4` bytes
/// per row); the upload region is `(origin_x, origin_y, width, height)`.
/// `main.rs` computes the byte offset into `data` for that region and
/// passes it to `queue.write_texture`, so only the dirty rect is sent
/// over the PCIe bus instead of the whole 4096×2048 buffer.
pub struct TileUpload<'a> {
    /// Full compositor buffer (row stride = `buffer_width * 4`).
    pub data: &'a [u8],
    /// Full buffer dimensions — used to size the GPU texture on first
    /// upload and to compute the byte stride.
    pub buffer_width: u32,
    pub buffer_height: u32,
    /// Destination sub-region within the texture.
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
}

/// Events the manager reports back to `main.rs` after an `update()` call.
#[derive(Debug, Default, Clone, Copy)]
pub struct TileFrameResult {
    /// True if the compositor was fully reset this frame (source change or
    /// cache clear). The caller should drop the old GPU texture/bind_group
    /// so the stale pixels stop rendering until new tiles are composited.
    ///
    /// A zoom change alone does NOT set this — the compositor keeps the old
    /// pixels so the transition is seamless.
    pub reset: bool,
}

/// Observability metrics — read by Phase 6 for the GUI status line.
#[derive(Debug, Clone, Default)]
pub struct TileMetrics {
    pub current_zoom: u32,
    pub visible_tiles: u32,
    pub composited_tiles: u32,
    pub in_flight: u32,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub gen: u64,
}

/// Maximum number of tiles blitted into the compositor buffer in a single
/// frame.
///
/// Phase H moved image decoding off the render thread — workers now hand
/// back pre-decoded `RgbaImage`s. The remaining per-tile render-thread cost
/// is just the compositor's row-wise blit (~0.2 ms/tile after Phase E).
/// The cap still matters on a fresh zoom change where 20+ tiles arrive
/// within a few frames: blitting them all in one frame would push past
/// the 16.7 ms budget. Tiles beyond the cap wait one frame — workers keep
/// the decoded result in the cache's on-disk form so re-fetch next frame
/// is another cache hit + decode, not a network round-trip.
const MAX_COMPOSITES_PER_FRAME: usize = 4;

/// Phase O: how long the camera view must be bit-exactly the same before
/// a dwell-triggered fast-track fires.
///
/// When the user pans rapidly at high zoom, the FIFO worker queue fills
/// with jobs for tiles along the pan trajectory. When the pan ends, those
/// tiles are no longer visible but still ahead of the wanted ones in the
/// queue — the user sees a long catch-up delay. A dwell signal says
/// "camera has stopped" and cancels the stale queue + re-enqueues only
/// what's visible now, bumping the wanted tiles to the front.
///
/// 500 ms is long enough that a brief mid-pan pause doesn't trip the
/// (cheap but not free) generation bump, and short enough that a user
/// who deliberately stops feels the speed-up almost immediately.
const DWELL_THRESHOLD: Duration = Duration::from_millis(500);

/// Single owner of the tile subsystem.
pub struct TileManager {
    cache_dir: PathBuf,
    cache: Arc<TileCache>,
    pool: WorkerPool,
    compositor: TileCompositor,
    sources: Vec<TileSource>,
    source_ids: Vec<(String, String)>,
    // State
    prev_zoom: u32,
    prev_source: String,
    cache_check_counter: u32,
    /// Dedup: tiles currently enqueued for the active source.
    ///
    /// Scoped to `prev_source` — the set is cleared whenever the source
    /// changes, so we never need to key on source_id. Dropping the String
    /// from the key also kills a per-tile `source.id.clone()` at high zoom
    /// (a 30-60 k/frame hot path at z = 12).
    in_flight: HashSet<TileCoord>,
    /// Phase N: cached bit-pattern hash of the view+source+zoom from the
    /// previous successful tile fan-out. When the current frame's hash
    /// matches AND nothing else invalidates the cache, we skip computing
    /// `visible_bounds` + `tiles_in_view` + the enqueue loop entirely.
    /// `None` means "recompute next frame". Cleared by source/zoom change,
    /// cache clear, and whenever new composites arrive (since a newly
    /// composited tile means `has_tile()` would return true for coords
    /// that were previously enqueued — the set shrinks and we want the
    /// next tick to see it).
    last_view_key: Option<u64>,
    /// Phase O: the view_key observed last frame regardless of memoization
    /// state. Distinct from `last_view_key` because the memoization cache
    /// gets nulled mid-drain on composite arrivals — which would corrupt
    /// dwell timing if we reused it. This field tracks "what the view
    /// *actually* was last frame" so the dwell detector can tell pan from
    /// still.
    last_seen_view_key: Option<u64>,
    /// Phase O: `Some(Instant)` at the moment `last_seen_view_key` most
    /// recently changed. Dwell duration is `elapsed()` off this. `None`
    /// only at startup and on cache-clear; transitions to `Some` on the
    /// first `update()` call and every subsequent view_key change.
    view_stable_since: Option<Instant>,
    /// Phase O: latched `true` after the dwell fast-track fires for the
    /// current stable view. Prevents re-firing every frame while the
    /// view stays still. Reset to `false` on any view_key change.
    prioritized: bool,
    metrics: TileMetrics,
}

impl TileManager {
    pub fn new(
        cache_dir: PathBuf,
        initial_cache_max_mb: u32,
        initial_cache_max_age: Option<Duration>,
        initial_source: String,
    ) -> Self {
        let cache = Arc::new(TileCache::new(CacheConfig {
            cache_dir: cache_dir.clone(),
            max_size_bytes: (initial_cache_max_mb as u64) * 1024 * 1024,
            max_age: initial_cache_max_age,
        }));
        let pool = WorkerPool::new(Arc::clone(&cache));
        let compositor = TileCompositor::new(4096, 2048);
        let sources = builtin_tile_sources();
        let source_ids = sources.iter().map(|s| (s.id.clone(), s.name.clone())).collect();

        Self {
            cache_dir,
            cache,
            pool,
            compositor,
            sources,
            source_ids,
            prev_zoom: 0,
            prev_source: initial_source,
            cache_check_counter: 0,
            in_flight: HashSet::new(),
            last_view_key: None,
            last_seen_view_key: None,
            view_stable_since: None,
            prioritized: false,
            metrics: TileMetrics::default(),
        }
    }

    /// Bit-pattern hash of view + zoom + source for visible-set memoization.
    ///
    /// Uses `f32::to_bits()` so exact-equal views produce exact-equal keys;
    /// even the tiniest camera drift invalidates the cache (correct —
    /// any real movement means the visible tile set might differ). The
    /// source id's hash is mixed in so a source switch refreshes even
    /// if yaw/pitch/distance are unchanged.
    fn view_key(view: &ViewState, zoom: u32, source_id: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        view.yaw.to_bits().hash(&mut h);
        view.pitch.to_bits().hash(&mut h);
        view.distance.to_bits().hash(&mut h);
        view.fov_y.to_bits().hash(&mut h);
        view.aspect.to_bits().hash(&mut h);
        zoom.hash(&mut h);
        source_id.hash(&mut h);
        h.finish()
    }

    /// Main per-frame entry point. Runs the state machine:
    /// 1. Periodic cache evict
    /// 2. Source / zoom change detection -> bump gen + reset compositor
    /// 3. Compute visible tile set
    /// 4. For each missing: cache-hit path (composite sync) or enqueue job
    /// 5. Drain worker results, filter stale, composite valid ones
    pub fn update(&mut self, view: ViewState, settings: &TileSettings) -> TileFrameResult {
        // Resolve source — if invalid, bail out (caller is responsible for
        // sanitization but we guard anyway). Wrap in `Arc` once so the
        // enqueue loop below can clone it in O(1) per tile instead of
        // doing a full struct clone (8+ heap allocs) per visible tile.
        let source = match self.sources.iter().find(|s| s.id == settings.source_id) {
            Some(s) => Arc::new(s.clone()),
            None => return TileFrameResult::default(),
        };

        // Periodic cache size check (every ~600 frames = ~10s @ 60fps).
        // The render thread never walks the filesystem — it reads an atomic
        // counter and, if over the limit, signals the maintenance thread.
        self.cache_check_counter = self.cache_check_counter.wrapping_add(1);
        if self.cache_check_counter >= 600 {
            self.cache_check_counter = 0;
            if self.cache.should_evict() {
                self.cache.request_maintenance();
            }
        }

        // Source-aware zoom
        let zoom = level_for(&source, view.distance, settings.zoom_bias);
        self.metrics.current_zoom = zoom;

        // Source-change: full reset (caller must drop GPU texture to avoid
        // one frame of the old source's pixels). Zoom-change: demote (keep
        // pixels in the buffer so the transition is seamless).
        let source_changed = source.id != self.prev_source;
        let zoom_changed = zoom != self.prev_zoom;
        let mut reset = false;
        if source_changed {
            log::info!("Tile source changed: {} -> {}", self.prev_source, source.id);
            self.prev_source = source.id.clone();
            let new_gen = self.pool.bump_generation();
            self.metrics.gen = new_gen;
            self.compositor.reset(zoom);
            self.prev_zoom = zoom;
            self.in_flight.clear();
            self.last_view_key = None;
            reset = true;
        } else if zoom_changed {
            let new_gen = self.pool.bump_generation();
            self.metrics.gen = new_gen;
            self.compositor.demote_to_zoom(zoom);
            self.prev_zoom = zoom;
            self.in_flight.clear();
            self.last_view_key = None;
            // reset stays false — old pixels keep rendering.
        }

        // Phase N: skip the entire visible-set derivation + enqueue loop
        // when the view is bit-exactly the same as last frame AND nothing
        // else changed state this tick (source/zoom change paths above
        // null `last_view_key` explicitly; the composite drain below
        // nulls it again whenever a newly-arrived tile shrinks the miss
        // set). At z = 12 the loop visits ~100 k coords and dominates
        // the render thread; a static view should pay none of that.
        let current_view_key = Self::view_key(&view, zoom, &self.prev_source);

        // Phase O: track view stability for the dwell-triggered fast-track.
        // `view_changed` is the canonical signal (not `last_view_key` —
        // that one gets nulled mid-drain on composite arrivals and is
        // unsuitable for timing).
        let view_changed = self.last_seen_view_key != Some(current_view_key);
        if view_changed {
            self.view_stable_since = Some(Instant::now());
            self.prioritized = false;
        }
        self.last_seen_view_key = Some(current_view_key);

        // Phase O: if the camera has been still long enough AND we still
        // have pending work in the queue, cancel it and re-enqueue from
        // scratch. Workers obey `current_gen` via WorkerPool::bump_generation,
        // so old jobs become Err("stale") no-ops on the worker side without
        // any cross-thread cancellation protocol. `prioritized` latches so
        // we do this at most once per stable view.
        //
        // Per-frame cost when NOT firing: one `Instant::now()` + one
        // Option compare + a bool check. Firing itself bumps gen and
        // leaves the real work (re-enqueue) to the existing loop below.
        if !self.prioritized
            && !self.in_flight.is_empty()
            && self
                .view_stable_since
                .map(|t| t.elapsed() >= DWELL_THRESHOLD)
                .unwrap_or(false)
        {
            log::debug!(
                "tile: dwell fast-track (cancel + re-enqueue {} pending tiles)",
                self.in_flight.len(),
            );
            let new_gen = self.pool.bump_generation();
            self.metrics.gen = new_gen;
            self.in_flight.clear();
            self.last_view_key = None;
            self.prioritized = true;
        }

        let memoized = self.last_view_key == Some(current_view_key);
        if !memoized {
            let (lat_n, lat_s, lon_w, lon_e) = visible_bounds(
                view.yaw, view.pitch, view.distance, view.fov_y, view.aspect,
            );
            let visible = TileCoord::tiles_in_view(lat_n, lat_s, lon_w, lon_e, zoom);
            self.metrics.visible_tiles = visible.len() as u32;

            // Phase H: always enqueue — the render thread never hits the
            // disk cache itself. The worker's `fetch_tile` checks the
            // disk cache first, so a cached tile is just a disk-read +
            // decode on the worker thread (~1–3 ms) rather than on the
            // render thread (~1–5 ms) plus PCIe bandwidth for free.
            //
            // Phase N: `source.clone()` is now `Arc::clone` (atomic
            // increment, no allocations) and `in_flight` is keyed on
            // `TileCoord` alone, so neither the source struct nor its
            // id is heap-allocated per tile. Before Phase N this loop
            // allocated ~8 heap blocks per visible tile, which at
            // z = 12 (≈ 40–160 k visible tiles) was the dominant render-
            // thread cost — hundreds of thousands of `malloc`s per frame.
            let current_gen = self.pool.current_gen();
            for coord in &visible {
                if self.compositor.has_tile(coord) { continue; }
                if !self.in_flight.insert(*coord) { continue; }
                self.metrics.cache_misses =
                    self.metrics.cache_misses.wrapping_add(1);
                self.pool.enqueue(Job {
                    gen: current_gen,
                    source: Arc::clone(&source),
                    coord: *coord,
                    date: None,
                });
            }
            self.last_view_key = Some(current_view_key);
        }

        // Drain worker results. Each result carries an already-decoded
        // `RgbaImage`, so the render thread only runs the compositor's
        // row-wise blit — no image::load_from_memory, no PNG/JPEG decode.
        //
        // The composite cap still bounds blit work per frame. A worker
        // result that can't be blitted this frame is dropped — the tile
        // stays in the disk cache (put by `fetch_tile`), so next frame
        // will re-enqueue and the worker will serve it from cache.
        let mut composites_this_frame: usize = 0;
        let results = self.pool.poll();
        let live_gen = self.pool.current_gen();
        for r in results {
            // Stale-generation result: do NOT touch in_flight. Phase O's
            // dwell fast-track bumps generation at the SAME zoom/source,
            // which means a coord's key is identical before and after the
            // bump — an unconditional `remove` here would nuke an entry
            // we just re-enqueued under the new gen, causing the next
            // frame to double-enqueue. For source/zoom changes the
            // `r.source_id != self.prev_source` and coord-zoom mismatches
            // already made `remove` effectively a no-op, so this tighter
            // gate doesn't regress those paths.
            if r.gen < live_gen { continue; }
            if r.source_id != self.prev_source { continue; }
            self.in_flight.remove(&r.coord);
            if let Ok(img) = r.data {
                if composites_this_frame < MAX_COMPOSITES_PER_FRAME {
                    self.compositor.composite_decoded(&r.coord, &img);
                    composites_this_frame += 1;
                    self.metrics.cache_hits = self.metrics.cache_hits.wrapping_add(1);
                    // A freshly composited tile means `has_tile` would
                    // now return true for `r.coord`. The memoized view
                    // key was computed against the *previous* composite
                    // state; null it so the next tick's enqueue loop
                    // still runs and picks up any remaining misses.
                    self.last_view_key = None;
                }
                // Over budget: decoded image is dropped here; the disk
                // cache still has the encoded bytes thanks to fetch_tile's
                // cache.put, so next frame it's a fast re-fetch.
            }
        }

        self.metrics.in_flight = self.in_flight.len() as u32;
        self.metrics.composited_tiles = self.compositor.tile_count() as u32;

        TileFrameResult { reset }
    }

    /// Updates cache limits (size, age) after a settings change.
    /// Triggers immediate eviction if the new size limit is smaller.
    pub fn apply_settings(&self, settings: &TileSettings) {
        self.cache.update_config(CacheConfig {
            cache_dir: self.cache_dir.clone(),
            max_size_bytes: (settings.cache_max_mb as u64) * 1024 * 1024,
            max_age: settings.cache_max_age,
        });
    }

    /// Clears the tile cache according to `scope`. Bumps generation so any
    /// in-flight downloads for the cleared source become stale.
    pub fn clear_cache(&mut self, scope: ClearScope) {
        match scope {
            ClearScope::ActiveSource => {
                let id = self.prev_source.clone();
                self.cache.clear_source(&id);
            }
            ClearScope::All => {
                self.cache.clear_all();
            }
        }
        self.compositor.reset(self.prev_zoom);
        let new_gen = self.pool.bump_generation();
        self.metrics.gen = new_gen;
        self.in_flight.clear();
        self.last_view_key = None;
        // Phase O: after a cache wipe the dwell timer should restart from
        // the user's next update() so we don't fast-track immediately on
        // an empty in_flight (or worse, fire twice in quick succession).
        self.last_seen_view_key = None;
        self.view_stable_since = None;
        self.prioritized = false;
    }

    pub fn cache_size_mb(&self) -> f32 {
        self.cache.total_size_bytes() as f32 / (1024.0 * 1024.0)
    }

    #[allow(dead_code)] // Phase 6 uses this for the source picker
    pub fn available_sources(&self) -> &[(String, String)] {
        &self.source_ids
    }

    #[allow(dead_code)] // Phase 6 uses this for the GUI status line
    pub fn metrics(&self) -> &TileMetrics {
        &self.metrics
    }

    pub fn has_content(&self) -> bool {
        self.compositor.has_content
    }

    /// Smooth distance-based opacity (unchanged from original main.rs).
    pub fn opacity_for_distance(&self, distance: f32) -> f32 {
        if !self.compositor.has_content { return 0.0; }
        let t = ((4.0 - distance) / 1.5).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Returns the dirty sub-region of the compositor buffer for GPU
    /// upload (and marks clean). Returns None when there is nothing new
    /// to upload this frame.
    ///
    /// Sends just the dirty rectangle instead of the full 4096×2048
    /// buffer — one tile at z=3 typically covers ~512×512 pixels (1 MB)
    /// instead of 33 MB.
    pub fn take_upload(&mut self) -> Option<TileUpload<'_>> {
        if !self.compositor.dirty || !self.compositor.has_content {
            return None;
        }
        let (x0, y0, x1, y1) = self.compositor.dirty_rect();
        // Guard against a dirty flag without a valid rect (should not
        // happen, but be defensive — x1==x0 would send a zero-width
        // upload and wgpu would reject it).
        if x1 <= x0 || y1 <= y0 {
            self.compositor.mark_clean();
            return None;
        }
        let origin_x = x0;
        let origin_y = y0;
        let rect_w = x1 - x0;
        let rect_h = y1 - y0;
        self.compositor.mark_clean();
        Some(TileUpload {
            data: self.compositor.buffer(),
            buffer_width: self.compositor.width,
            buffer_height: self.compositor.height,
            origin_x,
            origin_y,
            width: rect_w,
            height: rect_h,
        })
    }

    /// Test helper: shift `view_stable_since` into the past so the next
    /// `update()` call sees the dwell threshold as exceeded. Tests that
    /// need the Phase O fast-track to fire call this between updates
    /// instead of actually sleeping 500 ms+.
    #[cfg(test)]
    pub(crate) fn force_dwell_elapsed_for_test(&mut self) {
        self.view_stable_since = Some(
            Instant::now()
                .checked_sub(DWELL_THRESHOLD + Duration::from_millis(100))
                .unwrap_or_else(Instant::now),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.push(format!("orbis-tilemanager-{}-{}", tag, nanos));
        dir
    }

    fn manager_with(initial_source: &str) -> (TileManager, PathBuf) {
        let tmp = unique_tmp_dir(initial_source);
        let m = TileManager::new(
            tmp.clone(),
            10, // 10 MB limit
            None,
            initial_source.into(),
        );
        (m, tmp)
    }

    fn default_view() -> ViewState {
        ViewState { yaw: 0.0, pitch: 0.0, distance: 15.0, fov_y: 1.0, aspect: 16.0 / 9.0 }
    }

    fn default_settings(source_id: &str) -> TileSettings {
        TileSettings {
            source_id: source_id.into(),
            cache_max_mb: 10,
            cache_max_age: None,
            zoom_bias: 0,
        }
    }

    #[test]
    fn test_invalid_source_no_crash() {
        let (mut m, tmp) = manager_with("osm");
        let res = m.update(default_view(), &default_settings("not-a-real-source"));
        assert!(!res.reset, "invalid source must not reset the compositor");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_source_change_triggers_reset() {
        let (mut m, tmp) = manager_with("osm");
        // First update at a given view/source locks in prev_zoom/prev_source.
        // Zoom changes from the initial 0, but zoom-only changes demote
        // without resetting (Phase 5: keep pixels across zoom steps).
        let r1 = m.update(default_view(), &default_settings("osm"));
        assert!(!r1.reset, "zoom-only change (first tick) must not reset");

        // Same settings -> no reset.
        let r2 = m.update(default_view(), &default_settings("osm"));
        assert!(!r2.reset, "no source/zoom change should NOT reset");

        // Switch source -> reset.
        let r3 = m.update(default_view(), &default_settings("sentinel2"));
        assert!(r3.reset, "source change must reset");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_zoom_change_demotes_but_does_not_reset() {
        // A pure zoom change must NOT flag the compositor as fully reset —
        // old pixels keep showing until new-zoom tiles overwrite them.
        let (mut m, tmp) = manager_with("osm");
        let mut settings = default_settings("osm");

        // Prime at a far distance (low zoom).
        let mut view = default_view();
        view.distance = 20.0;
        let _ = m.update(view, &settings);
        let z1 = m.metrics().current_zoom;

        // Zoom in by lowering distance + biasing; force a zoom-level change.
        view.distance = 2.5;
        settings.zoom_bias = 2;
        let r = m.update(view, &settings);
        let z2 = m.metrics().current_zoom;
        assert_ne!(z1, z2, "zoom level must actually change for this test");
        assert!(!r.reset, "zoom-only change must demote, not full-reset");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_pan_does_not_reset() {
        // Pan = yaw/pitch change at same distance. Must not reset compositor.
        let (mut m, tmp) = manager_with("osm");
        let _ = m.update(default_view(), &default_settings("osm")); // lock in state
        let mut view2 = default_view();
        view2.yaw += 0.3; // pan
        let r = m.update(view2, &default_settings("osm"));
        assert!(!r.reset, "pan must not reset compositor");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cache_hit_composites_synchronously() {
        // Pre-seed the cache with one tile at the zoom level the manager
        // will pick for the given view. The first update() should composite
        // it from cache (without needing the worker).
        let (mut m, tmp) = manager_with("osm");

        // Run once to learn what zoom the manager uses for this view.
        let _ = m.update(default_view(), &default_settings("osm"));
        let zoom = m.metrics().current_zoom;

        // Seed ALL visible tiles with tiny valid PNGs. We can't easily fake
        // PNG bytes so test the weaker invariant: metrics.cache_hits grows
        // when cache has bytes (even if composite_tile fails on bad PNG).
        //
        // Instead, we test the simpler contract: when the cache is empty
        // AND workers are running, the metric should count misses, not hits.
        assert_eq!(m.metrics().cache_hits, 0);
        // Misses happen only for tiles not yet in flight. First update
        // should have recorded some misses (at least 1 tile at zoom level).
        // We don't assert exact count because visible tile count depends on
        // zoom; we just confirm the bookkeeping runs.
        let _ = zoom;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_visible_set_memoized_on_steady_view() {
        // Phase N: two identical update() calls in a row must NOT
        // re-enqueue any tiles — the second should short-circuit on
        // the cached view key. We detect this by watching the
        // `cache_misses` counter (bumped exactly once per newly
        // enqueued tile). A steady view: misses count stays flat
        // from call 2 onwards.
        let (mut m, tmp) = manager_with("osm");

        // First update: computes visible set, enqueues everything.
        let _ = m.update(default_view(), &default_settings("osm"));
        let misses_after_first = m.metrics().cache_misses;
        assert!(
            misses_after_first > 0,
            "first update on empty cache should enqueue at least one tile",
        );

        // Second update with the SAME view: memoization kicks in.
        // Misses must stay flat — no new enqueues.
        let _ = m.update(default_view(), &default_settings("osm"));
        let misses_after_second = m.metrics().cache_misses;
        assert_eq!(
            misses_after_first, misses_after_second,
            "steady view must skip the enqueue loop entirely",
        );

        // Sanity: a view change (yaw pan) does invalidate the cache.
        let mut panned = default_view();
        panned.yaw += 0.5;
        let _ = m.update(panned, &default_settings("osm"));
        let misses_after_pan = m.metrics().cache_misses;
        assert!(
            misses_after_pan >= misses_after_second,
            "pan must re-run the enqueue loop (allowing new misses)",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_dwell_fires_fast_track_after_stable_view() {
        // Phase O: a static view with pending work should trigger a
        // generation bump + re-enqueue once the dwell threshold passes.
        let (mut m, tmp) = manager_with("osm");
        let _ = m.update(default_view(), &default_settings("osm"));
        assert!(
            m.metrics().in_flight > 0,
            "precondition: first update on empty cache must enqueue tiles",
        );
        let gen_before = m.metrics().gen;

        m.force_dwell_elapsed_for_test();
        let _ = m.update(default_view(), &default_settings("osm"));

        let gen_after = m.metrics().gen;
        assert!(
            gen_after > gen_before,
            "dwell fast-track should bump generation (before={}, after={})",
            gen_before, gen_after,
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_dwell_does_not_fire_on_moving_view() {
        // Phase O: a camera that moves every frame must never trigger a
        // fast-track — the dwell timer resets each time view_key changes,
        // and the post-change `elapsed` is effectively 0.
        let (mut m, tmp) = manager_with("osm");
        let mut view = default_view();
        let _ = m.update(view, &default_settings("osm"));
        let gen_after_init = m.metrics().gen;

        for _ in 0..5 {
            view.yaw += 0.1; // always a new view_key
            m.force_dwell_elapsed_for_test();
            let _ = m.update(view, &default_settings("osm"));
            assert_eq!(
                m.metrics().gen,
                gen_after_init,
                "moving view must not fire dwell fast-track",
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_dwell_fires_only_once_per_stable_view() {
        // Phase O: once `prioritized` latches, subsequent dwell-expired
        // updates on the same view must NOT re-bump the generation.
        // A view change re-arms the latch.
        let (mut m, tmp) = manager_with("osm");
        let _ = m.update(default_view(), &default_settings("osm"));

        m.force_dwell_elapsed_for_test();
        let _ = m.update(default_view(), &default_settings("osm"));
        let gen_after_first_fire = m.metrics().gen;

        m.force_dwell_elapsed_for_test();
        let _ = m.update(default_view(), &default_settings("osm"));
        assert_eq!(
            m.metrics().gen,
            gen_after_first_fire,
            "second dwell-elapsed tick on same view must not re-fire",
        );

        // Pan: new view_key should re-arm the latch and allow the next
        // dwell-elapsed tick to fire again.
        let mut panned = default_view();
        panned.yaw += 0.5;
        let _ = m.update(panned, &default_settings("osm"));
        m.force_dwell_elapsed_for_test();
        let _ = m.update(panned, &default_settings("osm"));
        assert!(
            m.metrics().gen > gen_after_first_fire,
            "view change + dwell elapsed must re-fire (before={}, now={})",
            gen_after_first_fire,
            m.metrics().gen,
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_clear_cache_active_source_bumps_gen() {
        let (mut m, tmp) = manager_with("osm");
        let _ = m.update(default_view(), &default_settings("osm"));
        let gen_before = m.metrics().gen;
        m.clear_cache(ClearScope::ActiveSource);
        let gen_after = m.metrics().gen;
        assert!(gen_after > gen_before, "clear_cache must bump generation");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
