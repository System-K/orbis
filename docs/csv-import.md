# CSV Import

Drop a CSV (or TSV) of point coordinates into Orbis and it appears as a layer
of markers. Each row is one point. Optional columns become labels and
properties.

## Required columns

The CSV header must contain a latitude column and a longitude column. The
loader matches headers case-insensitively against a known synonym list:

| Role | Recognised header names |
|------|-------------------------|
| **Latitude** | `lat`, `latitude`, `y`, `decimal_latitude`, `lat_deg`, `lat_dd` |
| **Longitude** | `lon`, `long`, `longitude`, `lng`, `x`, `decimal_longitude`, `lon_deg`, `lon_dd` |

`x` and `y` follow cartographic convention: `x` = easting (longitude-like),
`y` = northing (latitude-like). If you have a CSV that uses x/y the other
way around, rename the columns before loading.

## Optional columns

| Role | Recognised header names | Where it goes |
|------|-------------------------|---------------|
| **Name** | `name`, `title`, `label`, `site`, `site_name`, `place`, `place_name` | Becomes the marker's visible label (toggle with `G`) |
| **Description** | `description`, `desc`, `notes`, `note`, `comment`, `comments`, `details`, `info` | Stored as `properties["description"]` for tooltip / inspector use |

Any other columns in the file (e.g. `population`, `country`, `url`) are
preserved verbatim in `feature.properties` under their original header name,
so nothing is lost. The lat/lon columns themselves are NOT duplicated as
properties — the geometry already carries them.

## Header normalisation

Headers are matched after a normalisation pass:

1. Trim whitespace
2. Lowercase
3. Strip parenthetical suffix: `Lat (deg)` → `lat`
4. Replace whitespace and hyphens with `_`: `site-name` → `site_name`

After normalisation, the header is **exact-matched** against the synonym
list. Substring or fuzzy matching is intentionally avoided — false-role
assignments are worse than rejecting an unrecognised header.

## Delimiter detection

The loader sniffs the first non-empty line:

1. Contains a tab → TSV (`\t`)
2. Contains `;` and no `,` → German-style CSV (`;`) — common in Excel exports
3. Otherwise → comma (`,`)

Quoted strings with embedded commas / semicolons / newlines work as
specified by [RFC 4180](https://datatracker.ietf.org/doc/html/rfc4180).

## Decimal separator

Both `.` and `,` are accepted as the decimal separator within a single cell:

- `52.5` → 52.5
- `52,5` → 52.5 (German Excel)

Strings containing both, or thousands separators (e.g. `1,234.5`), are
rejected to avoid ambiguity. The row is dropped with a warning.

## Coordinate validation

Lat must satisfy `|lat| ≤ 90`, lon must satisfy `|lon| ≤ 180`. Out-of-range
rows are dropped with a single summary warning per file.

If your data is in projected coordinates (UTM meters, Web Mercator meters,
etc.), the loader will reject every row. CSV has no projection metadata, so
there's nothing to detect from. Reproject to WGS84 in your source tool
(QGIS, OGR, pandas) before importing.

## Encoding

UTF-8 only. A UTF-8 BOM at the file start is stripped automatically. Files
in Windows-1252, Latin-1, or other legacy encodings will produce mangled
labels — convert them first.

## Custom-source integration

The "Add Custom Source" dialog has CSV as a 5th source type alongside WMS,
XYZ, REST, and Shapefile. Pick a `.csv` (or `.tsv`) file via the Browse
button or paste a path; it will be loaded synchronously and re-loaded if
the path changes via Edit-source.

Custom CSV sources are stored in `config/custom_sources.json` and reload
automatically at startup.

## Drag and drop

Dropping a `.csv` or `.tsv` file onto the window loads it as an ad-hoc
layer (not persisted across restarts). Same column-detection rules apply.

## Example files

A minimal valid CSV:

```csv
name,lat,lon
Berlin,52.5,13.4
Paris,48.8,2.35
Tokyo,35.7,139.7
```

A Darwin Core-style biodiversity export:

```csv
occurrence_id,decimal_latitude,decimal_longitude,scientific_name,collected_on
1234,52.5,13.4,Bombus terrestris,2024-06-15
1235,48.8,2.35,Apis mellifera,2024-07-02
```

The first uses the simple synonyms; the second uses the GBIF / Darwin Core
column names that map to the same roles. Both load without configuration.

## What this loader does NOT do

- **Reproject from non-WGS84 CRSes.** No metadata sidecar means no CRS to
  detect. Out-of-range rows get dropped instead.
- **Multi-locale headers.** `Breitengrad` / `Längengrad` (German), `緯度` /
  `経度` (Japanese) etc. are not recognised. English synonyms only.
- **Schemas with no header row.** The first row is always treated as the
  header. Headerless CSVs need a header line added.
- **Lines or polygons.** CSV is for points. For lines/polygons use GeoJSON
  or Shapefile.

## See also

- [`docs/projection-honesty.md`](projection-honesty.md) — the broader pattern
  for trusting (or not) what data sources claim about their CRS. CSV's
  contribution to that pattern is the bounds check (anything outside
  ±180/±90 isn't WGS84) — same shape, lighter weight, since there's no
  metadata to reconcile against.
