//! Closed shapes and free-form paths.

use std::any::Any;
use std::f64::consts::PI;

use serde_json::{Value, json};

use crate::drawings::drawing::{
    AnchorSchema, Bounds, Drawing, DrawingBase, DrawingInit, PlacementMode, Props, Span,
};
use crate::drawings::geometry::{DrawingPoint, Projector, SegmentGeometry};
use crate::drawings::hittest::{dist_to_polyline, dist_to_segment, point_in_box};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, SettingsField, SettingsSchema, border_fields, fill_fields,
    line_and_text_fields, line_fields, line_style_options, text_fields,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::drawings::types::lines::{two_point_pixels, two_point_price_range};
use crate::drawings::types::segment::{segment_bounds, segment_handle_points, segment_hit_test};
use crate::i18n::msg;
use crate::model::LineStyle;
use crate::palette::MARKER;

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

/// A tool struct with no state of its own beyond the shared base.
macro_rules! shape_struct {
    ($(#[$doc:meta])* $name:ident, $key:literal) => {
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
    };
}

/// Hit-test, handles, bounds and price range of a tool whose body is a
/// `geometry(proj)` (the original's `SegmentDrawing` base class).
macro_rules! segment_methods {
    () => {
        fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
            segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
        }

        fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
            segment_handle_points(&self.base, proj)
        }

        fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
            segment_bounds(&self.base, proj)
        }

        fn price_range(&self) -> Option<Span> {
            self.base.anchor_price_range()
        }
    };
}

/// A filled shape is grabbable anywhere inside; an unfilled one only near its outline.
fn has_fill(base: &DrawingBase) -> bool {
    base.style
        .fill_color
        .as_deref()
        .is_some_and(|c| !c.is_empty())
}

fn chord(a: (f64, f64), b: (f64, f64)) -> SegmentGeometry {
    SegmentGeometry {
        segments: vec![[a.0, a.1, b.0, b.1]],
        fill: None,
    }
}

/// The segments joining consecutive points.
fn polyline_segments(pts: &[(f64, f64)]) -> Vec<[f64; 4]> {
    pts.windows(2)
        .map(|w| [w[0].0, w[0].1, w[1].0, w[1].1])
        .collect()
}

fn line_style_from_str(s: &str) -> Option<LineStyle> {
    match s {
        "solid" => Some(LineStyle::Solid),
        "dashed" => Some(LineStyle::Dashed),
        "dotted" => Some(LineStyle::Dotted),
        _ => None,
    }
}

fn line_style_to_str(style: LineStyle) -> &'static str {
    match style {
        LineStyle::Solid => "solid",
        LineStyle::Dashed => "dashed",
        LineStyle::Dotted => "dotted",
    }
}

// ── box ──

/// A rectangle's own settings (round-trip through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct BoxSettings {
    /// A line at the halfway price between the top and the bottom.
    pub midline: bool,
    /// The midline's color; `None` = the border color, followed as it changes.
    pub mid_color: Option<String>,
    pub mid_style: LineStyle,
}

impl Default for BoxSettings {
    fn default() -> Self {
        Self {
            midline: false,
            mid_color: None,
            mid_style: LineStyle::Dashed,
        }
    }
}

/// A box's midline in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxMidline {
    pub x1: f64,
    pub x2: f64,
    pub y: f64,
    pub color: String,
    pub line_style: LineStyle,
}

/// A rectangle defined by two opposite corners. Optional fill + border, and an
/// optional midline at the halfway price.
///
/// Eight handles: the two anchor corners (0, 1), the other two corners (2, 3),
/// then the edge midpoints (4 to 7). Corners resize both ways; an edge handle
/// moves only its own side. (Named `BoxDrawing` here to stay clear of `std`'s `Box`.)
#[derive(Clone, Debug)]
pub struct BoxDrawing {
    base: DrawingBase,
    pub settings: BoxSettings,
}

impl BoxDrawing {
    pub const TYPE: &'static str = "box";

    pub fn new(init: &DrawingInit) -> Self {
        let mut this = Self {
            base: DrawingBase::new(init),
            settings: BoxSettings::default(),
        };
        if let Some(props) = &init.props {
            this.read_props(props);
        }
        this
    }

    /// The two corners as `(x1, y1, x2, y2)`.
    fn rect(&self, proj: &dyn Projector) -> Option<[f64; 4]> {
        two_point_pixels(&self.base, proj)
    }

    /// The midline in pixels: halfway between the box's top and bottom edges ON
    /// SCREEN (the same middle its label is centered on, so on a log scale too
    /// the line splits the box into two equal halves), across the box's full
    /// width, with its color (the border's unless one was picked) and style.
    /// `None` while the midline is off.
    pub fn midline(&self, proj: &dyn Projector) -> Option<BoxMidline> {
        if !self.settings.midline {
            return None;
        }
        let [xa, ya, xb, yb] = self.rect(proj)?;
        Some(BoxMidline {
            x1: xa.min(xb),
            x2: xa.max(xb),
            y: (ya + yb) / 2.0,
            color: self
                .settings
                .mid_color
                .clone()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| self.base.style.line_color.clone()),
            line_style: self.settings.mid_style,
        })
    }
}

impl Drawing for BoxDrawing {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["c1", "c2"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some([x1, y1, x2, y2]) = self.rect(proj) else {
            return false;
        };
        if has_fill(&self.base) && point_in_box(px, py, x1, y1, x2, y2, 0.0) {
            return true;
        }
        [
            [x1, y1, x2, y1],
            [x2, y1, x2, y2],
            [x2, y2, x1, y2],
            [x1, y2, x1, y1],
        ]
        .iter()
        .any(|e| dist_to_segment(px, py, e[0], e[1], e[2], e[3]) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        let Some([x1, y1, x2, y2]) = self.rect(proj) else {
            return Vec::new();
        };
        let mx = (x1 + x2) / 2.0;
        let my = (y1 + y2) / 2.0;
        // The anchor corners stay first: the painter and the label read the box
        // from points 0 and 1.
        vec![
            (x1, y1),
            (x2, y2),
            (x2, y1),
            (x1, y2),
            (mx, y1),
            (x2, my),
            (mx, y2),
            (x1, my),
        ]
    }

    fn drag_handle(&mut self, index: usize, pt: DrawingPoint) {
        if self.base.anchors.len() < 2 {
            return;
        }
        // Which anchor supplies each handle's x (time) and y (price); `None` =
        // that axis stays put.
        let (tx, ty) = match index {
            0 => (Some(0), Some(0)),
            1 => (Some(1), Some(1)),
            2 => (Some(1), Some(0)),
            3 => (Some(0), Some(1)),
            4 => (None, Some(0)),
            5 => (Some(1), None),
            6 => (None, Some(1)),
            7 => (Some(0), None),
            _ => (None, None),
        };
        if let Some(a) = tx.and_then(|i: usize| self.base.anchors.get_mut(i)) {
            a.time = pt.time;
        }
        if let Some(a) = ty.and_then(|i: usize| self.base.anchors.get_mut(i)) {
            a.price = pt.price;
        }
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let [x1, y1, x2, y2] = self.rect(proj)?;
        Some(Bounds {
            x: x1.min(x2),
            y: y1.min(y2),
            w: (x2 - x1).abs(),
            h: (y2 - y1).abs(),
        })
    }

    fn price_range(&self) -> Option<Span> {
        two_point_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let midline_on = |field: SettingsField| field.when("box.midline", vec![json!(true)]);
        let mut fields = border_fields();
        fields.push(
            SettingsField::new(
                "style.fillColor",
                msg("drawing.box.fillColor", "Fill color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
        );
        fields.push(
            SettingsField::new(
                "box.midline",
                msg("drawing.box.midline", "Midline"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        fields.push(midline_on(
            SettingsField::new(
                "box.midColor",
                msg("drawing.box.midlineColor", "Midline color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Line)
            .fallback("style.lineColor"),
        ));
        fields.push(midline_on(
            SettingsField::new(
                "box.midStyle",
                msg("drawing.box.midlineStyle", "Midline style"),
                FieldKind::LineStyle,
            )
            .options(line_style_options())
            .group(FieldGroup::Line),
        ));
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let s = &self.settings;
        let mut props = Props::new();
        props.insert("midline".into(), json!(s.midline));
        // An unpicked color stays absent, so the midline keeps following the border.
        if let Some(color) = s.mid_color.as_deref().filter(|c| !c.is_empty()) {
            props.insert("midColor".into(), json!(color));
        }
        props.insert("midStyle".into(), json!(line_style_to_str(s.mid_style)));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        let d = BoxSettings::default();
        self.settings = BoxSettings {
            midline: props
                .get("midline")
                .and_then(Value::as_bool)
                .unwrap_or(d.midline),
            mid_color: props
                .get("midColor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string),
            mid_style: props
                .get("midStyle")
                .and_then(Value::as_str)
                .and_then(line_style_from_str)
                .unwrap_or(d.mid_style),
        };
    }

    /// The `box.*` paths edit the typed settings; they are not mirrored in
    /// props under the same path, so the default routing would miss them.
    fn apply_setting(&mut self, path: &str, value: &Value) {
        match path {
            "box.midline" => {
                if let Some(v) = value.as_bool() {
                    self.settings.midline = v;
                }
            }
            "box.midColor" => match value {
                Value::String(s) => {
                    self.settings.mid_color = Some(s.clone()).filter(|c| !c.is_empty())
                }
                Value::Null => self.settings.mid_color = None,
                _ => {}
            },
            "box.midStyle" => {
                if let Some(style) = value.as_str().and_then(line_style_from_str) {
                    self.settings.mid_style = style;
                }
            }
            _ => crate::drawings::drawing::apply_setting_default(self, path, value),
        }
    }
}

// ── ellipse ──

/// Perimeter samples for outline hit-testing.
const PERIM_SAMPLES: usize = 48;

shape_struct!(
    /// An ellipse inscribed in the bounding box of two opposite corners. Optional fill.
    Ellipse,
    "ellipse"
);

/// An ellipse's center and radii in pixels.
struct Oval {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
}

impl Ellipse {
    fn oval(&self, proj: &dyn Projector) -> Option<Oval> {
        let [xa, ya, xb, yb] = two_point_pixels(&self.base, proj)?;
        Some(Oval {
            cx: (xa + xb) / 2.0,
            cy: (ya + yb) / 2.0,
            rx: (xb - xa).abs() / 2.0,
            ry: (yb - ya).abs() / 2.0,
        })
    }

    fn perimeter(o: &Oval) -> Vec<(f64, f64)> {
        (0..=PERIM_SAMPLES)
            .map(|i| {
                let t = (i as f64 / PERIM_SAMPLES as f64) * PI * 2.0;
                (o.cx + o.rx * t.cos(), o.cy + o.ry * t.sin())
            })
            .collect()
    }
}

impl Drawing for Ellipse {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["c1", "c2"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(o) = self.oval(proj) else {
            return false;
        };
        if o.rx < 1.0 || o.ry < 1.0 {
            return false;
        }
        let f = ((px - o.cx) / o.rx).powi(2) + ((py - o.cy) / o.ry).powi(2);
        if has_fill(&self.base) && f <= 1.0 {
            return true; // inside a filled ellipse
        }
        dist_to_polyline(px, py, &Self::perimeter(&o)) <= tol // near the outline
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        two_point_pixels(&self.base, proj)
            .map(|p| vec![(p[0], p[1]), (p[2], p[3])])
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let o = self.oval(proj)?;
        Some(Bounds {
            x: o.cx - o.rx,
            y: o.cy - o.ry,
            w: o.rx * 2.0,
            h: o.ry * 2.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        two_point_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── circle ──

shape_struct!(
    /// A true circle: a data-pinned center anchor + an edge anchor whose pixel
    /// distance from the projected center is the radius. Because the radius is
    /// a scalar pixel distance (not separate x/y radii), the shape stays
    /// perfectly round at any zoom, unlike the bounding-box [`Ellipse`], which
    /// distorts as the x/y scales diverge.
    Circle,
    "circle"
);

impl Circle {
    /// Center and radius in pixels.
    fn geom(&self, proj: &dyn Projector) -> Option<(f64, f64, f64)> {
        let [cx, cy, ex, ey] = two_point_pixels(&self.base, proj)?;
        Some((cx, cy, (ex - cx).hypot(ey - cy)))
    }
}

impl Drawing for Circle {
    drawing_identity!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["center", "edge"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some((cx, cy, r)) = self.geom(proj) else {
            return false;
        };
        if r < 1.0 {
            return false;
        }
        let d = (px - cx).hypot(py - cy);
        if has_fill(&self.base) && d <= r {
            return true; // inside a filled disc
        }
        (d - r).abs() <= tol // near the ring
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        two_point_pixels(&self.base, proj)
            .map(|p| vec![(p[0], p[1]), (p[2], p[3])])
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let (cx, cy, r) = self.geom(proj)?;
        Some(Bounds {
            x: cx - r,
            y: cy - r,
            w: r * 2.0,
            h: r * 2.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        two_point_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── triangle ──

shape_struct!(
    /// A triangle through three free vertices, with an optional fill.
    Triangle,
    "triangle"
);

impl Triangle {
    /// The edges and fill polygon; only the first edge while the third vertex
    /// is still being placed.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchor_px(0, proj)?;
        let b = self.base.anchor_px(1, proj)?;
        if self.base.anchors.len() < 3 {
            return Some(chord(a, b));
        }
        let c = self.base.anchor_px(2, proj)?;
        Some(SegmentGeometry {
            segments: vec![
                [a.0, a.1, b.0, b.1],
                [b.0, b.1, c.0, c.1],
                [c.0, c.1, a.0, a.1],
            ],
            fill: Some(vec![a, b, c]),
        })
    }
}

impl Drawing for Triangle {
    drawing_identity!();
    segment_methods!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["v1", "v2", "v3"])
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── rotated rectangle ──

shape_struct!(
    /// A rectangle at any angle: the baseline p1 to p2 sets length + rotation,
    /// and p3 sets the perpendicular thickness. The four corners are
    /// constructed in pixel space each frame (so the figure shears with the x/y
    /// scale, like every data-space drawing).
    RotatedRect,
    "rotatedrect"
);

impl RotatedRect {
    /// The four edges and fill polygon; the baseline only while placing.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchor_px(0, proj)?;
        let b = self.base.anchor_px(1, proj)?;
        if self.base.anchors.len() < 3 {
            return Some(chord(a, b)); // baseline only (placing)
        }
        let w = self.base.anchor_px(2, proj)?;
        let vx = b.0 - a.0;
        let vy = b.1 - a.1;
        let len = vx.hypot(vy);
        if len < 1e-6 {
            return Some(chord(a, b));
        }
        let nx = -vy / len; // unit perpendicular
        let ny = vx / len;
        let h = (w.0 - a.0) * nx + (w.1 - a.1) * ny; // signed perpendicular distance of the width point
        let (ox, oy) = (nx * h, ny * h);
        let c2 = (b.0 + ox, b.1 + oy);
        let c3 = (a.0 + ox, a.1 + oy);
        Some(SegmentGeometry {
            segments: vec![
                [a.0, a.1, b.0, b.1],
                [b.0, b.1, c2.0, c2.1],
                [c2.0, c2.1, c3.0, c3.1],
                [c3.0, c3.1, a.0, a.1],
            ],
            fill: Some(vec![a, b, c2, c3]),
        })
    }
}

impl Drawing for RotatedRect {
    drawing_identity!();
    segment_methods!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p1", "p2", "w"])
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = border_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── arc ──

const ARC_SAMPLES: usize = 40;

shape_struct!(
    /// A half-ellipse dome over a base chord (p1 to p2) with an apex (p3) whose
    /// height sets the bulge. The apex is pinned to the chord's horizontal
    /// midpoint, so the dome stays symmetric about the chord's perpendicular
    /// bisector.
    Arc,
    "arc"
);

impl Arc {
    fn pin_apex(&mut self) {
        let anchors = &mut self.base.anchors;
        if anchors.len() >= 3 {
            anchors[2].time = (anchors[0].time + anchors[1].time) / 2.0;
        }
    }

    /// The dome outline (closed along the chord) and its fill polygon; the
    /// chord only while placing.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchor_px(0, proj)?;
        let b = self.base.anchor_px(1, proj)?;
        let Some(apex) = self.base.anchors.get(2) else {
            return Some(chord(a, b)); // chord only (placing)
        };
        let ay = self.base.y_of(apex.price, proj)?;
        let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let (hx, hy) = ((b.0 - a.0) / 2.0, (b.1 - a.1) / 2.0); // half-chord vector
        let len = hx.hypot(hy);
        if len < 1e-6 {
            return Some(chord(a, b));
        }
        let mut nx = -hy / len; // unit normal to the chord
        let mut ny = hx / len;
        // Apex offset along the normal (apex x is pinned to mx, so its x term is 0).
        let off = (ay - my) * ny;
        if off < 0.0 {
            nx = -nx;
            ny = -ny;
        }
        let h = off.abs(); // dome height
        let pts: Vec<(f64, f64)> = (0..=ARC_SAMPLES)
            .map(|i| {
                let t = (i as f64 / ARC_SAMPLES as f64) * PI; // 0 to pi sweeps B to A over the dome
                let (s, c) = t.sin_cos();
                (mx + c * hx + s * h * nx, my + c * hy + s * h * ny)
            })
            .collect();
        let mut segments = polyline_segments(&pts);
        if let (Some(last), Some(first)) = (pts.last(), pts.first()) {
            segments.push([last.0, last.1, first.0, first.1]); // close along the chord
        }
        Some(SegmentGeometry {
            segments,
            fill: Some(pts),
        })
    }
}

impl Drawing for Arc {
    drawing_identity!();
    segment_methods!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p1", "p2", "apex"])
    }

    fn on_placed(&mut self, _proj: &dyn Projector) {
        self.pin_apex();
    }

    /// Keeps the apex on the chord's midpoint whichever handle moved.
    fn constrain_handle_drag(&mut self, _index: usize) {
        self.pin_apex();
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = border_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── curve ──

const CURVE_SAMPLES: usize = 40;

shape_struct!(
    /// A quadratic Bezier curve. p1 and p2 are the on-curve endpoints; p3 is
    /// the off-curve control handle (the curve bends toward it but does not
    /// pass through it). Sampled in pixel space, so it restretches with the
    /// endpoints on zoom.
    Curve,
    "curve"
);

impl Curve {
    /// The sampled curve and its curve-to-chord lens (only painted when a fill
    /// color is set); the straight chord while placing.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let p0 = self.base.anchor_px(0, proj)?; // start (on-curve)
        let p2 = self.base.anchor_px(1, proj)?; // end (on-curve)
        if self.base.anchors.len() < 3 {
            return Some(chord(p0, p2)); // straight chord (placing)
        }
        let c = self.base.anchor_px(2, proj)?; // control (off-curve)
        let pts: Vec<(f64, f64)> = (0..=CURVE_SAMPLES)
            .map(|i| {
                let t = i as f64 / CURVE_SAMPLES as f64;
                let u = 1.0 - t;
                (
                    u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p2.0,
                    u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p2.1,
                )
            })
            .collect();
        Some(SegmentGeometry {
            segments: polyline_segments(&pts),
            fill: Some(pts),
        })
    }
}

impl Drawing for Curve {
    drawing_identity!();
    segment_methods!();

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p1", "p2", "control"])
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(fill_fields());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── open paths ──

/// Cap on a path's point count: bounds freehand sampling and polyline clicks.
pub const MAX_PATH_POINTS: usize = 512;

/// The anchor schema shared by open multi-point paths (polyline, path,
/// freehand, highlighter): a variable count (2 up to [`MAX_PATH_POINTS`]),
/// with every vertex free on both axes.
pub fn path_anchor_schema() -> AnchorSchema {
    AnchorSchema {
        min: 2,
        max: MAX_PATH_POINTS,
        slots: Vec::new(),
    }
}

/// The run of segments between consecutive anchors (no fill); `None` with
/// fewer than two anchors or when the pane is gone.
pub fn path_geometry(base: &DrawingBase, proj: &dyn Projector) -> Option<SegmentGeometry> {
    let pts = base.anchors_px(proj)?;
    if pts.len() < 2 {
        return None;
    }
    Some(SegmentGeometry {
        segments: polyline_segments(&pts),
        fill: None,
    })
}

/// The settings schema of a plain path: line and text fields.
pub fn path_schema() -> SettingsSchema {
    SettingsSchema::new(line_and_text_fields())
}

/// The pixel bounds of a captured stroke, taken from its whole path.
pub fn brush_bounds(base: &DrawingBase, proj: &dyn Projector) -> Option<Bounds> {
    let pts: Vec<(f64, f64)> = (0..base.anchors.len())
        .filter_map(|i| base.anchor_px(i, proj))
        .collect();
    Bounds::of_points(&pts)
}

/// An open multi-point path tool (the original's `PathDrawing` subclasses).
macro_rules! path_tool {
    ($(#[$doc:meta])* $name:ident, $key:literal) => {
        shape_struct!($(#[$doc])* $name, $key);

        impl $name {
            /// The segments between consecutive vertices.
            pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
                path_geometry(&self.base, proj)
            }
        }

        impl Drawing for $name {
            drawing_identity!();
            segment_methods!();

            fn anchor_schema(&self) -> AnchorSchema {
                path_anchor_schema()
            }

            fn schema(&self) -> SettingsSchema {
                path_schema()
            }
        }
    };
}

path_tool!(
    /// A polyline: click each vertex, double-click to finish. Every vertex is an
    /// editable handle.
    Polyline,
    "polyline"
);

path_tool!(
    /// A connected multi-point polyline that ends in an arrowhead (the painter
    /// adds the head).
    Path,
    "path"
);

/// A captured freehand stroke (brush, highlighter): press, drag to capture the
/// sampled path, release to finish. A stroke has too many points to edit
/// per-vertex, so it shows no handles and moves as a whole.
macro_rules! brush_tool {
    ($(#[$doc:meta])* $name:ident, $key:literal, $schema:expr) => {
        shape_struct!($(#[$doc])* $name, $key);

        impl $name {
            /// The segments between consecutive captured points.
            pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
                path_geometry(&self.base, proj)
            }
        }

        impl Drawing for $name {
            drawing_identity!();

            fn anchor_schema(&self) -> AnchorSchema {
                path_anchor_schema()
            }

            fn placement_mode(&self) -> PlacementMode {
                PlacementMode::Freehand
            }

            fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
                segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
            }

            /// A captured stroke is moved as a whole, not edited per-point.
            fn handle_points(&self, _proj: &dyn Projector) -> Vec<(f64, f64)> {
                Vec::new()
            }

            fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
                brush_bounds(&self.base, proj)
            }

            fn price_range(&self) -> Option<Span> {
                self.base.anchor_price_range()
            }

            fn schema(&self) -> SettingsSchema {
                $schema
            }
        }
    };
}

brush_tool!(
    /// A freehand brush stroke: a solid, thin captured line in the drawing's color.
    Freehand,
    "freehand",
    path_schema()
);

brush_tool!(
    /// A highlighter stroke: a wide, translucent freehand swath (like a marker
    /// over text), painted as one round-capped path so a stroke reads as a
    /// single continuous highlight. Behaves like [`Freehand`] for
    /// placement/movement; differs only in style + paint.
    Highlighter,
    "highlighter",
    highlighter_schema()
);

/// The highlighter's editable cosmetics: a translucent marker, so it exposes
/// only color (its alpha is the highlight's opacity, set via the color picker)
/// and a wide stroke width. No dash/text: a marker is always a solid, wide,
/// see-through swath.
fn highlighter_schema() -> SettingsSchema {
    SettingsSchema::new(vec![
        SettingsField::new(
            "style.lineColor",
            msg("drawing.highlighter.lineColor", "Color"),
            FieldKind::Color,
        )
        .group(FieldGroup::Line),
        SettingsField::new(
            "style.lineWidth",
            msg("drawing.highlighter.lineWidth", "Width"),
            FieldKind::Number,
        )
        .range(4.0, 60.0, 1.0)
        .group(FieldGroup::Line),
    ])
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    let accent = DEFAULT_DRAWING_COLOR;
    let solid = |width: f64| DrawingStyle::line(accent, width, LineStyle::Solid);
    vec![
        DrawingTypeMeta::new(
            BoxDrawing::TYPE,
            "shapes",
            msg("drawing.box.label", "Rectangle"),
            svg24(r#"<rect x="4" y="6" width="16" height="12" rx="1"/>"#),
            solid(1.0).fill(format!("{accent}26")),
            |init| Box::new(BoxDrawing::new(init)),
        ),
        DrawingTypeMeta::new(
            Ellipse::TYPE,
            "shapes",
            msg("drawing.ellipse.label", "Ellipse"),
            svg24(r#"<ellipse cx="12" cy="12" rx="9" ry="6"/>"#),
            solid(2.0).fill(format!("{accent}1a")),
            |init| Box::new(Ellipse::new(init)),
        ),
        DrawingTypeMeta::new(
            Triangle::TYPE,
            "shapes",
            msg("drawing.triangle.label", "Triangle"),
            svg24(r#"<path d="M12 4 20 19 4 19Z"/>"#),
            solid(2.0).fill(format!("{accent}1a")),
            |init| Box::new(Triangle::new(init)),
        ),
        DrawingTypeMeta::new(
            Polyline::TYPE,
            "shapes",
            msg("drawing.polyline.label", "Polyline"),
            svg24(r#"<polyline points="3 17 9 10 13 14 21 5"/>"#),
            solid(2.0),
            |init| Box::new(Polyline::new(init)),
        ),
        DrawingTypeMeta::new(
            Freehand::TYPE,
            "shapes",
            msg("drawing.freehand.label", "Brush"),
            svg24(
                r#"<path d="M9.5 12 17 4.5a2.12 2.12 0 0 1 3 3L12.5 15"/><path d="M7 14a3 3 0 0 0-3 3c0 1.3-1.2 1.5-1.5 2 .8.9 2 1.5 3.5 1.5a3.5 3.5 0 0 0 3.5-3.5 3 3 0 0 0-2.5-3Z"/>"#,
            ),
            solid(2.0),
            |init| Box::new(Freehand::new(init)),
        ),
        DrawingTypeMeta::new(
            Highlighter::TYPE,
            "shapes",
            msg("drawing.highlighter.label", "Highlighter"),
            svg24(r#"<path d="M9 11 15 5l4 4-6 6H9Z"/><path d="M9 15l-2 2"/><path d="M4 21h6"/>"#),
            // A marker: wide + translucent by default (the alpha in the color
            // is the highlight's see-through).
            DrawingStyle::line(format!("{MARKER}59"), 14.0, LineStyle::Solid),
            |init| Box::new(Highlighter::new(init)),
        ),
        DrawingTypeMeta::new(
            Circle::TYPE,
            "shapes",
            msg("drawing.circle.label", "Circle"),
            svg24(r#"<circle cx="12" cy="12" r="8"/>"#),
            solid(2.0).fill(format!("{accent}1a")),
            |init| Box::new(Circle::new(init)),
        ),
        DrawingTypeMeta::new(
            RotatedRect::TYPE,
            "shapes",
            msg("drawing.rotatedrect.label", "Rotated Rectangle"),
            svg24(r#"<path d="M5 14 9.5 5 19 10 14.5 19 Z"/>"#),
            solid(2.0).fill(format!("{accent}26")),
            |init| Box::new(RotatedRect::new(init)),
        ),
        DrawingTypeMeta::new(
            Path::TYPE,
            "shapes",
            msg("drawing.path.label", "Path"),
            svg24(r#"<path d="M4 19 9 11 13 14 17 8 20 4"/><path d="M17.2 5.5 20 4 19.4 7.1"/>"#),
            solid(2.0),
            |init| Box::new(Path::new(init)),
        ),
        DrawingTypeMeta::new(
            Arc::TYPE,
            "shapes",
            msg("drawing.arc.label", "Arc"),
            svg24(r#"<path d="M4 17a8 8 0 0 1 16 0"/><path d="M4 17 20 17"/>"#),
            solid(2.0).fill(format!("{accent}26")),
            |init| Box::new(Arc::new(init)),
        ),
        DrawingTypeMeta::new(
            Curve::TYPE,
            "shapes",
            msg("drawing.curve.label", "Curve"),
            svg24(r#"<path d="M4 18Q12 1 20 14"/>"#),
            solid(2.0),
            |init| Box::new(Curve::new(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::projector::LinearProjector;

    const KEYS: [&str; 11] = [
        "box",
        "ellipse",
        "triangle",
        "circle",
        "rotatedrect",
        "arc",
        "curve",
        "polyline",
        "path",
        "freehand",
        "highlighter",
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
        d.on_placed(proj);
        for index in [0, 2, 9] {
            d.drag_handle(
                index,
                DrawingPoint {
                    time: 1.0,
                    price: 1.0,
                },
            );
            d.constrain_handle_drag(index);
        }
    }

    fn as_box(d: &dyn Drawing) -> &BoxDrawing {
        d.as_any().downcast_ref::<BoxDrawing>().expect("a box")
    }

    fn geometry_of(key: &str, px: &[(f64, f64)]) -> Option<SegmentGeometry> {
        let proj = LinearProjector::sample();
        let d = make(key, px);
        let any = d.as_any();
        match key {
            "triangle" => any.downcast_ref::<Triangle>()?.geometry(&proj),
            "rotatedrect" => any.downcast_ref::<RotatedRect>()?.geometry(&proj),
            "arc" => any.downcast_ref::<Arc>()?.geometry(&proj),
            "curve" => any.downcast_ref::<Curve>()?.geometry(&proj),
            "polyline" => any.downcast_ref::<Polyline>()?.geometry(&proj),
            "path" => any.downcast_ref::<Path>()?.geometry(&proj),
            "freehand" => any.downcast_ref::<Freehand>()?.geometry(&proj),
            "highlighter" => any.downcast_ref::<Highlighter>()?.geometry(&proj),
            _ => None,
        }
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn every_type_is_created_with_its_meta_style_and_schema() {
        assert_eq!(metas().len(), KEYS.len());
        let expect = [
            ("box", 2, 2),
            ("ellipse", 2, 2),
            ("triangle", 3, 3),
            ("circle", 2, 2),
            ("rotatedrect", 3, 3),
            ("arc", 3, 3),
            ("curve", 3, 3),
            ("polyline", 2, MAX_PATH_POINTS),
            ("path", 2, MAX_PATH_POINTS),
            ("freehand", 2, MAX_PATH_POINTS),
            ("highlighter", 2, MAX_PATH_POINTS),
        ];
        for (key, min, max) in expect {
            let d = make(key, &[]);
            assert_eq!(d.type_key(), key);
            let schema = d.anchor_schema();
            assert_eq!((schema.min, schema.max), (min, max), "{key}");
            assert_eq!(d.base().style, meta(key).default_style, "{key}");
            assert_eq!(meta(key).group, "shapes");
        }
        let roles = |key: &str| -> Vec<String> {
            make(key, &[])
                .anchor_schema()
                .slots
                .into_iter()
                .map(|s| s.role)
                .collect()
        };
        assert_eq!(roles("box"), ["c1", "c2"]);
        assert_eq!(roles("circle"), ["center", "edge"]);
        assert_eq!(roles("triangle"), ["v1", "v2", "v3"]);
        assert_eq!(roles("rotatedrect"), ["p1", "p2", "w"]);
        assert_eq!(roles("arc"), ["p1", "p2", "apex"]);
        assert_eq!(roles("curve"), ["p1", "p2", "control"]);
        assert!(roles("polyline").is_empty());
    }

    #[test]
    fn registry_entries_match_the_original() {
        let fill = |key: &str| meta(key).default_style.fill_color;
        assert_eq!(fill("box"), Some(format!("{DEFAULT_DRAWING_COLOR}26")));
        assert_eq!(fill("ellipse"), Some(format!("{DEFAULT_DRAWING_COLOR}1a")));
        assert_eq!(fill("triangle"), Some(format!("{DEFAULT_DRAWING_COLOR}1a")));
        assert_eq!(fill("circle"), Some(format!("{DEFAULT_DRAWING_COLOR}1a")));
        assert_eq!(
            fill("rotatedrect"),
            Some(format!("{DEFAULT_DRAWING_COLOR}26"))
        );
        assert_eq!(fill("arc"), Some(format!("{DEFAULT_DRAWING_COLOR}26")));
        assert_eq!(fill("curve"), None);
        assert_eq!(meta("box").default_style.line_width, 1.0);
        let marker = meta("highlighter").default_style;
        assert_eq!(marker.line_color, "#ff5d0059");
        assert_eq!(marker.line_width, 14.0);
        assert_eq!(
            meta("freehand").label,
            msg("drawing.freehand.label", "Brush")
        );
        assert_eq!(meta("box").label, msg("drawing.box.label", "Rectangle"));
        assert!(
            meta("arc")
                .icon
                .contains(r#"<path d="M4 17a8 8 0 0 1 16 0"/>"#)
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
            for count in 0..=4 {
                let px: Vec<(f64, f64)> = (0..count)
                    .map(|i| (100.0 + 60.0 * i as f64, 200.0 + 40.0 * (i % 2) as f64))
                    .collect();
                exercise(make(key, &px).as_mut(), &proj);
                let mut off_pane = make_on(key, "gone", &px);
                exercise(off_pane.as_mut(), &proj);
                assert!(!off_pane.hit_test(100.0, 200.0, &proj, 4.0), "{key}");
                assert!(off_pane.bounds(&proj).is_none(), "{key}");
            }
            let empty = make(key, &[]);
            assert!(!empty.is_complete(), "{key}");
            assert!(empty.price_range().is_none(), "{key}");
            assert!(empty.time_extent().is_none(), "{key}");
        }
    }

    #[test]
    fn serialization_round_trips_for_every_type() {
        let pts = [
            (100.0, 300.0),
            (300.0, 300.0),
            (200.0, 200.0),
            (250.0, 120.0),
        ];
        for key in KEYS {
            let d = make(key, &pts);
            let doc = d.serialize();
            assert_eq!(doc.type_key, key);
            let again = (meta(key).create)(&DrawingInit::from(doc.clone()));
            assert_eq!(again.serialize(), doc, "{key}");
            assert_eq!(doc.props.is_some(), key == "box", "{key}");
        }
    }

    #[test]
    fn box_geometry_handles_and_hits() {
        let proj = LinearProjector::sample();
        let mut d = make("box", &[(100.0, 200.0), (300.0, 400.0)]);
        assert_eq!(
            d.handle_points(&proj),
            vec![
                (100.0, 200.0),
                (300.0, 400.0),
                (300.0, 200.0),
                (100.0, 400.0),
                (200.0, 200.0),
                (300.0, 300.0),
                (200.0, 400.0),
                (100.0, 300.0),
            ]
        );
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 150.0
            })
        );
        assert_eq!(d.hit_handle(299.0, 300.0, &proj, 4.0), Some(5));
        // Filled: grabbable inside. Unfilled: only near the border.
        assert!(d.hit_test(200.0, 300.0, &proj, 4.0));
        assert!(!d.hit_test(350.0, 300.0, &proj, 4.0));
        d.base_mut().style.fill_color = None;
        assert!(!d.hit_test(200.0, 300.0, &proj, 4.0));
        assert!(d.hit_test(200.0, 203.0, &proj, 4.0));
        assert!(d.hit_test(103.0, 300.0, &proj, 4.0));
        d.base_mut().style.fill_color = Some(String::new());
        assert!(!d.hit_test(200.0, 300.0, &proj, 4.0));
    }

    #[test]
    fn box_handles_move_the_anchor_axes_they_own() {
        let mut d = make("box", &[(100.0, 200.0), (300.0, 400.0)]);
        let target = pt(150.0, 250.0);
        let anchors = |d: &dyn Drawing| (d.base().anchors[0], d.base().anchors[1]);
        let (a0, b0) = anchors(d.as_ref());

        d.drag_handle(4, target); // top-middle: only the top price
        let (a, b) = anchors(d.as_ref());
        assert_eq!((a.time, a.price), (a0.time, target.price));
        assert_eq!(b, b0);

        d.drag_handle(5, target); // right-middle: only the right time
        let (a1, b) = anchors(d.as_ref());
        assert_eq!(a1, a);
        assert_eq!((b.time, b.price), (target.time, b0.price));

        let mut d = make("box", &[(100.0, 200.0), (300.0, 400.0)]);
        d.drag_handle(2, target); // top-right: time of anchor 1, price of anchor 0
        let (a, b) = anchors(d.as_ref());
        assert_eq!(a.price, target.price);
        assert_eq!(b.time, target.time);
        assert_eq!((a.time, b.price), (a0.time, b0.price));

        d.drag_handle(12, target); // out of range: nothing moves
        assert_eq!(anchors(d.as_ref()), (a, b));
    }

    #[test]
    fn box_midline_follows_the_border_until_a_color_is_picked() {
        let proj = LinearProjector::sample();
        let mut d = make("box", &[(300.0, 400.0), (100.0, 200.0)]);
        assert_eq!(as_box(d.as_ref()).midline(&proj), None);
        d.apply_setting("box.midline", &serde_json::json!(true));
        let mid = as_box(d.as_ref()).midline(&proj).expect("midline");
        assert_eq!(
            mid,
            BoxMidline {
                x1: 100.0,
                x2: 300.0,
                y: 300.0,
                color: DEFAULT_DRAWING_COLOR.to_string(),
                line_style: LineStyle::Dashed,
            }
        );
        d.apply_setting("style.lineColor", &serde_json::json!("#123456"));
        assert_eq!(
            as_box(d.as_ref()).midline(&proj).map(|m| m.color),
            Some("#123456".into())
        );
        d.apply_setting("box.midColor", &serde_json::json!("#abcdef"));
        d.apply_setting("box.midStyle", &serde_json::json!("dotted"));
        let mid = as_box(d.as_ref()).midline(&proj).expect("midline");
        assert_eq!(
            (mid.color.as_str(), mid.line_style),
            ("#abcdef", LineStyle::Dotted)
        );
        // Wrong types and unknown styles are ignored; null clears the color.
        d.apply_setting("box.midline", &serde_json::json!("yes"));
        d.apply_setting("box.midStyle", &serde_json::json!("wavy"));
        assert!(as_box(d.as_ref()).settings.midline);
        assert_eq!(as_box(d.as_ref()).settings.mid_style, LineStyle::Dotted);
        d.apply_setting("box.midColor", &serde_json::Value::Null);
        assert_eq!(as_box(d.as_ref()).settings.mid_color, None);
    }

    #[test]
    fn box_props_round_trip_and_read_leniently() {
        let mut d = make("box", &[(100.0, 200.0), (300.0, 400.0)]);
        d.apply_setting("box.midline", &serde_json::json!(true));
        assert_eq!(
            serde_json::Value::Object(d.write_props().expect("props")),
            serde_json::json!({ "midline": true, "midStyle": "dashed" })
        );
        d.apply_setting("box.midColor", &serde_json::json!("#ff0000"));
        let props = d.write_props().expect("props");
        assert_eq!(props.get("midColor"), Some(&serde_json::json!("#ff0000")));
        let again = (meta("box").create)(&DrawingInit::on_pane("price").props(props.clone()));
        assert_eq!(again.write_props(), Some(props));

        let junk: Props = serde_json::from_value(serde_json::json!({
            "midline": "true", "midStyle": "wavy", "midColor": ""
        }))
        .expect("props");
        let lenient = (meta("box").create)(&DrawingInit::on_pane("price").props(junk));
        assert_eq!(as_box(lenient.as_ref()).settings, BoxSettings::default());
        // A key missing from existing props falls back to its default.
        let partial: Props =
            serde_json::from_value(serde_json::json!({ "midStyle": "solid" })).expect("props");
        let mut d = make("box", &[]);
        d.apply_setting("box.midline", &serde_json::json!(true));
        d.apply_props(&partial);
        let settings = &as_box(d.as_ref()).settings;
        assert!(!settings.midline);
        assert_eq!(settings.mid_style, LineStyle::Solid);
    }

    #[test]
    fn box_schema_gates_the_midline_options() {
        let schema = make("box", &[]).schema();
        let field = |path: &str| schema.fields.iter().find(|f| f.path == path).cloned();
        let color = field("box.midColor").expect("midColor");
        assert_eq!(color.fallback.as_deref(), Some("style.lineColor"));
        assert_eq!(
            color.when.as_ref().map(|w| w.path.as_str()),
            Some("box.midline")
        );
        let style = field("box.midStyle").expect("midStyle");
        assert_eq!(style.kind, FieldKind::LineStyle);
        assert_eq!(style.options, Some(line_style_options()));
        assert!(field("box.midline").is_some_and(|f| f.when.is_none()));
        assert_eq!(
            field("style.lineWidth").map(|f| f.label),
            Some(msg("drawing.field.borderWidth", "Border width"))
        );
    }

    #[test]
    fn ellipse_hits_its_outline_and_filled_inside() {
        let proj = LinearProjector::sample();
        let mut d = make("ellipse", &[(100.0, 200.0), (300.0, 400.0)]);
        assert_eq!(d.handle_points(&proj), vec![(100.0, 200.0), (300.0, 400.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 150.0
            })
        );
        assert!(d.hit_test(300.0, 300.0, &proj, 2.0)); // right edge of the outline
        assert!(d.hit_test(200.0, 300.0, &proj, 2.0)); // center, filled
        assert!(!d.hit_test(110.0, 210.0, &proj, 2.0)); // bounding-box corner, outside
        d.base_mut().style.fill_color = None;
        assert!(!d.hit_test(200.0, 300.0, &proj, 2.0));
        let flat = make("ellipse", &[(100.0, 200.0), (100.5, 400.0)]);
        assert!(!flat.hit_test(100.0, 300.0, &proj, 4.0));
    }

    #[test]
    fn circle_radius_is_the_pixel_distance_to_the_edge() {
        let proj = LinearProjector::sample();
        let mut d = make("circle", &[(200.0, 300.0), (300.0, 300.0)]);
        assert_eq!(d.handle_points(&proj), vec![(200.0, 300.0), (300.0, 300.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 125.0,
                max: 125.0
            })
        );
        assert!(d.hit_test(200.0, 202.0, &proj, 3.0)); // on the ring
        assert!(d.hit_test(210.0, 300.0, &proj, 3.0)); // inside the filled disc
        assert!(!d.hit_test(400.0, 300.0, &proj, 3.0));
        d.base_mut().style.fill_color = None;
        assert!(!d.hit_test(210.0, 300.0, &proj, 3.0));
        let tiny = make("circle", &[(200.0, 300.0), (200.5, 300.0)]);
        assert!(!tiny.hit_test(200.0, 300.0, &proj, 5.0));
    }

    #[test]
    fn triangle_geometry_closes_only_with_three_vertices() {
        let proj = LinearProjector::sample();
        let g = geometry_of(
            "triangle",
            &[(100.0, 100.0), (300.0, 100.0), (200.0, 300.0)],
        )
        .expect("geometry");
        assert_eq!(
            g.segments,
            vec![
                [100.0, 100.0, 300.0, 100.0],
                [300.0, 100.0, 200.0, 300.0],
                [200.0, 300.0, 100.0, 100.0],
            ]
        );
        assert_eq!(
            g.fill,
            Some(vec![(100.0, 100.0), (300.0, 100.0), (200.0, 300.0)])
        );
        let placing = geometry_of("triangle", &[(100.0, 100.0), (300.0, 100.0)]).expect("geometry");
        assert_eq!(placing.segments, vec![[100.0, 100.0, 300.0, 100.0]]);
        assert_eq!(placing.fill, None);
        assert!(geometry_of("triangle", &[(100.0, 100.0)]).is_none());

        let d = make(
            "triangle",
            &[(100.0, 100.0), (300.0, 100.0), (200.0, 300.0)],
        );
        assert!(d.hit_test(200.0, 150.0, &proj, 3.0)); // inside the fill
        assert!(!d.hit_test(50.0, 50.0, &proj, 3.0));
        assert_eq!(d.handle_points(&proj).len(), 3);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 100.0,
                w: 200.0,
                h: 200.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 125.0,
                max: 175.0
            })
        );
    }

    #[test]
    fn rotated_rect_offsets_the_baseline_by_the_width_point() {
        let g = geometry_of(
            "rotatedrect",
            &[(100.0, 200.0), (300.0, 200.0), (200.0, 300.0)],
        )
        .expect("geometry");
        let fill = g.fill.expect("fill");
        assert_eq!(
            fill,
            vec![
                (100.0, 200.0),
                (300.0, 200.0),
                (300.0, 300.0),
                (100.0, 300.0)
            ]
        );
        assert_eq!(g.segments.len(), 4);
        assert_eq!(g.segments[1], [300.0, 200.0, 300.0, 300.0]);
        assert_eq!(g.segments[3], [100.0, 300.0, 100.0, 200.0]);
        // A width point below or above the baseline flips the side.
        let up = geometry_of(
            "rotatedrect",
            &[(100.0, 200.0), (300.0, 200.0), (200.0, 150.0)],
        )
        .expect("geometry");
        assert_eq!(up.fill.expect("fill")[2], (300.0, 150.0));
        // Baseline only while placing, or when the baseline has no length.
        assert_eq!(
            geometry_of("rotatedrect", &[(100.0, 200.0), (300.0, 200.0)]).map(|g| g.segments.len()),
            Some(1)
        );
        let degenerate = geometry_of(
            "rotatedrect",
            &[(100.0, 200.0), (100.0, 200.0), (200.0, 300.0)],
        )
        .expect("geometry");
        assert_eq!((degenerate.segments.len(), degenerate.fill), (1, None));
    }

    #[test]
    fn arc_pins_its_apex_to_the_chord_midpoint() {
        let proj = LinearProjector::sample();
        let mut d = make("arc", &[(100.0, 300.0), (300.0, 300.0), (150.0, 200.0)]);
        d.on_placed(&proj);
        assert_eq!(d.base().anchors[2].time, 200.0 * 60_000.0);
        assert_eq!(d.base().anchors[2].price, pt(0.0, 200.0).price);
        d.drag_handle(1, pt(500.0, 300.0));
        d.constrain_handle_drag(1);
        assert_eq!(d.base().anchors[2].time, 300.0 * 60_000.0);
        // With fewer than three anchors there is nothing to pin.
        let mut short = make("arc", &[(100.0, 300.0), (300.0, 300.0)]);
        short.constrain_handle_drag(0);
        assert_eq!(short.base().anchors.len(), 2);
    }

    #[test]
    fn arc_dome_rises_toward_the_apex() {
        let g = geometry_of("arc", &[(100.0, 300.0), (300.0, 300.0), (200.0, 200.0)])
            .expect("geometry");
        let fill = g.fill.expect("fill");
        assert_eq!(fill.len(), ARC_SAMPLES + 1);
        assert!(near(fill[0], (300.0, 300.0)));
        assert!(near(fill[ARC_SAMPLES / 2], (200.0, 200.0)));
        assert!(
            (fill[ARC_SAMPLES].0 - 100.0).abs() < 1e-9
                && (fill[ARC_SAMPLES].1 - 300.0).abs() < 1e-9
        );
        assert_eq!(g.segments.len(), ARC_SAMPLES + 1); // dome + closing chord
        // An apex below the chord bulges downward.
        let down = geometry_of("arc", &[(100.0, 300.0), (300.0, 300.0), (200.0, 400.0)])
            .expect("geometry");
        assert!(near(
            down.fill.expect("fill")[ARC_SAMPLES / 2],
            (200.0, 400.0)
        ));
        // Chord only while placing.
        assert_eq!(
            geometry_of("arc", &[(100.0, 300.0), (300.0, 300.0)]).map(|g| g.segments.len()),
            Some(1)
        );
    }

    #[test]
    fn curve_samples_a_quadratic_bezier() {
        let g = geometry_of("curve", &[(100.0, 300.0), (300.0, 300.0), (200.0, 100.0)])
            .expect("geometry");
        let fill = g.fill.expect("fill");
        assert_eq!(fill.len(), CURVE_SAMPLES + 1);
        assert_eq!(g.segments.len(), CURVE_SAMPLES);
        assert!(near(fill[0], (100.0, 300.0)));
        assert!(near(fill[CURVE_SAMPLES], (300.0, 300.0)));
        assert!(near(fill[CURVE_SAMPLES / 2], (200.0, 200.0)));
        let chord = geometry_of("curve", &[(100.0, 300.0), (300.0, 300.0)]).expect("geometry");
        assert_eq!(
            (chord.segments, chord.fill),
            (vec![[100.0, 300.0, 300.0, 300.0]], None)
        );
    }

    #[test]
    fn paths_are_the_segments_between_consecutive_vertices() {
        let proj = LinearProjector::sample();
        let pts = [(100.0, 100.0), (200.0, 200.0), (300.0, 100.0)];
        for key in ["polyline", "path", "freehand", "highlighter"] {
            let g = geometry_of(key, &pts).expect("geometry");
            assert_eq!(
                g.segments,
                vec![[100.0, 100.0, 200.0, 200.0], [200.0, 200.0, 300.0, 100.0]],
                "{key}"
            );
            assert_eq!(g.fill, None, "{key}");
            assert!(geometry_of(key, &pts[..1]).is_none(), "{key}");
            let d = make(key, &pts);
            assert!(d.hit_test(150.0, 150.0, &proj, 2.0), "{key}");
            assert!(!d.hit_test(150.0, 100.0, &proj, 2.0), "{key}");
            assert_eq!(
                d.price_range(),
                Some(Span {
                    min: 150.0,
                    max: 175.0
                }),
                "{key}"
            );
            assert_eq!(
                d.bounds(&proj),
                Some(Bounds {
                    x: 100.0,
                    y: 100.0,
                    w: 200.0,
                    h: 100.0
                }),
                "{key}"
            );
        }
    }

    #[test]
    fn polyline_and_path_edit_every_vertex() {
        let proj = LinearProjector::sample();
        for key in ["polyline", "path"] {
            let mut d = make(key, &[(100.0, 100.0), (200.0, 200.0), (300.0, 100.0)]);
            assert_eq!(d.handle_points(&proj).len(), 3, "{key}");
            assert_eq!(d.hit_handle(201.0, 199.0, &proj, 4.0), Some(1), "{key}");
            assert_eq!(d.placement_mode(), PlacementMode::Click, "{key}");
            d.drag_handle(2, pt(400.0, 50.0));
            assert_eq!(d.base().anchors[2], pt(400.0, 50.0), "{key}");
            assert!(d.is_complete(), "{key}");
        }
    }

    #[test]
    fn brush_strokes_have_no_handles_and_use_freehand_placement() {
        let proj = LinearProjector::sample();
        for key in ["freehand", "highlighter"] {
            let d = make(key, &[(100.0, 100.0), (200.0, 200.0), (300.0, 100.0)]);
            assert_eq!(d.placement_mode(), PlacementMode::Freehand, "{key}");
            assert!(d.handle_points(&proj).is_empty(), "{key}");
            assert_eq!(d.hit_handle(100.0, 100.0, &proj, 4.0), None, "{key}");
        }
        assert_eq!(make("polyline", &[]).placement_mode(), PlacementMode::Click);
        // A single captured point still has bounds (a stroke in progress).
        let dot = make("freehand", &[(100.0, 100.0)]);
        assert_eq!(
            dot.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 100.0,
                w: 0.0,
                h: 0.0
            })
        );
    }

    #[test]
    fn highlighter_exposes_only_color_and_width() {
        let schema = make("highlighter", &[]).schema();
        assert_eq!(schema.fields.len(), 2);
        assert_eq!(schema.fields[0].path, "style.lineColor");
        assert_eq!(
            schema.fields[0].label,
            msg("drawing.highlighter.lineColor", "Color")
        );
        let width = &schema.fields[1];
        assert_eq!(width.path, "style.lineWidth");
        assert_eq!(
            (width.min, width.max, width.step),
            (Some(4.0), Some(60.0), Some(1.0))
        );
        assert_eq!(
            make("freehand", &[]).schema().fields.len(),
            line_and_text_fields().len()
        );
    }

    #[test]
    fn path_helpers_are_shared_and_public() {
        assert_eq!(MAX_PATH_POINTS, 512);
        let schema = path_anchor_schema();
        assert_eq!((schema.min, schema.max, schema.slots.len()), (2, 512, 0));
        let proj = LinearProjector::sample();
        let base = DrawingBase::new(&DrawingInit::on_pane("price").anchors(vec![pt(1.0, 1.0)]));
        assert!(path_geometry(&base, &proj).is_none());
        assert!(brush_bounds(&base, &proj).is_some());
    }
}
