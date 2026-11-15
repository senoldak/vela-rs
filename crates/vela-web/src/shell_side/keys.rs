//! The default key bindings of the widget (`registerDefaultKeys` of the original's
//! `VelaWorkspace.ts`), registered on the root's keymap, every binding acting on the ACTIVE
//! cell; the type-to-act router for bare keystrokes (`routeTyping`, on the pure
//! [`resolve_typing`]); the keyboard zoom glide (`glide.ts`) driven by
//! `requestAnimationFrame`; the shortcut hints pushed to the drawing toolbar's flyouts.
//!
//! Not here: the screenshot chord (`mod+alt+s`) belongs to the screenshot slot of the topbar,
//! which registers it with the slot.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use js_sys::Function;
use serde_json::json;
use vela_core::drawings::DrawingPoint;
use vela_core::drawings::controller::AddInit;
use vela_core::i18n::{MessageLike, msg};
use vela_core::ui::keymap::{KeyBindingDescriptor, KeyEvent, KeyEventTarget, is_editable_target};
use vela_core::vela::Vela;
use vela_core::widget::glide::{GlideSchedule, Glider, PAN_FAST, ZOOM_IN, ZOOM_OUT};
use vela_core::widget::tool_shortcuts::tool_shortcut_hints;
use vela_core::workspace::{TypingState, TypingTarget, Workspace, resolve_typing};
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use crate::dom::{Listener, get_bool, get_prop, get_str, prevent_default, vela_caf, vela_raf};

use super::kit::{Cx, Disposers};
use super::{Intent, Intents};

/// The keyboard zoom glide on .
pub struct GlideLoop {
    glider: RefCell<Glider>,
    frame: RefCell<Option<JsValue>>,
    cb: Closure<dyn FnMut()>,
    ws: vela_core::workspace::WeakWorkspace,
}

impl GlideLoop {
    pub fn new(ws: &Workspace) -> Rc<GlideLoop> {
        Rc::new_cyclic(|me: &std::rc::Weak<GlideLoop>| {
            let weak = me.clone();
            GlideLoop {
                glider: RefCell::new(Glider::new()),
                frame: RefCell::new(None),
                cb: Closure::<dyn FnMut()>::new(move || {
                    if let Some(g) = weak.upgrade() {
                        g.on_frame();
                    }
                }),
                ws: ws.downgrade(),
            }
        })
    }

    fn chart(&self) -> Option<Vela> {
        self.ws.upgrade()?.chart()
    }

    pub fn zoom(&self, factor: f64) {
        let chart = self.chart();
        let schedule = self.glider.borrow_mut().zoom(chart.as_ref(), factor);
        if schedule == GlideSchedule::NextFrame {
            self.request();
        }
    }

    fn request(&self) {
        let id = vela_raf(self.cb.as_ref().unchecked_ref::<Function>());
        *self.frame.borrow_mut() = Some(id);
    }

    fn on_frame(&self) {
        *self.frame.borrow_mut() = None;
        let chart = self.chart();
        let more = self.glider.borrow_mut().frame(chart.as_ref());
        if more {
            self.request();
        }
    }

    /// Stop gliding (a mid-glide cell switch must not steer the next cell's viewport).
    pub fn stop(&self) {
        self.glider.borrow_mut().stop();
        if let Some(id) = self.frame.borrow_mut().take() {
            vela_caf(&id);
        }
    }
}

fn text(id: &'static str, en: &'static str) -> MessageLike {
    msg(id, en)
}

/// Register the default set. `active_chart` is read at every key press.
pub fn register_defaults(
    cx: &Cx,
    ws: &Workspace,
    intents: &Rc<Intents>,
    glide: &Rc<GlideLoop>,
    help: Rc<dyn Fn()>,
    disposers: &Disposers,
) {
    let keymap = cx.root.keymap().clone();
    let cat_chart = text("workspace.keys.category.chart", "Chart");
    let cat_draw = text("workspace.keys.category.drawings", "Drawings");
    let cat_edit = text("workspace.keys.category.edit", "Edit");
    let cat_ind = text("workspace.keys.category.indicators", "Indicators");
    let cat_help = text("workspace.keys.category.help", "Help");
    let wk = ws.downgrade();
    let reg = |id: &str,
               keys: &[&str],
               label: MessageLike,
               category: &MessageLike,
               run: Rc<dyn Fn(&Workspace)>| {
        let wk = wk.clone();
        let mut d = KeyBindingDescriptor::new(id, keys[0], label, move |_| {
            if let Some(ws) = wk.upgrade() {
                run(&ws);
            }
        });
        d.keys = keys.iter().map(|k| k.to_string()).collect();
        d.category = Some(category.clone());
        let off = keymap.register(d);
        disposers.push(off);
    };
    reg(
        "chart.reset-view",
        &["alt+r"],
        text("workspace.keys.resetView", "Reset view (all history)"),
        &cat_chart,
        Rc::new(|ws| {
            if let Some(c) = ws.active() {
                c.reset_view();
            }
        }),
    );
    reg(
        "chart.toggle-log",
        &["alt+l"],
        text("workspace.keys.toggleLog", "Toggle logarithmic scale"),
        &cat_chart,
        Rc::new(|ws| {
            if let Some(chart) = ws.chart() {
                let on = chart.renderer().get("logScale").is_some_and(truthy);
                chart.renderer().set("logScale", json!(!on));
            }
        }),
    );
    reg(
        "chart.toggle-percent",
        &["alt+p"],
        text("workspace.keys.togglePercent", "Toggle percent scale"),
        &cat_chart,
        Rc::new(|ws| {
            if let Some(chart) = ws.chart() {
                let mode = chart.renderer().get("scaleMode");
                let percent = mode.as_ref().and_then(|v| v.as_str()) == Some("percent");
                chart.renderer().set(
                    "scaleMode",
                    json!(if percent { "price" } else { "percent" }),
                );
            }
        }),
    );
    reg(
        "drawings.trendline",
        &["alt+t"],
        text("workspace.keys.trendline", "Arm the trend line tool"),
        &cat_draw,
        Rc::new(|ws| {
            if let Some(chart) = ws.chart() {
                chart.drawings().set_tool(Some("trendline"));
            }
        }),
    );
    for (id, key, kind, label) in [
        (
            "drawings.hline-cursor",
            "alt+h",
            "hline",
            text(
                "workspace.keys.hlineCursor",
                "Horizontal line at the cursor price",
            ),
        ),
        (
            "drawings.vline-cursor",
            "alt+v",
            "vline",
            text(
                "workspace.keys.vlineCursor",
                "Vertical line at the cursor time",
            ),
        ),
    ] {
        reg(
            id,
            &[key],
            label,
            &cat_draw,
            Rc::new(move |ws| {
                let Some(cell) = ws.active() else { return };
                let (Some(time), Some(price)) = cell.last_crosshair() else {
                    return;
                };
                if let Some(chart) = cell.try_chart() {
                    chart.drawings().add(
                        kind,
                        AddInit {
                            anchors: Some(vec![DrawingPoint { time, price }]),
                            ..AddInit::default()
                        },
                    );
                }
            }),
        );
    }
    reg(
        "history.undo",
        &["mod+z"],
        text("workspace.keys.undo", "Undo (active chart)"),
        &cat_edit,
        Rc::new(|ws| {
            if let Some(c) = ws.active() {
                c.history().undo();
            }
        }),
    );
    reg(
        "history.redo",
        &["mod+y", "mod+shift+z"],
        text("workspace.keys.redo", "Redo (active chart)"),
        &cat_edit,
        Rc::new(|ws| {
            if let Some(c) = ws.active() {
                c.history().redo();
            }
        }),
    );
    let g = glide.clone();
    reg(
        "view.zoom-in",
        &["mod+arrowup"],
        text("workspace.keys.zoomIn", "Zoom in"),
        &cat_chart,
        Rc::new(move |_| g.zoom(ZOOM_IN)),
    );
    let g = glide.clone();
    reg(
        "view.zoom-out",
        &["mod+arrowdown"],
        text("workspace.keys.zoomOut", "Zoom out"),
        &cat_chart,
        Rc::new(move |_| g.zoom(ZOOM_OUT)),
    );
    reg(
        "view.pan-left",
        &["mod+arrowleft"],
        text("workspace.keys.panLeft", "Pan toward history"),
        &cat_chart,
        Rc::new(|ws| {
            if let Some(chart) = ws.chart() {
                chart.pan_by(-PAN_FAST);
            }
        }),
    );
    reg(
        "view.pan-right",
        &["mod+arrowright"],
        text("workspace.keys.panRight", "Pan toward now"),
        &cat_chart,
        Rc::new(|ws| {
            if let Some(chart) = ws.chart() {
                chart.pan_by(PAN_FAST);
            }
        }),
    );
    // `/` belongs to the indicators slot: it opens the picker (S1's), or whatever routes it.
    let it = intents.clone();
    reg(
        "indicators.open",
        &["/"],
        text("workspace.keys.openIndicators", "Open the indicator picker"),
        &cat_ind,
        Rc::new(move |_| it.emit(Intent::IndicatorPicker)),
    );
    reg(
        "help.shortcuts",
        &["?"],
        text("workspace.keys.showShortcuts", "Show this shortcuts panel"),
        &cat_help,
        Rc::new(move |_| help()),
    );
}

fn truthy(v: serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Shortcut hints beside the bound tools in the drawing toolbar's flyouts.
pub fn push_tool_hints(cx: &Cx, ws: &Workspace) {
    let tr = cx.tr();
    let hints = tool_shortcut_hints(cx.root.keymap(), tr.as_deref());
    let map: BTreeMap<String, String> = hints.into_iter().collect();
    for cell in ws.cells() {
        if let Some(chart) = cell.try_chart() {
            chart.drawings().set_tool_shortcuts(&map);
        }
    }
}

/// The bare-typing router: letters and `0` seed the symbol search, digits 1-9 the timeframe
/// entry. Listens on the root after the keymap's own listener.
pub fn route_typing(cx: &Cx, intents: &Rc<Intents>) -> Listener {
    let intents = intents.clone();
    let root: Element = cx.root.element().clone();
    Listener::new(
        root.as_ref(),
        "keydown",
        false,
        false,
        move |ev: JsValue| {
            let ctrl = get_bool(&ev, "ctrlKey");
            let meta = get_bool(&ev, "metaKey");
            let alt = get_bool(&ev, "altKey");
            if ctrl || meta || alt {
                return;
            }
            let key = get_str(&ev, "key");
            // An input method composing a character owns its keystrokes.
            if get_bool(&ev, "isComposing") || key == "Process" {
                return;
            }
            let target = get_prop(&ev, "target");
            let event = KeyEvent {
                key: key.clone(),
                target: (!target.is_undefined() && !target.is_null()).then(|| KeyEventTarget {
                    tag_name: get_str(&target, "tagName"),
                    is_content_editable: get_bool(&target, "isContentEditable"),
                    role: target
                        .dyn_ref::<Element>()
                        .and_then(|e| e.get_attribute("role")),
                }),
                ..KeyEvent::default()
            };
            // Never hijack a keystroke someone is TYPING.
            if is_editable_target(&event) {
                return;
            }
            let state = TypingState {
                dialogs: intents.dialogs(),
                symbol_search: intents.symbol_open(),
                timeframe_entry: intents.timeframe_open(),
            };
            let Some((to, text)) = resolve_typing(&key, state) else {
                return;
            };
            prevent_default(&ev);
            match to {
                TypingTarget::Symbol => intents.emit(Intent::TypeSymbol(text)),
                TypingTarget::Timeframe => intents.emit(Intent::TypeTimeframe(text)),
            }
        },
    )
}
