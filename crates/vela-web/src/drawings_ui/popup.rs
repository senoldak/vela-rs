//! The quick settings bar of a selected drawing (or a multi-selection): a port of
//! `DrawingSettingsPopup.ts`.
//!
//! A leading move handle, a row of icon controls the drawing's schema asks for (color, width,
//! style, fill, text, per-type extras), then the trailing group: the settings gear, lock,
//! delete and a kebab overflow (duplicate, order, reset). It floats clear of the drawing and
//! can be dragged anywhere. With several drawings it shows only the controls they all
//! support, a control whose values differ reads as mixed (half lit, or a striped swatch), and
//! every edit applies to all of them.
//!
//! Controls read through [`Core`] and edit through it. Each registers a `sync` that re-reads
//! its value, run from a timer after every model change ([`DrawingsUi::request_sync`]).
//!
//! The bar dismisses itself on a press outside it (bubble phase, armed one tick after it
//! opens so the press that opened it does not count) and reports `popup_dismissed(modifier)`
//! once; a close the controller asked for needs no report. With the settings panel open, a
//! press inside the chart is the chart's (dragging the drawing keeps the panel) and a press
//! elsewhere closes only the panel.
//!
//! Differences from the original: the color control is the dialogs' native swatch plus an
//! opacity slider (the kit's RGB picker is not ported), every control carries an
//! `aria-label`, and the move handle also moves with the keyboard (arrow keys).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use serde_json::{Value, json};
use vela_core::drawings::types::annotations::{GLYPH_OPTIONS, STAMP_SIZE_OPTIONS};
use vela_core::drawings::types::mach::{
    DEDEKIND_CURVATURE_OPTIONS, MACH_NUMBER_OPTIONS, MACH_WAVE_COUNT_OPTIONS,
};
use vela_core::drawings::types::measure::{magnifier_timeframe_label, magnifier_timeframes_below};
use vela_core::drawings::{DEFAULT_DRAWING_COLOR, line_style_options, text_size_options};
use vela_core::ports::chart_renderer::ReorderTo;
use vela_core::render::drawings::PopupRequest;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::DrawingsUi;
use super::float::FloatHost;
use super::glyphs::{self, glyph, sized};
use super::model;
use super::model::{
    Common, Snap, capitalize, common, distinct, fmt_num, loose_eq, shared_paths, size_key, stripes,
};
use super::session::Core;
use super::util;
use crate::dialogs::controls;
use crate::dialogs::{Cx, Tr};
use crate::dom::{
    Listener, attr, default_view, get_bool, get_f64, get_prop, get_str, prevent_default, set_style,
    set_styles, stop_propagation, vela_capture_pointer, vela_contains, vela_focus, vela_get_value,
    vela_release_pointer,
};

/// Icon-button side, px.
const BTN: f64 = 30.0;
/// Icon glyph side, px.
const ICON: u32 = 21;
/// Margin between the bar and the drawing it floats clear of, px.
const GAP: f64 = 10.0;
/// What a bar measures before layout has run (a detached element reads 0).
const FALLBACK_SIZE: (f64, f64) = (300.0, 38.0);

type Get<T> = Rc<dyn Fn() -> T>;

/// Whether one open bar's floating shell (menu or color popover) is open, and the bar.
pub struct Popup {
    me: Weak<Popup>,
    ui: Weak<DrawingsUi>,
    pub core: Rc<Core>,
    cx: Cx,
    el: Element,
    wrapper: Element,
    tip: RefCell<Option<Element>>,
    floats: Rc<FloatHost>,
    text_panel: RefCell<Option<Element>>,
    syncs: RefCell<Vec<Rc<dyn Fn()>>>,
    drag: RefCell<Vec<Listener>>,
    outside: RefCell<Option<Listener>>,
    /// The single drawing's box in wrapper px: the panel opens beside it.
    anchor: Option<(f64, f64, f64, f64)>,
}

/// A dropdown option's display options.
#[derive(Default)]
struct DropOpts {
    /// The text of an option row (and of the trigger when `in_trigger`).
    label: Option<Rc<dyn Fn(&Value) -> String>>,
    in_trigger: bool,
}

fn val_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(fmt_num).unwrap_or_default(),
        other => other.to_string(),
    }
}

impl Popup {
    /// Build and mount the bar for `request`. `None` when no drawing is left to edit.
    pub fn open(ui: &Rc<DrawingsUi>, request: &PopupRequest) -> Option<Rc<Popup>> {
        let core = Core::new(
            ui,
            request.ids.clone(),
            request.multi || request.ids.len() > 1,
        );
        if core.snaps().is_empty() {
            return None;
        }
        super::style::ensure_styles(&ui.doc);
        crate::dialogs::css::ensure_styles(&ui.doc);
        let cx = Cx::new(&ui.doc, ui.tr());
        let el = cx.el("div", "vela-dpop vela-dsp-scope");
        attr(&el, "role", "toolbar");
        attr(
            &el,
            "aria-label",
            &ui.tr().t("drawing.bar.settings", "Settings"),
        );
        let (off_x, off_y) = ui.plot_offset();
        let anchor = if request.multi {
            None
        } else {
            request.anchor.map(|a| (off_x + a.x, off_y + a.y, a.w, a.h))
        };
        let floats = FloatHost::new(ui, &cx);
        let popup = Rc::new_cyclic(|me| Popup {
            me: me.clone(),
            ui: Rc::downgrade(ui),
            core,
            cx,
            el,
            wrapper: ui.wrapper.clone(),
            tip: RefCell::new(None),
            floats,
            text_panel: RefCell::new(None),
            syncs: RefCell::new(Vec::new()),
            drag: RefCell::new(Vec::new()),
            outside: RefCell::new(None),
            anchor,
        });
        popup.build();
        set_style(&popup.el, "visibility", "hidden");
        let _ = popup.wrapper.append_child(&popup.el);
        popup.position(request.anchor.map(|a| (off_x + a.x, off_y + a.y, a.w, a.h)));
        set_style(&popup.el, "visibility", "");
        Some(popup)
    }

    /// The single drawing the bar is open on (a multi-selection has no panel).
    pub fn single(&self) -> Option<(String, Option<(f64, f64, f64, f64)>)> {
        if self.core.multi {
            return None;
        }
        self.core.ids.first().map(|id| (id.clone(), self.anchor))
    }

    /// The bar's box in wrapper px.
    pub fn bar_box(&self) -> Option<(f64, f64, f64, f64)> {
        let (x, y, w, h) = util::rect_in(&self.el, &self.wrapper);
        (w > 0.0 || h > 0.0).then_some((x, y, w, h))
    }

    /// Whether `node` belongs to the bar: the bar itself or its floating shell.
    pub fn contains(&self, node: &JsValue) -> bool {
        if vela_contains(&self.el, node) {
            return true;
        }
        self.floats.contains(node)
    }

    /// Re-read every control (the model changed under the bar).
    pub fn sync(&self) {
        let syncs: Vec<_> = self.syncs.borrow().clone();
        for f in syncs {
            f();
        }
    }

    /// Tear the bar down; the listeners are handed back so the owner can free them from a
    /// timer (the bar may be closing from inside one of its own handlers).
    pub fn close(&self) -> Vec<Listener> {
        self.hide_tip();
        let float_listeners = self.floats.close_now();
        let mut out: Vec<Listener> = std::mem::take(&mut *self.cx.bag.borrow_mut());
        out.extend(float_listeners);
        out.extend(self.drag.borrow_mut().drain(..));
        out.extend(self.outside.borrow_mut().take());
        self.syncs.borrow_mut().clear();
        self.el.remove();
        self.text_panel.borrow_mut().take();
        out
    }

    /// The panel is about to open beside the bar: the hover label and any menu go.
    pub fn prepare_for_panel(&self) {
        self.hide_tip();
        self.close_float();
    }

    /// Arm the outside-press dismissal (called one tick after the bar opens).
    pub fn arm_outside(&self, ui: &Rc<DrawingsUi>) {
        let weak = ui.weak();
        let listener = Listener::new(ui.doc.as_ref(), "pointerdown", false, true, move |ev| {
            let Some(ui) = weak.upgrade() else { return };
            ui.popup_outside_press(&ev);
        });
        *self.outside.borrow_mut() = Some(listener);
    }

    // ── labels ──

    fn t(&self, id: &'static str, en: &'static str) -> String {
        self.core.tr().t(id, en)
    }

    fn tr(&self) -> Tr {
        self.core.tr()
    }

    /// `small` to `Small`, through the schema's option labels.
    fn size_label(&self, v: &Value) -> String {
        let key = val_text(v);
        let tr = self.tr();
        text_size_options()
            .iter()
            .find(|o| o.value == key)
            .map_or_else(|| capitalize(&key), |o| tr.m(&o.label))
    }

    fn style_label(&self, v: &Value) -> String {
        let key = val_text(v);
        let tr = self.tr();
        line_style_options()
            .iter()
            .find(|o| o.value == key)
            .map_or_else(|| capitalize(&key), |o| tr.m(&o.label))
    }

    // ── the bar ──

    fn register(&self, sync: Rc<dyn Fn()>) {
        self.syncs.borrow_mut().push(sync);
    }

    /// A reader of one value over the selection (shared or mixed).
    fn cv<T: PartialEq + 'static>(&self, read: impl Fn(&Snap) -> T + 'static) -> Get<Common<T>> {
        let core = self.core.clone();
        Rc::new(move || common(&core.snaps(), &read))
    }

    fn patch1(&self, path: &'static str) -> Rc<dyn Fn(&Value)> {
        let core = self.core.clone();
        Rc::new(move |v: &Value| core.patch(&[(path, v.clone())]))
    }

    fn swatch(
        &self,
        tip: String,
        icon: &'static str,
        read: impl Fn(&Snap) -> String + Clone + 'static,
        path: &'static str,
    ) -> Element {
        let used_read = read.clone();
        let core = self.core.clone();
        let used: Get<Vec<String>> = Rc::new(move || distinct(&core.snaps(), &used_read));
        self.color_button(tip, icon, 17, self.cv(read), used, path)
    }

    fn build(&self) {
        let core = self.core.clone();
        let snaps = core.snaps().clone();
        let first = snaps[0].clone();
        let multi = self.core.multi;
        let paths = shared_paths(&snaps);
        let has = |p: &str| paths.contains(p);
        let every = |pred: &dyn Fn(&Snap) -> bool| snaps.iter().all(pred);
        let schema = &first.schema;
        // Text-first annotations (and computed labels) wear their text controls on the bar; on
        // a shape that merely CAN carry a label they stay beside the label field, and a
        // multi-selection has no label field.
        let text_on_bar = multi || schema.text_is_content || !has("text.value");

        let el = &self.el;
        let bar = self.cx.el("div", "vela-dpop-row");

        // Close an open popover when another control of the bar is engaged; triggers stop
        // their own press so a re-click can toggle.
        {
            let me = self.me.clone();
            self.cx.on(el, "pointerdown", move |_| {
                if let Some(p) = me.upgrade() {
                    p.close_float();
                }
            });
        }

        let _ = bar.append_child(&self.grip());

        if has("glyph") {
            let values: Vec<Value> = GLYPH_OPTIONS.iter().map(|g| json!(g)).collect();
            let get = self.cv(|s| {
                json!(
                    s.read_str("glyph")
                        .unwrap_or_else(|| GLYPH_OPTIONS[0].to_string())
                )
            });
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.icon", "Icon"),
                values,
                get,
                Rc::new(|v| glyphs::glyph_icon(&val_text(v))),
                self.patch1("glyph"),
                DropOpts::default(),
            ));
        }
        if has("size") {
            let values: Vec<Value> = STAMP_SIZE_OPTIONS.iter().map(|g| json!(g)).collect();
            let get =
                self.cv(|s| json!(s.read_str("size").unwrap_or_else(|| "normal".to_string())));
            let me = self.me.clone();
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.iconSize", "Icon size"),
                values,
                get,
                Rc::new(|v| glyphs::stamp_size_icon(&val_text(v))),
                self.patch1("size"),
                DropOpts {
                    label: Some(Rc::new(move |v| {
                        me.upgrade().map(|p| p.size_label(v)).unwrap_or_default()
                    })),
                    in_trigger: false,
                },
            ));
        }
        // Magnifier: the lower-timeframe pick leads the bar (text only: the label IS the
        // glyph); the inset candles' up / down colors ride along.
        if has("magnifier.timeframe") && every(&|s| s.type_key() == "magnifier") {
            let options = magnifier_timeframes_below(self.core.chart_bar_ms());
            if !options.is_empty() {
                let values: Vec<Value> = options.iter().map(|o| json!(o.value)).collect();
                let get = self.cv(|s| json!(s.read_str("magnifier.timeframe").unwrap_or_default()));
                let tr = self.tr();
                let _ = bar.append_child(&self.dropdown(
                    self.t("drawing.bar.lowerTimeframe", "Lower timeframe"),
                    values,
                    get,
                    Rc::new(|_| String::new()),
                    self.patch1("magnifier.timeframe"),
                    DropOpts {
                        label: Some(Rc::new(move |v| {
                            tr.m(&magnifier_timeframe_label(&val_text(v)))
                        })),
                        in_trigger: true,
                    },
                ));
            }
            let up = self.core.theme_color("upColor").unwrap_or_default();
            let down = self.core.theme_color("downColor").unwrap_or_default();
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.upCandles", "Up candles"),
                "bucket",
                move |s| {
                    s.read_str("magnifier.upColor")
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| up.clone())
                },
                "magnifier.upColor",
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.downCandles", "Down candles"),
                "bucket",
                move |s| {
                    s.read_str("magnifier.downColor")
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| down.clone())
                },
                "magnifier.downColor",
            ));
        }
        // The magnifier's unset border means the contrast ink of the background.
        if has("style.lineColor") {
            let bg = self.core.theme_color("background").unwrap_or_default();
            let ink =
                vela_core::render::shared::drawing_geometry::contrast_color(Some(&bg)).to_string();
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.lineColor", "Line color"),
                "brush",
                move |s| {
                    if !s.doc.style.line_color.is_empty() {
                        s.doc.style.line_color.clone()
                    } else if s.type_key() == "magnifier" {
                        ink.clone()
                    } else {
                        DEFAULT_DRAWING_COLOR.to_string()
                    }
                },
                "style.lineColor",
            ));
        }
        if has("style.lineWidth") {
            let width_field = |s: &Snap| {
                s.schema
                    .fields
                    .iter()
                    .find(|f| f.path == "style.lineWidth")
                    .cloned()
            };
            let wf = width_field(&first);
            let markers = every(&|s| {
                width_field(s).is_some_and(|f| {
                    f.kind == vela_core::drawings::FieldKind::Number && f.min.unwrap_or(1.0) > 1.0
                })
            });
            let get = self.cv(|s| (s.doc.style.line_width * 1000.0).round() as i64);
            if markers && let Some(wf) = wf {
                // A marker-width field (the highlighter's 4-60) can't live in the 1-4 ladder:
                // a free numeric input honoring the schema's range replaces it.
                let _ = bar.append_child(&self.width_input(
                    self.t("drawing.bar.lineWidth", "Line width"),
                    get,
                    wf.min.unwrap_or(1.0),
                    wf.max.unwrap_or(60.0),
                    wf.step.unwrap_or(1.0),
                ));
            } else {
                let g = get.clone();
                let get: Get<Common<Value>> = Rc::new(move || match g() {
                    Common::One(w) => Common::One(json!(w as f64 / 1000.0)),
                    Common::Mixed => Common::Mixed,
                });
                let _ = bar.append_child(&self.dropdown(
                    self.t("drawing.bar.lineWidth", "Line width"),
                    vec![json!(1), json!(2), json!(3), json!(4)],
                    get,
                    Rc::new(|w| glyphs::line_icon(w.as_f64().unwrap_or(2.0), "solid")),
                    self.patch1("style.lineWidth"),
                    DropOpts {
                        label: Some(Rc::new(|v| format!("{}px", val_text(v)))),
                        in_trigger: true,
                    },
                ));
            }
        }
        if has("style.lineStyle") {
            let values: Vec<Value> = line_style_options()
                .iter()
                .map(|o| json!(o.value))
                .collect();
            let get = self.cv(|s| {
                json!(
                    s.read_str("style.lineStyle")
                        .unwrap_or_else(|| "solid".into())
                )
            });
            let _ = bar.append_child(&self.style_dropdown(
                self.t("drawing.bar.lineStyle", "Line style"),
                values,
                get,
                "style.lineStyle",
            ));
        }
        // The Fill swatch starts from the color actually painted (validity tint, line-color
        // wash, background fallback), the same source the renderer fills with.
        if has("style.fillColor") {
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.fill", "Fill"),
                "bucket",
                |s| {
                    s.fill
                        .clone()
                        .or_else(|| s.doc.style.fill_color.clone())
                        .unwrap_or_else(|| DEFAULT_DRAWING_COLOR.to_string())
                },
                "style.fillColor",
            ));
        }
        // Position tool: zone colors sit on the bar; risk / reward numbers and the display
        // toggles live in the settings panel.
        if has("riskPercent") && every(&|s| s.type_key() == "position") {
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.profitZone", "Profit zone"),
                "bucket",
                |s| s.read_str("profitColor").unwrap_or_default(),
                "profitColor",
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.lossZone", "Loss zone"),
                "bucket",
                |s| s.read_str("lossColor").unwrap_or_default(),
                "lossColor",
            ));
        }
        // Regression channel: per-line color + style, the two area fills, the R² toggle.
        if has("reg.midColor") && every(&|s| s.read("reg.midColor").is_some()) {
            let styles: Vec<Value> = line_style_options()
                .iter()
                .map(|o| json!(o.value))
                .collect();
            let pair = |bar: &Element,
                        color_tip: String,
                        color_path: &'static str,
                        style_tip: String,
                        style_path: &'static str| {
                let _ = bar.append_child(&self.swatch(
                    color_tip,
                    "brush",
                    move |s| s.read_str(color_path).unwrap_or_default(),
                    color_path,
                ));
                let get = self
                    .cv(move |s| json!(s.read_str(style_path).unwrap_or_else(|| "solid".into())));
                let _ = bar.append_child(&self.style_dropdown(
                    style_tip,
                    styles.clone(),
                    get,
                    style_path,
                ));
            };
            pair(
                &bar,
                self.t("drawing.bar.midlineColor", "Midline color"),
                "reg.midColor",
                self.t("drawing.bar.midlineStyle", "Midline style"),
                "reg.midStyle",
            );
            pair(
                &bar,
                self.t("drawing.bar.upperLineColor", "Upper line color"),
                "reg.upperColor",
                self.t("drawing.bar.upperLineStyle", "Upper line style"),
                "reg.upperStyle",
            );
            pair(
                &bar,
                self.t("drawing.bar.lowerLineColor", "Lower line color"),
                "reg.lowerColor",
                self.t("drawing.bar.lowerLineStyle", "Lower line style"),
                "reg.lowerStyle",
            );
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.upperFill", "Upper fill"),
                "bucket",
                |s| s.read_str("reg.upperFill").unwrap_or_default(),
                "reg.upperFill",
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.lowerFill", "Lower fill"),
                "bucket",
                |s| s.read_str("reg.lowerFill").unwrap_or_default(),
                "reg.lowerFill",
            ));
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.showRSquared", "Show R\u{b2}"),
                "r-squared",
                None,
                self.cv(|s| s.read_bool("reg.showR2").unwrap_or(false)),
                self.bool_patch("reg.showR2"),
            ));
        }
        // Anchored VWAP: midline color + style, band multiplier, band-line colors, the fill.
        if has("vwap.midColor") && every(&|s| s.read("vwap.midColor").is_some()) {
            let styles: Vec<Value> = line_style_options()
                .iter()
                .map(|o| json!(o.value))
                .collect();
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.vwapColor", "VWAP color"),
                "brush",
                |s| s.read_str("vwap.midColor").unwrap_or_default(),
                "vwap.midColor",
            ));
            let get = self.cv(|s| {
                json!(
                    s.read_str("vwap.midStyle")
                        .unwrap_or_else(|| "solid".into())
                )
            });
            let _ = bar.append_child(&self.style_dropdown(
                self.t("drawing.bar.vwapStyle", "VWAP style"),
                styles,
                get,
                "vwap.midStyle",
            ));
            let mults: Vec<Value> = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0]
                .iter()
                .map(|m| json!(m))
                .collect();
            let get = self.cv(|s| {
                s.read_f64("vwap.multiplier")
                    .map(|m| (m * 1000.0).round() as i64)
            });
            let g = get.clone();
            let get: Get<Common<Value>> = Rc::new(move || match g() {
                Common::One(Some(m)) => Common::One(json!(m as f64 / 1000.0)),
                _ => Common::Mixed,
            });
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.bandMultiplier", "Band multiplier"),
                mults,
                get,
                Rc::new(|_| glyph("bands")),
                self.patch1("vwap.multiplier"),
                DropOpts {
                    label: Some(Rc::new(|v| format!("{}\u{3c3}", val_text(v)))),
                    in_trigger: true,
                },
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.upperBandColor", "Upper band color"),
                "brush",
                |s| s.read_str("vwap.upperColor").unwrap_or_default(),
                "vwap.upperColor",
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.lowerBandColor", "Lower band color"),
                "brush",
                |s| s.read_str("vwap.lowerColor").unwrap_or_default(),
                "vwap.lowerColor",
            ));
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.bandFill", "Band fill"),
                "bucket",
                |s| s.read_str("vwap.bandFill").unwrap_or_default(),
                "vwap.bandFill",
            ));
        }
        // Dedekind tessellation: max circle curvature (the tessellation density).
        if has("maxCurvature") {
            let values: Vec<Value> = DEDEKIND_CURVATURE_OPTIONS
                .iter()
                .map(|v| json!(v))
                .collect();
            let get = self.cv(|s| json!(s.read_f64("maxCurvature").unwrap_or(24.0)));
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.maxCurvature", "Max curvature"),
                values,
                get,
                Rc::new(|_| glyph("dedekind")),
                self.patch1("maxCurvature"),
                DropOpts {
                    label: Some(Rc::new(|v| format!("n={}", val_text(v)))),
                    in_trigger: true,
                },
            ));
        }
        // Mach figures: the wave count and the (supersonic) Mach number.
        if has("mach") {
            let values: Vec<Value> = MACH_NUMBER_OPTIONS.iter().map(|v| json!(v)).collect();
            let get = self.cv(|s| json!(s.read_f64("mach").unwrap_or(2.0)));
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.machNumber", "Mach number"),
                values,
                get,
                Rc::new(|_| glyph("supersonic")),
                self.patch1("mach"),
                DropOpts {
                    label: Some(Rc::new(|v| format!("M={}", val_text(v)))),
                    in_trigger: true,
                },
            ));
        }
        if has("waveCount") {
            let values: Vec<Value> = MACH_WAVE_COUNT_OPTIONS.iter().map(|v| json!(v)).collect();
            let get = self.cv(|s| json!(s.read_f64("waveCount").unwrap_or(6.0)));
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.waves", "Waves"),
                values,
                get,
                Rc::new(|_| glyph("sonic")),
                self.patch1("waveCount"),
                DropOpts {
                    label: Some(Rc::new(val_text)),
                    in_trigger: true,
                },
            ));
        }
        // Range toggles, and the text styling of drawings whose label is computed.
        if has("showPrice") {
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.showPrice", "Show price"),
                "price-delta",
                None,
                self.cv(|s| s.read_bool("showPrice") != Some(false)),
                self.bool_patch("showPrice"),
            ));
        }
        if has("showDate") {
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.showDate", "Show date"),
                "date-delta",
                None,
                self.cv(|s| s.read_bool("showDate") != Some(false)),
                self.bool_patch("showDate"),
            ));
        }
        if has("text.color") && text_on_bar {
            let tc = self.core.theme_color("textColor").unwrap_or_default();
            let _ = bar.append_child(&self.swatch(
                self.t("drawing.bar.textColor", "Text color"),
                "type",
                move |s| {
                    s.doc
                        .text
                        .as_ref()
                        .and_then(|t| t.color.clone())
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| tc.clone())
                },
                "text.color",
            ));
        }
        if has("text.size") && text_on_bar {
            let _ = bar.append_child(&self.text_size_dropdown(
                self.t("drawing.bar.textSize", "Text size"),
                glyphs::label_size_icon,
            ));
        }
        // Bold / italic live under the text field; a computed label (or a multi-selection)
        // has no field, so they go on the bar.
        let text_field = !multi && has("text.value");
        if has("text.bold") && !text_field {
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.bold", "Bold"),
                "bold",
                None,
                self.cv(|s| s.doc.text.as_ref().is_some_and(|t| t.bold == Some(true))),
                self.bool_patch("text.bold"),
            ));
        }
        if has("text.italic") && !text_field {
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.italic", "Italic"),
                "italic",
                None,
                self.cv(|s| s.doc.text.as_ref().is_some_and(|t| t.italic == Some(true))),
                self.bool_patch("text.italic"),
            ));
        }
        if text_field {
            let me = self.me.clone();
            let with_color_and_size = !text_on_bar;
            let _ = bar.append_child(&self.icon_btn(
                self.t("drawing.bar.text", "Text"),
                "type",
                move || {
                    if let Some(p) = me.upgrade() {
                        p.toggle_text_panel(with_color_and_size);
                    }
                },
            ));
        }
        if first.levels.is_some() && every(&|s| s.levels.is_some() && !s.is_mach) {
            let sizes: Vec<Value> = text_size_options().iter().map(|o| json!(o.value)).collect();
            let me = self.me.clone();
            let label: Rc<dyn Fn(&Value) -> String> =
                Rc::new(move |v| me.upgrade().map(|p| p.size_label(v)).unwrap_or_default());
            let get =
                self.cv(|s| json!(s.read_str("numbersSize").unwrap_or_else(|| "small".into())));
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.numbersSize", "Numbers size"),
                sizes.clone(),
                get,
                Rc::new(|s| glyphs::numbers_size_icon(&val_text(s))),
                self.patch1("numbersSize"),
                DropOpts {
                    label: Some(label.clone()),
                    in_trigger: false,
                },
            ));
            let get =
                self.cv(|s| json!(s.read_str("labelsSize").unwrap_or_else(|| "normal".into())));
            let _ = bar.append_child(&self.dropdown(
                self.t("drawing.bar.labelSize", "Label size"),
                sizes,
                get,
                Rc::new(|s| glyphs::label_size_icon(&val_text(s))),
                self.patch1("labelsSize"),
                DropOpts {
                    label: Some(label),
                    in_trigger: false,
                },
            ));
        }

        // The trailing group: the settings gear just left of the lock, the kebab overflow
        // just right of delete. The panel edits one drawing, so it stays off a multi-selection.
        let _ = bar.append_child(&self.divider());
        if !multi {
            let me = self.me.clone();
            let _ = bar.append_child(&self.icon_btn(
                self.t("drawing.bar.settings", "Settings"),
                "gear",
                move || {
                    if let Some(p) = me.upgrade()
                        && let Some(ui) = p.ui.upgrade()
                    {
                        ui.open_panel();
                    }
                },
            ));
        }
        {
            let core = self.core.clone();
            let _ = bar.append_child(&self.toggle(
                self.t("drawing.bar.lock", "Lock"),
                "lock",
                Some("unlock"),
                self.cv(|s| s.doc.locked),
                Rc::new(move |v| core.set_locked(v)),
            ));
        }
        {
            let core = self.core.clone();
            let del = self.icon_btn(self.t("drawing.bar.delete", "Delete"), "trash", move || {
                core.remove()
            });
            set_style(&del, "color", "var(--vela-danger, #f23645)");
            let _ = bar.append_child(&del);
        }
        let _ = bar.append_child(&self.kebab());

        let _ = el.append_child(&bar);
        // The hover label: a floating tip that flips above / below the bar.
        {
            let me = self.me.clone();
            self.cx.on(el, "mouseover", move |ev| {
                let Some(p) = me.upgrade() else { return };
                match util::ancestor_with_attr(&get_prop(&ev, "target"), &p.el, "data-tip") {
                    Some((target, _)) => p.show_tip(&target),
                    None => p.hide_tip(),
                }
            });
        }
        {
            let me = self.me.clone();
            self.cx.on(el, "mouseleave", move |_| {
                if let Some(p) = me.upgrade() {
                    p.hide_tip();
                }
            });
        }
    }

    fn bool_patch(&self, path: &'static str) -> Rc<dyn Fn(bool)> {
        let core = self.core.clone();
        Rc::new(move |v| core.patch(&[(path, json!(v))]))
    }

    fn divider(&self) -> Element {
        let d = self.cx.el("span", "vela-dpop-sep");
        attr(&d, "role", "separator");
        d
    }

    /// The line-style dropdown (glyph per style, text rows).
    fn style_dropdown(
        &self,
        tip: String,
        values: Vec<Value>,
        get: Get<Common<Value>>,
        path: &'static str,
    ) -> Element {
        let me = self.me.clone();
        self.dropdown(
            tip,
            values,
            get,
            Rc::new(|s| glyphs::line_icon(2.0, &val_text(s))),
            self.patch1(path),
            DropOpts {
                label: Some(Rc::new(move |v| {
                    me.upgrade().map(|p| p.style_label(v)).unwrap_or_default()
                })),
                in_trigger: false,
            },
        )
    }

    fn text_size_dropdown(&self, tip: String, icon: fn(&str) -> String) -> Element {
        let values: Vec<Value> = text_size_options().iter().map(|o| json!(o.value)).collect();
        let get = self.cv(|s| {
            json!(
                s.doc
                    .text
                    .as_ref()
                    .map_or("normal".to_string(), |t| size_key(&t.size))
            )
        });
        let me = self.me.clone();
        self.dropdown(
            tip,
            values,
            get,
            Rc::new(move |s| icon(&val_text(s))),
            self.patch1("text.size"),
            DropOpts {
                label: Some(Rc::new(move |v| {
                    me.upgrade().map(|p| p.size_label(v)).unwrap_or_default()
                })),
                in_trigger: false,
            },
        )
    }

    // ── controls ──

    fn base_btn(&self, tip: &str) -> Element {
        let b = self.cx.el("button", "vela-dpop-btn");
        attr(&b, "type", "button");
        // The custom tip strip reads this (no native `title`: that tooltip is noisy).
        attr(&b, "data-tip", tip);
        attr(&b, "aria-label", tip);
        b
    }

    fn icon_btn(&self, tip: String, icon: &str, on_click: impl Fn() + 'static) -> Element {
        let b = self.base_btn(&tip);
        b.set_inner_html(&sized(&glyph(icon), ICON));
        self.cx.on(&b, "click", move |_| on_click());
        b
    }

    /// An on / off button. A mixed state (the drawings disagree) reads as half lit and the
    /// first click turns it ON for all of them. `off` swaps in a distinct glyph while the
    /// toggle is off (a lock reads as an open padlock until it is engaged).
    fn toggle(
        &self,
        tip: String,
        icon: &str,
        off: Option<&str>,
        get: Get<Common<bool>>,
        on_change: Rc<dyn Fn(bool)>,
    ) -> Element {
        let b = self.base_btn(&tip);
        let on_glyph = sized(&glyph(icon), ICON);
        let off_glyph = off.map(|o| sized(&glyph(o), ICON));
        let state = Rc::new(RefCell::new(get()));
        let paint = {
            let b = b.clone();
            Rc::new(move |v: &Common<bool>| {
                let (active, pressed) = match v {
                    Common::Mixed => ("mixed", "mixed"),
                    Common::One(true) => ("1", "true"),
                    Common::One(false) => ("0", "false"),
                };
                attr(&b, "data-active", active);
                attr(&b, "aria-pressed", pressed);
                let g = match (v, &off_glyph) {
                    (Common::One(false), Some(o)) => o,
                    _ => &on_glyph,
                };
                b.set_inner_html(g);
            })
        };
        paint(&state.borrow());
        {
            let (state, paint) = (state.clone(), paint.clone());
            self.cx.on(&b, "click", move |_| {
                let next = match &*state.borrow() {
                    Common::Mixed => true,
                    Common::One(v) => !*v,
                };
                *state.borrow_mut() = Common::One(next);
                paint(&Common::One(next));
                on_change(next);
            });
        }
        self.register(Rc::new(move || {
            let v = get();
            *state.borrow_mut() = v.clone();
            paint(&v);
        }));
        b
    }

    /// The bar's move handle: press and drag it to move the whole bar around the chart. A
    /// quiet dim glyph with no hover fill, so it reads as chrome.
    fn grip(&self) -> Element {
        let b = self.cx.el("button", "vela-dpop-grip");
        attr(&b, "type", "button");
        attr(&b, "aria-label", &self.t("drawing.bar.move", "Move"));
        b.set_inner_html(&sized(&glyph("grip"), ICON));
        {
            let me = self.me.clone();
            let grip = b.clone();
            self.cx.on(&b, "pointerdown", move |ev| {
                let Some(p) = me.upgrade() else { return };
                if get_f64(&ev, "button").unwrap_or(0.0) != 0.0 {
                    return;
                }
                prevent_default(&ev);
                // Neither pre-close a popover nor dismiss the bar.
                stop_propagation(&ev);
                p.hide_tip();
                p.close_float();
                p.start_drag(&grip, &ev);
            });
        }
        {
            // The keyboard moves it too: arrow keys, Shift for bigger steps.
            let me = self.me.clone();
            self.cx.on(&b, "keydown", move |ev| {
                let Some(p) = me.upgrade() else { return };
                let step = if get_bool(&ev, "shiftKey") { 40.0 } else { 8.0 };
                let (dx, dy) = match get_str(&ev, "key").as_str() {
                    "ArrowLeft" => (-step, 0.0),
                    "ArrowRight" => (step, 0.0),
                    "ArrowUp" => (0.0, -step),
                    "ArrowDown" => (0.0, step),
                    _ => return,
                };
                prevent_default(&ev);
                let (x, y) = util::placed(&p.el);
                p.move_to(x + dx, y + dy);
            });
        }
        b
    }

    fn start_drag(&self, grip: &Element, ev: &JsValue) {
        let (orig_x, orig_y) = util::placed(&self.el);
        let start = (
            get_f64(ev, "clientX").unwrap_or(0.0),
            get_f64(ev, "clientY").unwrap_or(0.0),
        );
        set_style(grip, "cursor", "grabbing");
        // Capture keeps the move stream on the handle even when a finger slides off it.
        if let Some(id) = get_f64(ev, "pointerId") {
            vela_capture_pointer(grip, id as i32);
        }
        let Some(view) = default_view(&self.cx.doc) else {
            return;
        };
        let mut listeners = Vec::new();
        // A released drag stays attached until the timer frees it: it must not move the bar.
        let live = Rc::new(std::cell::Cell::new(true));
        {
            let (me, live) = (self.me.clone(), live.clone());
            listeners.push(Listener::new(
                &view,
                "pointermove",
                false,
                false,
                move |ev| {
                    let Some(p) = me.upgrade() else { return };
                    if !live.get() {
                        return;
                    }
                    let nx = orig_x + (get_f64(&ev, "clientX").unwrap_or(0.0) - start.0);
                    let ny = orig_y + (get_f64(&ev, "clientY").unwrap_or(0.0) - start.1);
                    p.move_to(nx, ny);
                },
            ));
        }
        for kind in ["pointerup", "pointercancel"] {
            let me = self.me.clone();
            let grip = grip.clone();
            let pointer = get_f64(ev, "pointerId");
            let live = live.clone();
            listeners.push(Listener::new(&view, kind, false, false, move |_| {
                let Some(p) = me.upgrade() else { return };
                set_style(&grip, "cursor", "grab");
                if let Some(id) = pointer {
                    vela_release_pointer(&grip, id as i32);
                }
                live.set(false);
                // Freed from a timer: this handler is one of them.
                let old: Vec<Listener> = p.drag.borrow_mut().drain(..).collect();
                if let Some(ui) = p.ui.upgrade() {
                    ui.retire(old);
                }
            }));
        }
        *self.drag.borrow_mut() = listeners;
    }

    /// Move the bar to `(x, y)` px, kept inside the chart.
    fn move_to(&self, x: f64, y: f64) {
        let (x, y) = model::clamp_into(x, y, self.size(), self.host_size());
        util::place(&self.el, x, y);
    }

    fn host_size(&self) -> (f64, f64) {
        let r = self.wrapper.get_bounding_client_rect();
        (r.width(), r.height())
    }

    fn size(&self) -> (f64, f64) {
        let r = self.el.get_bounding_client_rect();
        if r.width() > 0.0 && r.height() > 0.0 {
            (r.width(), r.height())
        } else {
            FALLBACK_SIZE
        }
    }

    /// Where the bar first sits: clear of the drawing, above it unless there is no room.
    fn position(&self, anchor: Option<(f64, f64, f64, f64)>) {
        let (x, y) = model::bar_origin(anchor, self.size(), self.host_size(), GAP);
        util::place(&self.el, x, y);
    }

    /// Keep the bar where it is when its size changes (a panel toggled): grow in place and
    /// only nudge back into view.
    fn reposition(&self) {
        let (x, y) = util::placed(&self.el);
        self.move_to(x, y);
    }

    // ── the hover label ──

    fn show_tip(&self, target: &Element) {
        let Some(text) = target.get_attribute("data-tip") else {
            self.hide_tip();
            return;
        };
        let tip = {
            let mut slot = self.tip.borrow_mut();
            slot.get_or_insert_with(|| {
                let tp = self.cx.el("div", "vela-dpop-tip");
                let _ = self.wrapper.append_child(&tp);
                tp
            })
            .clone()
        };
        tip.set_text_content(Some(&text));
        let (tx, ty, tw, th) = util::rect_in(target, &self.wrapper);
        let r = tip.get_bounding_client_rect();
        let (w, h) = (
            if r.width() > 0.0 { r.width() } else { 80.0 },
            if r.height() > 0.0 { r.height() } else { 22.0 },
        );
        let (host_w, _) = self.host_size();
        let mut left = tx + tw / 2.0 - w / 2.0;
        if host_w > 0.0 {
            left = left.min(host_w - w - 4.0);
        }
        left = left.max(4.0);
        // Hug the hovered control: just above it, below only when there is no room above.
        let above = ty - h - 6.0;
        let top = if above >= 4.0 {
            above
        } else {
            ty + if th > 0.0 { th } else { BTN } + 6.0
        };
        util::place(&tip, left, top);
    }

    fn hide_tip(&self) {
        let taken_1 = self.tip.borrow_mut().take();
        if let Some(tip) = taken_1 {
            tip.remove();
        }
    }

    // ── floating shells (menus, the color popover) ──

    /// A floating shell anchored on `anchor`, inside the chart (see [`FloatHost`]).
    fn open_float(
        &self,
        anchor: &Element,
        align_end: bool,
        z: u32,
        padding: &str,
        fill: impl FnOnce(&Element, &Cx),
    ) {
        self.floats
            .open(anchor, align_end, z, "", padding, None, fill);
    }

    fn float_owner_is(&self, anchor: &Element) -> bool {
        self.floats.owned_by(anchor)
    }

    /// Close the floating shell, if any; its listeners are freed from a timer.
    fn close_float(&self) {
        self.floats.close();
    }

    /// A pick-one dropdown: the trigger shows the current value's glyph (plus an optional
    /// text label for controls that would otherwise look alike), and clicking it opens a
    /// floating list of every option. A mixed value shows a dash and highlights no row.
    fn dropdown(
        &self,
        tip: String,
        values: Vec<Value>,
        get: Get<Common<Value>>,
        render: Rc<dyn Fn(&Value) -> String>,
        on_change: Rc<dyn Fn(&Value)>,
        opts: DropOpts,
    ) -> Element {
        let b = self.base_btn(&tip);
        set_styles(
            &b,
            &[
                ("width", "auto"),
                ("min-width", &format!("{BTN}px")),
                ("padding", "0 4px"),
                ("gap", "2px"),
            ],
        );
        attr(&b, "aria-haspopup", "menu");
        attr(&b, "aria-expanded", "false");
        let state = Rc::new(RefCell::new(get()));
        let label = opts.label.clone();
        let in_trigger = opts.in_trigger;
        let paint = {
            let (b, render, label) = (b.clone(), render.clone(), label.clone());
            Rc::new(move |v: &Common<Value>| {
                let mut html = String::new();
                match v {
                    Common::Mixed => {
                        html.push_str("<span class=\"vela-dpop-dash\">\u{2014}</span>")
                    }
                    Common::One(v) => {
                        let g = render(v);
                        if !g.is_empty() {
                            html.push_str(&format!(
                                "<span class=\"vela-dpop-g\">{}</span>",
                                sized(&g, ICON)
                            ));
                        }
                        if let (Some(label), true) = (&label, in_trigger) {
                            html.push_str(&format!(
                                "<span class=\"vela-dpop-lab\">{}</span>",
                                glyphs::escape(&label(v))
                            ));
                        }
                    }
                }
                html.push_str(&format!(
                    "<span class=\"vela-dpop-chev\">{}</span>",
                    sized(&glyph("chevron-down"), 12)
                ));
                b.set_inner_html(&html);
            })
        };
        paint(&state.borrow());
        {
            let me = self.me.clone();
            let (b2, state, paint) = (b.clone(), state.clone(), paint.clone());
            // A trigger stops its own press so a re-click can toggle the menu shut below.
            self.cx
                .on(&b, "pointerdown", move |ev| stop_propagation(&ev));
            self.cx.on(&b, "click", move |ev| {
                stop_propagation(&ev);
                let Some(p) = me.upgrade() else { return };
                if p.float_owner_is(&b2) {
                    p.close_float();
                    return;
                }
                let current = state.borrow().clone();
                let (values, render, label) = (values.clone(), render.clone(), label.clone());
                let (state, paint, on_change, me2) = (
                    state.clone(),
                    paint.clone(),
                    on_change.clone(),
                    p.me.clone(),
                );
                p.open_float(&b2, false, 26, "4px", move |menu, cx| {
                    attr(menu, "role", "menu");
                    for v in &values {
                        let active = matches!(&current, Common::One(c) if loose_eq(c, v));
                        let item = cx.el("button", "vela-dpop-item");
                        attr(&item, "type", "button");
                        attr(&item, "role", "menuitemradio");
                        attr(&item, "aria-checked", if active { "true" } else { "false" });
                        attr(&item, "data-active", if active { "1" } else { "0" });
                        let g = render(v);
                        let text = label.as_ref().map(|l| l(v));
                        if label.is_some() {
                            set_style(&item, "min-width", "118px");
                        }
                        attr(
                            &item,
                            "aria-label",
                            &text.clone().unwrap_or_else(|| val_text(v)),
                        );
                        let mut html = String::new();
                        if !g.is_empty() {
                            html.push_str(&format!(
                                "<span class=\"vela-dpop-g vela-dpop-g--row\">{}</span>",
                                sized(&g, 18)
                            ));
                        }
                        if let Some(text) = &text {
                            html.push_str(&format!(
                                "<span class=\"vela-dpop-lab vela-dpop-lab--row\">{}</span>",
                                glyphs::escape(text)
                            ));
                        }
                        item.set_inner_html(&html);
                        let (me, state, paint, on_change, v) = (
                            me2.clone(),
                            state.clone(),
                            paint.clone(),
                            on_change.clone(),
                            v.clone(),
                        );
                        cx.on(&item, "click", move |ev| {
                            stop_propagation(&ev);
                            if let Some(p) = me.upgrade() {
                                p.close_float();
                            }
                            *state.borrow_mut() = Common::One(v.clone());
                            paint(&Common::One(v.clone()));
                            on_change(&v);
                        });
                        let _ = menu.append_child(&item);
                    }
                });
            });
        }
        self.register(Rc::new(move || {
            let v = get();
            *state.borrow_mut() = v.clone();
            paint(&v);
        }));
        b
    }

    /// A color control: an icon over a thin colored underline showing the current value,
    /// with the native swatch and opacity slider in a popover to edit it. A mixed color
    /// paints the underline striped with the colors in use, and the popover then leads with
    /// those colors so unifying onto one of them is a single click.
    fn color_button(
        &self,
        tip: String,
        icon: &str,
        icon_size: u32,
        get: Get<Common<String>>,
        used: Get<Vec<String>>,
        path: &'static str,
    ) -> Element {
        let b = self.base_btn(&tip);
        set_styles(&b, &[("flex-direction", "column"), ("gap", "2px")]);
        attr(&b, "aria-haspopup", "dialog");
        attr(&b, "aria-expanded", "false");
        let ic = self.cx.el("span", "vela-dpop-g");
        ic.set_inner_html(&sized(&glyph(icon), icon_size));
        let under = self.cx.el("span", "vela-dpop-under");
        set_style(
            &under,
            "width",
            &format!("{}px", (f64::from(icon_size) * 0.85).round()),
        );
        let _ = b.append_child(&ic);
        let _ = b.append_child(&under);
        let state = Rc::new(RefCell::new(get()));
        let paint = {
            let (under, used) = (under.clone(), used.clone());
            Rc::new(move |v: &Common<String>| {
                let bg = match v {
                    Common::One(c) => c.clone(),
                    Common::Mixed => stripes(&used()),
                };
                set_style(&under, "background", &bg);
            })
        };
        paint(&state.borrow());
        {
            let me = self.me.clone();
            let (b2, state, paint, used) = (b.clone(), state.clone(), paint.clone(), used.clone());
            let core = self.core.clone();
            self.cx
                .on(&b, "pointerdown", move |ev| stop_propagation(&ev));
            self.cx.on(&b, "click", move |ev| {
                stop_propagation(&ev);
                let Some(p) = me.upgrade() else { return };
                if p.float_owner_is(&b2) {
                    p.close_float();
                    return;
                }
                // A mixed picker starts from the first color in use: some real value must
                // seed the swatch.
                let in_use = used();
                let mixed = state.borrow().is_mixed();
                let seed = match &*state.borrow() {
                    Common::One(c) => c.clone(),
                    Common::Mixed => in_use
                        .first()
                        .cloned()
                        .unwrap_or_else(|| DEFAULT_DRAWING_COLOR.to_string()),
                };
                let cur = Rc::new(RefCell::new(seed));
                let (state, paint, core, me2) =
                    (state.clone(), paint.clone(), core.clone(), p.me.clone());
                let inuse_label = p.t("drawing.bar.inUse", "In use");
                let aria = tip.clone();
                p.open_float(&b2, false, 25, "10px", move |el, cx| {
                    attr(el, "role", "dialog");
                    attr(el, "aria-label", &aria);
                    if mixed && in_use.len() > 1 {
                        let row = cx.el("div", "vela-dpop-inuse");
                        let lbl = cx.text("span", "vela-dpop-inuse-l", &inuse_label);
                        let _ = row.append_child(&lbl);
                        for c in &in_use {
                            let sw = cx.el("button", "vela-dpop-chip");
                            attr(&sw, "type", "button");
                            attr(&sw, "data-tip", c);
                            attr(&sw, "aria-label", c);
                            set_style(&sw, "background", c);
                            let (me, state, paint, core, c) = (
                                me2.clone(),
                                state.clone(),
                                paint.clone(),
                                core.clone(),
                                c.clone(),
                            );
                            cx.on(&sw, "click", move |ev| {
                                stop_propagation(&ev);
                                if let Some(p) = me.upgrade() {
                                    p.close_float();
                                }
                                *state.borrow_mut() = Common::One(c.clone());
                                paint(&Common::One(c.clone()));
                                core.patch(&[(path, json!(c))]);
                            });
                            let _ = row.append_child(&sw);
                        }
                        let _ = el.append_child(&row);
                    }
                    let (get_cur, set_cur) = (cur.clone(), cur.clone());
                    let field = controls::color_field(
                        cx,
                        &cx.uid("vela-dpop-color"),
                        &aria,
                        true,
                        Rc::new(move || get_cur.borrow().clone()),
                        Rc::new(move |v| {
                            *set_cur.borrow_mut() = v.clone();
                            *state.borrow_mut() = Common::One(v.clone());
                            paint(&Common::One(v.clone()));
                            core.patch(&[(path, json!(v))]);
                        }),
                    );
                    let _ = el.append_child(&field.el);
                });
            });
        }
        self.register(Rc::new(move || {
            let v = get();
            *state.borrow_mut() = v.clone();
            paint(&v);
        }));
        b
    }

    /// An inline numeric width field for tools whose stroke range outgrows the 1-4px ladder:
    /// the value is clamped to the schema's range on commit. A mixed value shows an empty
    /// field with a dash until a number is typed for all.
    fn width_input(
        &self,
        tip: String,
        get: Get<Common<i64>>,
        min: f64,
        max: f64,
        step: f64,
    ) -> Element {
        let wrap = self.cx.el("span", "vela-dpop-num");
        attr(&wrap, "data-tip", &tip);
        let read: Rc<dyn Fn() -> f64> = {
            let get = get.clone();
            Rc::new(move || match get() {
                Common::One(w) => w as f64 / 1000.0,
                Common::Mixed => min,
            })
        };
        let core = self.core.clone();
        let ctl = controls::number(
            &self.cx,
            &self.cx.uid("vela-dpop-width"),
            controls::NumOpts {
                min: Some(min),
                max: Some(max),
                step: Some(step),
                integer: step >= 1.0,
                compact: true,
                clamp: true,
                aria: tip.clone(),
                ..Default::default()
            },
            read,
            Rc::new(move |v| core.patch(&[("style.lineWidth", json!(v))])),
        );
        let mixed_view = {
            let (el, get) = (ctl.el.clone(), get.clone());
            Rc::new(move || {
                if get().is_mixed() {
                    crate::dom::vela_set_value(&el, "");
                    attr(&el, "placeholder", "\u{2014}");
                }
            })
        };
        mixed_view();
        // Typing in a bar field must not reach the chart (Delete would remove the drawing).
        self.cx
            .on(&ctl.el, "keydown", move |ev| stop_propagation(&ev));
        let sync = ctl.sync.clone();
        let focus_el = ctl.el.clone();
        self.register(Rc::new(move || {
            if active_is(&focus_el) {
                return;
            }
            sync();
            mixed_view();
        }));
        let _ = wrap.append_child(&ctl.el);
        wrap
    }

    /// The trailing kebab overflow: duplicate, z-order actions, reset settings.
    fn kebab(&self) -> Element {
        let b = self.base_btn(&self.t("drawing.bar.more", "More"));
        b.set_inner_html(&sized(&glyph("kebab"), ICON));
        attr(&b, "aria-haspopup", "menu");
        attr(&b, "aria-expanded", "false");
        let me = self.me.clone();
        let core = self.core.clone();
        let b2 = b.clone();
        self.cx
            .on(&b, "pointerdown", move |ev| stop_propagation(&ev));
        self.cx.on(&b, "click", move |ev| {
            stop_propagation(&ev);
            let Some(p) = me.upgrade() else { return };
            if p.float_owner_is(&b2) {
                p.close_float();
                return;
            }
            type Act = Rc<dyn Fn()>;
            let rows: Vec<(&'static str, String, Act)> = vec![
                ("clone", p.t("drawing.bar.duplicate", "Duplicate"), {
                    let c = core.clone();
                    Rc::new(move || c.duplicate())
                }),
                (
                    "bring-front",
                    p.t("drawing.bar.bringToFront", "Bring to front"),
                    {
                        let c = core.clone();
                        Rc::new(move || c.reorder(ReorderTo::Front))
                    },
                ),
                (
                    "send-back",
                    p.t("drawing.bar.sendToBack", "Send to back"),
                    {
                        let c = core.clone();
                        Rc::new(move || c.reorder(ReorderTo::Back))
                    },
                ),
                (
                    "reset",
                    p.t("drawing.bar.resetSettings", "Reset settings"),
                    {
                        let c = core.clone();
                        Rc::new(move || c.reset_settings())
                    },
                ),
            ];
            let me2 = p.me.clone();
            p.open_float(&b2, true, 26, "4px", move |menu, cx| {
                attr(menu, "role", "menu");
                for (icon, label, act) in rows {
                    let item = cx.el("button", "vela-dpop-item vela-dpop-item--action");
                    attr(&item, "type", "button");
                    attr(&item, "role", "menuitem");
                    let ic = cx.el("span", "vela-dpop-g vela-dpop-g--row");
                    ic.set_inner_html(&sized(&glyph(icon), 18));
                    let tx = cx.text("span", "vela-dpop-lab vela-dpop-lab--row", &label);
                    let _ = item.append_child(&ic);
                    let _ = item.append_child(&tx);
                    let me = me2.clone();
                    cx.on(&item, "click", move |ev| {
                        stop_propagation(&ev);
                        if let Some(p) = me.upgrade() {
                            p.close_float();
                        }
                        act();
                    });
                    let _ = menu.append_child(&item);
                }
            });
        });
        b
    }

    // ── the text editor under the bar ──

    /// The expandable text editor (the Text button): the field itself, with bold / italic
    /// under it, plus color and size when those aren't already on the bar.
    fn toggle_text_panel(&self, with_color_and_size: bool) {
        let taken_2 = self.text_panel.borrow_mut().take();
        if let Some(panel) = taken_2 {
            panel.remove();
            self.reposition();
            return;
        }
        self.close_float();
        let Some(first) = self.core.first() else {
            return;
        };
        let panel = self.cx.el("div", "vela-dpop-text");
        let core = self.core.clone();
        let text_value = {
            let core = self.core.clone();
            Rc::new(move || {
                core.first()
                    .and_then(|s| s.doc.text.map(|t| t.value))
                    .unwrap_or_default()
            })
        };
        let ta = controls::textarea(
            &self.cx,
            &self.cx.uid("vela-dpop-text"),
            &self.t("drawing.bar.label", "Label\u{2026}"),
            text_value.clone(),
            {
                let core = core.clone();
                Rc::new(move |v| core.patch(&[("text.value", json!(v))]))
            },
        );
        attr(&ta.el, "rows", "1");
        attr(
            &ta.el,
            "placeholder",
            &self.t("drawing.bar.label", "Label\u{2026}"),
        );
        set_styles(&ta.el, &[("min-width", "220px"), ("min-height", "30px")]);
        util::auto_grow(&ta.el, 4);
        {
            let (el, core, me) = (ta.el.clone(), core.clone(), self.me.clone());
            self.cx.on(&ta.el, "input", move |_| {
                core.patch(&[("text.value", json!(vela_get_value(&el)))]);
                util::auto_grow(&el, 4);
                if let Some(p) = me.upgrade() {
                    p.reposition();
                }
            });
        }
        {
            let me = self.me.clone();
            self.cx.on(&ta.el, "keydown", move |ev| {
                stop_propagation(&ev);
                if get_str(&ev, "key") == "Escape" {
                    prevent_default(&ev);
                    if let Some(ui) = me.upgrade().and_then(|p| p.ui.upgrade()) {
                        ui.popup_dismiss(false);
                    }
                }
            });
        }
        {
            let (sync, el) = (ta.sync.clone(), ta.el.clone());
            self.register(Rc::new(move || {
                if !active_is(&el) {
                    sync();
                }
            }));
        }
        let tools = self.cx.el("div", "vela-dpop-tools");
        if with_color_and_size {
            let tc = self.core.theme_color("textColor").unwrap_or_default();
            let color = self.color_button(
                self.t("drawing.bar.textColor", "Text color"),
                "type",
                14,
                self.cv(move |s| {
                    s.doc
                        .text
                        .as_ref()
                        .and_then(|t| t.color.clone())
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| tc.clone())
                }),
                Rc::new(Vec::new),
                "text.color",
            );
            let _ = tools.append_child(&color);
            // Under the field the trigger shows a letter (S / M / L / H), not the bar's "T".
            let _ = tools.append_child(&self.text_size_dropdown(
                self.t("drawing.bar.textSize", "Text size"),
                glyphs::size_icon,
            ));
        }
        let _ = tools.append_child(&self.toggle(
            self.t("drawing.bar.bold", "Bold"),
            "bold",
            None,
            self.cv(|s| s.doc.text.as_ref().is_some_and(|t| t.bold == Some(true))),
            self.bool_patch("text.bold"),
        ));
        let _ = tools.append_child(&self.toggle(
            self.t("drawing.bar.italic", "Italic"),
            "italic",
            None,
            self.cv(|s| s.doc.text.as_ref().is_some_and(|t| t.italic == Some(true))),
            self.bool_patch("text.italic"),
        ));
        let _ = panel.append_child(&ta.el);
        let _ = panel.append_child(&tools);
        let _ = self.el.append_child(&panel);
        *self.text_panel.borrow_mut() = Some(panel);
        self.reposition();
        vela_focus(&ta.el);
        let _ = first;
    }
}

/// Whether `el` has focus (a field being typed in is not overwritten by a refresh).
pub(super) fn active_is(el: &Element) -> bool {
    let Some(doc) = el.owner_document() else {
        return false;
    };
    let active =
        js_sys::Reflect::get(&doc, &JsValue::from_str("activeElement")).unwrap_or(JsValue::NULL);
    JsValue::from(el.clone()) == active
}
