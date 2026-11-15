//! Property reads and writes the top shell needs on elements and events. They go through
//! small JavaScript functions instead of `web-sys` getters: a getter casts a missing
//! property to its Rust type, and the page may be a stand-in DOM in a test.

use wasm_bindgen::prelude::*;
use web_sys::Element;

#[wasm_bindgen(inline_js = r#"
export function vst_value(el) { const v = el.value; return v == null ? '' : String(v); }
export function vst_set_value(el, v) { el.value = v; }
// The caret/selection of a text field as [start, end] (null when it has none).
export function vst_selection(el) {
    try {
        if (typeof el.selectionStart === 'number' && typeof el.selectionEnd === 'number') return [el.selectionStart, el.selectionEnd];
    } catch (_) {}
    return null;
}
export function vst_set_selection(el, start, end) {
    try { if (typeof el.setSelectionRange === 'function') el.setSelectionRange(start, end); } catch (_) {}
}
export function vst_set_flag(el, name, v) { el[name] = !!v; }
export function vst_flag(el, name) { return !!el[name]; }
export function vst_set_str(el, name, v) { el[name] = v; }
export function vst_key(ev) { return ev && typeof ev.key === 'string' ? ev.key : ''; }
export function vst_ev_flag(ev, name) { return !!(ev && ev[name]); }
export function vst_ev_num(ev, name) { const v = ev ? ev[name] : 0; return typeof v === 'number' ? v : 0; }
export function vst_target(ev) { return ev ? ev.target : null; }
export function vst_active(doc) { return (doc && doc.activeElement) || null; }
export function vst_contains(parent, node) {
    // `contains` throws for a target that is no Node (the window)
    try { return !!(parent && node && typeof parent.contains === 'function' && parent.contains(node)); } catch (_) { return false; }
}
export function vst_attached(el) {
    const d = el.ownerDocument;
    for (let n = el; n; n = n.parentNode) if (n === d || (d && n === d.documentElement)) return true;
    return false;
}
// The nearest ancestor (or the element itself) wearing a class; null when none.
export function vst_closest_class(el, cls) {
    for (let n = el; n; n = n.parentNode) {
        const c = typeof n.className === 'string' ? n.className : '';
        if (c && c.split(/\s+/).includes(cls)) return n;
    }
    return null;
}
export function vst_scroll_into_view(el) {
    try { if (el && typeof el.scrollIntoView === 'function') el.scrollIntoView({ block: 'nearest' }); } catch (_) {}
}
export function vst_scroll_top(el) { return typeof el.scrollTop === 'number' ? el.scrollTop : 0; }
export function vst_set_scroll_top(el, v) { try { el.scrollTop = v; } catch (_) {} }
export function vst_client_height(el) { return typeof el.clientHeight === 'number' ? el.clientHeight : 0; }
export function vst_scroll_height(el) { return typeof el.scrollHeight === 'number' ? el.scrollHeight : 0; }
export function vst_client_width(el) { return typeof el.clientWidth === 'number' ? el.clientWidth : 0; }
export function vst_scroll_width(el) { return typeof el.scrollWidth === 'number' ? el.scrollWidth : 0; }
export function vst_offset_width(el) { return typeof el.offsetWidth === 'number' ? el.offsetWidth : 0; }
export function vst_var_px(el, name) {
    try {
        const view = el.ownerDocument && el.ownerDocument.defaultView;
        if (!view || typeof view.getComputedStyle !== 'function') return 0;
        const v = Number.parseFloat(view.getComputedStyle(el).getPropertyValue(name));
        return Number.isFinite(v) ? v : 0;
    } catch (_) { return 0; }
}
export function vst_dpr(doc) {
    const v = doc && doc.defaultView;
    return v && typeof v.devicePixelRatio === 'number' && v.devicePixelRatio > 0 ? v.devicePixelRatio : 1;
}
export function vst_set_html(el, html) { el.innerHTML = html; }
export function vst_prevent(ev) { if (ev && typeof ev.preventDefault === 'function') ev.preventDefault(); }
export function vst_stop(ev) { if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation(); }
export function vst_capture(el, id) {
    try { if (typeof el.setPointerCapture === 'function') el.setPointerCapture(id); } catch (_) {}
}
// A download of `href` as `name`: a temporary anchor, clicked.
export function vst_download(doc, href, name) {
    try {
        const a = doc.createElement('a');
        a.href = href;
        a.download = name;
        a.style.display = 'none';
        doc.documentElement.appendChild(a);
        if (typeof a.click === 'function') a.click();
        if (typeof a.remove === 'function') a.remove();
    } catch (_) {}
}
// Re-run `cb` when web fonts finish loading (widths change without a resize).
export function vst_fonts_listen(doc, cb) {
    const f = doc && doc.fonts;
    if (!f || typeof f.addEventListener !== 'function') return null;
    f.addEventListener('loadingdone', cb);
    return () => f.removeEventListener('loadingdone', cb);
}
export function vst_blur(el) { try { if (typeof el.blur === 'function') el.blur(); } catch (_) {} }
export function vst_text_width(el) {
    if (!el || typeof el.getBoundingClientRect !== 'function') return 0;
    return el.getBoundingClientRect().width || 0;
}
"#)]
extern "C" {
    pub fn vst_value(el: &Element) -> String;
    pub fn vst_set_value(el: &Element, v: &str);
    pub fn vst_selection(el: &Element) -> JsValue;
    pub fn vst_set_selection(el: &Element, start: f64, end: f64);
    pub fn vst_set_flag(el: &Element, name: &str, v: bool);
    pub fn vst_flag(el: &Element, name: &str) -> bool;
    pub fn vst_set_str(el: &Element, name: &str, v: &str);
    pub fn vst_key(ev: &JsValue) -> String;
    pub fn vst_ev_flag(ev: &JsValue, name: &str) -> bool;
    pub fn vst_ev_num(ev: &JsValue, name: &str) -> f64;
    pub fn vst_target(ev: &JsValue) -> JsValue;
    pub fn vst_active(doc: &JsValue) -> JsValue;
    pub fn vst_contains(parent: &Element, node: &JsValue) -> bool;
    pub fn vst_attached(el: &Element) -> bool;
    pub fn vst_closest_class(el: &JsValue, cls: &str) -> JsValue;
    pub fn vst_scroll_into_view(el: &Element);
    pub fn vst_scroll_top(el: &Element) -> f64;
    pub fn vst_set_scroll_top(el: &Element, v: f64);
    pub fn vst_client_height(el: &Element) -> f64;
    pub fn vst_scroll_height(el: &Element) -> f64;
    pub fn vst_client_width(el: &Element) -> f64;
    pub fn vst_scroll_width(el: &Element) -> f64;
    pub fn vst_offset_width(el: &Element) -> f64;
    pub fn vst_var_px(el: &Element, name: &str) -> f64;
    pub fn vst_dpr(doc: &JsValue) -> f64;
    pub fn vst_set_html(el: &Element, html: &str);
    pub fn vst_prevent(ev: &JsValue);
    pub fn vst_stop(ev: &JsValue);
    pub fn vst_capture(el: &Element, id: i32);
    pub fn vst_download(doc: &JsValue, href: &str, name: &str);
    pub fn vst_fonts_listen(doc: &JsValue, cb: &js_sys::Function) -> JsValue;
    pub fn vst_blur(el: &Element);
    pub fn vst_text_width(el: &Element) -> f64;
}

/// Whether an event target is this element.
pub fn is_el(target: &JsValue, el: &Element) -> bool {
    *target == JsValue::from(el.clone())
}

/// Whether `el` is `node` or holds it.
pub fn holds(el: &Element, node: &JsValue) -> bool {
    !node.is_null() && !node.is_undefined() && vst_contains(el, node)
}

/// The selection of a text field, `None` when it has none.
pub fn selection_of(el: &Element) -> Option<(f64, f64)> {
    let v = vst_selection(el);
    let arr: js_sys::Array = v.dyn_into().ok()?;
    Some((arr.get(0).as_f64()?, arr.get(1).as_f64()?))
}
