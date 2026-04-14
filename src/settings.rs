// =============================================================================
// Orbis — Persistent Settings (M6 + M9)
// =============================================================================
// Stores user settings as a JSON file.
//
// Philosophy: The user is not locked in. We provide sensible defaults,
// but also allow "crazy" values — as long as they don't crash the software.
//
// File: config/settings.json (created automatically)
// =============================================================================

use serde::{Deserialize, Serialize};
use std::fs;

/// Path to the settings file.
const SETTINGS_DIR: &str = "config";
const SETTINGS_FILE: &str = "config/settings.json";

/// All persistent settings.
///
/// New fields can be added at any time — `serde` ignores unknown fields
/// when loading and uses #[serde(default)] for missing ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // --- Language ---
    /// Language override. None = auto-detect system language.
    pub language: Option<String>,

    // --- FOV ---
    /// FOV at close zoom (degrees). Lower values reduce perspective distortion.
    pub fov_near_deg: f32,
    /// FOV at far zoom (degrees). Narrow FOV gives a natural "satellite view".
    pub fov_far_deg: f32,

    // --- Projection ---
    /// Globe projection mode: "Orthographic" (no distortion) or "Perspective" (3D depth).
    pub globe_projection: crate::camera::GlobeProjection,

    // --- Mouse controls ---
    /// Invert horizontal mouse axis when orbiting the globe.
    pub invert_mouse_x: bool,
    /// Invert vertical mouse axis when orbiting the globe.
    pub invert_mouse_y: bool,

    // --- M16: Tile cache ---
    /// Maximum tile cache size in megabytes (default: 500)
    pub tile_cache_max_mb: u32,
    /// Maximum tile age in days (0 = forever, default: 7)
    pub tile_cache_max_days: u32,
    /// Active tile source for zoom detail (e.g. "sentinel2", "osm")
    pub tile_source: String,
    /// Optional user-provided zoom-level offset applied on top of the per-source
    /// `recommended_zoom_bias`. No GUI — power-users edit `settings.json`.
    pub tile_zoom_bias: i32,

    // --- M9: Active layers ---
    /// List of active layers (provider ID + settings).
    /// Restored on startup so the user gets the same view as last time.
    pub active_layers: Vec<LayerConfig>,
}

/// Persistent configuration for a single active layer.
///
/// Stores which provider to use and the user's settings for that layer.
/// On startup, the layer is re-downloaded from its provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerConfig {
    /// Provider ID (must match a provider in the catalog)
    pub provider_id: String,
    /// User-set opacity (0.0–1.0)
    pub opacity: f32,
    /// Whether the layer is enabled (visible)
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: None,
            fov_near_deg: 15.0,
            fov_far_deg: 10.0,
            globe_projection: crate::camera::GlobeProjection::Orthographic,
            invert_mouse_x: false,
            invert_mouse_y: false,
            tile_cache_max_mb: 500,
            tile_cache_max_days: 7,
            tile_source: "sentinel2".to_string(),
            tile_zoom_bias: 0,
            // Default: VIIRS True Color clouds + coordinate grid
            active_layers: vec![
                LayerConfig {
                    provider_id: "builtin:grid".to_string(),
                    opacity: 0.7,
                    enabled: true,
                },
                LayerConfig {
                    provider_id: "gibs_viirs_true_color".to_string(),
                    opacity: 0.45,
                    enabled: true,
                },
            ],
        }
    }
}

impl Settings {
    /// Loads settings from the JSON file.
    ///
    /// If the file doesn't exist or is malformed,
    /// defaults are used (no crash).
    pub fn load() -> Self {
        let path = crate::app_path(SETTINGS_FILE);
        if !path.exists() {
            log::info!("No settings file found, using defaults");
            return Self::default();
        }

        match fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(settings) => {
                    log::info!("Settings loaded: {}", path.display());
                    settings
                }
                Err(e) => {
                    log::warn!(
                        "Settings file malformed ({}), using defaults: {}",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                log::warn!("Settings file not readable: {}", e);
                Self::default()
            }
        }
    }

    /// Saves the current settings as a JSON file.
    ///
    /// Creates the config directory automatically if needed.
    /// Errors during saving are logged but don't cause a crash.
    pub fn save(&self) {
        let dir = crate::app_path(SETTINGS_DIR);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("Could not create config directory: {}", e);
            return;
        }

        let path = crate::app_path(SETTINGS_FILE);
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, &json) {
                    log::warn!("Settings could not be saved: {}", e);
                } else {
                    log::debug!("Settings saved: {}", path.display());
                }
            }
            Err(e) => {
                log::warn!("Settings serialization failed: {}", e);
            }
        }
    }

    /// FOV value for a given zoom distance (in radians).
    ///
    /// Smoothstep interpolation between near and far FOV based on distance.
    /// Low FOV values (10–15°) produce a natural, distortion-free view.
    pub fn compute_fov(&self, distance: f32, distance_min: f32, distance_max: f32) -> f32 {
        let fov_near = self.fov_near_deg.to_radians();
        let fov_far = self.fov_far_deg.to_radians();

        let range = distance_max - distance_min;
        if range.abs() < 0.001 {
            return fov_near;
        }

        let t = ((distance - distance_min) / range).clamp(0.0, 1.0);
        // Smoothstep for smooth transition
        let t_smooth = t * t * (3.0 - 2.0 * t);

        fov_near + (fov_far - fov_near) * t_smooth
    }

    /// Updates the active_layers list from the current layer stack state.
    ///
    /// Called when layers are added, removed, or their settings change.
    pub fn sync_layers(&mut self, layers: &[(String, f32, bool)]) {
        self.active_layers = layers
            .iter()
            .map(|(provider_id, opacity, enabled)| LayerConfig {
                provider_id: provider_id.clone(),
                opacity: *opacity,
                enabled: *enabled,
            })
            .collect();
    }

    /// Validates `tile_source` against a list of known source IDs and falls
    /// back to the first valid one if the stored value is unknown.
    ///
    /// Called immediately after `load()` so the rest of the application never
    /// sees a `tile_source` value that cannot be resolved by the tile registry.
    /// Guards against hand-edited or corrupted `settings.json` files.
    pub fn sanitize_tile_source(&mut self, valid: &[(String, String)]) {
        if valid.is_empty() {
            return;
        }
        if !valid.iter().any(|(id, _)| id == &self.tile_source) {
            let fallback = valid[0].0.clone();
            log::warn!(
                "Settings: unknown tile_source '{}', falling back to '{}'",
                self.tile_source, fallback,
            );
            self.tile_source = fallback;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_sources() -> Vec<(String, String)> {
        vec![
            ("sentinel2".to_string(), "Sentinel-2".to_string()),
            ("osm".to_string(), "OpenStreetMap".to_string()),
        ]
    }

    #[test]
    fn sanitize_keeps_valid_source() {
        let mut s = Settings::default();
        s.tile_source = "osm".to_string();
        s.sanitize_tile_source(&valid_sources());
        assert_eq!(s.tile_source, "osm");
    }

    #[test]
    fn sanitize_falls_back_when_unknown() {
        let mut s = Settings::default();
        s.tile_source = "mapbox-invalid-id".to_string();
        s.sanitize_tile_source(&valid_sources());
        assert_eq!(s.tile_source, "sentinel2");
    }

    #[test]
    fn sanitize_falls_back_when_empty_string() {
        let mut s = Settings::default();
        s.tile_source = String::new();
        s.sanitize_tile_source(&valid_sources());
        assert_eq!(s.tile_source, "sentinel2");
    }

    #[test]
    fn sanitize_noop_on_empty_valid_list() {
        let mut s = Settings::default();
        s.tile_source = "whatever".to_string();
        s.sanitize_tile_source(&[]);
        // no valid sources → leave alone (better than clearing)
        assert_eq!(s.tile_source, "whatever");
    }
}
