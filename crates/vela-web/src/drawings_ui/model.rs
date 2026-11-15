//! The pure logic behind the settings popup and panel: what a drawing looks like to the
//! controls ([`Snap`]), which controls several drawings share, how fields pair up into rows,
//! what a style template matches, and the small tables the original keeps beside its DOM
//! code (`DrawingSettingsPopup.ts`, `DrawingSettingsPanel.ts`).
//!
//! No DOM in here: every function reads plain data, so the rules are unit-tested natively.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use vela_core::drawings::effective_color::{EffectiveColorTheme, effective_fill_color};
use vela_core::drawings::types::mach::MachFigure;
use vela_core::drawings::types::measure::{PositionLevel, PositionLevelMode, PositionTool};
use vela_core::drawings::{
    Drawing, DrawingToolDefaults, EditableLevel, FreeAxis, SerializedDrawing, SettingsSchema,
};
use vela_core::i18n::MessageLike;

/// The tools whose label can slide along the line and sit under it.
pub const LINE_LABEL_TYPES: [&str; 5] =
    ["trendline", "ray", "extendedline", "infoline", "trendangle"];
/// Tools whose points are not worth listing: a free-hand stroke, a line with no price, or a
/// position (its own section edits entry, stop and target).
pub const NO_POINTS: [&str; 4] = ["freehand", "highlighter", "vline", "position"];
/// A drawing with more points than this lists none.
pub const MAX_POINTS: usize = 8;
/// Paths the position section owns, kept out of the generic rows.
pub const POSITION_PATHS: [&str; 13] = [
    "riskPercent",
    "accountBalance",
    "quantity",
    "direction",
    "entryPrice",
    "stopPrice",
    "targetPrice",
    "showText",
    "showHeader",
    "showLossSize",
    "showTargetLabel",
    "showStopLabel",
    "showPrices",
];
/// Paths the level block owns.
pub const LEVEL_PATHS: [&str; 1] = ["showRatios"];
/// "Show on" shortcuts, as timeframe bands.
pub const INTRADAY: [&str; 6] = ["s", "1", "5", "15", "60", "240"];
pub const HIGHER: [&str; 5] = ["60", "240", "D", "W", "M"];

/// The control value of several drawings that may disagree.
#[derive(Clone, Debug, PartialEq)]
pub enum Common<T> {
    One(T),
    Mixed,
}

impl<T> Common<T> {
    pub fn is_mixed(&self) -> bool {
        matches!(self, Common::Mixed)
    }
}

/// What the position section needs from a position tool (its own computed values).
#[derive(Clone, Debug, PartialEq)]
pub struct PositionSnap {
    pub header: String,
    pub loss_size: String,
    pub stop_price: f64,
    pub stop_points: f64,
    pub target_price: f64,
    pub target_points: f64,
}

/// One drawing as the controls read it: its document, the schema, the levels and anchor
/// slots, and the few values only the live instance can compute.
#[derive(Clone, Debug)]
pub struct Snap {
    pub doc: SerializedDrawing,
    /// `doc` as plain JSON (the shape the settings paths walk).
    pub json: Value,
    pub schema: SettingsSchema,
    pub levels: Option<Vec<EditableLevel>>,
    /// Per anchor slot: which axes the anchor moves along.
    pub slots: Vec<FreeAxis>,
    /// The color the body is actually filled with (what the Fill swatch shows).
    pub fill: Option<String>,
    /// Values a path reads that the props bag does not hold (a position's entry, stop...).
    pub extra: Map<String, Value>,
    pub is_mach: bool,
    pub position: Option<PositionSnap>,
}

impl Snap {
    /// Read `d`; `background` is the chart's chrome background (the callout's fill fallback).
    pub fn of(d: &dyn Drawing, background: &str) -> Snap {
        let doc = d.serialize();
        let json = serde_json::to_value(&doc).unwrap_or(Value::Null);
        let theme = EffectiveColorTheme {
            background: background.to_string(),
        };
        let mut extra = Map::new();
        let position = d.as_any().downcast_ref::<PositionTool>().map(|p| {
            extra.insert(
                "direction".into(),
                json!(if p.direction_label() == "LONG" {
                    "long"
                } else {
                    "short"
                }),
            );
            extra.insert("quantity".into(), json!(p.quantity()));
            extra.insert("entryPrice".into(), json!(p.entry_price()));
            extra.insert("stopPrice".into(), json!(p.stop_price()));
            extra.insert("targetPrice".into(), json!(p.target_price()));
            PositionSnap {
                header: p.header_label(),
                loss_size: p.loss_size_label(),
                stop_price: p.level_display_value(PositionLevel::Stop, PositionLevelMode::Price),
                stop_points: p.level_display_value(PositionLevel::Stop, PositionLevelMode::Points),
                target_price: p
                    .level_display_value(PositionLevel::Target, PositionLevelMode::Price),
                target_points: p
                    .level_display_value(PositionLevel::Target, PositionLevelMode::Points),
            }
        });
        let schema = d.anchor_schema();
        let slots = schema.slots.iter().map(|s| s.free).collect();
        Snap {
            json,
            doc,
            schema: d.schema(),
            levels: d.editable_levels(),
            slots,
            fill: effective_fill_color(d, &theme),
            extra,
            is_mach: d.as_any().is::<MachFigure>(),
            position,
        }
    }

    pub fn type_key(&self) -> &str {
        &self.doc.type_key
    }

    /// The setting at a dotted path, as the original reads it off the instance: the shared
    /// fields of the document, the computed ones, then the type's props. A path a type
    /// prefixes (`reg.midColor`, `box.midline`, `magnifier.timeframe`) is stored flat in
    /// the props under its last segment.
    pub fn read(&self, path: &str) -> Option<Value> {
        if let Some(v) = self.extra.get(path) {
            return Some(v.clone());
        }
        let root = path.split('.').next().unwrap_or(path);
        if matches!(
            root,
            "style" | "text" | "locked" | "visible" | "showOn" | "anchors" | "zIndex" | "paneId"
        ) {
            return get_path(&self.json, path).filter(|v| !v.is_null()).cloned();
        }
        let props = self.json.get("props")?;
        if let Some(v) = get_path(props, path).filter(|v| !v.is_null()) {
            return Some(v.clone());
        }
        let (_, flat) = path.split_once('.')?;
        get_path(props, flat).filter(|v| !v.is_null()).cloned()
    }

    pub fn read_str(&self, path: &str) -> Option<String> {
        self.read(path).and_then(|v| v.as_str().map(str::to_string))
    }

    pub fn read_bool(&self, path: &str) -> Option<bool> {
        self.read(path).and_then(|v| v.as_bool())
    }

    pub fn read_f64(&self, path: &str) -> Option<f64> {
        self.read(path).and_then(|v| v.as_f64())
    }

    pub fn has_field(&self, path: &str) -> bool {
        self.schema.fields.iter().any(|f| f.path == path)
    }

    /// A numeric props entry (`numbersSize` style check).
    pub fn has_prop(&self, key: &str) -> bool {
        self.json
            .get("props")
            .and_then(|p| p.get(key))
            .is_some_and(|v| !v.is_null())
    }

    pub fn show_on(&self) -> Option<Vec<String>> {
        self.doc.show_on.clone()
    }

    /// The anchor slot `i` is free along (`both` past the last slot, like the original).
    pub fn free(&self, i: usize) -> FreeAxis {
        self.slots
            .get(i)
            .or_else(|| self.slots.last())
            .copied()
            .unwrap_or(FreeAxis::Both)
    }
}

/// Walk a dotted path through objects and arrays.
pub fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for key in path.split('.') {
        cur = match cur {
            Value::Object(map) => map.get(key)?,
            Value::Array(items) => items.get(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// JSON equality that reads `1` and `1.0` the same.
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// The settings paths every one of `snaps` supports: what a multi-selection bar can edit.
pub fn shared_paths(snaps: &[Snap]) -> BTreeSet<String> {
    let mut iter = snaps.iter();
    let Some(first) = iter.next() else {
        return BTreeSet::new();
    };
    let mut paths: BTreeSet<String> = first.schema.fields.iter().map(|f| f.path.clone()).collect();
    for s in iter {
        let own: BTreeSet<&str> = s.schema.fields.iter().map(|f| f.path.as_str()).collect();
        paths.retain(|p| own.contains(p.as_str()));
    }
    paths
}

/// One value read off every snap: the shared value when they all agree, else mixed.
pub fn common<T: PartialEq>(snaps: &[Snap], read: impl Fn(&Snap) -> T) -> Common<T> {
    let mut iter = snaps.iter();
    let Some(first) = iter.next() else {
        return Common::Mixed;
    };
    let value = read(first);
    if iter.all(|s| read(s) == value) {
        Common::One(value)
    } else {
        Common::Mixed
    }
}

/// The distinct values a mixed control spans, first-seen order (the "in use" shortcuts).
pub fn distinct<T: PartialEq + Clone>(snaps: &[Snap], read: impl Fn(&Snap) -> T) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for s in snaps {
        let v = read(s);
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// A striped underline for a mixed color swatch: equal bands of every color in use (a
/// neutral gray pair when nothing concrete is known).
pub fn stripes(colors: &[String]) -> String {
    let fallback = [
        "var(--vela-fg-faint, #787b86)".to_string(),
        "var(--vela-fg-muted, #b2b5be)".to_string(),
    ];
    let bands: &[String] = if colors.len() > 1 { colors } else { &fallback };
    let step = 100.0 / bands.len() as f64;
    let stops: Vec<String> = bands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{c} {}% {}%",
                trim_num(i as f64 * step),
                trim_num((i + 1) as f64 * step)
            )
        })
        .collect();
    format!("linear-gradient(90deg,{})", stops.join(","))
}

fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{}", (v * 1000.0).round() / 1000.0)
    }
}

/// `small` to `Small`.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The plain English text of a message-like that is a bare string (an older schema's label).
pub fn plain_text(m: &MessageLike) -> Option<String> {
    match m {
        MessageLike::Text(t) => Some(t.to_string()),
        MessageLike::Message(_) => None,
    }
}

fn strip_suffix_ci<'a>(s: &'a str, suffixes: &[&str]) -> Option<&'a str> {
    let lower = s.to_lowercase();
    for suffix in suffixes {
        if lower.ends_with(suffix) && s.is_char_boundary(s.len() - suffix.len()) {
            return Some(&s[..s.len() - suffix.len()]);
        }
    }
    None
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let lower = s.to_lowercase();
    if lower.starts_with(prefix) && s.is_char_boundary(prefix.len()) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// The shared name of a color field and its line-style twin ("Midline color" + "Midline
/// style" gives "Midline"), or `None` when they do not pair up.
pub fn pair_base(color_label: &str, style_label: &str) -> Option<String> {
    let c = strip_suffix_ci(color_label, &[" color", " colour"])?;
    let st = strip_suffix_ci(style_label, &[" style"])?;
    (c == st).then(|| c.to_string())
}

/// The shared name of an upper / lower color pair ("Upper band color" + "Lower band color"
/// gives "Bands", "Upper fill" + "Lower fill" gives "Fill"), or `None`.
pub fn upper_lower_base(upper: &str, lower: &str) -> Option<String> {
    let u = strip_prefix_ci(upper, "upper ").filter(|s| !s.is_empty())?;
    let l = strip_prefix_ci(lower, "lower ").filter(|s| !s.is_empty())?;
    if u != l {
        return None;
    }
    let name = strip_suffix_ci(u, &[" color", " colour"]).unwrap_or(u);
    let word = capitalize(name);
    Some(if word.to_lowercase().ends_with("fill") {
        word
    } else {
        format!("{word}s")
    })
}

/// How the "Show on" fold names a band list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowMode {
    All,
    Intraday,
    Higher,
    Custom,
}

impl ShowMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ShowMode::All => "all",
            ShowMode::Intraday => "intraday",
            ShowMode::Higher => "higher",
            ShowMode::Custom => "custom",
        }
    }
}

/// The mode a band list reads as.
pub fn mode_of(show_on: Option<&[String]>) -> ShowMode {
    let Some(show_on) = show_on else {
        return ShowMode::All;
    };
    let same = |preset: &[&str]| {
        preset.len() == show_on.len() && preset.iter().all(|k| show_on.iter().any(|s| s == k))
    };
    if same(&INTRADAY) {
        ShowMode::Intraday
    } else if same(&HIGHER) {
        ShowMode::Higher
    } else {
        ShowMode::Custom
    }
}

/// Whether a drawing already wears a style: every setting the style names matches.
pub fn matches_template(snap: &Snap, settings: &DrawingToolDefaults) -> bool {
    let style = serde_json::to_value(settings.style.clone().unwrap_or_default())
        .ok()
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    let doc_style = snap.json.get("style");
    for (k, v) in &style {
        let have = doc_style.and_then(|s| s.get(k)).unwrap_or(&Value::Null);
        if !loose_eq(have, v) {
            return false;
        }
    }
    let text = settings.text.clone().unwrap_or_default();
    let doc_text = snap.json.get("text");
    for (k, v) in &text {
        let have = doc_text.and_then(|s| s.get(k)).unwrap_or(&Value::Null);
        if !loose_eq(have, v) {
            return false;
        }
    }
    let props = settings.props.clone().unwrap_or_default();
    let doc_props = snap.json.get("props");
    for (k, v) in &props {
        let have = doc_props.and_then(|s| s.get(k)).unwrap_or(&Value::Null);
        if !loose_eq(have, v) {
            return false;
        }
    }
    !style.is_empty() || !text.is_empty() || !props.is_empty()
}

/// The color a style is best recognized by, for its menu swatch.
pub fn template_color(settings: &DrawingToolDefaults) -> Option<String> {
    settings
        .style
        .as_ref()
        .and_then(|s| s.line_color.clone().or_else(|| s.fill_color.clone()))
        .or_else(|| {
            settings
                .text
                .as_ref()
                .and_then(|t| t.get("color"))
                .and_then(|c| c.as_str().map(str::to_string))
        })
}

/// The extend segment's value for a pair of flags.
pub fn extend_value(left: bool, right: bool) -> &'static str {
    match (left, right) {
        (true, true) => "both",
        (true, false) => "left",
        (false, true) => "right",
        _ => "none",
    }
}

/// The price as shown: to the symbol's increment, or a sensible precision for its size.
pub fn shown_price(n: f64, decimals: Option<f64>) -> f64 {
    let dec = decimals.unwrap_or_else(|| {
        let by_size: f64 = match n.abs() {
            a if a >= 100.0 => 0.0,
            a if a >= 1.0 => 2.0,
            a if a >= 0.01 => 4.0,
            _ => 6.0,
        };
        by_size.max(2.0)
    });
    let factor = 10f64.powi(dec.clamp(0.0, 10.0) as i32);
    (n * factor).round() / factor
}

/// A number as the controls print it (`3`, `0.236`).
pub fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// `round(n * 1e8) / 1e8`, the position section's display rounding.
pub fn round8(n: f64) -> f64 {
    (n * 1e8).round() / 1e8
}

/// Whether `gate` (`when`) holds for the current value.
pub fn gate_holds(current: Option<&Value>, any_of: &[Value]) -> bool {
    let current = current.unwrap_or(&Value::Null);
    any_of.iter().any(|v| loose_eq(v, current))
}

/// The key of a text size (`small`, `normal`, ...).
pub fn size_key(size: &vela_core::model::drawings::BoxTextSize) -> String {
    serde_json::to_value(size)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "normal".to_string())
}

// ── placement ──

/// A box in wrapper px: x, y, width, height.
pub type Rect = (f64, f64, f64, f64);

/// Space kept between a floating box and the chart's edges, px.
pub const EDGE: f64 = 4.0;

/// `(x, y)` kept inside a host of `host` px for a box of `size` (an unknown host, 0, clamps
/// nothing but the near edges).
pub fn clamp_into(x: f64, y: f64, size: (f64, f64), host: (f64, f64)) -> (f64, f64) {
    let max_x = if host.0 > 0.0 {
        host.0 - size.0 - EDGE
    } else {
        f64::MAX
    };
    let max_y = if host.1 > 0.0 {
        host.1 - size.1 - EDGE
    } else {
        f64::MAX
    };
    (x.min(max_x).max(EDGE), y.min(max_y).max(EDGE))
}

/// Where the quick bar first sits: centered on the drawing's box and clear of it (above,
/// else below when there is no room above), or top-center with no box; kept in the chart.
pub fn bar_origin(
    anchor: Option<Rect>,
    size: (f64, f64),
    host: (f64, f64),
    gap: f64,
) -> (f64, f64) {
    let (left, top) = match anchor {
        Some((ax, ay, aw, ah)) => {
            let above = ay - size.1 - gap;
            (
                ax + aw / 2.0 - size.0 / 2.0,
                if above >= EDGE { above } else { ay + ah + gap },
            )
        }
        None => (
            if host.0 > 0.0 {
                host.0 / 2.0 - size.0 / 2.0
            } else {
                8.0
            },
            12.0,
        ),
    };
    clamp_into(left, top, size, host)
}

/// Where a floating shell of `size` opens on `anchor`: below it (above when the chart's
/// bottom is too near and there is room above), its start or end edge in line with the
/// anchor's, kept in the chart.
pub fn float_origin(
    anchor: Rect,
    size: (f64, f64),
    host: (f64, f64),
    align_end: bool,
) -> (f64, f64) {
    let (ax, ay, aw, ah) = anchor;
    let mut left = if align_end { ax + aw - size.0 } else { ax };
    if host.0 > 0.0 {
        left = left.min(host.0 - size.0 - EDGE);
    }
    left = left.max(EDGE);
    let below = ay + ah + 6.0;
    let top = if host.1 > 0.0 && below + size.1 > host.1 - EDGE && ay - 6.0 - size.1 >= EDGE {
        ay - 6.0 - size.1
    } else {
        below
    };
    (left, top.max(EDGE))
}

/// The least height (px) the panel takes beside the bar; with less it sits over it.
pub const MIN_ROOM: f64 = 200.0;

/// Where the settings panel hangs off the quick bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beside {
    pub left: f64,
    /// Set when the panel opens below the bar (or sits over it).
    pub top: Option<f64>,
    /// Set when the panel opens above the bar.
    pub bottom: Option<f64>,
    /// The height the panel is capped to (it scrolls inside), when it is beside the bar.
    pub max_height: Option<f64>,
}

/// Hang the panel off the quick bar, like a menu from its gear: on the bar's side away from
/// the drawing when it fits there, else the side with more room (it scrolls inside), its
/// left edge in line with the bar's. With too little room either side (a short chart) it sits
/// over the bar, kept in view.
pub fn beside(bar: Rect, drawing: Option<Rect>, host: (f64, f64), panel: (f64, f64)) -> Beside {
    let (gap, edge) = (8.0, 12.0);
    let (w_host, h_host) = host;
    let (w, h) = panel;
    let room_above = bar.1 - gap - edge;
    let room_below = h_host - (bar.1 + bar.3) - gap - edge;
    let away_above = drawing.is_some_and(|d| d.1 + d.3 / 2.0 > bar.1 + bar.3 / 2.0);
    let order = if away_above {
        [true, false]
    } else {
        [false, true]
    };
    let room = |above: bool| if above { room_above } else { room_below };
    let above = order
        .iter()
        .copied()
        .find(|&a| room(a) >= h)
        .unwrap_or(room_above > room_below);
    let left = bar.0.min(w_host - w - EDGE).max(EDGE);
    if room(above) < MIN_ROOM {
        return Beside {
            left,
            top: Some(bar.1.min(h_host - h - edge).max(edge)),
            bottom: None,
            max_height: None,
        };
    }
    if above {
        Beside {
            left,
            top: None,
            bottom: Some(h_host - bar.1 + gap),
            max_height: Some(room(above)),
        }
    } else {
        Beside {
            left,
            top: Some(bar.1 + bar.3 + gap),
            bottom: None,
            max_height: Some(room(above)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use vela_core::drawings::{DrawingInit, DrawingPoint, DrawingRegistry, DrawingText, FieldKind};

    fn pt(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    fn make(type_key: &str) -> Box<dyn Drawing> {
        let registry = DrawingRegistry::builtin();
        let n = registry.get(type_key).map_or(2, |_| 3).max(2);
        let init = DrawingInit {
            pane_id: "main".into(),
            anchors: (0..n)
                .map(|i| {
                    pt(
                        1_700_000_000_000.0 + i as f64 * 3_600_000.0,
                        100.0 + i as f64,
                    )
                })
                .collect(),
            ..Default::default()
        };
        registry.create(type_key, init).expect(type_key)
    }

    fn snap(type_key: &str) -> Snap {
        Snap::of(make(type_key).as_ref(), "#151619")
    }

    #[test]
    fn pairs_a_color_with_its_style_twin() {
        assert_eq!(
            pair_base("Midline color", "Midline style").as_deref(),
            Some("Midline")
        );
        assert_eq!(
            pair_base("Upper line colour", "Upper line style").as_deref(),
            Some("Upper line")
        );
        assert_eq!(pair_base("Midline color", "Upper line style"), None);
        assert_eq!(pair_base("Midline", "Midline style"), None);
    }

    #[test]
    fn pairs_an_upper_and_a_lower_color() {
        assert_eq!(
            upper_lower_base("Upper band color", "Lower band color").as_deref(),
            Some("Bands")
        );
        assert_eq!(
            upper_lower_base("Upper fill", "Lower fill").as_deref(),
            Some("Fill")
        );
        assert_eq!(upper_lower_base("Upper fill", "Lower band color"), None);
        assert_eq!(upper_lower_base("Top", "Bottom"), None);
    }

    #[test]
    fn show_modes_follow_the_band_list() {
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(mode_of(None), ShowMode::All);
        assert_eq!(mode_of(Some(&own(&INTRADAY))), ShowMode::Intraday);
        assert_eq!(
            mode_of(Some(&own(&["240", "60", "D", "W", "M"]))),
            ShowMode::Higher
        );
        assert_eq!(mode_of(Some(&own(&["D"]))), ShowMode::Custom);
        assert_eq!(mode_of(Some(&[])), ShowMode::Custom);
    }

    #[test]
    fn a_mixed_control_unifies_on_the_first_edit() {
        let a = snap("trendline");
        let mut b = snap("trendline");
        assert_eq!(
            common(&[a.clone(), b.clone()], |s| s.doc.style.line_width as i64),
            Common::One(2)
        );
        b.doc.style.line_width = 4.0;
        let both = [a, b];
        assert_eq!(
            common(&both, |s| s.doc.style.line_width as i64),
            Common::Mixed
        );
        assert_eq!(
            distinct(&both, |s| s.doc.style.line_width as i64),
            vec![2, 4]
        );
    }

    #[test]
    fn a_multi_selection_shares_only_common_paths() {
        let line = snap("trendline");
        let fib = snap("fibretracement");
        let ray = snap("ray");
        let two_lines = shared_paths(&[line.clone(), ray]);
        assert!(two_lines.contains("style.lineColor") && two_lines.contains("text.value"));
        let shared = shared_paths(&[line.clone(), fib.clone()]);
        assert!(shared.iter().all(|p| fib.has_field(p) && line.has_field(p)));
        assert!(!shared.contains("showRatios"));
        assert_eq!(shared_paths(&[]), BTreeSet::new());
        let own: BTreeSet<String> = line.schema.fields.iter().map(|f| f.path.clone()).collect();
        assert_eq!(shared_paths(&[line.clone(), line]), own);
    }

    #[test]
    fn stripes_split_the_bar_evenly() {
        let s = stripes(&["#f00".into(), "#0f0".into(), "#00f".into(), "#fff".into()]);
        assert_eq!(
            s,
            "linear-gradient(90deg,#f00 0% 25%,#0f0 25% 50%,#00f 50% 75%,#fff 75% 100%)"
        );
        assert!(stripes(&["#f00".into()]).contains("var(--vela-fg-faint"));
        assert!(stripes(&["#f00".into(), "#0f0".into(), "#00f".into()]).contains("33.333%"));
    }

    #[test]
    fn a_style_matches_when_every_setting_it_names_does() {
        let mut s = snap("trendline");
        let defaults = DrawingToolDefaults {
            style: Some(vela_core::drawings::PartialDrawingStyle {
                line_color: Some(s.doc.style.line_color.clone()),
                line_width: Some(s.doc.style.line_width),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches_template(&s, &defaults));
        s.doc.style.line_width = 9.0;
        s.json = serde_json::to_value(&s.doc).unwrap();
        assert!(!matches_template(&s, &defaults));
        assert!(!matches_template(&s, &DrawingToolDefaults::default()));
        assert_eq!(
            template_color(&defaults),
            Some(s.doc.style.line_color.clone())
        );
    }

    #[test]
    fn a_text_style_compares_the_text_block() {
        let mut s = snap("trendline");
        s.doc.text = Some(DrawingText {
            bold: Some(true),
            ..DrawingText::default()
        });
        s.json = serde_json::to_value(&s.doc).unwrap();
        let mut text = Map::new();
        text.insert("bold".into(), json!(true));
        let t = DrawingToolDefaults {
            text: Some(text),
            ..Default::default()
        };
        assert!(matches_template(&s, &t));
        let mut text = Map::new();
        text.insert("bold".into(), json!(false));
        let f = DrawingToolDefaults {
            text: Some(text.clone()),
            ..Default::default()
        };
        assert!(!matches_template(&s, &f));
        assert_eq!(template_color(&f), None);
    }

    #[test]
    fn prices_show_to_the_symbol_increment() {
        assert_eq!(shown_price(123.456789, Some(2.0)), 123.46);
        assert_eq!(shown_price(123.456789, None), 123.46);
        assert_eq!(shown_price(0.123456789, None), 0.1235);
        assert_eq!(shown_price(0.00001234, None), 0.000012);
        assert_eq!(round8(1.234_567_891_23), 1.23456789);
    }

    #[test]
    fn extend_reads_two_flags_as_one_value() {
        assert_eq!(extend_value(false, false), "none");
        assert_eq!(extend_value(true, false), "left");
        assert_eq!(extend_value(false, true), "right");
        assert_eq!(extend_value(true, true), "both");
    }

    #[test]
    fn a_gate_holds_for_a_listed_value() {
        assert!(gate_holds(
            Some(&json!("tested")),
            &[json!("tested"), json!("half")]
        ));
        assert!(!gate_holds(Some(&json!("none")), &[json!("tested")]));
        assert!(!gate_holds(None, &[json!("tested")]));
        assert!(gate_holds(Some(&json!(1)), &[json!(1.0)]));
    }

    #[test]
    fn capitalize_and_plain_text() {
        assert_eq!(capitalize("small"), "Small");
        assert_eq!(capitalize(""), "");
        assert_eq!(plain_text(&MessageLike::from("Hi")).as_deref(), Some("Hi"));
        assert_eq!(plain_text(&vela_core::i18n::msg("a.b", "Hi")), None);
    }

    /// Every settings field of every built-in tool reads back a value (or is an optional
    /// one the model leaves unset), so no control starts from a blind path.
    #[test]
    fn every_schema_path_of_every_tool_is_readable() {
        let registry = DrawingRegistry::builtin();
        let optional = [
            "style.fillColor",
            "style.arrowLeft",
            "style.arrowRight",
            "style.extendLeft",
            "style.extendRight",
            "text.color",
            "text.bold",
            "text.italic",
            "text.value",
            "text.size",
            "box.midColor",
            "frvp.pocColor",
            "text.place",
            "text.side",
        ];
        let mut blind: Vec<String> = Vec::new();
        for meta in registry.entries() {
            let s = snap(meta.type_key);
            for f in &s.schema.fields {
                if s.read(&f.path).is_none() && !optional.contains(&f.path.as_str()) {
                    blind.push(format!("{}:{}", meta.type_key, f.path));
                }
            }
        }
        assert!(blind.is_empty(), "unreadable: {blind:?}");
    }

    #[test]
    fn a_position_reads_its_computed_values() {
        let s = snap("position");
        let p = s.position.as_ref().expect("a position snap");
        assert!(s.read("entryPrice").is_some());
        assert!(matches!(
            s.read_str("direction").as_deref(),
            Some("long" | "short")
        ));
        assert!(p.header.contains("R:R"));
        assert!(p.loss_size.starts_with("Loss"));
        assert!(s.read_f64("riskPercent").is_some());
        assert!(s.position.is_some() && !s.is_mach);
    }

    #[test]
    fn a_retracement_lists_levels_and_a_text_tool_is_content() {
        let fib = snap("fibretracement");
        assert!(fib.levels.as_ref().is_some_and(|l| l.len() > 10));
        assert!(fib.has_prop("numbersSize") && fib.has_prop("labelSide"));
        assert_eq!(fib.read_str("labelSide").as_deref(), Some("left"));
        assert!(fib.read("levels.0.ratio").is_some());
        let text = snap("text");
        assert!(text.schema.text_is_content);
        assert!(text.has_field("text.value"));
        assert_eq!(text.type_key(), "text");
    }

    #[test]
    fn anchor_slots_fall_back_to_the_last_one() {
        let s = snap("trendline");
        assert_eq!(s.free(0), FreeAxis::Both);
        assert_eq!(s.free(9), s.free(1));
        let hline = snap("hline");
        assert_ne!(hline.free(0), FreeAxis::Both);
    }

    /// Every `("drawing.x.y", "English")` pair the popup, the panel and their sections pass to
    /// the translator, as written in their sources.
    fn used_messages() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for file in [
            "popup.rs",
            "panel.rs",
            "sections.rs",
            "kit.rs",
            "session.rs",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("drawings_ui")
                .join(file);
            let src = std::fs::read_to_string(&path).expect("source");
            let mut rest = src.as_str();
            while let Some(at) = rest.find("\"drawing.").or_else(|| rest.find("\"chart.")) {
                let after = &rest[at + 1..];
                let Some(end) = after.find('"') else { break };
                let id = after[..end].to_string();
                let tail = &after[end + 1..];
                rest = tail;
                let tail = tail.trim_start();
                let Some(tail) = tail.strip_prefix(',') else {
                    continue;
                };
                let Some(tail) = tail.trim_start().strip_prefix('"') else {
                    continue;
                };
                let mut english = String::new();
                let mut chars = tail.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('"') => english.push('"'),
                            Some('\\') => english.push('\\'),
                            Some('u') => {
                                let _ = chars.next(); // {
                                let hex: String =
                                    chars.by_ref().take_while(|c| *c != '}').collect();
                                if let Some(ch) =
                                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                                {
                                    english.push(ch);
                                }
                            }
                            Some(other) => english.push(other),
                            None => break,
                        },
                        c => english.push(c),
                    }
                }
                out.push((id, english));
            }
        }
        out
    }

    /// The English catalog is the source of the fallback texts: where it has an entry the
    /// source text must agree with it, and the ids it lacks are the ones that read in English
    /// in every language (listed by `--nocapture`).
    #[test]
    fn the_english_texts_agree_with_the_catalog() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("vela-core")
            .join("locales")
            .join("en.json");
        let catalog: serde_json::Map<String, Value> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("catalog")).expect("json");
        let used = used_messages();
        assert!(used.len() > 150, "found {} messages", used.len());
        let mut missing: BTreeSet<String> = BTreeSet::new();
        let mut wrong: Vec<String> = Vec::new();
        for (id, english) in &used {
            match catalog.get(id).and_then(Value::as_str) {
                Some(text) if text != english => {
                    wrong.push(format!("{id}: source {english:?}, catalog {text:?}"))
                }
                Some(_) => {}
                None => {
                    missing.insert(id.clone());
                }
            }
        }
        println!("ids with no catalog entry ({}): {missing:?}", missing.len());
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
    #[test]
    fn number_fields_keep_their_declared_range() {
        let s = snap("highlighter");
        let f = s
            .schema
            .fields
            .iter()
            .find(|f| f.path == "style.lineWidth")
            .expect("width");
        assert_eq!(f.kind, FieldKind::Number);
        assert!(f.min.unwrap_or(1.0) > 1.0);
    }

    #[test]
    fn the_bar_floats_clear_of_the_drawing() {
        let host = (900.0, 560.0);
        let size = (300.0, 38.0);
        // Room above: centered on the box, 10 px over it.
        assert_eq!(
            bar_origin(Some((300.0, 200.0, 200.0, 80.0)), size, host, 10.0),
            (250.0, 152.0)
        );
        // No room above: below the box.
        assert_eq!(
            bar_origin(Some((300.0, 20.0, 200.0, 80.0)), size, host, 10.0),
            (250.0, 110.0)
        );
        // Kept inside the chart on both sides.
        assert_eq!(
            bar_origin(Some((0.0, 200.0, 20.0, 20.0)), size, host, 10.0).0,
            4.0
        );
        assert_eq!(
            bar_origin(Some((880.0, 200.0, 20.0, 20.0)), size, host, 10.0).0,
            596.0
        );
        // No box: top-center.
        assert_eq!(bar_origin(None, size, host, 10.0), (300.0, 12.0));
    }

    #[test]
    fn clamping_keeps_a_box_in_view() {
        assert_eq!(
            clamp_into(-5.0, -5.0, (100.0, 50.0), (400.0, 300.0)),
            (4.0, 4.0)
        );
        assert_eq!(
            clamp_into(999.0, 999.0, (100.0, 50.0), (400.0, 300.0)),
            (296.0, 246.0)
        );
        // An unknown host (0) only keeps the near edges.
        assert_eq!(
            clamp_into(999.0, 999.0, (100.0, 50.0), (0.0, 0.0)),
            (999.0, 999.0)
        );
    }

    #[test]
    fn a_menu_opens_below_its_trigger_or_above_near_the_bottom() {
        let host = (900.0, 560.0);
        assert_eq!(
            float_origin((100.0, 100.0, 30.0, 30.0), (150.0, 120.0), host, false),
            (100.0, 136.0)
        );
        // End-aligned: its right edge on the trigger's.
        assert_eq!(
            float_origin((500.0, 100.0, 30.0, 30.0), (150.0, 120.0), host, true),
            (380.0, 136.0)
        );
        // Near the bottom: flips above.
        assert_eq!(
            float_origin((100.0, 520.0, 30.0, 30.0), (150.0, 120.0), host, false),
            (100.0, 394.0)
        );
        // Near the right edge: pulled in.
        assert_eq!(
            float_origin((880.0, 100.0, 30.0, 30.0), (150.0, 120.0), host, false).0,
            746.0
        );
        // No room above or below: stays below.
        assert_eq!(
            float_origin(
                (100.0, 100.0, 30.0, 30.0),
                (150.0, 520.0),
                (900.0, 300.0),
                false
            )
            .1,
            136.0
        );
    }

    #[test]
    fn the_panel_hangs_off_the_bar_on_the_side_away_from_the_drawing() {
        let host = (900.0, 1200.0);
        let panel = (360.0, 420.0);
        let bar = (200.0, 500.0, 300.0, 38.0);
        // The drawing is below the bar: the panel prefers above it.
        let above = beside(bar, Some((250.0, 600.0, 200.0, 100.0)), host, panel);
        assert_eq!(above.left, 200.0);
        assert_eq!(above.top, None);
        assert_eq!(above.bottom, Some(1200.0 - 500.0 + 8.0));
        // The drawing is above: below the bar.
        let below = beside(bar, Some((250.0, 300.0, 200.0, 100.0)), host, panel);
        assert_eq!(below.top, Some(546.0));
        assert_eq!(below.bottom, None);
        // Its left edge is the bar's, kept in view.
        assert_eq!(
            beside((700.0, 500.0, 100.0, 38.0), None, host, panel).left,
            536.0
        );
    }

    #[test]
    fn the_panel_takes_the_side_with_more_room_and_scrolls_inside() {
        let host = (900.0, 560.0);
        let panel = (360.0, 520.0);
        // 520 fits on neither side: the side with more room, capped to it.
        let near_top = beside((100.0, 60.0, 300.0, 38.0), None, host, panel);
        assert_eq!(near_top.top, Some(106.0));
        assert_eq!(near_top.max_height, Some(560.0 - 98.0 - 8.0 - 12.0));
        // Too little room either side: over the bar, kept in view.
        let short = beside((100.0, 100.0, 300.0, 38.0), None, (900.0, 260.0), panel);
        assert_eq!(short.max_height, None);
        assert_eq!(short.bottom, None);
        assert_eq!(short.top, Some(12.0));
    }
    // ── ported from `drawings-popup-multi.test.ts` ──

    /// A drawing as that test builds it: two anchors (one for a text note), a trend line
    /// with its own color at width 1.
    fn built(type_key: &str, line_color: Option<&str>, locked: bool) -> Snap {
        let anchors = if type_key == "text" {
            vec![pt(0.0, 0.0)]
        } else {
            vec![pt(0.0, 0.0), pt(10.0, 10.0)]
        };
        let init = DrawingInit {
            id: Some("d".into()),
            pane_id: "price".into(),
            anchors,
            style: line_color.map(|c| vela_core::drawings::PartialDrawingStyle {
                line_color: Some(c.to_string()),
                line_width: Some(1.0),
                ..Default::default()
            }),
            locked: Some(locked),
            ..Default::default()
        };
        let d = DrawingRegistry::builtin()
            .create(type_key, init)
            .expect(type_key);
        Snap::of(d.as_ref(), "#151619")
    }

    fn trend(color: &str) -> Snap {
        built("trendline", Some(color), false)
    }

    #[test]
    fn multi_same_type_drawings_share_their_whole_schema() {
        let one: BTreeSet<String> = trend("#f00")
            .schema
            .fields
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(
            shared_paths(&[trend("#f00"), trend("#0f0"), trend("#00f")]),
            one
        );
    }

    #[test]
    fn multi_mixed_types_keep_only_the_intersection() {
        let stroked = shared_paths(&[trend("#f00"), built("box", None, false)]);
        assert!(stroked.contains("style.lineColor")); // both stroke
        assert!(!stroked.contains("style.fillColor")); // the box's alone
        let all = [
            trend("#f00"),
            built("box", None, false),
            built("text", None, false),
        ];
        let paths = shared_paths(&all);
        assert!(!paths.contains("style.lineColor")); // a text annotation has no stroke
        for p in &paths {
            assert!(all.iter().all(|s| s.has_field(p)), "{p}");
        }
    }

    #[test]
    fn multi_a_single_drawing_shares_everything_and_an_empty_list_nothing() {
        let b = built("box", None, false);
        assert_eq!(
            shared_paths(std::slice::from_ref(&b)).len(),
            b.schema.fields.len()
        );
        assert!(shared_paths(&[]).is_empty());
    }

    #[test]
    fn multi_agreeing_drawings_yield_the_value_and_disagreeing_ones_mixed() {
        let same = [trend("#ff0000"), trend("#ff0000")];
        assert_eq!(
            common(&same, |s| s.doc.style.line_color.clone()),
            Common::One("#ff0000".to_string())
        );
        let differ = [trend("#ff0000"), trend("#0000ff")];
        assert_eq!(
            common(&differ, |s| s.doc.style.line_color.clone()),
            Common::Mixed
        );
    }

    #[test]
    fn multi_a_lock_state_of_false_is_not_mixed() {
        let free = [
            built("trendline", None, false),
            built("trendline", None, false),
        ];
        assert_eq!(common(&free, |s| s.doc.locked), Common::One(false));
        let one_locked = [
            built("trendline", None, false),
            built("trendline", None, true),
        ];
        assert_eq!(common(&one_locked, |s| s.doc.locked), Common::Mixed);
        let both = [
            built("trendline", None, true),
            built("trendline", None, true),
        ];
        assert_eq!(common(&both, |s| s.doc.locked), Common::One(true));
    }

    // ── ported from `drawings-settings-panel.test.ts`, "where the panel opens" ──
    // A 1000 x 600 chart, the bar 330 x 40 at `(x, y)`, the panel measuring 360 x 360.

    #[test]
    fn the_panel_opens_right_under_a_bar_at_the_top_of_the_chart() {
        let at = beside(
            (40.0, 20.0, 330.0, 40.0),
            Some((200.0, 120.0, 400.0, 80.0)),
            (1000.0, 600.0),
            (360.0, 360.0),
        );
        assert_eq!((at.left, at.top, at.bottom), (40.0, Some(68.0), None));
    }

    #[test]
    fn the_panel_opens_above_a_bar_that_sits_above_a_drawing_low_on_the_chart() {
        let at = beside(
            (300.0, 420.0, 330.0, 40.0),
            Some((300.0, 480.0, 300.0, 80.0)),
            (1000.0, 600.0),
            (360.0, 360.0),
        );
        assert_eq!((at.left, at.top, at.bottom), (300.0, None, Some(188.0)));
    }

    #[test]
    fn the_panel_stays_inside_the_chart_near_its_right_edge() {
        let at = beside(
            (900.0, 20.0, 330.0, 40.0),
            Some((800.0, 120.0, 150.0, 80.0)),
            (1000.0, 600.0),
            (360.0, 360.0),
        );
        assert_eq!(at.left, 636.0);
    }
}
