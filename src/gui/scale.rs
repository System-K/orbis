// =============================================================================
// Scale HUD — tile zoom level + map scale bar (Phase 2 indicator overlay)
// =============================================================================
//
// Renders a small translucent card anchored to the lower-left of the
// available viewport rect (i.e. excluding the left side panel when open).
// Shows:
//
//   Z = 7
//   |-----------------| 500 km
//
// The tile zoom level comes from `gui_state.tile_metrics.current_zoom`.
// The scale bar comes from `gui_state.scale_meters_per_pixel`, which
// main.rs computes per frame based on the active view mode.
//
// Purpose: diagnostic during the perf-fix work (so we can correlate fps
// dips with tile level and ground extent), and a generally-useful map UI
// indicator for end users.

use super::GuiState;

/// Target length for the scale bar in screen pixels. Nice round distances
/// get picked to land ≤ this length — see `pick_scale_bar`.
const TARGET_BAR_PX: f32 = 120.0;

/// Draws the scale + tile-zoom HUD anchored at the bottom-left of the
/// available rect (the viewport region left over after egui panels are
/// placed). Call after all panels have been shown in the current frame.
pub fn draw_scale_hud(ctx: &egui::Context, gui_state: &GuiState) {
    let mpp = gui_state.scale_meters_per_pixel;
    if mpp <= 0.0 || !mpp.is_finite() {
        return; // main.rs hasn't computed scale yet this frame
    }

    let (bar_px, label) = pick_scale_bar(mpp, TARGET_BAR_PX);
    let z = gui_state.tile_metrics.current_zoom;

    let rect = ctx.available_rect();
    let anchor = egui::pos2(rect.left() + 8.0, rect.bottom() - 8.0);
    let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170);
    let fg = egui::Color32::WHITE;

    egui::Area::new(egui::Id::new("orbis_scale_hud"))
        .fixed_pos(anchor)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 6))
                .corner_radius(4.0)
                .fill(bg)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Z = {}", z))
                                .color(fg)
                                .monospace()
                                .size(12.0),
                        );
                        draw_bar(ui, bar_px, &label, fg);
                    });
                });
        });
}

/// Renders the horizontal scale bar with end-caps and a label to the right.
fn draw_bar(ui: &mut egui::Ui, bar_px: f32, label: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        let thickness = 4.0;
        let cap_h = 10.0;
        let (rect, _resp) = ui.allocate_exact_size(
            egui::vec2(bar_px.max(1.0), cap_h),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        let center_y = rect.center().y;
        // Main horizontal line
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left(), center_y - thickness / 2.0),
            egui::pos2(rect.right(), center_y + thickness / 2.0),
        );
        painter.rect_filled(bar_rect, 0.0, color);
        // End caps (small vertical ticks)
        let cap_w = 2.0;
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), center_y - cap_h / 2.0),
                egui::vec2(cap_w, cap_h),
            ),
            0.0,
            color,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.right() - cap_w, center_y - cap_h / 2.0),
                egui::vec2(cap_w, cap_h),
            ),
            0.0,
            color,
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(label)
                .color(color)
                .monospace()
                .size(11.0),
        );
    });
}

/// Picks a "nice" round distance (1/2/5 × 10^n) that maps to ≤ `target_px`
/// at the given meters-per-pixel. Returns the resulting bar length in
/// pixels and a formatted label ("500 m", "50 km", ...).
pub(crate) fn pick_scale_bar(mpp: f32, target_px: f32) -> (f32, String) {
    if mpp <= 0.0 || !mpp.is_finite() || target_px <= 0.0 {
        return (1.0, "—".to_string());
    }
    let target_meters = mpp * target_px;
    let exp = target_meters.log10().floor();
    let base = 10.0_f32.powf(exp);
    let normalized = target_meters / base;
    // Pick the largest "nice" multiplier that still fits under target.
    let nice = if normalized >= 5.0 {
        5.0
    } else if normalized >= 2.0 {
        2.0
    } else {
        1.0
    };
    let distance_m = nice * base;
    let bar_px = distance_m / mpp;
    let label = format_distance(distance_m);
    (bar_px, label)
}

/// Formats a distance in meters using km for ≥ 1 km.
fn format_distance(distance_m: f32) -> String {
    if distance_m >= 1000.0 {
        let km = distance_m / 1000.0;
        if km >= 100.0 {
            format!("{:.0} km", km)
        } else if km.fract() < 0.05 {
            format!("{:.0} km", km)
        } else {
            format!("{:.1} km", km)
        }
    } else {
        format!("{:.0} m", distance_m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_follows_1_2_5_pattern() {
        // target_meters = 1200 → pick 1000 m (1 km), 100 px at mpp=10
        let (px, label) = pick_scale_bar(10.0, 120.0);
        assert!((px - 100.0).abs() < 1e-3, "got {}", px);
        assert_eq!(label, "1 km");

        // target_meters = 300 → pick 200 m, 200 px at mpp=1
        let (px, label) = pick_scale_bar(1.0, 300.0);
        assert!((px - 200.0).abs() < 1e-3, "got {}", px);
        assert_eq!(label, "200 m");

        // target_meters = 60 → pick 50 m, 100 px at mpp=0.5
        let (px, label) = pick_scale_bar(0.5, 120.0);
        assert!((px - 100.0).abs() < 1e-3, "got {}", px);
        assert_eq!(label, "50 m");

        // target_meters = 600_000 → pick 500 km, 100 px at mpp=5000
        let (px, label) = pick_scale_bar(5_000.0, 120.0);
        assert!((px - 100.0).abs() < 1e-3, "got {}", px);
        assert_eq!(label, "500 km");
    }

    #[test]
    fn pick_handles_zero_and_nan() {
        let (_, label) = pick_scale_bar(0.0, 120.0);
        assert_eq!(label, "—");
        let (_, label) = pick_scale_bar(f32::NAN, 120.0);
        assert_eq!(label, "—");
        let (_, label) = pick_scale_bar(1.0, 0.0);
        assert_eq!(label, "—");
    }

    #[test]
    fn pick_bar_length_never_exceeds_target() {
        // Walk a range of mpp values and confirm the nice-number bar
        // is always ≤ the target length (that's the whole contract).
        for i in 0..200 {
            let mpp = 10.0_f32.powf(-3.0 + i as f32 * 0.05);
            let (px, _) = pick_scale_bar(mpp, 120.0);
            assert!(px <= 120.0 + 1e-3, "mpp={} px={}", mpp, px);
            assert!(px > 0.0);
        }
    }
}
