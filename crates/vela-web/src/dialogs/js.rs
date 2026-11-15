//! Property reads and writes the dialogs need on form controls and events. They go through
//! small JavaScript functions instead of `web-sys` getters: a getter casts a missing
//! property to its Rust type (`undefined` reads as `true` for a `bool`), and the page may be
//! a stand-in DOM in a test.

use wasm_bindgen::prelude::*;
use web_sys::Element;

#[wasm_bindgen(inline_js = r#"
export function dlg_checked(el) { return !!el.checked; }
export function dlg_set_checked(el, v) { el.checked = !!v; }
export function dlg_click(el) { if (el && typeof el.click === 'function') el.click(); }
export function dlg_value(el) { const v = el.value; return v == null ? '' : String(v); }
export function dlg_set_value(el, v) { el.value = v; }
export function dlg_set_flag(el, name, v) { el[name] = !!v; }
export function dlg_target(ev) { return ev ? ev.target : null; }
export function dlg_key(ev) { return ev && typeof ev.key === 'string' ? ev.key : ''; }
export function dlg_flag(ev, name) { return !!(ev && ev[name]); }
export function dlg_num(ev, name) { const v = ev ? ev[name] : 0; return typeof v === 'number' ? v : 0; }
export function dlg_active(doc) { return (doc && doc.activeElement) || null; }
export function dlg_attached(el) {
    const d = el.ownerDocument;
    for (let n = el; n; n = n.parentNode) if (n === d || (d && n === d.documentElement)) return true;
    return false;
}
export function dlg_scroll_into_view(el) {
    try { if (el && typeof el.scrollIntoView === 'function') el.scrollIntoView({ block: 'nearest' }); } catch (_) {}
}
export function dlg_date_ms(y, mo, d, h, mi) { return new Date(y, mo - 1, d, h, mi, 0).getTime(); }
export function dlg_date_parts(ms) {
    const d = new Date(ms);
    if (Number.isNaN(d.getTime())) return [];
    return [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes()];
}
export function dlg_dispatch(el, name, json) {
    try {
        if (typeof el.dispatchEvent !== 'function' || typeof CustomEvent !== 'function') return;
        el.dispatchEvent(new CustomEvent(name, { bubbles: true, detail: JSON.parse(json) }));
    } catch (_) {}
}
export function dlg_window_height(doc) {
    const v = doc && doc.defaultView;
    return v && typeof v.innerHeight === 'number' ? v.innerHeight : 0;
}
"#)]
extern "C" {
    pub fn dlg_checked(el: &Element) -> bool;
    pub fn dlg_set_checked(el: &Element, v: bool);
    pub fn dlg_click(el: &Element);
    pub fn dlg_value(el: &Element) -> String;
    pub fn dlg_set_value(el: &Element, v: &str);
    pub fn dlg_set_flag(el: &Element, name: &str, v: bool);
    pub fn dlg_target(ev: &JsValue) -> JsValue;
    pub fn dlg_key(ev: &JsValue) -> String;
    pub fn dlg_flag(ev: &JsValue, name: &str) -> bool;
    pub fn dlg_num(ev: &JsValue, name: &str) -> f64;
    pub fn dlg_active(doc: &JsValue) -> JsValue;
    pub fn dlg_attached(el: &Element) -> bool;
    pub fn dlg_scroll_into_view(el: &Element);
    pub fn dlg_date_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> f64;
    pub fn dlg_date_parts(ms: f64) -> js_sys::Array;
    pub fn dlg_dispatch(el: &Element, name: &str, json: &str);
}

/// Whether an event target is this element.
pub fn is_el(target: &JsValue, el: &Element) -> bool {
    *target == JsValue::from(el.clone())
}
