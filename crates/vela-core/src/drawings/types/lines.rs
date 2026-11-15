//! Straight-line tools.

use std::any::Any;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Span,
};
use crate::drawings::geometry::{FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_segment, extend_ray};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, SettingsField, SettingsSchema, line_and_text_fields, line_fields,
    text_fields,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::i18n::msg;
use crate::indicators::classics::math::js_max;
use crate::model::{DrawingExtend, LineStyle};

/// The pixel segment between the first two anchors.
pub fn two_point_pixels(base: &DrawingBase, proj: &dyn Projector) -> Option<[f64; 4]> {
    let (x1, y1) = base.anchor_px(0, proj)?;
    let (x2, y2) = base.anchor_px(1, proj)?;
    Some([x1, y1, x2, y2])
}

/// The price span of the first two anchors.
pub fn two_point_price_range(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Some(Span {
        min: a.price.min(b.price),
        max: a.price.max(b.price),
    })
}

/// A two-point trend line. Both endpoints move freely on time + price; either
/// end can run on to the chart edge.
#[derive(Clone, Debug)]
pub struct TrendLine {
    base: DrawingBase,
}

impl TrendLine {
    pub const TYPE: &'static str = "trendline";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    /// Which ends run on past the anchors.
    pub fn extension(&self) -> DrawingExtend {
        let left = self.base.style.extend_left.unwrap_or(false);
        let right = self.base.style.extend_right.unwrap_or(false);
        match (left, right) {
            (true, true) => DrawingExtend::Both,
            (true, false) => DrawingExtend::Left,
            (false, true) => DrawingExtend::Right,
            (false, false) => DrawingExtend::None,
        }
    }
}

impl Drawing for TrendLine {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p1", "p2"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(p) = two_point_pixels(&self.base, proj) else {
            return false;
        };
        let [x1, y1, x2, y2] = extend_ray(
            p[0],
            p[1],
            p[2],
            p[3],
            self.extension(),
            proj.width(),
            proj.height(),
        );
        dist_to_segment(px, py, x1, y1, x2, y2) <= tol
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        two_point_pixels(&self.base, proj)
            .map(|p| vec![(p[0], p[1]), (p[2], p[3])])
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let p = two_point_pixels(&self.base, proj)?;
        Some(Bounds {
            x: p[0].min(p[2]),
            y: p[1].min(p[3]),
            w: (p[2] - p[0]).abs(),
            h: (p[3] - p[1]).abs(),
        })
    }

    fn price_range(&self) -> Option<Span> {
        two_point_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.push(
            SettingsField::new(
                "style.arrowRight",
                msg("drawing.trendline.arrowRight", "Arrow"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.push(
            SettingsField::new(
                "style.extendLeft",
                msg("drawing.trendline.extendLeft", "Extend left"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.push(
            SettingsField::new(
                "style.extendRight",
                msg("drawing.trendline.extendRight", "Extend right"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

/// A single-price horizontal line spanning the full plot width.
#[derive(Clone, Debug)]
pub struct HorizontalLine {
    base: DrawingBase,
}

impl HorizontalLine {
    pub const TYPE: &'static str = "hline";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    fn y(&self, proj: &dyn Projector) -> Option<f64> {
        self.base.y_of(self.base.anchors.first()?.price, proj)
    }
}

impl Drawing for HorizontalLine {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        // One anchor; only its price (y) matters. Its time pins the drag handle.
        AnchorSchema {
            min: 1,
            max: 1,
            slots: vec![AnchorSlot::new("p", FreeAxis::Y)],
        }
    }

    fn hit_test(&self, _px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        // Ignores x: the line spans the full width.
        self.y(proj).is_some_and(|y| (py - y).abs() <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let y = self.y(proj)?;
        Some(Bounds {
            x: 0.0,
            y: y - 1.0,
            w: proj.width(),
            h: 2.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        Some(Span {
            min: a.price,
            max: a.price,
        })
    }

    /// A horizontal line spans the full width, so it never culls by time.
    fn time_extent(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        SettingsSchema::new(line_and_text_fields())
    }
}

/// The `Drawing` methods every concrete tool spells the same way.
macro_rules! drawing_identity {
    () => {
        fn base(&self) -> &DrawingBase {
            &self.base
        }

        fn base_mut(&mut self) -> &mut DrawingBase {
            &mut self.base
        }

        fn type_key(&self) -> &'static str {
            Self::TYPE
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    };
}

/// Bounds of the first two anchors' pixel segment.
fn two_point_bounds(base: &DrawingBase, proj: &dyn Projector) -> Option<Bounds> {
    let p = two_point_pixels(base, proj)?;
    Some(Bounds {
        x: p[0].min(p[2]),
        y: p[1].min(p[3]),
        w: (p[2] - p[0]).abs(),
        h: (p[3] - p[1]).abs(),
    })
}

fn two_point_handles(base: &DrawingBase, proj: &dyn Projector) -> Vec<(f64, f64)> {
    two_point_pixels(base, proj)
        .map(|p| vec![(p[0], p[1]), (p[2], p[3])])
        .unwrap_or_default()
}

fn two_point_schema() -> AnchorSchema {
    AnchorSchema::free(&["p1", "p2"])
}

/// A two-anchor straight-line tool: both endpoints move freely on time + price
/// and the body runs on past the anchors per `$extend` (the original's
/// `TwoPointLine` base class plus `Ray`). Painting is dispatched by type, so
/// this only owns the geometry. `$extra` adds trait items a type overrides.
macro_rules! two_point_line {
    ($(#[$doc:meta])* $name:ident, $key:literal, $extend:expr, { $($extra:tt)* }) => {
        $(#[$doc])*
        #[derive(Clone, Debug)]
        pub struct $name {
            base: DrawingBase,
        }

        impl $name {
            pub const TYPE: &'static str = $key;

            pub fn new(init: &DrawingInit) -> Self {
                Self {
                    base: DrawingBase::new(init),
                }
            }
        }

        impl Drawing for $name {
            drawing_identity!();

            fn anchor_schema(&self) -> AnchorSchema {
                two_point_schema()
            }

            fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
                let Some(p) = two_point_pixels(&self.base, proj) else {
                    return false;
                };
                let [x1, y1, x2, y2] =
                    extend_ray(p[0], p[1], p[2], p[3], $extend, proj.width(), proj.height());
                dist_to_segment(px, py, x1, y1, x2, y2) <= tol
            }

            fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
                two_point_handles(&self.base, proj)
            }

            fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
                two_point_bounds(&self.base, proj)
            }

            fn price_range(&self) -> Option<Span> {
                two_point_price_range(&self.base)
            }

            fn schema(&self) -> SettingsSchema {
                SettingsSchema::new(line_and_text_fields())
            }

            $($extra)*
        }
    };
}

two_point_line!(
    /// A ray: a line through two anchors that extends to the right chart edge.
    Ray,
    "ray",
    DrawingExtend::Right,
    {}
);

two_point_line!(
    /// A straight line through two anchors, extended to both chart edges.
    ExtendedLine,
    "extendedline",
    DrawingExtend::Both,
    {
        /// Spans the full width in both directions, so it never culls by time.
        fn time_extent(&self) -> Option<Span> {
            None
        }
    }
);

two_point_line!(
    /// A trend-line segment that shows a price/percent/bars readout at its midpoint.
    InfoLine,
    "infoline",
    DrawingExtend::None,
    {}
);

two_point_line!(
    /// A trend-line segment that shows its inclination (degrees) with a baseline
    /// and arc at the start.
    TrendAngle,
    "trendangle",
    DrawingExtend::None,
    {}
);

/// A vertical line at a single time, spanning the full pane height.
#[derive(Clone, Debug)]
pub struct VerticalLine {
    base: DrawingBase,
}

impl VerticalLine {
    pub const TYPE: &'static str = "vline";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    fn x(&self, proj: &dyn Projector) -> Option<f64> {
        self.base.anchors.first().map(|a| proj.x_of(a.time))
    }
}

impl Drawing for VerticalLine {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        // One anchor; only its time (x) matters. Its price pins the drag handle.
        AnchorSchema {
            min: 1,
            max: 1,
            slots: vec![AnchorSlot::new("p", FreeAxis::X)],
        }
    }

    fn hit_test(&self, px: f64, _py: f64, proj: &dyn Projector, tol: f64) -> bool {
        // Ignores y: the line spans the full height.
        self.x(proj).is_some_and(|x| (px - x).abs() <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let x = self.x(proj)?;
        Some(Bounds {
            x: x - 1.0,
            y: 0.0,
            w: 2.0,
            h: proj.height(),
        })
    }

    /// A vertical line does not constrain the price axis.
    fn price_range(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        SettingsSchema::new(line_and_text_fields())
    }
}

/// A horizontal line from an anchor extending to the right chart edge.
#[derive(Clone, Debug)]
pub struct HorizontalRay {
    base: DrawingBase,
}

impl HorizontalRay {
    pub const TYPE: &'static str = "hray";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }
}

impl Drawing for HorizontalRay {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.base
            .anchor_px(0, proj)
            .is_some_and(|p| dist_to_segment(px, py, p.0, p.1, proj.width(), p.1) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let p = self.base.anchor_px(0, proj)?;
        Some(Bounds {
            x: p.0,
            y: p.1 - 1.0,
            w: js_max(0.0, proj.width() - p.0),
            h: 2.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        Some(Span {
            min: a.price,
            max: a.price,
        })
    }

    /// Extends right to the edge: visible whenever its anchor sits at or left
    /// of the viewport.
    fn time_extent(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        Some(Span {
            min: a.time,
            max: f64::INFINITY,
        })
    }

    fn schema(&self) -> SettingsSchema {
        SettingsSchema::new(line_and_text_fields())
    }
}

/// A full-width horizontal and full-height vertical line crossing at a single anchor.
#[derive(Clone, Debug)]
pub struct CrossLine {
    base: DrawingBase,
}

impl CrossLine {
    pub const TYPE: &'static str = "crossline";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }
}

impl Drawing for CrossLine {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        // Either arm.
        self.base
            .anchor_px(0, proj)
            .is_some_and(|p| (py - p.1).abs() <= tol || (px - p.0).abs() <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let p = self.base.anchor_px(0, proj)?;
        Some(Bounds {
            x: p.0 - 1.0,
            y: p.1 - 1.0,
            w: 2.0,
            h: 2.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        Some(Span {
            min: a.price,
            max: a.price,
        })
    }

    /// The horizontal arm spans the full width, so it never culls by time.
    fn time_extent(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        SettingsSchema::new(line_and_text_fields())
    }
}

/// A two-point arrow: a line with an arrowhead (at the end by default; start optional).
#[derive(Clone, Debug)]
pub struct Arrow {
    base: DrawingBase,
}

impl Arrow {
    pub const TYPE: &'static str = "arrow";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }
}

impl Drawing for Arrow {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        two_point_pixels(&self.base, proj)
            .is_some_and(|p| dist_to_segment(px, py, p[0], p[1], p[2], p[3]) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        two_point_handles(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        two_point_bounds(&self.base, proj)
    }

    fn price_range(&self) -> Option<Span> {
        two_point_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.push(
            SettingsField::new(
                "style.arrowRight",
                msg("drawing.arrow.arrowRight", "Head (end)"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.push(
            SettingsField::new(
                "style.arrowLeft",
                msg("drawing.arrow.arrowLeft", "Head (start)"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    let solid = |width: f64| DrawingStyle::line(DEFAULT_DRAWING_COLOR, width, LineStyle::Solid);
    let mut arrow_style = solid(2.0);
    arrow_style.arrow_right = Some(true);
    vec![
        DrawingTypeMeta::new(
            TrendLine::TYPE,
            "lines",
            msg("drawing.trendline.label", "Trend Line"),
            svg24(r#"<line x1="4" y1="19" x2="20" y2="5"/>"#),
            solid(2.0),
            |init| Box::new(TrendLine::new(init)),
        ),
        DrawingTypeMeta::new(
            HorizontalLine::TYPE,
            "lines",
            msg("drawing.hline.label", "Horizontal Line"),
            svg24(r#"<line x1="3" y1="12" x2="21" y2="12"/>"#),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 2.0, LineStyle::Dashed),
            |init| Box::new(HorizontalLine::new(init)),
        ),
        DrawingTypeMeta::new(
            Ray::TYPE,
            "lines",
            msg("drawing.ray.label", "Ray"),
            svg24(
                r#"<line x1="3" y1="20" x2="21" y2="6"/><circle cx="3" cy="20" r="1.6" fill="currentColor"/>"#,
            ),
            solid(2.0),
            |init| Box::new(Ray::new(init)),
        ),
        DrawingTypeMeta::new(
            ExtendedLine::TYPE,
            "lines",
            msg("drawing.extendedline.label", "Extended Line"),
            svg24(
                r#"<path d="M5 19 19 5"/><path d="M5 19 8.5 19M5 19 5 15.5"/><path d="M19 5 15.5 5M19 5 19 8.5"/>"#,
            ),
            solid(2.0),
            |init| Box::new(ExtendedLine::new(init)),
        ),
        DrawingTypeMeta::new(
            VerticalLine::TYPE,
            "lines",
            msg("drawing.vline.label", "Vertical Line"),
            svg24(r#"<line x1="12" y1="3" x2="12" y2="21"/>"#),
            solid(2.0),
            |init| Box::new(VerticalLine::new(init)),
        ),
        DrawingTypeMeta::new(
            HorizontalRay::TYPE,
            "lines",
            msg("drawing.hray.label", "Horizontal Ray"),
            svg24(
                r#"<line x1="5" y1="12" x2="21" y2="12"/><circle cx="5" cy="12" r="1.6" fill="currentColor"/>"#,
            ),
            solid(2.0),
            |init| Box::new(HorizontalRay::new(init)),
        ),
        DrawingTypeMeta::new(
            CrossLine::TYPE,
            "lines",
            msg("drawing.crossline.label", "Cross Line"),
            svg24(
                r#"<line x1="3" y1="12" x2="21" y2="12"/><line x1="12" y1="3" x2="12" y2="21"/>"#,
            ),
            solid(1.0),
            |init| Box::new(CrossLine::new(init)),
        ),
        DrawingTypeMeta::new(
            InfoLine::TYPE,
            "lines",
            msg("drawing.infoline.label", "Info Line"),
            svg24(r#"<path d="M4 20 20 4"/><rect x="9" y="9" width="9" height="6" rx="1"/>"#),
            solid(2.0),
            |init| Box::new(InfoLine::new(init)),
        ),
        DrawingTypeMeta::new(
            TrendAngle::TYPE,
            "lines",
            msg("drawing.trendangle.label", "Trend Angle"),
            svg24(
                r#"<path d="M4 20 20 6"/><path d="M4 20 15 20"/><path d="M11 20a7 7 0 0 0-2-4.9"/>"#,
            ),
            solid(2.0),
            |init| Box::new(TrendAngle::new(init)),
        ),
        DrawingTypeMeta::new(
            Arrow::TYPE,
            "shapes",
            msg("drawing.arrow.label", "Arrow"),
            svg24(r#"<path d="M7 7h10v10"/><path d="M7 17 17 7"/>"#),
            arrow_style,
            |init| Box::new(Arrow::new(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::projector::LinearProjector;

    const KEYS: [&str; 10] = [
        "trendline",
        "hline",
        "ray",
        "extendedline",
        "vline",
        "hray",
        "crossline",
        "infoline",
        "trendangle",
        "arrow",
    ];

    /// The data point under pixel (x, y) of the sample projector.
    fn pt(x: f64, y: f64) -> DrawingPoint {
        DrawingPoint {
            time: x * 60_000.0,
            price: 200.0 - y * 0.25,
        }
    }

    fn meta(key: &str) -> DrawingTypeMeta {
        metas()
            .into_iter()
            .find(|m| m.type_key == key)
            .expect("registered type")
    }

    fn make_on(key: &str, pane: &str, px: &[(f64, f64)]) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init =
            DrawingInit::on_pane(pane).anchors(px.iter().map(|p| pt(p.0, p.1)).collect());
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    fn make(key: &str, px: &[(f64, f64)]) -> Box<dyn Drawing> {
        make_on(key, "price", px)
    }

    /// Every trait method, called without panicking.
    fn exercise(d: &mut dyn Drawing, proj: &LinearProjector) {
        d.hit_test(10.0, 10.0, proj, 4.0);
        d.handle_points(proj);
        d.hit_handle(10.0, 10.0, proj, 4.0);
        d.bounds(proj);
        d.price_range();
        d.time_extent();
        d.schema();
        d.write_props();
        d.serialize();
        d.is_complete();
        d.drag_handle(
            0,
            DrawingPoint {
                time: 1.0,
                price: 1.0,
            },
        );
        d.drag_handle(
            9,
            DrawingPoint {
                time: 1.0,
                price: 1.0,
            },
        );
    }

    #[test]
    fn every_type_is_created_with_its_meta_style_and_schema() {
        let all = metas();
        assert_eq!(all.len(), KEYS.len());
        let expect = [
            ("trendline", 2),
            ("hline", 1),
            ("ray", 2),
            ("extendedline", 2),
            ("vline", 1),
            ("hray", 1),
            ("crossline", 1),
            ("infoline", 2),
            ("trendangle", 2),
            ("arrow", 2),
        ];
        for (key, count) in expect {
            let d = make(key, &[]);
            assert_eq!(d.type_key(), key);
            let schema = d.anchor_schema();
            assert_eq!((schema.min, schema.max), (count, count), "{key}");
            assert_eq!(d.base().style, meta(key).default_style, "{key}");
        }
        let roles = |key: &str| -> Vec<String> {
            make(key, &[])
                .anchor_schema()
                .slots
                .into_iter()
                .map(|s| s.role)
                .collect()
        };
        assert_eq!(roles("ray"), ["p1", "p2"]);
        assert_eq!(roles("vline"), ["p"]);
        assert_eq!(roles("crossline"), ["p"]);
        assert_eq!(roles("hray"), ["p"]);
    }

    #[test]
    fn registry_entries_match_the_original() {
        assert_eq!(meta("hline").default_style.line_style, LineStyle::Dashed);
        assert_eq!(meta("crossline").default_style.line_width, 1.0);
        assert_eq!(meta("arrow").default_style.arrow_right, Some(true));
        assert_eq!(meta("arrow").group, "shapes");
        assert_eq!(meta("ray").group, "lines");
        assert_eq!(
            meta("infoline").label,
            msg("drawing.infoline.label", "Info Line")
        );
        assert!(
            meta("vline")
                .icon
                .contains(r#"<line x1="12" y1="3" x2="12" y2="21"/>"#)
        );
        for m in metas() {
            assert!(
                !m.covers_series && m.placement_hint.is_none(),
                "{}",
                m.type_key
            );
        }
    }

    #[test]
    fn drawings_survive_missing_anchors_and_a_missing_pane() {
        let proj = LinearProjector::sample();
        for key in KEYS {
            for count in 0..=3 {
                let px: Vec<(f64, f64)> = (0..count)
                    .map(|i| (100.0 + 50.0 * i as f64, 200.0))
                    .collect();
                let mut on_pane = make(key, &px);
                exercise(on_pane.as_mut(), &proj);
                let mut off_pane = make_on(key, "gone", &px);
                exercise(off_pane.as_mut(), &proj);
                if key != "vline" {
                    // A vertical line needs no pane: only its time places it.
                    assert!(!off_pane.hit_test(100.0, 200.0, &proj, 4.0), "{key}");
                    assert!(off_pane.bounds(&proj).is_none(), "{key}");
                }
            }
            let empty = make(key, &[]);
            assert!(!empty.is_complete(), "{key}");
            assert!(empty.price_range().is_none(), "{key}");
        }
    }

    #[test]
    fn serialization_round_trips_for_every_type() {
        for key in KEYS {
            let d = make(key, &[(100.0, 200.0), (300.0, 400.0)]);
            let doc = d.serialize();
            assert_eq!(doc.type_key, key);
            let again = (meta(key).create)(&DrawingInit::from(doc.clone()));
            assert_eq!(again.serialize(), doc, "{key}");
            assert!(doc.props.is_none(), "{key}");
        }
    }

    fn sample_line(key: &str) -> Box<dyn Drawing> {
        make(key, &[(100.0, 200.0), (300.0, 400.0)])
    }

    #[test]
    fn two_point_tools_share_handles_bounds_and_price_range() {
        let proj = LinearProjector::sample();
        for key in [
            "trendline",
            "ray",
            "extendedline",
            "infoline",
            "trendangle",
            "arrow",
        ] {
            let d = sample_line(key);
            assert_eq!(
                d.handle_points(&proj),
                vec![(100.0, 200.0), (300.0, 400.0)],
                "{key}"
            );
            assert_eq!(
                d.bounds(&proj),
                Some(Bounds {
                    x: 100.0,
                    y: 200.0,
                    w: 200.0,
                    h: 200.0
                }),
                "{key}"
            );
            assert_eq!(
                d.price_range(),
                Some(Span {
                    min: 100.0,
                    max: 150.0
                }),
                "{key}"
            );
            assert_eq!(d.hit_handle(103.0, 203.0, &proj, 4.0), Some(0), "{key}");
            assert_eq!(d.hit_handle(299.0, 401.0, &proj, 4.0), Some(1), "{key}");
            assert_eq!(d.hit_handle(200.0, 300.0, &proj, 4.0), None, "{key}");
            // On the segment, near it, and off it.
            assert!(d.hit_test(200.0, 300.0, &proj, 4.0), "{key}");
            assert!(d.hit_test(200.0, 303.0, &proj, 4.0), "{key}");
            assert!(!d.hit_test(200.0, 320.0, &proj, 4.0), "{key}");
        }
    }

    #[test]
    fn rays_and_extended_lines_hit_past_their_anchors() {
        let proj = LinearProjector::sample();
        // The line y = x + 100 continues to x = 500 on the right and to x = 0 on the left.
        let ray = sample_line("ray");
        assert!(ray.hit_test(500.0, 600.0, &proj, 2.0));
        assert!(!ray.hit_test(50.0, 150.0, &proj, 2.0));
        let ext = sample_line("extendedline");
        assert!(ext.hit_test(500.0, 600.0, &proj, 2.0));
        assert!(ext.hit_test(50.0, 150.0, &proj, 2.0));
        let plain = sample_line("infoline");
        assert!(!plain.hit_test(500.0, 600.0, &proj, 2.0));
        assert!(!plain.hit_test(50.0, 150.0, &proj, 2.0));
        let arrow = sample_line("arrow");
        assert!(!arrow.hit_test(500.0, 600.0, &proj, 2.0));
    }

    #[test]
    fn time_extents_follow_each_type() {
        assert_eq!(sample_line("extendedline").time_extent(), None);
        assert_eq!(
            sample_line("ray").time_extent(),
            Some(Span {
                min: 100.0 * 60_000.0,
                max: 300.0 * 60_000.0
            })
        );
        let hray = make("hray", &[(100.0, 200.0)]);
        assert_eq!(
            hray.time_extent(),
            Some(Span {
                min: 100.0 * 60_000.0,
                max: f64::INFINITY
            })
        );
        assert_eq!(make("crossline", &[(100.0, 200.0)]).time_extent(), None);
        assert!(make("vline", &[(100.0, 200.0)]).time_extent().is_some());
        assert!(make("hray", &[]).time_extent().is_none());
    }

    #[test]
    fn vertical_line_spans_the_height_and_pins_to_time() {
        let proj = LinearProjector::sample();
        let d = make("vline", &[(100.0, 200.0)]);
        assert!(d.hit_test(102.0, 5.0, &proj, 3.0));
        assert!(d.hit_test(100.0, 590.0, &proj, 3.0));
        assert!(!d.hit_test(110.0, 200.0, &proj, 3.0));
        assert_eq!(d.handle_points(&proj), vec![(100.0, 200.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 99.0,
                y: 0.0,
                w: 2.0,
                h: 600.0
            })
        );
        assert_eq!(d.price_range(), None);
        assert_eq!(d.anchor_schema().slots[0].free, FreeAxis::X);
        // The handle moves along time only.
        let mut d = d;
        d.drag_handle(0, pt(150.0, 50.0));
        let a = d.base().anchors[0];
        assert_eq!(a.time, 150.0 * 60_000.0);
        assert_eq!(a.price, 150.0);
    }

    #[test]
    fn horizontal_ray_runs_right_from_its_anchor() {
        let proj = LinearProjector::sample();
        let d = make("hray", &[(100.0, 200.0)]);
        assert!(d.hit_test(700.0, 202.0, &proj, 3.0));
        assert!(!d.hit_test(50.0, 200.0, &proj, 3.0));
        assert!(!d.hit_test(700.0, 220.0, &proj, 3.0));
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 199.0,
                w: 900.0,
                h: 2.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 150.0,
                max: 150.0
            })
        );
        // An anchor right of the plot never gives a negative width.
        let far = make("hray", &[(1200.0, 200.0)]);
        assert_eq!(far.bounds(&proj).map(|b| b.w), Some(0.0));
    }

    #[test]
    fn cross_line_hits_either_arm() {
        let proj = LinearProjector::sample();
        let d = make("crossline", &[(100.0, 200.0)]);
        assert!(d.hit_test(900.0, 201.0, &proj, 3.0));
        assert!(d.hit_test(101.0, 500.0, &proj, 3.0));
        assert!(!d.hit_test(150.0, 250.0, &proj, 3.0));
        assert_eq!(d.handle_points(&proj), vec![(100.0, 200.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 99.0,
                y: 199.0,
                w: 2.0,
                h: 2.0
            })
        );
    }

    #[test]
    fn arrow_schema_offers_both_heads() {
        let schema = make("arrow", &[]).schema();
        let paths: Vec<&str> = schema.fields.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"style.arrowRight"));
        assert!(paths.contains(&"style.arrowLeft"));
        let right = schema
            .fields
            .iter()
            .find(|f| f.path == "style.arrowRight")
            .map(|f| f.label.clone());
        assert_eq!(right, Some(msg("drawing.arrow.arrowRight", "Head (end)")));
        // Plain two-point tools carry only line and text fields.
        let plain = make("infoline", &[]).schema();
        assert!(plain.fields.iter().all(|f| !f.path.contains("arrow")));
        assert_eq!(plain.fields.len(), line_and_text_fields().len());
    }

    #[test]
    fn settings_apply_through_the_default_path() {
        let mut d = make("arrow", &[]);
        d.apply_setting("style.arrowLeft", &serde_json::json!(true));
        d.apply_setting("style.lineWidth", &serde_json::json!(5));
        assert_eq!(d.base().style.arrow_left, Some(true));
        assert_eq!(d.base().style.line_width, 5.0);
    }
}
