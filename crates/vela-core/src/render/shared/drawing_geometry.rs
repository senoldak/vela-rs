//! Pure geometry, color and text helpers for the drawing layer. No chart dependency, so
//! they are unit-testable in isolation.

use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::drawings::{BoxTextSize, DrawingExtend};
use crate::model::series::LineStyle;

/// Map a bar time (epoch ms) to a fractional logical bar index, given the sorted bar
/// times and the median bar interval. Extrapolates linearly past either end (negative
/// before the first bar, beyond `n - 1` after the last) so drawings anchored to future or
/// extended times still resolve.
///
/// An interval of 0 or NaN reads as "unknown" (the original's `intervalMs || ...`). A NaN
/// `ms` falls through every branch to -1, as the original's search does.
pub fn bar_time_to_logical(ms: f64, times: &[i64], interval_ms: f64) -> f64 {
    let n = times.len();
    if n == 0 {
        return 0.0;
    }
    let known = |iv: f64, fallback: f64| {
        if iv == 0.0 || iv.is_nan() {
            fallback
        } else {
            iv
        }
    };
    let first = times[0] as f64;
    let last = times[n - 1] as f64;
    if ms <= first {
        let iv = if n > 1 {
            times[1] as f64 - first
        } else {
            known(interval_ms, 1.0)
        };
        return if iv > 0.0 { (ms - first) / iv } else { 0.0 };
    }
    if ms >= last {
        let fallback = if n > 1 {
            last - times[n - 2] as f64
        } else {
            1.0
        };
        let iv = known(interval_ms, fallback);
        return if iv > 0.0 {
            (n - 1) as f64 + (ms - last) / iv
        } else {
            (n - 1) as f64
        };
    }
    let (mut lo, mut hi) = (0i64, n as i64 - 1);
    while lo <= hi {
        let mid = (lo + hi) >> 1;
        let t = times[mid as usize] as f64;
        if t == ms {
            return mid as f64;
        }
        if t < ms {
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    // times[i] < ms < times[i + 1]
    let i = hi;
    let (Some(t0), Some(t1)) = (
        usize::try_from(i).ok().and_then(|i| times.get(i)),
        usize::try_from(i + 1).ok().and_then(|i| times.get(i)),
    ) else {
        return i as f64;
    };
    let (t0, t1) = (*t0 as f64, *t1 as f64);
    if t1 > t0 {
        i as f64 + (ms - t0) / (t1 - t0)
    } else {
        i as f64
    }
}

/// Median of consecutive diffs (a robust bar interval); 0 without two increasing times.
pub fn median_interval(times: &[i64]) -> f64 {
    let mut diffs: Vec<i64> = times
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| *d > 0)
        .collect();
    if diffs.is_empty() {
        return 0.0;
    }
    diffs.sort_unstable();
    diffs[diffs.len() >> 1] as f64
}

/// Canvas dash pattern (px) for a Pine line style, scaled by width.
pub fn dash_pattern(style: LineStyle, width: f64) -> Vec<f64> {
    let w = js_max(1.0, width);
    match style {
        LineStyle::Dotted => vec![w, w * 2.0],
        LineStyle::Dashed => vec![w * 4.0, w * 3.0],
        LineStyle::Solid => Vec::new(),
    }
}

/// Endpoints to actually stroke for a line, applying Pine `extend`. The returned segment
/// is left to right; arrowheads should use the ORIGINAL endpoints, not these. A vertical
/// line with any extend spans the full pane height.
pub fn extend_endpoints(
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
        // Vertical line: extend runs along the line itself (it has no horizontal room).
        // both: full height; right: ray from p1 through p2 to the edge; left: ray from p2
        // through p1 to the edge.
        if left && right {
            return [x1, 0.0, x2, height];
        }
        let downward = y2 >= y1; // p2 below p1 on canvas (y grows downward)
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
    let lx = if left { -2.0 } else { js_min(x1, x2) };
    let rx = if right { width + 2.0 } else { js_max(x1, x2) };
    [lx, y_at(lx), rx, y_at(rx)]
}

/// Canvas angle of the segment `(x1, y1)` to `(x2, y2)`, flipped 180 degrees when needed
/// so a label along the line stays upright (never upside-down). Canvas Y grows down.
pub fn upright_line_angle(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let mut a = (y2 - y1).atan2(x2 - x1);
    if a > std::f64::consts::FRAC_PI_2 {
        a -= std::f64::consts::PI;
    }
    if a < -std::f64::consts::FRAC_PI_2 {
        a += std::f64::consts::PI;
    }
    a
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// JavaScript whitespace and line terminators (what `trim()` and `\s` cover).
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `parseFloat`: the longest decimal prefix after leading whitespace, NaN when there is
/// none.
pub(crate) fn js_parse_float(text: &str) -> f64 {
    let s = text.trim_start_matches(is_js_space);
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'-') => (-1.0, &s[1..]),
        Some(b'+') => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    if rest.starts_with("Infinity") {
        return sign * f64::INFINITY;
    }
    let b = rest.as_bytes();
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int_len = digits(0);
    let mut end = int_len;
    let mut frac_len = 0;
    if b.get(end) == Some(&b'.') {
        frac_len = digits(end + 1);
        if int_len > 0 || frac_len > 0 {
            end += 1 + frac_len;
        }
    }
    if int_len == 0 && frac_len == 0 {
        return f64::NAN;
    }
    if matches!(b.get(end), Some(b'e' | b'E')) {
        let mut e = end + 1;
        if matches!(b.get(e), Some(b'+' | b'-')) {
            e += 1;
        }
        let exp_len = digits(e);
        if exp_len > 0 {
            end = e + exp_len;
        }
    }
    sign * rest[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// Parse `#RGB` / `#RRGGBB` / `#RRGGBBAA` / `rgb()` / `rgba()` into channels, or `None`.
pub fn parse_color(c: &str) -> Option<Rgba> {
    let s = c.trim_matches(is_js_space);
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0));
        let nibble = |i: usize| {
            let v = u8::from_str_radix(&hex[i..=i], 16).unwrap_or(0);
            f64::from(v * 17)
        };
        return match hex.len() {
            3 => Some(Rgba {
                r: nibble(0),
                g: nibble(1),
                b: nibble(2),
                a: 1.0,
            }),
            6 | 8 => Some(Rgba {
                r: byte(0),
                g: byte(2),
                b: byte(4),
                a: if hex.len() == 8 { byte(6) / 255.0 } else { 1.0 },
            }),
            _ => None,
        };
    }
    // rgb() / rgba(): `^rgba?\(([^)]+)\)$`, case-insensitive.
    let lower = s.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    if inner.is_empty() || inner.contains(')') {
        return None;
    }
    let parts: Vec<f64> = inner.split(',').map(js_parse_float).collect();
    Some(Rgba {
        r: parts.first().copied().unwrap_or(0.0),
        g: parts.get(1).copied().unwrap_or(0.0),
        b: parts.get(2).copied().unwrap_or(0.0),
        a: parts.get(3).copied().unwrap_or(1.0),
    })
}

pub fn relative_luminance(r: f64, g: f64, b: f64) -> f64 {
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

/// Black or white, whichever contrasts the given background better.
pub fn contrast_color(bg: Option<&str>) -> &'static str {
    let Some(bg) = bg.filter(|b| !b.is_empty()) else {
        return "#000000";
    };
    let Some(c) = parse_color(bg) else {
        return "#000000";
    };
    if relative_luminance(c.r, c.g, c.b) > 0.55 {
        "#000000"
    } else {
        "#ffffff"
    }
}

/// Line height (px) for multi-line drawing labels: one value shared by the canvas painter
/// and the inline text editor, so the glyphs do not shift when an edit starts.
pub fn label_line_height(font_size: f64) -> f64 {
    js_round(font_size * 1.4)
}

/// How far the frame around a targeted or edited text label extends past its glyph box,
/// horizontally (`inset`) and vertically (`rise`). Shared by the canvas painter
/// (selection and hover frame) and the inline editor (border and padding), which keeps
/// the two frames coincident by construction.
pub const TEXT_FRAME_INSET: f64 = 5.0;
pub const TEXT_FRAME_RISE: f64 = 3.0;

/// Fixed px for a named box text size; `auto` returns 0 (caller fits to box).
pub fn named_font_size(size: BoxTextSize) -> f64 {
    match size {
        BoxTextSize::Tiny => 10.0,
        BoxTextSize::Small => 12.0,
        BoxTextSize::Large => 18.0,
        BoxTextSize::Huge => 28.0,
        BoxTextSize::Normal => 14.0,
        BoxTextSize::Auto => 0.0,
    }
}

/// Smallest auto-fitted box text still drawn (px); below it the text is hidden.
const MIN_AUTO_FONT_PX: f64 = 6.0;

/// Auto font size (px) that fits `line_count` lines, the widest measuring `widest_at_14`
/// px at 14px, within a box's text area; 0 when the fit is too small to read.
pub fn auto_font_size(widest_at_14: f64, line_count: f64, box_w: f64, box_h: f64) -> f64 {
    let by_width = if widest_at_14 > 0.0 {
        (box_w * 14.0) / widest_at_14
    } else {
        f64::INFINITY
    };
    let by_height = (box_h * 0.85) / (line_count * 1.3);
    let size = js_min(js_min(by_width, by_height), 48.0);
    if size >= MIN_AUTO_FONT_PX { size } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMES: [i64; 5] = [0, 100, 200, 300, 400];

    // barTimeToLogical

    #[test]
    fn maps_exact_bar_times_to_integer_logicals() {
        assert_eq!(bar_time_to_logical(0.0, &TIMES, 100.0), 0.0);
        assert_eq!(bar_time_to_logical(200.0, &TIMES, 100.0), 2.0);
        assert_eq!(bar_time_to_logical(400.0, &TIMES, 100.0), 4.0);
    }

    #[test]
    fn interpolates_between_bars() {
        assert_eq!(bar_time_to_logical(250.0, &TIMES, 100.0), 2.5);
        assert_eq!(bar_time_to_logical(150.0, &TIMES, 100.0), 1.5);
    }

    #[test]
    fn extrapolates_before_the_first_bar() {
        assert_eq!(bar_time_to_logical(-100.0, &TIMES, 100.0), -1.0);
    }

    #[test]
    fn extrapolates_past_the_last_bar() {
        assert_eq!(bar_time_to_logical(600.0, &TIMES, 100.0), 6.0);
    }

    #[test]
    fn handles_empty_and_degenerate_inputs() {
        assert_eq!(bar_time_to_logical(5.0, &[], 0.0), 0.0); // no bars
        assert_eq!(bar_time_to_logical(10.0, &[10], 0.0), 0.0); // single bar, exact
        assert_eq!(bar_time_to_logical(5.0, &[10], 0.0), -5.0); // single bar, unit interval
        assert_eq!(bar_time_to_logical(f64::NAN, &TIMES, 100.0), -1.0);
        // Equal bar times give a zero interval before the first bar.
        assert_eq!(bar_time_to_logical(-5.0, &[0, 0], 100.0), 0.0);
        // Past the last bar without an interval, the last two bars' spacing is used.
        assert_eq!(bar_time_to_logical(500.0, &TIMES, 0.0), 5.0);
    }

    #[test]
    fn the_floor_of_the_fractional_logical_is_the_containing_bar() {
        // The external-crosshair ghost must light the bar a foreign time falls INSIDE,
        // never the nearest boundary: a 1h pointer at 14:00 belongs to TODAY's daily
        // candle even though it is past the midpoint (rounding would jump to tomorrow).
        let day: i64 = 86_400_000;
        let days = [0, day, 2 * day, 3 * day];
        let d = day as f64;
        let hover = 2.0 * d + 14.0 * 3_600_000.0;
        assert_eq!(js_round(bar_time_to_logical(hover, &days, d)), 3.0);
        assert_eq!(bar_time_to_logical(hover, &days, d).floor(), 2.0);
        assert_eq!(bar_time_to_logical(2.0 * d, &days, d).floor(), 2.0);
        assert_eq!(
            bar_time_to_logical(3.0 * d + d / 2.0, &days, d).floor(),
            3.0
        );
        assert_eq!(bar_time_to_logical(4.0 * d + 1.0, &days, d).floor(), 4.0);
    }

    // medianInterval

    #[test]
    fn returns_the_median_consecutive_diff() {
        assert_eq!(median_interval(&[0, 100, 200, 300]), 100.0);
        assert_eq!(median_interval(&[0, 100, 200, 10_000]), 100.0); // robust to one gap
    }

    #[test]
    fn median_is_zero_for_fewer_than_two_points() {
        assert_eq!(median_interval(&[5]), 0.0);
        assert_eq!(median_interval(&[]), 0.0);
        assert_eq!(median_interval(&[5, 5, 5]), 0.0);
    }

    // dashPattern

    #[test]
    fn solid_has_no_dash_and_dotted_dashed_scale_with_width() {
        assert!(dash_pattern(LineStyle::Solid, 2.0).is_empty());
        assert_eq!(dash_pattern(LineStyle::Dotted, 2.0), [2.0, 4.0]);
        assert_eq!(dash_pattern(LineStyle::Dashed, 2.0), [8.0, 6.0]);
        assert_eq!(dash_pattern(LineStyle::Dashed, 0.0), [4.0, 3.0]);
    }

    // extendEndpoints

    #[test]
    fn extend_none_returns_the_original_segment() {
        assert_eq!(
            extend_endpoints(10.0, 50.0, 20.0, 50.0, DrawingExtend::None, 100.0, 200.0),
            [10.0, 50.0, 20.0, 50.0]
        );
    }

    #[test]
    fn extend_right_runs_to_the_right_edge_and_left_to_the_left_edge() {
        assert_eq!(
            extend_endpoints(10.0, 50.0, 20.0, 50.0, DrawingExtend::Right, 100.0, 200.0),
            [10.0, 50.0, 102.0, 50.0]
        );
        assert_eq!(
            extend_endpoints(10.0, 50.0, 20.0, 50.0, DrawingExtend::Left, 100.0, 200.0),
            [-2.0, 50.0, 20.0, 50.0]
        );
    }

    #[test]
    fn extend_both_follows_the_slope_across_the_whole_pane() {
        assert_eq!(
            extend_endpoints(0.0, 0.0, 10.0, 10.0, DrawingExtend::Both, 100.0, 200.0),
            [-2.0, -2.0, 102.0, 102.0]
        );
    }

    #[test]
    fn a_vertical_line_with_extend_both_spans_the_full_pane_height() {
        assert_eq!(
            extend_endpoints(5.0, 30.0, 5.0, 80.0, DrawingExtend::Both, 100.0, 200.0),
            [5.0, 0.0, 5.0, 200.0]
        );
    }

    #[test]
    fn a_vertical_line_with_one_sided_extend_is_a_half_ray_along_its_direction() {
        let e = |y1, y2, extend| extend_endpoints(5.0, y1, 5.0, y2, extend, 100.0, 200.0);
        // downward line (y2 > y1): the right ray runs from y1 to the bottom edge
        assert_eq!(e(30.0, 80.0, DrawingExtend::Right), [5.0, 30.0, 5.0, 200.0]);
        // downward line: the left ray runs from the top edge through to y2
        assert_eq!(e(30.0, 80.0, DrawingExtend::Left), [5.0, 0.0, 5.0, 80.0]);
        // upward line (y2 < y1): the right ray runs from y1 up to the top edge
        assert_eq!(e(80.0, 30.0, DrawingExtend::Right), [5.0, 80.0, 5.0, 0.0]);
        // upward line: the left ray runs from the bottom edge to y2
        assert_eq!(e(80.0, 30.0, DrawingExtend::Left), [5.0, 200.0, 5.0, 30.0]);
    }

    // colour

    #[test]
    fn parses_hex_and_rgba_forms() {
        let rgba = |r, g, b, a| Some(Rgba { r, g, b, a });
        assert_eq!(parse_color("#fff"), rgba(255.0, 255.0, 255.0, 1.0));
        assert_eq!(parse_color("#4caf50"), rgba(76.0, 175.0, 80.0, 1.0));
        let c = parse_color("#4CAF5033").unwrap();
        assert_eq!([c.r, c.g, c.b], [76.0, 175.0, 80.0]);
        assert!((c.a - f64::from(0x33) / 255.0).abs() < 1e-3);
        assert_eq!(parse_color("rgba(1,2,3,0.5)"), rgba(1.0, 2.0, 3.0, 0.5));
        assert_eq!(parse_color("not-a-color"), None);
    }

    #[test]
    fn parses_the_edge_forms_the_original_regexes_accept() {
        // `rgb(` takes whatever floats it finds; missing channels read 0, missing alpha 1.
        assert_eq!(
            parse_color(" RGB( 10 , 20 ) "),
            Some(Rgba {
                r: 10.0,
                g: 20.0,
                b: 0.0,
                a: 1.0
            })
        );
        // An unparsable channel is NaN (it is present, so `?? 0` keeps it).
        assert!(parse_color("rgb(x,2,3)").unwrap().r.is_nan());
        assert_eq!(parse_color("rgb()"), None);
        assert_eq!(parse_color("#ff"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(parse_color(""), None);
        // Percent signs end the number, as `parseFloat` does.
        assert_eq!(parse_color("rgb(50%,0,0)").unwrap().r, 50.0);
        assert_eq!(parse_color("rgba(0,0,0,.5)").unwrap().a, 0.5);
        assert_eq!(parse_color("rgba(0,0,0,1e-1)").unwrap().a, 0.1);
    }

    #[test]
    fn js_parse_float_matches_the_javascript_function() {
        assert_eq!(js_parse_float("  12.5px"), 12.5);
        assert_eq!(js_parse_float("-3"), -3.0);
        assert_eq!(js_parse_float(".5"), 0.5);
        assert_eq!(js_parse_float("5."), 5.0);
        assert_eq!(js_parse_float("1e3x"), 1000.0);
        assert_eq!(js_parse_float("1e"), 1.0);
        assert_eq!(js_parse_float("-Infinityx"), f64::NEG_INFINITY);
        assert!(js_parse_float("").is_nan());
        assert!(js_parse_float(".").is_nan());
        assert!(js_parse_float("abc").is_nan());
    }

    #[test]
    fn auto_contrast_picks_black_on_light_and_white_on_dark() {
        assert_eq!(contrast_color(Some("#ffffff")), "#000000");
        assert_eq!(contrast_color(Some("#000000")), "#ffffff");
        assert_eq!(contrast_color(None), "#000000");
        assert_eq!(contrast_color(Some("")), "#000000");
        assert_eq!(contrast_color(Some("papayawhip")), "#000000"); // unparsable: black
    }

    // uprightLineAngle

    #[test]
    fn a_rightward_horizontal_is_0_and_a_leftward_one_flips_to_0() {
        assert!(upright_line_angle(0.0, 0.0, 10.0, 0.0).abs() < 1e-12);
        assert!(upright_line_angle(10.0, 0.0, 0.0, 0.0).abs() < 1e-12);
    }

    #[test]
    fn follows_a_diagonal_and_flips_past_vertical_so_glyphs_never_read_upside_down() {
        use std::f64::consts::FRAC_PI_4;
        let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-12, "{a} != {b}");
        near(upright_line_angle(0.0, 10.0, 10.0, 0.0), -FRAC_PI_4); // up-right
        near(upright_line_angle(0.0, 0.0, 10.0, 10.0), FRAC_PI_4); // down-right
        near(upright_line_angle(10.0, 0.0, 0.0, 10.0), -FRAC_PI_4); // down-left, flipped
        near(upright_line_angle(10.0, 10.0, 0.0, 0.0), FRAC_PI_4); // up-left, flipped
    }

    // label and box text sizes

    #[test]
    fn label_line_height_rounds_the_scaled_font_size() {
        assert_eq!(label_line_height(10.0), 14.0);
        assert_eq!(label_line_height(11.0), 15.0); // 15.4
        assert_eq!(label_line_height(12.5), 18.0); // 17.5 rounds up
    }

    #[test]
    fn named_font_sizes() {
        assert_eq!(named_font_size(BoxTextSize::Tiny), 10.0);
        assert_eq!(named_font_size(BoxTextSize::Small), 12.0);
        assert_eq!(named_font_size(BoxTextSize::Normal), 14.0);
        assert_eq!(named_font_size(BoxTextSize::Large), 18.0);
        assert_eq!(named_font_size(BoxTextSize::Huge), 28.0);
        assert_eq!(named_font_size(BoxTextSize::Auto), 0.0);
    }

    #[test]
    fn auto_font_size_fits_the_box_and_hides_below_six_px() {
        // 100px wide at 14px in a 200x100 box: width allows 28, height (3 lines) 21.8.
        assert!((auto_font_size(100.0, 3.0, 200.0, 100.0) - 85.0 / 3.9).abs() < 1e-9);
        // Capped at 48.
        assert_eq!(auto_font_size(0.0, 1.0, 1000.0, 1000.0), 48.0);
        // Too small to read.
        assert_eq!(auto_font_size(1000.0, 1.0, 10.0, 100.0), 0.0);
        assert_eq!(auto_font_size(10.0, 1.0, 100.0, 4.0), 0.0);
        // NaN input reads as unreadable.
        assert_eq!(auto_font_size(10.0, 1.0, f64::NAN, 100.0), 0.0);
    }
}
