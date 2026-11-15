//! Renderer FEATURE defaults contributed by plugins.
//!
//! A renderer feature (`chart.renderer.set(key, value)`) is per-chart state set
//! on an instance that does not exist yet when a plugin runs, and no registry
//! carried it: a plugin could contribute a chart type, an engine or a panel, but
//! not "every chart should start with this feature set". This is that missing
//! half, the renderer-side counterpart of `registerDefaultEngine`, except it
//! reaches EVERY chart: the widget's, each workspace cell's, and a bare chart.
//!
//! The original keeps one module-level map; here the registry is a value the
//! caller owns and shares among the charts it builds.

use serde_json::Value;

/// What [`RendererDefaults::register`] set: hand it to
/// [`RendererDefaults::dispose`] to remove precisely those values.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultsRegistration {
    applied: Vec<(String, Value)>,
}

/// The renderer feature defaults registered so far, in registration order.
///
/// Register default values for renderer features: every chart built afterwards
/// applies them once its renderer is mounted, before the first paint. Keys are
/// renderer feature names (`chart.renderer.features`); one the active renderer
/// does not support is warned about and ignored, exactly as `renderer.set()` does.
///
/// These are DEFAULTS, not locks: an explicit `chart.renderer.set(...)`, an
/// applied config template, or the user's own in-chart settings still win
/// afterwards. Charts already built are untouched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RendererDefaults {
    entries: Vec<(String, Value)>,
}

impl RendererDefaults {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register default values. A key registered again keeps its place and takes
    /// the new value. The returned registration removes exactly what this call
    /// set, leaving any later re-registration of the same key in place.
    pub fn register(
        &mut self,
        values: impl IntoIterator<Item = (String, Value)>,
    ) -> DefaultsRegistration {
        let applied: Vec<(String, Value)> = values.into_iter().collect();
        for (key, value) in &applied {
            match self.entries.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value.clone(),
                None => self.entries.push((key.clone(), value.clone())),
            }
        }
        DefaultsRegistration { applied }
    }

    /// Remove precisely the values `registration` set: a key whose value has
    /// since been replaced by another registration stays.
    pub fn dispose(&mut self, registration: &DefaultsRegistration) {
        for (key, value) in &registration.applied {
            self.entries.retain(|(k, v)| !(k == key && v == value));
        }
    }

    /// Drop registered defaults by key.
    pub fn unregister(&mut self, keys: &[&str]) {
        self.entries.retain(|(k, _)| !keys.contains(&k.as_str()));
    }

    /// Drop every registered default.
    pub fn unregister_all(&mut self) {
        self.entries.clear();
    }

    /// The defaults a chart applies at construction (empty when no plugin
    /// registered any), in registration order. A snapshot: changing it changes
    /// nothing here.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        self.entries.clone()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn values(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn starts_empty_and_collects_what_is_registered() {
        let mut defaults = RendererDefaults::new();
        assert!(defaults.snapshot().is_empty());
        defaults.register(values(&[
            ("attribution", json!(false)),
            ("gridlines", json!(false)),
        ]));
        assert_eq!(
            defaults.snapshot(),
            values(&[("attribution", json!(false)), ("gridlines", json!(false))])
        );
    }

    #[test]
    fn merges_registrations_last_write_winning_per_key() {
        let mut defaults = RendererDefaults::new();
        defaults.register(values(&[
            ("attribution", json!(false)),
            ("gridlines", json!(false)),
        ]));
        defaults.register(values(&[("attribution", json!("<b>ACME</b>"))]));
        assert_eq!(
            defaults.snapshot(),
            values(&[
                ("attribution", json!("<b>ACME</b>")),
                ("gridlines", json!(false))
            ])
        );
    }

    #[test]
    fn the_disposer_removes_exactly_what_it_set_and_never_a_newer_value() {
        let mut defaults = RendererDefaults::new();
        let first = defaults.register(values(&[
            ("attribution", json!(false)),
            ("gridlines", json!(false)),
        ]));
        defaults.register(values(&[("attribution", json!("<b>ACME</b>"))]));
        defaults.dispose(&first);
        assert_eq!(
            defaults.snapshot(),
            values(&[("attribution", json!("<b>ACME</b>"))])
        );
    }

    #[test]
    fn unregister_drops_by_key_or_everything() {
        let mut defaults = RendererDefaults::new();
        defaults.register(values(&[
            ("attribution", json!(false)),
            ("gridlines", json!(false)),
        ]));
        defaults.unregister(&["gridlines"]);
        assert_eq!(
            defaults.snapshot(),
            values(&[("attribution", json!(false))])
        );
        defaults.unregister_all();
        assert!(defaults.snapshot().is_empty());
        defaults.unregister(&["missing"]);
    }

    #[test]
    fn hands_out_a_snapshot_so_mutating_it_cannot_poison_the_registry() {
        let mut defaults = RendererDefaults::new();
        defaults.register(values(&[("gridlines", json!(false))]));
        let mut snapshot = defaults.snapshot();
        snapshot[0].1 = json!(true);
        snapshot.push(("injected".into(), json!("nope")));
        assert_eq!(defaults.snapshot(), values(&[("gridlines", json!(false))]));
    }

    #[test]
    fn a_key_registered_again_keeps_its_place() {
        let mut defaults = RendererDefaults::new();
        defaults.register(values(&[("a", json!(1)), ("b", json!(2))]));
        defaults.register(values(&[("a", json!(3))]));
        assert_eq!(
            defaults.snapshot(),
            values(&[("a", json!(3)), ("b", json!(2))])
        );
    }
}
