//! The single seam between the renderer and a drawing backend. The backend consumes the
//! retained scene and the shared coordinate system and rasterizes the data layer
//! (series, fills, bgcolor, hline, drawing geometry) onto the data canvas. Two
//! implementations sit behind it: the canvas2d backend (primary) and the WebGL2 backend
//! (hand-rolled, selected when available).
//!
//! The crosshair is NOT the backend's concern: it is a renderer-owned overlay layer on its
//! own canvas, repainted on the scheduler's cheap cursor tier without touching the data
//! layer. Neither is the grid: it lives with the session highlights on the renderer-owned
//! backdrop canvas at the very bottom of the pile, so SDK layer canvases stacked below the
//! data canvas still paint above it.
//!
//! The original's `mount(canvas: HTMLCanvasElement)` binds the backend to the (already
//! DOM-sized) data canvas. What a backend binds to differs by kind (a 2d context for
//! canvas2d, a GL context for WebGL2), so binding is not part of the trait: a concrete
//! backend takes its surface when it is constructed (a canvas2d backend paints through a
//! [`Canvas2d`](crate::render::canvas::Canvas2d)).

use crate::ports::chart_renderer::VelaTheme;
use crate::render::core::{CoordinateSystem, SceneGraph};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Canvas2d,
    WebGl2,
}

impl BackendKind {
    /// The kind's name as the original spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Canvas2d => "canvas2d",
            BackendKind::WebGl2 => "webgl2",
        }
    }
}

pub trait RenderBackend {
    fn kind(&self) -> BackendKind;

    /// Opacity multiplier for indicator models (series, fills, bgcolor, hline); candles,
    /// grid and axes stay opaque. The renderer drives it to fade indicators in after the
    /// intro.
    fn model_alpha(&self) -> f64;
    fn set_model_alpha(&mut self, alpha: f64);

    /// Opacity of the candle BODY fill (and non-candle price series). 1 normally; the
    /// renderer fades it toward transparent as a reveal-under layer opens on zoom-in.
    fn candle_body_alpha(&self) -> f64;
    fn set_candle_body_alpha(&mut self, alpha: f64);

    /// Opacity of the candle STRUCTURE (wicks and body border). 1 normally; fades only
    /// partway on zoom-in so the candle keeps a visible skeleton over the revealed layer.
    fn candle_structure_alpha(&self) -> f64;
    fn set_candle_structure_alpha(&mut self, alpha: f64);

    /// Multiplier on the candle BODY width. 1 normally; the renderer shrinks it as a
    /// side-positioned reveal layer opens, freeing the inter-candle gap for it.
    fn candle_body_scale(&self) -> f64;
    fn set_candle_body_scale(&mut self, scale: f64);

    /// Paint the data layer for the current frame.
    fn render(&mut self, scene: &SceneGraph, coords: &CoordinateSystem, theme: &VelaTheme);

    fn destroy(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Recording {
        model_alpha: f64,
        body_alpha: f64,
        structure_alpha: f64,
        body_scale: f64,
        frames: usize,
        destroyed: bool,
    }

    impl RenderBackend for Recording {
        fn kind(&self) -> BackendKind {
            BackendKind::Canvas2d
        }
        fn model_alpha(&self) -> f64 {
            self.model_alpha
        }
        fn set_model_alpha(&mut self, alpha: f64) {
            self.model_alpha = alpha;
        }
        fn candle_body_alpha(&self) -> f64 {
            self.body_alpha
        }
        fn set_candle_body_alpha(&mut self, alpha: f64) {
            self.body_alpha = alpha;
        }
        fn candle_structure_alpha(&self) -> f64 {
            self.structure_alpha
        }
        fn set_candle_structure_alpha(&mut self, alpha: f64) {
            self.structure_alpha = alpha;
        }
        fn candle_body_scale(&self) -> f64 {
            self.body_scale
        }
        fn set_candle_body_scale(&mut self, scale: f64) {
            self.body_scale = scale;
        }
        fn render(&mut self, _: &SceneGraph, _: &CoordinateSystem, _: &VelaTheme) {
            self.frames += 1;
        }
        fn destroy(&mut self) {
            self.destroyed = true;
        }
    }

    #[test]
    fn the_trait_is_object_safe_and_drives_a_frame() {
        let mut backend: Box<dyn RenderBackend> = Box::new(Recording {
            model_alpha: 1.0,
            body_alpha: 1.0,
            structure_alpha: 1.0,
            body_scale: 1.0,
            frames: 0,
            destroyed: false,
        });
        backend.set_model_alpha(0.5);
        backend.set_candle_body_scale(0.07);
        backend.render(&SceneGraph::new(), &CoordinateSystem::new(), &json!({}));
        assert_eq!(backend.kind(), BackendKind::Canvas2d);
        assert_eq!(backend.model_alpha(), 0.5);
        assert_eq!(backend.candle_body_scale(), 0.07);
        assert_eq!(backend.candle_body_alpha(), 1.0);
        assert_eq!(backend.candle_structure_alpha(), 1.0);
        backend.destroy();
    }

    #[test]
    fn kinds_read_as_the_originals_names() {
        assert_eq!(BackendKind::Canvas2d.as_str(), "canvas2d");
        assert_eq!(BackendKind::WebGl2.as_str(), "webgl2");
    }
}
