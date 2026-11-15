//! `registerTheme`: the original's module-global theme registry (`core/theme.ts`), shared by
//! every chart, app, workspace and widget on the page.
//!
//! ```js
//! const off = registerTheme({ id: 'midnight', label: 'Midnight', theme: { background: ..., } });
//! chart.setTheme('midnight');      // VelaChart, VelaApp, VelaWorkspace, VelaWidget
//! registeredThemes();              // [{ id, label, theme }]
//! off();                           // withdraw it (unless a later registration replaced it)
//! ```
//!
//! `theme` is a chart theme (`background`, `textColor`, `gridColor`, `borderColor`, `upColor`,
//! `downColor`, `fontFamily`, optional `ui`) or a `createTheme` result (it carries `tokens`).
//! The registry is one `Rc<RefCell<ThemeRegistry>>` per thread: [`attach`] makes an engine
//! environment use it, so a registration reaches widgets that are already running.
//! Rust-side readers (the chart settings dialog's Theme row) use [`registered`] and
//! [`resolve_named`].

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use vela_core::options::ThemeSpec;
use vela_core::ports::chart_renderer::VelaTheme;
use vela_core::theme::{RegisteredTheme, ThemeRegistry, dark_theme, light_theme};
use vela_core::vela::VelaEnv;
use wasm_bindgen::prelude::*;

thread_local! {
    static SHARED: Rc<RefCell<ThemeRegistry>> = Rc::new(RefCell::new(ThemeRegistry::new()));
}

/// The page's registry.
pub fn shared() -> Rc<RefCell<ThemeRegistry>> {
    SHARED.with(Clone::clone)
}

/// Make `env` resolve theme ids through the page's registry.
pub fn attach(env: &mut VelaEnv) {
    env.registries.themes = shared();
}

/// The themes added with `registerTheme`, in registration order.
pub fn registered() -> Vec<RegisteredTheme> {
    shared().borrow().registered()
}

/// `dark`, `light` or the id of a registered theme, resolved; anything else is an error
/// (not a silent default).
pub fn resolve_named(name: &str) -> Result<VelaTheme, String> {
    match name {
        "dark" => Ok(dark_theme()),
        "light" => Ok(light_theme()),
        other => {
            let reg = shared();
            let reg = reg.borrow();
            if reg.registered().iter().any(|t| t.id == other) {
                Ok(reg.resolve(Some(&ThemeSpec::Id(other.to_string()))))
            } else {
                Err(format!(
                    "vela: unknown theme \"{other}\" (use \"dark\", \"light\" or a registered theme)"
                ))
            }
        }
    }
}

fn js_value(v: &JsValue) -> Result<serde_json::Value, JsValue> {
    let text = js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .ok_or_else(|| JsError::new("registerTheme: `theme` must be a plain object"))?;
    serde_json::from_str(&text)
        .map_err(|e| JsError::new(&format!("registerTheme: `theme`: {e}")).into())
}

/// Offer a theme by id (`{ id, label?, theme }`); registering an id again replaces it.
/// Returns the function that withdraws this registration.
#[wasm_bindgen(js_name = registerTheme)]
pub fn register_theme(entry: &JsValue) -> Result<Function, JsValue> {
    let get = |k: &str| Reflect::get(entry, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED);
    let id = get("id")
        .as_string()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| JsError::new("registerTheme expects { id: string, theme: object }"))?;
    let theme = get("theme");
    if !theme.is_object() {
        return Err(JsError::new("registerTheme expects { id: string, theme: object }").into());
    }
    let label = get("label").as_string().unwrap_or_else(|| id.clone());
    let registration = shared()
        .borrow_mut()
        .register(RegisteredTheme {
            id,
            label,
            theme: js_value(&theme)?,
        })
        .map_err(|e| JsError::new(&e.to_string()))?;
    let cell = Rc::new(RefCell::new(Some(registration)));
    Ok(Closure::<dyn FnMut()>::new(move || {
        let registration = cell.borrow_mut().take();
        if let Some(r) = registration {
            shared().borrow_mut().unregister(&r);
        }
    })
    .into_js_value()
    .unchecked_into())
}

/// The registered themes as `[{ id, label, theme }]`.
#[wasm_bindgen(js_name = registeredThemes)]
pub fn registered_themes() -> Array {
    registered()
        .iter()
        .map(|t| {
            let o = Object::new();
            let _ = Reflect::set(&o, &"id".into(), &JsValue::from_str(&t.id));
            let _ = Reflect::set(&o, &"label".into(), &JsValue::from_str(&t.label));
            let theme = js_sys::JSON::parse(&t.theme.to_string()).unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&o, &"theme".into(), &theme);
            JsValue::from(o)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_unknown_names() {
        assert!(resolve_named("dark").is_ok());
        assert!(resolve_named("light").is_ok());
        assert!(resolve_named("nope").is_err());
    }

    #[test]
    fn registered_theme_resolves_and_withdraws() {
        let reg = shared();
        let r = reg
            .borrow_mut()
            .register(RegisteredTheme {
                id: "unit".into(),
                label: "Unit".into(),
                theme: serde_json::json!({"background": "#010203"}),
            })
            .unwrap();
        assert_eq!(resolve_named("unit").unwrap()["background"], "#010203");
        assert!(reg.borrow_mut().unregister(&r));
        assert!(resolve_named("unit").is_err());
    }
}
