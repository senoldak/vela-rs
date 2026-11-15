//! The data window (`data-window.ts`): a docked side panel showing the bar under the
//! crosshair, its date and time, its OHLCV, then every indicator's value there. The readout
//! arrives pre-formatted from the renderer (`renderer.data_window_readout`); the layout is
//! `vela_core::widget::data_window`. Hover updates ride the crosshair seam, resting updates
//! the chart events of `REFRESH_EVENTS`. It refreshes only while the panel is visible and a
//! chart is bound, and is rebound to the active cell's chart.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::vela::Vela;
use vela_core::widget::data_window::{
    REFRESH_EVENTS, data_window_sections, data_window_title, no_data_message, no_readout_message,
};
use vela_core::widget::side_panel::SidePanelOptions;

use crate::dom::attr;

use super::bind::{ChartSubs, chart_on};
use super::dock::{Dock, PanelSpec, PanelView};
use super::kit::{Cx, clear, is_hidden};

pub struct DataWindow {
    cx: Cx,
    me: Weak<DataWindow>,
    pub view: Rc<PanelView>,
    chart: RefCell<Option<Vela>>,
    subs: ChartSubs,
}

impl DataWindow {
    pub fn new(cx: &Cx, dock: &Rc<Dock>) -> Rc<DataWindow> {
        let view = dock.add_built_in(PanelSpec {
            id: "dataWindow",
            title: data_window_title(),
            icon: "datawindow",
            order: 10.0,
            modifier: "vela-dw",
            opts: SidePanelOptions::default(),
        });
        let dw = Rc::new_cyclic(|me| DataWindow {
            cx: cx.clone(),
            me: me.clone(),
            view,
            chart: RefCell::new(None),
            subs: ChartSubs::default(),
        });
        let w = Rc::downgrade(&dw);
        dock.on_open_hook(
            "dataWindow",
            Rc::new(move || {
                if let Some(d) = w.upgrade() {
                    d.refresh();
                }
            }),
        );
        let w = Rc::downgrade(&dw);
        dock.on_chart_hook(
            "dataWindow",
            Rc::new(move |_cell: &str, chart: &Vela| {
                if let Some(d) = w.upgrade() {
                    d.on_chart(chart);
                }
            }),
        );
        dw
    }

    /// (Re)bind to a chart instance.
    pub fn on_chart(&self, chart: &Vela) {
        self.subs.clear();
        *self.chart.borrow_mut() = Some(chart.clone());
        for ev in REFRESH_EVENTS {
            let me = self.me.clone();
            self.subs.push(chart_on(chart, ev, move |_| {
                if let Some(d) = me.upgrade() {
                    d.refresh();
                }
            }));
        }
        let me = self.me.clone();
        let off = chart.renderer().on_crosshair_move(Box::new(move |_| {
            if let Some(d) = me.upgrade() {
                d.refresh();
            }
        }));
        self.subs.push(off);
        self.refresh();
    }

    pub fn detach(&self) {
        self.subs.clear();
        *self.chart.borrow_mut() = None;
    }

    pub fn relocalize(&self) {
        self.refresh();
    }

    pub fn refresh(&self) {
        if is_hidden(&self.view.el) {
            return;
        }
        let Some(chart) = self.chart.borrow().clone() else {
            return;
        };
        let readout = chart.renderer().data_window_readout();
        let body = &self.view.body;
        clear(body);
        let tr = self.cx.tr();
        let sections = readout
            .as_ref()
            .map(|r| data_window_sections(r, tr.as_deref()))
            .unwrap_or_default();
        if sections.is_empty() {
            let msg = if readout.is_some() {
                no_data_message()
            } else {
                no_readout_message()
            };
            let _ = body.append_child(&self.cx.text("div", "vela-dw-empty", &self.cx.t(&msg)));
            return;
        }
        for section in sections {
            let _ = body.append_child(&self.cx.text("div", "vela-dw-group", &section.title));
            for line in section.lines {
                let row = self.cx.el("div", "vela-dw-row");
                let label = self.cx.text("span", "vela-dw-label", &line.label);
                let value = self.cx.text("span", "vela-dw-value", &line.value);
                attr(&value, "dir", "ltr");
                if !line.color.is_empty() {
                    crate::dom::set_style(&value, "color", &line.color);
                }
                let _ = row.append_child(&label);
                let _ = row.append_child(&value);
                let _ = body.append_child(&row);
            }
        }
    }

    pub fn destroy(&self) {
        self.detach();
    }
}
