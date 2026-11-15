//! The pure decision logic of the user-drawing interaction (`DrawingInteraction.ts`) and
//! of the controller's slice keying (`UserDrawingController.ts`).
//!
//! The interaction itself is a state machine over live drawings that it mutates through
//! shared references (a drag translates a drawing's anchors in place), driven by pointer
//! events: that wiring is a later stage. What is here are the steps of it that are plain
//! functions of numbers and anchors: which slice a drawing belongs to, the drag slop,
//! Shift's 45 degree angle lock, the parallel channel's placement anchor, the freehand
//! sampling and thinning, and the marquee box and what it selects.

use crate::drawings::{Bounds, Drawing, DrawingPoint, Projector};
use crate::indicators::classics::math::js_round;

/// Pixels of motion before a press counts as a drag (vs a click, which opens settings).
pub const DRAG_SLOP: f64 = 3.0;
/// Minimum pixel gap between captured freehand points (keeps a stroke from over-sampling).
pub const FREEHAND_SAMPLE_PX: f64 = 4.0;
/// Shift-snap quantum: the free endpoint of a line locks to 45 degree steps.
pub const ANGLE_SNAP_STEP: f64 = std::f64::consts::FRAC_PI_4;
/// Two-point segment tools whose free endpoint angle-snaps while Shift is held.
pub const ANGLE_SNAP_TYPES: [&str; 6] = [
    "trendline",
    "ray",
    "extendedline",
    "infoline",
    "trendangle",
    "arrow",
];

/// Whether `type_key` is a two-point segment tool that angle-snaps under Shift.
pub fn is_angle_snap_type(type_key: &str) -> bool {
    ANGLE_SNAP_TYPES.contains(&type_key)
}

/// Which interleave layer a drawing belongs to: the first series boundary at or above its
/// z (the slice painted just before that series, so a drawing TYING a series' z paints
/// under it). `None` when the drawing clears every boundary and belongs on the top canvas
/// instead. `boundaries` ascend.
pub fn slice_key_for(z_index: f64, boundaries: &[f64]) -> Option<f64> {
    let top = boundaries.last()?;
    if z_index > *top {
        return None;
    }
    boundaries.iter().copied().find(|b| *b >= z_index)
}

/// Whether a press at `(px, py)` has moved far enough to `(x, y)` to be a drag. The slop
/// is per axis and inclusive: a move of exactly [`DRAG_SLOP`] on both axes is still a
/// click.
pub fn exceeds_drag_slop(x: f64, y: f64, px: f64, py: f64) -> bool {
    !((x - px).abs() <= DRAG_SLOP && (y - py).abs() <= DRAG_SLOP)
}

/// Whether the magnet moved a point: the snap ring is shown only when it did (so `weak`
/// outside its radius shows nothing).
pub fn snap_moved(raw: DrawingPoint, snapped: DrawingPoint) -> bool {
    snapped.time != raw.time || snapped.price != raw.price
}

/// The pixel `(x, y)` locked to the nearest 45 degree ray around the pivot pixel
/// `(px, py)`, keeping the radius (Shift held on a line tool). It works in pixel space:
/// the user reasons about the angle they see, not about time and price units. `None`
/// when the cursor is within a pixel of the pivot (too close to define an angle): the
/// raw cursor is used then.
pub fn angle_snapped_pixel(px: f64, py: f64, x: f64, y: f64) -> Option<(f64, f64)> {
    let r = (x - px).hypot(y - py);
    if r < 1.0 {
        return None;
    }
    let angle = js_round((y - py).atan2(x - px) / ANGLE_SNAP_STEP) * ANGLE_SNAP_STEP;
    Some((px + r * angle.cos(), py + r * angle.sin()))
}

/// Map a raw placement cursor to the anchor the drawing actually stores.
///
/// Parallel channel: its 3rd anchor is the parallel-line price measured AT THE BASELINE
/// MIDPOINT (its time is ignored). The cursor, though, sits near p2, so storing it raw
/// would read as an offset of about half the baseline's price span and snap the channel
/// wide open. Instead measure the cursor's vertical gap from the baseline at its own time
/// and re-express that as the midpoint price, so the channel line simply passes through
/// the cursor. Every other type is unchanged.
pub fn placement_anchor(
    type_key: &str,
    anchors: &[DrawingPoint],
    cursor: DrawingPoint,
) -> DrawingPoint {
    if type_key != "parallelchannel" || anchors.len() != 2 {
        return cursor;
    }
    let (a, b) = (anchors[0], anchors[1]);
    let span = b.time - a.time;
    let base_at_cursor = if span == 0.0 {
        a.price
    } else {
        a.price + ((b.price - a.price) * (cursor.time - a.time)) / span
    };
    let gap = cursor.price - base_at_cursor;
    DrawingPoint {
        time: (a.time + b.time) / 2.0,
        price: (a.price + b.price) / 2.0 + gap,
    }
}

/// Whether a freehand point is captured at `(x, y)` given the pixel of the last captured
/// anchor `(last_x, last_y)`. A last anchor that cannot be projected is `NaN`, and a `NaN`
/// distance never exceeds the gap (nothing is captured), as in the original.
pub fn freehand_should_sample(x: f64, y: f64, last_x: f64, last_y: f64) -> bool {
    (x - last_x).hypot(y - last_y) > FREEHAND_SAMPLE_PX
}

/// At the path cap a long stroke keeps drawing: halve the sampled points (keep the
/// endpoints, drop every other interior one) so the older trail gets progressively
/// coarser instead of the capture dying mid-gesture (frozen anchors plus a live cursor
/// read as one straight rubber band). Does nothing below `cap`.
pub fn thin_freehand_anchors(anchors: &mut Vec<DrawingPoint>, cap: usize) {
    if anchors.is_empty() || anchors.len() < cap {
        return;
    }
    let last = anchors.len() - 1;
    let mut i = 0;
    anchors.retain(|_| {
        let keep = i % 2 == 0 || i == last;
        i += 1;
        keep
    });
}

/// The marquee box swept from the press `(x0, y0)` to the cursor `(x1, y1)`, normalized.
pub fn marquee_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds {
    Bounds {
        x: x0.min(x1),
        y: y0.min(y1),
        w: (x1 - x0).abs(),
        h: (y1 - y0).abs(),
    }
}

/// Whether a swept box counts as a sweep (a twitch within the drag slop selects nothing).
pub fn marquee_selects(rect: &Bounds) -> bool {
    rect.w > DRAG_SLOP || rect.h > DRAG_SLOP
}

/// Closed-interval overlap, so a zero-thickness bound (a horizontal line's) still counts.
pub fn rects_intersect(a: &Bounds, b: &Bounds) -> bool {
    a.x <= b.x + b.w && b.x <= a.x + a.w && a.y <= b.y + b.h && b.y <= a.y + a.h
}

/// The selection after a sweep: the current `selected` ids plus every visible drawing
/// whose on-screen bounds touch `rect` (a union, so successive sweeps accumulate), in
/// paint order after the existing ones. `None` when the box touches nothing new (no
/// change, no intent).
pub fn marquee_selection(
    selected: &[String],
    drawings: &[&dyn Drawing],
    proj: &dyn Projector,
    rect: &Bounds,
) -> Option<Vec<String>> {
    let mut ids: Vec<String> = selected.to_vec();
    for d in drawings {
        let base = d.base();
        if !base.visible || ids.contains(&base.id) {
            continue;
        }
        if d.bounds(proj).is_some_and(|b| rects_intersect(rect, &b)) {
            ids.push(base.id.clone());
        }
    }
    (ids.len() > selected.len()).then_some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::{DrawingInit, DrawingRegistry, LinearProjector};

    fn pt(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    // slice keys (indicator-drawing-slices.test.ts, drawings-interaction.test.ts)

    #[test]
    fn a_drawing_tying_a_series_z_keys_at_that_series() {
        // sliceKeyFor pins a tie UNDER the series (indicators key strictly above it).
        assert_eq!(slice_key_for(-1.0, &[-1.0, 0.0]), Some(-1.0));
    }

    #[test]
    fn a_drawing_keys_at_the_first_boundary_at_or_above_it() {
        assert_eq!(slice_key_for(-0.5, &[-1.0, 0.0, 2.0]), Some(0.0));
        assert_eq!(slice_key_for(-5.0, &[-1.0, 0.0]), Some(-1.0));
        assert_eq!(slice_key_for(2.0, &[-1.0, 0.0, 2.0]), Some(2.0));
    }

    #[test]
    fn a_drawing_clearing_every_boundary_belongs_on_the_top_canvas() {
        assert_eq!(slice_key_for(0.5, &[-1.0, 0.0]), None);
        assert_eq!(slice_key_for(0.0, &[]), None);
    }

    // drag slop

    #[test]
    fn the_slop_is_per_axis_and_inclusive() {
        assert!(!exceeds_drag_slop(13.0, 93.0, 10.0, 90.0));
        assert!(!exceeds_drag_slop(7.0, 87.0, 10.0, 90.0));
        assert!(exceeds_drag_slop(13.5, 90.0, 10.0, 90.0));
        assert!(exceeds_drag_slop(10.0, 93.5, 10.0, 90.0));
    }

    #[test]
    fn a_snap_ring_shows_only_when_the_magnet_moved_the_point() {
        assert!(!snap_moved(pt(1.0, 2.0), pt(1.0, 2.0)));
        assert!(snap_moved(pt(1.0, 2.0), pt(1.0, 2.5)));
        assert!(snap_moved(pt(1.0, 2.0), pt(1.5, 2.0)));
    }

    // Shift angle snap (drawings-interaction.test.ts, "Shift angle snap (45 degree steps)")

    #[test]
    fn a_near_horizontal_cursor_locks_flat_keeping_the_radius() {
        // pivot pixel (10, 90), cursor (50, 85): about 7 degrees off horizontal.
        let (x, y) = angle_snapped_pixel(10.0, 90.0, 50.0, 85.0).unwrap();
        assert!((y - 90.0).abs() < 1e-9);
        assert!((x - (10.0 + 40.0_f64.hypot(5.0))).abs() < 1e-9);
    }

    #[test]
    fn a_diagonal_cursor_locks_to_the_45_degree_ray() {
        let (x, y) = angle_snapped_pixel(10.0, 90.0, 45.0, 60.0).unwrap();
        // 40.6 degrees up snaps to 45: equal horizontal and vertical deltas, upward on screen.
        assert!(((x - 10.0) - (90.0 - y)).abs() < 1e-9);
        assert!(y < 90.0);
        let r = 35.0_f64.hypot(30.0);
        assert!(((x - 10.0).hypot(y - 90.0) - r).abs() < 1e-9);
    }

    #[test]
    fn a_cursor_within_a_pixel_of_the_pivot_has_no_angle() {
        assert_eq!(angle_snapped_pixel(10.0, 90.0, 10.5, 90.5), None);
        assert_eq!(angle_snapped_pixel(10.0, 90.0, 10.0, 90.0), None);
    }

    #[test]
    fn the_ray_choice_rounds_halves_like_math_round() {
        // exactly 22.5 degrees rounds up to the 45 degree ray (Math.round(0.5) = 1)
        let theta = std::f64::consts::FRAC_PI_8;
        let (x, y) =
            angle_snapped_pixel(0.0, 0.0, 100.0 * theta.cos(), 100.0 * theta.sin()).unwrap();
        // a floating point 22.4999... may land on either side: the result is one of the two rays
        let on_flat = y.abs() < 1e-9;
        let on_diagonal = (x - y).abs() < 1e-9;
        assert!(on_flat || on_diagonal);
    }

    #[test]
    fn angle_snap_applies_to_the_two_point_segment_tools_only() {
        for t in [
            "trendline",
            "ray",
            "extendedline",
            "infoline",
            "trendangle",
            "arrow",
        ] {
            assert!(is_angle_snap_type(t), "{t}");
        }
        for t in ["datepricerange", "hline", "parallelchannel", "rect"] {
            assert!(!is_angle_snap_type(t), "{t}");
        }
    }

    // parallel channel placement (drawings-interaction.test.ts)

    #[test]
    fn the_parallel_channels_third_anchor_tracks_the_cursors_gap_from_the_baseline() {
        let anchors = [pt(0.0, 0.0), pt(50.0, 50.0)];
        let at = |c: DrawingPoint| placement_anchor("parallelchannel", &anchors, c);
        // on the baseline, whatever its time: collapsed (midpoint price, offset 0)
        assert_eq!(at(pt(50.0, 50.0)), pt(25.0, 25.0));
        assert_eq!(at(pt(10.0, 10.0)), pt(25.0, 25.0));
        // 8 above the baseline near p2 (baseline there = 50): midprice 25 + 8
        assert_eq!(at(pt(50.0, 58.0)), pt(25.0, 33.0));
    }

    #[test]
    fn a_vertical_baseline_measures_from_its_first_anchor() {
        let anchors = [pt(10.0, 5.0), pt(10.0, 9.0)];
        assert_eq!(
            placement_anchor("parallelchannel", &anchors, pt(30.0, 12.0)),
            pt(10.0, 7.0 + (12.0 - 5.0))
        );
    }

    #[test]
    fn other_types_and_other_anchor_counts_store_the_cursor_as_is() {
        let two = [pt(0.0, 0.0), pt(50.0, 50.0)];
        let cursor = pt(50.0, 58.0);
        assert_eq!(placement_anchor("trendline", &two, cursor), cursor);
        assert_eq!(
            placement_anchor("parallelchannel", &two[..1], cursor),
            cursor
        );
        let three = [pt(0.0, 0.0), pt(50.0, 50.0), pt(25.0, 25.0)];
        assert_eq!(placement_anchor("parallelchannel", &three, cursor), cursor);
    }

    // freehand

    #[test]
    fn a_freehand_point_is_taken_past_four_pixels_from_the_last() {
        assert!(!freehand_should_sample(14.0, 50.0, 10.0, 50.0));
        assert!(freehand_should_sample(15.0, 50.0, 10.0, 50.0));
        // a last anchor that cannot be projected is NaN: nothing is captured
        assert!(!freehand_should_sample(500.0, 50.0, f64::NAN, 50.0));
        assert!(!freehand_should_sample(500.0, 50.0, 10.0, f64::NAN));
    }

    #[test]
    fn a_stroke_at_the_cap_halves_keeping_the_endpoints() {
        let mut a: Vec<DrawingPoint> = (0..8).map(|i| pt(f64::from(i), 0.0)).collect();
        thin_freehand_anchors(&mut a, 8);
        let times: Vec<f64> = a.iter().map(|p| p.time).collect();
        // even indices plus the last
        assert_eq!(times, vec![0.0, 2.0, 4.0, 6.0, 7.0]);
        // an odd count keeps the last (even) index once
        let mut b: Vec<DrawingPoint> = (0..5).map(|i| pt(f64::from(i), 0.0)).collect();
        thin_freehand_anchors(&mut b, 5);
        assert_eq!(
            b.iter().map(|p| p.time).collect::<Vec<_>>(),
            vec![0.0, 2.0, 4.0]
        );
    }

    #[test]
    fn a_stroke_below_the_cap_is_untouched() {
        let mut a: Vec<DrawingPoint> = (0..7).map(|i| pt(f64::from(i), 0.0)).collect();
        thin_freehand_anchors(&mut a, 8);
        assert_eq!(a.len(), 7);
        let mut empty = Vec::new();
        thin_freehand_anchors(&mut empty, 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn a_marathon_stroke_never_freezes_or_exceeds_the_cap() {
        // drawings-interaction.test.ts: 5px steps for MAX_PATH_POINTS + 200 samples.
        let cap = crate::drawings::types::shapes::MAX_PATH_POINTS;
        let mut anchors = vec![pt(0.0, 50.0)];
        for i in 1..=(cap + 200) {
            let x = i as f64 * 5.0;
            let last = anchors[anchors.len() - 1];
            if freehand_should_sample(x, 50.0, last.time, 50.0) {
                if anchors.len() >= cap {
                    thin_freehand_anchors(&mut anchors, cap);
                }
                anchors.push(pt(x, 50.0));
            }
        }
        assert!(anchors.len() <= cap);
        assert!(anchors[anchors.len() - 1].time > cap as f64 * 5.0);
    }

    // marquee (drawings-interaction.test.ts, "marquee")

    #[test]
    fn the_marquee_box_normalizes_in_any_direction() {
        let r = marquee_rect(10.0, 10.0, 60.0, 50.0);
        assert_eq!(
            r,
            Bounds {
                x: 10.0,
                y: 10.0,
                w: 50.0,
                h: 40.0
            }
        );
        let r = marquee_rect(60.0, 50.0, 10.0, 10.0);
        assert_eq!(
            r,
            Bounds {
                x: 10.0,
                y: 10.0,
                w: 50.0,
                h: 40.0
            }
        );
        assert_eq!(
            marquee_rect(100.0, 90.0, 100.0, 90.0),
            Bounds {
                x: 100.0,
                y: 90.0,
                w: 0.0,
                h: 0.0
            }
        );
    }

    #[test]
    fn a_box_within_the_slop_selects_nothing() {
        assert!(!marquee_selects(&marquee_rect(30.0, 30.0, 32.0, 31.0)));
        assert!(!marquee_selects(&marquee_rect(30.0, 30.0, 33.0, 33.0)));
        assert!(marquee_selects(&marquee_rect(30.0, 30.0, 34.0, 30.0)));
        assert!(marquee_selects(&marquee_rect(30.0, 30.0, 30.0, 34.0)));
    }

    #[test]
    fn rect_overlap_is_a_closed_interval_so_flat_bounds_count() {
        let a = Bounds {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        assert!(rects_intersect(
            &a,
            &Bounds {
                x: 10.0,
                y: 10.0,
                w: 5.0,
                h: 5.0
            }
        ));
        assert!(rects_intersect(
            &a,
            &Bounds {
                x: 5.0,
                y: 5.0,
                w: 0.0,
                h: 0.0
            }
        ));
        assert!(rects_intersect(
            &a,
            &Bounds {
                x: -5.0,
                y: 4.0,
                w: 100.0,
                h: 0.0
            }
        ));
        assert!(!rects_intersect(
            &a,
            &Bounds {
                x: 10.5,
                y: 0.0,
                w: 5.0,
                h: 5.0
            }
        ));
    }

    /// The fixture projector of the original test: x = time, y = 100 - price.
    fn test_projector() -> LinearProjector {
        LinearProjector {
            pane_id: "price".into(),
            time_origin: 0.0,
            ms_per_px: 1.0,
            price_top: 100.0,
            price_per_px: 1.0,
            width: 200.0,
            height: 100.0,
            bars: None,
        }
    }

    fn drawing(type_key: &str, id: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        DrawingRegistry::builtin()
            .create(
                type_key,
                DrawingInit {
                    id: Some(id.to_string()),
                    anchors,
                    ..DrawingInit::on_pane("price")
                },
            )
            .expect("registered")
    }

    fn refs(ds: &[Box<dyn Drawing>]) -> Vec<&dyn Drawing> {
        ds.iter().map(|d| d.as_ref()).collect()
    }

    fn marquee_fixture() -> Vec<Box<dyn Drawing>> {
        vec![
            // px (20,20) to (40,40)
            drawing("trendline", "dw-1", vec![pt(20.0, 80.0), pt(40.0, 60.0)]),
            // px (150,80) to (180,90): far right
            drawing("trendline", "dw-2", vec![pt(150.0, 20.0), pt(180.0, 10.0)]),
            // a full-width line at y = 30
            drawing("hline", "dw-3", vec![pt(0.0, 70.0)]),
        ]
    }

    #[test]
    fn a_sweep_selects_every_visible_drawing_whose_bounds_touch_the_box() {
        let ds = marquee_fixture();
        let rect = marquee_rect(10.0, 10.0, 60.0, 50.0);
        let picked = marquee_selection(&[], &refs(&ds), &test_projector(), &rect);
        // dw-2 sits far right; the full-width line crosses the box
        assert_eq!(picked, Some(vec!["dw-1".to_string(), "dw-3".to_string()]));
    }

    #[test]
    fn a_hidden_drawing_is_never_picked() {
        let mut ds = marquee_fixture();
        ds[0].base_mut().visible = false;
        let rect = marquee_rect(60.0, 50.0, 10.0, 10.0);
        let picked = marquee_selection(&[], &refs(&ds), &test_projector(), &rect);
        assert_eq!(picked, Some(vec!["dw-3".to_string()]));
    }

    #[test]
    fn a_sweep_adds_to_the_selection_and_stays_silent_when_nothing_is_new() {
        let ds = marquee_fixture();
        let rect = marquee_rect(10.0, 10.0, 60.0, 50.0);
        let proj = test_projector();
        // dw-2 stays selected, dw-1 and dw-3 are added in paint order
        let picked = marquee_selection(&["dw-2".to_string()], &refs(&ds), &proj, &rect);
        assert_eq!(
            picked,
            Some(vec![
                "dw-2".to_string(),
                "dw-1".to_string(),
                "dw-3".to_string()
            ])
        );
        let all = ["dw-1", "dw-2", "dw-3"].map(String::from);
        assert_eq!(marquee_selection(&all, &refs(&ds), &proj, &rect), None);
    }
}
