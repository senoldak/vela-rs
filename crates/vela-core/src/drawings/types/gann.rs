//! Gann tools. Each extends the Fibonacci base ([`FibRatios`]): its angles or
//! grid lines are editable levels, painted by the shared fib painter.

use crate::drawings::drawing::{AnchorSchema, Drawing, DrawingBase, DrawingInit, Props, Span};
use crate::drawings::geometry::{DrawingPoint, Projector};
use crate::drawings::hittest::{extend_ray, point_in_box};
use crate::drawings::level_palette::{
    FibLevel, LEVEL_AMBER, LEVEL_ANCHOR, LEVEL_AQUA, LEVEL_BLUE, LEVEL_GREEN, LEVEL_ORANGE,
    LEVEL_PURPLE, LEVEL_RED, LEVEL_TEAL, LEVEL_UNITY, LevelSpec, fib_levels,
};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::SettingsSchema;
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::drawings::types::fib::{
    FibAlign, FibCore, FibEntryLine, FibRatios, fib_drawing_common, fib_ratios_accessors,
    fib_schema, two_anchor_price_range, two_point_schema,
};
use crate::i18n::msg;
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::{DrawingExtend, LineStyle};
use crate::util::js_number;

fn y_px(base: &DrawingBase, price: f64, proj: &dyn Projector) -> Option<f64> {
    base.y_of(price, proj)
}

/// An entry with no custom label and a number text.
fn line(
    color: &str,
    seg: [f64; 4],
    number_text: String,
    number: (f64, f64, FibAlign),
    label_at: (f64, f64),
) -> FibEntryLine {
    FibEntryLine {
        color: color.to_string(),
        label: None,
        x1: seg[0],
        y1: seg[1],
        x2: seg[2],
        y2: seg[3],
        number_text,
        number_x: number.0,
        number_y: number.1,
        number_align: number.2,
        label_x: label_at.0,
        label_y: label_at.1,
    }
}

/// The text of a level: its label, else its ratio.
fn level_name(lv: &FibLevel) -> String {
    lv.label.clone().unwrap_or_else(|| js_number(lv.ratio))
}

/// The Gann angles: price-per-time ratios relative to the 1x1 (the line
/// through the second anchor). A ratio > 1 is steeper (more price per unit
/// time), < 1 shallower.
fn gann_fan_levels() -> Vec<FibLevel> {
    let spec =
        |ratio: f64, color: &str, label: &str| LevelSpec::ratio(ratio).color(color).label(label);
    fib_levels([
        spec(0.125, LEVEL_RED, "1/8"),
        spec(0.25, LEVEL_ORANGE, "1/4"),
        spec(0.333, LEVEL_AMBER, "1/3"),
        spec(0.5, LEVEL_GREEN, "1/2"),
        spec(1.0, LEVEL_UNITY, "1/1"),
        spec(2.0, LEVEL_TEAL, "2/1"),
        spec(3.0, LEVEL_BLUE, "3/1"),
        spec(4.0, LEVEL_AQUA, "4/1"),
        spec(8.0, LEVEL_PURPLE, "8/1"),
    ])
}

/// The Gann-box / Gann-square grid subdivisions, applied to BOTH the price
/// (horizontal) and time (vertical) axes.
fn grid_levels() -> Vec<FibLevel> {
    fib_levels(
        [0.0, 0.25, 0.382, 0.5, 0.618, 0.75, 1.0]
            .iter()
            .map(|&ratio| LevelSpec::ratio(ratio).label(js_number(ratio))),
    )
}

// ── fan ──

/// A Gann fan: rays from the first anchor at the Gann angles, scaled so the
/// 1x1 passes through the second anchor (which sets the price-per-time unit).
/// Each angle is an editable level (color / enabled / label) via the gear
/// panel; rendered by the shared fib painter.
#[derive(Clone, Debug)]
pub struct GannFan {
    base: DrawingBase,
    core: FibCore,
}

impl GannFan {
    pub const TYPE: &'static str = "gannfan";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(gann_fan_levels()),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl FibRatios for GannFan {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let x0 = proj.x_of(a.time);
        let y0 = y_px(&self.base, a.price, proj)?;
        let xb = proj.x_of(b.time);
        let dp = b.price - a.price; // the 1x1 rise; ratio r scales it to dp * r
        let mut out = Vec::new();
        for lv in &self.core.levels {
            if !lv.enabled {
                continue;
            }
            let Some(ty) = y_px(&self.base, a.price + dp * lv.ratio, proj) else {
                continue;
            };
            out.push(line(
                &lv.color,
                extend_ray(
                    x0,
                    y0,
                    xb,
                    ty,
                    DrawingExtend::Right,
                    proj.width(),
                    proj.height(),
                ),
                level_name(lv),
                (xb + 5.0, ty, FibAlign::Left),
                (xb + 5.0, ty),
            ));
        }
        Some(out)
    }
}

impl Drawing for GannFan {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── box ──

/// The two corners of a Gann box or square, resolved to pixels.
struct Corners {
    c1: DrawingPoint,
    c2: DrawingPoint,
    left: f64,
    right: f64,
    top: f64,
    bot: f64,
}

fn corners(base: &DrawingBase, proj: &dyn Projector) -> Option<Corners> {
    let c1 = *base.anchors.first()?;
    let c2 = *base.anchors.get(1)?;
    let xa = proj.x_of(c1.time);
    let xb = proj.x_of(c2.time);
    let ya = y_px(base, c1.price, proj)?;
    let yb = y_px(base, c2.price, proj)?;
    Some(Corners {
        c1,
        c2,
        left: js_min(xa, xb),
        right: js_max(xa, xb),
        top: js_min(ya, yb),
        bot: js_max(ya, yb),
    })
}

/// The horizontal (price, labelled at the left edge) and vertical (time,
/// unlabelled to avoid clutter) gridlines of one level.
fn grid_entries(
    base: &DrawingBase,
    lv: &FibLevel,
    g: &Corners,
    proj: &dyn Projector,
    out: &mut Vec<FibEntryLine>,
) {
    let (left, right, top, bot) = (g.left, g.right, g.top, g.bot);
    if let Some(py) = y_px(
        base,
        g.c1.price + lv.ratio * (g.c2.price - g.c1.price),
        proj,
    ) {
        out.push(line(
            &lv.color,
            [left, py, right, py],
            level_name(lv),
            (left - 5.0, py, FibAlign::Right),
            (left, py),
        ));
    }
    let vx = proj.x_of(g.c1.time + lv.ratio * (g.c2.time - g.c1.time));
    out.push(line(
        &lv.color,
        [vx, top, vx, bot],
        String::new(),
        (vx, top, FibAlign::Center),
        (vx, top),
    ));
}

/// A Gann box: a price/time grid between two corners, with horizontal +
/// vertical lines at the Gann ratios plus the two diagonals. Each ratio is an
/// editable level (color / enabled / label) via the gear panel; rendered by the
/// shared fib painter (each grid line is an entry line). Placed
/// click-move-click like the plain rectangle.
#[derive(Clone, Debug)]
pub struct GannBox {
    base: DrawingBase,
    core: FibCore,
}

impl GannBox {
    pub const TYPE: &'static str = "gannbox";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(grid_levels()),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl FibRatios for GannBox {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let g = corners(&self.base, proj)?;
        let mut out = Vec::new();
        for lv in self.core.levels.iter().filter(|l| l.enabled) {
            grid_entries(&self.base, lv, &g, proj, &mut out);
        }
        // the two diagonals (in the 1.0 level's color)
        let diag = self
            .core
            .levels
            .iter()
            .find(|l| l.ratio == 1.0)
            .map_or(LEVEL_ANCHOR, |l| l.color.as_str());
        let blank = (0.0, 0.0, FibAlign::Left);
        out.push(line(
            diag,
            [g.left, g.top, g.right, g.bot],
            String::new(),
            blank,
            (0.0, 0.0),
        ));
        out.push(line(
            diag,
            [g.left, g.bot, g.right, g.top],
            String::new(),
            blank,
            (0.0, 0.0),
        ));
        Some(out)
    }
}

impl Drawing for GannBox {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    /// Grabbable anywhere inside.
    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        corners(&self.base, proj)
            .is_some_and(|g| point_in_box(px, py, g.left, g.top, g.right, g.bot, tol))
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── square ──

/// One Gann angle of the square's fan, from the origin corner. `x` = time
/// units, `y` = price units.
struct FanAngle {
    label: &'static str,
    x: f64,
    y: f64,
    color: &'static str,
}

const FAN: [FanAngle; 5] = [
    FanAngle {
        label: "3x1",
        x: 3.0,
        y: 1.0,
        color: LEVEL_RED,
    },
    FanAngle {
        label: "2x1",
        x: 2.0,
        y: 1.0,
        color: LEVEL_ORANGE,
    },
    FanAngle {
        label: "1x1",
        x: 1.0,
        y: 1.0,
        color: LEVEL_UNITY,
    },
    FanAngle {
        label: "1x2",
        x: 1.0,
        y: 2.0,
        color: LEVEL_TEAL,
    },
    FanAngle {
        label: "1x3",
        x: 1.0,
        y: 3.0,
        color: LEVEL_BLUE,
    },
];

/// One concentric quarter-ellipse arc of the square: its radius as a fraction
/// of the box, centered on p1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GannSquareArc {
    pub k: f64,
    pub color: &'static str,
}

pub const GANN_SQUARE_ARCS: [GannSquareArc; 4] = [
    GannSquareArc {
        k: 0.25,
        color: LEVEL_RED,
    },
    GannSquareArc {
        k: 0.5,
        color: LEVEL_ORANGE,
    },
    GannSquareArc {
        k: 0.75,
        color: LEVEL_GREEN,
    },
    GannSquareArc {
        k: 1.0,
        color: LEVEL_TEAL,
    },
];

/// The origin corner and the box's pixel deltas, consumed by the arc painter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GannArcGeom {
    pub ox: f64,
    pub oy: f64,
    pub bx: f64,
    pub py: f64,
}

/// A Gann square: a price/time box with an H/V fib grid, the Gann angle fan
/// emanating from the origin corner (p1), and concentric quarter-ellipse arcs
/// centered on p1. The grid + fan are straight [`FibEntryLine`]s (the shared
/// fib painter); the arcs get a dedicated paint branch. Placed
/// click-move-click like the plain rectangle.
#[derive(Clone, Debug)]
pub struct GannSquare {
    base: DrawingBase,
    core: FibCore,
}

impl GannSquare {
    pub const TYPE: &'static str = "gannsquare";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(grid_levels()),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// The origin corner + box pixel deltas, consumed by the arc painter.
    pub fn arc_geom(&self, proj: &dyn Projector) -> Option<GannArcGeom> {
        let (ox, oy) = self.base.anchor_px(0, proj)?;
        let (ex, ey) = self.base.anchor_px(1, proj)?;
        Some(GannArcGeom {
            ox,
            oy,
            bx: ex - ox,
            py: ey - oy,
        })
    }
}

impl FibRatios for GannSquare {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let g = corners(&self.base, proj)?;
        let o = self.arc_geom(proj)?;
        let mut out = Vec::new();
        for lv in self.core.levels.iter().filter(|l| l.enabled) {
            grid_entries(&self.base, lv, &g, proj, &mut out);
        }
        for f in &FAN {
            let (u, v) = if f.x > f.y {
                // shallow: reaches the far vertical edge at fraction y/x of the height
                (o.ox + o.bx, o.oy + (f.y / f.x) * o.py)
            } else {
                // steep: reaches the far horizontal edge at fraction x/y of the width
                (o.ox + (f.x / f.y) * o.bx, o.oy + o.py)
            };
            out.push(line(
                f.color,
                [o.ox, o.oy, u, v],
                f.label.to_string(),
                (u + 4.0, v, FibAlign::Left),
                (u + 4.0, v),
            ));
        }
        Some(out)
    }
}

impl Drawing for GannSquare {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    /// Grabbable anywhere inside.
    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        corners(&self.base, proj)
            .is_some_and(|g| point_in_box(px, py, g.left, g.top, g.right, g.bot, tol))
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

fn gann_style() -> DrawingStyle {
    DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid)
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            GannFan::TYPE,
            "fibonacci",
            msg("drawing.gannfan.label", "Gann Fan"),
            svg24(r#"<path d="M4 20 20 20M4 20 20 13M4 20 20 6M4 20 15 4M4 20 9 4M4 20 4 4"/>"#),
            gann_style(),
            |init| Box::new(GannFan::new(init)),
        ),
        DrawingTypeMeta::new(
            GannBox::TYPE,
            "fibonacci",
            msg("drawing.gannbox.label", "Gann Box"),
            svg24(
                r#"<rect x="4" y="4" width="16" height="16" rx="1"/><path d="M4 12h16M12 4v16M4 4 20 20" opacity="0.6"/>"#,
            ),
            gann_style(),
            |init| Box::new(GannBox::new(init)),
        ),
        DrawingTypeMeta::new(
            GannSquare::TYPE,
            "fibonacci",
            msg("drawing.gannsquare.label", "Gann Square"),
            svg24(
                r#"<rect x="4" y="4" width="16" height="16" rx="1"/><path d="M4 20 20 4M4 20 20 12M4 20 12 4" opacity="0.7"/><path d="M4 20a16 16 0 0 1 16-16" opacity="0.5"/>"#,
            ),
            gann_style(),
            |init| Box::new(GannSquare::new(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::projector::LinearProjector;
    use crate::drawings::types::fib::test_support::{check_family, make, props, pt};

    fn down<T: 'static>(d: &dyn Drawing) -> &T {
        d.as_any().downcast_ref::<T>().expect("downcast")
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn every_type_is_registered_and_survives_sparse_anchors() {
        assert_eq!(metas().len(), 3);
        check_family(
            &metas(),
            &[
                ("gannfan", &["p1", "p2"]),
                ("gannbox", &["p1", "p2"]),
                ("gannsquare", &["p1", "p2"]),
            ],
        );
        let all = metas();
        assert_eq!(all[0].label.source_text(), "Gann Fan");
        assert!(all[2].icon.contains("opacity=\"0.5\""));
    }

    #[test]
    fn fan_rays_scale_the_one_by_one_through_the_second_anchor() {
        let d = make(
            &metas(),
            "gannfan",
            vec![pt(100.0, 100.0), pt(200.0, 125.0)],
            None,
        );
        let proj = LinearProjector::sample();
        let lines = down::<GannFan>(d.as_ref()).entry_lines(&proj).unwrap();
        assert_eq!(lines.len(), 9);
        let unity = lines.iter().find(|l| l.number_text == "1/1").unwrap();
        // The 1x1 passes through the second anchor (200, 300) and runs to the right edge.
        assert_eq!((unity.x1, unity.y1), (100.0, 400.0));
        assert!(close(unity.x2, 1002.0) && close(unity.y2, 400.0 - 902.0 * 1.0));
        assert_eq!((unity.number_x, unity.number_y), (205.0, 300.0));
        assert!(unity.label.is_none());
        assert!(lines.iter().any(|l| l.number_text == "1/3"));
        let range = d.price_range().unwrap();
        assert_eq!((range.min, range.max), (100.0, 125.0));
        assert!(d.hit_test(150.0, 350.0, &proj, 3.0));
        assert!(!d.hit_test(150.0, 100.0, &proj, 3.0));
        // A custom label is not lost when a level has none: it falls back to the ratio.
        let mut d = make(
            &metas(),
            "gannfan",
            vec![pt(100.0, 100.0), pt(200.0, 125.0)],
            None,
        );
        d.apply_setting("levels.4.label", &json!(""));
        let lines = down::<GannFan>(d.as_ref()).entry_lines(&proj).unwrap();
        assert!(lines.iter().any(|l| l.number_text == "1"));
    }

    #[test]
    fn box_grid_diagonals_and_hit_test() {
        let d = make(
            &metas(),
            "gannbox",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = LinearProjector::sample();
        let lines = down::<GannBox>(d.as_ref()).entry_lines(&proj).unwrap();
        // Seven levels, a horizontal and a vertical line each, plus two diagonals.
        assert_eq!(lines.len(), 16);
        let half = lines
            .iter()
            .find(|l| l.number_text == "0.5" && l.y1 == l.y2)
            .unwrap();
        assert!(close(half.y1, 300.0));
        assert_eq!((half.x1, half.x2), (100.0, 300.0));
        assert_eq!((half.number_x, half.number_align), (95.0, FibAlign::Right));
        let vertical = lines
            .iter()
            .find(|l| l.x1 == 200.0 && l.x2 == 200.0)
            .unwrap();
        assert_eq!((vertical.y1, vertical.y2), (200.0, 400.0));
        assert!(vertical.number_text.is_empty());
        let diagonals = &lines[14..];
        assert_eq!(diagonals[0].color, LEVEL_ANCHOR);
        assert_eq!(
            (
                diagonals[1].x1,
                diagonals[1].y1,
                diagonals[1].x2,
                diagonals[1].y2
            ),
            (100.0, 400.0, 300.0, 200.0)
        );
        // Grabbable anywhere inside, and a little outside by the tolerance.
        assert!(d.hit_test(150.0, 250.0, &proj, 3.0));
        assert!(d.hit_test(98.0, 250.0, &proj, 3.0));
        assert!(!d.hit_test(50.0, 250.0, &proj, 3.0));
        assert!(!d.hit_test(150.0, 500.0, &proj, 3.0));
        let range = d.price_range().unwrap();
        assert_eq!((range.min, range.max), (100.0, 150.0));
        assert_eq!(
            d.bounds(&proj).map(|b| (b.x, b.y, b.w, b.h)),
            Some((100.0, 200.0, 200.0, 200.0))
        );
    }

    #[test]
    fn box_diagonals_take_the_one_level_color() {
        let mut d = make(
            &metas(),
            "gannbox",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        d.apply_setting("levels.6.color", &json!("#abcdef"));
        let proj = LinearProjector::sample();
        let lines = down::<GannBox>(d.as_ref()).entry_lines(&proj).unwrap();
        assert_eq!(lines[14].color, "#abcdef");
        // Without a ratio-1 level they fall back to the anchor color.
        d.read_props(&props(
            json!({ "levels": [{ "ratio": 0.5, "color": "#111111" }] }),
        ));
        let lines = down::<GannBox>(d.as_ref()).entry_lines(&proj).unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2].color, LEVEL_ANCHOR);
    }

    #[test]
    fn square_adds_the_angle_fan_and_exposes_the_arcs() {
        let d = make(
            &metas(),
            "gannsquare",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = LinearProjector::sample();
        let square = down::<GannSquare>(d.as_ref());
        let lines = square.entry_lines(&proj).unwrap();
        assert_eq!(lines.len(), 19);
        let fan = &lines[14..];
        assert_eq!(
            fan.iter()
                .map(|l| l.number_text.as_str())
                .collect::<Vec<_>>(),
            ["3x1", "2x1", "1x1", "1x2", "1x3"]
        );
        // 3x1: shallow, reaches the far vertical edge at a third of the height.
        assert!(close(fan[0].x2, 300.0) && close(fan[0].y2, 400.0 - 200.0 / 3.0));
        assert_eq!((fan[0].x1, fan[0].y1), (100.0, 400.0));
        // 1x3: steep, reaches the far horizontal edge at a third of the width.
        assert!(close(fan[4].x2, 100.0 + 200.0 / 3.0) && close(fan[4].y2, 200.0));
        assert_eq!(
            square.arc_geom(&proj),
            Some(GannArcGeom {
                ox: 100.0,
                oy: 400.0,
                bx: 200.0,
                py: -200.0
            })
        );
        assert_eq!(GANN_SQUARE_ARCS.map(|a| a.k), [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(GANN_SQUARE_ARCS[3].color, LEVEL_TEAL);
        assert!(d.hit_test(200.0, 300.0, &proj, 3.0));
        assert!(!d.hit_test(400.0, 300.0, &proj, 3.0));
        let lone = make(&metas(), "gannsquare", vec![pt(100.0, 100.0)], None);
        assert!(down::<GannSquare>(lone.as_ref()).arc_geom(&proj).is_none());
        assert!(
            down::<GannSquare>(lone.as_ref())
                .entry_lines(&proj)
                .is_none()
        );
    }

    #[test]
    fn level_edits_round_trip_through_props() {
        let mut d = make(
            &metas(),
            "gannfan",
            vec![pt(100.0, 100.0), pt(200.0, 125.0)],
            None,
        );
        d.apply_setting("levels.0.enabled", &json!(false));
        d.apply_setting("levels.2.color", &json!("#010203"));
        d.apply_setting("levels.40.enabled", &json!(false));
        let levels = d.editable_levels().unwrap();
        assert_eq!(levels.len(), 9);
        assert!(!levels[0].enabled);
        assert_eq!(levels[2].color, "#010203");
        assert_eq!(levels[2].label.as_deref(), Some("1/3"));
        let doc = d.serialize();
        let back = make(&metas(), "gannfan", doc.anchors.clone(), doc.props.clone());
        assert_eq!(back.editable_levels(), d.editable_levels());
        let proj = LinearProjector::sample();
        assert_eq!(
            down::<GannFan>(back.as_ref())
                .entry_lines(&proj)
                .unwrap()
                .len(),
            8
        );
    }
}
