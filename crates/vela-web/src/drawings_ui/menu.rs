//! The magnifier chip's timeframe menu: a small list of the timeframes below the chart's own
//! (`magnifier_timeframes_below`), the drawing's current one checked, anchored under the
//! chip. A pick is reported with `magnifier_timeframe_picked(id, value)`; an outside press or
//! Escape closes it. Labels read through the chart's translator. With no lower timeframe the
//! menu holds a note saying so.

use vela_core::drawings::types::measure::magnifier_timeframes_below;
use vela_core::render::drawings::MagnifierMenuRequest;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::DrawingsUi;
use super::style::sized_icon;
use crate::dom::{
    Listener, attr, get_prop, get_str, make, on, prevent_default, set_styles, stop_propagation,
    vela_contains, vela_focus,
};
use crate::icons::icon;

pub struct Menu {
    el: Element,
    listeners: Vec<Listener>,
    outside: Option<Listener>,
}

impl Menu {
    pub fn dispose(self) {
        self.el.remove();
        drop(self.listeners);
        drop(self.outside);
    }
}

impl DrawingsUi {
    fn menu_close(&self) {
        let menu = self.menu.borrow_mut().take();
        let Some(menu) = menu else { return };
        self.bump_generation();
        let Menu {
            el,
            mut listeners,
            outside,
        } = menu;
        listeners.extend(outside);
        self.retire(listeners);
        el.remove();
    }

    pub(super) fn menu_open(&self, request: &MagnifierMenuRequest) {
        self.menu_close();
        let bar_ms = self.renderer().map_or(0.0, |r| r.chart_bar_ms());
        let options = magnifier_timeframes_below(bar_ms);
        let tr = self.tr();
        let el = make(&self.doc, "div", "vela-dmenu");
        attr(&el, "role", "menu");
        attr(&el, "aria-label", "Magnifier timeframe");
        let mut listeners: Vec<Listener> = Vec::new();
        let mut items: Vec<Element> = Vec::new();
        if options.is_empty() {
            // The chart is already at the finest offered step: say so instead of showing an
            // empty list.
            let note = make(&self.doc, "div", "vela-dmenu-note");
            note.set_text_content(Some(&tr.t(
                "drawing.bar.noLowerTimeframeAvailable",
                "No lower timeframe available",
            )));
            let _ = el.append_child(&note);
        }
        for option in &options {
            let label = tr.m(&option.label);
            let selected = option.value == request.current;
            let item = make(&self.doc, "button", "vela-dtb-item");
            attr(&item, "type", "button");
            attr(&item, "role", "menuitemradio");
            attr(
                &item,
                "aria-checked",
                if selected { "true" } else { "false" },
            );
            attr(&item, "aria-label", &label);
            let text = make(&self.doc, "span", "vela-dtb-label");
            text.set_text_content(Some(&label));
            let _ = item.append_child(&text);
            let check = make(&self.doc, "span", "vela-dtb-check");
            if selected {
                check.set_inner_html(&sized_icon(icon("check")));
            }
            let _ = item.append_child(&check);
            let ui = self.weak();
            let id = request.id.clone();
            let value = option.value.to_string();
            listeners.push(on(&item, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                if let Some(ui) = ui.upgrade() {
                    ui.menu_close();
                }
                DrawingsUi::with_ctl(&ui, |c| c.magnifier_timeframe_picked(&id, &value));
            }));
            let _ = el.append_child(&item);
            items.push(item);
        }
        {
            let ui = self.weak();
            listeners.push(on(&el, "keydown", move |ev: JsValue| {
                if get_str(&ev, "key") == "Escape" {
                    prevent_default(&ev);
                    if let Some(ui) = ui.upgrade() {
                        ui.menu_close();
                    }
                }
            }));
        }
        // Under the chip, in wrapper coordinates (the chip's rect is plot-relative).
        let (off_x, off_y) = self.plot_offset();
        set_styles(
            &el,
            &[
                ("left", &format!("{}px", off_x + request.rect.x)),
                (
                    "top",
                    &format!("{}px", off_y + request.rect.y + request.rect.h + 6.0),
                ),
            ],
        );
        let _ = self.wrapper.append_child(&el);
        if let Some(first) = items.first() {
            vela_focus(first);
        }
        *self.menu.borrow_mut() = Some(Menu {
            el,
            listeners,
            outside: None,
        });
        let generation = self.bump_generation();
        self.after_tick(generation, |ui| {
            let weak = ui.weak();
            let listener = Listener::new(
                ui.doc.as_ref(),
                "pointerdown",
                false,
                true,
                move |ev: JsValue| {
                    let Some(ui) = weak.upgrade() else { return };
                    let el = ui.menu.borrow().as_ref().map(|m| m.el.clone());
                    let target = get_prop(&ev, "target");
                    if el.is_some_and(|el| vela_contains(&el, &target)) {
                        return;
                    }
                    ui.menu_close();
                },
            );
            if let Some(menu) = ui.menu.borrow_mut().as_mut() {
                menu.outside = Some(listener);
            }
        });
    }
}
