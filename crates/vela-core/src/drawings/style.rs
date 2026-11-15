//! The cosmetic payload shared by every drawing: the settings a user edits.
//! The serde shapes match the original's persisted JSON field for field.

use serde::{Deserialize, Serialize};

use crate::model::{BoxHAlign, BoxTextSize, BoxVAlign, LineStyle};
use crate::palette::ACCENT_BRIGHT;

/// The default drawing accent: the Vela logo blue.
pub const DEFAULT_DRAWING_COLOR: &str = ACCENT_BRIGHT;

fn default_line_color() -> String {
    DEFAULT_DRAWING_COLOR.to_string()
}

fn default_line_width() -> f64 {
    2.0
}

/// A drawing's line and fill cosmetics.
///
/// Reading is lenient: a missing line field takes the neutral default, so a
/// partial style from an older or hand-written document still loads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingStyle {
    #[serde(default = "default_line_color")]
    pub line_color: String,
    #[serde(default = "default_line_width")]
    pub line_width: f64,
    #[serde(default)]
    pub line_style: LineStyle,
    /// Fill (box / closed path); `None` ⇒ no fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    /// Fill opacity 0..1 (applied over `fill_color`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_left: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_right: Option<bool>,
    /// Run the line past its first anchor to the chart edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_left: Option<bool>,
    /// Run the line past its second anchor to the chart edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_right: Option<bool>,
}

impl Default for DrawingStyle {
    /// A neutral default style; concrete tools override it through their type's default.
    fn default() -> Self {
        Self::line(DEFAULT_DRAWING_COLOR, 2.0, LineStyle::Solid)
    }
}

impl DrawingStyle {
    /// A style with only the line fields set.
    pub fn line(color: impl Into<String>, width: f64, line_style: LineStyle) -> Self {
        Self {
            line_color: color.into(),
            line_width: width,
            line_style,
            fill_color: None,
            fill_opacity: None,
            arrow_left: None,
            arrow_right: None,
            extend_left: None,
            extend_right: None,
        }
    }

    /// The same style with a fill color.
    pub fn fill(mut self, color: impl Into<String>) -> Self {
        self.fill_color = Some(color.into());
        self
    }

    /// Overlay a partial style: every field `over` sets replaces this one's
    /// (the original's `{ ...base, ...over }`).
    pub fn merged(&self, over: &PartialDrawingStyle) -> Self {
        Self {
            line_color: over
                .line_color
                .clone()
                .unwrap_or_else(|| self.line_color.clone()),
            line_width: over.line_width.unwrap_or(self.line_width),
            line_style: over.line_style.unwrap_or(self.line_style),
            fill_color: over.fill_color.clone().or_else(|| self.fill_color.clone()),
            fill_opacity: over.fill_opacity.or(self.fill_opacity),
            arrow_left: over.arrow_left.or(self.arrow_left),
            arrow_right: over.arrow_right.or(self.arrow_right),
            extend_left: over.extend_left.or(self.extend_left),
            extend_right: over.extend_right.or(self.extend_right),
        }
    }

    /// This style as a partial with every set field present.
    pub fn to_partial(&self) -> PartialDrawingStyle {
        PartialDrawingStyle {
            line_color: Some(self.line_color.clone()),
            line_width: Some(self.line_width),
            line_style: Some(self.line_style),
            fill_color: self.fill_color.clone(),
            fill_opacity: self.fill_opacity,
            arrow_left: self.arrow_left,
            arrow_right: self.arrow_right,
            extend_left: self.extend_left,
            extend_right: self.extend_right,
        }
    }
}

/// A style where every field is optional: a caller's override on a type's
/// default, a remembered tool default, a template.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialDrawingStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_style: Option<LineStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_left: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_right: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_left: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_right: Option<bool>,
}

/// Where a line's label sits along it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextPlace {
    Start,
    Middle,
    End,
}

/// Which side of the line its label sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSide {
    Above,
    Below,
}

fn default_text_size() -> BoxTextSize {
    BoxTextSize::Normal
}

fn default_h_align() -> BoxHAlign {
    BoxHAlign::Left
}

fn default_v_align() -> BoxVAlign {
    BoxVAlign::Top
}

/// A drawing's editable text/annotation block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingText {
    #[serde(default)]
    pub value: String,
    /// `None` ⇒ auto-contrast against the fill/background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default = "default_text_size")]
    pub size: BoxTextSize,
    #[serde(default = "default_h_align")]
    pub h_align: BoxHAlign,
    #[serde(default = "default_v_align")]
    pub v_align: BoxVAlign,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// Where a line's label sits along it (line tools); `None` = the middle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<TextPlace>,
    /// Which side of the line its label sits on (line tools); `None` = above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<TextSide>,
}

impl DrawingText {
    /// A neutral text block holding `value` (used when a drawing first gains text).
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            color: None,
            size: BoxTextSize::Normal,
            h_align: BoxHAlign::Left,
            v_align: BoxVAlign::Top,
            bold: None,
            italic: None,
            place: None,
            side: None,
        }
    }
}

impl Default for DrawingText {
    fn default() -> Self {
        Self::new("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_json_matches_the_original_field_names() {
        let style = DrawingStyle::line("#fff", 1.0, LineStyle::Dashed).fill("#00000026");
        let json = serde_json::to_value(&style).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "lineColor": "#fff", "lineWidth": 1.0, "lineStyle": "dashed", "fillColor": "#00000026" })
        );
        assert_eq!(serde_json::from_value::<DrawingStyle>(json).unwrap(), style);
    }

    #[test]
    fn a_partial_style_document_loads_with_neutral_defaults() {
        let style: DrawingStyle =
            serde_json::from_value(serde_json::json!({ "extendRight": true })).unwrap();
        assert_eq!(style.line_color, DEFAULT_DRAWING_COLOR);
        assert_eq!(style.line_width, 2.0);
        assert_eq!(style.extend_right, Some(true));
    }

    #[test]
    fn merging_keeps_what_the_override_leaves_out() {
        let base = DrawingStyle::line("#111", 2.0, LineStyle::Solid).fill("#222");
        let over = PartialDrawingStyle {
            line_color: Some("#333".into()),
            ..Default::default()
        };
        let merged = base.merged(&over);
        assert_eq!(merged.line_color, "#333");
        assert_eq!(merged.fill_color.as_deref(), Some("#222"));
    }

    #[test]
    fn text_json_uses_camel_case_alignment_keys() {
        let text = DrawingText::new("Note");
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "value": "Note", "size": "normal", "hAlign": "left", "vAlign": "top" })
        );
    }
}
