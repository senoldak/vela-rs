//! The built-in chart themes and how a `theme` option resolves to one.
//!
//! A chart theme is opaque JSON ([`VelaTheme`]): the chart's seven colors and
//! font, with optional `ui` settings. A `createTheme` result (an object that
//! carries `tokens`) resolves to its `chart` colors plus its `settings` as `ui`.
//!
//! The original keeps registered themes in a module-level map; here they live in
//! a [`ThemeRegistry`] the caller owns. Its `WeakMap` that makes re-resolving
//! one `createTheme` object return the same object is not ported: resolving the
//! same theme twice returns equal values.

use serde_json::{Value, json};

use crate::options::ThemeSpec;
use crate::palette::{BEARISH, BULLISH};
use crate::ports::chart_renderer::VelaTheme;

/// The reference dark palette (the design spec's first-run chart cosmetics:
/// surface, axis text, subtle grid, candle green/red).
pub fn dark_theme() -> VelaTheme {
    json!({
        "background": "#151619",
        "textColor": "#b2b5be",
        "gridColor": "#20222c",
        "borderColor": "#2a2b30",
        "upColor": BULLISH,
        "downColor": BEARISH,
        "fontFamily": "sans-serif",
    })
}

/// The built-in light chart theme. Candle hues are shared across themes:
/// switching themes recolors surfaces and text, never the series.
pub fn light_theme() -> VelaTheme {
    json!({
        "background": "#ffffff",
        "textColor": "#1e293b",
        // Soft chrome on white: grid is a neutral wash; pane separator / axis
        // border sit a step darker so stacked panes stay distinct.
        "gridColor": "#cccccc",
        "borderColor": "#d4dae3",
        "upColor": BULLISH,
        "downColor": BEARISH,
        "fontFamily": "sans-serif",
    })
}

/// A theme offered by id, next to Dark and Light, in the chart settings' theme choice.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredTheme {
    pub id: String,
    pub label: String,
    /// A chart theme, or a `createTheme` result.
    pub theme: Value,
}

/// Why a theme could not be registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeError {
    /// `dark` and `light` are built in.
    BuiltIn(String),
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::BuiltIn(id) => {
                write!(f, "registerTheme: \"{id}\" is a built-in theme")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// What [`ThemeRegistry::register`] returns: hand it to
/// [`ThemeRegistry::unregister`] to withdraw precisely that registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeRegistration {
    pub id: String,
    serial: u64,
}

/// The themes added with `registerTheme`, in registration order.
#[derive(Clone, Debug, Default)]
pub struct ThemeRegistry {
    entries: Vec<(u64, RegisteredTheme)>,
    next_serial: u64,
}

impl ThemeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a theme by id: `theme: "<id>"`, `setTheme("<id>")` and the chart
    /// settings' theme choice. Registering an id again replaces it (keeping its
    /// place in the order).
    pub fn register(&mut self, entry: RegisteredTheme) -> Result<ThemeRegistration, ThemeError> {
        if entry.id == "dark" || entry.id == "light" {
            return Err(ThemeError::BuiltIn(entry.id));
        }
        self.next_serial += 1;
        let serial = self.next_serial;
        let id = entry.id.clone();
        match self.entries.iter_mut().find(|(_, e)| e.id == id) {
            Some(slot) => *slot = (serial, entry),
            None => self.entries.push((serial, entry)),
        }
        Ok(ThemeRegistration { id, serial })
    }

    /// Withdraw a registration, unless a later one replaced it. Returns whether
    /// anything was removed.
    pub fn unregister(&mut self, registration: &ThemeRegistration) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|(serial, e)| !(e.id == registration.id && *serial == registration.serial));
        self.entries.len() != before
    }

    /// The registered themes, in registration order.
    pub fn registered(&self) -> Vec<RegisteredTheme> {
        self.entries.iter().map(|(_, e)| e.clone()).collect()
    }

    fn get(&self, id: &str) -> Option<&RegisteredTheme> {
        self.entries
            .iter()
            .find(|(_, e)| e.id == id)
            .map(|(_, e)| e)
    }

    /// The chart theme for a theme option. A `createTheme` result resolves to its
    /// chart colors with its settings; an unknown id resolves to the dark theme.
    pub fn resolve(&self, theme: Option<&ThemeSpec>) -> VelaTheme {
        match theme {
            None => dark_theme(),
            Some(ThemeSpec::Id(id)) => self.resolve_id(id),
            Some(ThemeSpec::Theme(value)) => resolve_object(value),
        }
    }

    fn resolve_id(&self, id: &str) -> VelaTheme {
        match id {
            "" | "dark" => dark_theme(),
            "light" => light_theme(),
            other => match self.get(other) {
                Some(entry) => resolve_object(&entry.theme),
                None => dark_theme(),
            },
        }
    }

    /// The id of the registered theme a chart theme is, if any.
    pub fn registered_theme_id_of(&self, theme: &VelaTheme) -> Option<String> {
        let key = theme_key(theme);
        self.entries
            .iter()
            .find(|(_, e)| theme_key(&resolve_object(&e.theme)) == key)
            .map(|(_, e)| e.id.clone())
    }
}

/// A theme object: a `createTheme` result (it carries `tokens`) becomes its chart
/// colors with its settings as `ui`; a plain chart theme is itself.
fn resolve_object(theme: &Value) -> VelaTheme {
    match theme {
        Value::Null => dark_theme(),
        Value::Object(map) if map.contains_key("tokens") => {
            let mut chart = match map.get("chart") {
                Some(Value::Object(chart)) => chart.clone(),
                _ => serde_json::Map::new(),
            };
            if let Some(settings) = map.get("settings") {
                chart.insert("ui".to_string(), settings.clone());
            }
            Value::Object(chart)
        }
        other => other.clone(),
    }
}

/// Two themes are the same when their seven fields and `ui` settings are equal.
fn theme_key(theme: &VelaTheme) -> String {
    let field = |name: &str| theme.get(name).cloned().unwrap_or(Value::Null);
    json!([
        field("background"),
        field("textColor"),
        field("gridColor"),
        field("borderColor"),
        field("upColor"),
        field("downColor"),
        field("fontFamily"),
        field("ui"),
    ])
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn midnight() -> Value {
        json!({
            "id": "t1",
            "chart": {
                "background": "#000011", "textColor": "#ccd", "gridColor": "#112",
                "borderColor": "#223", "upColor": "#0f0", "downColor": "#f00",
                "fontFamily": "serif"
            },
            "tokens": { "--vela-accent": "#38c0fd" },
            "components": {}, "css": {},
            "settings": { "accent": "#38c0fd", "radius": "lg" }
        })
    }

    fn entry(id: &str, theme: Value) -> RegisteredTheme {
        RegisteredTheme {
            id: id.into(),
            label: id.to_uppercase(),
            theme,
        }
    }

    #[test]
    fn the_builtin_themes_carry_the_original_colors() {
        assert_eq!(
            dark_theme(),
            json!({
                "background": "#151619", "textColor": "#b2b5be", "gridColor": "#20222c",
                "borderColor": "#2a2b30", "upColor": "#089981", "downColor": "#f23645",
                "fontFamily": "sans-serif"
            })
        );
        assert_eq!(
            light_theme(),
            json!({
                "background": "#ffffff", "textColor": "#1e293b", "gridColor": "#cccccc",
                "borderColor": "#d4dae3", "upColor": "#089981", "downColor": "#f23645",
                "fontFamily": "sans-serif"
            })
        );
    }

    #[test]
    fn nothing_dark_and_light_resolve_to_the_builtins() {
        let registry = ThemeRegistry::new();
        assert_eq!(registry.resolve(None), dark_theme());
        assert_eq!(registry.resolve(Some(&"dark".into())), dark_theme());
        assert_eq!(registry.resolve(Some(&"light".into())), light_theme());
        assert_eq!(registry.resolve(Some(&"".into())), dark_theme());
        assert_eq!(registry.resolve(Some(&"nope".into())), dark_theme());
    }

    #[test]
    fn a_chart_theme_object_is_itself() {
        let registry = ThemeRegistry::new();
        let mine = json!({ "background": "#123", "ui": { "accent": "#fff" } });
        assert_eq!(
            registry.resolve(Some(&ThemeSpec::Theme(mine.clone()))),
            mine
        );
    }

    #[test]
    fn a_create_theme_result_resolves_to_its_chart_colors_with_its_settings() {
        let registry = ThemeRegistry::new();
        let chart = registry.resolve(Some(&ThemeSpec::Theme(midnight())));
        assert_eq!(chart["background"], "#000011");
        assert_eq!(chart["fontFamily"], "serif");
        assert_eq!(chart["ui"], json!({ "accent": "#38c0fd", "radius": "lg" }));
        assert!(chart.get("tokens").is_none());
        assert_eq!(registry.resolve(Some(&ThemeSpec::Theme(midnight()))), chart);
    }

    #[test]
    fn a_registered_theme_resolves_by_id() {
        let mut registry = ThemeRegistry::new();
        let off = registry
            .register(entry("midnight", midnight()))
            .expect("registered");
        assert_eq!(
            registry
                .registered()
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            ["midnight"]
        );
        assert_eq!(
            registry.resolve(Some(&"midnight".into())),
            registry.resolve(Some(&ThemeSpec::Theme(midnight())))
        );
        let resolved = registry.resolve(Some(&ThemeSpec::Theme(midnight())));
        assert_eq!(
            registry.registered_theme_id_of(&resolved).as_deref(),
            Some("midnight")
        );
        // A copy, with a different key order or not, is still the same theme.
        let mut copy = resolved.clone();
        copy["extra"] = json!(1);
        assert_eq!(
            registry.registered_theme_id_of(&copy).as_deref(),
            Some("midnight")
        );
        assert_eq!(registry.registered_theme_id_of(&dark_theme()), None);
        assert!(registry.unregister(&off));
        assert!(registry.registered().is_empty());
        assert_eq!(registry.resolve(Some(&"midnight".into())), dark_theme());
    }

    #[test]
    fn a_registered_plain_chart_theme_resolves_to_itself() {
        let mut registry = ThemeRegistry::new();
        let plain = json!({ "background": "#222" });
        registry.register(entry("plain", plain.clone())).unwrap();
        assert_eq!(registry.resolve(Some(&"plain".into())), plain);
    }

    #[test]
    fn the_builtin_ids_cannot_be_registered() {
        let mut registry = ThemeRegistry::new();
        let err = registry.register(entry("dark", dark_theme())).unwrap_err();
        assert_eq!(err, ThemeError::BuiltIn("dark".into()));
        assert_eq!(
            err.to_string(),
            "registerTheme: \"dark\" is a built-in theme"
        );
        assert!(registry.register(entry("light", light_theme())).is_err());
        assert!(registry.registered().is_empty());
    }

    #[test]
    fn re_registering_replaces_in_place_and_an_old_disposer_leaves_the_newer_one() {
        let mut registry = ThemeRegistry::new();
        let first = registry
            .register(entry("a", json!({ "background": "#111" })))
            .unwrap();
        registry
            .register(entry("b", json!({ "background": "#222" })))
            .unwrap();
        let second = registry
            .register(entry("a", json!({ "background": "#333" })))
            .unwrap();
        let ids: Vec<String> = registry.registered().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(registry.resolve(Some(&"a".into()))["background"], "#333");
        assert!(!registry.unregister(&first));
        assert_eq!(registry.registered().len(), 2);
        assert!(registry.unregister(&second));
        assert_eq!(registry.registered().len(), 1);
        assert!(!registry.unregister(&second));
    }
}
