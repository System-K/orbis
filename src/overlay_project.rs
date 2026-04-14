// =============================================================================
// Orbis — Screen-Space Overlay Projection
// =============================================================================
// Projects world-space satellites, ground tracks, and planets onto screen-space
// egui::Pos2 coordinates for the overlay render pass. Extracted from main.rs
// to keep main.rs under the 45 KB refactor budget (< 50 KB hard limit).
// =============================================================================

use crate::{GpuState, QUAD_HALF_WIDTH, ViewMode, gui, label, planets};

impl GpuState {
    pub(crate) fn project_screen_overlays(&mut self, sat_utc: chrono::DateTime<chrono::Utc>) {
        let vp = match self.view_mode {
            ViewMode::Globe3D => self.camera.build_view_projection_matrix(),
            ViewMode::Map2D => self.camera.build_ortho_view_projection(
                self.map_zoom, self.map_pan, QUAD_HALF_WIDTH,
            ),
        };
        let eye = self.camera.eye_position();
        let label_config = label::LabelConfig {
            view_proj: vp,
            eye_pos: eye,
            screen_width: self.config.width as f32 / self.window.scale_factor() as f32,
            screen_height: self.config.height as f32 / self.window.scale_factor() as f32,
            is_map: self.view_mode == ViewMode::Map2D,
            quad_hw: QUAD_HALF_WIDTH,
        };

        // Generate GeoJSON labels
        self.gui_state.geo_labels =
            label::generate_labels(self.marker_system.geo_layers(), &label_config);

        // Sync layer info for GUI
        self.gui_state.geo_layer_info = self.marker_system.geo_layers().iter().map(|l| {
            gui::GeoLayerInfo {
                name: l.name.clone(),
                visible: l.visible,
                point_count: l.points().count(),
                line_count: l.lines().count(),
                polygon_count: l.polygons().count(),
            }
        }).collect();
        {
            let mut attrs: Vec<String> = Vec::new();
            for l in self.marker_system.geo_layers() {
                if let Some(ref a) = l.attribution {
                    if !attrs.contains(a) {
                        attrs.push(a.clone());
                    }
                }
            }
            self.gui_state.geo_attributions = attrs;
        }

        let sw = label_config.screen_width;
        let sh = label_config.screen_height;

        // Project satellite positions to screen (only enabled ones)
        self.gui_state.satellite_markers.clear();
        for sat in self.satellite_tracker.states() {
            if !self.gui_state.enabled_satellites.contains(&sat.norad_id) {
                continue;
            }
            let lat_r = (sat.latitude as f32).to_radians();
            let lon_r = (sat.longitude as f32).to_radians();
            let wp = if label_config.is_map {
                let u = (sat.longitude as f32 + 180.0) / 360.0;
                let v = (90.0 - sat.latitude as f32) / 180.0;
                glam::Vec3::new(
                    (u * 2.0 - 1.0) * QUAD_HALF_WIDTH,
                    (1.0 - v * 2.0) * QUAD_HALF_WIDTH * 0.5,
                    0.02,
                )
            } else {
                glam::Vec3::new(
                    -1.005 * lat_r.cos() * lon_r.cos(),
                     1.005 * lat_r.sin(),
                    -1.005 * lat_r.cos() * lon_r.sin(),
                )
            };

            let vis = if label_config.is_map {
                true
            } else {
                let n = wp.normalize();
                let tc = (eye - wp).normalize();
                n.dot(tc) <= 0.05
            };

            let c = vp * glam::Vec4::new(wp.x, wp.y, wp.z, 1.0);
            if c.w <= 0.0 { continue; }
            let nx = c.x / c.w;
            let ny = c.y / c.w;
            if nx.abs() > 1.2 || ny.abs() > 1.2 { continue; }

            self.gui_state.satellite_markers.push(gui::SatelliteMarker {
                x: (nx + 1.0) * 0.5 * sw,
                y: (1.0 - ny) * 0.5 * sh,
                name: sat.name.clone(),
                norad_id: sat.norad_id,
                altitude_km: sat.altitude_km,
                velocity_km_s: sat.velocity_km_s,
                visible: vis,
            });
        }

        // Project ground tracks (only enabled satellites)
        self.gui_state.satellite_tracks.clear();
        for sat in self.satellite_tracker.states() {
            if !self.gui_state.enabled_satellites.contains(&sat.norad_id) {
                continue;
            }
            let track_pts = self.satellite_tracker.compute_ground_track(
                sat.norad_id, &sat_utc, 90.0, 90.0, 2.0,
            );
            let mut past_segments: Vec<Vec<egui::Pos2>> = Vec::new();
            let mut future_segments: Vec<Vec<egui::Pos2>> = Vec::new();
            let mut cur_past: Vec<egui::Pos2> = Vec::new();
            let mut cur_future: Vec<egui::Pos2> = Vec::new();
            let mut prev_sx = f32::NAN;

            for pt in &track_pts {
                let lat_r = (pt.latitude as f32).to_radians();
                let lon_r = (pt.longitude as f32).to_radians();
                let wp = if label_config.is_map {
                    let u = (pt.longitude as f32 + 180.0) / 360.0;
                    let v = (90.0 - pt.latitude as f32) / 180.0;
                    glam::Vec3::new(
                        (u * 2.0 - 1.0) * QUAD_HALF_WIDTH,
                        (1.0 - v * 2.0) * QUAD_HALF_WIDTH * 0.5,
                        0.015,
                    )
                } else {
                    glam::Vec3::new(
                        -1.002 * lat_r.cos() * lon_r.cos(),
                         1.002 * lat_r.sin(),
                        -1.002 * lat_r.cos() * lon_r.sin(),
                    )
                };

                let occluded = if !label_config.is_map {
                    let n = wp.normalize();
                    let tc = (eye - wp).normalize();
                    n.dot(tc) > 0.05
                } else {
                    false
                };

                if occluded {
                    if cur_past.len() >= 2 {
                        past_segments.push(std::mem::take(&mut cur_past));
                    } else { cur_past.clear(); }
                    if cur_future.len() >= 2 {
                        future_segments.push(std::mem::take(&mut cur_future));
                    } else { cur_future.clear(); }
                    prev_sx = f32::NAN;
                    continue;
                }

                let c = vp * glam::Vec4::new(wp.x, wp.y, wp.z, 1.0);
                if c.w <= 0.0 { continue; }
                let nx = c.x / c.w;
                let ny = c.y / c.w;
                if nx.abs() > 1.2 || ny.abs() > 1.2 { continue; }

                let scr_x = (nx + 1.0) * 0.5 * sw;
                let scr_y = (1.0 - ny) * 0.5 * sh;

                if (scr_x - prev_sx).abs() > sw * 0.4 && !prev_sx.is_nan() {
                    if pt.minutes_offset <= 0.0 && cur_past.len() >= 2 {
                        past_segments.push(std::mem::take(&mut cur_past));
                    } else if pt.minutes_offset > 0.0 && cur_future.len() >= 2 {
                        future_segments.push(std::mem::take(&mut cur_future));
                    }
                }
                prev_sx = scr_x;

                let pos = egui::pos2(scr_x, scr_y);
                if pt.minutes_offset <= 0.0 {
                    cur_past.push(pos);
                } else {
                    cur_future.push(pos);
                }
            }
            if cur_past.len() >= 2 { past_segments.push(cur_past); }
            if cur_future.len() >= 2 { future_segments.push(cur_future); }

            self.gui_state.satellite_tracks.push(gui::SatelliteTrack {
                norad_id: sat.norad_id,
                past_segments, future_segments,
            });
        }

        // Project planet positions
        let (py, pm, pd, ph, pmin, ps) = {
            use chrono::{Datelike, Timelike};
            (sat_utc.year(), sat_utc.month(), sat_utc.day(),
             sat_utc.hour(), sat_utc.minute(), sat_utc.second())
        };
        let planet_states = planets::compute_planet_positions(py, pm, pd, ph, pmin, ps);
        self.gui_state.planet_markers.clear();
        for planet in &planet_states {
            let pos = planet.to_sky_position();
            let world = glam::Vec3::new(pos[0], pos[1], pos[2]);
            let clip = vp * glam::Vec4::new(world.x, world.y, world.z, 1.0);
            if clip.w <= 0.0 { continue; }
            let nx = clip.x / clip.w;
            let ny = clip.y / clip.w;
            if nx.abs() > 1.1 || ny.abs() > 1.1 { continue; }
            self.gui_state.planet_markers.push(gui::PlanetMarker {
                x: (nx + 1.0) * 0.5 * sw,
                y: (1.0 - ny) * 0.5 * sh,
                name: planet.name,
                color: planet.color,
                radius: planet.marker_radius(),
                visible: true,
            });
        }
    }
}
