//! The few browser globals the workspace host reaches, in one JavaScript snippet (through
//! `globalThis`, like [`crate::dom`]: they work in a window and in Node with a stand-in DOM).

use js_sys::Function;
use wasm_bindgen::prelude::*;
use web_sys::Element;

#[wasm_bindgen(inline_js = r#"
export function vela_ws_storage_get(key) {
    try {
        const v = globalThis.localStorage.getItem(key);
        return v == null ? undefined : v;
    } catch (_) {
        return undefined; // blocked or absent storage reads as an empty key
    }
}

export function vela_ws_storage_set(key, value) {
    try {
        globalThis.localStorage.setItem(key, value);
    } catch (_) {
        // quota or privacy mode: best effort
    }
}

export function vela_ws_storage_remove(key) {
    try {
        globalThis.localStorage.removeItem(key);
    } catch (_) {
        // best effort
    }
}

// A ResizeObserver on one element; `cb()` runs for every batch. Null where there is none.
export function vela_ws_observe(el, cb) {
    const RO = globalThis.ResizeObserver;
    if (typeof RO !== 'function') return null;
    const ro = new RO(() => cb());
    ro.observe(el);
    return ro;
}

export function vela_ws_disconnect(observer) {
    if (observer) observer.disconnect();
}

export function vela_ws_coarse_now() {
    try {
        return typeof globalThis.matchMedia === 'function' && !!globalThis.matchMedia('(pointer: coarse)').matches;
    } catch (_) {
        return false;
    }
}

// Watches `(pointer: coarse)`; returns the function that stops watching.
export function vela_ws_watch_coarse(cb) {
    if (typeof globalThis.matchMedia !== 'function') return () => {};
    let query;
    try {
        query = globalThis.matchMedia('(pointer: coarse)');
    } catch (_) {
        return () => {};
    }
    if (!query || typeof query.addEventListener !== 'function') return () => {};
    const handler = () => cb();
    query.addEventListener('change', handler);
    return () => {
        if (typeof query.removeEventListener === 'function') query.removeEventListener('change', handler);
    };
}

export function vela_ws_computed_background(el) {
    try {
        const f = globalThis.getComputedStyle;
        return typeof f === 'function' ? f(el).backgroundColor || '' : '';
    } catch (_) {
        return '';
    }
}

export function vela_ws_to_png(canvas) {
    try {
        const url = canvas.toDataURL('image/png');
        return url || undefined;
    } catch (_) {
        return undefined; // a tainted canvas
    }
}

export function vela_ws_download(doc, url, filename) {
    const a = doc.createElement('a');
    a.href = url;
    a.download = filename;
    if (typeof a.click === 'function') a.click();
}

export function vela_ws_visibility(doc) {
    return doc && typeof doc.visibilityState === 'string' ? doc.visibilityState : 'visible';
}

// The context object contributed callbacks receive. `data` carries the live reads taken when
// the context is built; every method goes through the root's one `dispatch(op, a, b)`.
export function vela_ctx_build(dispatch, data, host) {
    const ctx = Object.assign({}, data);
    ctx.host = host;
    const call = (op, a, b) => dispatch(op, a, b);
    ctx.setSymbol = (s) => { call('setSymbol', s); };
    ctx.setTimeframe = (tf) => { call('setTimeframe', tf); };
    ctx.setPriceStyle = (s) => { call('setPriceStyle', s); };
    ctx.openSymbolSearch = (q) => { call('openSymbolSearch', q); };
    ctx.togglePanel = (id, open) => { call('togglePanel', id, open); };
    ctx.toast = (message, kind) => { call('toast', message, kind); };
    ctx.stateChanged = () => { call('stateChanged'); };
    ctx.addNativeIndicator = (type) => { call('addNativeIndicator', type); };
    ctx.dockStrip = (el, opts) => call('dockStrip', el, opts);
    ctx.dockEdge = (el) => call('dockEdge', el);
    ctx.topbar = {
        isVisible: (id) => call('topbar.isVisible', id),
        setVisible: (id, visible) => { call('topbar.setVisible', id, visible); },
    };
    ctx.sidebar = {
        // A getter, as in the original (`readonly enabled: boolean`).
        get enabled() { return call('sidebar.enabled'); },
        setBadge: (id, badge) => { call('sidebar.setBadge', id, badge); },
        refresh: () => { call('sidebar.refresh'); },
        isVisible: (id) => call('sidebar.isVisible', id),
        setVisible: (id, visible) => { call('sidebar.setVisible', id, visible); },
    };
    ctx.panels = {
        list: () => call('panels.list'),
        openId: () => call('panels.openId'),
        isMaximized: (id) => call('panels.isMaximized', id),
        onDidChange: (cb) => call('panels.onDidChange', cb),
    };
    return ctx;
}

export function vela_ws_platform_is_mac() {
    try {
        const p = globalThis.navigator && globalThis.navigator.platform;
        return typeof p === 'string' && /mac|iphone|ipad/i.test(p);
    } catch (_) {
        return false;
    }
}
"#)]
extern "C" {
    pub fn vela_ws_storage_get(key: &str) -> Option<String>;
    pub fn vela_ws_storage_set(key: &str, value: &str);
    pub fn vela_ws_storage_remove(key: &str);
    pub fn vela_ws_observe(el: &Element, callback: &Function) -> JsValue;
    pub fn vela_ws_disconnect(observer: &JsValue);
    pub fn vela_ws_coarse_now() -> bool;
    pub fn vela_ws_watch_coarse(callback: &Function) -> Function;
    pub fn vela_ws_computed_background(el: &Element) -> String;
    pub fn vela_ws_to_png(canvas: &JsValue) -> Option<String>;
    pub fn vela_ws_download(doc: &JsValue, url: &str, filename: &str);
    pub fn vela_ws_visibility(doc: &JsValue) -> String;
    pub fn vela_ws_platform_is_mac() -> bool;
    /// Build the context object of a contributed callback (see the snippet).
    pub fn vela_ctx_build(dispatch: &Function, data: &JsValue, host: &JsValue) -> JsValue;
}
