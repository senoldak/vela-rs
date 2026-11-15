//! Geometry and paint rules of the price styles drawn from the close alone: line with
//! markers, step line and columns. Shared by both geometry backends so canvas2d and
//! WebGL2 place, show and color them identically.

use crate::indicators::classics::math::{js_max, js_round};
use crate::model::Ohlcv;

use super::candle_lod::CANDLE_BODY_MIN_SPACING;

/// How far a line marker stands out of the line on each side, in CSS px.
const LINE_MARKER_RIM: f64 = 3.0;
/// Clear space kept between two neighbouring markers, in CSS px.
pub const LINE_MARKER_MIN_GAP: f64 = 2.0;

/// Radius (CSS px) of the line-with-markers dot for a line width: 8 px across on the
/// default 2 px line, growing with a heavier line so the dot still reads as one.
pub fn line_marker_radius(line_width: f64) -> f64 {
    js_max(1.0, line_width) / 2.0 + LINE_MARKER_RIM
}

/// Whether the dots show at this bar pitch (center-to-center px). Below it two
/// neighbours touch or overlap and the dots merge into a lumpy band thicker than the
/// line, which reads worse than the plain line, so a dense view shows just the line
/// (10 px on the default 2 px line: an 8 px dot with 2 px of line between neighbours).
pub fn line_markers_visible(px_per_bar: f64, radius: f64) -> bool {
    px_per_bar >= 2.0 * radius + LINE_MARKER_MIN_GAP
}

/// Whether bar `i`'s column wears the up color: its close is at or above the previous
/// bar's close (an unchanged value stays up even right after a fall). The first bar has
/// no previous close and is up.
pub fn column_is_up(bars: &[Ohlcv], i: usize) -> bool {
    let (Some(bar), Some(prev)) = (bars.get(i), i.checked_sub(1).and_then(|p| bars.get(p))) else {
        return true;
    };
    bar.close >= prev.close
}

/// Gap between two columns, in CSS px.
pub const COLUMN_GAP: f64 = 1.0;

/// One column's horizontal extent, in CSS px, edges on the device-pixel grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnGeometry {
    pub left: f64,
    pub width: f64,
}

/// A column's extent around bar center `x` for body spacing `spacing`: it fills the bar's
/// slot less a [`COLUMN_GAP`] gap, split evenly on both sides where the device pixels
/// allow. Edges snap to the device grid from the slot boundaries, so neighbouring slots
/// share an edge and the gaps stay uniform. Below the candle-body spacing the gap would
/// take a third of every slot, so tight zooms tile the columns into a solid silhouette.
pub fn column_geometry(x: f64, spacing: f64, dpr: f64) -> ColumnGeometry {
    let half = spacing / 2.0;
    let left_dev = js_round((x - half) * dpr);
    let right_dev = js_round((x + half) * dpr);
    let gap_dev = if spacing >= CANDLE_BODY_MIN_SPACING {
        js_max(1.0, js_round(COLUMN_GAP * dpr))
    } else {
        0.0
    };
    let width_dev = js_max(1.0, right_dev - left_dev - gap_dev);
    ColumnGeometry {
        left: (left_dev + f64::floor(gap_dev / 2.0)) / dpr,
        width: width_dev / dpr,
    }
}

/// Snap a stroke's centerline coordinate so a stroke `width` CSS px wide covers whole
/// device pixels: an odd device-pixel width centers on a pixel's middle, an even one on a
/// pixel edge. The step line is only horizontal and vertical runs, which this keeps
/// crisp.
pub fn snap_stroke(v: f64, width: f64, dpr: f64) -> f64 {
    let off = if js_max(1.0, js_round(width * dpr)) % 2.0 == 1.0 {
        0.5
    } else {
        0.0
    };
    (js_round(v * dpr - off) + off) / dpr
}

/// A point of a close path, in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClosePoint {
    pub x: f64,
    pub y: f64,
}

/// The contiguous runs of close points over bars `[from, to]`: a bar `point_of` has no
/// point for (a hole in the series) ends the run, as it breaks the line style.
pub fn close_runs(
    from: usize,
    to: usize,
    point_of: &dyn Fn(usize) -> Option<ClosePoint>,
) -> Vec<Vec<ClosePoint>> {
    let mut runs = Vec::new();
    let mut run = Vec::new();
    for i in from..=to {
        if let Some(p) = point_of(i) {
            run.push(p);
            continue;
        }
        if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// The step line's vertices through one run of bar points (left to right): each close
/// held flat to the next bar's x, then a vertical move to that bar's close. A move with
/// no height adds no vertex.
pub fn step_vertices(pts: &[ClosePoint]) -> Vec<ClosePoint> {
    let mut out = Vec::new();
    for (k, p) in pts.iter().enumerate() {
        if k == 0 {
            out.push(*p);
            continue;
        }
        let prev_y = pts[k - 1].y;
        out.push(ClosePoint { x: p.x, y: prev_y });
        if p.y != prev_y {
            out.push(*p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOSES: [f64; 5] = [10.0, 12.0, 11.0, 11.0, 13.0];

    fn bar(time: i64, close: f64) -> Ohlcv {
        Ohlcv::new(time, close, close, close, close, None)
    }

    fn bars() -> Vec<Ohlcv> {
        CLOSES
            .iter()
            .enumerate()
            .map(|(i, c)| bar(i as i64 * 60_000, *c))
            .collect()
    }

    fn pt(x: f64, y: f64) -> ClosePoint {
        ClosePoint { x, y }
    }

    #[test]
    fn a_column_is_up_when_its_close_is_at_or_above_the_previous_close() {
        let b = bars();
        let ups: Vec<bool> = (0..CLOSES.len()).map(|i| column_is_up(&b, i)).collect();
        assert_eq!(ups, [true, true, false, true, true]);
        // An unchanged value stays up even right after a fall; the open never decides.
        assert!(column_is_up(&[bar(0, 5.0), bar(1, 4.0), bar(2, 4.0)], 2));
        let tall = Ohlcv::new(1, 9.0, 9.0, 1.0, 6.0, None);
        assert!(column_is_up(&[bar(0, 5.0), tall], 1));
        let lone = Ohlcv::new(0, 9.0, 9.0, 1.0, 1.0, None);
        assert!(column_is_up(&[lone], 0));
    }

    #[test]
    fn a_missing_bar_reads_as_up() {
        assert!(column_is_up(&[], 0));
        assert!(column_is_up(&[bar(0, 1.0)], 7));
    }

    #[test]
    fn a_nan_close_reads_as_down() {
        assert!(!column_is_up(&[bar(0, 1.0), bar(1, f64::NAN)], 1));
    }

    #[test]
    fn markers_are_8px_across_on_the_default_line_and_show_only_where_bars_have_room() {
        let r = line_marker_radius(2.0);
        assert_eq!(r * 2.0, 8.0);
        assert!(!line_markers_visible(6.0, r));
        assert!(!line_markers_visible(9.9, r));
        assert!(line_markers_visible(10.0, r));
        assert!(line_markers_visible(13.0, r));
        assert!(line_marker_radius(4.0) > r);
        assert_eq!(line_marker_radius(0.0), 3.5); // a zero width reads as 1
    }

    #[test]
    fn columns_fill_their_slot_less_a_1px_gap_and_tight_zooms_tile() {
        let spacing = 10.6;
        let slots: Vec<ColumnGeometry> = (0..5)
            .map(|i| column_geometry(100.0 + f64::from(i) * spacing, spacing, 1.0))
            .collect();
        for w in slots.windows(2) {
            assert_eq!(w[1].left - (w[0].left + w[0].width), 1.0);
        }
        for s in &slots {
            assert!(s.width == 9.0 || s.width == 10.0);
        }
        // Below the candle-body spacing the gap would eat a third of the slot: no gap.
        let a = column_geometry(50.0, 2.0, 1.0);
        let b = column_geometry(52.0, 2.0, 1.0);
        assert_eq!(b.left, a.left + a.width);
        // On a 2x screen the gap is two device pixels, one on each side.
        assert_eq!(
            column_geometry(50.0, 10.0, 2.0),
            ColumnGeometry {
                left: 45.5,
                width: 9.0
            }
        );
    }

    #[test]
    fn a_column_is_never_narrower_than_one_device_pixel() {
        let g = column_geometry(10.0, 0.1, 1.0);
        assert_eq!(g.width, 1.0);
    }

    #[test]
    fn the_step_line_holds_each_close_to_the_next_bar_then_steps_vertically() {
        let pts = [
            pt(0.0, 50.0),
            pt(10.0, 40.0),
            pt(20.0, 40.0),
            pt(30.0, 45.0),
        ];
        assert_eq!(
            step_vertices(&pts),
            vec![
                pt(0.0, 50.0),
                pt(10.0, 50.0),
                pt(10.0, 40.0),
                pt(20.0, 40.0), // unchanged close: no vertical step
                pt(30.0, 40.0),
                pt(30.0, 45.0),
            ]
        );
        assert!(step_vertices(&[]).is_empty());
        assert_eq!(step_vertices(&[pt(1.0, 2.0)]), vec![pt(1.0, 2.0)]);
    }

    #[test]
    fn a_hole_in_the_series_ends_the_run() {
        let point_of = |i: usize| (i != 1).then(|| pt(i as f64, i as f64));
        assert_eq!(
            close_runs(0, 3, &point_of),
            vec![vec![pt(0.0, 0.0)], vec![pt(2.0, 2.0), pt(3.0, 3.0)]]
        );
        assert!(close_runs(2, 1, &point_of).is_empty());
        assert!(close_runs(0, 2, &|_| None).is_empty());
    }

    #[test]
    fn step_line_strokes_snap_onto_whole_device_pixels() {
        assert_eq!(snap_stroke(10.3, 2.0, 1.0), 10.0); // 2 device px: centered on an edge
        assert_eq!(snap_stroke(10.3, 1.0, 1.0), 10.5); // 1 device px: on a pixel's middle
        assert_eq!(snap_stroke(10.3, 2.0, 2.0), 10.5); // 4 device px: on an edge of the 2x grid
    }
}
