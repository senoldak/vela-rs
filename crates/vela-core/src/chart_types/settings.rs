//! The declarative settings rows a chart type contributes to the chart-settings
//! dialog: pure data (never DOM) plus the pure helpers that reduce and evaluate it.

use serde_json::{Map, Value};

use crate::i18n::{MessageArg, MessageLike, msg, with_values};

/// A value a settings condition compares against: a boolean, string or number.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsValue {
    Bool(bool),
    Text(String),
    Number(f64),
}

impl SettingsValue {
    /// Strict equality with a stored value (`===`): same JSON type and same value.
    fn matches(&self, stored: &Value) -> bool {
        match (self, stored) {
            (SettingsValue::Bool(a), Value::Bool(b)) => a == b,
            (SettingsValue::Text(a), Value::String(b)) => a == b,
            (SettingsValue::Number(a), Value::Number(b)) => b.as_f64() == Some(*a),
            _ => false,
        }
    }
}

/// A declarative visibility condition on another row's CURRENT value (stored value,
/// or that row's `defval` while unset). Pure data: the dialog evaluates it live as
/// the user edits, so dependent rows appear/disappear without a rebuild.
/// `any_of` wins over `equals` when both are given.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowCondition {
    pub key: String,
    pub equals: Option<SettingsValue>,
    pub any_of: Option<Vec<SettingsValue>>,
}

impl SettingsRowCondition {
    /// A condition that holds while `key` equals `value`.
    pub fn equals(key: impl Into<String>, value: SettingsValue) -> Self {
        Self {
            key: key.into(),
            equals: Some(value),
            any_of: None,
        }
    }

    /// A condition that holds while `key` equals any of `values`.
    pub fn any_of(key: impl Into<String>, values: Vec<SettingsValue>) -> Self {
        Self {
            key: key.into(),
            equals: None,
            any_of: Some(values),
        }
    }

    fn holds(&self, bag: &Map<String, Value>) -> bool {
        let stored = bag.get(&self.key);
        if let Some(any_of) = &self.any_of {
            return stored.is_some_and(|v| any_of.iter().any(|x| x.matches(v)));
        }
        match (&self.equals, stored) {
            // `bag[key] === undefined`: an unset key matches an absent `equals`.
            (None, None) => true,
            (Some(eq), Some(v)) => eq.matches(v),
            _ => false,
        }
    }
}

/// A row's visibility gate: one condition, or several AND-ed together.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsRowWhen {
    One(SettingsRowCondition),
    All(Vec<SettingsRowCondition>),
}

/// An inline color swatch on a toggle row: the color(s) the toggle governs, edited
/// right on the toggle's row (dimmed while the toggle is off) instead of a
/// conditionally revealed row below. Each swatch stores under its own bag key.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowSwatch {
    pub key: String,
    /// Names the color for the swatch's tooltip (`"Highlight color"`).
    pub label: MessageLike,
    pub defval: String,
    /// Optional visibility gate, evaluated live against the values bag exactly like
    /// a row's own `when`: lets one toggle row swap its swatch set as another value
    /// changes. A gated swatch is EXEMPT from the toggle-off dim: its gate already
    /// says when it matters, and it may exist specifically for the off state.
    pub when: Option<SettingsRowWhen>,
}

/// An inline LINE-WIDTH dropdown on a toggle row: sits beside the row's color
/// swatches (dimmed with them while the toggle is off) and stores a px number
/// under its own bag key. The control offers the classic drawing-bar weights
/// (1 to 5 px), each option previewed as a line at that weight.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowWidth {
    pub key: String,
    /// Names the control for its tooltip (`"Line width"`).
    pub label: MessageLike,
    pub defval: f64,
}

/// An inline NUMBER input on a toggle row: sits AHEAD of the row's color swatches
/// (dimmed with them while the toggle is off) and stores a number under its own
/// bag key. For the one value the toggle governs (a percent, a count): the
/// declarative alternative to a separate `number` row gated on the toggle.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowInlineNumber {
    pub key: String,
    /// Names the input for its tooltip (`"Value area (%)"`).
    pub label: MessageLike,
    pub defval: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
}

/// A select option: a bare string (value = label) or a value/label pair when the
/// stored id differs from the human-readable text (`("bidAskProfile", "Bid × Ask Profile")`).
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsSelectOption {
    Plain(String),
    Pair { value: String, label: MessageLike },
}

/// One INLINE CONTROL on a composite `row`: the composable unit every value row is
/// made of. Each keyed control stores under its own bag key; `label` names it for
/// the control's tooltip. A control may carry its own `when` gate (same shape as a
/// row's), evaluated live against the values bag: a gated control appears or
/// disappears as other values change, and is EXEMPT from the row's toggle-off dim:
/// its gate already says when it matters, and it may exist specifically for the
/// off state.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsInlineControl {
    /// A compact number input. With `placeholder`, an input at the default value
    /// renders EMPTY showing it, and clearing the input stores the default back:
    /// the placeholder names the "unset" state (`"Off"` for a 0-disables bound).
    Number {
        key: String,
        label: MessageLike,
        defval: f64,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        placeholder: Option<MessageLike>,
        when: Option<SettingsRowWhen>,
    },
    /// A color swatch opening the shared color picker.
    Color {
        key: String,
        label: MessageLike,
        defval: String,
        when: Option<SettingsRowWhen>,
    },
    /// A line-width dropdown (the classic drawing-bar 1 to 5 px weights, each
    /// option previewed as a line at that weight), storing a px number.
    Width {
        key: String,
        label: MessageLike,
        defval: f64,
        when: Option<SettingsRowWhen>,
    },
    /// A dropdown over [`SettingsSelectOption`]s.
    Select {
        key: String,
        label: MessageLike,
        options: Vec<SettingsSelectOption>,
        defval: String,
        when: Option<SettingsRowWhen>,
    },
    /// Display-only dimmed text between controls (the `–` of a min–max pair, a
    /// unit); stores nothing.
    Hint { text: MessageLike },
}

/// The leading checkbox of a composite row, storing a boolean under `key`.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowToggle {
    pub key: String,
    pub defval: bool,
}

/// One declarative settings row.
///
/// `Heading` titles a GROUP of rows: the heading plus everything after it up to the
/// next heading. In a flat `rows` section headings render as inline group titles;
/// inside `instances`/`subsections` (or a `Grouped` section) they become entries of
/// the pane's group TOC (see [`ChartTypeSettingsSection`]).
///
/// `Header` is an in-pane subgroup title: same visual as a flat heading, but inside
/// a structured pane it stays in the rows column (does NOT become a TOC entry). Use
/// it to cluster rows inside a TOC group (e.g. Colors / Values under Display).
///
/// `Row` is the COMPOSITE form every value row reduces to: a label, an optional
/// leading `toggle` (a checkbox storing a boolean under `toggle.key`; while off, the
/// row's controls dim and ignore input), and an ordered list of
/// [`SettingsInlineControl`]s in the control column. Any mix of control kinds in any
/// order.
///
/// The remaining kinds are SUGAR over `Row` (see [`normalize_settings_row`]):
/// - `Toggle`: a checkbox row; optional inline `number`, color `colors`, and `width`
///   attachments render in that order.
/// - `Number` / `Color` / `Select`: one control on its own row.
/// - `Range`: a min–max pair (two number inputs under `min_key`/`max_key`, both
///   seeded from the shared `defval`, with `placeholder` naming the unset state).
///
/// Any row may carry `when`: it is shown only while the condition holds.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsRowDescriptor {
    Heading {
        label: MessageLike,
        when: Option<SettingsRowWhen>,
    },
    Header {
        label: MessageLike,
        when: Option<SettingsRowWhen>,
    },
    Row {
        label: MessageLike,
        toggle: Option<SettingsRowToggle>,
        controls: Vec<SettingsInlineControl>,
        when: Option<SettingsRowWhen>,
    },
    Toggle {
        key: String,
        label: MessageLike,
        defval: bool,
        number: Option<SettingsRowInlineNumber>,
        colors: Vec<SettingsRowSwatch>,
        width: Option<SettingsRowWidth>,
        when: Option<SettingsRowWhen>,
    },
    Number {
        key: String,
        label: MessageLike,
        defval: f64,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        when: Option<SettingsRowWhen>,
    },
    Color {
        key: String,
        label: MessageLike,
        defval: String,
        when: Option<SettingsRowWhen>,
    },
    Select {
        key: String,
        label: MessageLike,
        options: Vec<SettingsSelectOption>,
        defval: String,
        when: Option<SettingsRowWhen>,
    },
    Range {
        label: MessageLike,
        min_key: String,
        max_key: String,
        defval: f64,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        placeholder: Option<MessageLike>,
        when: Option<SettingsRowWhen>,
    },
}

impl SettingsRowDescriptor {
    /// Whether this is a value-carrying row (everything except the key-less
    /// `Heading`/`Header` titles).
    pub fn is_value_row(&self) -> bool {
        !matches!(
            self,
            SettingsRowDescriptor::Heading { .. } | SettingsRowDescriptor::Header { .. }
        )
    }
}

/// The canonical row shape every value row reduces to (the `Row` kind's fields).
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedSettingsRow {
    pub label: MessageLike,
    pub toggle: Option<SettingsRowToggle>,
    pub controls: Vec<SettingsInlineControl>,
    pub when: Option<SettingsRowWhen>,
}

/// A message argument carrying a [`MessageLike`] (plain text stays text).
fn label_arg(label: &MessageLike) -> MessageArg {
    match label {
        MessageLike::Text(t) => MessageArg::Text(t.clone()),
        MessageLike::Message(d) => MessageArg::Message(d.clone()),
    }
}

/// Reduce a value row to the canonical composite shape: the ONE contract view
/// layers render and the persistence layer enumerates. The sugar kinds map exactly
/// onto their historical rendering: `Toggle` attachments in number, colors, width
/// order, `Range` as two number controls around a `–` hint (placeholder semantics
/// preserved). `None` for the key-less `Heading`/`Header` titles (the original's
/// type excludes them from this function).
pub fn normalize_settings_row(r: &SettingsRowDescriptor) -> Option<NormalizedSettingsRow> {
    use SettingsRowDescriptor as D;
    Some(match r {
        D::Heading { .. } | D::Header { .. } => return None,
        D::Row {
            label,
            toggle,
            controls,
            when,
        } => NormalizedSettingsRow {
            label: label.clone(),
            toggle: toggle.clone(),
            controls: controls.clone(),
            when: when.clone(),
        },
        D::Toggle {
            key,
            label,
            defval,
            number,
            colors,
            width,
            when,
        } => {
            let mut controls = Vec::new();
            if let Some(n) = number {
                controls.push(SettingsInlineControl::Number {
                    key: n.key.clone(),
                    label: n.label.clone(),
                    defval: n.defval,
                    min: n.min,
                    max: n.max,
                    step: n.step,
                    placeholder: None,
                    when: None,
                });
            }
            for c in colors {
                controls.push(SettingsInlineControl::Color {
                    key: c.key.clone(),
                    label: c.label.clone(),
                    defval: c.defval.clone(),
                    when: c.when.clone(),
                });
            }
            if let Some(w) = width {
                controls.push(SettingsInlineControl::Width {
                    key: w.key.clone(),
                    label: w.label.clone(),
                    defval: w.defval,
                    when: None,
                });
            }
            NormalizedSettingsRow {
                label: label.clone(),
                toggle: Some(SettingsRowToggle {
                    key: key.clone(),
                    defval: *defval,
                }),
                controls,
                when: when.clone(),
            }
        }
        D::Number {
            key,
            label,
            defval,
            min,
            max,
            step,
            when,
        } => NormalizedSettingsRow {
            label: label.clone(),
            toggle: None,
            controls: vec![SettingsInlineControl::Number {
                key: key.clone(),
                label: label.clone(),
                defval: *defval,
                min: *min,
                max: *max,
                step: *step,
                placeholder: None,
                when: None,
            }],
            when: when.clone(),
        },
        D::Color {
            key,
            label,
            defval,
            when,
        } => NormalizedSettingsRow {
            label: label.clone(),
            toggle: None,
            controls: vec![SettingsInlineControl::Color {
                key: key.clone(),
                label: label.clone(),
                defval: defval.clone(),
                when: None,
            }],
            when: when.clone(),
        },
        D::Select {
            key,
            label,
            options,
            defval,
            when,
        } => NormalizedSettingsRow {
            label: label.clone(),
            toggle: None,
            controls: vec![SettingsInlineControl::Select {
                key: key.clone(),
                label: label.clone(),
                options: options.clone(),
                defval: defval.clone(),
                when: None,
            }],
            when: when.clone(),
        },
        D::Range {
            label,
            min_key,
            max_key,
            defval,
            min,
            max,
            step,
            placeholder,
            when,
        } => {
            // The tooltips of a range's two inputs; `{label}` is the row's own label.
            let range_min = msg("chart.settings.rangeMin", "{label} — min");
            let range_max = msg("chart.settings.rangeMax", "{label} — max");
            let number = |key: &String, tip: &MessageLike| SettingsInlineControl::Number {
                key: key.clone(),
                label: with_values(tip, [("label", label_arg(label))]),
                defval: *defval,
                min: *min,
                max: *max,
                step: *step,
                placeholder: placeholder.clone(),
                when: None,
            };
            NormalizedSettingsRow {
                label: label.clone(),
                toggle: None,
                controls: vec![
                    number(min_key, &range_min),
                    SettingsInlineControl::Hint { text: "–".into() },
                    number(max_key, &range_max),
                ],
                when: when.clone(),
            }
        }
    })
}

/// The runtime type of a value a settings row stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsValueType {
    Boolean,
    Number,
    String,
}

impl SettingsValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsValueType::Boolean => "boolean",
            SettingsValueType::Number => "number",
            SettingsValueType::String => "string",
        }
    }
}

/// One value a settings row stores: its bag key, runtime type, and registry default.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsRowValueKey {
    pub key: String,
    pub value_type: SettingsValueType,
    pub defval: SettingsValue,
}

/// EVERY key a row stores (toggle key + each keyed inline control), with its
/// expected type and default: the single enumeration the dialog seeds from and the
/// factory reset restores, so no key can fall through a kind-specific walk.
pub fn settings_row_value_keys(r: &SettingsRowDescriptor) -> Vec<SettingsRowValueKey> {
    let Some(n) = normalize_settings_row(r) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    if let Some(t) = &n.toggle {
        keys.push(SettingsRowValueKey {
            key: t.key.clone(),
            value_type: SettingsValueType::Boolean,
            defval: SettingsValue::Bool(t.defval),
        });
    }
    for c in &n.controls {
        match c {
            SettingsInlineControl::Hint { .. } => {}
            SettingsInlineControl::Color { key, defval, .. } => {
                keys.push(SettingsRowValueKey {
                    key: key.clone(),
                    value_type: SettingsValueType::String,
                    defval: SettingsValue::Text(defval.clone()),
                });
            }
            SettingsInlineControl::Select { key, defval, .. } => {
                keys.push(SettingsRowValueKey {
                    key: key.clone(),
                    value_type: SettingsValueType::String,
                    defval: SettingsValue::Text(defval.clone()),
                });
            }
            SettingsInlineControl::Number { key, defval, .. }
            | SettingsInlineControl::Width { key, defval, .. } => {
                keys.push(SettingsRowValueKey {
                    key: key.clone(),
                    value_type: SettingsValueType::Number,
                    defval: SettingsValue::Number(*defval),
                });
            }
        }
    }
    keys
}

/// Whether a row's `when` gate passes against the current values bag.
pub fn settings_row_visible(when: Option<&SettingsRowWhen>, bag: &Map<String, Value>) -> bool {
    match when {
        None => true,
        Some(SettingsRowWhen::One(c)) => c.holds(bag),
        Some(SettingsRowWhen::All(cs)) => cs.iter().all(|c| c.holds(bag)),
    }
}

/// One tab of a section's INSTANCE STRIP: a repeated block of settings (e.g. one of
/// several overlays a style can paint). The dialog shows a tab per PRESENT
/// instance, a dashed `+` that turns on the next absent one, and an `×` on the
/// active removable tab that turns it off. Presence is the boolean at `enable_key`
/// (stored in the same per-type bag as every other value); an instance without
/// `enable_key` is always present and not removable: the base instance.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartTypeSettingsInstance {
    pub label: MessageLike,
    /// Boolean bag key controlling presence; `None` = always present, not removable.
    pub enable_key: Option<String>,
    pub rows: Vec<SettingsRowDescriptor>,
}

/// An indented sub-entry under the section's rail tab, with its own pane of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartTypeSettingsSubsection {
    pub title: MessageLike,
    pub rows: Vec<SettingsRowDescriptor>,
    /// Boolean bag key that masters this pane. While it is false, every row EXCEPT
    /// the one whose `key` matches stays visible but grayed out (not hidden), so
    /// users can still browse and preview settings with the feature off. The
    /// matching toggle is typically the first row of the Display group.
    pub enable_key: Option<String>,
}

/// How a section's flat `rows` are presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsLayout {
    /// Headings render as inline group titles (the historical form).
    #[default]
    Flat,
    /// Headings are promoted to a group TOC beside the rows (the structured pane's
    /// presentation, without an instance strip).
    Grouped,
}

impl SettingsLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsLayout::Flat => "flat",
            SettingsLayout::Grouped => "grouped",
        }
    }
}

/// When a section is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsVisibility {
    /// Only while this chart type is the active price style.
    #[default]
    Active,
    /// Whenever the type is registered.
    Always,
}

impl SettingsVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsVisibility::Active => "active",
            SettingsVisibility::Always => "always",
        }
    }
}

/// Where a section's tab sits in the dialog's rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPlacement {
    /// After the built-in tabs.
    #[default]
    End,
    /// Directly under the Symbol tab. Subsections follow their parent.
    AfterSymbol,
}

impl SettingsPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsPlacement::End => "end",
            SettingsPlacement::AfterSymbol => "after-symbol",
        }
    }
}

/// A chart type's settings tab. Two forms:
///
/// - **Flat**: `rows` only, rendered as one scrollable pane, `Heading` rows as
///   inline group titles (the historical form). `layout: Grouped` upgrades the same
///   rows to the GROUP TOC presentation (below) without declaring an instance strip.
/// - **Structured**: `instances` (and optionally `subsections`): the pane opens
///   with an instance TAB STRIP, and each instance's rows are organized by a GROUP
///   TOC on the left of the pane (right of the dialog's tab rail) built from its
///   `Heading` rows (rows before the first heading always show above the groups;
///   selecting a TOC entry shows only that group's rows). `subsections` add
///   indented rail entries under the section's tab, each a pane with the same TOC
///   treatment. A TOC entry hides itself while every row of its group is hidden by
///   `when` gates.
///
/// All values, every instance and subsection included, live in the ONE per-type bag
/// (`config.chartTypes[<id>]`), so consumers keep receiving a single flat object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartTypeSettingsSection {
    /// Section heading in the settings dialog.
    pub title: MessageLike,
    /// Flat form. Ignored when `instances` is non-empty.
    pub rows: Vec<SettingsRowDescriptor>,
    /// How `rows` are presented. Ignored with `instances`.
    pub layout: SettingsLayout,
    /// Structured form: the pane's instance tab strip.
    pub instances: Vec<ChartTypeSettingsInstance>,
    /// Indented sub-entries under this section's rail tab.
    pub subsections: Vec<ChartTypeSettingsSubsection>,
    pub visibility: SettingsVisibility,
    pub placement: SettingsPlacement,
}
