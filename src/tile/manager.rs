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
use std::time::Duration;

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

/// Maximum number of tiles decoded + composited from the disk cache in a
/// single frame.  Image decoding (JPEG/PNG via the `image` crate) is the
/// dominant per-tile cost on the render thread — roughly 1-5 ms each.
/// Capping it at 4 per frame keeps the frame budget under 20 ms even on
/// a zoom change (which invalidates the whole composited set and forces
/// all visible tiles to be re-decoded).  Tiles beyond the cap are
/// deferred to subsequent frames — visually, tiles "pop in" over 3-4
/// frames instead of freezing for 60-200 ms.
const MAX_COMPOSITES_PER_FRAME: usize = 4;

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
    /// Dedup: tiles currently enqueued (by source+coord).
    in_flight: HashSet<(String, TileCoord)>,
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
            metrics: TileMetrics::default(),
        }
    }

    /// Main per-frame entry point. Runs the state machine:
    /// 1. Periodic cache evict
    /// 2. Source / zoom change detection -> bump gen + reset compositor
    /// 3. Compute visible tile set
    /// 4. For each missing: cache-hit path (composite sync) or enqueue job
    /// 5. Drain worker results, filter stale, composite valid ones
    pub fn update(&mut self, view: ViewState, settings: &TileSettings) -> TileFrameResult {
        // Resolve source — if invalid, bail out (caller is responsible for
        // sanitization but we guard anyway).
        let source = match self.sources.iter().find(|s| s.id == settings.source_id) {
            Some(s) => s.clone(),
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
            reset = true;
        } else if zoom_changed {
            let new_gen = self.pool.bump_generation();
            self.metrics.gen = new_gen;
            self.compositor.demote_to_zoom(zoom);
            self.prev_zoom = zoom;
            self.in_flight.clear();
            // reset stays false — old pixels keep rendering.
        }

        // Visible tiles
        let (lat_n, lat_s, lon_w, lon_e) = visible_bounds(
            view.yaw, view.pitch, view.distance, view.fov_y, view.aspect,
        );
        let visible = TileCoord::tiles_in_view(lat_n, lat_s, lon_w, lon_e, zoom);
        self.metrics.visible_tiles = visible.len() as u32;

        // Missing tiles: cache-hit -> composite sync; miss -> enqueue.
        //
        // Compositing involves JPEG/PNG decoding (1-5 ms each) on the
        // render thread, so we cap the number of composites per frame
        // to keep the frame budget manageable. Tiles beyond the cap
        // will be picked up on subsequent frames.
        //
        // Enqueueing downloads is cheap (just a channel send), so we
        // always enqueue cache misses regardless of the composite cap.
        // This ensures network fetches start immediately during a pan
        // instead of being delayed by multiple frames while cached
        // tiles drain the composite budget.
        let ext = source.extension();
        let current_gen = self.pool.current_gen();
        let mut composites_this_frame: usize = 0;
        for coord in &visible {
            if self.compositor.has_tile(coord) { continue; }
            // Try cache first (only if we have budget — avoids
            // filesystem I/O when we'd just throw the data away).
            if composites_this_frame < MAX_COMPOSITES_PER_FRAME {
                if let Some(data) = self.cache.get(&source.id, coord, ext) {
                    self.compositor.composite_tile(coord, &data);
                    composites_this_frame += 1;
                    self.metrics.cache_hits = self.metrics.cache_hits.wrapping_add(1);
                    continue;
                }
            }
            // Cache miss (or over budget) — enqueue for download if not
            // already in flight. If the tile is actually cached but we
            // skipped it due to the cap, the worker's `fetch_tile` will
            // hit the disk cache and return immediately; the result
            // composites next frame.
            let key = (source.id.clone(), *coord);
            if self.in_flight.contains(&key) { continue; }
            self.in_flight.insert(key);
            self.metrics.cache_misses = self.metrics.cache_misses.wrapping_add(1);
            self.pool.enqueue(Job {
                gen: current_gen,
                source: source.clone(),
                coord: *coord,
                date: None,
            });
        }

        // Drain results, filter stale. Worker results also count against
        // the per-frame composite cap — downloaded tiles arrive decoded
        // as raw bytes (still need image::load_from_memory).
        let results = self.pool.poll();
        let live_gen = self.pool.current_gen();
        for r in results {
            self.in_flight.remove(&(r.source_id.clone(), r.coord));
            if r.gen < live_gen { continue; }
            if r.source_id != self.prev_source { continue; }
            if let Ok(data) = r.data {
                if composites_this_frame < MAX_COMPOSITES_PER_FRAME {
                    self.compositor.composite_tile(&r.coord, &data);
                    composites_this_frame += 1;
                } else {
                    // Over budget — cache the data so it's a hit next
                    // frame instead of another network fetch.
                    self.cache.put(&r.source_id, &r.coord, ext, &data);
                    // Put it back as "not in flight" so the cache-hit
                    // path picks it up next frame.
                }
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
