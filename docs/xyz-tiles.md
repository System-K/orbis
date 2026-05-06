# Custom XYZ Tile Sources

The "Add Custom Source" dialog has XYZ as one of its types. A custom XYZ
source becomes a new entry in the tile-source dropdown alongside the
built-in Sentinel-2, OpenStreetMap, and NASA GIBS layers — it's the same
machinery that drives those, just with a user-supplied URL template.

This is for **base map** tiles: the imagery the globe is wrapped in. It is
NOT for vector overlays (which is GeoJSON / Shapefile / CSV territory).

## URL template

The URL goes into the `url_template` field with the standard slippy-map
placeholders:

| Placeholder | Meaning |
|-------------|---------|
| `{z}` | Zoom level (0 = whole world, 19 = street level for OSM) |
| `{x}` | Tile column, `0..2^z - 1`, left to right |
| `{y}` | Tile row, `0..2^z - 1`, top to bottom (Google/OSM convention) |
| `{s}` | Optional. Replaced per-tile from the `subdomains` list (round-robin) for load-balancing across mirror domains |

Examples:

```
https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png
https://a.tile.opentopomap.org/{z}/{x}/{y}.png
https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}
https://api.maptiler.com/maps/streets/{z}/{x}/{y}.png?key=YOUR_KEY
```

Note the second-to-last: ArcGIS REST tile services use `{z}/{y}/{x}` order.
The placeholder ordering follows whatever your server uses — Orbis trusts
the template verbatim.

If the template is missing one of `{z}`, `{x}`, or `{y}`, Orbis warns to
the log but still accepts the source — some private servers use unusual
templates that we can't anticipate.

## Required and optional fields

| Field | Required | Notes |
|-------|----------|-------|
| `url_template` | yes | See above |
| `max_zoom` | yes | Highest zoom level the server serves; clamped to 0–22 |
| `format` | optional | `"png"`, `"jpg"`/`"jpeg"`, or empty (sniffed from URL extension) |
| `subdomains` | optional | List of strings replacing `{s}`. Empty list means no subdomain rotation |

The display **name** comes from the source's general `name` field; the
**attribution** comes from the source's `attribution` field. Both surface
in the GUI's tile-source dropdown and the attribution footer.

## Identity and dropdown integration

The internal tile-source ID is namespaced as `custom:<your_id>` so it
never collides with a built-in. After you save a new XYZ source, the
tile-source combo box (Settings panel → Tile source) lists it alongside
the built-ins. Selecting it switches the globe imagery.

## Subdomain rotation

XYZ servers commonly host the same content at `a.example.com`, `b.example.com`,
`c.example.com` to spread load. Set the `{s}` placeholder in your URL and
fill the `subdomains` list with `["a", "b", "c"]`. Orbis cycles through
them based on `(x + y) % len`, the same scheme Leaflet uses.

## User-Agent

Orbis sends a `User-Agent: Orbis/0.1 (https://github.com/System-K/orbis)`
header on every tile request from a custom XYZ source. Many tile servers
(notably the OSM Tile Usage Policy) reject blank User-Agents — the default
is set so users don't have to think about it. If you need a specific UA
for a private server, edit `config/custom_sources.json` directly:

```json
{
  "id": "user_topo",
  "type": "xyz",
  "xyz": { "url_template": "...", "max_zoom": 17, "format": "png", "subdomains": [] },
  "headers": { "User-Agent": "MyClient/1.0" }
}
```

(Note: `headers` is the catch-all for arbitrary HTTP headers; the explicit
`User-Agent` is set automatically and overridden by an entry in `headers`.)

## Coordinate system

XYZ tiles always use **Web Mercator (EPSG:3857)** by spec. The tile system
already handles Mercator via the existing high-zoom overlay path, so there's
no projection-honesty step at this layer — the geometry is structural, not
declared. (Compare to WMS, where the server can lie about delivered CRS;
see [docs/projection-honesty.md](projection-honesty.md).)

## Lifecycle

- **Add** a new source via the dialog → it appears in the dropdown immediately.
- **Edit** an existing source's URL or max_zoom → the tile manager picks
  up the new template the next time you select that source.
- **Disable** or **remove** a source → if you had it selected, the active
  selection falls back to the first built-in (Sentinel-2 by default).
- **Restart** Orbis → custom sources are re-loaded from
  `config/custom_sources.json`. The previously selected source restores
  if it still exists.

## Persistence

Custom XYZ sources live in `config/custom_sources.json` alongside WMS,
REST, Shapefile, and CSV entries. The file is human-editable; same
caveats as those other types.

## Caching

Custom XYZ tiles share the regular tile cache (`cache/tiles/`) with
built-in sources, scoped per source ID, governed by the same
"Tile cache (MB)" and "Cache age (days)" settings as everything else.

## What this does NOT do

- **Authenticated tile servers** with bespoke OAuth flows. Static API
  keys baked into the URL work (`?key=YOUR_KEY`); rotating-token flows
  don't.
- **Vector tiles** (.pbf, MVT, MapTiler vector). Raster only.
- **TMS y-axis flipping**. The y axis is treated as XYZ (top-to-bottom).
  TMS servers (which use bottom-to-top) need a small wrapper or proxy.
  When a real case shows up, the right fix is a `flip_y` flag rather
  than guessing.
- **Time-varying templates** with `{date}`. That's a built-in source
  feature (NASA GIBS uses it) but isn't in the custom-source schema yet.

## See also

- [`docs/csv-import.md`](csv-import.md), [`docs/projection-honesty.md`](projection-honesty.md)
- Built-in tile sources: `src/tile/source.rs::builtin_tile_sources`
- Conversion logic: `src/tile/source.rs::tile_source_from_custom`
