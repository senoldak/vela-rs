//! The ticks of each pane's MASTER price axis: the one source both the gridlines
//! (backdrop) and the axis labels (chrome) read, so they always agree. Without a host
//! function they are the built-in ladder; with one (the `priceAxisTicks` feature) the
//! host's ticks replace it. Host results are memoized per pane on every input they depend
//! on, so the backdrop and the chrome share one call per data frame and a chrome-only
//! repaint (the countdown's second pulse, a mark hover) never calls the host again.
//!
//! The source is shared by the backdrop and the chrome painters, so it is a handle with
//! interior state (`&self` methods). Differences from the original, all forced by the
//! language:
//! - a Rust host function cannot throw or return a non-array, so the original's "warn once
//!   and use the defaults" paths do not exist; it returns `None` to keep the defaults;
//! - the memo is keyed by pane id rather than pane identity, and the chart's formatters
//!   have no identity to compare: the memo compares their locale, numbering system and
//!   hour cycle, and a host that swaps its formatters outright calls
//!   [`invalidate`](PriceAxisTickSource::invalidate) (setting the hook does it too);
//! - `price_to_y` does not need the coordinate system: the mapping reads only the scale
//!   and bounds, so the pane geometry is all [`ticks_for`](PriceAxisTickSource::ticks_for)
//!   takes.
// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::format::{HourCycleSetting, ResolvedFormatters, english_format};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::PaneKind;
use crate::options::{
    PriceAxisMode, PriceAxisPane, PriceAxisTick, PriceAxisTickContext, PriceAxisTicksFn,
};
use crate::render::core::scene_graph::{PaneAxisFormat, percent_scale_for};
use crate::render::core::{CoordinateSystem, PaneBounds, PaneNode, PriceScale, SceneGraph};

use super::ticks::{AxisScale, AxisTick, pane_axis_ticks};

/// Everything a pane's ticks depend on, plus the ticks computed from it.
struct TickMemo {
    kind: PaneKind,
    min: f64,
    max: f64,
    log: bool,
    invert: bool,
    height: f64,
    baseline: Option<f64>,
    indexed: bool,
    mintick: Option<f64>,
    format: Option<PaneAxisFormat>,
    locale: String,
    numbering_system: String,
    hour_cycle: HourCycleSetting,
    font_size: f64,
    ticks: Rc<[PriceAxisTick]>,
}

#[derive(Default)]
pub struct PriceAxisTickSource {
    hook: RefCell<Option<PriceAxisTicksFn>>,
    memo: RefCell<HashMap<String, TickMemo>>,
}

fn builtin_formatters() -> &'static ResolvedFormatters {
    static ENGLISH: OnceLock<ResolvedFormatters> = OnceLock::new();
    ENGLISH.get_or_init(english_format)
}

fn to_price_axis_ticks(ticks: Vec<AxisTick>) -> Vec<PriceAxisTick> {
    ticks
        .into_iter()
        .map(|t| PriceAxisTick {
            price: t.price,
            label: t.label,
            major: None,
        })
        .collect()
}

impl PriceAxisTickSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// The host function, or `None` for the built-in ticks.
    pub fn hook(&self) -> Option<PriceAxisTicksFn> {
        self.hook.borrow().clone()
    }

    /// Set (or clear) the host function. Drops every memoized result, so setting the same
    /// function again recomputes: how a host refreshes ticks that depend on its own state.
    pub fn set_hook(&self, hook: Option<PriceAxisTicksFn>) {
        *self.hook.borrow_mut() = hook;
        self.invalidate();
    }

    /// Drop every memoized host result.
    pub fn invalidate(&self) {
        self.memo.borrow_mut().clear();
    }

    /// The pane's master-scale ticks for this frame. An unscaled pane
    /// (`axis_format: NoAxis`) gets none, and the host is never asked for it.
    pub fn ticks_for(&self, scene: &SceneGraph, pane: &PaneNode) -> Rc<[PriceAxisTick]> {
        let format = pane.axis_format;
        if format == Some(PaneAxisFormat::NoAxis) {
            return Rc::from(Vec::new());
        }
        let pct = percent_scale_for(scene, pane);
        let height = pane.bounds.height;
        let fmt = scene
            .format
            .as_ref()
            .unwrap_or_else(|| builtin_formatters());
        let defaults = || {
            to_price_axis_ticks(pane_axis_ticks(
                &AxisScale::from(&pane.scale),
                height,
                pct,
                scene.price_mintick,
                format,
                fmt,
            ))
        };
        let Some(hook) = self.hook() else {
            return Rc::from(defaults());
        };
        if !(height > 0.0) {
            return Rc::from(defaults());
        }
        let s = &pane.scale;
        let settings = fmt.settings();
        let font_size = scene.style.font_size;
        if let Some(m) = self.memo.borrow().get(&pane.id)
            && m.kind == pane.kind
            && m.min == s.min
            && m.max == s.max
            && m.log == s.is_log()
            && m.invert == s.is_inverted()
            && m.height == height
            && m.baseline == pct.map(|p| p.baseline)
            && m.indexed == pct.is_some_and(|p| p.indexed)
            && m.mintick == scene.price_mintick
            && m.format == format
            && m.locale == settings.locale
            && m.numbering_system == settings.numbering_system
            && m.hour_cycle == settings.hour_cycle
            && m.font_size == font_size
        {
            return Rc::clone(&m.ticks);
        }
        let mode = match pct {
            Some(p) if p.indexed => PriceAxisMode::Indexed,
            Some(_) => PriceAxisMode::Percent,
            None => PriceAxisMode::Price,
        };
        let ticks = call_hook(
            &hook,
            scene,
            pane,
            defaults(),
            pct.map(|p| p.baseline),
            mode,
            font_size,
        );
        self.memo.borrow_mut().insert(
            pane.id.clone(),
            TickMemo {
                kind: pane.kind,
                min: s.min,
                max: s.max,
                log: s.is_log(),
                invert: s.is_inverted(),
                height,
                baseline: pct.map(|p| p.baseline),
                indexed: pct.is_some_and(|p| p.indexed),
                mintick: scene.price_mintick,
                format,
                locale: settings.locale.clone(),
                numbering_system: settings.numbering_system.clone(),
                hour_cycle: settings.hour_cycle,
                font_size,
                ticks: Rc::clone(&ticks),
            },
        );
        ticks
    }
}

fn call_hook(
    hook: &PriceAxisTicksFn,
    scene: &SceneGraph,
    pane: &PaneNode,
    defaults: Vec<PriceAxisTick>,
    baseline: Option<f64>,
    mode: PriceAxisMode,
    font_size: f64,
) -> Rc<[PriceAxisTick]> {
    // A snapshot: the live scale object is restamped every frame, and a host may keep
    // `price_to_y`.
    let scale = PriceScale {
        min: pane.scale.min,
        max: pane.scale.max,
        log: Some(pane.scale.is_log()),
        invert: Some(pane.scale.is_inverted()),
    };
    let bounds = PaneBounds {
        top: 0.0,
        height: pane.bounds.height,
    };
    // The mapping reads only the scale and bounds; a fresh coordinate system gives the
    // renderer's own `price_to_y` without borrowing the live one into a `'static` closure.
    let coords = CoordinateSystem::new();
    let ctx = PriceAxisTickContext {
        pane: PriceAxisPane {
            id: pane.id.clone(),
            kind: pane.kind,
        },
        min: js_min(scale.min, scale.max),
        max: js_max(scale.min, scale.max),
        log: scale.is_log(),
        height: bounds.height,
        mode,
        baseline,
        mintick: scene.price_mintick,
        font_size,
        price_to_y: Rc::new(move |price| coords.price_to_y(price, &scale, &bounds)),
        defaults: defaults.clone(),
    };
    match hook(&ctx) {
        None => Rc::from(defaults),
        Some(out) => Rc::from(
            out.into_iter()
                .filter(|t| t.price.is_finite())
                .collect::<Vec<_>>(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::PriceAxisTick;
    use crate::render::core::scene_graph::ScaleMode;
    use std::cell::Cell;

    fn scene() -> SceneGraph {
        let mut s = SceneGraph::new();
        {
            let price = s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
            price.bounds = PaneBounds {
                top: 0.0,
                height: 300.0,
            };
            price.scale = PriceScale::new(100.0, 200.0);
        }
        {
            let study = s.ensure_pane("study-1", PaneKind::Study, 1, Some(1.0));
            study.bounds = PaneBounds {
                top: 300.0,
                height: 100.0,
            };
            study.scale = PriceScale::new(0.0, 100.0);
        }
        s
    }

    fn pane<'a>(s: &'a SceneGraph, id: &str) -> &'a PaneNode {
        s.panes.get(id).unwrap()
    }

    fn at(prices: &[f64]) -> Vec<PriceAxisTick> {
        prices
            .iter()
            .map(|p| PriceAxisTick {
                price: *p,
                label: format!("L{p}"),
                major: None,
            })
            .collect()
    }

    fn hook(
        f: impl Fn(&PriceAxisTickContext) -> Option<Vec<PriceAxisTick>> + 'static,
    ) -> Option<PriceAxisTicksFn> {
        Some(Rc::new(f))
    }

    fn defaults(s: &SceneGraph, id: &str, mintick: Option<f64>) -> Vec<PriceAxisTick> {
        let p = pane(s, id);
        to_price_axis_ticks(pane_axis_ticks(
            &AxisScale::from(&p.scale),
            p.bounds.height,
            percent_scale_for(s, p),
            mintick,
            p.axis_format,
            builtin_formatters(),
        ))
    }

    #[test]
    fn without_a_function_it_yields_the_built_in_ladder() {
        let mut s = scene();
        s.price_mintick = Some(0.5);
        let src = PriceAxisTickSource::new();
        assert!(src.hook().is_none());
        assert_eq!(
            src.ticks_for(&s, pane(&s, "price")).to_vec(),
            defaults(&s, "price", Some(0.5))
        );
        assert!(!src.ticks_for(&s, pane(&s, "price")).is_empty());
    }

    #[test]
    fn the_hosts_ticks_replace_the_defaults_and_none_keeps_them() {
        let s = scene();
        let src = PriceAxisTickSource::new();
        src.set_hook(hook(|_| Some(at(&[120.0, 150.0]))));
        assert_eq!(
            src.ticks_for(&s, pane(&s, "price")).to_vec(),
            at(&[120.0, 150.0])
        );
        src.set_hook(hook(|_| None));
        assert_eq!(
            src.ticks_for(&s, pane(&s, "price")).to_vec(),
            defaults(&s, "price", None)
        );
        // An empty ladder is a real answer.
        src.set_hook(hook(|_| Some(vec![])));
        assert!(src.ticks_for(&s, pane(&s, "price")).is_empty());
    }

    #[test]
    fn drops_invalid_entries_and_keeps_major_as_given() {
        let s = scene();
        let src = PriceAxisTickSource::new();
        let entry = |price: f64, label: &str, major: Option<bool>| PriceAxisTick {
            price,
            label: label.into(),
            major,
        };
        src.set_hook(hook(move |_| {
            Some(vec![
                entry(110.0, "ok", Some(true)),
                entry(f64::NAN, "nan", None),
                entry(f64::INFINITY, "inf", None),
                entry(150.0, "muted", Some(false)),
                entry(160.0, "plain", None),
            ])
        }));
        assert_eq!(
            src.ticks_for(&s, pane(&s, "price")).to_vec(),
            vec![
                entry(110.0, "ok", Some(true)),
                entry(150.0, "muted", Some(false)),
                entry(160.0, "plain", None),
            ]
        );
    }

    #[test]
    fn hands_the_host_the_pane_range_size_mode_baseline_tick_size_font_size_and_defaults() {
        let mut s = scene();
        s.price_mintick = Some(0.01);
        let seen: Rc<RefCell<Vec<PriceAxisTickContext>>> = Rc::default();
        let sink = Rc::clone(&seen);
        let src = PriceAxisTickSource::new();
        src.set_hook(hook(move |ctx| {
            sink.borrow_mut().push(ctx.clone());
            None
        }));

        src.ticks_for(&s, pane(&s, "price"));
        {
            let seen = seen.borrow();
            let c = &seen[0];
            assert_eq!(
                (c.pane.id.as_str(), c.pane.kind),
                ("price", PaneKind::Price)
            );
            assert_eq!(
                (c.min, c.max, c.log, c.height),
                (100.0, 200.0, false, 300.0)
            );
            assert_eq!(c.mode, PriceAxisMode::Price);
            assert_eq!(
                (c.mintick, c.font_size, c.baseline),
                (Some(0.01), 11.0, None)
            );
            assert_eq!(c.defaults, defaults(&s, "price", Some(0.01)));
        }

        s.scale_mode = ScaleMode::Percent;
        s.panes.get_mut("price").unwrap().percent_baseline = 125.0;
        src.ticks_for(&s, pane(&s, "price"));
        {
            let seen = seen.borrow();
            assert_eq!(seen[1].mode, PriceAxisMode::Percent);
            assert_eq!(seen[1].baseline, Some(125.0));
            assert_eq!((seen[1].min, seen[1].max), (100.0, 200.0));
            assert!(seen[1].defaults[0].label.ends_with('%'));
        }

        s.scale_mode = ScaleMode::Indexed;
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(seen.borrow()[2].mode, PriceAxisMode::Indexed);
        assert_eq!(seen.borrow()[2].baseline, Some(125.0));

        // A study pane follows its own mode, not the price pane's.
        src.ticks_for(&s, pane(&s, "study-1"));
        let seen = seen.borrow();
        let c = &seen[3];
        assert_eq!(
            (c.pane.id.as_str(), c.pane.kind),
            ("study-1", PaneKind::Study)
        );
        assert_eq!(c.mode, PriceAxisMode::Price);
        assert_eq!(
            (c.height, c.min, c.max, c.baseline),
            (100.0, 0.0, 100.0, None)
        );
    }

    #[test]
    fn price_to_y_is_the_renderers_own_mapping_pane_relative_for_linear_log_and_inverted() {
        let mut s = scene();
        let c = CoordinateSystem::new();
        let captured: Rc<RefCell<Option<PriceAxisTickContext>>> = Rc::default();
        let sink = Rc::clone(&captured);
        let src = PriceAxisTickSource::new();
        src.set_hook(hook(move |ctx| {
            *sink.borrow_mut() = Some(ctx.clone());
            None
        }));
        let scales = [
            PriceScale::new(0.0, 100.0),
            PriceScale {
                log: Some(true),
                ..PriceScale::new(10.0, 1000.0)
            },
            PriceScale {
                invert: Some(true),
                ..PriceScale::new(0.0, 100.0)
            },
            PriceScale {
                log: Some(true),
                invert: Some(true),
                ..PriceScale::new(10.0, 1000.0)
            },
        ];
        for scale in scales {
            s.panes.get_mut("study-1").unwrap().scale = scale;
            let study = pane(&s, "study-1");
            src.ticks_for(&s, study);
            let ctx = captured.borrow().clone().unwrap();
            for p in [10.0, 25.0, 50.0, 99.0] {
                let want = c.price_to_y(p, &scale, &study.bounds) - study.bounds.top;
                assert!(((ctx.price_to_y)(p) - want).abs() < 1e-9);
            }
            assert_eq!(ctx.log, scale.is_log());
        }
    }

    #[test]
    fn an_unscaled_pane_gets_no_ticks_and_is_never_asked_about() {
        let mut s = scene();
        s.panes.get_mut("study-1").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        let calls = Rc::new(Cell::new(0));
        let counter = Rc::clone(&calls);
        let src = PriceAxisTickSource::new();
        src.set_hook(hook(move |_| {
            counter.set(counter.get() + 1);
            Some(at(&[50.0]))
        }));
        assert!(src.ticks_for(&s, pane(&s, "study-1")).is_empty());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn calls_the_host_once_per_pane_until_the_range_size_or_mode_changes() {
        let mut s = scene();
        let calls = Rc::new(Cell::new(0));
        let counter = Rc::clone(&calls);
        let f = hook(move |_| {
            counter.set(counter.get() + 1);
            Some(at(&[150.0]))
        });
        let src = PriceAxisTickSource::new();
        src.set_hook(f.clone());
        src.ticks_for(&s, pane(&s, "price"));
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 1);
        s.panes.get_mut("price").unwrap().scale = PriceScale::new(100.0, 210.0);
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 2);
        s.panes.get_mut("price").unwrap().bounds = PaneBounds {
            top: 0.0,
            height: 280.0,
        };
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 3);
        s.price_mintick = Some(0.1);
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 4);
        s.style.font_size = 12.0;
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 5);
        src.set_hook(f); // the same function: the host asks for a refresh
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 6);
        src.invalidate();
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 7);
    }

    #[test]
    fn a_zero_height_pane_gets_the_defaults_without_asking_the_host() {
        let mut s = scene();
        s.panes.get_mut("price").unwrap().bounds = PaneBounds {
            top: 0.0,
            height: 0.0,
        };
        let calls = Rc::new(Cell::new(0));
        let counter = Rc::clone(&calls);
        let src = PriceAxisTickSource::new();
        src.set_hook(hook(move |_| {
            counter.set(counter.get() + 1);
            Some(at(&[150.0]))
        }));
        src.ticks_for(&s, pane(&s, "price"));
        assert_eq!(calls.get(), 0);
    }
}
