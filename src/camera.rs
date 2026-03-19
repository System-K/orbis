// =============================================================================
// Orbis — Camera (Orbit Camera)
// =============================================================================
// An orbit camera revolves around a fixed point (Earth) at a given distance.
// Position is stored as spherical coordinates:
//
//   - yaw:      Horizontal angle (longitude, 0..2π)
//   - pitch:    Vertical angle (latitude, -89°..+89°)
//   - distance: Distance from center
//
// This is more intuitive than cartesian xyz coordinates:
// Mouse left/right → yaw changes
// Mouse up/down    → pitch changes
// Scroll wheel     → distance changes
//
// Each frame, spherical coordinates are converted to a cartesian position
// and from that the view-projection matrix is built.
// =============================================================================

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use serde::{Serialize, Deserialize};

/// Globe projection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlobeProjection {
    /// Orthographic: zero distortion, globe appears as a flat disc.
    /// This is what Google Maps, Apple Maps, etc. use.
    /// No perspective warping at all — default and recommended.
    Orthographic,
    /// Perspective: classic 3D view with depth perception.
    /// Slight edge distortion, but gives a more immersive feel
    /// at close zoom levels (xPlanet-style).
    Perspective,
}

impl Default for GlobeProjection {
    fn default() -> Self {
        GlobeProjection::Orthographic
    }
}

/// Default start position: Gulf of Guinea (0°N, 0°E)
///
/// In our coordinate system (see mesh.rs):
///   phi=0 → u=0 → 180°W (Pacific, position +X)
///   phi=π → u=0.5 → 0°E (Gulf of Guinea, position -X)
///   Camera at +X looks at -X → sees 0°E → yaw = +π/2
const DEFAULT_YAW: f32 = std::f32::consts::FRAC_PI_2;
const DEFAULT_PITCH: f32 = 0.0;
const DEFAULT_DISTANCE: f32 = 8.0;

/// Orbit camera around the Earth sphere.
pub struct Camera {
    /// Horizontal angle in radians (like longitude)
    pub yaw: f32,
    /// Vertical angle in radians (like latitude, clamped)
    pub pitch: f32,
    /// Distance from Earth's center
    pub distance: f32,

    /// Minimum zoom distance (close enough, but not inside the Earth)
    pub distance_min: f32,
    /// Maximum zoom distance (far enough for overview)
    pub distance_max: f32,

    /// Point the camera orbits around (Earth center)
    pub target: Vec3,
    /// Which direction is "up"
    pub up: Vec3,

    // Projection parameters
    pub fov_y: f32,
    pub aspect: f32,
    pub z_near: f32,
    pub z_far: f32,

    /// Projection mode for the globe view.
    pub projection: GlobeProjection,

    /// Invert horizontal mouse axis (orbit left/right)
    pub invert_x: bool,
    /// Invert vertical mouse axis (orbit up/down)
    pub invert_y: bool,
}

impl Camera {
    pub fn new(aspect: f32) -> Self {
        Self {
            // Start position: Gulf of Guinea (0°N, 0°E)
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            distance: DEFAULT_DISTANCE,

            distance_min: 2.0,    // Close: Earth fills ~60% of screen
            distance_max: 15.0,   // Far: Earth is small

            target: Vec3::ZERO,
            up: Vec3::Y,

            // FOV is controlled externally by Settings.
            // Start value is a placeholder — main.rs calls update_fov() immediately.
            fov_y: 25.0_f32.to_radians(),
            aspect,
            z_near: 0.1,
            z_far: 100.0,

            projection: GlobeProjection::default(),
            invert_x: false,
            invert_y: false,
        }
    }

    /// Computes the cartesian camera position from spherical coordinates.
    ///
    /// Spherical → Cartesian:
    ///   x = distance * cos(pitch) * sin(yaw)
    ///   y = distance * sin(pitch)
    ///   z = distance * cos(pitch) * cos(yaw)
    ///
    /// Same formula as sphere generation (see mesh.rs),
    /// just with yaw/pitch instead of theta/phi.
    fn eye_position(&self) -> Vec3 {
        let x = self.distance * self.pitch.cos() * self.yaw.sin();
        let y = self.distance * self.pitch.sin();
        let z = self.distance * self.pitch.cos() * self.yaw.cos();
        self.target + Vec3::new(x, y, z)
    }

    /// Rotates the camera based on mouse movement (in pixels).
    ///
    /// `sensitivity` scales mouse movement to angles.
    /// Pitch is clamped to ±89° to prevent the camera from flipping
    /// over the poles (that would reverse the "up" direction → gimbal lock).
    pub fn orbit(&mut self, delta_x: f32, delta_y: f32) {
        let sensitivity = 0.005;

        let dx = if self.invert_x { delta_x } else { -delta_x };
        let dy = if self.invert_y { -delta_y } else { delta_y };

        self.yaw += dx * sensitivity;
        self.pitch += dy * sensitivity;

        // Clamp pitch: not quite 90°, otherwise gimbal lock
        let max_pitch = 89.0_f32.to_radians();
        self.pitch = self.pitch.clamp(-max_pitch, max_pitch);
    }

    /// Zooms in/out based on scroll wheel delta.
    ///
    /// Positive delta = zoom in (closer), negative = zoom out.
    /// Multiplicative instead of additive: zoom feels uniform
    /// regardless of distance.
    pub fn zoom(&mut self, delta: f32) {
        let zoom_speed = 0.1;
        self.distance *= 1.0 - delta * zoom_speed;
        self.distance = self.distance.clamp(self.distance_min, self.distance_max);
    }

    /// Resets the camera to start position (Gulf of Guinea).
    pub fn reset(&mut self) {
        self.yaw = DEFAULT_YAW;
        self.pitch = DEFAULT_PITCH;
        self.distance = DEFAULT_DISTANCE;
    }

    pub fn build_view_projection_matrix(&self) -> Mat4 {
        let eye = self.eye_position();
        let view = Mat4::look_at_rh(eye, self.target, self.up);

        let proj = match self.projection {
            GlobeProjection::Perspective => {
                Mat4::perspective_rh(self.fov_y, self.aspect, self.z_near, self.z_far)
            }
            GlobeProjection::Orthographic => {
                // Compute ortho extents so that framing matches perspective.
                // half_h = distance * tan(fov/2) gives identical angular coverage.
                let half_h = self.distance * (self.fov_y / 2.0).tan();
                let half_w = half_h * self.aspect;
                Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, self.z_near, self.z_far)
            }
        };

        proj * view
    }

    /// Builds an orthographic view-projection matrix for the 2D map.
    ///
    /// The camera looks along -Z onto the XY plane, where the map quad lies.
    /// `map_zoom` determines how much of the map is visible (smaller = closer).
    /// `map_pan` shifts the visible region.
    ///
    /// The quad has a 2:1 aspect ratio (half_width : half_height).
    /// The orthographic projection is adjusted to the window aspect
    /// so the map is not distorted.
    pub fn build_ortho_view_projection(
        &self,
        map_zoom: f32,
        map_pan: (f32, f32),
        quad_half_width: f32,
    ) -> Mat4 {
        // Visible height based on zoom level
        // map_zoom = 1.0 → entire quad visible
        // map_zoom = 2.0 → half quad visible (zoomed in)
        let visible_half_height = quad_half_width / (2.0 * map_zoom);
        let visible_half_width = visible_half_height * self.aspect;

        // Camera looks from +Z onto the XY plane
        let eye = Vec3::new(map_pan.0, map_pan.1, 10.0);
        let target = Vec3::new(map_pan.0, map_pan.1, 0.0);
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);

        let proj = Mat4::orthographic_rh(
            -visible_half_width,
            visible_half_width,
            -visible_half_height,
            visible_half_height,
            0.1,
            100.0,
        );

        proj * view
    }

    /// Builds a CameraUniform for the 2D map mode.
    pub fn to_map_uniform(
        &self,
        sun_dir: Vec3,
        map_zoom: f32,
        map_pan: (f32, f32),
        quad_half_width: f32,
    ) -> CameraUniform {
        let vp = self.build_ortho_view_projection(map_zoom, map_pan, quad_half_width);
        let eye = Vec3::new(map_pan.0, map_pan.1, 10.0);
        CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            eye_pos: [eye.x, eye.y, eye.z],
            _pad1: 0.0,
            sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z],
            view_mode: 1.0, // Map2D
        }
    }

    /// Builds the GPU uniform with camera matrix, eye position and sun direction.
    ///
    /// `sun_dir` comes from `sun::sun_direction_now()` and is updated every
    /// frame so the lighting follows the real-time sun position.
    pub fn to_uniform(&self, sun_dir: glam::Vec3) -> CameraUniform {
        let eye = self.eye_position();
        CameraUniform {
            view_proj: self.build_view_projection_matrix().to_cols_array_2d(),
            eye_pos: [eye.x, eye.y, eye.z],
            _pad1: 0.0,
            sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z],
            view_mode: 0.0, // Globe3D
        }
    }

    pub fn set_aspect(&mut self, width: u32, height: u32) {
        self.aspect = width as f32 / height as f32;
    }
}

/// The data structure sent to the GPU.
///
/// Contains, besides the view-projection matrix:
/// - `eye_pos`: Camera position (for specular lighting / fallback light)
/// - `sun_dir`: Direction to the sun (for day/night lighting)
///
/// GPU alignment: vec3 has 12 bytes, but the GPU expects 16-byte alignment.
/// Therefore `_padding` after each vec3, so the next field is correctly aligned.
///
/// Memory layout (offsets in bytes):
///   view_proj:     0..64   (4×4 matrix = 64 bytes)
///   eye_pos:      64..76   (vec3 = 12 bytes)
///   _pad1:        76..80   (4 bytes padding)
///   sun_dir:      80..92   (vec3 = 12 bytes)
///   view_mode:    92..96   (f32: 0.0 = Globe3D, 1.0 = Map2D)
///   Total: 96 bytes
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub eye_pos: [f32; 3],
    pub _pad1: f32,
    pub sun_dir: [f32; 3],
    /// 0.0 = Globe3D, 1.0 = Map2D (replaces former _pad2;
    /// existing shaders ignore this field via automatic vec3 padding)
    pub view_mode: f32,
}
