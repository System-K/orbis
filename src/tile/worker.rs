// =============================================================================
// Background Worker Pool (Phase 4)
// =============================================================================
//
// Fixed-size thread pool that consumes tile download `Job`s via an mpsc
// channel. Each worker skips jobs whose `gen` is older than `current_gen` —
// this lets the `TileManager` invalidate all in-flight work on source /
// zoom / cache-clear without killing threads.
//
// Replaces the old `TileDownloadQueue` which spawned one thread per tile
// and silently dropped tiles past `max_concurrent`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use super::fetcher::DEFAULT_FETCH_TIMEOUT;
use super::{fetch_tile, TileCache, TileCoord, TileSource};

/// A tile download request tagged with a generation counter.
///
/// `source` is an `Arc<TileSource>` so enqueueing is a cheap atomic ref-bump
/// instead of ~8 heap allocations per tile (id/name/url_template/attribution
/// strings + the subdomain vec). At close zoom the manager may fan out tens
/// of thousands of tiles per frame; cloning the full struct used to be the
/// dominant render-thread cost at z ≥ 11.
pub struct Job {
    pub gen: u64,
    pub source: Arc<TileSource>,
    pub coord: TileCoord,
    pub date: Option<String>,
}

/// Result of a tile fetch attempt.
///
/// Phase H: the worker decodes the tile to an `image::RgbaImage` on its own
/// thread before handing back to the manager. Previously this returned the
/// raw bytes and the render thread ran `image::load_from_memory` — a 1–5 ms
/// per-tile hit that caused the zoom-freeze symptom. Decoding in parallel
/// across the worker pool keeps the render thread free.
pub struct WorkerResult {
    pub gen: u64,
    pub coord: TileCoord,
    pub source_id: String,
    /// Ok(decoded) on success (from cache or network, then decoded on this
    /// worker thread). Err on fetch failure, decode failure, or stale job.
    pub data: Result<image::RgbaImage, String>,
}

/// Number of concurrent download threads.
const DEFAULT_WORKER_COUNT: usize = 4;

/// Fixed-size worker pool with generation-based cancellation.
///
/// Thread model:
/// - One shared mpsc::channel for jobs. Workers compete for jobs via a
///   mutex-wrapped Receiver.
/// - One shared mpsc::channel for results, non-blocking polled by the
///   manager each frame.
/// - `current_gen: Arc<AtomicU64>` — bumped by the manager on invalidation
///   events. Workers check it BEFORE fetching to skip cancelled jobs.
pub struct WorkerPool {
    job_tx: mpsc::Sender<Job>,
    result_rx: mpsc::Receiver<WorkerResult>,
    current_gen: Arc<AtomicU64>,
    // Handles kept so workers are joined/dropped when the pool drops.
    _workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub fn new(cache: Arc<TileCache>) -> Self {
        Self::with_threads(cache, DEFAULT_WORKER_COUNT)
    }

    pub fn with_threads(cache: Arc<TileCache>, num_threads: usize) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
        let current_gen = Arc::new(AtomicU64::new(0));
        let job_rx = Arc::new(Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(num_threads);
        for i in 0..num_threads {
            let rx = Arc::clone(&job_rx);
            let tx = result_tx.clone();
            let cache_c = Arc::clone(&cache);
            let gen_c = Arc::clone(&current_gen);
            let handle = std::thread::Builder::new()
                .name(format!("orbis-tile-worker-{}", i))
                .spawn(move || worker_loop(rx, tx, cache_c, gen_c))
                .expect("failed to spawn tile worker thread");
            workers.push(handle);
        }
        // Drop the original result_tx — result_rx unblocks if all workers exit.
        drop(result_tx);

        Self { job_tx, result_rx, current_gen, _workers: workers }
    }

    /// Enqueue a job. Workers pick it up FIFO.
    pub fn enqueue(&self, job: Job) {
        let _ = self.job_tx.send(job);
    }

    /// Drain all completed results (non-blocking).
    pub fn poll(&self) -> Vec<WorkerResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.result_rx.try_recv() {
            out.push(r);
        }
        out
    }

    /// Bump the generation counter. All jobs (both in-flight and pending in
    /// the channel) with `gen < new_gen` will be treated as cancelled.
    /// Returns the new generation.
    pub fn bump_generation(&self) -> u64 {
        self.current_gen.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn current_gen(&self) -> u64 {
        self.current_gen.load(Ordering::Relaxed)
    }
}

fn worker_loop(
    rx: Arc<Mutex<mpsc::Receiver<Job>>>,
    tx: mpsc::Sender<WorkerResult>,
    cache: Arc<TileCache>,
    current_gen: Arc<AtomicU64>,
) {
    loop {
        // Serialize receives across threads (Receiver is !Sync).
        let job = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            match guard.recv() {
                Ok(j) => j,
                Err(_) => return, // all senders dropped = pool shutdown
            }
        };

        // Stale check — cheap, avoids doing a network fetch for cancelled work.
        if job.gen < current_gen.load(Ordering::Relaxed) {
            let _ = tx.send(WorkerResult {
                gen: job.gen,
                coord: job.coord,
                source_id: job.source.id.clone(),
                data: Err("stale".into()),
            });
            continue;
        }

        // Phase H: fetch + decode on the worker thread. A cache-hit tile is
        // disk-read here and decoded here — the render thread never sees
        // raw tile bytes.
        let fetched = fetch_tile(
            &job.source,
            &job.coord,
            &cache,
            job.date.as_deref(),
            DEFAULT_FETCH_TIMEOUT,
        );
        let decoded = match fetched {
            Ok(bytes) => match image::load_from_memory(&bytes) {
                Ok(img) => Ok(img.to_rgba8()),
                Err(e) => Err(format!("tile decode failed ({}): {}", job.coord, e)),
            },
            Err(e) => Err(e),
        };
        // Clone only the source_id — WorkerResult crosses the channel back
        // to the manager, which needs to match it against the current source
        // without holding on to the full Arc<TileSource>.
        let source_id = job.source.id.clone();
        let _ = tx.send(WorkerResult {
            gen: job.gen,
            coord: job.coord,
            source_id,
            data: decoded,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{builtin_tile_sources, CacheConfig};
    use std::time::{Duration, SystemTime};

    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.push(format!("orbis-worker-{}-{}", tag, nanos));
        dir
    }

    #[test]
    fn test_pool_respects_generation() {
        // A job tagged with gen=0 enqueued AFTER current_gen was bumped to 1
        // must be skipped by the worker (marked Err("stale")), not fetched.
        let tmp = unique_tmp_dir("gen");
        let cache = Arc::new(TileCache::new(CacheConfig {
            cache_dir: tmp.clone(),
            max_size_bytes: 1024 * 1024,
            max_age: None,
        }));
        let pool = WorkerPool::with_threads(Arc::clone(&cache), 1);

        let new_gen = pool.bump_generation();
        assert_eq!(new_gen, 1);

        let source = Arc::new(
            builtin_tile_sources()
                .into_iter()
                .find(|s| s.id == "osm")
                .expect("osm source must exist"),
        );
        pool.enqueue(Job {
            gen: 0, // stale
            source,
            coord: TileCoord { z: 0, x: 0, y: 0 },
            date: None,
        });

        // Poll for up to 500 ms — worker should mark the job stale immediately.
        let mut results = Vec::new();
        for _ in 0..50 {
            results = pool.poll();
            if !results.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(results.len(), 1, "expected stale result back from worker");
        let r = &results[0];
        assert_eq!(r.gen, 0);
        assert!(r.data.is_err(), "stale job must not be fetched");
        let err = r.data.as_ref().err().unwrap();
        assert!(err.contains("stale"), "expected 'stale' in err, got: {}", err);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Build a 4×4 valid PNG so the worker's `image::load_from_memory` call
    /// can actually decode it. Tests that exercise the decoded-output
    /// contract must use this rather than arbitrary bytes.
    fn minimal_png_4x4() -> Vec<u8> {
        use image::ImageEncoder;
        let mut img = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(1, 2, image::Rgba([200, 100, 50, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .expect("encode minimal png");
        out
    }

    #[test]
    fn test_pool_serves_cached_tile_without_network() {
        // Phase H: the worker fetches cached bytes AND decodes on its own
        // thread. We seed the disk cache with a valid 4×4 PNG and assert
        // the pool hands back an `RgbaImage` of the right dimensions,
        // proving cache lookup + decode both happened off the render thread.
        let tmp = unique_tmp_dir("cached");
        let cache = Arc::new(TileCache::new(CacheConfig {
            cache_dir: tmp.clone(),
            max_size_bytes: 1024 * 1024,
            max_age: None,
        }));
        let coord = TileCoord { z: 3, x: 1, y: 1 };
        let png = minimal_png_4x4();
        cache.put("osm", &coord, "png", &png);

        let pool = WorkerPool::with_threads(Arc::clone(&cache), 1);
        let source = Arc::new(
            builtin_tile_sources()
                .into_iter()
                .find(|s| s.id == "osm")
                .unwrap(),
        );
        pool.enqueue(Job {
            gen: pool.current_gen(),
            source,
            coord,
            date: None,
        });

        let mut results = Vec::new();
        for _ in 0..50 {
            results = pool.poll();
            if !results.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(results.len(), 1);
        let r = &results[0];
        let img = r.data.as_ref().expect("cached tile must decode");
        assert_eq!(img.dimensions(), (4, 4), "decoded image should match seed");
        let px = img.get_pixel(1, 2);
        assert_eq!(px.0, [200, 100, 50, 255], "decoded pixel preserved");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
