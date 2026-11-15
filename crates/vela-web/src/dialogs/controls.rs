//! The native controls the dialogs are made of: what the original's `buildFieldControl`
//! builds from the kit (checkbox, number input, select, color field, text, textarea) plus
//! the ⓘ hint and the footer button. Every control takes a `get` that is the source of its
//! value (re-read by its `sync`) and an `on_change` that receives each edit.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::Element;

use super::Cx;
use super::color::{self, Rgba};
use super::icons;
use super::js::*;
use crate::dom::attr;

/// A built control and the function that re-reads its value from its source.
pub struct Control {
    pub el: Element,
    pub sync: Rc<dyn Fn()>,
}

fn label_attr(el: &Element, aria: &str) {
    if !aria.is_empty() {
        attr(el, "aria-label", aria);
    }
}

/// A checkbox.
pub fn checkbox(
    cx: &Cx,
    id: &str,
    aria: &str,
    get: Rc<dyn Fn() -> bool>,
    on_change: Rc<dyn Fn(bool)>,
) -> Control {
    let el = cx.el("input", "vela-ctl-check");
    attr(&el, "type", "checkbox");
    el.set_id(id);
    label_attr(&el, aria);
    dlg_set_checked(&el, get());
    {
        let el2 = el.clone();
        cx.on(&el, "change", move |_| on_change(dlg_checked(&el2)));
    }
    let el2 = el.clone();
    Control {
        el,
        sync: Rc::new(move || dlg_set_checked(&el2, get())),
    }
}

/// An on/off switch: the checkbox with the `switch` role and the switch look (the kit's
/// `switch` field kind; the dialogs themselves use plain checkboxes).
pub fn switch(
    cx: &Cx,
    id: &str,
    aria: &str,
    get: Rc<dyn Fn() -> bool>,
    on_change: Rc<dyn Fn(bool)>,
) -> Control {
    let control = checkbox(cx, id, aria, get, on_change);
    control.el.set_class_name("vela-ctl-switch");
    attr(&control.el, "role", "switch");
    control
}

/// Options of a number control.
#[derive(Clone, Default)]
pub struct NumOpts {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub integer: bool,
    /// Commit while typing (`true`) or on blur / Enter (`false`).
    pub live: bool,
    pub compact: bool,
    pub aria: String,
    /// At `empty_value` the field reads empty and shows this; clearing stores `empty_value`.
    pub placeholder: Option<String>,
    pub empty_value: Option<f64>,
    /// Clamp out-of-range text on commit (otherwise a live out-of-range value is ignored
    /// until the field settles).
    pub clamp: bool,
}

fn num_text(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// A number input.
pub fn number(
    cx: &Cx,
    id: &str,
    o: NumOpts,
    get: Rc<dyn Fn() -> f64>,
    on_change: Rc<dyn Fn(f64)>,
) -> Control {
    let el = cx.el("input", "vela-ctl-num");
    attr(&el, "type", "number");
    attr(
        &el,
        "inputmode",
        if o.integer { "numeric" } else { "decimal" },
    );
    el.set_id(id);
    label_attr(&el, &o.aria);
    if let Some(m) = o.min {
        attr(&el, "min", &num_text(m));
    }
    if let Some(m) = o.max {
        attr(&el, "max", &num_text(m));
    }
    attr(
        &el,
        "step",
        &num_text(o.step.unwrap_or(if o.integer { 1.0 } else { 0.1 })),
    );
    if o.compact {
        attr(&el, "data-compact", "");
    }
    if let Some(p) = &o.placeholder {
        attr(&el, "placeholder", p);
    }
    let last = Rc::new(Cell::new(get()));
    let show = {
        let el = el.clone();
        let o = o.clone();
        Rc::new(move |v: f64| {
            let blank = o.placeholder.is_some() && o.empty_value == Some(v);
            dlg_set_value(&el, &if blank { String::new() } else { num_text(v) });
        })
    };
    show(last.get());
    let settle = {
        let o = o.clone();
        Rc::new(move |text: &str, force: bool| -> Option<f64> {
            let t = text.trim();
            if t.is_empty() {
                return o.empty_value;
            }
            let mut v: f64 = t.parse().ok().filter(|v: &f64| v.is_finite())?;
            if o.integer {
                v = v.round();
            }
            let below = o.min.is_some_and(|m| v < m);
            let above = o.max.is_some_and(|m| v > m);
            if below || above {
                if !(force || o.clamp) {
                    return None;
                }
                v = v.clamp(o.min.unwrap_or(f64::MIN), o.max.unwrap_or(f64::MAX));
            }
            Some(v)
        })
    };
    let commit = {
        let last = last.clone();
        let on_change = on_change.clone();
        Rc::new(move |v: f64| {
            if v != last.get() {
                last.set(v);
                on_change(v);
            }
        })
    };
    if o.live {
        let (el2, settle, commit) = (el.clone(), settle.clone(), commit.clone());
        cx.on(&el, "input", move |_| {
            if let Some(v) = settle(&dlg_value(&el2), false) {
                commit(v);
            }
        });
    }
    {
        let (el2, settle, commit, show, last) = (
            el.clone(),
            settle.clone(),
            commit.clone(),
            show.clone(),
            last.clone(),
        );
        cx.on(&el, "change", move |_| {
            match settle(&dlg_value(&el2), true) {
                Some(v) => {
                    show(v);
                    commit(v);
                }
                None => show(last.get()),
            }
        });
    }
    Control {
        el,
        sync: Rc::new(move || {
            let v = get();
            last.set(v);
            show(v);
        }),
    }
}

/// One option of a select.
#[derive(Clone)]
pub struct SelOpt {
    pub value: String,
    pub label: String,
    pub disabled: bool,
    pub title: Option<String>,
}

impl SelOpt {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            disabled: false,
            title: None,
        }
    }
}

/// A native dropdown.
pub fn select(
    cx: &Cx,
    id: &str,
    aria: &str,
    opts: &[SelOpt],
    get: Rc<dyn Fn() -> String>,
    on_change: Rc<dyn Fn(String)>,
) -> Control {
    let el = cx.el("select", "vela-ctl-select");
    el.set_id(id);
    label_attr(&el, aria);
    for o in opts {
        let opt = cx.el("option", "");
        attr(&opt, "value", &o.value);
        opt.set_text_content(Some(&o.label));
        if o.disabled {
            attr(&opt, "disabled", "");
        }
        if let Some(t) = &o.title {
            attr(&opt, "title", t);
        }
        let _ = el.append_child(&opt);
    }
    dlg_set_value(&el, &get());
    {
        let el2 = el.clone();
        cx.on(&el, "change", move |_| on_change(dlg_value(&el2)));
    }
    let el2 = el.clone();
    Control {
        el,
        sync: Rc::new(move || dlg_set_value(&el2, &get())),
    }
}

/// A color: a swatch plus an opacity slider. `release` commits once on release instead of
/// on every drag step.
pub fn color_field(
    cx: &Cx,
    id: &str,
    aria: &str,
    release: bool,
    get: Rc<dyn Fn() -> String>,
    on_change: Rc<dyn Fn(String)>,
) -> Control {
    let wrap = cx.el("span", "vela-ctl-color");
    let swatch = cx.el("input", "vela-ctl-swatch");
    attr(&swatch, "type", "color");
    swatch.set_id(id);
    label_attr(&swatch, aria);
    let alpha = cx.el("input", "vela-ctl-alpha");
    attr(&alpha, "type", "range");
    attr(&alpha, "min", "0");
    attr(&alpha, "max", "100");
    attr(&alpha, "step", "1");
    let opacity = cx.tr.t("chart.settings.opacity", "Opacity");
    attr(&alpha, "aria-label", format!("{aria} {opacity}").trim());
    let _ = wrap.append_child(&swatch);
    let _ = wrap.append_child(&alpha);
    let current = Rc::new(RefCell::new(Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 1.0,
    }));
    let paint = {
        let (swatch, alpha, current) = (swatch.clone(), alpha.clone(), current.clone());
        Rc::new(move |css: &str| {
            let c = color::parse(css).unwrap_or(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: if css.trim().is_empty() || css == "transparent" {
                    0.0
                } else {
                    1.0
                },
            });
            *current.borrow_mut() = c;
            dlg_set_value(&swatch, &color::hex(&c));
            dlg_set_value(&alpha, &format!("{}", (c.a * 100.0).round() as i64));
        })
    };
    paint(&get());
    let emit = {
        let current = current.clone();
        Rc::new(move || on_change(color::css(&current.borrow())))
    };
    let events: &[&'static str] = if release {
        &["change"]
    } else {
        &["input", "change"]
    };
    for ev in events {
        {
            let (swatch2, current, emit) = (swatch.clone(), current.clone(), emit.clone());
            cx.on(&swatch, ev, move |_| {
                if let Some(c) = color::parse(&dlg_value(&swatch2)) {
                    let mut cur = current.borrow_mut();
                    if (cur.r, cur.g, cur.b) == (c.r, c.g, c.b) {
                        return;
                    }
                    cur.r = c.r;
                    cur.g = c.g;
                    cur.b = c.b;
                    drop(cur);
                    emit();
                }
            });
        }
        {
            let (alpha2, current, emit) = (alpha.clone(), current.clone(), emit.clone());
            cx.on(&alpha, ev, move |_| {
                let v: f64 = dlg_value(&alpha2).parse().unwrap_or(100.0);
                let a = (v / 100.0).clamp(0.0, 1.0);
                if (current.borrow().a - a).abs() < 1e-9 {
                    return;
                }
                current.borrow_mut().a = a;
                emit();
            });
        }
    }
    Control {
        el: wrap,
        sync: Rc::new(move || paint(&get())),
    }
}

/// A single-line text field that commits on blur / Enter.
pub fn text_field(
    cx: &Cx,
    id: &str,
    aria: &str,
    get: Rc<dyn Fn() -> String>,
    on_change: Rc<dyn Fn(String)>,
) -> Control {
    let el = cx.el("input", "vela-ctl-text");
    attr(&el, "type", "text");
    attr(&el, "autocomplete", "off");
    attr(&el, "spellcheck", "false");
    el.set_id(id);
    label_attr(&el, aria);
    dlg_set_value(&el, &get());
    {
        let el2 = el.clone();
        cx.on(&el, "change", move |_| on_change(dlg_value(&el2)));
    }
    let el2 = el.clone();
    Control {
        el,
        sync: Rc::new(move || dlg_set_value(&el2, &get())),
    }
}

/// A multi-line text field that commits on blur.
pub fn textarea(
    cx: &Cx,
    id: &str,
    aria: &str,
    get: Rc<dyn Fn() -> String>,
    on_change: Rc<dyn Fn(String)>,
) -> Control {
    let el = cx.el("textarea", "vela-ctl-textarea");
    el.set_id(id);
    label_attr(&el, aria);
    dlg_set_value(&el, &get());
    {
        let el2 = el.clone();
        cx.on(&el, "change", move |_| on_change(dlg_value(&el2)));
    }
    let el2 = el.clone();
    Control {
        el,
        sync: Rc::new(move || dlg_set_value(&el2, &get())),
    }
}

/// The ⓘ mark after a label: its text is the tooltip and the accessible description. With a
/// URL (http / https only) it is a link opening in a new tab (a group's help mark).
pub fn hint(cx: &Cx, text: &str, href: Option<&str>, label: &str, help_icon: bool) -> Element {
    // `vela-hint` is the original's class (what hosts and probes look for).
    let glyph = icons::icon(if help_icon { "help" } else { "info" }, 16);
    let el = match href {
        Some(url) => {
            let a = cx.el("a", "vela-ctl-hint vela-hint");
            attr(&a, "href", url);
            attr(&a, "target", "_blank");
            attr(&a, "rel", "noopener noreferrer");
            a
        }
        None => {
            let b = cx.el("button", "vela-ctl-hint vela-hint");
            attr(&b, "type", "button");
            b
        }
    };
    el.set_inner_html(&glyph);
    let name = if text.is_empty() { label } else { text };
    attr(&el, "aria-label", name);
    if !text.is_empty() {
        attr(&el, "title", text);
    }
    el
}

/// A URL cleared for a link or image source, or `None` (`safeUrl` of the original's
/// `sanitize-html.ts`; one implementation, shared with the mark popup's sanitizer).
pub fn safe_url(value: &str, absolute_only: bool) -> Option<String> {
    crate::chrome_ui::sanitize::safe_url(value, absolute_only)
}

/// A footer button.
pub fn button(
    cx: &Cx,
    text: &str,
    primary: bool,
    start: bool,
    on_click: impl Fn() + 'static,
) -> Element {
    let b = cx.el("button", "vela-dlg-btn");
    attr(&b, "type", "button");
    b.set_text_content(Some(text));
    if primary {
        attr(&b, "data-primary", "");
    }
    if start {
        attr(&b, "data-start", "");
    }
    cx.on(&b, "click", move |_| on_click());
    b
}

#[cfg(test)]
mod tests {
    use super::safe_url;

    // Ported from `test/sanitize-html.test.ts` ("rejects code and content-smuggling URL
    // schemes", "image sources must be absolute http(s)"): the dialogs' help links use the
    // same `safeUrl`.
    #[test]
    fn code_and_content_smuggling_schemes_are_rejected_whitespace_split_ones_included() {
        let ok = |s: &str| safe_url(s, false);
        assert_eq!(
            ok("https://example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(ok("/relative/path").as_deref(), Some("/relative/path"));
        assert_eq!(ok("#anchor").as_deref(), Some("#anchor"));
        assert_eq!(ok("mailto:a@b.c").as_deref(), Some("mailto:a@b.c"));
        assert_eq!(ok("javascript:alert(1)"), None);
        assert_eq!(ok("JavaScript:alert(1)"), None);
        assert_eq!(ok("java\tscript:alert(1)"), None);
        assert_eq!(ok(" \n javascript:alert(1)"), None);
        assert_eq!(ok("data:text/html,hi"), None);
        assert_eq!(ok("vbscript:x"), None);
    }

    #[test]
    fn image_style_urls_must_be_absolute_http() {
        assert_eq!(
            safe_url("https://cdn.example.com/x.png", true).as_deref(),
            Some("https://cdn.example.com/x.png")
        );
        assert_eq!(safe_url("/x.png", true), None);
        assert_eq!(safe_url("data:image/png;base64,AAAA", true), None);
        // Group help links take the absolute-only form: nothing but http(s).
        assert_eq!(safe_url("mailto:a@b.c", true), None);
        assert_eq!(safe_url("java\tscript:alert(1)", true), None);
    }
}
