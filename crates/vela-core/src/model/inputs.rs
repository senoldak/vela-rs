//! Renderer-neutral indicator input schema: Vela's own shape, so no scripting
//! language's input model leaks into core. Drives a settings dialog.

use std::collections::{BTreeMap, BTreeSet};

use crate::i18n::{MessageLike, number_text};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputType {
    Int,
    Float,
    Bool,
    String,
    Source,
    Color,
    Price,
    Time,
    Session,
    Timeframe,
    Symbol,
    TextArea,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    Number(f64),
    Text(String),
    Bool(bool),
}

impl InputValue {
    /// The value as text, the way a multi-select or an option list reads it.
    pub fn to_text(&self) -> String {
        match self {
            InputValue::Number(n) => number_text(*n),
            InputValue::Text(s) => s.clone(),
            InputValue::Bool(b) => b.to_string(),
        }
    }
}

impl From<f64> for InputValue {
    fn from(v: f64) -> Self {
        InputValue::Number(v)
    }
}

impl From<i32> for InputValue {
    fn from(v: i32) -> Self {
        InputValue::Number(f64::from(v))
    }
}

impl From<bool> for InputValue {
    fn from(v: bool) -> Self {
        InputValue::Bool(v)
    }
}

impl From<&str> for InputValue {
    fn from(v: &str) -> Self {
        InputValue::Text(v.to_string())
    }
}

impl From<String> for InputValue {
    fn from(v: String) -> Self {
        InputValue::Text(v)
    }
}

/// A bag of input values keyed by input key.
pub type Inputs = BTreeMap<String, InputValue>;

/// The display side of an input: what a settings dialog and the legend SHOW.
/// The input's own `title`, `group` and `options` stay what they are (a script
/// reads them, layouts persist the option values). Each field falls back to
/// the identity field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputDisplay {
    pub title: Option<MessageLike>,
    pub tooltip: Option<MessageLike>,
    pub group: Option<MessageLike>,
    /// Shown label per stored option value. Values without an entry show as themselves.
    pub options: Option<BTreeMap<String, MessageLike>>,
}

impl InputDisplay {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.tooltip.is_none()
            && self.group.is_none()
            && self.options.is_none()
    }
}

/// A multi-select string input. The stored value stays ONE string: the checked
/// options joined by `separator` in `options` order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputMultiple {
    pub separator: String,
    /// Options that are always checked.
    pub locked: Vec<String>,
}

/// A help mark on a group's section heading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputGroupHelp {
    pub text: Option<MessageLike>,
    /// Only `http:` and `https:` URLs are honoured by a host.
    pub url: Option<String>,
}

/// One visibility condition on another input's current value: `any_of`
/// matches a set, otherwise `equals` matches a single value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputCondition {
    pub key: String,
    pub equals: Option<InputValue>,
    pub any_of: Option<Vec<InputValue>>,
}

/// An input's visibility gate: conditions AND-ed together. Empty ⇒ visible.
pub type InputWhen = Vec<InputCondition>;

/// Evaluate a visibility gate against the dialog's resolved current values.
pub fn input_visible(when: &[InputCondition], values: &Inputs) -> bool {
    when.iter().all(|c| {
        let current = values.get(&c.key);
        match &c.any_of {
            Some(set) => set.iter().any(|x| Some(x) == current),
            None => current == c.equals.as_ref(),
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputSchema {
    /// Stable key used to set the input.
    pub key: String,
    /// Display label (the input's identity text).
    pub title: String,
    pub input_type: InputType,
    pub defval: InputValue,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// Choices for a dropdown.
    pub options: Option<Vec<String>>,
    /// Grouping label for the dialog layout.
    pub group: Option<String>,
    /// Inline grouping label (controls placed on one row).
    pub inline: Option<String>,
    /// Settings-dialog tab hosting this input; `None` ⇒ the default tab.
    pub tab: Option<String>,
    /// Visibility gate; a hidden input keeps its value.
    pub when: InputWhen,
    pub tooltip: Option<String>,
    /// Translatable display text for this input.
    pub display: Option<InputDisplay>,
    /// Check several `options` at once. Read on a string input with `options` only.
    pub multiple: Option<InputMultiple>,
    pub group_help: Option<InputGroupHelp>,
}

impl InputSchema {
    /// A schema with only the required fields set.
    pub fn new(
        key: impl Into<String>,
        title: impl Into<String>,
        input_type: InputType,
        defval: impl Into<InputValue>,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            input_type,
            defval: defval.into(),
            min: None,
            max: None,
            step: None,
            options: None,
            group: None,
            inline: None,
            tab: None,
            when: Vec::new(),
            tooltip: None,
            display: None,
            multiple: None,
            group_help: None,
        }
    }
}

/// The distinct options, in declaration order.
fn option_list(options: Option<&[String]>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    options
        .unwrap_or(&[])
        .iter()
        .filter(|o| seen.insert(o.as_str()))
        .cloned()
        .collect()
}

/// The options a multi-select value checks, in `options` order: the value
/// split by the separator, fragments that name no option dropped, `locked`
/// options always in. An empty value checks only the locked options.
pub fn parse_multi_value(
    options: Option<&[String]>,
    multiple: Option<&InputMultiple>,
    value: &InputValue,
) -> Vec<String> {
    let options = option_list(options);
    let raw = value.to_text();
    let sep = multiple.map(|m| m.separator.as_str()).unwrap_or("");
    let mut checked: BTreeSet<&str> = multiple
        .map(|m| m.locked.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let parts: Vec<&str> = if raw.is_empty() {
        Vec::new()
    } else if sep.is_empty() {
        vec![raw.as_str()]
    } else {
        raw.split(sep).collect()
    };
    for part in parts {
        // Trimmed as a fallback: a `,` separator over `A, B` still names B.
        if options.iter().any(|o| o == part) {
            checked.insert(part);
        } else if options.iter().any(|o| o == part.trim()) {
            checked.insert(part.trim());
        }
    }
    options
        .iter()
        .filter(|o| checked.contains(o.as_str()))
        .cloned()
        .collect()
}

/// The stored value for a set of checked options: the locked options added,
/// unknown ones dropped, joined by the separator in `options` order.
pub fn join_multi_value<'a>(
    options: Option<&[String]>,
    multiple: Option<&InputMultiple>,
    checked: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut picked: BTreeSet<&str> = checked.into_iter().collect();
    if let Some(m) = multiple {
        picked.extend(m.locked.iter().map(String::as_str));
    }
    let sep = multiple.map(|m| m.separator.as_str()).unwrap_or("");
    option_list(options)
        .into_iter()
        .filter(|o| picked.contains(o.as_str()))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Equality the way the original compares JSON text: a non-finite number
/// serializes to `null`, so two of them are equal.
fn same_json(a: &InputValue, b: &InputValue) -> bool {
    match (a, b) {
        (InputValue::Number(x), InputValue::Number(y)) => {
            (!x.is_finite() && !y.is_finite()) || x == y
        }
        _ => a == b,
    }
}

/// The DELTAS of a value bag against its schema's declaration defaults: what
/// state persistence stores. Keys the schema does not declare are ignored.
/// `None` = nothing deviates.
pub fn input_deltas(schema: &[InputSchema], values: &Inputs) -> Option<Inputs> {
    let mut out = Inputs::new();
    for s in schema {
        if let Some(v) = values.get(&s.key)
            && !same_json(v, &s.defval)
        {
            out.insert(s.key.clone(), v.clone());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gate_passes_on_equals_and_any_of() {
        let mut values = Inputs::new();
        values.insert("mode".into(), "B".into());
        values.insert("on".into(), true.into());
        let eq = InputCondition {
            key: "on".into(),
            equals: Some(true.into()),
            any_of: None,
        };
        let any = InputCondition {
            key: "mode".into(),
            equals: None,
            any_of: Some(vec!["A".into(), "B".into()]),
        };
        assert!(input_visible(&[], &values));
        assert!(input_visible(&[eq.clone(), any], &values));
        let miss = InputCondition {
            key: "mode".into(),
            equals: None,
            any_of: Some(vec!["C".into()]),
        };
        assert!(!input_visible(&[eq, miss], &values));
    }

    #[test]
    fn multi_value_round_trips_in_option_order() {
        let options = opts(&["A", "B", "C"]);
        let multiple = InputMultiple {
            separator: ", ".into(),
            locked: vec!["A".into()],
        };
        let checked = parse_multi_value(Some(&options), Some(&multiple), &"C, X, B".into());
        assert_eq!(checked, opts(&["A", "B", "C"]));
        assert_eq!(
            parse_multi_value(Some(&options), Some(&multiple), &"".into()),
            opts(&["A"])
        );
        assert_eq!(
            join_multi_value(Some(&options), Some(&multiple), ["C", "Z"]),
            "A, C"
        );
    }

    #[test]
    fn a_comma_separator_still_names_a_spaced_fragment() {
        let options = opts(&["A", "B"]);
        let multiple = InputMultiple {
            separator: ",".into(),
            locked: vec![],
        };
        assert_eq!(
            parse_multi_value(Some(&options), Some(&multiple), &"A, B".into()),
            opts(&["A", "B"])
        );
    }

    #[test]
    fn deltas_keep_only_what_deviates() {
        let schema = vec![
            InputSchema::new("length", "Length", InputType::Int, 14),
            InputSchema::new("source", "Source", InputType::String, "Close"),
        ];
        let mut values = Inputs::new();
        values.insert("length".into(), 14.into());
        values.insert("source".into(), "Open".into());
        values.insert("unknown".into(), 1.into());
        let deltas = input_deltas(&schema, &values).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas.get("source"), Some(&"Open".into()));
        values.insert("source".into(), "Close".into());
        assert!(input_deltas(&schema, &values).is_none());
    }
}
