//! The full settings panel of one drawing: a port of `DrawingSettingsPanel.ts`.
//!
//! Every option the tool has, laid out the same way for every tool. The schema-driven core
//! (style, text, the tool's own options) is followed by folds for the label, the anchor
//! points and the timeframes the drawing shows on; tools with richer data (levels, position
//! sizing, a volume profile) add their own section. Options that only matter in some state
//! appear when they apply. Edits are live; Cancel puts the drawing back as it was when the
//! panel opened (Escape cancels too); OK, the close button and a close the bar asks for
//! keep the edits. A style menu in the header applies, previews and saves named looks for the
//! tool.
//!
//! The panel is non-modal: the chart stays interactive behind it, so the drawing's points can
//! still be dragged while it is open. It opens beside the quick bar, like a menu from its
//! gear, on the bar's side away from the drawing when it fits there.
//!
//! Differences from the original: there is no ready-made color picker (the dialogs' native
//! swatch and opacity slider stand in), times print through the English formatters, and
//! the frame is built here (the kit's dialog is not ported).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use serde_json::{Value, json};
use vela_core::drawings::fib_presets::{fib_presets, match_fib_preset};
use vela_core::drawings::types::measure::magnifier_timeframes_below;
use vela_core::drawings::{
    DEFAULT_DRAWING_COLOR, DrawingRegistry, DrawingToolTemplate, FieldKind, FreeAxis,
    SerializedDrawing, SettingsField, builtin_tool_templates, line_style_options,
    text_size_options, timeframe_band_of, timeframe_bands,
};
use vela_core::format::{FormatSurface, english_format};
use vela_core::i18n::{MessageArg, msg};
use vela_core::render::chrome::ticks::format_time_stamp;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use super::DrawingsUi;
use super::float::FloatHost;
use super::glyphs::{self, dash_icon, extend_icon, width_icon};
use super::kit::{Build, SegOpt};
use super::model::{
    self, HIGHER, INTRADAY, LEVEL_PATHS, LINE_LABEL_TYPES, MAX_POINTS, NO_POINTS, POSITION_PATHS,
    ShowMode, Snap, extend_value, fmt_num, gate_holds, matches_template, mode_of, pair_base,
    plain_text, shown_price, size_key, template_color, upper_lower_base,
};
use super::sections;
use super::session::Core;
use super::util;
use crate::dialogs::Cx;
use crate::dialogs::controls::{self, SelOpt};
use crate::dom::{
    Listener, attr, default_view, get_f64, get_prop, get_str, prevent_default, set_style,
    set_styles, stop_propagation, vela_contains, vela_focus,
};

type Rect = (f64, f64, f64, f64);

const FALLBACK_SIZE: (f64, f64) = (360.0, 420.0);

pub struct Panel {
    me: Weak<Panel>,
    ui: Weak<DrawingsUi>,
    pub core: Rc<Core>,
    /// The frame's own listeners (header, footer, keyboard); sections have their own bag.
    cx: Cx,
    positioner: Element,
    panel: Element,
    scroll: Element,
    build_cx: RefCell<Cx>,
    syncs: Rc<RefCell<Vec<Rc<dyn Fn()>>>>,
    /// The listener bags of the build's own sub-contexts (the level grid), freed with it.
    bags: Rc<RefCell<Vec<Rc<RefCell<Vec<Listener>>>>>>,
    floats: Rc<FloatHost>,
    snapshot: SerializedDrawing,
    drag: RefCell<Vec<Listener>>,
    style_label: Element,
    style_btn: Element,
}

impl Panel {
    /// Open the panel for drawing `id`. `anchor` is the drawing's box and `toolbar` the quick
    /// bar's, both in wrapper px.
    pub fn open(
        ui: &Rc<DrawingsUi>,
        id: &str,
        anchor: Option<Rect>,
        toolbar: Option<Rect>,
    ) -> Option<Rc<Panel>> {
        let core = Core::new(ui, vec![id.to_string()], false);
        let first = core.first()?;
        super::style::ensure_styles(&ui.doc);
        crate::dialogs::css::ensure_styles(&ui.doc);
        let cx = Cx::new(&ui.doc, ui.tr());
        let tr = ui.tr();
        let label = DrawingRegistry::builtin()
            .get(first.type_key())
            .map(|m| tr.m(&m.label))
            .unwrap_or_else(|| tr.t("drawing.settings.settings", "Settings"));

        let positioner = cx.el("div", "vela-dsp-pos");
        let panel = cx.el("div", "vela-dlg-panel vela-dsp vela-dsp-scope");
        let title_id = cx.uid("vela-dsp-title");
        attr(&panel, "role", "dialog");
        attr(&panel, "aria-modal", "false");
        attr(&panel, "aria-labelledby", &title_id);
        attr(&panel, "tabindex", "-1");
        let header = cx.el("div", "vela-dlg-header");
        let title = cx.text("h2", "vela-dlg-title", &label);
        title.set_id(&title_id);
        let style_btn = cx.el("button", "vela-dsp-style");
        attr(&style_btn, "type", "button");
        attr(&style_btn, "aria-haspopup", "menu");
        attr(&style_btn, "aria-expanded", "false");
        let style_label = cx.el("span", "");
        let chev = cx.el("span", "vela-dsp-style-c");
        chev.set_inner_html(glyphs::CHEVRON_DOWN);
        let _ = style_btn.append_child(&style_label);
        let _ = style_btn.append_child(&chev);
        let closer = cx.el("button", "vela-dlg-close");
        attr(&closer, "type", "button");
        let close_label = tr.t("chart.indicatorSettings.close", "Close");
        attr(&closer, "aria-label", &close_label);
        attr(&closer, "title", &close_label);
        closer.set_inner_html(&glyphs::sized(
            &glyphs::svg24("<path d=\"m6 6 12 12M18 6 6 18\"/>", ""),
            16,
        ));
        let _ = header.append_child(&title);
        let _ = header.append_child(&style_btn);
        let _ = header.append_child(&closer);
        let body = cx.el("div", "vela-dlg-body");
        let scroll = cx.el("div", "vela-dsp-scroll");
        let _ = body.append_child(&scroll);
        let footer = cx.el("div", "vela-dlg-footer");
        let _ = panel.append_child(&header);
        let _ = panel.append_child(&body);
        let _ = panel.append_child(&footer);
        let _ = positioner.append_child(&panel);

        let floats = FloatHost::new(ui, &cx);
        let build_cx = cx.sub();
        let me_panel = Rc::new_cyclic(|me| Panel {
            me: me.clone(),
            ui: Rc::downgrade(ui),
            core,
            cx: cx.clone(),
            positioner,
            panel,
            scroll,
            build_cx: RefCell::new(build_cx),
            syncs: Rc::new(RefCell::new(Vec::new())),
            bags: Rc::new(RefCell::new(Vec::new())),
            floats,
            snapshot: first.doc.clone(),
            drag: RefCell::new(Vec::new()),
            style_label,
            style_btn,
        });
        me_panel.wire_frame(&header, &closer, &footer);
        me_panel.rebuild();
        me_panel.refresh_style_label();
        // Without the toolbar to hang off: the side of the chart away from the drawing.
        let (host_w, _) = me_panel.host_size();
        let left_half = anchor.is_some_and(|a| host_w > 0.0 && a.0 + a.2 / 2.0 < host_w / 2.0);
        set_styles(
            &me_panel.positioner,
            &[
                (
                    "justify-content",
                    if left_half { "flex-end" } else { "flex-start" },
                ),
                ("padding", "0 12px"),
            ],
        );
        let _ = ui.wrapper.append_child(&me_panel.positioner);
        if let Some(bar) = toolbar {
            me_panel.place_beside(bar, anchor);
        }
        vela_focus(&me_panel.panel);
        Some(me_panel)
    }

    fn host_size(&self) -> (f64, f64) {
        match self.ui.upgrade() {
            Some(ui) => ui.wrapper_size(),
            None => (0.0, 0.0),
        }
    }

    fn t(&self, id: &'static str, en: &'static str) -> String {
        self.cx.tr.t(id, en)
    }

    /// Whether `node` is inside the panel or its style menu.
    pub fn contains(&self, node: &JsValue) -> bool {
        vela_contains(&self.positioner, node) || self.floats.contains(node)
    }

    /// Cancel: put the drawing back as it was when the panel opened.
    fn cancel(&self) {
        self.core.preview(None);
        self.core.restore(&self.snapshot);
        self.finish();
    }

    /// Close, keeping the edits.
    fn finish(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.panel_close();
        }
    }

    /// Tear the panel down and hand its listeners back.
    pub fn close(&self) -> Vec<Listener> {
        let mut out = self.floats.close_now();
        out.extend(std::mem::take(&mut *self.cx.bag.borrow_mut()));
        out.extend(std::mem::take(
            &mut *self.build_cx.borrow().bag.borrow_mut(),
        ));
        out.extend(self.drag.borrow_mut().drain(..));
        for bag in self.bags.borrow_mut().drain(..) {
            out.extend(std::mem::take(&mut *bag.borrow_mut()));
        }
        self.syncs.borrow_mut().clear();
        self.positioner.remove();
        out
    }

    /// Re-read the drawing after a change made outside the panel (a drag, an undo).
    pub fn sync(&self) {
        if !self.core.refresh() {
            self.finish(); // the drawing went away (deleted, undone)
            return;
        }
        let syncs: Vec<_> = self.syncs.borrow().clone();
        for f in syncs {
            f();
        }
        self.refresh_style_label();
    }

    fn wire_frame(&self, header: &Element, closer: &Element, footer: &Element) {
        {
            let me = self.me.clone();
            self.cx.on(closer, "click", move |_| {
                if let Some(p) = me.upgrade() {
                    p.finish();
                }
            });
        }
        {
            let me = self.me.clone();
            let cancel = controls::button(
                &self.cx,
                &self.t("drawing.settings.cancel", "Cancel"),
                false,
                false,
                move || {
                    if let Some(p) = me.upgrade() {
                        p.cancel();
                    }
                },
            );
            let me = self.me.clone();
            let ok = controls::button(
                &self.cx,
                &self.t("drawing.settings.ok", "OK"),
                true,
                false,
                move || {
                    if let Some(p) = me.upgrade() {
                        p.finish();
                    }
                },
            );
            let _ = footer.append_child(&cancel);
            let _ = footer.append_child(&ok);
        }
        {
            let me = self.me.clone();
            self.cx.on(&self.panel, "keydown", move |ev| {
                if get_str(&ev, "key") != "Escape" {
                    return;
                }
                prevent_default(&ev);
                let Some(p) = me.upgrade() else { return };
                if p.floats.is_open() {
                    p.floats.close();
                } else {
                    p.cancel();
                }
            });
        }
        // The header drags the panel.
        {
            let me = self.me.clone();
            self.cx.on(header, "pointerdown", move |ev| {
                let Some(p) = me.upgrade() else { return };
                if get_f64(&ev, "button").unwrap_or(0.0) != 0.0 {
                    return;
                }
                let tag = util::tag_of(&get_prop(&ev, "target"));
                if tag == "button" || tag == "svg" || tag == "path" || tag == "span" {
                    return;
                }
                p.start_drag(&ev);
            });
        }
        {
            let me = self.me.clone();
            self.cx.on(&self.style_btn, "pointerdown", move |ev| {
                stop_propagation(&ev);
                let _ = &me;
            });
        }
        {
            let me = self.me.clone();
            self.cx.on(&self.style_btn, "click", move |ev| {
                stop_propagation(&ev);
                let Some(p) = me.upgrade() else { return };
                if p.floats.is_open() {
                    p.floats.close();
                } else {
                    p.open_style_menu();
                }
            });
        }
    }

    fn start_drag(&self, ev: &JsValue) {
        let (ox, oy) = self.dragged();
        let start = (
            get_f64(ev, "clientX").unwrap_or(0.0),
            get_f64(ev, "clientY").unwrap_or(0.0),
        );
        let Some(view) = default_view(&self.cx.doc) else {
            return;
        };
        let mut listeners = Vec::new();
        let live = Rc::new(Cell::new(true));
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
                    let dx = ox + get_f64(&ev, "clientX").unwrap_or(0.0) - start.0;
                    let dy = oy + get_f64(&ev, "clientY").unwrap_or(0.0) - start.1;
                    set_style(&p.panel, "transform", &format!("translate({dx}px, {dy}px)"));
                },
            ));
        }
        for kind in ["pointerup", "pointercancel"] {
            let me = self.me.clone();
            let live = live.clone();
            listeners.push(Listener::new(&view, kind, false, false, move |_| {
                live.set(false);
                let Some(p) = me.upgrade() else { return };
                let old: Vec<Listener> = p.drag.borrow_mut().drain(..).collect();
                if let Some(ui) = p.ui.upgrade() {
                    ui.retire(old);
                }
            }));
        }
        *self.drag.borrow_mut() = listeners;
    }

    /// The translation the header drag has applied so far.
    fn dragged(&self) -> (f64, f64) {
        let style = self.panel.unchecked_ref::<web_sys::HtmlElement>().style();
        let t = style.get_property_value("transform").unwrap_or_default();
        let inner = t.trim_start_matches("translate(").trim_end_matches(')');
        let mut nums = inner.split(',').map(|p| {
            p.trim()
                .trim_end_matches("px")
                .trim()
                .parse::<f64>()
                .unwrap_or(0.0)
        });
        (nums.next().unwrap_or(0.0), nums.next().unwrap_or(0.0))
    }

    /// Hang the panel off the quick bar, like a menu from its gear: on the bar's side away
    /// from the drawing when it fits there, else the side with more room (it scrolls inside),
    /// its left edge in line with the bar's. On the mobile layout the panel fills the shell
    /// instead.
    fn place_beside(&self, bar: Rect, drawing: Option<Rect>) {
        let ui = self.ui.upgrade();
        let mobile = ui
            .as_ref()
            .and_then(|u| u.wrapper.parent_element())
            .is_some_and(|c| c.get_attribute("data-vela-layout").as_deref() == Some("mobile"));
        if mobile {
            return;
        }
        let (w_host, h_host) = self.host_size();
        if w_host == 0.0 || h_host == 0.0 {
            return;
        }
        set_style(&self.positioner, "padding", "0");
        set_styles(&self.panel, &[("position", "absolute"), ("margin", "0")]);
        let r = self.panel.get_bounding_client_rect();
        let size = if r.width() > 0.0 && r.height() > 0.0 {
            (r.width(), r.height())
        } else {
            FALLBACK_SIZE
        };
        let at = model::beside(bar, drawing, (w_host, h_host), size);
        let px = |v: f64| format!("{}px", v.round());
        set_style(&self.panel, "left", &px(at.left));
        if let Some(top) = at.top {
            set_style(&self.panel, "top", &px(top));
        }
        if let Some(bottom) = at.bottom {
            set_style(&self.panel, "bottom", &px(bottom));
        }
        if let Some(max) = at.max_height {
            set_style(&self.panel, "max-height", &px(max));
        }
    }

    // ── sections ──

    /// Rebuild every section (a control set changed shape).
    pub fn rebuild(&self) {
        let top = util::scroll_top(&self.scroll);
        // The old controls' listeners are freed from a timer: this may run from one of them.
        let old = {
            let mut cx = self.build_cx.borrow_mut();
            let mut old: Vec<Listener> = std::mem::take(&mut *cx.bag.borrow_mut());
            for bag in self.bags.borrow_mut().drain(..) {
                old.extend(std::mem::take(&mut *bag.borrow_mut()));
            }
            *cx = self.cx.sub();
            old
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.retire(old);
        }
        self.syncs.borrow_mut().clear();
        util::clear(&self.scroll);
        let b = Build::new(
            self.core.clone(),
            self.build_cx.borrow().clone(),
            self.syncs.clone(),
            self.bags.clone(),
        );
        for el in self.sections(&b) {
            let _ = self.scroll.append_child(&el);
        }
        util::set_scroll_top(&self.scroll, top);
        self.refresh_style_label();
    }

    fn sections(&self, b: &Build) -> Vec<Element> {
        let Some(snap) = self.core.first() else {
            return Vec::new();
        };
        let mut out: Vec<Element> = Vec::new();
        if snap.schema.text_is_content && snap.has_field("text.value") {
            out.push(self.text_content_block(b, &snap));
        }
        if let Some(style) = self.style_block(b, &snap) {
            out.push(style);
        }
        if snap.levels.is_some() {
            out.push(self.levels_block(b, &snap));
        }
        if sections::is_position(&snap) {
            let sub = sections::position(b);
            for s in &sub {
                s.set_class_name("vela-dsp-sub");
            }
            out.push(b.block(Some(&b.t("drawing.settings.position", "Position")), sub));
        }
        if sections::is_profile(&snap) {
            out.extend(sections::profile(b));
        }
        if let Some(own) = self.options_block(b, &snap) {
            out.push(own);
        }
        if !snap.schema.text_is_content
            && snap
                .schema
                .fields
                .iter()
                .any(|f| f.path.starts_with("text."))
        {
            out.push(self.text_fold(b, &snap));
        }
        if let Some(points) = self.points_fold(b, &snap) {
            out.push(points);
        }
        out.push(self.show_on_fold(b));
        out
    }

    /// A color read through a snapshot path, with a fallback.
    fn color_reader(
        b: &Build,
        read: impl Fn(&Snap) -> Option<String> + 'static,
    ) -> Rc<dyn Fn() -> String> {
        let b = b.clone();
        Rc::new(move || {
            b.live(|s| read(s))
                .flatten()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| DEFAULT_DRAWING_COLOR.to_string())
        })
    }

    /// Line, width, dash, ends and fill: the look every stroked tool shares.
    fn style_block(&self, b: &Build, snap: &Snap) -> Option<Element> {
        let has = |p: &str| snap.has_field(p);
        let mut rows: Vec<Element> = Vec::new();
        if has("style.lineColor") || has("style.lineWidth") {
            let mut ctrls: Vec<Element> = Vec::new();
            if has("style.lineColor") {
                ctrls.push(b.swatch(
                    &b.t("drawing.settings.lineColor", "Line color"),
                    Self::color_reader(b, |s| Some(s.doc.style.line_color.clone())),
                    "style.lineColor".into(),
                ));
            }
            if let Some(wf) = snap
                .schema
                .fields
                .iter()
                .find(|f| f.path == "style.lineWidth")
            {
                // A marker's broad stroke (the highlighter's 4-60) outgrows the 1-4 ladder.
                if wf.min.unwrap_or(1.0) > 1.0 {
                    ctrls.push(self.number_field(
                        b,
                        &b.t("drawing.settings.lineWidth", "Line width"),
                        "style.lineWidth",
                        wf,
                    ));
                } else {
                    let opts = [1.0, 2.0, 3.0, 4.0]
                        .iter()
                        .map(|w| SegOpt::new(fmt_num(*w), width_icon(*w)).titled(format!("{w}px")))
                        .collect();
                    let b2 = b.clone();
                    let b3 = b.clone();
                    ctrls.push(b.seg(
                        &b.t("drawing.settings.lineWidth", "Line width"),
                        opts,
                        Rc::new(move || b2.live(|s| fmt_num(s.doc.style.line_width))),
                        Rc::new(move |v| {
                            b3.edit(&[("style.lineWidth", json!(v.parse::<f64>().unwrap_or(2.0)))]);
                        }),
                        false,
                    ));
                }
            }
            rows.push(b.row(&b.t("drawing.settings.line", "Line"), &ctrls));
        }
        if has("style.lineStyle") {
            rows.push(b.row(
                &b.t("drawing.settings.style", "Style"),
                &[self.line_style_seg(
                    b,
                    &b.t("drawing.settings.lineStyle", "Line style"),
                    "style.lineStyle",
                )],
            ));
        }
        if has("style.extendLeft") && has("style.extendRight") {
            let opts = vec![
                SegOpt::new("none", extend_icon("none"))
                    .titled(b.t("drawing.settings.dontExtend", "Don't extend")),
                SegOpt::new("left", extend_icon("left"))
                    .titled(b.t("drawing.settings.extendLeft", "Extend left")),
                SegOpt::new("right", extend_icon("right"))
                    .titled(b.t("drawing.settings.extendRight", "Extend right")),
                SegOpt::new("both", extend_icon("both"))
                    .titled(b.t("drawing.settings.extendBothWays", "Extend both ways")),
            ];
            let b2 = b.clone();
            let b3 = b.clone();
            rows.push(b.row(
                &b.t("drawing.settings.extend", "Extend"),
                &[b.seg(
                    &b.t("drawing.settings.extend", "Extend"),
                    opts,
                    Rc::new(move || {
                        b2.live(|s| {
                            extend_value(
                                s.doc.style.extend_left == Some(true),
                                s.doc.style.extend_right == Some(true),
                            )
                            .to_string()
                        })
                    }),
                    Rc::new(move |v| {
                        b3.edit(&[
                            ("style.extendLeft", json!(v == "left" || v == "both")),
                            ("style.extendRight", json!(v == "right" || v == "both")),
                        ]);
                    }),
                    false,
                )],
            ));
        }
        let mut arrows: Vec<Element> = Vec::new();
        if has("style.arrowLeft") {
            arrows.push(self.flag_chip(
                b,
                &b.t("drawing.settings.startArrow", "Start arrow"),
                "style.arrowLeft",
                None,
            ));
        }
        if has("style.arrowRight") {
            let text = if has("style.arrowLeft") {
                b.t("drawing.settings.endArrow", "End arrow")
            } else {
                b.t("drawing.settings.arrow", "Arrow")
            };
            arrows.push(self.flag_chip(b, &text, "style.arrowRight", None));
        }
        if !arrows.is_empty() {
            rows.push(b.row(&b.t("drawing.settings.ends", "Ends"), &[b.chips(&arrows)]));
        }
        if has("style.fillColor") {
            rows.push(b.row(
                &b.t("drawing.settings.fill", "Fill"),
                &[b.swatch(
                    &b.t("drawing.settings.fill", "Fill"),
                    Self::color_reader(b, |s| {
                        s.fill.clone().or_else(|| s.doc.style.fill_color.clone())
                    }),
                    "style.fillColor".into(),
                )],
            ));
        }
        // A computed label (no typed words) keeps its text styling with the look.
        if !has("text.value") && has("text.color") {
            let styling = self.text_styling(b, snap);
            rows.push(b.row(&b.t("drawing.settings.text", "Text"), &styling));
        }
        (!rows.is_empty()).then(|| b.block(None, rows))
    }

    /// A segmented line-style control over `path`.
    fn line_style_seg(&self, b: &Build, label: &str, path: &'static str) -> Element {
        let opts = line_style_options()
            .iter()
            .map(|o| SegOpt::new(o.value.clone(), dash_icon(&o.value)).titled(b.m(&o.label)))
            .collect();
        let b2 = b.clone();
        let b3 = b.clone();
        b.seg(
            label,
            opts,
            Rc::new(move || b2.live(|s| s.read_str(path)).flatten()),
            Rc::new(move |v| b3.edit(&[(path, json!(v))])),
            false,
        )
    }

    /// A toggle chip over a boolean `path`.
    fn flag_chip(&self, b: &Build, text: &str, path: &'static str, title: Option<&str>) -> Element {
        let b2 = b.clone();
        let b3 = b.clone();
        b.chip(
            text,
            Rc::new(move || {
                b2.live(|s| s.read_bool(path) == Some(true))
                    .unwrap_or(false)
            }),
            Rc::new(move |on| b3.edit(&[(path, json!(on))])),
            title,
        )
    }

    fn number_field(
        &self,
        b: &Build,
        title: &str,
        path: &'static str,
        f: &SettingsField,
    ) -> Element {
        let b2 = b.clone();
        let b3 = b.clone();
        let step = f.step.unwrap_or(1.0);
        b.number(
            title,
            Rc::new(move || b2.live(|s| s.read_f64(path).unwrap_or(0.0)).unwrap_or(0.0)),
            f.min,
            f.max,
            Some(step),
            step >= 1.0 && step.fract() == 0.0,
            Rc::new(move |v| b3.edit(&[(path, json!(v))])),
        )
        .el
    }

    /// The tool's own options: every schema field the shared look and the rich sections do
    /// not cover, each as the control its kind calls for.
    fn options_block(&self, b: &Build, snap: &Snap) -> Option<Element> {
        let is_position = sections::is_position(snap);
        let has_levels = snap.levels.is_some();
        let skip = |f: &SettingsField| {
            f.path.starts_with("style.")
                || f.path.starts_with("text.")
                || f.path.starts_with("frvp.")
                || (is_position && POSITION_PATHS.contains(&f.path.as_str()))
                || (has_levels && LEVEL_PATHS.contains(&f.path.as_str()))
        };
        let own: Vec<&SettingsField> = snap.schema.fields.iter().filter(|f| !skip(f)).collect();
        let mut rows: Vec<Element> = Vec::new();
        let mut i = 0;
        while i < own.len() {
            let f = own[i];
            let next = own.get(i + 1).copied();
            // "Upper line color" + "Upper line style" read as one "Upper line" row, and an
            // upper / lower color pair as one "Bands" row.
            if let Some(base) = self.pair_label(b, f, next) {
                let color = self.field_control(b, f);
                let style = next.and_then(|n| self.field_control(b, n));
                if let (Some(color), Some(style)) = (color, style) {
                    rows.push(b.row(&base, &[color, style]));
                }
                i += 2;
                continue;
            }
            i += 1;
            let Some(ctrl) = self.field_control(b, f) else {
                continue;
            };
            let row = b.row(&b.m(&f.label), &[ctrl]);
            // An option that matters only in some modes comes and goes with them.
            if let Some(cond) = &f.when {
                let (row, b2, path, any_of) = (
                    row.clone(),
                    b.clone(),
                    cond.path.clone(),
                    cond.any_of.clone(),
                );
                b.watch(move || {
                    let held = b2
                        .live(|s| gate_holds(s.read(&path).as_ref(), &any_of))
                        .unwrap_or(true);
                    util::set_hidden(&row, !held);
                });
            }
            rows.push(row);
        }
        // Level numbers and their labels have their own sizes (fib-family tools).
        let sizes = || -> Vec<SegOpt> {
            text_size_options()
                .iter()
                .map(|o| SegOpt::new(o.value.clone(), b.m(&o.label)))
                .collect()
        };
        for (prop, label_id, label_en, title_id, title_en, default) in [
            (
                "numbersSize",
                "drawing.settings.numbers",
                "Numbers",
                ("drawing.settings.numbersSize", "Numbers size"),
                "small",
            ),
            (
                "labelsSize",
                "drawing.settings.labels",
                "Labels",
                ("drawing.settings.labelsSize", "Labels size"),
                "normal",
            ),
        ]
        .map(|(p, i, e, t, d)| (p, i, e, t.0, t.1, d))
        {
            if !snap.has_prop(prop) {
                continue;
            }
            let (b2, b3) = (b.clone(), b.clone());
            let seg = b.seg(
                &b.t(title_id, title_en),
                sizes(),
                Rc::new(move || {
                    b2.live(|s| s.read_str(prop).unwrap_or_else(|| default.to_string()))
                }),
                Rc::new(move |v| b3.edit(&[(prop, json!(v))])),
                true,
            );
            rows.push(b.row(&b.t(label_id, label_en), &[seg]));
        }
        (!rows.is_empty()).then(|| b.block(None, rows))
    }

    /// The label of the one row a field shares with the next: a color and its line style, or
    /// an upper and a lower color. A field declares it (`row_label`); plain English labels
    /// from older schemas still pair by their wording.
    fn pair_label(
        &self,
        b: &Build,
        f: &SettingsField,
        next: Option<&SettingsField>,
    ) -> Option<String> {
        let next = next?;
        if f.kind != FieldKind::Color
            || !matches!(next.kind, FieldKind::LineStyle | FieldKind::Color)
        {
            return None;
        }
        if let Some(label) = &f.row_label {
            return Some(b.m(label));
        }
        let (fl, nl) = (plain_text(&f.label)?, plain_text(&next.label)?);
        if next.kind == FieldKind::LineStyle {
            pair_base(&fl, &nl)
        } else {
            upper_lower_base(&fl, &nl)
        }
    }

    /// The control a schema field's kind calls for (`None` when it has nothing to offer).
    fn field_control(&self, b: &Build, f: &SettingsField) -> Option<Element> {
        let path = f.path.clone();
        let label = b.m(&f.label);
        match f.kind {
            FieldKind::Color => {
                let (b2, p, fallback) = (b.clone(), path.clone(), f.fallback.clone());
                Some(b.swatch(
                    &label,
                    Rc::new(move || {
                        b2.live(|s| {
                            s.read_str(&p)
                                .filter(|c| !c.is_empty())
                                .or_else(|| fallback.as_deref().and_then(|fb| s.read_str(fb)))
                        })
                        .flatten()
                        .unwrap_or_else(|| DEFAULT_DRAWING_COLOR.to_string())
                    }),
                    path,
                ))
            }
            FieldKind::LineStyle => {
                let opts = line_style_options()
                    .iter()
                    .map(|o| {
                        SegOpt::new(o.value.clone(), dash_icon(&o.value)).titled(b.m(&o.label))
                    })
                    .collect();
                let (b2, b3, p, p2) = (b.clone(), b.clone(), path.clone(), path);
                Some(b.seg(
                    &label,
                    opts,
                    Rc::new(move || b2.live(|s| s.read_str(&p)).flatten()),
                    Rc::new(move |v| b3.edit(&[(p2.as_str(), json!(v))])),
                    false,
                ))
            }
            FieldKind::Boolean => {
                let (b2, b3, p, p2) = (b.clone(), b.clone(), path.clone(), path);
                Some(b.switch(
                    &label,
                    Rc::new(move || b2.live(|s| s.read_bool(&p) == Some(true)).unwrap_or(false)),
                    Rc::new(move |v| b3.edit(&[(p2.as_str(), json!(v))])),
                ))
            }
            FieldKind::Number | FieldKind::Opacity => {
                let (b2, b3, p, p2) = (b.clone(), b.clone(), path.clone(), path);
                let step = f.step.unwrap_or(1.0);
                Some(
                    b.number(
                        &label,
                        Rc::new(move || b2.live(|s| s.read_f64(&p).unwrap_or(0.0)).unwrap_or(0.0)),
                        f.min,
                        f.max,
                        Some(step),
                        step >= 1.0 && step.fract() == 0.0,
                        Rc::new(move |v| b3.edit(&[(p2.as_str(), json!(v))])),
                    )
                    .el,
                )
            }
            FieldKind::Select => {
                // Only timeframes below the chart's own make sense for a lower-timeframe inset.
                let options: Vec<SelOpt> = if f.path == "magnifier.timeframe" {
                    magnifier_timeframes_below(self.core.chart_bar_ms())
                        .iter()
                        .map(|o| SelOpt::new(o.value, b.m(&o.label)))
                        .collect()
                } else {
                    f.options
                        .iter()
                        .flatten()
                        .map(|o| SelOpt::new(o.value.clone(), b.m(&o.label)))
                        .collect()
                };
                if options.is_empty() {
                    return None;
                }
                let first = options[0].value.clone();
                let (b2, b3, p, p2) = (b.clone(), b.clone(), path.clone(), path);
                Some(
                    b.select(
                        &label,
                        &options,
                        Rc::new(move || {
                            b2.live(|s| {
                                s.read_str(&p)
                                    .or_else(|| s.read(&p).map(|v| value_text(&v)))
                            })
                            .flatten()
                            .unwrap_or_else(|| first.clone())
                        }),
                        Rc::new(move |v| b3.edit(&[(p2.as_str(), select_value(&v))])),
                    )
                    .el,
                )
            }
            FieldKind::Text => {
                let (b2, b3, p, p2) = (b.clone(), b.clone(), path.clone(), path);
                let c = controls::text_field(
                    &b.cx,
                    &b.uid("vela-dsp-text"),
                    &label,
                    Rc::new(move || {
                        b2.live(|s| s.read_str(&p).unwrap_or_default())
                            .unwrap_or_default()
                    }),
                    Rc::new(move |v| b3.edit(&[(p2.as_str(), json!(v))])),
                );
                b.watch_control(&c);
                crate::dom::set_style(&c.el, "width", "100%");
                Some(c.el)
            }
        }
    }

    // ── levels ──

    /// A levelled tool's levels: every level in a two-column grid (on / off, ratio, color):
    /// the ones that are off stay listed, ready to switch on. A retracement leads with its
    /// ready-made sets; the horizontal fibs follow with how their level text and fill show.
    fn levels_block(&self, b: &Build, snap: &Snap) -> Element {
        let mut rows: Vec<Element> = Vec::new();
        let grid = b.cx.el("div", "vela-dsp-lv");
        let key = {
            let b = b.clone();
            move || {
                b.live(|s| serde_json::to_string(&s.levels).unwrap_or_default())
                    .unwrap_or_default()
            }
        };
        let last = Rc::new(RefCell::new(String::new()));
        // A level edited here is already shown: only a change from elsewhere (a preset, an
        // undo) rebuilds the grid, and never under the field being typed in.
        let grid_cx: Rc<RefCell<Cx>> = Rc::new(RefCell::new(b.sub_cx()));
        let fill: Rc<dyn Fn()> = {
            let (b, grid, last, grid_cx, key, ui) = (
                b.clone(),
                grid.clone(),
                last.clone(),
                grid_cx.clone(),
                key.clone(),
                self.ui.clone(),
            );
            let free = snap.type_key().starts_with("fib") && snap.has_prop("labelSide"); // horizontal levels may sit at or beyond either anchor
            Rc::new(move || {
                *last.borrow_mut() = key();
                let old: Vec<Listener> = std::mem::take(&mut *grid_cx.borrow().bag.borrow_mut());
                if let Some(ui) = ui.upgrade() {
                    ui.retire(old);
                }
                util::clear(&grid);
                let cx = grid_cx.borrow().clone();
                let levels = b.live(|s| s.levels.clone()).flatten().unwrap_or_default();
                for (i, lv) in levels.iter().enumerate() {
                    let cell = cx.el("div", "vela-dsp-lv-c");
                    util::set_flag(&cell, "data-off", !lv.enabled);
                    let n = i + 1;
                    let level_text = b.tr().mv(
                        &msg("drawing.settings.level", "Level {n}"),
                        &[("n", MessageArg::Number(n as f64))],
                    );
                    let color_text = b.tr().mv(
                        &msg("drawing.settings.levelColor", "Level {n} color"),
                        &[("n", MessageArg::Number(n as f64))],
                    );
                    let (b2, b3, cell2, last2, key2) = (
                        b.clone(),
                        b.clone(),
                        cell.clone(),
                        last.clone(),
                        key.clone(),
                    );
                    let on = b.bare_switch(
                        &level_text,
                        Rc::new(move || {
                            b2.live(|s| s.levels.as_ref().and_then(|l| l.get(i).map(|l| l.enabled)))
                                .flatten()
                                .unwrap_or(false)
                        }),
                        Rc::new(move |v| {
                            util::set_flag(&cell2, "data-off", !v);
                            b3.edit(&[(format!("levels.{i}.enabled").as_str(), json!(v))]);
                            *last2.borrow_mut() = key2();
                        }),
                    );
                    // The ratio field takes any number on a horizontal tool; elsewhere it stays above 0.
                    let ratio = Rc::new(std::cell::Cell::new(lv.ratio));
                    let holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
                    let (b2, b3, ratio2, ratio3, holder2, last3, key3) = (
                        b.clone(),
                        b.clone(),
                        ratio.clone(),
                        ratio.clone(),
                        holder.clone(),
                        last.clone(),
                        key.clone(),
                    );
                    let num = b.number(
                        &level_text,
                        Rc::new(move || {
                            let v = b2
                                .live(|s| s.levels.as_ref().and_then(|l| l.get(i).map(|l| l.ratio)))
                                .flatten()
                                .unwrap_or(ratio2.get());
                            ratio2.set(v);
                            v
                        }),
                        if free { None } else { Some(0.0) },
                        None,
                        Some(0.01),
                        false,
                        Rc::new(move |v| {
                            if !v.is_finite() || (!free && v <= 0.0) {
                                if let Some(sync) = holder2.borrow().clone() {
                                    sync();
                                }
                                return;
                            }
                            ratio3.set(v);
                            b3.edit(&[(format!("levels.{i}.ratio").as_str(), json!(v))]);
                            *last3.borrow_mut() = key3();
                        }),
                    );
                    *holder.borrow_mut() = Some(num.sync.clone());
                    crate::dom::attr(&num.el, "data-compact", "");
                    let (b2, b3, last4, key4) = (b.clone(), b.clone(), last.clone(), key.clone());
                    let swatch = b.color_with(
                        &color_text,
                        Rc::new(move || {
                            b2.live(|s| {
                                s.levels
                                    .as_ref()
                                    .and_then(|l| l.get(i).map(|l| l.color.clone()))
                            })
                            .flatten()
                            .unwrap_or_default()
                        }),
                        Rc::new(move |v| {
                            b3.edit(&[(format!("levels.{i}.color").as_str(), json!(v))]);
                            *last4.borrow_mut() = key4();
                        }),
                    );
                    let _ = cell.append_child(&on.el);
                    let _ = cell.append_child(&num.el);
                    let _ = cell.append_child(&swatch);
                    let _ = grid.append_child(&cell);
                }
            })
        };
        fill();
        {
            // Only a change from elsewhere rebuilds the grid, never under the focused field.
            let (b2, grid, last, key, fill) = (
                b.clone(),
                grid.clone(),
                last.clone(),
                key.clone(),
                fill.clone(),
            );
            let _ = &b2;
            b.watch(move || {
                let focused = active_inside(&grid);
                if *last.borrow() != key() && !focused {
                    fill();
                }
            });
        }

        let is_retracement = snap.type_key() == "fibretracement";
        let has_flags = snap.has_prop("labelSide");
        if is_retracement {
            let presets = fib_presets();
            let opts = presets
                .iter()
                .map(|p| SegOpt::new(p.key.as_str(), b.m(&p.label)))
                .collect();
            let (b2, b3) = (b.clone(), b.clone());
            rows.push(b.seg(
                &b.t("drawing.settings.levelSet", "Level set"),
                opts,
                Rc::new(move || {
                    b2.live(|s| {
                        let levels = s.levels.clone().unwrap_or_default();
                        match_fib_preset(&levels, s.read_bool("reverse") == Some(true))
                            .map(|k| k.as_str().to_string())
                    })
                    .flatten()
                }),
                Rc::new(move |k| {
                    if let Some(p) = presets.iter().find(|p| p.key.as_str() == k) {
                        b3.edit(&[
                            (
                                "levels",
                                serde_json::to_value(&p.levels).unwrap_or(Value::Null),
                            ),
                            ("reverse", json!(p.reverse)),
                        ]);
                    }
                }),
                true,
            ));
        }
        if snap.is_mach {
            let (b2, b3) = (b.clone(), b.clone());
            let sw = b.switch(
                &b.t("drawing.settings.ratios", "Ratios"),
                Rc::new(move || {
                    b2.live(|s| s.read_bool("showRatios") != Some(false))
                        .unwrap_or(true)
                }),
                Rc::new(move |v| b3.edit(&[("showRatios", json!(v))])),
            );
            rows.push(b.row(&b.t("drawing.settings.ratios", "Ratios"), &[sw]));
        }
        rows.push(grid);
        if has_flags {
            let mut flags = vec![
                self.flag_chip(
                    b,
                    &b.t("drawing.settings.ratios", "Ratios"),
                    "showRatios",
                    None,
                ),
                self.flag_chip(
                    b,
                    &b.t("drawing.settings.prices", "Prices"),
                    "showPrices",
                    None,
                ),
                self.flag_chip(
                    b,
                    &b.t("drawing.settings.background", "Background"),
                    "background",
                    None,
                ),
            ];
            if is_retracement {
                flags.push(self.flag_chip(
                    b,
                    &b.t("drawing.settings.reverse", "Reverse"),
                    "reverse",
                    None,
                ));
            }
            rows.push(b.row(&b.t("drawing.settings.show", "Show"), &[b.chips(&flags)]));
            let (b2, b3) = (b.clone(), b.clone());
            let seg = b.seg(
                &b.t("drawing.settings.labelSide", "Label side"),
                vec![
                    SegOpt::new("left", b.t("drawing.settings.left", "Left")),
                    SegOpt::new("right", b.t("drawing.settings.right", "Right")),
                ],
                Rc::new(move || b2.live(|s| s.read_str("labelSide")).flatten()),
                Rc::new(move |v| b3.edit(&[("labelSide", json!(v))])),
                false,
            );
            rows.push(b.row(&b.t("drawing.settings.labelSide", "Label side"), &[seg]));
        }
        b.block(Some(&b.t("drawing.settings.levels", "Levels")), rows)
    }

    // ── text ──

    /// A note, a callout, a text label: the words lead the panel.
    fn text_content_block(&self, b: &Build, snap: &Snap) -> Element {
        let b2 = b.clone();
        let area = b.text_area(
            &b.t("drawing.settings.text", "Text"),
            3,
            Rc::new(move || {
                b2.live(|s| s.doc.text.as_ref().map(|t| t.value.clone()))
                    .flatten()
                    .unwrap_or_default()
            }),
        );
        crate::dom::set_style(&area, "width", "100%");
        let styling = self.text_styling(b, snap);
        b.block(
            None,
            vec![
                area,
                b.row(&b.t("drawing.settings.style", "Style"), &styling),
            ],
        )
    }

    /// A shape's optional label: folded to one line saying what it reads.
    fn text_fold(&self, b: &Build, snap: &Snap) -> Element {
        let has_value = snap.has_field("text.value");
        let type_key = snap.type_key().to_string();
        let b2 = b.clone();
        let summary: Rc<dyn Fn() -> String> = Rc::new(move || {
            b2.live(|s| {
                if !s.has_field("text.value") {
                    let size = s
                        .doc
                        .text
                        .as_ref()
                        .map_or("normal".to_string(), |t| size_key(&t.size));
                    return text_size_options()
                        .iter()
                        .find(|o| o.value == size)
                        .map_or_else(
                            || b2.t("drawing.settings.normal", "Normal"),
                            |o| b2.m(&o.label),
                        );
                }
                let words = s
                    .doc
                    .text
                    .as_ref()
                    .map(|t| t.value.trim().to_string())
                    .unwrap_or_default();
                if words.is_empty() {
                    b2.t("drawing.settings.none", "None")
                } else {
                    format!("\u{201c}{}\u{201d}", words.lines().next().unwrap_or(""))
                }
            })
            .unwrap_or_default()
        });
        let title = if has_value {
            b.t("drawing.settings.label", "Label")
        } else {
            b.t("drawing.settings.text", "Text")
        };
        let snap = snap.clone();
        b.fold("text", &title, summary, move |b| {
            let mut rows: Vec<Element> = Vec::new();
            if has_value {
                let b2 = b.clone();
                rows.push(b.text_area(
                    &b.t("drawing.settings.addALabel", "Add a label"),
                    1,
                    Rc::new(move || {
                        b2.live(|s| s.doc.text.as_ref().map(|t| t.value.clone()))
                            .flatten()
                            .unwrap_or_default()
                    }),
                ));
            }
            let styling = text_styling(b, &snap);
            rows.push(b.row(&b.t("drawing.settings.style", "Style"), &styling));
            if LINE_LABEL_TYPES.contains(&type_key.as_str()) && has_value {
                let (b2, b3) = (b.clone(), b.clone());
                let place = b.row(
                    &b.t("drawing.settings.place", "Position"),
                    &[b.seg(
                        &b.t(
                            "drawing.settings.positionAlongTheLine",
                            "Position along the line",
                        ),
                        vec![
                            SegOpt::new("start", b.t("drawing.settings.start", "Start")),
                            SegOpt::new("middle", b.t("drawing.settings.middle", "Middle")),
                            SegOpt::new("end", b.t("drawing.settings.end", "End")),
                        ],
                        Rc::new(move || {
                            Some(
                                b2.live(|s| {
                                    s.read_str("text.place").unwrap_or_else(|| "middle".into())
                                })
                                .unwrap_or_default(),
                            )
                        }),
                        Rc::new(move |v| b3.edit(&[("text.place", json!(v))])),
                        true,
                    )],
                );
                let (b2, b3) = (b.clone(), b.clone());
                let side = b.row(
                    &b.t("drawing.settings.side", "Side"),
                    &[b.seg(
                        &b.t("drawing.settings.sideOfTheLine", "Side of the line"),
                        vec![
                            SegOpt::new("above", b.t("drawing.settings.above", "Above")),
                            SegOpt::new("below", b.t("drawing.settings.below", "Below")),
                        ],
                        Rc::new(move || {
                            Some(
                                b2.live(|s| {
                                    s.read_str("text.side").unwrap_or_else(|| "above".into())
                                })
                                .unwrap_or_default(),
                            )
                        }),
                        Rc::new(move |v| b3.edit(&[("text.side", json!(v))])),
                        true,
                    )],
                );
                // Where the label sits only matters once there is one.
                let (p2, s2, b2) = (place.clone(), side.clone(), b.clone());
                b.watch(move || {
                    let empty = b2
                        .live(|s| {
                            s.doc
                                .text
                                .as_ref()
                                .is_none_or(|t| t.value.trim().is_empty())
                        })
                        .unwrap_or(true);
                    util::set_hidden(&p2, empty);
                    util::set_hidden(&s2, empty);
                });
                rows.push(place);
                rows.push(side);
            }
            rows
        })
    }

    fn text_styling(&self, b: &Build, snap: &Snap) -> Vec<Element> {
        text_styling(b, snap)
    }

    // ── points ──

    /// Each anchor's price (editable) and time.
    fn points_fold(&self, b: &Build, snap: &Snap) -> Option<Element> {
        let n = snap.doc.anchors.len();
        if NO_POINTS.contains(&snap.type_key()) || n == 0 || n > MAX_POINTS {
            return None;
        }
        let shown: Vec<usize> = (0..n).filter(|i| snap.free(*i) != FreeAxis::None).collect();
        if shown.is_empty() {
            return None;
        }
        let priced: Vec<usize> = shown
            .iter()
            .copied()
            .filter(|i| snap.free(*i) != FreeAxis::X)
            .collect();
        let bar_ms = self.core.chart_bar_ms();
        let tz = self.core.time_zone();
        let decimals = self.core.price_decimals();
        let stamp = {
            let tz = tz.clone();
            move |ms: f64| {
                format_time_stamp(ms, &tz, bar_ms, &english_format(), FormatSurface::Crosshair)
            }
        };
        let (shown2, priced2, b2, stamp2) =
            (shown.clone(), priced.clone(), b.clone(), stamp.clone());
        let summary: Rc<dyn Fn() -> String> = Rc::new(move || {
            b2.live(|s| {
                let a = &s.doc.anchors;
                if !priced2.is_empty() {
                    return priced2
                        .iter()
                        .map(|i| fmt_num(shown_price(a.get(*i).map_or(0.0, |p| p.price), decimals)))
                        .collect::<Vec<_>>()
                        .join(" \u{2192} ");
                }
                let time = a.get(shown2[0]).map(|p| p.time);
                match time {
                    Some(t) if shown2.len() == 1 => stamp2(t),
                    _ => b2.tr().mv(
                        &msg(
                            "drawing.settings.pointCount",
                            "{count, plural, one {# point} other {# points}}",
                        ),
                        &[("count", MessageArg::Number(shown2.len() as f64))],
                    ),
                }
            })
            .unwrap_or_default()
        });
        let slots_snap = snap.clone();
        Some(b.fold(
            "points",
            &b.t("drawing.settings.points", "Points"),
            summary,
            move |b| {
                shown
                    .iter()
                    .enumerate()
                    .map(|(k, &i)| {
                        let free = slots_snap.free(i);
                        let key = if shown.len() == 1 {
                            if free == FreeAxis::X {
                                b.t("drawing.settings.pointTime", "Time")
                            } else {
                                b.t("drawing.settings.pointPrice", "Price")
                            }
                        } else {
                            char::from(b'A' + k as u8).to_string()
                        };
                        let line = b.cx.el("div", "vela-dsp-pt");
                        let _ = line.append_child(&b.cx.text("span", "vela-dsp-pt-k", &key));
                        if free != FreeAxis::X {
                            let title = b.tr().mv(
                                &msg("drawing.settings.pointPriceTitle", "Point {point} price"),
                                &[("point", MessageArg::Text(key.clone().into()))],
                            );
                            let (b2, b3) = (b.clone(), b.clone());
                            let ni = b.number(
                                &title,
                                Rc::new(move || {
                                    b2.live(|s| {
                                        shown_price(
                                            s.doc.anchors.get(i).map_or(0.0, |a| a.price),
                                            decimals,
                                        )
                                    })
                                    .unwrap_or(0.0)
                                }),
                                None,
                                None,
                                None,
                                false,
                                Rc::new(move |v| {
                                    // The anchors travel as one list: the edit changes one price in it.
                                    let anchors = b3.live(|s| {
                                        let mut a = s.doc.anchors.clone();
                                        if let Some(p) = a.get_mut(i) {
                                            p.price = v;
                                        }
                                        serde_json::to_value(a).unwrap_or(Value::Null)
                                    });
                                    if let Some(anchors) = anchors {
                                        b3.edit(&[("anchors", anchors)]);
                                    }
                                }),
                            );
                            crate::dom::attr(&ni.el, "data-fill", "");
                            let _ = line.append_child(&ni.el);
                        } else {
                            let _ = line.append_child(&b.cx.el("span", ""));
                        }
                        let time = b.cx.el("span", "vela-dsp-pt-t");
                        let (t2, b2, stamp) = (time.clone(), b.clone(), stamp.clone());
                        b.watch(move || {
                            let text = b2
                                .live(|s| s.doc.anchors.get(i).map(|a| stamp(a.time)))
                                .flatten()
                                .unwrap_or_default();
                            t2.set_text_content(Some(&text));
                        });
                        let _ = line.append_child(&time);
                        line
                    })
                    .collect()
            },
        ))
    }

    // ── show on ──

    /// The timeframes the drawing shows on: one line while it is every timeframe, the band
    /// picker only once the user goes custom.
    fn show_on_fold(&self, b: &Build) -> Element {
        // Read from the drawing on every refresh, so an undo or a change elsewhere shows;
        // "Custom" sticks only while it was picked here and the drawing is still limited.
        let picked_custom = Rc::new(Cell::new(false));
        let bands = {
            let b = b.clone();
            move || -> Option<Vec<String>> { b.live(|s| s.show_on()).flatten() }
        };
        let mode = {
            let (bands, picked) = (bands.clone(), picked_custom.clone());
            move || -> ShowMode {
                let current = bands();
                if picked.get() && current.is_some() {
                    ShowMode::Custom
                } else {
                    mode_of(current.as_deref())
                }
            }
        };
        let here = timeframe_band_of(self.core.chart_bar_ms());
        let all_bands = timeframe_bands();
        let (bands2, mode2, b2, all2) = (bands.clone(), mode.clone(), b.clone(), all_bands.clone());
        let summary: Rc<dyn Fn() -> String> = Rc::new(move || {
            let Some(s) = bands2() else {
                return b2.t("drawing.settings.allTimeframes", "All timeframes");
            };
            match mode2() {
                ShowMode::Intraday => return b2.t("drawing.settings.intraday", "Intraday"),
                ShowMode::Higher => return b2.t("drawing.settings.n1HAndUp", "1H and up"),
                _ => {}
            }
            if s.is_empty() {
                return b2.t("drawing.settings.nowhere", "Nowhere");
            }
            s.iter()
                .map(|k| {
                    all2.iter()
                        .find(|band| band.key == k)
                        .map_or_else(|| k.clone(), |band| b2.m(&band.label))
                })
                .collect::<Vec<_>>()
                .join(", ")
        });
        b.fold(
            "show",
            &b.t("drawing.settings.showOn", "Show on"),
            summary,
            move |b| {
                let set: Rc<dyn Fn(Option<Vec<String>>)> = {
                    let b = b.clone();
                    Rc::new(move |next| {
                        let v = next.map_or(Value::Null, |keys| json!(keys));
                        b.edit(&[("showOn", v)]);
                    })
                };
                let pick = {
                    let (set, bands, picked, all) = (
                        set.clone(),
                        bands.clone(),
                        picked_custom.clone(),
                        all_bands.clone(),
                    );
                    let mode = mode.clone();
                    b.seg(
                        &b.t("drawing.settings.showOn", "Show on"),
                        vec![
                            SegOpt::new("all", b.t("drawing.settings.all", "All")),
                            SegOpt::new("intraday", b.t("drawing.settings.intraday", "Intraday")),
                            SegOpt::new("higher", b.t("drawing.settings.n1HAndUp", "1H and up")),
                            SegOpt::new("custom", b.t("drawing.settings.custom", "Custom")),
                        ],
                        Rc::new(move || Some(mode().as_str().to_string())),
                        Rc::new(move |v| {
                            picked.set(v == "custom");
                            match v {
                                "all" => set(None),
                                "intraday" => {
                                    set(Some(INTRADAY.iter().map(|s| s.to_string()).collect()))
                                }
                                "higher" => {
                                    set(Some(HIGHER.iter().map(|s| s.to_string()).collect()))
                                }
                                // Start from what shows now.
                                _ => set(Some(bands().unwrap_or_else(|| {
                                    all.iter().map(|b| b.key.to_string()).collect()
                                }))),
                            }
                        }),
                        true,
                    )
                };
                let grid = b.cx.el("div", "vela-dsp-tf");
                for band in &all_bands {
                    let btn = b.cx.text("button", "", &b.m(&band.label));
                    attr(&btn, "type", "button");
                    if Some(band.key) == here {
                        attr(&btn, "data-here", "");
                        attr(
                            &btn,
                            "title",
                            &b.t("drawing.settings.chartTimeframe", "The chart's timeframe"),
                        );
                    }
                    let key = band.key;
                    let (bands_c, set, all) = (bands.clone(), set.clone(), all_bands.clone());
                    b.cx.on(&btn, "click", move |_| {
                        let mut cur: Vec<String> = bands_c().unwrap_or_default();
                        if let Some(at) = cur.iter().position(|k| k == key) {
                            cur.remove(at);
                        } else {
                            cur.push(key.to_string());
                        }
                        set(Some(
                            all.iter()
                                .map(|b| b.key.to_string())
                                .filter(|k| cur.contains(k))
                                .collect(),
                        ));
                    });
                    let (btn2, bands) = (btn.clone(), bands.clone());
                    b.watch(move || {
                        let on = bands().is_none_or(|s| s.iter().any(|k| k == key));
                        util::set_pressed(&btn2, on);
                    });
                    let _ = grid.append_child(&btn);
                }
                let note = b.cx.el("div", "vela-dsp-note");
                {
                    let (grid, note, bands, mode, b2) = (
                        grid.clone(),
                        note.clone(),
                        bands.clone(),
                        mode.clone(),
                        b.clone(),
                    );
                    b.watch(move || {
                        util::set_hidden(&grid, mode() != ShowMode::Custom);
                        let s = bands();
                        let hidden = s
                            .as_ref()
                            .is_some_and(|s| here.is_some_and(|h| !s.iter().any(|k| k == h)));
                        util::set_hidden(&note, !hidden);
                        note.set_text_content(Some(&if s.as_ref().is_some_and(Vec::is_empty) {
                            b2.t(
                                "drawing.settings.hiddenOnEveryTimeframe",
                                "Hidden on every timeframe.",
                            )
                        } else {
                            b2.t(
                                "drawing.settings.hiddenOnThisTimeframe",
                                "Hidden on this chart's timeframe.",
                            )
                        }));
                    });
                }
                vec![pick, grid, note]
            },
        )
    }

    // ── the style menu ──

    fn all_templates(&self, snap: &Snap) -> Vec<DrawingToolTemplate> {
        let mut all = builtin_tool_templates(snap.type_key());
        all.extend(self.core.templates());
        all
    }

    fn template_name(&self, tpl: &DrawingToolTemplate) -> String {
        match &tpl.label {
            Some(label) => self.cx.tr.m(label),
            None => tpl.name.clone(),
        }
    }

    /// The header button names the style the drawing wears (the last match), else "Style".
    fn refresh_style_label(&self) {
        let Some(snap) = self.core.first() else {
            return;
        };
        let matched = self
            .all_templates(&snap)
            .into_iter()
            .rfind(|t| matches_template(&snap, &t.settings));
        let (text, title) = match &matched {
            Some(m) => {
                let name = self.template_name(m);
                (
                    name.clone(),
                    self.cx.tr.mv(
                        &msg("drawing.settings.styleTitle", "Style: {name}"),
                        &[("name", MessageArg::Text(name.into()))],
                    ),
                )
            }
            None => (
                self.t("drawing.settings.style", "Style"),
                self.t("drawing.settings.styles", "Styles"),
            ),
        };
        self.style_label.set_text_content(Some(&text));
        attr(&self.style_btn, "title", &title);
        attr(&self.style_btn, "aria-label", &title);
    }

    /// The header's style menu: the tool's ready-made and saved looks (hover to preview,
    /// click to apply), saving the current look, and going back to the tool's defaults.
    fn open_style_menu(&self) {
        let me = self.me.clone();
        let holder: Rc<RefCell<Option<(Element, Cx)>>> = Rc::new(RefCell::new(None));
        let fill: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let core = self.core.clone();
        let on_close: Box<dyn FnOnce()> = {
            let (core, holder, fill) = (core.clone(), holder.clone(), fill.clone());
            Box::new(move || {
                core.preview(None);
                // The menu and its refill closure hold each other: let go of both.
                holder.borrow_mut().take();
                fill.borrow_mut().take();
            })
        };
        let (holder2, fill2) = (holder.clone(), fill.clone());
        self.floats.open(
            &self.style_btn,
            true,
            6000,
            "vela-dsp-menu",
            "4px",
            Some(on_close),
            move |el, cx| {
                attr(el, "role", "menu");
                *holder2.borrow_mut() = Some((el.clone(), cx.clone()));
                let me2 = me.clone();
                let (holder3, fill3) = (holder2.clone(), fill2.clone());
                let refill: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(p) = me2.upgrade() else { return };
                    let Some((el, cx)) = holder3.borrow().clone() else {
                        return;
                    };
                    let refill = fill3.borrow().clone();
                    p.fill_style_menu(&el, &cx, refill);
                });
                *fill2.borrow_mut() = Some(refill.clone());
                if let Some(p) = me.upgrade() {
                    p.fill_style_menu(el, cx, Some(refill));
                }
            },
        );
    }

    fn fill_style_menu(&self, el: &Element, cx: &Cx, refill: Option<Rc<dyn Fn()>>) {
        let Some(snap) = self.core.first() else {
            return;
        };
        util::clear(el);
        let tr = self.cx.tr.clone();
        let builtins = builtin_tool_templates(snap.type_key());
        let saved = self.core.templates();
        let me = self.me.clone();
        let item = |tpl: &DrawingToolTemplate, removable: bool| -> Element {
            let row = cx.el("button", "vela-dsp-mi");
            attr(&row, "type", "button");
            attr(&row, "role", "menuitem");
            if matches_template(&snap, &tpl.settings) {
                attr(&row, "data-current", "");
            }
            let dot = cx.el("span", "vela-dsp-mi-dot");
            set_style(
                &dot,
                "background",
                &template_color(&tpl.settings).unwrap_or_else(|| "transparent".into()),
            );
            let name = self.template_name(tpl);
            let text = cx.text("span", "vela-dsp-mi-t", &name);
            let _ = row.append_child(&dot);
            let _ = row.append_child(&text);
            if removable {
                let x = cx.el("span", "vela-dsp-mi-x");
                attr(&x, "role", "button");
                attr(&x, "tabindex", "0");
                attr(
                    &x,
                    "aria-label",
                    &tr.mv(
                        &msg("drawing.settings.deleteStyle", "Delete {name}"),
                        &[("name", MessageArg::Text(name.clone().into()))],
                    ),
                );
                x.set_inner_html(glyphs::CROSS);
                let (core, refill, me, tpl_name) = (
                    self.core.clone(),
                    refill.clone(),
                    me.clone(),
                    tpl.name.clone(),
                );
                cx.on(&x, "click", move |ev| {
                    stop_propagation(&ev);
                    core.preview(None);
                    core.remove_template(&tpl_name);
                    if let Some(r) = &refill {
                        r();
                    }
                    if let Some(p) = me.upgrade() {
                        p.refresh_style_label();
                    }
                });
                let _ = row.append_child(&x);
            }
            {
                let (core, settings) = (self.core.clone(), tpl.settings.clone());
                cx.on(&row, "pointerenter", move |_| core.preview(Some(&settings)));
                let core = self.core.clone();
                cx.on(&row, "pointerleave", move |_| core.preview(None));
            }
            {
                let (core, settings, me) = (self.core.clone(), tpl.settings.clone(), me.clone());
                cx.on(&row, "click", move |_| {
                    core.preview(None);
                    core.apply_tool_settings(&settings);
                    if let Some(p) = me.upgrade() {
                        p.floats.close();
                        p.rebuild();
                    }
                });
            }
            row
        };
        let heading = |text: String| cx.text("div", "vela-dsp-mh", &text);
        if !builtins.is_empty() {
            let _ = el.append_child(&heading(self.t("drawing.settings.presets", "Presets")));
            for t in &builtins {
                let _ = el.append_child(&item(t, false));
            }
        }
        if !saved.is_empty() {
            let _ = el.append_child(&heading(self.t("drawing.settings.saved", "Saved")));
            for t in &saved {
                let _ = el.append_child(&item(t, true));
            }
        }
        if !builtins.is_empty() || !saved.is_empty() {
            let _ = el.append_child(&cx.el("div", "vela-dsp-msep"));
        }
        let _ = el.append_child(&self.save_row(cx, refill));
        let reset = cx.el("button", "vela-dsp-mi");
        attr(&reset, "type", "button");
        attr(&reset, "role", "menuitem");
        let _ = reset.append_child(&cx.text(
            "span",
            "vela-dsp-mi-t",
            &self.t("drawing.settings.resetToDefault", "Reset to default"),
        ));
        {
            let (core, me) = (self.core.clone(), self.me.clone());
            cx.on(&reset, "click", move |_| {
                core.reset_settings();
                if let Some(p) = me.upgrade() {
                    p.floats.close();
                    p.rebuild();
                }
            });
        }
        let _ = el.append_child(&reset);
    }

    /// "Save as style...": a menu row that turns into a name field.
    fn save_row(&self, cx: &Cx, refill: Option<Rc<dyn Fn()>>) -> Element {
        let wrap = cx.el("div", "");
        let start = cx.el("button", "vela-dsp-mi");
        attr(&start, "type", "button");
        attr(&start, "role", "menuitem");
        let _ = start.append_child(&cx.text(
            "span",
            "vela-dsp-mi-t",
            &self.t("drawing.settings.saveAsStyle", "Save as style\u{2026}"),
        ));
        let (cx2, wrap2, core, me) = (cx.clone(), wrap.clone(), self.core.clone(), self.me.clone());
        let name_label = self.t("drawing.settings.styleName", "Style name");
        let save_label = self.t("drawing.settings.save", "Save");
        cx.on(&start, "click", move |ev| {
            stop_propagation(&ev);
            let form = cx2.el("div", "vela-dsp-mname");
            let input = cx2.el("input", "");
            attr(&input, "type", "text");
            attr(&input, "placeholder", &name_label);
            attr(&input, "maxlength", "60");
            attr(&input, "aria-label", &name_label);
            let commit: Rc<dyn Fn()> = {
                let (input, core, refill, me) =
                    (input.clone(), core.clone(), refill.clone(), me.clone());
                Rc::new(move || {
                    let name = crate::dom::vela_get_value(&input).trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    core.save_template(&name);
                    if let Some(r) = &refill {
                        r();
                    }
                    if let Some(p) = me.upgrade() {
                        p.refresh_style_label();
                    }
                })
            };
            let save = {
                let commit = commit.clone();
                controls::button(&cx2, &save_label, true, false, move || commit())
            };
            set_style(&save, "height", "28px");
            {
                let (refill, commit) = (refill.clone(), commit.clone());
                cx2.on(&input, "keydown", move |ev| {
                    stop_propagation(&ev);
                    match get_str(&ev, "key").as_str() {
                        "Enter" => commit(),
                        "Escape" => {
                            // Back to the row; the panel's own Escape stays out of it.
                            stop_propagation(&ev);
                            prevent_default(&ev);
                            if let Some(r) = &refill {
                                r();
                            }
                        }
                        _ => {}
                    }
                });
            }
            let _ = form.append_child(&input);
            let _ = form.append_child(&save);
            util::clear(&wrap2);
            let _ = wrap2.append_child(&form);
            vela_focus(&input);
        });
        let _ = wrap.append_child(&start);
        wrap
    }
}

/// A select's chosen value as the model stores it: numbers stay numbers.
fn select_value(v: &str) -> Value {
    json!(v)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(fmt_num).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// Whether focus sits inside `el`.
fn active_inside(el: &Element) -> bool {
    let Some(doc) = el.owner_document() else {
        return false;
    };
    let active =
        js_sys::Reflect::get(&doc, &JsValue::from_str("activeElement")).unwrap_or(JsValue::NULL);
    !active.is_null() && !active.is_undefined() && vela_contains(el, &active)
}

/// Color, size and bold / italic of a drawing's text.
fn text_styling(b: &Build, snap: &Snap) -> Vec<Element> {
    let has = |p: &str| snap.has_field(p);
    let mut out: Vec<Element> = Vec::new();
    if has("text.color") {
        let b2 = b.clone();
        let tc = b.core.theme_color("textColor").unwrap_or_default();
        out.push(b.swatch(
            &b.t("drawing.settings.textColor", "Text color"),
            Rc::new(move || {
                b2.live(|s| s.doc.text.as_ref().and_then(|t| t.color.clone()))
                    .flatten()
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| tc.clone())
            }),
            "text.color".into(),
        ));
    }
    if has("text.size") {
        let options: Vec<SelOpt> = text_size_options()
            .iter()
            .map(|o| SelOpt::new(o.value.clone(), b.m(&o.label)))
            .collect();
        let (b2, b3) = (b.clone(), b.clone());
        let sel = b.select(
            &b.t("drawing.bar.textSize", "Text size"),
            &options,
            Rc::new(move || {
                b2.live(|s| {
                    s.doc
                        .text
                        .as_ref()
                        .map_or("normal".to_string(), |t| size_key(&t.size))
                })
                .unwrap_or_default()
            }),
            Rc::new(move |v| b3.edit(&[("text.size", json!(v))])),
        );
        out.push(sel.el);
    }
    let mut flags: Vec<Element> = Vec::new();
    for (path, glyph, id, en) in [
        ("text.bold", "B", "drawing.settings.bold", "Bold"),
        ("text.italic", "I", "drawing.settings.italic", "Italic"),
    ] {
        if !has(path) {
            continue;
        }
        let (b2, b3) = (b.clone(), b.clone());
        let key = path.strip_prefix("text.").unwrap_or(path);
        flags.push(b.chip(
            glyph,
            Rc::new(move || {
                b2.live(|s| {
                    s.doc.text.as_ref().is_some_and(|t| {
                        if key == "bold" {
                            t.bold == Some(true)
                        } else {
                            t.italic == Some(true)
                        }
                    })
                })
                .unwrap_or(false)
            }),
            Rc::new(move |on| b3.edit(&[(path, json!(on))])),
            Some(&b.t(id, en)),
        ));
    }
    if !flags.is_empty() {
        out.push(b.chips(&flags));
    }
    out
}
