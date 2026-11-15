//! Small DOM helpers shared by the host and the input wiring.
//!
//! Browser globals (`requestAnimationFrame`, `devicePixelRatio`, `matchMedia`,
//! `ResizeObserver`, `performance`) are reached through `globalThis` in one
//! JavaScript snippet instead of `web_sys::window()`: that works in a window,
//! in a worker and in Node with a stand-in DOM alike, where `web_sys::window()`
//! would insist on a real `Window`.
//!
//! Elements are cast with `unchecked_into` for the same reason: `dyn_into`
//! runs an `instanceof` check against the real DOM classes.

use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, EventTarget, HtmlElement};

#[wasm_bindgen(inline_js = r#"
export function vela_now() {
    const p = globalThis.performance;
    return p && typeof p.now === 'function' ? p.now() : Date.now();
}

export function vela_dpr() {
    const d = globalThis.devicePixelRatio;
    return typeof d === 'number' && d > 0 ? d : 1;
}

export function vela_raf(cb) {
    const f = globalThis.requestAnimationFrame;
    if (typeof f === 'function') return f.call(globalThis, cb);
    return setTimeout(() => cb(vela_now()), 16);
}

export function vela_caf(id) {
    const f = globalThis.cancelAnimationFrame;
    if (typeof f === 'function') f.call(globalThis, id);
    else clearTimeout(id);
}

export function vela_document() {
    return globalThis.document;
}

// Observe the wrapper, and the plot's device-pixel box where the browser has one.
// `onSize(hasDevice, width, height)` runs for every batch of entries.
export function vela_observe_size(wrapper, plot, onSize) {
    const RO = globalThis.ResizeObserver;
    if (typeof RO !== 'function') return null;
    const ro = new RO((entries) => {
        let has = false;
        let w = 0;
        let h = 0;
        for (const e of entries) {
            if (e.target !== plot) continue;
            const s = e.devicePixelContentBoxSize && e.devicePixelContentBoxSize[0];
            if (s) {
                has = true;
                w = s.inlineSize;
                h = s.blockSize;
            }
        }
        onSize(has, w, h);
    });
    ro.observe(wrapper);
    try {
        ro.observe(plot, { box: 'device-pixel-content-box' });
    } catch (_) {
        // Box type unsupported (WebKit): the client rect is rounded instead.
    }
    return ro;
}

export function vela_disconnect(observer) {
    if (observer) observer.disconnect();
}

// `(resolution: Xdppx)` tracks one dpr boundary only, so it is re-armed for the new dpr
// every time it fires. Returns the function that stops watching.
export function vela_watch_dpr(callback) {
    if (typeof globalThis.matchMedia !== 'function') return () => {};
    let query = null;
    let stopped = false;
    const arm = () => {
        if (stopped) return;
        query = globalThis.matchMedia('(resolution: ' + globalThis.devicePixelRatio + 'dppx)');
        query.addEventListener('change', onChange, { once: true });
    };
    const onChange = () => {
        if (stopped) return;
        arm();
        callback();
    };
    arm();
    return () => {
        stopped = true;
        if (query) query.removeEventListener('change', onChange);
    };
}

export function vela_listen(target, type, handler, capture, passive) {
    target.addEventListener(type, handler, { capture, passive });
}

export function vela_unlisten(target, type, handler, capture) {
    target.removeEventListener(type, handler, capture);
}

export function vela_capture_pointer(el, id) {
    try {
        el.setPointerCapture(id);
    } catch (_) {
        // A synthetic or already released pointer cannot be captured; the gesture goes on uncaptured.
    }
}

export function vela_release_pointer(el, id) {
    try {
        el.releasePointerCapture(id);
    } catch (_) {
        // Never captured, or the pointer is gone.
    }
}

export function vela_get_value(el) {
    const v = el.value;
    return v == null ? '' : String(v);
}

export function vela_set_value(el, value) {
    el.value = value;
}

// The caret at the end of the text, nothing selected.
export function vela_caret_end(el) {
    const n = vela_get_value(el).length;
    try {
        el.setSelectionRange(n, n);
    } catch (_) {
        // Not a text field.
    }
}

// A line break at the selection (replacing it), caret after it.
export function vela_insert_newline(el) {
    const text = vela_get_value(el);
    const start = typeof el.selectionStart === 'number' ? el.selectionStart : text.length;
    const end = typeof el.selectionEnd === 'number' ? el.selectionEnd : start;
    try {
        el.setRangeText('\n', start, end, 'end');
    } catch (_) {
        el.value = text.slice(0, start) + '\n' + text.slice(end);
    }
}

export function vela_focus(el) {
    try {
        el.focus({ preventScroll: true });
    } catch (_) {
        // Detached or not focusable.
    }
}

export function vela_contains(parent, node) {
    if (!(parent && node && typeof parent.contains === 'function')) return false;
    try {
        return !!parent.contains(node);
    } catch (_) {
        // `contains` throws for a target that is not a Node (the window)
        return false;
    }
}
"#)]
extern "C" {
    pub fn vela_now() -> f64;
    pub fn vela_dpr() -> f64;
    pub fn vela_raf(callback: &Function) -> JsValue;
    pub fn vela_caf(id: &JsValue);
    pub fn vela_document() -> JsValue;
    pub fn vela_observe_size(wrapper: &Element, plot: &Element, on_size: &Function) -> JsValue;
    pub fn vela_disconnect(observer: &JsValue);
    pub fn vela_watch_dpr(callback: &Function) -> Function;
    pub fn vela_listen(
        target: &EventTarget,
        event_type: &str,
        handler: &Function,
        capture: bool,
        passive: bool,
    );
    pub fn vela_unlisten(target: &EventTarget, event_type: &str, handler: &Function, capture: bool);
    pub fn vela_capture_pointer(element: &Element, id: i32);
    pub fn vela_release_pointer(element: &Element, id: i32);
    pub fn vela_get_value(element: &Element) -> String;
    pub fn vela_set_value(element: &Element, value: &str);
    pub fn vela_caret_end(element: &Element);
    pub fn vela_insert_newline(element: &Element);
    pub fn vela_focus(element: &Element);
    pub fn vela_contains(parent: &Element, node: &JsValue) -> bool;
}

/// The page's `document`, `None` where there is none (a worker).
pub fn global_document() -> Option<Document> {
    let doc = vela_document();
    if doc.is_undefined() || doc.is_null() {
        None
    } else {
        Some(doc.unchecked_into())
    }
}

/// The `defaultView` of a document, as an event target.
pub fn default_view(doc: &Document) -> Option<EventTarget> {
    let view = Reflect::get(doc, &JsValue::from_str("defaultView")).ok()?;
    if view.is_undefined() || view.is_null() {
        None
    } else {
        Some(view.unchecked_into())
    }
}

/// An element of `tag`, cast without an `instanceof` check.
pub fn create_element(doc: &Document, tag: &str) -> Element {
    doc.create_element(tag)
        .expect("createElement does not throw for a valid tag name")
}

/// The element's inline style, written through `setProperty` (kebab-case names).
pub fn set_styles(el: &Element, styles: &[(&str, &str)]) {
    let style = el.unchecked_ref::<HtmlElement>().style();
    for (name, value) in styles {
        let _ = style.set_property(name, value);
    }
}

pub fn set_style(el: &Element, name: &str, value: &str) {
    set_styles(el, &[(name, value)]);
}

pub fn remove_style(el: &Element, name: &str) {
    let _ = el
        .unchecked_ref::<HtmlElement>()
        .style()
        .remove_property(name);
}

/// A DOM event listener that removes itself when dropped.
pub struct Listener {
    target: EventTarget,
    event_type: &'static str,
    capture: bool,
    handler: Closure<dyn FnMut(JsValue)>,
}

impl Listener {
    /// Register `f` for `event_type` on `target`. The event arrives as a plain
    /// `JsValue`; cast it with `unchecked_ref` to the type the event is.
    pub fn new(
        target: &EventTarget,
        event_type: &'static str,
        capture: bool,
        passive: bool,
        f: impl FnMut(JsValue) + 'static,
    ) -> Self {
        let handler = Closure::<dyn FnMut(JsValue)>::new(f);
        vela_listen(
            target,
            event_type,
            handler.as_ref().unchecked_ref(),
            capture,
            passive,
        );
        Self {
            target: target.clone(),
            event_type,
            capture,
            handler,
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        vela_unlisten(
            &self.target,
            self.event_type,
            self.handler.as_ref().unchecked_ref(),
            self.capture,
        );
    }
}

/// A new element of `tag` with a class name (empty for none).
pub fn make(doc: &Document, tag: &str, class: &str) -> Element {
    let el = create_element(doc, tag);
    if !class.is_empty() {
        el.set_class_name(class);
    }
    el
}

/// Set one attribute (the names used here are always valid).
pub fn attr(el: &Element, name: &str, value: &str) {
    let _ = el.set_attribute(name, value);
}

/// Listen for `event_type` on `target` (bubble phase, not passive, so the handler can
/// `preventDefault`).
pub fn on(
    target: &Element,
    event_type: &'static str,
    f: impl FnMut(JsValue) + 'static,
) -> Listener {
    Listener::new(target.as_ref(), event_type, false, false, f)
}

/// A string property of a JavaScript object (empty when missing).
pub fn get_str(object: &JsValue, name: &str) -> String {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

/// An event target as an element: `None` for a text node, the document, the window or
/// nothing (their `tagName` / `getAttribute` are absent and a typed accessor would throw).
pub fn target_element(target: &JsValue) -> Option<Element> {
    let has_method = |name: &str| {
        Reflect::get(target, &JsValue::from_str(name))
            .ok()
            .is_some_and(|v| v.is_function())
    };
    (target.is_object() && has_method("getAttribute") && has_method("hasAttribute"))
        .then(|| target.clone().unchecked_into())
}

/// A truthy property of a JavaScript object.
pub fn get_bool(object: &JsValue, name: &str) -> bool {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()
        .is_some_and(|v| v.is_truthy())
}

/// An object-valued property (`undefined` / `null` when missing).
pub fn get_prop(object: &JsValue, name: &str) -> JsValue {
    Reflect::get(object, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

/// A numeric property of a JavaScript object (`None` when missing or not a number).
pub fn get_f64(object: &JsValue, name: &str) -> Option<f64> {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()
        .and_then(|v| v.as_f64())
}

/// `event.stopPropagation()`.
pub fn stop_propagation(event: &JsValue) {
    event.unchecked_ref::<web_sys::Event>().stop_propagation();
}

/// `event.preventDefault()`.
pub fn prevent_default(event: &JsValue) {
    event.unchecked_ref::<web_sys::Event>().prevent_default();
}
