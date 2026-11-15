//! A data-driven settings schema. Each [`SettingsField`] names a dot-path into
//! a drawing (`style.lineColor`, `text.value`) plus a control kind, so a
//! settings popup builds controls generically: adding a drawing type is a
//! schema entry, not new UI code.

use serde_json::Value;

use crate::i18n::{MessageLike, msg};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Color,
    Number,
    Select,
    LineStyle,
    Boolean,
    Text,
    Opacity,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Color => "color",
            FieldKind::Number => "number",
            FieldKind::Select => "select",
            FieldKind::LineStyle => "lineStyle",
            FieldKind::Boolean => "boolean",
            FieldKind::Text => "text",
            FieldKind::Opacity => "opacity",
        }
    }
}

/// Cosmetic grouping in the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldGroup {
    Line,
    Fill,
    Text,
    Behavior,
}

impl FieldGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldGroup::Line => "line",
            FieldGroup::Fill => "fill",
            FieldGroup::Text => "text",
            FieldGroup::Behavior => "behavior",
        }
    }
}

/// One choice of a select control.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldOption {
    pub value: String,
    pub label: MessageLike,
}

impl FieldOption {
    pub fn new(value: impl Into<String>, label: impl Into<MessageLike>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A field offered only while another setting holds one of these values.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldWhen {
    pub path: String,
    pub any_of: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsField {
    /// Dot-path into the drawing, e.g. `style.lineColor`, `text.value`, `locked`.
    pub path: String,
    /// What the settings show.
    pub label: MessageLike,
    pub kind: FieldKind,
    /// number/opacity bounds.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// select options.
    pub options: Option<Vec<FieldOption>>,
    pub group: Option<FieldGroup>,
    /// Only offered while another setting holds one of these values.
    pub when: Option<FieldWhen>,
    /// A color that follows another setting until one is picked: the path
    /// whose value the control shows while this one is unset.
    pub fallback: Option<String>,
    /// The label of the one row this field shares with the next one.
    pub row_label: Option<MessageLike>,
}

impl SettingsField {
    /// A field with only the required parts; refine it with the builder methods.
    pub fn new(path: impl Into<String>, label: impl Into<MessageLike>, kind: FieldKind) -> Self {
        Self {
            path: path.into(),
            label: label.into(),
            kind,
            min: None,
            max: None,
            step: None,
            options: None,
            group: None,
            when: None,
            fallback: None,
            row_label: None,
        }
    }

    pub fn group(mut self, group: FieldGroup) -> Self {
        self.group = Some(group);
        self
    }

    /// Bounds of a number/opacity control.
    pub fn range(mut self, min: f64, max: f64, step: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self.step = Some(step);
        self
    }

    pub fn options(mut self, options: Vec<FieldOption>) -> Self {
        self.options = Some(options);
        self
    }

    pub fn when(mut self, path: impl Into<String>, any_of: Vec<Value>) -> Self {
        self.when = Some(FieldWhen {
            path: path.into(),
            any_of,
        });
        self
    }

    pub fn fallback(mut self, path: impl Into<String>) -> Self {
        self.fallback = Some(path.into());
        self
    }

    pub fn row_label(mut self, label: impl Into<MessageLike>) -> Self {
        self.row_label = Some(label.into());
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsSchema {
    pub fields: Vec<SettingsField>,
    /// The text IS the drawing (a text label, a note, a callout) rather than an
    /// optional label on a shape: a popup then puts the text controls on the
    /// bar itself instead of under the text field.
    pub text_is_content: bool,
}

impl SettingsSchema {
    pub fn new(fields: Vec<SettingsField>) -> Self {
        Self {
            fields,
            text_is_content: false,
        }
    }
}

/// The line-style choices most tools expose.
pub fn line_style_options() -> Vec<FieldOption> {
    vec![
        FieldOption::new("solid", msg("drawing.option.solid", "Solid")),
        FieldOption::new("dashed", msg("drawing.option.dashed", "Dashed")),
        FieldOption::new("dotted", msg("drawing.option.dotted", "Dotted")),
    ]
}

/// Reusable line-cosmetics fields shared by most drawings.
pub fn line_fields() -> Vec<SettingsField> {
    vec![
        SettingsField::new(
            "style.lineColor",
            msg("drawing.field.lineColor", "Line color"),
            FieldKind::Color,
        )
        .group(FieldGroup::Line),
        SettingsField::new(
            "style.lineWidth",
            msg("drawing.field.lineWidth", "Line width"),
            FieldKind::Number,
        )
        .range(1.0, 10.0, 1.0)
        .group(FieldGroup::Line),
        SettingsField::new(
            "style.lineStyle",
            msg("drawing.field.lineStyle", "Line style"),
            FieldKind::LineStyle,
        )
        .options(line_style_options())
        .group(FieldGroup::Line),
    ]
}

/// The line fields of a closed shape, where the line is its border.
pub fn border_fields() -> Vec<SettingsField> {
    vec![
        SettingsField::new(
            "style.lineColor",
            msg("drawing.field.borderColor", "Border color"),
            FieldKind::Color,
        )
        .group(FieldGroup::Line),
        SettingsField::new(
            "style.lineWidth",
            msg("drawing.field.borderWidth", "Border width"),
            FieldKind::Number,
        )
        .range(1.0, 10.0, 1.0)
        .group(FieldGroup::Line),
        SettingsField::new(
            "style.lineStyle",
            msg("drawing.field.borderStyle", "Border style"),
            FieldKind::LineStyle,
        )
        .options(line_style_options())
        .group(FieldGroup::Line),
    ]
}

/// Reusable fill fields shared by filled shapes. The fill's opacity lives in
/// the fill color's own alpha, so there is no separate opacity field.
pub fn fill_fields() -> Vec<SettingsField> {
    vec![
        SettingsField::new(
            "style.fillColor",
            msg("drawing.field.fillColor", "Fill color"),
            FieldKind::Color,
        )
        .group(FieldGroup::Fill),
    ]
}

/// Text-size choices (drives the size select).
pub fn text_size_options() -> Vec<FieldOption> {
    vec![
        FieldOption::new("small", msg("drawing.option.small", "Small")),
        FieldOption::new("normal", msg("drawing.option.normal", "Normal")),
        FieldOption::new("large", msg("drawing.option.large", "Large")),
        FieldOption::new("huge", msg("drawing.option.huge", "Huge")),
    ]
}

/// Reusable text fields: every drawing can carry an optional label.
pub fn text_fields() -> Vec<SettingsField> {
    vec![
        SettingsField::new(
            "text.value",
            msg("drawing.field.text", "Text"),
            FieldKind::Text,
        )
        .group(FieldGroup::Text),
        SettingsField::new(
            "text.color",
            msg("drawing.field.textColor", "Text color"),
            FieldKind::Color,
        )
        .group(FieldGroup::Text),
        SettingsField::new(
            "text.size",
            msg("drawing.field.textSize", "Text size"),
            FieldKind::Select,
        )
        .options(text_size_options())
        .group(FieldGroup::Text),
        SettingsField::new(
            "text.bold",
            msg("drawing.field.bold", "Bold"),
            FieldKind::Boolean,
        )
        .group(FieldGroup::Text),
        SettingsField::new(
            "text.italic",
            msg("drawing.field.italic", "Italic"),
            FieldKind::Boolean,
        )
        .group(FieldGroup::Text),
    ]
}

/// Line fields followed by text fields: the schema of a plain line tool.
pub fn line_and_text_fields() -> Vec<SettingsField> {
    let mut fields = line_fields();
    fields.extend(text_fields());
    fields
}
