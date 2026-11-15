//! Tests of the drawing painter: the ported TS cases, a smoke test per registered tool, and
//! full call streams for one tool of each family.

use serde_json::{Value, json};

use crate::drawings::{
    Drawing, DrawingInit, DrawingPoint, DrawingRegistry, DrawingSeriesState, DrawingText,
    LinearProjector, PaneRect, Projector, SeriesUnavailable,
};
use crate::model::{BoxHAlign, BoxTextSize, BoxVAlign, Ohlcv};
use crate::render::canvas::RecordingCanvas;

use super::*;

mod families;
mod more;
mod smoke;
mod ts_ports;

pub(super) fn theme() -> DrawingTheme {
    DrawingTheme {
        background: "#101010".to_string(),
        text_color: "#ffffff".to_string(),
        font_family: "sans-serif".to_string(),
    }
}

/// A projector of plain closures' worth: `x = time`, `y = y_origin - price` on pane `price`.
/// Without `with_panes` it exposes no pane geometry (the unclipped, back-compat path).
pub(super) struct FnProj {
    pub y_origin: f64,
    pub width: f64,
    pub height: f64,
    pub with_panes: bool,
    pub log_scale: bool,
}

impl FnProj {
    pub fn new(y_origin: f64, width: f64, height: f64) -> Self {
        Self {
            y_origin,
            width,
            height,
            with_panes: false,
            log_scale: false,
        }
    }

    pub fn paned(mut self) -> Self {
        self.with_panes = true;
        self
    }

    pub fn log(mut self) -> Self {
        self.log_scale = true;
        self
    }
}

impl Projector for FnProj {
    fn x_of(&self, time: f64) -> f64 {
        time
    }

    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
        let known = pane_id == "price" || (self.with_panes && pane_id == "hidden");
        known.then(|| {
            if self.log_scale {
                300.0 - 100.0 * price.log10()
            } else {
                self.y_origin - price
            }
        })
    }

    fn px_to_point(&self, x: f64, y: f64, _pane_id: &str) -> DrawingPoint {
        DrawingPoint {
            time: x,
            price: self.y_origin - y,
        }
    }

    fn pane_id_at_y(&self, _y: f64) -> Option<String> {
        Some("price".to_string())
    }

    fn pane_rect(&self, pane_id: &str) -> Option<PaneRect> {
        if !self.with_panes {
            return None;
        }
        match pane_id {
            "price" => Some(PaneRect {
                top: 0.0,
                height: 100.0,
            }),
            "hidden" => Some(PaneRect {
                top: 100.0,
                height: 0.0,
            }),
            _ => None,
        }
    }

    fn width(&self) -> f64 {
        self.width
    }

    fn height(&self) -> f64 {
        self.height
    }
}

pub(super) fn point(time: f64, price: f64) -> DrawingPoint {
    DrawingPoint { time, price }
}

/// A fresh drawing of `key` on pane `price`, with the type's default style and the given
/// anchors; `props` and `text` optional.
pub(super) fn make(
    key: &str,
    id: &str,
    anchors: Vec<DrawingPoint>,
    props: Option<Value>,
    text: Option<DrawingText>,
) -> Box<dyn Drawing> {
    let mut init = DrawingInit::on_pane("price").anchors(anchors);
    init.id = Some(id.to_string());
    init.text = text;
    init.props = props.map(|v| v.as_object().cloned().unwrap_or_default());
    DrawingRegistry::builtin()
        .create(key, init)
        .unwrap_or_else(|| panic!("registered type {key}"))
}

pub(super) fn text_block(value: &str) -> DrawingText {
    DrawingText {
        value: value.to_string(),
        color: None,
        size: BoxTextSize::Normal,
        h_align: BoxHAlign::Center,
        v_align: BoxVAlign::Top,
        bold: None,
        italic: None,
        place: None,
        side: None,
    }
}

pub(super) fn paint(
    drawings: &[&dyn Drawing],
    proj: &dyn Projector,
    targets: &PaintTargets,
) -> RecordingCanvas {
    let canvas = RecordingCanvas::new();
    DrawingPainter::new().paint_all(&canvas, drawings, proj, &theme(), targets);
    canvas
}

pub(super) fn paint_one_drawing(d: &dyn Drawing, proj: &dyn Projector) -> RecordingCanvas {
    paint(&[d], proj, &PaintTargets::default())
}

/// True when any argument of a recorded call is `null`: a NaN or an infinity went through
/// the recorder's JSON encoding.
pub(super) fn has_non_finite(op: &Value) -> bool {
    match op {
        Value::Null => true,
        Value::Array(items) => items.iter().any(has_non_finite),
        Value::Object(map) => map.values().any(has_non_finite),
        _ => false,
    }
}

/// Non-empty, every `save` closed by its `restore` (never closing one that is not open), no
/// NaN or infinite argument.
pub(super) fn assert_sane(canvas: &RecordingCanvas, label: &str) {
    let ops = canvas.ops();
    assert!(!ops.is_empty(), "{label}: painted nothing");
    assert_balanced(canvas, label);
    for op in &ops {
        assert!(!has_non_finite(op), "{label}: non-finite argument in {op}");
    }
}

pub(super) fn assert_balanced(canvas: &RecordingCanvas, label: &str) {
    let mut depth = 0i32;
    for name in canvas.names() {
        match name.as_str() {
            "save" => depth += 1,
            "restore" => {
                depth -= 1;
                assert!(depth >= 0, "{label}: restore without save");
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "{label}: unbalanced save/restore");
}

/// Candles five minutes apart (5 px each on the sample projector) with a bullish fair value
/// gap at bar 20, a swept swing high near bar 30 and a block-like down candle at bar 10.
pub(super) fn sample_bars() -> Vec<Ohlcv> {
    (0..100)
        .map(|i| {
            let wave = ((i as f64) * 0.35).sin() * 4.0;
            let (open, high, low, close) = match i {
                19 => (100.0, 101.0, 99.0, 100.5),
                20 => (100.5, 108.0, 100.0, 107.5),
                21 => (107.5, 110.0, 105.0, 109.0),
                30 => (108.0, 112.0, 107.0, 108.5),
                35 => (108.5, 114.0, 108.0, 109.0),
                10 => (101.0, 101.5, 97.0, 98.0),
                11 => (98.0, 104.0, 97.5, 103.0),
                _ => {
                    let base = 105.0 + wave;
                    (base, base + 1.5, base - 1.5, base + 0.4)
                }
            };
            Ohlcv::new(
                i * 300_000,
                open,
                high,
                low,
                close,
                Some(1000.0 + (i % 7) as f64 * 150.0),
            )
        })
        .collect()
}

pub(super) fn bars_projector() -> LinearProjector {
    LinearProjector::sample().with_bars(sample_bars())
}

/// A `Projector` with a scripted series gateway, for the magnifier.
pub(super) struct GatewayProj {
    pub inner: LinearProjector,
    pub state: DrawingSeriesState,
}

impl Projector for GatewayProj {
    fn x_of(&self, time: f64) -> f64 {
        self.inner.x_of(time)
    }

    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
        self.inner.y_of(price, pane_id)
    }

    fn px_to_point(&self, x: f64, y: f64, pane_id: &str) -> DrawingPoint {
        self.inner.px_to_point(x, y, pane_id)
    }

    fn pane_id_at_y(&self, y: f64) -> Option<String> {
        self.inner.pane_id_at_y(y)
    }

    fn pane_rect(&self, pane_id: &str) -> Option<PaneRect> {
        self.inner.pane_rect(pane_id)
    }

    fn bars_between(&self, t1: f64, t2: f64) -> Option<f64> {
        self.inner.bars_between(t1, t2)
    }

    fn bars_in_range(&self, from: f64, to: f64) -> Option<Vec<Ohlcv>> {
        self.inner.bars_in_range(from, to)
    }

    fn series_in_range(&self, _tf: &str, _from: f64, _to: f64) -> Option<DrawingSeriesState> {
        Some(self.state.clone())
    }

    fn width(&self) -> f64 {
        self.inner.width()
    }

    fn height(&self) -> f64 {
        self.inner.height()
    }
}
