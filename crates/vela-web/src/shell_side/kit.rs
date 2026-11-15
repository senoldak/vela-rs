//! Small DOM helpers shared by the side surfaces. Everything here sticks to the properties and
//! methods the fake DOM of the smoke checks implements (no `classList`, `dataset`,
//! `querySelector`, `closest`): the real DOM supports them all, but the helpers read the
//! tree through `children`, `parentNode` and attributes so both behave alike.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Reflect;
use vela_core::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Document, Element, HtmlElement};

use crate::dom::{Listener, attr, make};
use crate::workspace_ui::WidgetRoot;

use super::icons;

/// The shared handles every surface needs: the root, its document, the translator.
#[derive(Clone)]
pub struct Cx {
    pub root: Rc<WidgetRoot>,
    pub doc: Document,
}

impl Cx {
    pub fn new(root: &Rc<WidgetRoot>) -> Self {
        Self {
            root: root.clone(),
            doc: root.document().clone(),
        }
    }

    pub fn tr(&self) -> Option<Rc<dyn Translator>> {
        self.root.translator()
    }

    /// Text of a message in the shell's language.
    pub fn t(&self, m: &MessageLike) -> String {
        let tr = self.tr();
        text_of(tr.as_deref(), m, &[])
    }

    pub fn tv(&self, m: &MessageLike, values: &[(&str, MessageArg)]) -> String {
        let tr = self.tr();
        text_of(tr.as_deref(), m, values)
    }

    /// A catalog message by id with its English fallback.
    pub fn m(&self, id: &'static str, en: &'static str) -> String {
        self.t(&msg(id, en))
    }

    pub fn el(&self, tag: &str, class: &str) -> Element {
        make(&self.doc, tag, class)
    }

    pub fn text(&self, tag: &str, class: &str, text: &str) -> Element {
        let e = make(&self.doc, tag, class);
        e.set_text_content(Some(text));
        e
    }

    /// A button with an accessible name, a native tooltip and a `type=button`.
    pub fn button(&self, class: &str, label: &str) -> Element {
        let b = make(&self.doc, "button", class);
        attr(&b, "type", "button");
        attr(&b, "aria-label", label);
        set_title(&b, label);
        b
    }

    pub fn icon(&self, id: &str) -> Element {
        icon_el(&self.doc, id)
    }
}

/// `span.vela-icon` holding the registry's markup for `id` (an empty span when unknown).
pub fn icon_el(doc: &Document, id: &str) -> Element {
    let span = make(doc, "span", "vela-icon");
    attr(&span, "aria-hidden", "true");
    let svg = icons::icon(id);
    if !svg.is_empty() {
        span.set_inner_html(svg);
    }
    span
}

/// `span.vela-icon` holding raw markup (a drawing type's glyph).
pub fn markup_icon(doc: &Document, markup: &str) -> Element {
    let span = make(doc, "span", "vela-icon");
    attr(&span, "aria-hidden", "true");
    span.set_inner_html(markup);
    span
}

pub fn set_title(el: &Element, title: &str) {
    let _ = Reflect::set(el, &JsValue::from_str("title"), &JsValue::from_str(title));
}

pub fn set_hidden(el: &Element, hidden: bool) {
    let _ = Reflect::set(
        el,
        &JsValue::from_str("hidden"),
        &JsValue::from_bool(hidden),
    );
}

pub fn is_hidden(el: &Element) -> bool {
    Reflect::get(el, &JsValue::from_str("hidden"))
        .ok()
        .is_some_and(|v| v.is_truthy())
}

pub fn set_value(el: &Element, value: &str) {
    let _ = Reflect::set(el, &JsValue::from_str("value"), &JsValue::from_str(value));
}

pub fn get_value(el: &Element) -> String {
    Reflect::get(el, &JsValue::from_str("value"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

pub fn set_disabled(el: &Element, disabled: bool) {
    let _ = Reflect::set(
        el,
        &JsValue::from_str("disabled"),
        &JsValue::from_bool(disabled),
    );
}

/// `data-<name>` as an attribute (`""` clears it when `value` is `None`).
pub fn set_data(el: &Element, name: &str, value: Option<&str>) {
    let key = format!("data-{name}");
    match value {
        Some(v) => attr(el, &key, v),
        None => {
            let _ = el.remove_attribute(&key);
        }
    }
}

pub fn data(el: &Element, name: &str) -> Option<String> {
    el.get_attribute(&format!("data-{name}"))
}

/// The element children, through the live `children` collection (array-like).
pub fn kids(el: &Element) -> Vec<Element> {
    let list = Reflect::get(el, &JsValue::from_str("children")).unwrap_or(JsValue::UNDEFINED);
    if list.is_undefined() || list.is_null() {
        return Vec::new();
    }
    let len = Reflect::get(&list, &JsValue::from_str("length"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;
    (0..len)
        .filter_map(|i| Reflect::get_u32(&list, i).ok())
        .filter(|v| !v.is_undefined() && !v.is_null())
        .map(|v| v.unchecked_into::<Element>())
        .collect()
}

/// Remove every child.
pub fn clear(el: &Element) {
    for k in kids(el) {
        k.remove();
    }
    // Text nodes (a `textContent` set before) go with the content reset.
    if kids(el).is_empty() {
        el.set_text_content(None);
    }
}

/// The element's classes as one string: the `class` attribute (the same on HTML and SVG
/// elements), else a string `className` (a tree that does not mirror the attribute).
pub fn class_of(el: &Element) -> String {
    el.get_attribute("class")
        .or_else(|| {
            Reflect::get(el, &JsValue::from_str("className"))
                .ok()
                .and_then(|v| v.as_string())
        })
        .unwrap_or_default()
}

/// Replace the element's classes. `className` is a read-only `SVGAnimatedString` on SVG
/// elements (writing it throws in strict code), so only a string-valued one is written
/// through the property; the attribute is the way in for everything else.
pub fn set_class(el: &Element, class: &str) {
    let plain = Reflect::get(el, &JsValue::from_str("className"))
        .ok()
        .is_some_and(|v| v.is_string());
    if plain {
        let _ = Reflect::set(
            el,
            &JsValue::from_str("className"),
            &JsValue::from_str(class),
        );
    } else {
        let _ = el.set_attribute("class", class);
    }
}

pub fn has_class(el: &Element, class: &str) -> bool {
    class_of(el).split_whitespace().any(|c| c == class)
}

pub fn add_class(el: &Element, class: &str) {
    if !has_class(el, class) {
        let cur = class_of(el);
        set_class(
            el,
            &if cur.is_empty() {
                class.to_string()
            } else {
                format!("{cur} {class}")
            },
        );
    }
}

/// The nearest element (the element itself included) for which `pred` holds, walking up
/// through `parentNode`.
pub fn closest(el: &Element, pred: impl Fn(&Element) -> bool) -> Option<Element> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if pred(&e) {
            return Some(e);
        }
        cur = e.parent_element();
    }
    None
}

/// Every descendant matching `pred`, in document order.
pub fn find_all(el: &Element, pred: &dyn Fn(&Element) -> bool, out: &mut Vec<Element>) {
    for k in kids(el) {
        if pred(&k) {
            out.push(k.clone());
        }
        find_all(&k, pred, out);
    }
}

pub fn contains(parent: &Element, node: &JsValue) -> bool {
    crate::dom::vela_contains(parent, node)
}

/// The `target` of an event as an element: `None` for a text node, the document, the window
/// or anything else that is not an element (those have no `getAttribute`, and every helper
/// here that walks up from the target would throw on them).
pub fn event_target(ev: &JsValue) -> Option<Element> {
    let t = crate::dom::get_prop(ev, "target");
    if t.is_undefined() || t.is_null() || !t.is_object() {
        return None;
    }
    let is_element = Reflect::get(&t, &JsValue::from_str("getAttribute"))
        .ok()
        .is_some_and(|f| f.is_function());
    is_element.then(|| t.unchecked_into::<Element>())
}

pub fn ev_f64(ev: &JsValue, name: &str) -> f64 {
    crate::dom::get_f64(ev, name).unwrap_or(0.0)
}

pub fn ev_key(ev: &JsValue) -> String {
    crate::dom::get_str(ev, "key")
}

pub fn ev_flag(ev: &JsValue, name: &str) -> bool {
    crate::dom::get_bool(ev, name)
}

/// A bag of listeners that can be swapped out from inside one of its own handlers: the old
/// listeners are dropped from a microtask, never from the stack of a running closure.
#[derive(Default)]
pub struct Bag(RefCell<Vec<Listener>>);

impl Bag {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn on(&self, el: &Element, event: &'static str, f: impl FnMut(JsValue) + 'static) {
        self.0.borrow_mut().push(crate::dom::on(el, event, f));
    }

    pub fn push(&self, l: Listener) {
        self.0.borrow_mut().push(l);
    }

    /// Hand the current listeners to a microtask that frees them.
    pub fn retire(&self, cx: &Cx) {
        let old = std::mem::take(&mut *self.0.borrow_mut());
        if !old.is_empty() {
            cx.root.defer(move || drop(old));
        }
    }

    /// Free everything now (teardown: nothing of ours is running).
    pub fn clear(&self) {
        let old = std::mem::take(&mut *self.0.borrow_mut());
        drop(old);
    }
}

/// A set of disposers run once, in reverse order.
#[derive(Default)]
pub struct Disposers(RefCell<Vec<Box<dyn FnOnce()>>>);

impl Disposers {
    pub fn push(&self, f: impl FnOnce() + 'static) {
        self.0.borrow_mut().push(Box::new(f));
    }

    pub fn run(&self) {
        let list: Vec<_> = self.0.borrow_mut().drain(..).rev().collect();
        for f in list {
            f();
        }
    }
}

/// The elements inside `container` that Tab can land on, in document order.
fn tabbables(container: &Element) -> Vec<Element> {
    let mut all = Vec::new();
    find_all(
        container,
        &|e| {
            let tag = e.tag_name().to_ascii_lowercase();
            let natively = matches!(
                tag.as_str(),
                "button" | "input" | "select" | "textarea" | "a"
            );
            let index = e
                .get_attribute("tabindex")
                .and_then(|t| t.trim().parse::<i32>().ok());
            ((natively && index.is_none_or(|i| i >= 0)) || index.is_some_and(|i| i >= 0))
                && !is_hidden(e)
                && !crate::dom::get_bool(e, "disabled")
        },
        &mut all,
    );
    all
}

/// Keep Tab (and Shift+Tab) inside a modal `container`: from its last tabbable the next
/// stop is its first, and the other way round. The container itself (focusable with
/// `tabindex=-1`) counts as outside the ring, so the first press from it lands on the
/// first tabbable. Call from the container's `keydown` handler.
pub fn trap_tab(ev: &JsValue, container: &Element, doc: &Document) {
    if ev_key(ev) != "Tab" {
        return;
    }
    let list = tabbables(container);
    crate::dom::prevent_default(ev);
    let Some(first) = list.first() else {
        focus(container);
        return;
    };
    let last = list.last().unwrap_or(first);
    let shift = ev_flag(ev, "shiftKey");
    let at = doc
        .active_element()
        .and_then(|a| list.iter().position(|e| *e == a));
    let next = match (at, shift) {
        (None, false) => first,
        (None, true) => last,
        (Some(i), false) => list.get(i + 1).unwrap_or(first),
        (Some(0), true) => last,
        (Some(i), true) => &list[i - 1],
    };
    focus(next);
}

/// The element as an `HtmlElement` (for `focus`, `offsetWidth`).
pub fn html(el: &Element) -> &HtmlElement {
    el.unchecked_ref::<HtmlElement>()
}

pub fn focus(el: &Element) {
    crate::dom::vela_focus(el);
}
