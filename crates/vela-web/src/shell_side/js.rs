//! The few JavaScript shims of the side surfaces (a `CustomEvent` that also works against the
//! smoke checks' fake DOM).

use wasm_bindgen::prelude::*;
use web_sys::Element;

#[wasm_bindgen(inline_js = r#"
export function vela_ss_emit(el, name, intent, text) {
    const detail = { intent, text };
    if (typeof el.dispatchEvent === 'function' && typeof CustomEvent === 'function') {
        el.dispatchEvent(new CustomEvent(name, { detail, bubbles: true }));
    } else if (typeof el.dispatch === 'function') {
        el.dispatch(name, { detail, bubbles: true });
    }
}

export function vela_ss_now() {
    return typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now();
}

// A function that forwards to `f` until `vela_ss_kill`ed, then does nothing: what a plugin
// holding a copy of a callback sees after the Rust closure behind it was freed.
export function vela_ss_guard(f) {
    let live = f;
    const g = (...args) => (live ? live(...args) : undefined);
    g.kill = () => { live = null; };
    return g;
}

export function vela_ss_kill(g) {
    if (g && typeof g.kill === 'function') g.kill();
}

export function vela_ss_window_height(el) {
    const doc = el && el.ownerDocument;
    const view = doc && doc.defaultView;
    return view && typeof view.innerHeight === 'number' ? view.innerHeight : 0;
}
"#)]
extern "C" {
    /// Dispatch the bubbling `CustomEvent` `name` on `el` with `detail = { intent, text }`.
    pub fn vela_ss_emit(el: &Element, name: &str, intent: &str, text: &str);
    pub fn vela_ss_now() -> f64;
    pub fn vela_ss_guard(f: &js_sys::Function) -> js_sys::Function;
    pub fn vela_ss_kill(g: &js_sys::Function);
    pub fn vela_ss_window_height(el: &Element) -> f64;
}
