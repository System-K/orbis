// =============================================================================
// Orbis — Shapefile Loader (Esri Shapefile / .shp + .dbf + .prj)
// =============================================================================
// Drop-target for shapefile bundles: the user drags a .shp into Orbis and
// we produce a `GeoLayer` ready for rendering. The .prj sidecar is
// reconciled against the .shp bounding box via `projection::detect_projection`
// before we trust either, applying the projection-honesty pattern from
// docs/projection-honesty.md.
//
// Module layout:
// - projection.rs     pure CRS detection (no shapefile dep)
// - (mod.rs)          loader: file I/O, geometry → GeoFeature, reproject
// =============================================================================

pub mod projection;
