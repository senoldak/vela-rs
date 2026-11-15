//! The catalog of drawing types: what each contributes to a toolbar, and the
//! factory that builds an instance from a (partial) record.
//!
//! As with the indicator catalog, the registry is an explicit value rather
//! than the original's process-wide map. [`DrawingRegistry::builtin`] holds
//! every built-in tool in the original's registration order.

use crate::i18n::MessageLike;

use super::drawing::{Drawing, DrawingInit, SerializedDrawing};
use super::style::DrawingStyle;
use super::types;

/// Builds an instance from a (partial) record. Serves create + deserialize.
pub type DrawingFactory = fn(&DrawingInit) -> Box<dyn Drawing>;

/// What a drawing type contributes to the toolbar + factory (renderer-neutral).
#[derive(Clone, Debug)]
pub struct DrawingTypeMeta {
    pub type_key: &'static str,
    /// Toolbar group id (e.g. `lines`, `shapes`, `annotations`).
    pub group: &'static str,
    /// The tool's name (toolbar, settings, object tree).
    pub label: MessageLike,
    /// Inline SVG markup for the toolbar button; a renderer paints it.
    pub icon: String,
    pub default_style: DrawingStyle,
    /// The tool's body REPLACES the series pixels inside its area (an opaque
    /// inset): a fresh instance then starts ABOVE the pane's series stack
    /// instead of just under the candles.
    pub covers_series: bool,
    /// One short sentence prompting the placement gesture.
    pub placement_hint: Option<MessageLike>,
    pub create: DrawingFactory,
}

impl DrawingTypeMeta {
    pub fn new(
        type_key: &'static str,
        group: &'static str,
        label: impl Into<MessageLike>,
        icon: impl Into<String>,
        default_style: DrawingStyle,
        create: DrawingFactory,
    ) -> Self {
        Self {
            type_key,
            group,
            label: label.into(),
            icon: icon.into(),
            default_style,
            covers_series: false,
            placement_hint: None,
            create,
        }
    }

    pub fn covers_series(mut self) -> Self {
        self.covers_series = true;
        self
    }

    pub fn placement_hint(mut self, hint: impl Into<MessageLike>) -> Self {
        self.placement_hint = Some(hint.into());
        self
    }
}

/// A 24×24 drawing-tool icon around `body`.
pub fn svg24(body: &str) -> String {
    svg24_with(body, "")
}

/// A 24×24 drawing-tool icon; `extra` adds root attributes ahead of the defaults.
pub fn svg24_with(body: &str, extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("{extra} ")
    };
    format!(
        r#"<svg viewBox="0 0 24 24" width="1em" height="1em" {extra}fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
    )
}

/// A 24×24 glyph drawn as a solid shape rather than a stroke.
pub fn svg24_solid(body: &str) -> String {
    format!(
        r#"<svg viewBox="0 0 24 24" width="1em" height="1em" fill="currentColor" stroke="none">{body}</svg>"#
    )
}

/// A catalog of drawing types, in registration order.
#[derive(Clone, Debug, Default)]
pub struct DrawingRegistry {
    entries: Vec<DrawingTypeMeta>,
}

impl DrawingRegistry {
    /// An empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every built-in tool, in the original's registration order.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for meta in types::builtin_metas() {
            registry.register(meta);
        }
        registry
    }

    /// Register (or replace, in place) a drawing type.
    pub fn register(&mut self, meta: DrawingTypeMeta) {
        match self
            .entries
            .iter_mut()
            .find(|m| m.type_key == meta.type_key)
        {
            Some(slot) => *slot = meta,
            None => self.entries.push(meta),
        }
    }

    /// A registered type.
    pub fn get(&self, type_key: &str) -> Option<&DrawingTypeMeta> {
        self.entries.iter().find(|m| m.type_key == type_key)
    }

    /// Every registered type, in registration order.
    pub fn entries(&self) -> &[DrawingTypeMeta] {
        &self.entries
    }

    /// Build a fresh drawing of `type_key`, seeding the type's default style.
    /// Any caller style merges onto the default, so a partial override keeps
    /// the rest (a box keeps its default fill when only the line color is set).
    pub fn create(&self, type_key: &str, mut init: DrawingInit) -> Option<Box<dyn Drawing>> {
        let meta = self.get(type_key)?;
        let style = meta
            .default_style
            .merged(&init.style.take().unwrap_or_default());
        init.style = Some(style.to_partial());
        Some((meta.create)(&init))
    }

    /// Restore a drawing's cosmetics + type extras to the type's factory
    /// defaults. Keeps identity / geometry / lock / visibility / z-order, and
    /// preserves typed text content when the type seeds a text block.
    pub fn reset_settings(&self, drawing: &mut dyn Drawing) {
        let Some(fresh) = self.create(
            drawing.type_key(),
            DrawingInit::on_pane(drawing.base().pane_id.clone()),
        ) else {
            return;
        };
        let kept_value = drawing.base().text.as_ref().map(|t| t.value.clone());
        let base = drawing.base_mut();
        base.style = fresh.base().style.clone();
        // A type that seeds no text drops lazily-created text styling.
        base.text = fresh.base().text.clone().map(|mut text| {
            if let Some(value) = kept_value {
                text.value = value;
            }
            text
        });
        if let Some(props) = fresh.write_props() {
            drawing.apply_props(&props);
        }
    }

    /// Rehydrate a drawing from a plain record, or `None` for an unknown type.
    pub fn deserialize(&self, doc: &SerializedDrawing) -> Option<Box<dyn Drawing>> {
        let meta = self.get(&doc.type_key)?;
        Some((meta.create)(&DrawingInit::from(doc.clone())))
    }
}
