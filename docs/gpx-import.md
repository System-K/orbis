# GPX Import

Drop a `.gpx` file into Orbis and its waypoints, routes, and tracks
appear as a layer. The GPX schema is well-defined and always WGS84, so
there's no projection-detection step — the loader is the simplest of
the three vector format paths.

## What maps to what

| GPX element | Internal representation | Notes |
|-------------|------------------------|-------|
| `<wpt>` (waypoint) | `GeoFeature::Point` | `<name>` → `style.label`, `<desc>` → `properties["description"]` |
| `<rte>` (route) | `GeoFeature::LineString` (one per route) | `<name>` → `style.label`, `gpx_kind = "route"` in properties |
| `<trk>` + `<trkseg>` (track segments) | `GeoFeature::LineString` (one per segment) | `<name>` repeats on every segment so the user can tell which track a segment belongs to. `gpx_kind = "track"` in properties |

A track with three `<trkseg>` elements produces three separate features.
That matches GPX's own model — segments represent continuous spans of
GPS reception, with breaks between them, so they shouldn't be visually
joined.

## Properties preserved

For each waypoint, the loader copies these fields to
`feature.properties` when present and non-empty:

| Field | Property key | Type |
|-------|--------------|------|
| `<desc>` | `description` | string |
| `<cmt>` | `comment` | string |
| `<ele>` | `elevation_m` | number (meters) |
| `<time>` | `time` | ISO-8601 string |
| `<src>` | `source` | string |
| `<type>` | `type` | string |
| `<sym>` | `symbol` | string |

For routes and tracks, only `description`, `comment`, `type`, and
`gpx_kind` (`"route"` or `"track"`) are populated — segment-level
metadata isn't part of the GPX spec.

## Coordinate validation

GPX is always WGS84 by spec. The loader still bounds-checks each point
(|lat| ≤ 90, |lon| ≤ 180) and drops any that don't fit, matching the
CSV import policy. A summary warning is logged at the end of the load.

## What's NOT done

- **Per-trackpoint properties on track segments.** The current loader
  attaches metadata to the segment feature as a whole, not to
  individual points within the LineString. If you need per-point
  attributes (elevation profile, speed, time at each fix), a future
  pass could fan out trackpoints into individual Point features.
- **GPX extensions.** The GPX 1.1 extensions namespace (Garmin / Strava
  / etc.) is parsed by the underlying `gpx` crate but not surfaced in
  Orbis's properties. Easy to add once a real use case shows up.
- **Time-based playback / animation.** A track has timestamps per
  point; nothing renders them yet.

## Custom-source integration

The "Add Custom Source" dialog has GPX as the 6th source type. Pick a
`.gpx` file via Browse or paste a path. Persistent — reloads on
restart, re-syncs on path change, removes from the marker system on
disable. Same lifecycle as Shapefile and CSV.

## Drag and drop

Drop a `.gpx` onto the window for an ad-hoc layer (not persisted across
restarts). Same dispatcher as `.geojson` / `.shp` / `.csv`.

## Architecture note

GPX is the third format to use the file-based-vector-source pattern,
which triggered the rule of three: `ShapefileSourceManager` and
`CsvSourceManager` were consolidated into a single
`LocalFileSourceManager<K>` parameterised by a `LocalFileSourceKind`
trait. `ShapefileKind`, `CsvKind`, and `GpxKind` are zero-sized marker
types that supply the source-type filter, config-path extractor, and
loader function. Adding a fourth file format (KML, FlatGeobuf,
TopoJSON) is now a 30-line `Kind` impl plus the loader module.

## Example file

A minimal GPX with one waypoint, one track, and one route:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="orbis-example"
     xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="52.5" lon="13.4">
    <name>Berlin</name>
    <desc>The capital</desc>
    <ele>34</ele>
  </wpt>
  <trk>
    <name>Sunday hike</name>
    <trkseg>
      <trkpt lat="48.8" lon="2.35"></trkpt>
      <trkpt lat="48.81" lon="2.36"></trkpt>
      <trkpt lat="48.82" lon="2.37"></trkpt>
    </trkseg>
  </trk>
  <rte>
    <name>To the harbour</name>
    <rtept lat="35.7" lon="139.7"></rtept>
    <rtept lat="35.6" lon="139.7"></rtept>
  </rte>
</gpx>
```

## See also

- [`docs/csv-import.md`](csv-import.md) — analogous file-based layer flow
- [`docs/projection-honesty.md`](projection-honesty.md) — design pattern
  for formats that DO have projection metadata to reconcile (GPX
  doesn't, but Shapefile and WMS do)
