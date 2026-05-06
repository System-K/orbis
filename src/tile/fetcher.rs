// =============================================================================
// Tile Fetcher (download with cache)
// =============================================================================

use std::time::Duration;

use super::{TileSource, TileCoord, TileCache};

/// Default per-request HTTP timeout for tile downloads.
///
/// A slow or unresponsive tile server used to park a worker indefinitely
/// (ureq defaults to no timeout) — with four workers, four bad jobs in a
/// row meant the entire pool was dead. 15 s is generous enough to ride
/// out normal latency spikes on Sentinel-2/GIBS while still releasing
/// the worker on a truly stuck connection.
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Fetches a tile: cache hit -> return immediately, miss -> download and cache.
///
/// `timeout` bounds the full download (connect + headers + body) so a
/// stuck connection cannot starve the worker pool. Production code should
/// pass `DEFAULT_FETCH_TIMEOUT`; tests use shorter values so the timeout
/// path is exercised without a minute-long wait.
///
/// This is a blocking call — intended to be run from a background thread.
pub fn fetch_tile(
    source: &TileSource,
    coord: &TileCoord,
    cache: &TileCache,
    date: Option<&str>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let ext = source.extension();

    // Cache hit?
    if let Some(data) = cache.get(&source.id, coord, ext) {
        return Ok(data);
    }

    // Cache miss -> download
    let url = source.tile_url(coord, date);

    let mut request = ureq::get(&url);
    if let Some(ua) = &source.user_agent {
        request = request.header("User-Agent", ua);
    }
    // Per-source headers (Authorization, X-API-Key, Referer, etc.). A
    // `User-Agent` entry here intentionally wins over the `user_agent`
    // field above — that's the documented escape hatch for users who
    // need a specific UA on a private server.
    for (key, value) in &source.headers {
        request = request.header(key.as_str(), value.as_str());
    }

    let response = request
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|e| format!("Tile download failed {}: {}", url, e))?;

    let data = response
        .into_body()
        .read_to_vec()
        .map_err(|e| format!("Tile read failed {}: {}", url, e))?;

    if data.is_empty() {
        return Err(format!("Empty tile response from {}", url));
    }

    // Store in cache
    cache.put(&source.id, coord, ext, &data);

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::{CacheConfig, TileCache, TileSource, TileFormat};
    use std::io::Read;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.push(format!("orbis-fetcher-{}-{}", tag, nanos));
        dir
    }

    /// Spawns a TcpListener that accepts incoming connections but never
    /// reads or writes from them — simulating a slow/hung tile server.
    /// Returns the URL template to aim a fetch at, plus a stop flag.
    fn spawn_dead_server() -> (String, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local listener");
        let addr = listener.local_addr().expect("local_addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
        std::thread::spawn(move || {
            // Keep held connections alive until stop is signalled so the
            // fetcher's read actually blocks waiting for a response.
            let mut held = Vec::new();
            for conn in listener.incoming() {
                if stop_c.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(mut s) = conn {
                    // Drain the request but never reply.
                    let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
                    let mut buf = [0u8; 512];
                    let _ = s.read(&mut buf);
                    held.push(s);
                }
            }
            drop(held);
        });
        (format!("http://{}/{{z}}/{{x}}/{{y}}.png", addr), stop)
    }

    #[test]
    fn test_fetch_respects_timeout() {
        // A source pointed at a dead-accept server must bubble a timeout
        // error within a small window above the timeout, not hang forever.
        let (url_template, stop) = spawn_dead_server();
        let timeout = Duration::from_millis(500);

        let source = TileSource {
            id: "dead-test".into(),
            name: "dead".into(),
            url_template,
            subdomains: vec![],
            max_zoom: 5,
            format: TileFormat::Png,
            attribution: String::new(),
            user_agent: None,
            headers: std::collections::HashMap::new(),
            recommended_zoom_bias: 0,
        };
        let tmp = unique_tmp_dir("timeout");
        let cache = TileCache::new(CacheConfig {
            cache_dir: tmp.clone(),
            max_size_bytes: 1024 * 1024,
            max_age: None,
        });

        let coord = TileCoord { z: 1, x: 0, y: 0 };
        let started = Instant::now();
        let result = fetch_tile(&source, &coord, &cache, None, timeout);
        let elapsed = started.elapsed();

        stop.store(true, Ordering::Relaxed);

        assert!(result.is_err(), "dead server must produce a fetch error");
        assert!(
            elapsed < timeout + Duration::from_secs(2),
            "fetch must respect timeout: elapsed {:?} > {:?} + 2s grace",
            elapsed,
            timeout,
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
