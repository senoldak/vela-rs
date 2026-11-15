//! The 2D drawing surface the renderer paints on.
//!
//! The original paints through the browser's `CanvasRenderingContext2D`. Core
//! stays platform-neutral, so painters take a [`Canvas2d`] instead: the subset
//! of that API the renderer uses, method for method. `vela-web` implements it
//! on a real canvas context; [`RecordingCanvas`] records every call, which is
//! what tests assert on and what the parity check against the original
//! compares (the original is run on a JavaScript context that records the
//! same way).
//!
//! Methods take `&self`, as a context object does: a canvas is shared by the
//! layers painting on it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};

/// A fill or stroke style: a CSS color or a gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Color(String),
    LinearGradient(LinearGradient),
}

impl From<&str> for Paint {
    fn from(color: &str) -> Self {
        Paint::Color(color.to_string())
    }
}

impl From<String> for Paint {
    fn from(color: String) -> Self {
        Paint::Color(color)
    }
}

impl From<&String> for Paint {
    fn from(color: &String) -> Self {
        Paint::Color(color.clone())
    }
}

impl From<LinearGradient> for Paint {
    fn from(gradient: LinearGradient) -> Self {
        Paint::LinearGradient(gradient)
    }
}

/// `createLinearGradient(x0, y0, x1, y1)` with its color stops.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// `(offset, color)` pairs, in the order `addColorStop` was called.
    pub stops: Vec<(f64, String)>,
}

impl LinearGradient {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0,
            y0,
            x1,
            y1,
            stops: Vec::new(),
        }
    }

    /// `addColorStop(offset, color)`.
    pub fn add_color_stop(&mut self, offset: f64, color: impl Into<String>) {
        self.stops.push((offset, color.into()));
    }

    /// Builder form of [`add_color_stop`](Self::add_color_stop).
    pub fn stop(mut self, offset: f64, color: impl Into<String>) -> Self {
        self.add_color_stop(offset, color);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    /// The context default.
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
}

impl TextAlign {
    pub fn as_str(self) -> &'static str {
        match self {
            TextAlign::Start => "start",
            TextAlign::End => "end",
            TextAlign::Left => "left",
            TextAlign::Right => "right",
            TextAlign::Center => "center",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextBaseline {
    Top,
    Hanging,
    Middle,
    /// The context default.
    #[default]
    Alphabetic,
    Ideographic,
    Bottom,
}

impl TextBaseline {
    pub fn as_str(self) -> &'static str {
        match self {
            TextBaseline::Top => "top",
            TextBaseline::Hanging => "hanging",
            TextBaseline::Middle => "middle",
            TextBaseline::Alphabetic => "alphabetic",
            TextBaseline::Ideographic => "ideographic",
            TextBaseline::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    /// The context default.
    #[default]
    Butt,
    Round,
    Square,
}

impl LineCap {
    pub fn as_str(self) -> &'static str {
        match self {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    /// The context default.
    #[default]
    Miter,
    Round,
    Bevel,
}

impl LineJoin {
    pub fn as_str(self) -> &'static str {
        match self {
            LineJoin::Miter => "miter",
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
        }
    }
}

/// The fields of `TextMetrics` the renderer reads.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f64,
    pub actual_bounding_box_ascent: f64,
    pub actual_bounding_box_descent: f64,
}

/// Something `drawImage` can draw: another canvas, a decoded image. The
/// renderer treats it as opaque; a backend downcasts it to its own type.
pub type CanvasImage = Rc<dyn std::any::Any>;

/// The subset of `CanvasRenderingContext2D` the renderer uses.
///
/// Every method is the context member of the same name. Style properties the
/// original assigns (`ctx.fillStyle = …`) are `set_*` methods; the two it also
/// reads back have getters.
pub trait Canvas2d {
    // ── state ──
    fn save(&self);
    fn restore(&self);

    // ── transforms ──
    fn translate(&self, x: f64, y: f64);
    fn rotate(&self, angle: f64);
    fn scale(&self, x: f64, y: f64);
    fn set_transform(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64);

    // ── styles ──
    fn set_fill_style(&self, paint: &Paint);
    fn set_stroke_style(&self, paint: &Paint);
    fn set_global_alpha(&self, alpha: f64);
    fn global_alpha(&self) -> f64;
    fn set_line_width(&self, width: f64);
    fn line_width(&self) -> f64;
    fn set_line_dash(&self, segments: &[f64]);
    fn set_line_cap(&self, cap: LineCap);
    fn set_line_join(&self, join: LineJoin);
    fn set_font(&self, font: &str);
    fn set_text_align(&self, align: TextAlign);
    fn set_text_baseline(&self, baseline: TextBaseline);
    /// `ctx.direction`: `"ltr"`, `"rtl"` or `"inherit"`.
    fn set_direction(&self, direction: &str);

    // ── paths ──
    fn begin_path(&self);
    fn move_to(&self, x: f64, y: f64);
    fn line_to(&self, x: f64, y: f64);
    fn close_path(&self);
    fn rect(&self, x: f64, y: f64, w: f64, h: f64);
    /// `roundRect(x, y, w, h, radii)` with a single radius for every corner.
    fn round_rect(&self, x: f64, y: f64, w: f64, h: f64, radius: f64);
    fn arc(
        &self,
        x: f64,
        y: f64,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
        counterclockwise: bool,
    );
    fn arc_to(&self, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64);
    #[allow(clippy::too_many_arguments)]
    fn ellipse(
        &self,
        x: f64,
        y: f64,
        radius_x: f64,
        radius_y: f64,
        rotation: f64,
        start_angle: f64,
        end_angle: f64,
        counterclockwise: bool,
    );
    fn quadratic_curve_to(&self, cpx: f64, cpy: f64, x: f64, y: f64);
    fn bezier_curve_to(&self, cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64);

    // ── painting ──
    fn fill(&self);
    fn stroke(&self);
    fn clip(&self);
    fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64);
    fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64);
    fn clear_rect(&self, x: f64, y: f64, w: f64, h: f64);

    // ── text ──
    fn fill_text(&self, text: &str, x: f64, y: f64);
    fn stroke_text(&self, text: &str, x: f64, y: f64);
    fn measure_text(&self, text: &str) -> TextMetrics;

    // ── images ──
    /// `drawImage(image, dx, dy, dw, dh)`.
    fn draw_image(&self, image: &CanvasImage, dx: f64, dy: f64, dw: f64, dh: f64);
}

/// The font size in px of a CSS font shorthand (`"600 11px Inter"` → 11);
/// 10 (the context default) when it names none.
pub fn font_size_px(font: &str) -> f64 {
    let bytes = font.as_bytes();
    let mut end = 0;
    while let Some(at) = font[end..].find("px") {
        let stop = end + at;
        let mut start = stop;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        if start < stop
            && let Ok(size) = font[start..stop].parse::<f64>()
        {
            return size;
        }
        end = stop + 2;
    }
    10.0
}

/// The fixed text metric of [`RecordingCanvas`]: every UTF-16 unit is 0.6 em
/// wide, ascent 0.8 em, descent 0.2 em. The parity check's JavaScript
/// recorder uses the same rule, so layouts that depend on text width agree.
pub fn recorded_text_metrics(text: &str, font: &str) -> TextMetrics {
    let size = font_size_px(font);
    TextMetrics {
        width: text.encode_utf16().count() as f64 * size * 0.6,
        actual_bounding_box_ascent: size * 0.8,
        actual_bounding_box_descent: size * 0.2,
    }
}

fn paint_json(paint: &Paint) -> Value {
    match paint {
        Paint::Color(color) => json!(color),
        Paint::LinearGradient(g) => {
            json!({ "linearGradient": [g.x0, g.y0, g.x1, g.y1], "stops": g.stops.iter().map(|(o, c)| json!([o, c])).collect::<Vec<_>>() })
        }
    }
}

/// A [`Canvas2d`] that paints nothing and records every call as a JSON array
/// `[name, arg…]`, in call order. Property writes are recorded under the
/// property's JavaScript name (`fillStyle`, `lineWidth`, …).
#[derive(Default)]
pub struct RecordingCanvas {
    ops: RefCell<Vec<Value>>,
    font: RefCell<String>,
    global_alpha: Cell<f64>,
    line_width: Cell<f64>,
    /// Saved `(font, global_alpha, line_width)` per `save()`.
    stack: RefCell<Vec<(String, f64, f64)>>,
}

impl RecordingCanvas {
    pub fn new() -> Self {
        Self {
            ops: RefCell::new(Vec::new()),
            font: RefCell::new("10px sans-serif".to_string()),
            global_alpha: Cell::new(1.0),
            line_width: Cell::new(1.0),
            stack: RefCell::new(Vec::new()),
        }
    }

    fn push(&self, op: Value) {
        self.ops.borrow_mut().push(op);
    }

    /// Every recorded call so far, in order.
    pub fn ops(&self) -> Vec<Value> {
        self.ops.borrow().clone()
    }

    /// The recorded calls as one JSON array.
    pub fn to_json(&self) -> Value {
        Value::Array(self.ops())
    }

    /// The names of the recorded calls, in order.
    pub fn names(&self) -> Vec<String> {
        self.ops
            .borrow()
            .iter()
            .filter_map(|op| op.get(0).and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    /// The recorded calls named `name`.
    pub fn calls(&self, name: &str) -> Vec<Value> {
        self.ops
            .borrow()
            .iter()
            .filter(|op| op.get(0).and_then(Value::as_str) == Some(name))
            .cloned()
            .collect()
    }

    /// The text of every `fillText` call, in order.
    pub fn texts(&self) -> Vec<String> {
        self.calls("fillText")
            .iter()
            .filter_map(|op| op.get(1).and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    /// Forget what was recorded (drawing state is kept).
    pub fn clear(&self) {
        self.ops.borrow_mut().clear();
    }

    pub fn is_empty(&self) -> bool {
        self.ops.borrow().is_empty()
    }
}

impl Canvas2d for RecordingCanvas {
    fn save(&self) {
        self.stack.borrow_mut().push((
            self.font.borrow().clone(),
            self.global_alpha.get(),
            self.line_width.get(),
        ));
        self.push(json!(["save"]));
    }

    fn restore(&self) {
        if let Some((font, alpha, width)) = self.stack.borrow_mut().pop() {
            *self.font.borrow_mut() = font;
            self.global_alpha.set(alpha);
            self.line_width.set(width);
        }
        self.push(json!(["restore"]));
    }

    fn translate(&self, x: f64, y: f64) {
        self.push(json!(["translate", x, y]));
    }

    fn rotate(&self, angle: f64) {
        self.push(json!(["rotate", angle]));
    }

    fn scale(&self, x: f64, y: f64) {
        self.push(json!(["scale", x, y]));
    }

    fn set_transform(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        self.push(json!(["setTransform", a, b, c, d, e, f]));
    }

    fn set_fill_style(&self, paint: &Paint) {
        self.push(json!(["fillStyle", paint_json(paint)]));
    }

    fn set_stroke_style(&self, paint: &Paint) {
        self.push(json!(["strokeStyle", paint_json(paint)]));
    }

    fn set_global_alpha(&self, alpha: f64) {
        // A context ignores values outside 0..=1 and non-finite ones.
        if alpha.is_finite() && (0.0..=1.0).contains(&alpha) {
            self.global_alpha.set(alpha);
        }
        self.push(json!(["globalAlpha", alpha]));
    }

    fn global_alpha(&self) -> f64 {
        self.global_alpha.get()
    }

    fn set_line_width(&self, width: f64) {
        // A context ignores zero, negative and non-finite widths.
        if width.is_finite() && width > 0.0 {
            self.line_width.set(width);
        }
        self.push(json!(["lineWidth", width]));
    }

    fn line_width(&self) -> f64 {
        self.line_width.get()
    }

    fn set_line_dash(&self, segments: &[f64]) {
        self.push(json!(["setLineDash", segments]));
    }

    fn set_line_cap(&self, cap: LineCap) {
        self.push(json!(["lineCap", cap.as_str()]));
    }

    fn set_line_join(&self, join: LineJoin) {
        self.push(json!(["lineJoin", join.as_str()]));
    }

    fn set_font(&self, font: &str) {
        *self.font.borrow_mut() = font.to_string();
        self.push(json!(["font", font]));
    }

    fn set_text_align(&self, align: TextAlign) {
        self.push(json!(["textAlign", align.as_str()]));
    }

    fn set_text_baseline(&self, baseline: TextBaseline) {
        self.push(json!(["textBaseline", baseline.as_str()]));
    }

    fn set_direction(&self, direction: &str) {
        self.push(json!(["direction", direction]));
    }

    fn begin_path(&self) {
        self.push(json!(["beginPath"]));
    }

    fn move_to(&self, x: f64, y: f64) {
        self.push(json!(["moveTo", x, y]));
    }

    fn line_to(&self, x: f64, y: f64) {
        self.push(json!(["lineTo", x, y]));
    }

    fn close_path(&self) {
        self.push(json!(["closePath"]));
    }

    fn rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.push(json!(["rect", x, y, w, h]));
    }

    fn round_rect(&self, x: f64, y: f64, w: f64, h: f64, radius: f64) {
        self.push(json!(["roundRect", x, y, w, h, radius]));
    }

    fn arc(
        &self,
        x: f64,
        y: f64,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
        counterclockwise: bool,
    ) {
        self.push(json!([
            "arc",
            x,
            y,
            radius,
            start_angle,
            end_angle,
            counterclockwise
        ]));
    }

    fn arc_to(&self, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64) {
        self.push(json!(["arcTo", x1, y1, x2, y2, radius]));
    }

    fn ellipse(
        &self,
        x: f64,
        y: f64,
        radius_x: f64,
        radius_y: f64,
        rotation: f64,
        start_angle: f64,
        end_angle: f64,
        counterclockwise: bool,
    ) {
        self.push(json!([
            "ellipse",
            x,
            y,
            radius_x,
            radius_y,
            rotation,
            start_angle,
            end_angle,
            counterclockwise
        ]));
    }

    fn quadratic_curve_to(&self, cpx: f64, cpy: f64, x: f64, y: f64) {
        self.push(json!(["quadraticCurveTo", cpx, cpy, x, y]));
    }

    fn bezier_curve_to(&self, cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64) {
        self.push(json!(["bezierCurveTo", cp1x, cp1y, cp2x, cp2y, x, y]));
    }

    fn fill(&self) {
        self.push(json!(["fill"]));
    }

    fn stroke(&self) {
        self.push(json!(["stroke"]));
    }

    fn clip(&self) {
        self.push(json!(["clip"]));
    }

    fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.push(json!(["fillRect", x, y, w, h]));
    }

    fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.push(json!(["strokeRect", x, y, w, h]));
    }

    fn clear_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.push(json!(["clearRect", x, y, w, h]));
    }

    fn fill_text(&self, text: &str, x: f64, y: f64) {
        self.push(json!(["fillText", text, x, y]));
    }

    fn stroke_text(&self, text: &str, x: f64, y: f64) {
        self.push(json!(["strokeText", text, x, y]));
    }

    fn measure_text(&self, text: &str) -> TextMetrics {
        recorded_text_metrics(text, &self.font.borrow())
    }

    fn draw_image(&self, _image: &CanvasImage, dx: f64, dy: f64, dw: f64, dh: f64) {
        self.push(json!(["drawImage", dx, dy, dw, dh]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_size_is_read_from_the_shorthand() {
        assert_eq!(font_size_px("11px Inter"), 11.0);
        assert_eq!(font_size_px("600 12.5px/1.2 'Segoe UI', sans-serif"), 12.5);
        assert_eq!(font_size_px("bold italic"), 10.0);
        assert_eq!(font_size_px("px"), 10.0);
    }

    #[test]
    fn calls_are_recorded_in_order_under_their_javascript_names() {
        let c = RecordingCanvas::new();
        c.save();
        c.set_fill_style(&"#fff".into());
        c.begin_path();
        c.move_to(1.0, 2.0);
        c.line_to(3.5, 4.0);
        c.set_line_dash(&[4.0, 2.0]);
        c.stroke();
        c.fill_text("hi", 5.0, 6.0);
        c.restore();
        assert_eq!(
            c.names(),
            vec![
                "save",
                "fillStyle",
                "beginPath",
                "moveTo",
                "lineTo",
                "setLineDash",
                "stroke",
                "fillText",
                "restore"
            ]
        );
        assert_eq!(c.to_json()[4], json!(["lineTo", 3.5, 4.0]));
        assert_eq!(c.texts(), vec!["hi"]);
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn text_is_measured_by_the_fixed_rule_and_follows_save_restore() {
        let c = RecordingCanvas::new();
        c.set_font("20px Inter");
        assert_eq!(c.measure_text("abcd").width, 48.0);
        c.save();
        c.set_font("10px Inter");
        c.set_global_alpha(0.5);
        assert_eq!(c.measure_text("ab").width, 12.0);
        c.restore();
        assert_eq!(c.measure_text("ab").width, 24.0);
        assert_eq!(c.global_alpha(), 1.0);
        // An out-of-range alpha is recorded but does not change the state, as on a real context.
        c.set_global_alpha(3.0);
        assert_eq!(c.global_alpha(), 1.0);
    }

    #[test]
    fn a_gradient_records_its_line_and_stops() {
        let c = RecordingCanvas::new();
        let g = LinearGradient::new(0.0, 0.0, 0.0, 10.0)
            .stop(0.0, "#000")
            .stop(1.0, "#fff");
        c.set_fill_style(&g.into());
        assert_eq!(
            c.ops()[0],
            json!(["fillStyle", { "linearGradient": [0.0, 0.0, 0.0, 10.0], "stops": [[0.0, "#000"], [1.0, "#fff"]] }])
        );
    }
}
