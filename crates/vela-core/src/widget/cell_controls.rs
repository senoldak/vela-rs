//! Per-cell view controls: a hover cluster pinned to the bottom-center of a workspace
//! cell's PRICE PLOT (the candles, not the price-scale gutter): a drag handle to move the
//! chart within the grid, zoom out / zoom in, maximize-or-restore, and reset view. Revealed
//! by cursor proximity, the same affordance as the renderer's scroll-to-realtime button.
//!
//! This is the pure half of the original `CellControls`: the proximity maths, the button
//! set and its gating ([`cell_control_buttons`]), the reveal state ([`ClusterReveal`]) and
//! the grip drag ([`GripDrag`]). Left for the browser crate: the cluster element and its
//! CSS (scrim pill, `left: CLUSTER_LEFT_CSS`), the host `pointermove`/`pointerleave`
//! listeners, pointer capture, reading the renderer gutters from the host's computed style,
//! and driving a [`Glider`](super::glide::Glider) per cell for the zoom buttons (which stops
//! it before the reset action).

use crate::i18n::{MessageLike, msg};

/// Cursor distance (px, from the cluster center) that reveals the cluster: mirrors the
/// scroll-to-realtime button's proximity radius.
pub const CELL_CONTROLS_PROXIMITY_PX: f64 = 120.0;

/// px the renderer reserves for a time axis (mirrors the native renderer's `TIME_AXIS_H`).
const TIME_AXIS_H: f64 = 22.0;
/// Cluster bottom inset: above the time axis, level with the scroll button.
pub const CONTROLS_BOTTOM_PX: f64 = TIME_AXIS_H + 12.0;
/// Cluster height incl. padding (20px buttons + 2px padding each side).
const CLUSTER_H: f64 = 24.0;

/// The cluster's scrim pill: floats over chart content, so a neutral darkening rather than
/// a themed surface (same value as the pane clusters).
pub const CLUSTER_PILL: &str = "rgba(0,0,0,0.65)";

/// CSS `left` that centers the cluster on the plot (candles), not the full cell.
/// `--vela-toolbar-gutter` / `--vela-scale-gutter` are published on the mount container
/// (the cell host) by the renderer.
pub const CLUSTER_LEFT_CSS: &str =
    "calc((100% + var(--vela-toolbar-gutter, 0px) - var(--vela-scale-gutter, 0px)) / 2)";

/// Horizontal center of the plot (candles) in cell-local x: the scale gutter is the price
/// axis, the toolbar gutter is the drawings dock.
pub fn plot_center_x(width: f64, toolbar_gutter: f64, scale_gutter: f64) -> f64 {
    (width + toolbar_gutter - scale_gutter) / 2.0
}

/// The renderer gutters published on the cell host (0 when unpublished).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gutters {
    pub toolbar: f64,
    pub scale: f64,
}

/// Is the pointer near the cluster's resting spot (bottom-center of the plot)? `x`/`y` are
/// cell-local coordinates, `width`/`height` the cell's size. Pass the renderer gutters so
/// the hotspot tracks the visual center when a price axis (or drawings toolbar) is reserved.
pub fn near_bottom_center(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    proximity_px: f64,
    gutters: Gutters,
) -> bool {
    let cx = plot_center_x(width, gutters.toolbar, gutters.scale);
    let cy = height - CONTROLS_BOTTOM_PX - CLUSTER_H / 2.0;
    (x - cx).hypot(y - cy) <= proximity_px
}

/// A cluster button's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellControlAction {
    /// The drag handle (2x3 dot grip).
    Grip,
    ZoomOut,
    ZoomIn,
    ToggleMaximize,
    Reset,
}

/// One button of the cluster.
#[derive(Clone, Debug, PartialEq)]
pub struct CellControlButton {
    pub action: CellControlAction,
    pub icon: &'static str,
    pub label: MessageLike,
    /// The inverse "selected" chip: the maximized state.
    pub selected: bool,
}

/// The buttons of the cluster, left to right. The drag handle only exists when there is
/// somewhere to move to: never on a single-cell grid, and not while this chart covers the
/// grid. Maximize is gated the same way (a lone chart has nothing to trade space with).
pub fn cell_control_buttons(multi_cell: bool, is_maximized: bool) -> Vec<CellControlButton> {
    let maximized = multi_cell && is_maximized;
    let mut out = Vec::new();
    if multi_cell && !maximized {
        out.push(CellControlButton {
            action: CellControlAction::Grip,
            icon: "grip",
            label: msg("widget.cellControls.dragToMove", "Drag to move chart"),
            selected: false,
        });
    }
    out.push(CellControlButton {
        action: CellControlAction::ZoomOut,
        icon: "minus",
        label: msg("widget.cellControls.zoomOut", "Zoom out"),
        selected: false,
    });
    out.push(CellControlButton {
        action: CellControlAction::ZoomIn,
        icon: "plus",
        label: msg("widget.cellControls.zoomIn", "Zoom in"),
        selected: false,
    });
    if multi_cell {
        out.push(CellControlButton {
            action: CellControlAction::ToggleMaximize,
            icon: if maximized { "restore" } else { "maximize" },
            label: if maximized {
                msg("widget.cellControls.restoreLayout", "Restore layout")
            } else {
                msg("widget.cellControls.maximizeChart", "Maximize chart")
            },
            // The maximized state reads as an inverse chip (white-on-dark, dark-on-light),
            // the same active-state affordance as a collapsed pane's expand button.
            selected: maximized,
        });
    }
    out.push(CellControlButton {
        action: CellControlAction::Reset,
        icon: "reset",
        label: msg("widget.cellControls.resetChart", "Reset chart"),
        selected: false,
    });
    out
}

/// Whether the cluster is revealed. The reveal follows cursor proximity, except that a
/// grip drag must not hide it while captured pointer moves sweep across the whole grid, and
/// mobile suspends it entirely (the proximity reveal is meaningless without a cursor: the
/// mobile bar's maximize stop replaces the cluster).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClusterReveal {
    near: bool,
    dragging: bool,
    suspended: bool,
}

impl ClusterReveal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the cluster shows now (`display: flex`).
    pub fn visible(&self) -> bool {
        self.near
    }

    /// Mobile flips the cluster off entirely (and hides it if currently revealed). Returns
    /// whether the visibility changed.
    pub fn set_suspended(&mut self, on: bool) -> bool {
        self.suspended = on;
        if on { self.set_near(false) } else { false }
    }

    /// A pointer move over the host, in cell-local coordinates. Returns whether the
    /// visibility changed.
    pub fn on_host_move(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        gutters: Gutters,
    ) -> bool {
        if self.suspended || self.dragging {
            return false;
        }
        let near = near_bottom_center(x, y, width, height, CELL_CONTROLS_PROXIMITY_PX, gutters);
        self.set_near(near)
    }

    /// The pointer left the host. Returns whether the visibility changed.
    pub fn on_host_leave(&mut self) -> bool {
        if self.dragging {
            return false;
        }
        self.set_near(false)
    }

    pub fn set_dragging(&mut self, dragging: bool) {
        self.dragging = dragging;
    }

    fn set_near(&mut self, near: bool) -> bool {
        if near == self.near {
            return false;
        }
        self.near = near;
        true
    }
}

/// What a grip drag asks of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GripEffect {
    /// Highlight the would-be drop cell (`None` clears the highlight).
    Preview(Option<String>),
    /// Commit: this cell and the target trade slots.
    Drop(String),
}

/// A grip drag. Press and drag onto another cell to trade slots with it: the preview
/// highlight follows the pointer; releasing outside any other cell, or cancelling, commits
/// nothing. The caller hit-tests the pointer against the OTHER live cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GripDrag {
    target: Option<String>,
}

impl GripDrag {
    /// A drag starts (the caller marks the reveal dragging).
    pub fn start() -> Self {
        Self::default()
    }

    /// The pointer moved over `target` (`None` over this cell, the chrome, or outside the
    /// grid).
    pub fn on_move(&mut self, target: Option<String>) -> GripEffect {
        self.target = target.clone();
        GripEffect::Preview(target)
    }

    /// The drag ended: the highlight clears first, then a release over a cell commits.
    pub fn finish(self, commit: bool) -> Vec<GripEffect> {
        let mut out = vec![GripEffect::Preview(None)];
        if let (true, Some(target)) = (commit, self.target) {
            out.push(GripEffect::Drop(target));
        }
        out
    }
}

/// Only a primary-button press (or any non-mouse pointer) starts a grip drag.
pub fn grip_press_starts_drag(button: i32, pointer_is_mouse: bool) -> bool {
    !(button != 0 && pointer_is_mouse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::text_of;

    const NO_GUTTERS: Gutters = Gutters {
        toolbar: 0.0,
        scale: 0.0,
    };

    fn near(x: f64, y: f64, w: f64, h: f64) -> bool {
        near_bottom_center(x, y, w, h, CELL_CONTROLS_PROXIMITY_PX, NO_GUTTERS)
    }

    #[test]
    fn near_is_true_at_the_cluster_spot_and_within_the_proximity_radius() {
        // 800x600 cell, no gutters: the cluster centers at (400, 600 - 34 - 12) = (400, 554).
        assert!(near(400.0, 554.0, 800.0, 600.0));
        assert!(near(
            400.0 + CELL_CONTROLS_PROXIMITY_PX,
            554.0,
            800.0,
            600.0
        )); // radius inclusive
        assert!(near(
            400.0,
            554.0 - CELL_CONTROLS_PROXIMITY_PX,
            800.0,
            600.0
        ));
    }

    #[test]
    fn near_is_false_away_from_the_bottom_center() {
        assert!(!near(400.0, 300.0, 800.0, 600.0)); // middle of the plot
        assert!(!near(20.0, 580.0, 800.0, 600.0)); // bottom-left corner
        assert!(!near(780.0, 580.0, 800.0, 600.0)); // bottom-right (the scroll button's home)
        assert!(!near(400.0, 20.0, 800.0, 600.0)); // top-center
    }

    #[test]
    fn near_scales_with_the_cell_size() {
        assert!(near(200.0, 254.0, 400.0, 300.0)); // half-size cell, its own bottom-center
        assert!(!near(400.0, 554.0, 400.0, 300.0)); // the big cell's spot is outside a small cell
    }

    #[test]
    fn near_centers_on_the_plot_when_a_scale_gutter_is_reserved() {
        // 800px cell, 64px price axis: plot center at (800 - 64) / 2 = 368.
        let g = Gutters {
            toolbar: 0.0,
            scale: 64.0,
        };
        let cx = plot_center_x(800.0, 0.0, 64.0);
        assert_eq!(cx, 368.0);
        assert!(near_bottom_center(
            cx,
            554.0,
            800.0,
            600.0,
            CELL_CONTROLS_PROXIMITY_PX,
            g
        ));
        assert!(near_bottom_center(
            cx + CELL_CONTROLS_PROXIMITY_PX,
            554.0,
            800.0,
            600.0,
            CELL_CONTROLS_PROXIMITY_PX,
            g
        ));
        // The old full-cell right edge (400 + 120 = 520) is now outside the radius.
        assert!(!near_bottom_center(
            520.0,
            554.0,
            800.0,
            600.0,
            CELL_CONTROLS_PROXIMITY_PX,
            g
        ));
        assert!(near(520.0, 554.0, 800.0, 600.0)); // without a gutter, the old spot still holds
    }

    #[test]
    fn the_toolbar_gutter_shifts_the_center_right_by_half() {
        assert_eq!(plot_center_x(800.0, 44.0, 64.0), 390.0); // (800 + 44 - 64) / 2
    }

    #[test]
    fn the_cluster_pins_to_the_plot_center() {
        assert!(CLUSTER_LEFT_CSS.contains("--vela-scale-gutter"));
        assert_eq!(CONTROLS_BOTTOM_PX, 34.0);
    }

    fn titles(multi: bool, maximized: bool) -> Vec<String> {
        cell_control_buttons(multi, maximized)
            .iter()
            .map(|b| text_of(None, &b.label, &[]))
            .collect()
    }

    #[test]
    fn builds_drag_zoom_out_zoom_in_maximize_reset_on_a_multi_cell_grid() {
        assert_eq!(
            titles(true, false),
            [
                "Drag to move chart",
                "Zoom out",
                "Zoom in",
                "Maximize chart",
                "Reset chart"
            ]
        );
    }

    #[test]
    fn drops_the_drag_handle_and_maximize_on_single_cell_grids() {
        assert_eq!(titles(false, false), ["Zoom out", "Zoom in", "Reset chart"]);
        // A lone chart is never "maximized", whatever the flag says.
        assert_eq!(titles(false, true), ["Zoom out", "Zoom in", "Reset chart"]);
    }

    #[test]
    fn the_maximized_cell_flips_maximize_to_restore_and_hides_its_drag_handle() {
        let resting = cell_control_buttons(true, false);
        assert!(!resting[3].selected);
        assert_eq!(resting[3].icon, "maximize");
        let maximized = cell_control_buttons(true, true);
        assert_eq!(
            titles(true, true),
            ["Zoom out", "Zoom in", "Restore layout", "Reset chart"]
        );
        // The maximized state reads as the inverse "selected" chip.
        assert!(maximized[2].selected);
        assert_eq!(maximized[2].icon, "restore");
        assert_eq!(maximized[2].action, CellControlAction::ToggleMaximize);
    }

    #[test]
    fn the_reveal_starts_hidden_shows_near_the_bottom_center_and_hides_on_leave() {
        let mut r = ClusterReveal::new();
        assert!(!r.visible());
        assert!(r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS));
        assert!(r.visible());
        assert!(
            !r.on_host_move(401.0, 554.0, 800.0, 600.0, NO_GUTTERS),
            "no change, no notification"
        );
        assert!(r.on_host_move(400.0, 100.0, 800.0, 600.0, NO_GUTTERS));
        assert!(!r.visible());
        r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS);
        assert!(r.on_host_leave());
        assert!(!r.visible());
    }

    #[test]
    fn suspending_hides_the_cluster_and_blocks_reveals() {
        let mut r = ClusterReveal::new();
        r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS);
        assert!(r.visible());
        assert!(r.set_suspended(true)); // a mode flip hides a revealed cluster immediately
        assert!(!r.visible());
        assert!(!r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS));
        assert!(!r.visible(), "no reveal while suspended");
        assert!(!r.set_suspended(false));
        assert!(r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS));
        assert!(r.visible(), "back to desktop behavior");
    }

    #[test]
    fn a_drag_keeps_the_cluster_up_while_pointer_moves_sweep_the_grid() {
        let mut r = ClusterReveal::new();
        r.on_host_move(400.0, 554.0, 800.0, 600.0, NO_GUTTERS);
        r.set_dragging(true);
        assert!(!r.on_host_move(10.0, 10.0, 800.0, 600.0, NO_GUTTERS));
        assert!(!r.on_host_leave());
        assert!(r.visible());
        r.set_dragging(false);
        assert!(r.on_host_leave());
    }

    #[test]
    fn the_grip_previews_the_hovered_cell_and_commits_the_drop_on_release() {
        let mut drag = GripDrag::start();
        assert_eq!(
            drag.on_move(Some("eth".into())),
            GripEffect::Preview(Some("eth".into()))
        );
        assert_eq!(
            drag.finish(true),
            [GripEffect::Preview(None), GripEffect::Drop("eth".into())]
        );
    }

    #[test]
    fn releasing_over_nothing_or_a_cancel_commits_no_move() {
        let mut drag = GripDrag::start();
        drag.on_move(None);
        assert_eq!(drag.finish(true), [GripEffect::Preview(None)]);
        let mut cancelled = GripDrag::start();
        cancelled.on_move(Some("eth".into()));
        assert_eq!(cancelled.finish(false), [GripEffect::Preview(None)]);
        // The drop follows the LAST move: leaving every cell before releasing commits nothing.
        let mut left = GripDrag::start();
        left.on_move(Some("eth".into()));
        left.on_move(None);
        assert_eq!(left.finish(true), [GripEffect::Preview(None)]);
    }

    #[test]
    fn only_a_primary_press_starts_a_drag() {
        assert!(grip_press_starts_drag(0, true));
        assert!(!grip_press_starts_drag(2, true));
        assert!(grip_press_starts_drag(2, false)); // touch and pen report button 0 or -1
    }
}
