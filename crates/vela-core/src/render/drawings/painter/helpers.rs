//! Small shared pieces of the drawing painter: stroke setup, the rounded rectangle and
//! speech-bubble paths, color tests, and text-font strings.

use crate::drawings::DrawingStyle;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{BoxTextSize, LineStyle};
use crate::render::canvas::{Canvas2d, Paint};
use crate::render::core::color::parse_rgb;
use crate::render::shared::drawing_geometry::dash_pattern;
use crate::util::js_number;

/// A pixel point.
pub(super) type Pt = (f64, f64);

/// The three fields of a drawing style a stroke reads (the original passes
/// `{ lineColor, lineWidth, lineStyle }` objects, often a spread of a style with one field
/// replaced).
#[derive(Clone, Copy)]
pub(super) struct StrokeSpec<'a> {
    pub color: &'a str,
    pub width: f64,
    pub line_style: LineStyle,
}

impl<'a> From<&'a DrawingStyle> for StrokeSpec<'a> {
    fn from(style: &'a DrawingStyle) -> Self {
        Self {
            color: &style.line_color,
            width: style.line_width,
            line_style: style.line_style,
        }
    }
}

impl<'a> StrokeSpec<'a> {
    pub fn new(color: &'a str, width: f64, line_style: LineStyle) -> Self {
        Self {
            color,
            width,
            line_style,
        }
    }

    /// The style with its color replaced (`{ ...style, lineColor }`).
    pub fn colored(style: &'a DrawingStyle, color: &'a str) -> Self {
        Self {
            color,
            width: style.line_width,
            line_style: style.line_style,
        }
    }

    /// The style with its line style replaced (`{ ...style, lineStyle }`).
    pub fn styled(style: &'a DrawingStyle, line_style: LineStyle) -> Self {
        Self {
            color: &style.line_color,
            width: style.line_width,
            line_style,
        }
    }
}

pub(super) fn set_fill(ctx: &dyn Canvas2d, color: &str) {
    ctx.set_fill_style(&Paint::from(color));
}

pub(super) fn set_stroke(ctx: &dyn Canvas2d, color: &str) {
    ctx.set_stroke_style(&Paint::from(color));
}

/// Set the stroke color, width and dash of a line style.
pub(super) fn apply_stroke(ctx: &dyn Canvas2d, spec: StrokeSpec) {
    set_stroke(ctx, spec.color);
    ctx.set_line_width(spec.width);
    ctx.set_line_dash(&dash_pattern(spec.line_style, spec.width));
}

/// Stroke a path with a line style (dash + width + color), then reset the dash.
pub(super) fn stroke_path(ctx: &dyn Canvas2d, spec: StrokeSpec, path: impl FnOnce()) {
    apply_stroke(ctx, spec);
    ctx.begin_path();
    path();
    ctx.stroke();
    ctx.set_line_dash(&[]);
}

/// Stroke a connected polyline with a line style. Skips fully transparent colors so the
/// default (hidden) band edges cost nothing.
pub(super) fn stroke_polyline(ctx: &dyn Canvas2d, pts: &[Pt], spec: StrokeSpec) {
    if pts.len() < 2 || is_transparent(spec.color) {
        return;
    }
    stroke_path(ctx, spec, || {
        ctx.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            ctx.line_to(p.0, p.1);
        }
    });
}

/// A fill color counts as set when it is non-empty (a falsy string in the original).
pub(super) fn fill_of(style: &DrawingStyle) -> Option<&str> {
    style.fill_color.as_deref().filter(|c| !c.is_empty())
}

/// The `parseFloat` of a leading run of digits and dots (`[\d.]+`).
fn parse_float_prefix(s: &str) -> f64 {
    let mut end = 0;
    let mut seen_dot = false;
    let mut digits = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            digits = true;
            end = i + 1;
        } else if c == '.' && !seen_dot {
            seen_dot = true;
            end = i + 1;
        } else {
            break;
        }
    }
    if !digits {
        return f64::NAN;
    }
    s[..end].parse().unwrap_or(f64::NAN)
}

/// True when a color is fully transparent (8-digit hex ending `00`, or an `rgba(...)` with
/// alpha 0), so hidden band edges are skipped entirely.
pub(super) fn is_transparent(color: &str) -> bool {
    let c = color.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    let c = c.to_lowercase();
    if let Some(hex) = c.strip_prefix('#')
        && hex.len() == 8
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return c.ends_with("00");
    }
    // `rgba?\([^)]*,\s*([\d.]+)\s*\)$`, matched at the leftmost position that works.
    let mut from = 0;
    while let Some(rel) = c[from..].find("rgb") {
        let start = from + rel;
        from = start + 1;
        let rest = &c[start + 3..];
        let rest = rest.strip_prefix('a').unwrap_or(rest);
        let Some(inner) = rest.strip_prefix('(') else {
            continue;
        };
        let Some(inner) = inner.strip_suffix(')') else {
            continue;
        };
        if inner.contains(')') {
            continue;
        }
        let Some((_, tail)) = inner.rsplit_once(',') else {
            continue;
        };
        let tail = tail.trim();
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            continue;
        }
        return parse_float_prefix(tail) == 0.0;
    }
    false
}

/// `#rrggbb` for any parseable color; `fallback` when it cannot be parsed.
pub(super) fn to_hex6(color: &str, fallback: &str) -> String {
    let Some(c) = parse_rgb(color) else {
        return fallback.to_string();
    };
    let h = |n: f64| {
        let v = js_max(0.0, js_min(255.0, js_round(n)));
        if v.is_nan() {
            "NaN".to_string()
        } else {
            format!("{:02x}", v as u32)
        }
    };
    format!("#{}{}{}", h(c.r), h(c.g), h(c.b))
}

/// Begin a rounded-rectangle path (arcTo-based, no `roundRect` dependency).
pub(super) fn round_rect(ctx: &dyn Canvas2d, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let rr = js_min(js_min(r, w / 2.0), h / 2.0);
    ctx.begin_path();
    ctx.move_to(x + rr, y);
    ctx.arc_to(x + w, y, x + w, y + h, rr);
    ctx.arc_to(x + w, y + h, x, y + h, rr);
    ctx.arc_to(x, y + h, x, y, rr);
    ctx.arc_to(x, y, x + w, y, rr);
    ctx.close_path();
}

/// The box edge a callout's tail leaves from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TailEdge {
    Top,
    Right,
    Bottom,
    Left,
}

/// A callout's tail: the edge it exits, where along that edge, its apex at the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Tail {
    pub edge: TailEdge,
    pub at: f64,
    pub apex: Pt,
    pub half: f64,
}

/// Trace a callout's speech-bubble outline as ONE continuous path: the rounded rectangle,
/// but on the tail's edge the path detours out to the apex and back, so the border never
/// crosses the tail's base (the pointer side stays open). Used for both fill and stroke.
pub(super) fn callout_bubble_path(
    ctx: &dyn Canvas2d,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    tail: Option<Tail>,
) {
    let seg = |edge: TailEdge, ex: f64, ey: f64| {
        if let Some(t) = tail.filter(|t| t.edge == edge) {
            let (ax, ay) = t.apex;
            match edge {
                TailEdge::Top => {
                    ctx.line_to(t.at - t.half, y);
                    ctx.line_to(ax, ay);
                    ctx.line_to(t.at + t.half, y);
                }
                TailEdge::Bottom => {
                    ctx.line_to(t.at + t.half, y + h);
                    ctx.line_to(ax, ay);
                    ctx.line_to(t.at - t.half, y + h);
                }
                TailEdge::Right => {
                    ctx.line_to(x + w, t.at - t.half);
                    ctx.line_to(ax, ay);
                    ctx.line_to(x + w, t.at + t.half);
                }
                TailEdge::Left => {
                    ctx.line_to(x, t.at + t.half);
                    ctx.line_to(ax, ay);
                    ctx.line_to(x, t.at - t.half);
                }
            }
        }
        ctx.line_to(ex, ey);
    };
    ctx.begin_path();
    ctx.move_to(x + r, y);
    seg(TailEdge::Top, x + w - r, y);
    ctx.arc_to(x + w, y, x + w, y + r, r);
    seg(TailEdge::Right, x + w, y + h - r);
    ctx.arc_to(x + w, y + h, x + w - r, y + h, r);
    seg(TailEdge::Bottom, x + r, y + h);
    ctx.arc_to(x, y + h, x, y + h - r, r);
    seg(TailEdge::Left, x, y + r);
    ctx.arc_to(x, y, x + r, y, r);
    ctx.close_path();
}

/// Space kept clear on each side of a box's label where its midline breaks around it, in px.
pub(super) const MIDLINE_LABEL_PAD: f64 = 6.0;

/// The pieces of the midline from `x1` to `x2` left once the label's `gap` is cut out of it
/// (the whole line without a gap; nothing of a side the gap swallows).
pub fn midline_segments(x1: f64, x2: f64, gap: Option<(f64, f64)>) -> Vec<(f64, f64)> {
    let Some(gap) = gap.filter(|g| !(g.1 <= x1 || g.0 >= x2)) else {
        return vec![(x1, x2)];
    };
    let mut pieces = Vec::new();
    if gap.0 > x1 {
        pieces.push((x1, gap.0));
    }
    if gap.1 < x2 {
        pieces.push((gap.1, x2));
    }
    pieces
}

/// Compact ratio text for Mach circle labels (trailing zeros trimmed).
pub(super) fn format_mach_ratio(ratio: f64) -> String {
    if !ratio.is_finite() {
        return String::new();
    }
    js_number(js_round(ratio * 1000.0) / 1000.0)
}

/// The CSS font shorthand of a text block: optional weight and slant, a size, the family.
pub(super) fn text_font(bold: bool, italic: bool, size: f64, family: &str) -> String {
    format!(
        "{}{}{}px {}",
        if bold { "bold " } else { "" },
        if italic { "italic " } else { "" },
        js_number(size),
        family
    )
}

/// `${size}px ${family}`.
pub(super) fn px_font(size: f64, family: &str) -> String {
    format!("{}px {}", js_number(size), family)
}

/// The fixed font size of a Fibonacci tool's text-size setting.
pub(super) fn fib_font_size(size: crate::drawings::types::fib::FibTextSize) -> f64 {
    use crate::drawings::types::fib::FibTextSize;
    use crate::render::shared::drawing_geometry::named_font_size;
    named_font_size(match size {
        FibTextSize::Small => BoxTextSize::Small,
        FibTextSize::Normal => BoxTextSize::Normal,
        FibTextSize::Large => BoxTextSize::Large,
        FibTextSize::Huge => BoxTextSize::Huge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_colors_are_recognised() {
        assert!(is_transparent("#12345600"));
        assert!(!is_transparent("#123456ff"));
        assert!(!is_transparent("#123456"));
        assert!(is_transparent("rgba(1, 2, 3, 0)"));
        assert!(is_transparent(" RGBA(1,2,3,0.0) "));
        assert!(!is_transparent("rgba(1,2,3,0.5)"));
        assert!(!is_transparent("rgb(1,2,3)"));
        assert!(!is_transparent("transparent"));
    }

    #[test]
    fn hex6_rounds_and_clamps() {
        assert_eq!(to_hex6("#12345680", "#888888"), "#123456");
        assert_eq!(to_hex6("rgb(300.4, 12.5, 3)", "#888888"), "#ff0d03");
        assert_eq!(to_hex6("nope", "#abc"), "#abc");
    }

    #[test]
    fn midline_breaks_around_the_label_gap() {
        assert_eq!(midline_segments(10.0, 110.0, None), vec![(10.0, 110.0)]);
        assert_eq!(
            midline_segments(10.0, 110.0, Some((34.0, 86.0))),
            vec![(10.0, 34.0), (86.0, 110.0)]
        );
        assert_eq!(
            midline_segments(10.0, 110.0, Some((0.0, 120.0))),
            Vec::<(f64, f64)>::new()
        );
        assert_eq!(
            midline_segments(10.0, 110.0, Some((120.0, 130.0))),
            vec![(10.0, 110.0)]
        );
        assert_eq!(
            midline_segments(10.0, 110.0, Some((-5.0, 40.0))),
            vec![(40.0, 110.0)]
        );
    }

    #[test]
    fn mach_ratios_trim_to_three_decimals() {
        assert_eq!(format_mach_ratio(1.0), "1");
        assert_eq!(format_mach_ratio(0.6180339), "0.618");
        assert_eq!(format_mach_ratio(f64::NAN), "");
    }
}
