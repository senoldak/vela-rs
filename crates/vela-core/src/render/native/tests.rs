//! Tests of the native renderer shell, against the in-memory [`TestHost`].
//!
//! The shared rig lives here; the cases are grouped by the original tests they
//! port in the files of `tests/`.

mod config;
mod crosshair;
mod end_to_end;
mod features;
mod more;
mod panes;
mod viewport;

use std::rc::Rc;

use serde_json::Value;

use crate::model::{AnchorUpdate, DirtyRange, IndicatorModel, Ohlcv, ScenePatch, ValuePatch};
use crate::options::{IntroAnimation, RendererDisplayOptions};
use crate::ports::chart_renderer::{ChartRenderer, IndicatorRenderHandle, SetBarsOptions};
use crate::theme::dark_theme;

use super::testing::{TestHost, line_model, sample_bars};
use super::*;

pub(super) const T0: i64 = 1_700_000_000_000;
pub(super) const STEP: i64 = 60_000;

pub(super) fn display(intro: bool) -> RendererDisplayOptions {
    RendererDisplayOptions {
        current_price_line: true,
        log_scale: false,
        native_backend: crate::options::NativeBackend::Canvas2d,
        anim_zoom: 70.0,
        anim_pan: 110.0,
        anim_scroll: 130.0,
        anim_autoscale: 80.0,
        anim_live_bar: 0.0,
        anim_intro: if intro {
            IntroAnimation {
                style: Some(crate::options::IntroStyle::Settle),
                duration: 650.0,
            }
        } else {
            IntroAnimation::OFF
        },
        glow: 0.0,
        up_color: crate::palette::BULLISH.into(),
        down_color: crate::palette::BEARISH.into(),
        price_style: "candles".into(),
        price_axis_ticks: None,
        free_pan: None,
    }
}

pub(super) struct Rig {
    pub host: Rc<TestHost>,
    pub r: Rc<NativeRenderer>,
}

pub(super) fn rig_with(intro: bool, w: f64, h: f64) -> Rig {
    let host = TestHost::new(w, h, 1.0);
    let r = NativeRenderer::new(host.clone(), Some(display(intro)));
    r.mount(&dark_theme());
    Rig { host, r }
}

pub(super) fn rig() -> Rig {
    rig_with(false, 800.0, 420.0)
}

/// A rig whose DATA area is `width` x `height` (the axis gutters are added to the host).
pub(super) fn data_rig(width: f64, height: f64) -> Rig {
    rig_with(false, width + 64.0, height + 22.0)
}

pub(super) fn flat_bars(n: usize, from: usize) -> Vec<Ohlcv> {
    (0..n)
        .map(|i| Ohlcv::new(T0 + (from + i) as i64 * STEP, 1.0, 2.0, 0.5, 1.5, Some(1.0)))
        .collect()
}

pub(super) fn near(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() <= eps, "{a} is not within {eps} of {b}");
}

impl Rig {
    /// Set `n` sample bars and step frames until idle.
    pub fn bars(&self, n: usize) -> Vec<Ohlcv> {
        let bars = sample_bars(n, T0, STEP);
        self.r.set_bars(&bars, SetBarsOptions::default());
        self.host.run_until_idle();
        bars
    }

    pub fn viewport(&self) -> (f64, f64) {
        let v = self.r.coordinates().viewport();
        (v.bar_spacing, v.right_offset)
    }

    pub fn set_viewport(&self, bar_spacing: f64, right_offset: f64) {
        self.r.with(|st| {
            st.coords.set_viewport(crate::render::core::ViewportState {
                bar_spacing,
                right_offset,
            });
        });
    }

    pub fn set_bars(&self, bars: &[Ohlcv], preserve: bool) {
        self.r.set_bars(
            bars,
            SetBarsOptions {
                preserve_view: preserve,
            },
        );
    }

    pub fn feature(&self, key: &str) -> Value {
        self.r.read_feature(key).unwrap_or(Value::Null)
    }

    pub fn offset_of(&self, id: &str) -> i64 {
        self.r.with(|st| st.scene.offset_of(id))
    }

    pub fn mount_model(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
        self.r.mount_indicator(model)
    }
}

pub(super) fn anchored_model(id: &str, n: usize, from: usize) -> IndicatorModel {
    let times: Vec<i64> = (0..n).map(|i| T0 + (from + i) as i64 * STEP).collect();
    let values: Vec<Option<f64>> = (0..n).map(|i| Some((from + i) as f64)).collect();
    let mut m = line_model(id, "price", true, &times, &values);
    m.anchor_time = Some(T0 + from as i64 * STEP);
    m
}

pub(super) fn value_patch(id: &str, anchor: AnchorUpdate) -> ValuePatch {
    ValuePatch {
        indicator_id: id.into(),
        dirty: DirtyRange { from: 0, to: 0 },
        anchor_time: anchor,
        series: vec![],
        lines: None,
        boxes: None,
        labels: None,
        polylines: None,
        linefills: None,
        tables: None,
        trades: None,
    }
}

pub(super) fn patch(id: &str, anchor: AnchorUpdate) -> ScenePatch {
    ScenePatch::Value(value_patch(id, anchor))
}
