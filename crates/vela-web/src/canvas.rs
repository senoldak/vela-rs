//! [`vela_core::render::canvas::Canvas2d`] on a real `CanvasRenderingContext2D`.

use js_sys::Array;
use vela_core::render::canvas::{
    Canvas2d, CanvasImage, LineCap, LineJoin, Paint, TextAlign, TextBaseline, TextMetrics,
};
use wasm_bindgen::prelude::*;
use web_sys::CanvasRenderingContext2d;

#[wasm_bindgen(inline_js = r#"
// Two context members whose bindings vary between binding-generator versions.
export function vela_round_rect(ctx, x, y, w, h, radius) {
    if (typeof ctx.roundRect === 'function') {
        ctx.roundRect(x, y, w, h, radius);
        return;
    }
    // Engines without roundRect: the same outline from arcs.
    const r = Math.max(0, Math.min(radius, Math.abs(w) / 2, Math.abs(h) / 2));
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
}

export function vela_set_direction(ctx, direction) {
    ctx.direction = direction;
}

export function vela_draw_image(ctx, image, dx, dy, dw, dh) {
    ctx.drawImage(image, dx, dy, dw, dh);
}
"#)]
extern "C" {
    fn vela_round_rect(ctx: &CanvasRenderingContext2d, x: f64, y: f64, w: f64, h: f64, radius: f64);
    fn vela_set_direction(ctx: &CanvasRenderingContext2d, direction: &str);
    fn vela_draw_image(
        ctx: &CanvasRenderingContext2d,
        image: &JsValue,
        dx: f64,
        dy: f64,
        dw: f64,
        dh: f64,
    );
}

/// A browser 2D context as core's drawing surface.
///
/// Calls that can throw in the browser (a negative radius, a non-finite
/// argument) are ignored, as an uncaught exception inside a paint would
/// otherwise abort the whole frame.
#[derive(Clone)]
pub struct BrowserCanvas {
    ctx: CanvasRenderingContext2d,
}

impl BrowserCanvas {
    pub fn new(ctx: CanvasRenderingContext2d) -> Self {
        Self { ctx }
    }

    /// The underlying context.
    pub fn context(&self) -> &CanvasRenderingContext2d {
        &self.ctx
    }

    fn apply(
        &self,
        paint: &Paint,
        set_color: impl Fn(&str),
        set_gradient: impl Fn(&web_sys::CanvasGradient),
    ) {
        match paint {
            Paint::Color(color) => set_color(color),
            Paint::LinearGradient(g) => {
                let gradient = self.ctx.create_linear_gradient(g.x0, g.y0, g.x1, g.y1);
                for (offset, color) in &g.stops {
                    // An out-of-range offset or an unparseable color throws: skip that stop.
                    let _ = gradient.add_color_stop(*offset as f32, color);
                }
                set_gradient(&gradient);
            }
        }
    }
}

impl Canvas2d for BrowserCanvas {
    fn save(&self) {
        self.ctx.save();
    }

    fn restore(&self) {
        self.ctx.restore();
    }

    fn translate(&self, x: f64, y: f64) {
        let _ = self.ctx.translate(x, y);
    }

    fn rotate(&self, angle: f64) {
        let _ = self.ctx.rotate(angle);
    }

    fn scale(&self, x: f64, y: f64) {
        let _ = self.ctx.scale(x, y);
    }

    fn set_transform(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        let _ = self.ctx.set_transform(a, b, c, d, e, f);
    }

    fn set_fill_style(&self, paint: &Paint) {
        self.apply(
            paint,
            |color| self.ctx.set_fill_style_str(color),
            |gradient| self.ctx.set_fill_style_canvas_gradient(gradient),
        );
    }

    fn set_stroke_style(&self, paint: &Paint) {
        self.apply(
            paint,
            |color| self.ctx.set_stroke_style_str(color),
            |gradient| self.ctx.set_stroke_style_canvas_gradient(gradient),
        );
    }

    fn set_global_alpha(&self, alpha: f64) {
        self.ctx.set_global_alpha(alpha);
    }

    fn global_alpha(&self) -> f64 {
        self.ctx.global_alpha()
    }

    fn set_line_width(&self, width: f64) {
        self.ctx.set_line_width(width);
    }

    fn line_width(&self) -> f64 {
        self.ctx.line_width()
    }

    fn set_line_dash(&self, segments: &[f64]) {
        let list = Array::new();
        for segment in segments {
            list.push(&JsValue::from_f64(*segment));
        }
        let _ = self.ctx.set_line_dash(&list);
    }

    fn set_line_cap(&self, cap: LineCap) {
        self.ctx.set_line_cap(cap.as_str());
    }

    fn set_line_join(&self, join: LineJoin) {
        self.ctx.set_line_join(join.as_str());
    }

    fn set_font(&self, font: &str) {
        self.ctx.set_font(font);
    }

    fn set_text_align(&self, align: TextAlign) {
        self.ctx.set_text_align(align.as_str());
    }

    fn set_text_baseline(&self, baseline: TextBaseline) {
        self.ctx.set_text_baseline(baseline.as_str());
    }

    fn set_direction(&self, direction: &str) {
        vela_set_direction(&self.ctx, direction);
    }

    fn begin_path(&self) {
        self.ctx.begin_path();
    }

    fn move_to(&self, x: f64, y: f64) {
        self.ctx.move_to(x, y);
    }

    fn line_to(&self, x: f64, y: f64) {
        self.ctx.line_to(x, y);
    }

    fn close_path(&self) {
        self.ctx.close_path();
    }

    fn rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.ctx.rect(x, y, w, h);
    }

    fn round_rect(&self, x: f64, y: f64, w: f64, h: f64, radius: f64) {
        vela_round_rect(&self.ctx, x, y, w, h, radius);
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
        let _ =
            self.ctx
                .arc_with_anticlockwise(x, y, radius, start_angle, end_angle, counterclockwise);
    }

    fn arc_to(&self, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64) {
        let _ = self.ctx.arc_to(x1, y1, x2, y2, radius);
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
        let _ = self.ctx.ellipse_with_anticlockwise(
            x,
            y,
            radius_x,
            radius_y,
            rotation,
            start_angle,
            end_angle,
            counterclockwise,
        );
    }

    fn quadratic_curve_to(&self, cpx: f64, cpy: f64, x: f64, y: f64) {
        self.ctx.quadratic_curve_to(cpx, cpy, x, y);
    }

    fn bezier_curve_to(&self, cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64) {
        self.ctx.bezier_curve_to(cp1x, cp1y, cp2x, cp2y, x, y);
    }

    fn fill(&self) {
        self.ctx.fill();
    }

    fn stroke(&self) {
        self.ctx.stroke();
    }

    fn clip(&self) {
        self.ctx.clip();
    }

    fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.ctx.fill_rect(x, y, w, h);
    }

    fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.ctx.stroke_rect(x, y, w, h);
    }

    fn clear_rect(&self, x: f64, y: f64, w: f64, h: f64) {
        self.ctx.clear_rect(x, y, w, h);
    }

    fn fill_text(&self, text: &str, x: f64, y: f64) {
        let _ = self.ctx.fill_text(text, x, y);
    }

    fn stroke_text(&self, text: &str, x: f64, y: f64) {
        let _ = self.ctx.stroke_text(text, x, y);
    }

    fn measure_text(&self, text: &str) -> TextMetrics {
        match self.ctx.measure_text(text) {
            Ok(m) => TextMetrics {
                width: m.width(),
                actual_bounding_box_ascent: m.actual_bounding_box_ascent(),
                actual_bounding_box_descent: m.actual_bounding_box_descent(),
            },
            Err(_) => TextMetrics::default(),
        }
    }

    /// The image must be a `JsValue` holding something `drawImage` accepts (a
    /// canvas, an image element, an `ImageBitmap`); anything else is ignored.
    fn draw_image(&self, image: &CanvasImage, dx: f64, dy: f64, dw: f64, dh: f64) {
        if let Some(source) = image.downcast_ref::<JsValue>() {
            vela_draw_image(&self.ctx, source, dx, dy, dw, dh);
        }
    }
}
