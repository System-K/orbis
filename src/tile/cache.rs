// =============================================================================
// Disk Tile Cache (LRU)
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::{SystemTime, Duration};

use super::TileCoord;

/// Configuration for the tile disk cache.
///
/// `cache_dir` is only used at construction time; later calls to
/// `update_config` silently ignore a changed `cache_dir` (the path is
/// fixed at `TileCache::new()` time).
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Root directory for cached tiles (immutable after `new()`)
    pub cache_dir: PathBuf,
    /// Maximum cache size in bytes (default: 500 MB)
    pub max_size_bytes: u64,
    /// Maximum age of cached tiles. `None` = no age limit (default: 7 days).
    pub max_age: Option<Duration>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cache_dir: crate::app_path("cache/tiles"),
            max_size_bytes: 500 * 1024 * 1024, // 500 MB
            max_age: Some(Duration::from_secs(7 * 24 * 3600)), // 7 days
        }
    }
}

/// Runtime-mutable cache limits (the subset of `CacheConfig` that can change
/// while the tile cache is live). `cache_dir` lives outside this, as an
/// immutable field on `TileCache`.
#[derive(Debug, Clone, Copy)]
struct CacheLimits {
    max_size_bytes: u64,
    max_age: Option<Duration>,
}

/// On-disk tile cache with LRU eviction.
///
/// Thread-safe: mutable limits (size + age) sit behind a RwLock so they can
/// be updated while download threads hold a shared reference. The cache root
/// directory is immutable after construction — it is read on the hot path
/// without any locking.
///
/// The total on-disk size is tracked in an atomic counter (`total_bytes`) so
/// that `total_size_bytes()` — called once per frame by the GUI usage readout
/// — is O(1) instead of walking `cache_dir` recursively. The counter is
/// populated on startup by the maintenance thread's first iteration, and
/// maintained incrementally by `put`, `get` (expired-file removal),
/// `evict_if_needed`, `clear_source`, and `clear_all`.
///
/// LRU eviction and periodic reconciliation run on a dedicated background
/// maintenance thread — the render thread never walks `cache_dir`. The
/// thread is parked on a 30 s timer and unparked via `request_maintenance`
/// when the render loop notices the cache has exceeded its size limit.
pub struct TileCache {
    cache_dir: PathBuf,
    limits: Arc<std::sync::RwLock<CacheLimits>>,
    /// Running total of on-disk bytes. Exposes an atomic `total_size_bytes`
    /// read; maintained by every write/delete path. Seeded by the
    /// maintenance thread's first iteration.
    total_bytes: Arc<AtomicU64>,
    /// Handle to the maintenance thread — kept so Drop can shut it down.
    maintenance: Option<MaintenanceHandle>,
}

/// Shutdown coordination for the maintenance thread.
struct MaintenanceHandle {
    thread: Thread,
    shutdown: Arc<AtomicBool>,
}

impl TileCache {
    pub fn new(config: CacheConfig) -> Self {
        if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
            log::warn!("Could not create tile cache directory: {}", e);
        }
        let total_bytes = Arc::new(AtomicU64::new(0));
        let limits = Arc::new(std::sync::RwLock::new(CacheLimits {
            max_size_bytes: config.max_size_bytes,
            max_age: config.max_age,
        }));
        let shutdown = Arc::new(AtomicBool::new(false));

        let maintenance = spawn_maintenance_thread(
            config.cache_dir.clone(),
            Arc::clone(&limits),
            Arc::clone(&total_bytes),
            Arc::clone(&shutdown),
        )
        .map(|thread| MaintenanceHandle { thread, shutdown });

        Self {
            cache_dir: config.cache_dir,
            limits,
            total_bytes,
            maintenance,
        }
    }

    /// Returns the file path for a cached tile.
    fn tile_path(&self, source_id: &str, coord: &TileCoord, ext: &str) -> PathBuf {
        self.cache_dir
            .join(source_id)
            .join(coord.z.to_string())
            .join(coord.x.to_string())
            .join(format!("{}.{}", coord.y, ext))
    }

    /// Tries to read a tile from the cache.
    ///
    /// Returns None if the tile is not cached or has expired.
    /// Updates the file's access time on hit (for LRU).
    ///
    /// Phase K: the miss path used to be `path.exists()` + `fs::read()` —
    /// two syscalls. `fs::read` on a missing path returns `io::Error`
    /// (`NotFound`) anyway, so the `exists()` check was pure overhead.
    /// We now read unconditionally and treat any I/O error as a miss.
    pub fn get(&self, source_id: &str, coord: &TileCoord, ext: &str) -> Option<Vec<u8>> {
        let path = self.tile_path(source_id, coord, ext);

        // Age check — only meaningful for an existing file; wrapped around
        // the read so we do just one `metadata` call total on the hot path.
        let max_age = self.limits.read().unwrap().max_age;
        if let Some(max_age) = max_age {
            if let Ok(metadata) = path.metadata() {
                let file_size = metadata.len();
                if let Ok(modified) = metadata.modified() {
                    if let Ok(age) = SystemTime::now().duration_since(modified) {
                        if age > max_age {
                            // Expired — remove and return miss
                            if std::fs::remove_file(&path).is_ok() {
                                self.total_bytes
                                    .fetch_sub(file_size, Ordering::Relaxed);
                            }
                            return None;
                        }
                    }
                }
            }
            // `metadata.err()` on a missing file is the same "miss" outcome
            // we'd get from `fs::read` below, so fall through either way.
        }

        // Read and "touch" (update mtime for LRU ordering). Missing-file
        // error = cache miss; no need for a separate `exists()` probe.
        match std::fs::read(&path) {
            Ok(data) => {
                if let Ok(file) = std::fs::File::open(&path) {
                    let times = std::fs::FileTimes::new()
                        .set_modified(SystemTime::now());
                    let _ = file.set_times(times);
                }
                Some(data)
            }
            Err(_) => None,
        }
    }

    /// Stores a tile in the cache.
    pub fn put(&self, source_id: &str, coord: &TileCoord, ext: &str, data: &[u8]) {
        let path = self.tile_path(source_id, coord, ext);

        // Create parent directories
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // If this path already has a file, subtract its old size before
        // overwriting so the counter doesn't double-count.
        let old_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        if let Err(e) = std::fs::write(&path, data) {
            log::warn!("Failed to cache tile {}/{}: {}", source_id, coord, e);
            return;
        }

        let new_size = data.len() as u64;
        if new_size >= old_size {
            self.total_bytes
                .fetch_add(new_size - old_size, Ordering::Relaxed);
        } else {
            self.total_bytes
                .fetch_sub(old_size - new_size, Ordering::Relaxed);
        }
    }

    /// Returns the total size of the cache in bytes. O(1) — atomic load.
    pub fn total_size_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    /// Runs LRU eviction if the cache exceeds max_size_bytes.
    ///
    /// Collects all cached files, sorts by modification time (oldest first),
    /// and deletes until total size is under the limit.
    pub fn evict_if_needed(&self) {
        // Read the size limit, then drop the lock BEFORE any I/O — holding
        // the config lock across directory traversal would serialize tile
        // reads unnecessarily.
        let max_size = match self.limits.read() {
            Ok(l) => l.max_size_bytes,
            Err(_) => return,
        };

        let current_size = self.total_bytes.load(Ordering::Relaxed);
        if current_size <= max_size {
            return;
        }

        log::info!(
            "Tile cache eviction: {:.1} MB / {:.1} MB limit",
            current_size as f64 / (1024.0 * 1024.0),
            max_size as f64 / (1024.0 * 1024.0),
        );

        // Collect all files with their size and mtime
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        collect_files_recursive(&self.cache_dir, &mut files);

        // Sort by mtime: oldest first (LRU eviction order)
        files.sort_by_key(|f| f.2);

        let mut freed: u64 = 0;
        let target = current_size - max_size;

        for (path, size, _) in &files {
            if freed >= target {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                freed += size;
            }
        }

        // One bulk fetch_sub for everything we freed.
        if freed > 0 {
            self.total_bytes.fetch_sub(freed, Ordering::Relaxed);
        }

        log::info!("Tile cache: freed {:.1} MB", freed as f64 / (1024.0 * 1024.0));
    }

    /// Removes cached tiles for a single source only.
    ///
    /// Used by the "Clear cache" GUI button so that clearing while
    /// Sentinel-2 is active doesn't wipe OSM and GIBS too.
    pub fn clear_source(&self, source_id: &str) {
        let source_dir = self.cache_dir.join(source_id);
        if !source_dir.exists() {
            return;
        }
        // Sum the source's bytes before removing so we can subtract them from
        // the counter.
        let source_size = dir_size_recursive(&source_dir);
        if let Err(e) = std::fs::remove_dir_all(&source_dir) {
            log::warn!("Failed to clear tile cache for source '{}': {}", source_id, e);
        } else {
            self.total_bytes
                .fetch_sub(source_size, Ordering::Relaxed);
            log::info!("Tile cache cleared for source '{}'", source_id);
        }
    }

    /// Removes all cached tiles across every source.
    ///
    /// Reserved for explicit full-wipe operations; the GUI "Clear cache"
    /// button calls `clear_source` instead to preserve other sources.
    #[allow(dead_code)] // Kept for future "wipe everything" admin flows
    pub fn clear_all(&self) {
        if let Err(e) = std::fs::remove_dir_all(&self.cache_dir) {
            log::warn!("Failed to clear tile cache: {}", e);
        }
        let _ = std::fs::create_dir_all(&self.cache_dir);
        self.total_bytes.store(0, Ordering::Relaxed);
        log::info!("Tile cache cleared (all sources)");
    }

    /// Updates the cache configuration (e.g. after settings change).
    ///
    /// Triggers eviction immediately so a tightened size limit takes
    /// effect on the next frame instead of waiting for the periodic check.
    /// A changed `cache_dir` is silently ignored — the path is fixed at
    /// construction time.
    ///
    /// This is an explicit, user-initiated path and runs synchronously —
    /// the per-frame render-thread path goes through `request_maintenance`
    /// instead.
    pub fn update_config(&self, config: CacheConfig) {
        if config.cache_dir != self.cache_dir {
            log::warn!(
                "update_config: cache_dir changes are not supported (fixed at new() time, keeping {:?})",
                self.cache_dir,
            );
        }
        if let Ok(mut lims) = self.limits.write() {
            lims.max_size_bytes = config.max_size_bytes;
            lims.max_age = config.max_age;
        }
        self.evict_if_needed();
    }

    /// Returns true if the current on-disk size exceeds the configured limit.
    /// Cheap — one atomic load + one RwLock read.
    pub fn should_evict(&self) -> bool {
        let max = self
            .limits
            .read()
            .map(|l| l.max_size_bytes)
            .unwrap_or(u64::MAX);
        self.total_bytes.load(Ordering::Relaxed) > max
    }

    /// Asks the maintenance thread to run reconciliation + eviction
    /// as soon as possible. Non-blocking — the render thread never
    /// walks `cache_dir`.
    pub fn request_maintenance(&self) {
        if let Some(m) = &self.maintenance {
            m.thread.unpark();
        }
    }
}

impl Drop for TileCache {
    fn drop(&mut self) {
        if let Some(m) = self.maintenance.take() {
            m.shutdown.store(true, Ordering::Relaxed);
            m.thread.unpark();
            // Detached — no join. Thread observes shutdown=true on its next
            // park_timeout wake and exits. For a long-lived process this
            // happens at shutdown; in tests the thread exits within 30 s
            // (or immediately if still parked when we unpark).
        }
    }
}

/// Long-lived maintenance thread.
///
/// Performs the initial `dir_size_recursive` walk to seed `total_bytes`,
/// then loops: park for up to 30 s (unpark-able by `request_maintenance`
/// or Drop), reconcile `total_bytes` against the on-disk reality, and
/// run LRU eviction if over the configured limit.
fn spawn_maintenance_thread(
    cache_dir: PathBuf,
    limits: Arc<std::sync::RwLock<CacheLimits>>,
    total_bytes: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
) -> Option<Thread> {
    let handle = std::thread::Builder::new()
        .name("tile-cache-maintenance".into())
        .spawn(move || {
            // Initial reconciliation: this is what seeds total_bytes on
            // startup so `total_size_bytes()` returns the real value a
            // fraction of a second after `TileCache::new` returns.
            let initial = dir_size_recursive(&cache_dir);
            total_bytes.store(initial, Ordering::Relaxed);
            log::info!(
                "Tile cache initial size: {:.1} MB",
                initial as f64 / (1024.0 * 1024.0),
            );

            loop {
                std::thread::park_timeout(Duration::from_secs(30));
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Reconcile: the atomic counter drifts from reality if
                // anything modifies the cache dir from outside our own
                // put/delete paths. A walk every 30 s corrects that.
                let actual = dir_size_recursive(&cache_dir);
                total_bytes.store(actual, Ordering::Relaxed);

                // Evict if over the configured limit.
                let max = match limits.read() {
                    Ok(l) => l.max_size_bytes,
                    Err(_) => continue,
                };
                if actual <= max {
                    continue;
                }

                log::info!(
                    "Tile cache maintenance: {:.1} MB / {:.1} MB limit",
                    actual as f64 / (1024.0 * 1024.0),
                    max as f64 / (1024.0 * 1024.0),
                );

                let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
                collect_files_recursive(&cache_dir, &mut files);
                files.sort_by_key(|f| f.2);

                let mut freed: u64 = 0;
                let target = actual - max;
                for (path, size, _) in &files {
                    if freed >= target {
                        break;
                    }
                    if std::fs::remove_file(path).is_ok() {
                        freed += size;
                    }
                }
                if freed > 0 {
                    total_bytes.fetch_sub(freed, Ordering::Relaxed);
                }
                log::info!(
                    "Tile cache maintenance: freed {:.1} MB",
                    freed as f64 / (1024.0 * 1024.0),
                );
            }
        });
    match handle {
        Ok(h) => Some(h.thread().clone()),
        Err(e) => {
            log::warn!("Failed to spawn tile-cache maintenance thread: {}", e);
            None
        }
    }
}

/// Recursively computes total file size in a directory.
fn dir_size_recursive(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                total += dir_size_recursive(&entry_path);
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Recursively collects all files with size and mtime.
fn collect_files_recursive(path: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                collect_files_recursive(&entry_path, out);
            } else if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((entry_path, meta.len(), mtime));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.push(format!("orbis-tilecache-{}-{}", tag, nanos));
        dir
    }

    fn cache_config(dir: PathBuf, max_size: u64, max_age: Option<Duration>) -> CacheConfig {
        CacheConfig { cache_dir: dir, max_size_bytes: max_size, max_age }
    }

    /// Wait for the background startup walk to settle on a known-empty dir.
    /// Without this, tests that immediately `put` + assert total_size_bytes
    /// can race the walk and observe a lingering 0 or the walk overwriting
    /// the put's fetch_add.
    fn wait_initial_scan_done(cache: &TileCache, expected: u64) {
        // The init thread does one store. Busy-wait briefly for it to happen
        // on fresh / empty directories where `expected` is typically 0.
        for _ in 0..100 {
            if cache.total_size_bytes() == expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn test_cache_put_get_roundtrip() {
        let dir = unique_tmp_dir("roundtrip");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, Some(Duration::from_secs(3600))));
        let coord = TileCoord { z: 5, x: 1, y: 2 };
        cache.put("osm", &coord, "png", b"hello");
        let got = cache.get("osm", &coord, "png");
        assert_eq!(got.as_deref(), Some(&b"hello"[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backdates the mtime of `path` by the given duration.
    ///
    /// On Windows, `File::set_times` requires write access to the file, so
    /// we open it with `OpenOptions::write(true)` to ensure the call succeeds.
    fn backdate(path: &Path, how_old: Duration) -> bool {
        let file = match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let old_time = SystemTime::now() - how_old;
        let times = std::fs::FileTimes::new().set_modified(old_time);
        file.set_times(times).is_ok()
    }

    #[test]
    fn test_cache_max_age_none_never_expires() {
        let dir = unique_tmp_dir("no-expire");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        let coord = TileCoord { z: 5, x: 1, y: 2 };
        cache.put("osm", &coord, "png", b"stale");

        // Backdate the file by 30 days — with max_age=None it must still be served.
        let path = dir.join("osm").join("5").join("1").join("2.png");
        assert!(backdate(&path, Duration::from_secs(30 * 24 * 3600)),
                "failed to backdate test file");

        let got = cache.get("osm", &coord, "png");
        assert!(got.is_some(), "max_age=None must never expire a tile");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cache_max_age_some_expires() {
        let dir = unique_tmp_dir("expire");
        let cache = TileCache::new(cache_config(
            dir.clone(), 1_000_000, Some(Duration::from_secs(60)),
        ));
        let coord = TileCoord { z: 5, x: 1, y: 2 };
        cache.put("osm", &coord, "png", b"old");

        // Backdate the file to be older than max_age
        let path = dir.join("osm").join("5").join("1").join("2.png");
        assert!(backdate(&path, Duration::from_secs(3600)),
                "failed to backdate test file");

        assert!(cache.get("osm", &coord, "png").is_none(), "expired tile must miss");
        assert!(!path.exists(), "expired tile must be removed on miss");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clear_source_only_removes_active_source() {
        let dir = unique_tmp_dir("clear-source");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        let c = TileCoord { z: 4, x: 1, y: 1 };
        cache.put("osm", &c, "png", b"osm-data");
        cache.put("sentinel2", &c, "jpg", b"s2-data");

        cache.clear_source("sentinel2");

        assert!(cache.get("osm", &c, "png").is_some(), "osm must survive clear_source(sentinel2)");
        assert!(cache.get("sentinel2", &c, "jpg").is_none(), "sentinel2 must be wiped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_update_config_triggers_eviction() {
        let dir = unique_tmp_dir("update-evict");
        // Start with a huge limit, put several tiles
        let cache = TileCache::new(cache_config(dir.clone(), 10_000_000, None));
        wait_initial_scan_done(&cache, 0);
        let data = vec![0u8; 4096]; // 4 KB each
        for x in 0..8u32 {
            let c = TileCoord { z: 3, x, y: 0 };
            cache.put("osm", &c, "png", &data);
            // Stagger mtimes so LRU ordering is deterministic
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let size_before = cache.total_size_bytes();
        assert!(size_before >= 8 * 4096, "expected ~32 KB, got {}", size_before);

        // Tighten the limit — update_config should evict immediately.
        cache.update_config(cache_config(dir.clone(), 10_000, None));
        let size_after = cache.total_size_bytes();
        assert!(
            size_after <= 10_000,
            "update_config must evict immediately: {} > 10_000",
            size_after,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_update_config_ignores_cache_dir_change() {
        let dir = unique_tmp_dir("ignore-dir");
        let other = unique_tmp_dir("ignore-dir-other");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        let c = TileCoord { z: 3, x: 1, y: 1 };
        cache.put("osm", &c, "png", b"data");

        // Attempt to move the cache — should be ignored (logged as warning)
        cache.update_config(cache_config(other.clone(), 1_000_000, None));
        let got = cache.get("osm", &c, "png");
        assert!(got.is_some(), "cache_dir change must be a no-op on storage location");
        assert!(!other.exists(), "new cache_dir must not have been created/used");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------- Phase A: O(1) total_bytes counter --------

    #[test]
    fn test_total_size_after_put_is_sum_of_data() {
        let dir = unique_tmp_dir("total-put");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        wait_initial_scan_done(&cache, 0);

        cache.put("osm", &TileCoord { z: 1, x: 0, y: 0 }, "png", &vec![0u8; 100]);
        cache.put("osm", &TileCoord { z: 1, x: 0, y: 1 }, "png", &vec![0u8; 250]);
        cache.put("osm", &TileCoord { z: 1, x: 1, y: 0 }, "png", &vec![0u8; 42]);

        assert_eq!(cache.total_size_bytes(), 100 + 250 + 42);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_size_after_overwrite_does_not_double_count() {
        let dir = unique_tmp_dir("total-overwrite");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        wait_initial_scan_done(&cache, 0);

        let coord = TileCoord { z: 1, x: 0, y: 0 };
        cache.put("osm", &coord, "png", &vec![0u8; 100]);
        cache.put("osm", &coord, "png", &vec![0u8; 250]); // overwrite, not new
        assert_eq!(cache.total_size_bytes(), 250);

        cache.put("osm", &coord, "png", &vec![0u8; 10]); // shrink
        assert_eq!(cache.total_size_bytes(), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_size_after_clear_source_drops_that_source_only() {
        let dir = unique_tmp_dir("total-clear-source");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        wait_initial_scan_done(&cache, 0);

        cache.put("osm", &TileCoord { z: 1, x: 0, y: 0 }, "png", &vec![0u8; 100]);
        cache.put("sentinel2", &TileCoord { z: 1, x: 0, y: 0 }, "jpg", &vec![0u8; 250]);
        assert_eq!(cache.total_size_bytes(), 350);

        cache.clear_source("sentinel2");
        assert_eq!(cache.total_size_bytes(), 100, "only sentinel2 bytes should drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_size_after_clear_all_is_zero() {
        let dir = unique_tmp_dir("total-clear-all");
        let cache = TileCache::new(cache_config(dir.clone(), 1_000_000, None));
        wait_initial_scan_done(&cache, 0);

        cache.put("osm", &TileCoord { z: 1, x: 0, y: 0 }, "png", &vec![0u8; 100]);
        cache.put("sentinel2", &TileCoord { z: 1, x: 0, y: 0 }, "jpg", &vec![0u8; 250]);

        cache.clear_all();
        assert_eq!(cache.total_size_bytes(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_size_after_eviction_matches_remaining() {
        let dir = unique_tmp_dir("total-evict");
        let cache = TileCache::new(cache_config(dir.clone(), 10_000_000, None));
        wait_initial_scan_done(&cache, 0);

        // Put ~32 KB of tiles, then shrink the limit and expect the
        // counter to track the on-disk total after eviction.
        let data = vec![0u8; 4096];
        for x in 0..8u32 {
            cache.put("osm", &TileCoord { z: 3, x, y: 0 }, "png", &data);
            std::thread::sleep(Duration::from_millis(5));
        }
        cache.update_config(cache_config(dir.clone(), 10_000, None));

        let counted = cache.total_size_bytes();
        let actual = dir_size_recursive(&dir);
        assert_eq!(counted, actual, "counter must match actual size after evict");
        assert!(counted <= 10_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------- Phase B: maintenance thread --------

    #[test]
    fn test_should_evict_reflects_counter_vs_limit() {
        let dir = unique_tmp_dir("should-evict");
        let cache = TileCache::new(cache_config(dir.clone(), 500, None));
        wait_initial_scan_done(&cache, 0);
        assert!(!cache.should_evict(), "fresh cache is under the limit");

        cache.put("osm", &TileCoord { z: 1, x: 0, y: 0 }, "png", &vec![0u8; 400]);
        assert!(!cache.should_evict(), "400 bytes under 500-byte limit");

        cache.put("osm", &TileCoord { z: 1, x: 0, y: 1 }, "png", &vec![0u8; 400]);
        assert!(cache.should_evict(), "800 bytes exceeds 500-byte limit");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_request_maintenance_shrinks_cache_below_limit() {
        // Put ~32 KB, set a tight limit, ask the maintenance thread to run,
        // wait for it to observe the new state, and verify it shrank the
        // cache. This exercises the full async path: flag -> unpark ->
        // walk -> evict -> counter update.
        let dir = unique_tmp_dir("maint-evict");
        let cache = TileCache::new(cache_config(dir.clone(), 10_000_000, None));
        wait_initial_scan_done(&cache, 0);

        let data = vec![0u8; 4096];
        for x in 0..8u32 {
            cache.put("osm", &TileCoord { z: 3, x, y: 0 }, "png", &data);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cache.total_size_bytes() >= 8 * 4096);

        // Tighten limit WITHOUT calling evict_if_needed synchronously.
        if let Ok(mut lims) = cache.limits.write() {
            lims.max_size_bytes = 10_000;
        }
        assert!(cache.should_evict());
        cache.request_maintenance();

        // Wait up to 2 s for the maintenance thread to shrink the cache.
        let mut ok = false;
        for _ in 0..200 {
            if cache.total_size_bytes() <= 10_000 {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ok,
            "maintenance thread must shrink cache within 2 s (still {} bytes)",
            cache.total_size_bytes(),
        );
        assert_eq!(cache.total_size_bytes(), dir_size_recursive(&dir),
            "counter must match on-disk reality after maintenance");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_size_after_expired_get_drops_file_size() {
        let dir = unique_tmp_dir("total-expire");
        let cache = TileCache::new(cache_config(
            dir.clone(), 1_000_000, Some(Duration::from_secs(60)),
        ));
        wait_initial_scan_done(&cache, 0);

        let coord = TileCoord { z: 5, x: 1, y: 2 };
        cache.put("osm", &coord, "png", &vec![0u8; 300]);
        assert_eq!(cache.total_size_bytes(), 300);

        // Backdate to force expiry on get()
        let path = dir.join("osm").join("5").join("1").join("2.png");
        assert!(backdate(&path, Duration::from_secs(3600)));
        assert!(cache.get("osm", &coord, "png").is_none());
        assert_eq!(cache.total_size_bytes(), 0, "expired-file removal must update counter");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
