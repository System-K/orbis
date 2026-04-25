# Projection Honesty — Defending Against Dishonest Geo Sources

A note on a class of bug we hit in WMS and will hit again in shapefile,
GeoTIFF, GeoPackage, GML — anything where a source declares a coordinate
reference system in metadata while the actual coordinates are in something
else. Distilled from the WMS rework (commits `e55729e..55a1036`).

## The class of problem

A source ships geometric data alongside a CRS declaration:

| Format     | Declaration | Payload |
|------------|-------------|---------|
| WMS        | `GetCapabilities` lists `EPSG:4326` | GetMap returns Mercator-shaped pixels |
| Shapefile  | `.prj` file says `EPSG:4326` | `.shp` coordinates are in metres (UTM) |
| GeoTIFF    | `GeoKeyDirectoryTag` says one thing | Pixel grid is georeferenced as another |
| GeoJSON    | RFC 7946 mandates `WGS84` | Coordinates are local grid (common in EU exports) |
| GML        | `srsName` attribute | Geometries don't match it |

The declaration is **trusted metadata**. When a producer's tool chain
mislabels output (Terrestris-class servers proxy OSM tiles in Mercator
under whatever CRS the client asks for; QGIS users export shapefiles
without updating `.prj`; ArcGIS sometimes writes the wrong projection
into a TIFF header), trusting the declaration silently distorts geometry.

Symptom is always the same: things land on the map at the wrong place.
Often subtly — a few pixels off near the equator, badly off near the
poles. Easy to dismiss as "rounding". The right diagnosis is **the
source lied about its CRS**.

## The general defence

Two layers, used together:

### 1. Structural preference

When the source offers multiple representations of the same data, **prefer
the one that is structurally hard to lie about**. The provider can't fake
its own native format.

- **WMS**: prefer `EPSG:3857` over `EPSG:4326`. Tile-backed servers ARE
  Mercator natively — asking for it returns the cache verbatim. Proper
  WMS servers implement Mercator through the same engine as everything
  else; if Mercator is wrong, 4326 was already wrong too. Either way,
  asking for Mercator dodges the lie.
- **Shapefile**: prefer the coordinate-bounds heuristic (see below) over
  trusting `.prj`. The bounds are the data; they can't be faked except
  by faking the data itself.
- **GeoTIFF**: prefer the embedded `ModelTiepointTag` + `ModelPixelScaleTag`
  over a sidecar `.tfw`/`.prj`. Sidecars get out of sync; tags travel
  with the file.

The pattern: **trust the most-bound-up-with-the-data declaration**.

### 2. Validation probe

For cases where structural preference isn't conclusive — only one CRS
declared, no native form to compare against — run a cheap consistency
test before trusting the declaration.

A probe doesn't need to be expensive. It just needs ONE bit of ground
truth that the declaration's correctness can be checked against:

- **WMS**: fetch the same world view in two CRSes (declared + 3857),
  bring both into the same equirect frame, mean-RGB-diff. If different,
  the declared CRS is wrong. Code in [`src/wms/probe.rs`](../src/wms/probe.rs).
- **Shapefile**: read the bounding box from the `.shp` header. For a
  declared `EPSG:4326`, ranges must satisfy `|x| ≤ 180`, `|y| ≤ 90`.
  Anything else (e.g. `x ≈ 600000`, `y ≈ 5500000`) is meters — try UTM
  zones. For a declared UTM, easting should be `[100000, 900000]` and
  northing `[0, 10000000]`. Wildly out → declaration is wrong.
- **GeoTIFF**: sample a few corner pixels via the declared transform,
  check they land on plausible Earth coordinates. If a pixel near the
  centre maps to lat=200°, the geokey is lying.
- **GeoJSON**: by spec all coordinates ARE WGS84, so the only check is
  bounds (`|lon| ≤ 180`, `|lat| ≤ 90`). Out-of-range → producer ignored
  the spec, treat as projected and try to detect the actual CRS.

The probe's verdict is three-state: `Consistent` (trust), `Inconsistent`
(switch), `Inconclusive` (default to trusting — false-switch is worse
than false-trust).

## How it landed in WMS

| File | Role |
|------|------|
| [`wms/crs.rs`](../src/wms/crs.rs) | Typed `Crs` enum + lat/lon → fractional pixel forward transform per CRS |
| [`wms/reproject.rs`](../src/wms/reproject.rs) | Generic `to_equirect(src, src_crs, src_bbox, ...)` — one loop, dispatches by Crs |
| [`wms/capabilities.rs`](../src/wms/capabilities.rs) | Stream-parse a `GetCapabilities` doc, return supported CRSes for one layer |
| [`wms/behavior.rs`](../src/wms/behavior.rs) | `SourceBehavior` (request_crs + response_crs + bbox), preference order, per-source 30-day cache, URL builder |
| [`wms/probe.rs`](../src/wms/probe.rs) | Two-image consistency check — pixel-space, alpha-skipping |

Discovery flow on first fetch:

```
GetCapabilities → parse → pick most-preferred CRS (Mercator wins) →
build SourceBehavior → persist as <id>.behavior.json → use it →
re-discover after 30 days
```

When discovery fails, fall back to a safe default (assume EPSG:4326,
trust the server). Falling back is better than refusing to render —
the user gets a layer, possibly subtly wrong, and can complain.

## Sketch: applying this to shapefile

Adding shapefile support without re-living the WMS bug means baking
the same defence in from day one.

**1. Don't parse `.prj` and stop.** Use it as the *first* hypothesis,
not the only one.

**2. Pure detection module.** Shape of it:

```rust
// src/shp/projection.rs
pub enum ProjectionVerdict {
    /// .prj declares CRS X, .shp bounds plausible for X — trust it.
    Confirmed(Crs),
    /// .prj says X but bounds say Y. Use Y, log a warning.
    Corrected { declared: Crs, actual: Crs },
    /// No .prj or unparseable; bounds suggest a likely CRS.
    Inferred(Crs),
    /// Bounds don't match anything we recognise. Render at our peril,
    /// or skip the layer with a clear error.
    Unknown(Bbox),
}

pub fn detect_projection(prj_text: Option<&str>, shp_bbox: Bbox) -> ProjectionVerdict;
```

Test cases that ARE the API contract — write these before the parser:

- `prj=EPSG:4326`, `bbox=(-10, 40, 10, 60)` → `Confirmed(WGS84)`
- `prj=EPSG:4326`, `bbox=(550000, 5400000, 700000, 5600000)` → `Corrected { declared: WGS84, actual: UTM32N }` (or whichever zone fits)
- `prj=None`, `bbox=(-180, -90, 180, 90)` → `Inferred(WGS84)`
- `prj=EPSG:25832`, `bbox=(550000, 5400000, ...)` → `Confirmed(UTM32N)`
- `prj=None`, `bbox=(50, 50, 60, 60)` → `Unknown` (could be degrees, could be a local grid)

**3. Bounds-table for known CRSes.** Each CRS variant carries its
plausible-bounds region. The verdict function compares the file's
bbox against that table.

**4. Always reproject through your generic engine.** Whatever the
detected CRS, run the geometry through one transform pipeline that
ends in WGS84 (or whatever Orbis uses internally). No format-specific
shortcut paths — `reproject::to_wgs84(geometry, source_crs)` and
nothing else. This is the same lesson as
[`wms/reproject.rs`](../src/wms/reproject.rs): one engine, dispatched
by CRS. New CRSes are an extension point, not a fork.

**5. Cache the verdict per file.** Same pattern as
`<id>.behavior.json`: detect once, persist, re-detect on file mtime
change. Avoids re-running detection on every load.

## Things to avoid

- **Per-source manual flags** (`reproject_mercator: bool` was this — it
  shifted the burden of correctness onto whoever added the source.
  Doesn't scale, doesn't survive new servers, can't be done at all by
  users adding sources at runtime.)
- **Trusting metadata silently.** If you do trust it (cheap path, common
  case), at least log what you trusted at INFO level so a user
  debugging "why is this in the wrong place" has a paper trail.
- **Refusing to render on detection failure.** A layer in slightly the
  wrong place is more useful than no layer. Render with a warning;
  don't drop.
- **Rolling separate parsers for the same format.** If `wms_caps.rs`
  and `wms/capabilities.rs` ever diverge in what they extract from the
  same XML, that's a footgun. Periodic chore: pick one, route the
  other through it.

## When to skip the probe

The probe is belt-and-suspenders. Skip it when:

- The structural-preference rule reaches a unique answer (WMS sees
  3857 in caps → we're done, don't probe).
- The cost of the probe is comparable to the cost of just rendering
  and seeing if it looks wrong (small one-off layers).
- You can't construct a meaningful comparison (only one declared CRS
  AND no second-source ground truth is available).

In WMS the probe primitive exists ([`probe.rs`](../src/wms/probe.rs))
but is currently not invoked from the fetch path — Mercator-preference
already covers Terrestris-class lies. The primitive is staged for the
day a server appears that declares 4326, refuses 3857, and lies. If
that server doesn't materialise, the primitive sits there as
documentation of how we'd respond.
