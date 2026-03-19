// =============================================================================
// Orbis — Label System (M11e)
// =============================================================================
// Projects GeoJSON feature labels from geographic coordinates to screen
// space for rendering as egui text overlays.
//
// The projection matches the marker/line/polygon shaders:
// - Globe mode: Orbis convention (negated x/z)
// - Map mode: equirectangular on flat quad
// - Labels on the far side of the globe are hidden
//
// Features:
// - Collision avoidance: overlapping labels are nudged apart
// - Leader lines: displaced labels show a connecting line to the anchor
// =============================================================================

use glam::{Mat4, Vec3, Vec4};

use crate::geojson::{GeoCoord, GeoGeometry, GeoLayer};

/// A label ready to be rendered on screen.
pub struct ScreenLabel {
    /// Anchor X: original feature position (screen pixels, logical)
    pub anchor_x: f32,
    /// Anchor Y: original feature position (screen pixels, logical)
    pub anchor_y: f32,
    /// Label X: possibly displaced to avoid overlaps
    pub x: f32,
    /// Label Y: possibly displaced to avoid overlaps
    pub y: f32,
    /// Display text
    pub text: String,
    /// Label color (RGBA)
    pub color: [f32; 4],
    /// Estimated label width in logical pixels
    pub width: f32,
    /// Estimated label height in logical pixels
    pub height: f32,
    /// Whether this is a route label (LineString) — rendered without box
    pub is_route: bool,
    /// Priority for clustering (higher = more important, kept when grouped).
    /// Defaults to marker_size which naturally reflects significance.
    pub priority: f32,
    /// Texts of labels that were merged into this cluster representative.
    /// Empty if this label is not a cluster (or cluster of 1).
    pub clustered_texts: Vec<String>,
}

impl ScreenLabel {
    /// Whether this label was displaced from its anchor.
    pub fn is_displaced(&self) -> bool {
        let dx = (self.x - self.anchor_x).abs();
        let dy = (self.y - self.anchor_y).abs();
        dx > 2.0 || dy > 2.0
    }
}

/// Configuration for label rendering.
pub struct LabelConfig {
    /// View-projection matrix
    pub view_proj: Mat4,
    /// Camera eye position (for occlusion in globe mode)
    pub eye_pos: Vec3,
    /// Screen width in logical points (physical pixels / scale_factor)
    pub screen_width: f32,
    /// Screen height in logical points (physical pixels / scale_factor)
    pub screen_height: f32,
    /// true = Map2D, false = Globe3D
    pub is_map: bool,
    /// Map quad half-width (for 2D projection)
    pub quad_hw: f32,
}

/// Minimum label width in logical pixels.
const MIN_LABEL_WIDTH: f32 = 80.0;
/// Approximate character width for size estimation.
const CHAR_WIDTH: f32 = 7.0;
/// Label height (single line with padding).
const LABEL_HEIGHT: f32 = 20.0;
/// Padding between labels during collision avoidance.
const LABEL_PAD: f32 = 4.0;
/// Maximum displacement distance (labels beyond this are hidden).
const MAX_DISPLACEMENT: f32 = 120.0;
/// Offset from anchor point (so the label doesn't sit on the marker).
const ANCHOR_OFFSET_X: f32 = 10.0;
const ANCHOR_OFFSET_Y: f32 = -8.0;
/// Screen-space radius (logical px) for clustering nearby labels.
const CLUSTER_RADIUS: f32 = 30.0;

/// Generates screen-space labels for all visible features that have a label.
pub fn generate_labels(
    geo_layers: &[GeoLayer],
    config: &LabelConfig,
) -> Vec<ScreenLabel> {
    let mut labels = Vec::new();

    for layer in geo_layers.iter().filter(|l| l.visible) {
        for feature in &layer.features {
            // Only render features with a label
            let text = match &feature.style.label {
                Some(t) if !t.is_empty() => t.clone(),
                _ => continue,
            };

            // Get representative coordinate and geometry type
            let (coord, is_route) = match &feature.geometry {
                GeoGeometry::Point(c) => (*c, false),
                GeoGeometry::LineString(coords) => {
                    if coords.is_empty() { continue; }
                    (coords[coords.len() / 2], true)
                }
                GeoGeometry::Polygon(rings) => {
                    if rings.is_empty() || rings[0].is_empty() { continue; }
                    (centroid(&rings[0]), false)
                }
            };

            // Project to screen
            if let Some((sx, sy)) = project_to_screen(&coord, config) {
                let est_width = (text.len() as f32 * CHAR_WIDTH).max(MIN_LABEL_WIDTH);

                labels.push(ScreenLabel {
                    anchor_x: sx,
                    anchor_y: sy,
                    x: sx + ANCHOR_OFFSET_X,
                    y: sy + ANCHOR_OFFSET_Y,
                    text,
                    color: feature.style.color,
                    width: est_width,
                    height: LABEL_HEIGHT,
                    is_route,
                    priority: feature.style.marker_size,
                    clustered_texts: Vec::new(),
                });
            }
        }
    }

    // Cluster labels with nearby anchors (keeps highest priority, adds "+N")
    cluster_nearby(&mut labels);

    // Resolve remaining overlaps
    resolve_collisions(&mut labels, config.screen_width, config.screen_height);

    labels
}

/// Clusters labels whose anchor points are within CLUSTER_RADIUS.
///
/// Algorithm:
/// 1. Sort by priority (highest first — e.g. largest earthquake).
/// 2. Walk through; for each label, check if a kept label has a
///    nearby anchor. If so, increment that representative's count.
/// 3. Only the representative survives; its text gets " +N" appended.
///
/// This prevents the "vertical waterfall" problem when dozens of
/// labeled earthquakes share the same geographic region.
fn cluster_nearby(labels: &mut Vec<ScreenLabel>) {
    if labels.len() < 2 {
        return;
    }

    // Sort highest priority first
    labels.sort_by(|a, b| b.priority.partial_cmp(&a.priority).unwrap_or(std::cmp::Ordering::Equal));

    // Track which indices survive and which texts they absorb
    let mut kept = Vec::<usize>::new();
    let mut absorbed: Vec<Vec<String>> = vec![Vec::new(); labels.len()];
    let r2 = CLUSTER_RADIUS * CLUSTER_RADIUS;

    for i in 0..labels.len() {
        let ax = labels[i].anchor_x;
        let ay = labels[i].anchor_y;

        // Check if any already-kept label is close enough
        let mut merged = false;
        for &k in &kept {
            let dx = ax - labels[k].anchor_x;
            let dy = ay - labels[k].anchor_y;
            if dx * dx + dy * dy < r2 {
                absorbed[k].push(labels[i].text.clone());
                merged = true;
                break;
            }
        }

        if !merged {
            kept.push(i);
        }
    }

    // Build result: representative gets "+N" suffix and stores merged texts
    let placeholder = || ScreenLabel {
        anchor_x: 0.0, anchor_y: 0.0, x: 0.0, y: 0.0,
        text: String::new(), color: [0.0; 4],
        width: 0.0, height: 0.0, is_route: false, priority: 0.0,
        clustered_texts: Vec::new(),
    };

    let mut result = Vec::with_capacity(kept.len());
    for k in kept {
        let mut label = std::mem::replace(&mut labels[k], placeholder());
        let merged = std::mem::take(&mut absorbed[k]);
        if !merged.is_empty() {
            // Keep text as-is (no "+N" suffix); gui.rs appends it when collapsed.
            // Estimate width for the collapsed display (with suffix).
            let display_len = label.text.len() + 3 + merged.len().to_string().len();
            label.width = (display_len as f32 * CHAR_WIDTH).max(MIN_LABEL_WIDTH);
            label.clustered_texts = merged;
        }
        result.push(label);
    }

    *labels = result;
}

/// Simple greedy collision avoidance.
///
/// Sort labels by Y position, then push overlapping labels downward.
/// Labels that get displaced too far are kept but capped at MAX_DISPLACEMENT.
fn resolve_collisions(labels: &mut [ScreenLabel], screen_w: f32, screen_h: f32) {
    if labels.len() < 2 {
        return;
    }

    // Sort by Y position (top to bottom)
    labels.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

    // Multi-pass: push overlapping labels apart
    for _pass in 0..5 {
        let mut any_moved = false;

        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                if !overlaps(&labels[i], &labels[j]) {
                    continue;
                }

                // Push j downward
                let overlap_y = (labels[i].y + labels[i].height + LABEL_PAD) - labels[j].y;
                if overlap_y > 0.0 {
                    labels[j].y += overlap_y;
                    any_moved = true;
                }
            }
        }

        if !any_moved {
            break;
        }
    }

    // Clamp labels to screen bounds
    for label in labels.iter_mut() {
        label.x = label.x.clamp(0.0, screen_w - label.width);
        label.y = label.y.clamp(0.0, screen_h - label.height);

        // If displaced too far, cap displacement
        let dx = label.x - label.anchor_x;
        let dy = label.y - label.anchor_y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist > MAX_DISPLACEMENT {
            let scale = MAX_DISPLACEMENT / dist;
            label.x = label.anchor_x + dx * scale;
            label.y = label.anchor_y + dy * scale;
        }
    }
}

/// Checks if two labels overlap (axis-aligned bounding box test).
fn overlaps(a: &ScreenLabel, b: &ScreenLabel) -> bool {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;

    a.x < bx2 && ax2 > b.x && a.y < by2 && ay2 > b.y
}

/// Projects a geographic coordinate to screen pixels.
///
/// Returns None if the point is behind the camera or on the far side
/// of the globe.
fn project_to_screen(coord: &GeoCoord, config: &LabelConfig) -> Option<(f32, f32)> {
    let lon_rad = (coord.lon as f32).to_radians();
    let lat_rad = (coord.lat as f32).to_radians();

    let world_pos = if config.is_map {
        // Map2D: equirectangular on flat quad
        let u = (coord.lon as f32 + 180.0) / 360.0;
        let v = (90.0 - coord.lat as f32) / 180.0;
        let x = (u * 2.0 - 1.0) * config.quad_hw;
        let y = (1.0 - v * 2.0) * config.quad_hw * 0.5;
        Vec3::new(x, y, 0.01)
    } else {
        // Globe3D: Orbis convention (negated x/z)
        let r = 1.002_f32;
        Vec3::new(
            -r * lat_rad.cos() * lon_rad.cos(),
             r * lat_rad.sin(),
            -r * lat_rad.cos() * lon_rad.sin(),
        )
    };

    // Globe occlusion check.
    // In Orbis coords (negated x/z), visible points have dot < 0,
    // horizon ≈ 0, far side > 0. We use a tighter threshold than
    // markers (-0.15 vs 0.05) because labels near the horizon
    // project to extreme screen positions and look bad.
    if !config.is_map {
        let normal = world_pos.normalize();
        let to_cam = (config.eye_pos - world_pos).normalize();
        if normal.dot(to_cam) > -0.15 {
            return None;
        }
    }

    // Project to clip space
    let clip = config.view_proj * Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);

    // Behind camera?
    if clip.w <= 0.0 {
        return None;
    }

    // NDC (-1..1)
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;

    // Outside screen? (tight bound — labels shouldn't clip the edge)
    if ndc_x.abs() > 0.95 || ndc_y.abs() > 0.95 {
        return None;
    }

    // Screen pixels (0,0 = top-left)
    let sx = (ndc_x + 1.0) * 0.5 * config.screen_width;
    let sy = (1.0 - ndc_y) * 0.5 * config.screen_height;

    Some((sx, sy))
}

/// Computes the centroid (simple average) of a ring of coordinates.
fn centroid(ring: &[GeoCoord]) -> GeoCoord {
    if ring.is_empty() {
        return GeoCoord::new(0.0, 0.0);
    }
    let mut lon_sum = 0.0;
    let mut lat_sum = 0.0;
    let n = ring.len() as f64;
    for c in ring {
        lon_sum += c.lon;
        lat_sum += c.lat;
    }
    GeoCoord::new(lon_sum / n, lat_sum / n)
}
