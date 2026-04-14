// =============================================================================
// Tile Fetcher (download with cache)
// =============================================================================

use super::{TileSource, TileCoord, TileCache};

/// Fetches a tile: cache hit -> return immediately, miss -> download and cache.
///
/// This is a blocking call — intended to be run from a background thread.
pub fn fetch_tile(
    source: &TileSource,
    coord: &TileCoord,
    cache: &TileCache,
    date: Option<&str>,
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

    let response = request
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
