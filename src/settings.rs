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
}
