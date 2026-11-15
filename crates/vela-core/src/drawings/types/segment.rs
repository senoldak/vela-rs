//! Shared behavior of multi-line drawings (channels, pitchforks, closed
//! shapes): the original's `SegmentDrawing` base class. The line math lives
//! in one place, a type's `geometry(proj)` (pixel segments + optional fill),
//! and hit-testing, handle positions and bounds all derive from it plus the
//! anchors.

use crate::drawings::drawing::{Bounds, DrawingBase};
use crate::drawings::geometry::{Projector, SegmentGeometry};
use crate::drawings::hittest::{dist_to_segment, point_in_polygon};

/// Body hit-test: inside the fill polygon (when the drawing has a fill
/// color), or within `tol` px of any segment.
pub fn segment_hit_test(
    base: &DrawingBase,
    geometry: Option<&SegmentGeometry>,
    px: f64,
    py: f64,
    tol: f64,
) -> bool {
    let Some(g) = geometry else { return false };
    if base
        .style
        .fill_color
        .as_deref()
        .is_some_and(|c| !c.is_empty())
        && g.fill
            .as_ref()
            .is_some_and(|fill| point_in_polygon(px, py, fill))
    {
        return true;
    }
    g.segments
        .iter()
        .any(|s| dist_to_segment(px, py, s[0], s[1], s[2], s[3]) <= tol)
}

/// One handle per anchor, at its pixel position; none when the pane is gone.
pub fn segment_handle_points(base: &DrawingBase, proj: &dyn Projector) -> Vec<(f64, f64)> {
    base.anchors_px(proj).unwrap_or_default()
}

/// The tight bounds of the anchor handles.
pub fn segment_bounds(base: &DrawingBase, proj: &dyn Projector) -> Option<Bounds> {
    Bounds::of_points(&segment_handle_points(base, proj))
}
