// =============================================================================
// Disk Tile Cache (LRU)
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// populated on startup by a background thread and maintained incrementally
/// by `put`, `get` (expired-file removal), `evict_if_needed`, `clear_source`,
/// and `clear_all`.
pub struct TileCache {
    cache_dir: PathBuf,
    limits: std::sync::RwLock<CacheLimits>,
    /// Running total of on-disk bytes. Exposes an atomic `total_size_bytes`
    /// read; maintained by every write/delete path. Seeded by a background
    /// startup walk in `new()`.
    total_bytes: Arc<AtomicU64>,
}

impl TileCache {
    pub fn new(config: CacheConfig) -> Self {
        if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
            log::warn!("Could not create tile cache directory: {}", e);
        }
        let total_bytes = Arc::new(AtomicU64::new(0));

        // Seed total_bytes via a one-shot background walk so `new()` doesn't
        // block startup. Until the walk completes, the GUI usage readout
        // reports 0, which is acceptable for a fraction of a second.
        {
            let dir = config.cache_dir.clone();
            let counter = Arc::clone(&total_bytes);
            std::thread::Builder::new()
                .name("tile-cache-init".into())
                .spawn(move || {
                    let size = dir_size_recursive(&dir);
                    counter.store(size, Ordering::Relaxed);
                    log::info!(
                        "Tile cache initial size: {:.1} MB",
                        size as f64 / (1024.0 * 1024.0),
                    );
                })
                .ok();
        }

        Self {
            cache_dir: config.cache_dir,
            limits: std::sync::RwLock::new(CacheLimits {
                max_size_bytes: config.max_size_bytes,
                max_age: config.max_age,
            }),
            total_bytes,
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
    pub fn get(&self, source_id: &str, coord: &TileCoord, ext: &str) -> Option<Vec<u8>> {
        let path = self.tile_path(source_id, coord, ext);
        if !path.exists() {
            return None;
        }

        // Check age — `None` disables expiry entirely
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
        }

        // Read and "touch" (update mtime for LRU ordering)
        match std::fs::read(&path) {
            Ok(data) => {
                // Touch the file to update mtime (for LRU ordering)
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
