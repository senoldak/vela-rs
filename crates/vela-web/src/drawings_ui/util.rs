//! Small DOM helpers the popup and the panel share. They stay on what the stand-in DOM of
//! the smoke tests implements: attributes and class names rather than `classList`,
//! `dataset` or `closest`, and property reads through `Reflect`.

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use crate::dom::{attr, set_style};

/// Remove every child of `el`.
pub fn clear(el: &Element) {
    while let Some(child) = el.first_child() {
        child.unchecked_ref::<Element>().remove();
    }
}

/// An element's box relative to `origin`'s top-left: `(x, y, w, h)`.
pub fn rect_in(el: &Element, origin: &Element) -> (f64, f64, f64, f64) {
    let r = el.get_bounding_client_rect();
    let o = origin.get_bounding_client_rect();
    (
        r.left() - o.left(),
        r.top() - o.top(),
        r.width(),
        r.height(),
    )
}

/// `el.scrollTop`, 0 when the element has none.
pub fn scroll_top(el: &Element) -> f64 {
    Reflect::get(el, &JsValue::from_str("scrollTop"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

pub fn set_scroll_top(el: &Element, top: f64) {
    let _ = Reflect::set(el, &JsValue::from_str("scrollTop"), &JsValue::from_f64(top));
}

/// Show or hide an element with the `hidden` attribute (the stylesheet makes it win).
pub fn set_hidden(el: &Element, hidden: bool) {
    if hidden {
        attr(el, "hidden", "");
    } else {
        let _ = el.remove_attribute("hidden");
    }
}

/// Set or clear a presence attribute (`data-open`, `data-off`).
pub fn set_flag(el: &Element, name: &str, on: bool) {
    if on {
        attr(el, name, "");
    } else {
        let _ = el.remove_attribute(name);
    }
}

/// `aria-pressed="true|false"`.
pub fn set_pressed(el: &Element, on: bool) {
    attr(el, "aria-pressed", if on { "true" } else { "false" });
}

/// The nearest element from `target` up to (not past) `stop` that carries `name`, with its
/// value.
pub fn ancestor_with_attr(
    target: &JsValue,
    stop: &Element,
    name: &str,
) -> Option<(Element, String)> {
    let mut cur = crate::dom::target_element(target);
    while let Some(el) = cur {
        if let Some(v) = el.get_attribute(name) {
            return Some((el, v));
        }
        if JsValue::from(el.clone()) == JsValue::from(stop.clone()) {
            return None;
        }
        cur = el.parent_element();
    }
    None
}

/// The tag name of an event target, lower-cased (empty when it has none).
pub fn tag_of(target: &JsValue) -> String {
    Reflect::get(target, &JsValue::from_str("tagName"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
        .to_lowercase()
}

/// Place `el` at `(left, top)` px.
pub fn place(el: &Element, left: f64, top: f64) {
    set_style(el, "left", &format!("{}px", left.round()));
    set_style(el, "top", &format!("{}px", top.round()));
}

/// `parseFloat(el.style.left) || 0` and the same for top.
pub fn placed(el: &Element) -> (f64, f64) {
    let style = el.unchecked_ref::<web_sys::HtmlElement>().style();
    let num = |name: &str| {
        style
            .get_property_value(name)
            .ok()
            .and_then(|v| v.trim_end_matches("px").trim().parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    (num("left"), num("top"))
}

/// Fit a textarea to its content, up to `max_lines` lines (the kit text area's auto-grow:
/// 18px lines and 12px of padding). An element with no layout (`scrollHeight` unknown)
/// keeps the height its rows give it.
pub fn auto_grow(el: &Element, max_lines: u32) {
    set_style(el, "height", "auto");
    let Some(content) = Reflect::get(el, &JsValue::from_str("scrollHeight"))
        .ok()
        .and_then(|v| v.as_f64())
        .filter(|h| *h > 0.0)
    else {
        return;
    };
    let max = f64::from(max_lines) * 18.0 + 12.0;
    set_style(el, "height", &format!("{}px", content.min(max)));
    set_style(
        el,
        "overflow-y",
        if content > max { "auto" } else { "hidden" },
    );
}
