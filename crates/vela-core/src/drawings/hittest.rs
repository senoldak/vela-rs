//! Pure hit-test geometry for user drawings. All inputs/outputs are media
//! pixels; no DOM, no canvas.

use crate::model::DrawingExtend;

/// Distance from point (px,py) to the segment (ax,ay)→(bx,by).
pub fn dist_to_segment(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 <= 1e-9 {
        return (px - ax).hypot(py - ay); // degenerate segment → point
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

/// Distance from a point to a polyline (min over its segments); ∞ for <2 points.
pub fn dist_to_polyline(px: f64, py: f64, pts: &[(f64, f64)]) -> f64 {
    let mut best = f64::INFINITY;
    for pair in pts.windows(2) {
        let d = dist_to_segment(px, py, pair[0].0, pair[0].1, pair[1].0, pair[1].1);
        if d < best {
            best = d;
        }
    }
    best
}

/// Whether (px,py) lies inside the axis-aligned box, expanded by `pad` px.
pub fn point_in_box(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64, pad: f64) -> bool {
    let lo = x1.min(x2) - pad;
    let hi = x1.max(x2) + pad;
    let top = y1.min(y2) - pad;
    let bot = y1.max(y2) + pad;
    px >= lo && px <= hi && py >= top && py <= bot
}

/// Whether (px,py) is within `r` px of the circle centered at (cx,cy).
pub fn point_in_circle(px: f64, py: f64, cx: f64, cy: f64, r: f64) -> bool {
    (px - cx).hypot(py - cy) <= r
}

/// Ray-casting point-in-polygon. <3 points ⇒ false.
pub fn point_in_polygon(px: f64, py: f64, poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    if poly.is_empty() {
        return false;
    }
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[j];
        let intersect =
            (a.1 > py) != (b.1 > py) && px < ((b.0 - a.0) * (py - a.1)) / (b.1 - a.1) + a.0;
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Where the infinite line through (lx1,ly1)→(lx2,ly2) crosses the segment
/// (ax,ay)→(bx,by), or `None` if they're parallel or the crossing falls
/// outside the segment.
#[allow(clippy::too_many_arguments)]
pub fn line_segment_intersection(
    lx1: f64,
    ly1: f64,
    lx2: f64,
    ly2: f64,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
) -> Option<(f64, f64)> {
    let dx = lx2 - lx1;
    let dy = ly2 - ly1;
    let ex = bx - ax;
    let ey = by - ay;
    let denom = ex * dy - ey * dx;
    if denom.abs() < 1e-9 {
        return None; // parallel
    }
    let s = ((ay - ly1) * dx - (ax - lx1) * dy) / denom; // segment param at the crossing
    if !(0.0..=1.0).contains(&s) {
        return None; // crossing is beyond the segment's endpoints
    }
    Some((ax + s * ex, ay + s * ey))
}

/// Default handle pick radius, in px.
pub const HANDLE_RADIUS: f64 = 6.0;

/// Index of the handle within `rad` px of (px,py). Handles are tested in
/// order; the first match wins (callers pass them in priority order).
pub fn handle_at(px: f64, py: f64, handles: &[(f64, f64)], rad: f64) -> Option<usize> {
    handles
        .iter()
        .position(|h| (px - h.0).hypot(py - h.1) <= rad)
}

/// Extend a segment p1→p2 to the chart edges. Returns the endpoints to stroke
/// as `[x1, y1, x2, y2]` (left→right not guaranteed; arrowheads use the originals).
pub fn extend_ray(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    extend: DrawingExtend,
    width: f64,
    height: f64,
) -> [f64; 4] {
    if extend == DrawingExtend::None {
        return [x1, y1, x2, y2];
    }
    let dx = x2 - x1;
    let left = matches!(extend, DrawingExtend::Left | DrawingExtend::Both);
    let right = matches!(extend, DrawingExtend::Right | DrawingExtend::Both);
    if dx.abs() < 1e-6 {
        // Vertical: extend runs along the line itself.
        if left && right {
            return [x1, 0.0, x2, height];
        }
        let downward = y2 >= y1;
        if right {
            return if downward {
                [x1, y1, x2, height]
            } else {
                [x1, y1, x2, 0.0]
            };
        }
        return if downward {
            [x1, 0.0, x2, y2]
        } else {
            [x1, height, x2, y2]
        };
    }
    let slope = (y2 - y1) / dx;
    let y_at = |x: f64| y1 + slope * (x - x1);
    let lx = if left { -2.0 } else { x1.min(x2) };
    let rx = if right { width + 2.0 } else { x1.max(x2) };
    [lx, y_at(lx), rx, y_at(rx)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_to_a_segment_clamps_to_its_ends() {
        assert_eq!(dist_to_segment(5.0, 3.0, 0.0, 0.0, 10.0, 0.0), 3.0);
        assert_eq!(dist_to_segment(-3.0, 4.0, 0.0, 0.0, 10.0, 0.0), 5.0);
        assert_eq!(dist_to_segment(3.0, 4.0, 0.0, 0.0, 0.0, 0.0), 5.0);
    }

    #[test]
    fn polyline_distance_is_infinite_without_a_segment() {
        assert_eq!(dist_to_polyline(0.0, 0.0, &[(1.0, 1.0)]), f64::INFINITY);
        assert_eq!(
            dist_to_polyline(5.0, 2.0, &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]),
            2.0
        );
    }

    #[test]
    fn box_circle_and_polygon_containment() {
        assert!(point_in_box(5.0, 5.0, 10.0, 10.0, 0.0, 0.0, 0.0));
        assert!(!point_in_box(11.0, 5.0, 0.0, 0.0, 10.0, 10.0, 0.0));
        assert!(point_in_box(11.0, 5.0, 0.0, 0.0, 10.0, 10.0, 2.0));
        assert!(point_in_circle(3.0, 4.0, 0.0, 0.0, 5.0));
        assert!(!point_in_circle(3.0, 4.1, 0.0, 0.0, 5.0));
        let square = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(point_in_polygon(5.0, 5.0, &square));
        assert!(!point_in_polygon(15.0, 5.0, &square));
        assert!(!point_in_polygon(0.0, 0.0, &square[..2]));
    }

    #[test]
    fn line_meets_a_segment_only_within_it() {
        assert_eq!(
            line_segment_intersection(0.0, 0.0, 1.0, 1.0, 0.0, 10.0, 10.0, 0.0),
            Some((5.0, 5.0))
        );
        assert_eq!(
            line_segment_intersection(0.0, 0.0, 1.0, 0.0, 0.0, 5.0, 10.0, 5.0),
            None
        );
        assert_eq!(
            line_segment_intersection(0.0, 0.0, 1.0, 1.0, 20.0, 0.0, 30.0, 0.0),
            None
        );
    }

    #[test]
    fn first_handle_in_range_wins() {
        let handles = [(0.0, 0.0), (3.0, 0.0), (50.0, 50.0)];
        assert_eq!(handle_at(2.0, 0.0, &handles, HANDLE_RADIUS), Some(0));
        assert_eq!(handle_at(48.0, 50.0, &handles, HANDLE_RADIUS), Some(2));
        assert_eq!(handle_at(100.0, 100.0, &handles, HANDLE_RADIUS), None);
    }

    // Mirrors the original `extendEndpoints` cases (the renderer copy this function shadows).
    #[test]
    fn extend_ray_runs_to_the_chart_edges() {
        use DrawingExtend::*;
        assert_eq!(
            extend_ray(10.0, 50.0, 20.0, 50.0, None, 100.0, 200.0),
            [10.0, 50.0, 20.0, 50.0]
        );
        assert_eq!(
            extend_ray(10.0, 50.0, 20.0, 50.0, Right, 100.0, 200.0),
            [10.0, 50.0, 102.0, 50.0]
        );
        assert_eq!(
            extend_ray(10.0, 50.0, 20.0, 50.0, Left, 100.0, 200.0),
            [-2.0, 50.0, 20.0, 50.0]
        );
        assert_eq!(
            extend_ray(0.0, 0.0, 10.0, 10.0, Both, 100.0, 200.0),
            [-2.0, -2.0, 102.0, 102.0]
        );
        assert_eq!(
            extend_ray(5.0, 30.0, 5.0, 80.0, Both, 100.0, 200.0),
            [5.0, 0.0, 5.0, 200.0]
        );
        assert_eq!(
            extend_ray(5.0, 30.0, 5.0, 80.0, Right, 100.0, 200.0),
            [5.0, 30.0, 5.0, 200.0]
        );
        assert_eq!(
            extend_ray(5.0, 30.0, 5.0, 80.0, Left, 100.0, 200.0),
            [5.0, 0.0, 5.0, 80.0]
        );
        assert_eq!(
            extend_ray(5.0, 80.0, 5.0, 30.0, Right, 100.0, 200.0),
            [5.0, 80.0, 5.0, 0.0]
        );
        assert_eq!(
            extend_ray(5.0, 80.0, 5.0, 30.0, Left, 100.0, 200.0),
            [5.0, 200.0, 5.0, 30.0]
        );
    }
}
