//! A tool's remembered settings and its saved looks.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::i18n::{MessageLike, msg};
use crate::model::LineStyle;
use crate::palette::{BEARISH, BULLISH, NEUTRAL};

use super::drawing::{Drawing, Props};
use super::style::{DrawingText, PartialDrawingStyle};

/// A tool's remembered settings: what the next drawing of that type starts
/// from. Captured from the last drawing of the type the user styled: its
/// style, its text styling (never the words themselves) and its per-type
/// settings (`props`: fib levels, position sizing, profile rows…). Geometry,
/// lock, visibility and depth are per-drawing and never carried over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DrawingToolDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<PartialDrawingStyle>,
    /// Text styling as plain JSON, without the `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Props>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<Props>,
}

fn text_styling(text: &DrawingText) -> Props {
    let mut styling = match serde_json::to_value(text) {
        Ok(Value::Object(map)) => map,
        _ => Props::new(),
    };
    styling.remove("value");
    styling
}

/// The settings a drawing would hand to the next one of its type.
pub fn capture_tool_defaults(d: &dyn Drawing) -> DrawingToolDefaults {
    let base = d.base();
    DrawingToolDefaults {
        style: Some(base.style.to_partial()),
        text: base.text.as_ref().map(text_styling),
        props: d.write_props(),
    }
}

/// Start `d` from a tool's remembered settings (keeps its geometry and any typed text).
pub fn apply_tool_defaults(d: &mut dyn Drawing, defaults: Option<&DrawingToolDefaults>) {
    let Some(defaults) = defaults else { return };
    if let Some(style) = &defaults.style {
        let base = d.base_mut();
        base.style = base.style.merged(style);
    }
    if let Some(styling) = &defaults.text {
        let base = d.base_mut();
        let current = base.text.clone().unwrap_or_default();
        let mut merged = text_styling(&current);
        for (key, value) in styling {
            // A null entry clears the field, as an absent one would.
            if value.is_null() {
                merged.remove(key);
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
        merged.insert("value".to_string(), Value::String(current.value.clone()));
        // Styling the text type rejects leaves the block as it was.
        base.text = Some(serde_json::from_value(Value::Object(merged)).unwrap_or(current));
    }
    if let Some(props) = &defaults.props {
        d.apply_props(props);
    }
}

/// Whether two remembered settings are the same.
pub fn same_tool_defaults(
    a: Option<&DrawingToolDefaults>,
    b: Option<&DrawingToolDefaults>,
) -> bool {
    a == b
}

/// Coerce an untrusted value (a persisted document) into remembered
/// settings, or `None`. Only the object shapes are checked here: each drawing
/// type validates its own props when it reads them.
pub fn sanitize_tool_defaults(raw: &Value) -> Option<DrawingToolDefaults> {
    let r = raw.as_object()?;
    let object = |key: &str| r.get(key).and_then(Value::as_object).cloned();
    // A style object with a mistyped field counts as no style at all.
    let style = object("style")
        .and_then(|s| serde_json::from_value::<PartialDrawingStyle>(Value::Object(s)).ok());
    let text = object("text").map(|mut t| {
        t.remove("value");
        t
    });
    let props = object("props");
    if style.is_none() && text.is_none() && props.is_none() {
        return None;
    }
    Some(DrawingToolDefaults { style, text, props })
}

/// A named, saved look for one tool (a "style"): applied, it sets the same
/// settings a remembered default does.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingToolTemplate {
    /// The style's name: its identity among the tool's styles.
    pub name: String,
    /// What a built-in style's name reads as in the chart's language. `None` ⇒ `name`.
    pub label: Option<MessageLike>,
    pub settings: DrawingToolDefaults,
}

/// Longest template name kept (a label, not a note).
const TEMPLATE_NAME_MAX: usize = 60;

/// Coerce an untrusted list into templates: named, de-duplicated by name
/// (last wins, in first-seen position), with valid settings.
pub fn sanitize_tool_templates(raw: &Value) -> Vec<DrawingToolTemplate> {
    let mut out: Vec<DrawingToolTemplate> = Vec::new();
    for entry in raw.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(e) = entry.as_object() else { continue };
        let name: String = e
            .get("name")
            .and_then(Value::as_str)
            .map(|n| n.trim().chars().take(TEMPLATE_NAME_MAX).collect())
            .unwrap_or_default();
        let Some(settings) = e.get("settings").and_then(sanitize_tool_defaults) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let template = DrawingToolTemplate {
            name,
            label: None,
            settings,
        };
        match out.iter_mut().find(|t| t.name == template.name) {
            Some(slot) => *slot = template,
            None => out.push(template),
        }
    }
    out
}

const LINE_TOOLS: [&str; 9] = [
    "trendline",
    "ray",
    "extendedline",
    "hline",
    "hray",
    "vline",
    "crossline",
    "infoline",
    "trendangle",
];
const AREA_TOOLS: [&str; 8] = [
    "box",
    "rotatedrect",
    "ellipse",
    "circle",
    "triangle",
    "parallelchannel",
    "disjointchannel",
    "flattopbottom",
];

fn line_look(
    name: &str,
    label: MessageLike,
    color: &str,
    width: f64,
    line_style: LineStyle,
) -> DrawingToolTemplate {
    let style = PartialDrawingStyle {
        line_color: Some(color.to_string()),
        line_width: Some(width),
        line_style: Some(line_style),
        ..Default::default()
    };
    DrawingToolTemplate {
        name: name.to_string(),
        label: Some(label),
        settings: DrawingToolDefaults {
            style: Some(style),
            text: None,
            props: None,
        },
    }
}

fn area_look(name: &str, label: MessageLike, color: &str) -> DrawingToolTemplate {
    let style = PartialDrawingStyle {
        line_color: Some(color.to_string()),
        fill_color: Some(format!("{color}26")),
        ..Default::default()
    };
    DrawingToolTemplate {
        name: name.to_string(),
        label: Some(label),
        settings: DrawingToolDefaults {
            style: Some(style),
            text: None,
            props: None,
        },
    }
}

/// The looks every chart offers for a tool before the user saves any of their own.
pub fn builtin_tool_templates(type_key: &str) -> Vec<DrawingToolTemplate> {
    if LINE_TOOLS.contains(&type_key) {
        return vec![
            line_look(
                "Support",
                msg("drawing.template.support", "Support"),
                BULLISH,
                2.0,
                LineStyle::Solid,
            ),
            line_look(
                "Resistance",
                msg("drawing.template.resistance", "Resistance"),
                BEARISH,
                2.0,
                LineStyle::Solid,
            ),
            line_look(
                "Projection",
                msg("drawing.template.projection", "Projection"),
                NEUTRAL,
                1.0,
                LineStyle::Dashed,
            ),
        ];
    }
    if AREA_TOOLS.contains(&type_key) {
        return vec![
            area_look("Demand", msg("drawing.template.demand", "Demand"), BULLISH),
            area_look("Supply", msg("drawing.template.supply", "Supply"), BEARISH),
        ];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sanitizing_drops_the_words_and_empty_shapes() {
        let d = sanitize_tool_defaults(&json!({ "style": { "lineColor": "#fff" }, "text": { "value": "secret", "bold": true }, "props": { "extend": "right" } })).unwrap();
        assert_eq!(d.style.unwrap().line_color.as_deref(), Some("#fff"));
        assert_eq!(Value::Object(d.text.unwrap()), json!({ "bold": true }));
        assert_eq!(
            Value::Object(d.props.unwrap()),
            json!({ "extend": "right" })
        );
        assert_eq!(sanitize_tool_defaults(&json!({})), None);
        assert_eq!(sanitize_tool_defaults(&json!([1])), None);
        assert_eq!(sanitize_tool_defaults(&json!({ "style": 5 })), None);
    }

    #[test]
    fn templates_dedupe_by_name_with_the_last_winning_in_place() {
        let raw = json!([
            { "name": " A ", "settings": { "style": { "lineWidth": 1 } } },
            { "name": "B", "settings": { "style": { "lineWidth": 2 } } },
            { "name": "A", "settings": { "style": { "lineWidth": 3 } } },
            { "name": "", "settings": { "style": { "lineWidth": 4 } } },
            { "name": "C", "settings": {} },
            7
        ]);
        let templates = sanitize_tool_templates(&raw);
        assert_eq!(
            templates
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert_eq!(
            templates[0].settings.style.as_ref().unwrap().line_width,
            Some(3.0)
        );
    }

    #[test]
    fn builtin_looks_follow_the_tool_family() {
        assert_eq!(builtin_tool_templates("trendline").len(), 3);
        let area = builtin_tool_templates("box");
        assert_eq!(
            area[0]
                .settings
                .style
                .as_ref()
                .unwrap()
                .fill_color
                .as_deref(),
            Some("#08998126")
        );
        assert!(builtin_tool_templates("fibretracement").is_empty());
    }
}
