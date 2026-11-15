//! Fibonacci tools.
//!
//! The original's `FibRatios` base class becomes the [`FibRatios`] trait plus
//! the shared [`FibCore`] state every Fibonacci-style tool embeds (the Gann
//! tools in `gann.rs` reuse both). `FibLevels` (the horizontal level tools)
//! and `RadialFib` (the concentric-ring tools) are traits layered on top.

use std::any::Any;
use std::f64::consts::PI;

use serde_json::Value;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Props, Span,
};
use crate::drawings::fib_presets::{classic_levels, ladder};
use crate::drawings::geometry::{DrawingPoint, FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_polyline, dist_to_segment, extend_ray};
use crate::drawings::level_palette::{
    FibLevel, LEVEL_PURPLE, LevelSpec, fib_levels, fib_levels_of,
};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, SettingsField, SettingsSchema, line_fields, text_fields,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::i18n::msg;
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::{DrawingExtend, LineStyle};
use crate::util::{js_number, to_fixed};

/// Font size for the entry numbers / labels (cycled by the bar buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FibTextSize {
    Small,
    Normal,
    Large,
    Huge,
}

impl FibTextSize {
    pub fn as_str(self) -> &'static str {
        match self {
            FibTextSize::Small => "small",
            FibTextSize::Normal => "normal",
            FibTextSize::Large => "large",
            FibTextSize::Huge => "huge",
        }
    }

    fn parse(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "small" => Some(FibTextSize::Small),
            "normal" => Some(FibTextSize::Normal),
            "large" => Some(FibTextSize::Large),
            "huge" => Some(FibTextSize::Huge),
            _ => None,
        }
    }
}

/// Which side of the levels their ratio and price text sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FibLabelSide {
    Left,
    Right,
}

impl FibLabelSide {
    pub fn as_str(self) -> &'static str {
        match self {
            FibLabelSide::Left => "left",
            FibLabelSide::Right => "right",
        }
    }
}

/// How an entry's number text aligns to its anchor point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FibAlign {
    Left,
    Center,
    Right,
}

/// Coerce an untrusted value into a valid level (defensive, for persistence
/// and round-trip).
fn sanitize_level(value: &Value) -> Option<FibLevel> {
    let o = value.as_object()?;
    let ratio = o.get("ratio")?.as_f64()?;
    let color = o.get("color")?.as_str()?.to_string();
    Some(FibLevel {
        ratio,
        color,
        enabled: o.get("enabled") != Some(&Value::Bool(false)),
        label: o
            .get("label")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .map(str::to_string),
    })
}

/// A resolved entry's pixel line + auto-number + custom-label placement (the painter's input).
#[derive(Clone, Debug, PartialEq)]
pub struct FibEntryLine {
    pub color: String,
    pub label: Option<String>,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub number_text: String,
    pub number_x: f64,
    pub number_y: f64,
    pub number_align: FibAlign,
    pub label_x: f64,
    pub label_y: f64,
}

/// A filled band between two neighbouring levels, in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FibBand {
    pub color: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The editable per-entry config every Fibonacci tool shares: `levels` (ratio
/// / color / enabled / label) plus number/label font sizes. It drives the
/// settings gear panel and is persisted through `props`.
#[derive(Clone, Debug, PartialEq)]
pub struct FibCore {
    /// Editable per-entry config (seeded from the tool's default levels).
    pub levels: Vec<FibLevel>,
    /// Font size of the auto numbers (a bar button cycles it).
    pub numbers_size: FibTextSize,
    /// Font size of the custom labels (a bar button cycles it).
    pub labels_size: FibTextSize,
}

impl FibCore {
    pub fn new(default_levels: Vec<FibLevel>) -> Self {
        Self {
            levels: default_levels,
            numbers_size: FibTextSize::Small,
            labels_size: FibTextSize::Normal,
        }
    }

    pub fn write_props(&self) -> Props {
        let mut props = Props::new();
        props.insert(
            "levels".into(),
            serde_json::to_value(&self.levels).unwrap_or(Value::Null),
        );
        props.insert("numbersSize".into(), self.numbers_size.as_str().into());
        props.insert("labelsSize".into(), self.labels_size.as_str().into());
        props
    }

    pub fn read_props(&mut self, props: &Props) {
        if let Some(items) = props.get("levels").and_then(Value::as_array) {
            let parsed: Vec<FibLevel> = items.iter().filter_map(sanitize_level).collect();
            if !parsed.is_empty() {
                self.levels = parsed;
            }
        }
        if let Some(size) = props.get("numbersSize").and_then(FibTextSize::parse) {
            self.numbers_size = size;
        }
        if let Some(size) = props.get("labelsSize").and_then(FibTextSize::parse) {
            self.labels_size = size;
        }
    }
}

/// The anchor schema of the two-point Fibonacci tools.
pub fn two_point_schema() -> AnchorSchema {
    AnchorSchema::free(&["p1", "p2"])
}

/// An anchor schema of `roles.len()` free points.
pub fn free_schema(roles: &[&str]) -> AnchorSchema {
    AnchorSchema {
        min: roles.len(),
        max: roles.len(),
        slots: roles
            .iter()
            .map(|r| AnchorSlot::new(*r, FreeAxis::Both))
            .collect(),
    }
}

/// Width + style apply to all entries; per-entry color/enable/label live in the gear panel.
pub fn fib_schema() -> SettingsSchema {
    SettingsSchema::new(
        line_fields()
            .into_iter()
            .filter(|f| f.path != "style.lineColor")
            .collect(),
    )
}

/// Shared behavior of every Fibonacci tool. The hit-test, handles, schema and
/// serialization all derive from [`FibRatios::entry_lines`]; a tool only
/// supplies its geometry (and its default levels, via its constructor).
pub trait FibRatios: Drawing {
    fn fib_core(&self) -> &FibCore;
    fn fib_core_mut(&mut self) -> &mut FibCore;

    /// ENABLED entries resolved to pixel lines + label placement: the painter's input.
    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>>;

    /// Optional fill bands between entries (retracement/extension override); default none.
    fn fill_bands(&self, _proj: &dyn Projector) -> Vec<FibBand> {
        Vec::new()
    }

    /// Within `tol` px of any entry line.
    fn entry_hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_lines(proj).is_some_and(|lines| {
            lines
                .iter()
                .any(|l| dist_to_segment(px, py, l.x1, l.y1, l.x2, l.y2) <= tol)
        })
    }
}

/// The `Drawing` methods every `FibRatios` tool implements the same way. The
/// type must have `base: DrawingBase` and `core: FibCore` fields and a
/// `TYPE` constant.
macro_rules! fib_drawing_common {
    () => {
        fn base(&self) -> &$crate::drawings::drawing::DrawingBase {
            &self.base
        }

        fn base_mut(&mut self) -> &mut $crate::drawings::drawing::DrawingBase {
            &mut self.base
        }

        fn type_key(&self) -> &'static str {
            Self::TYPE
        }

        fn as_any(&self) -> &dyn ::std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any {
            self
        }

        fn handle_points(
            &self,
            proj: &dyn $crate::drawings::geometry::Projector,
        ) -> Vec<(f64, f64)> {
            $crate::drawings::types::segment::segment_handle_points(&self.base, proj)
        }

        fn bounds(
            &self,
            proj: &dyn $crate::drawings::geometry::Projector,
        ) -> Option<$crate::drawings::drawing::Bounds> {
            $crate::drawings::types::segment::segment_bounds(&self.base, proj)
        }

        fn editable_levels(&self) -> Option<Vec<$crate::drawings::drawing::EditableLevel>> {
            Some(self.core.levels.clone())
        }
    };
}
pub(crate) use fib_drawing_common;

/// Implements [`FibRatios`] for a type with `base` and `core` fields.
macro_rules! fib_ratios_accessors {
    () => {
        fn fib_core(&self) -> &$crate::drawings::types::fib::FibCore {
            &self.core
        }

        fn fib_core_mut(&mut self) -> &mut $crate::drawings::types::fib::FibCore {
            &mut self.core
        }
    };
}
pub(crate) use fib_ratios_accessors;

/// The price span of a two-anchor tool.
pub fn two_anchor_price_range(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Some(Span {
        min: js_min(a.price, b.price),
        max: js_max(a.price, b.price),
    })
}

fn y_px(base: &DrawingBase, price: f64, proj: &dyn Projector) -> Option<f64> {
    base.y_of(price, proj)
}

fn entry(
    color: &str,
    label: Option<&String>,
    line: [f64; 4],
    number: (String, f64, f64, FibAlign),
    label_at: (f64, f64),
) -> FibEntryLine {
    FibEntryLine {
        color: color.to_string(),
        label: label.cloned(),
        x1: line[0],
        y1: line[1],
        x2: line[2],
        y2: line[3],
        number_text: number.0,
        number_x: number.1,
        number_y: number.2,
        number_align: number.3,
        label_x: label_at.0,
        label_y: label_at.1,
    }
}

// ── horizontal level tools (retracement, extension) ──

/// A resolved (enabled) level in pixels: its price, color, label, and line to stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct FibLevelLine {
    pub ratio: f64,
    pub color: String,
    pub label: Option<String>,
    pub price: f64,
    pub x1: f64,
    pub x2: f64,
    pub y: f64,
}

/// How the horizontal level tools show their levels.
#[derive(Clone, Debug, PartialEq)]
pub struct FibLevelsState {
    /// Show each level's ratio beside it.
    pub show_ratios: bool,
    /// Show each level's price beside it.
    pub show_prices: bool,
    /// Where the ratio / price text sits.
    pub label_side: FibLabelSide,
    /// Tint the bands between levels.
    pub background: bool,
}

impl Default for FibLevelsState {
    fn default() -> Self {
        Self {
            show_ratios: true,
            show_prices: true,
            label_side: FibLabelSide::Left,
            background: true,
        }
    }
}

impl FibLevelsState {
    fn write_props(&self, props: &mut Props) {
        props.insert("showRatios".into(), self.show_ratios.into());
        props.insert("showPrices".into(), self.show_prices.into());
        props.insert("labelSide".into(), self.label_side.as_str().into());
        props.insert("background".into(), self.background.into());
    }

    fn read_props(&mut self, props: &Props) {
        if let Some(v) = props.get("showRatios").and_then(Value::as_bool) {
            self.show_ratios = v;
        }
        if let Some(v) = props.get("showPrices").and_then(Value::as_bool) {
            self.show_prices = v;
        }
        match props.get("labelSide").and_then(Value::as_str) {
            Some("left") => self.label_side = FibLabelSide::Left,
            Some("right") => self.label_side = FibLabelSide::Right,
            _ => {}
        }
        if let Some(v) = props.get("background").and_then(Value::as_bool) {
            self.background = v;
        }
    }
}

/// Shared behavior of the horizontal Fibonacci level tools (retracement,
/// extension): two anchors define a price range; each level is a horizontal
/// line at `level_price(ratio, p1, p2)`, spanning the anchors' time range,
/// with fill bands between consecutive levels.
pub trait FibLevels: FibRatios {
    fn level_state(&self) -> &FibLevelsState;

    /// Price of a level for the two anchor prices; a tool overrides this to
    /// change which anchor ratio 0 sits on.
    fn level_price(&self, ratio: f64, p1: f64, p2: f64) -> f64 {
        p1 + ratio * (p2 - p1)
    }

    /// Per-level pixel line + price for the ENABLED levels, spanning the anchors' time range.
    fn level_lines(&self, proj: &dyn Projector) -> Option<Vec<FibLevelLine>> {
        let base = self.base();
        let a = base.anchors.first()?;
        let b = base.anchors.get(1)?;
        let xa = proj.x_of(a.time);
        let xb = proj.x_of(b.time);
        // Extended levels run on to the chart's edge on that side.
        let x1 = if base.style.extend_left.unwrap_or(false) {
            0.0
        } else {
            js_min(xa, xb)
        };
        let x2 = if base.style.extend_right.unwrap_or(false) {
            proj.width()
        } else {
            js_max(xa, xb)
        };
        let mut out = Vec::new();
        for lv in &self.fib_core().levels {
            if !lv.enabled {
                continue;
            }
            let price = self.level_price(lv.ratio, a.price, b.price);
            let Some(y) = y_px(base, price, proj) else {
                continue;
            };
            out.push(FibLevelLine {
                ratio: lv.ratio,
                color: lv.color.clone(),
                label: lv.label.clone(),
                price,
                x1,
                x2,
                y,
            });
        }
        Some(out)
    }

    /// The text beside a level: its ratio, its price, both, or nothing.
    fn level_text(&self, ratio: f64, price: f64) -> String {
        let state = self.level_state();
        let p = to_fixed(price, 2);
        match (state.show_ratios, state.show_prices) {
            (true, true) => format!("{} ({})", js_number(ratio), p),
            (true, false) => js_number(ratio),
            (false, true) => p,
            (false, false) => String::new(),
        }
    }

    fn level_entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let lines = self.level_lines(proj)?;
        let right = self.level_state().label_side == FibLabelSide::Right;
        Some(
            lines
                .iter()
                .map(|l| {
                    entry(
                        &l.color,
                        l.label.as_ref(),
                        [l.x1, l.y, l.x2, l.y],
                        (
                            self.level_text(l.ratio, l.price),
                            if right { l.x2 - 4.0 } else { l.x1 + 4.0 },
                            l.y - 7.0,
                            if right {
                                FibAlign::Right
                            } else {
                                FibAlign::Left
                            },
                        ),
                        ((l.x1 + l.x2) / 2.0, l.y - 7.0),
                    )
                })
                .collect(),
        )
    }

    fn level_fill_bands(&self, proj: &dyn Projector) -> Vec<FibBand> {
        if !self.level_state().background {
            return Vec::new();
        }
        // Bands join neighbouring levels by ratio, however the list is ordered.
        let Some(mut lines) = self.level_lines(proj) else {
            return Vec::new();
        };
        lines.sort_by(|p, q| {
            p.ratio
                .partial_cmp(&q.ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        lines
            .windows(2)
            .map(|w| FibBand {
                color: w[1].color.clone(),
                x: w[0].x1,
                y: js_min(w[0].y, w[1].y),
                w: w[0].x2 - w[0].x1,
                h: (w[1].y - w[0].y).abs(),
            })
            .collect()
    }

    fn level_price_range(&self) -> Option<Span> {
        let base = self.base();
        let a = base.anchors.first()?;
        let b = base.anchors.get(1)?;
        Span::of(
            self.fib_core()
                .levels
                .iter()
                .filter(|l| l.enabled)
                .map(|l| self.level_price(l.ratio, a.price, b.price)),
        )
    }

    fn level_schema(&self) -> SettingsSchema {
        let mut schema = fib_schema();
        schema.fields.push(
            SettingsField::new(
                "style.extendLeft",
                msg("drawing.field.extendLeft", "Extend left"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        schema.fields.push(
            SettingsField::new(
                "style.extendRight",
                msg("drawing.field.extendRight", "Extend right"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
        );
        schema
    }

    fn level_write_props(&self) -> Props {
        let mut props = self.fib_core().write_props();
        self.level_state().write_props(&mut props);
        props
    }
}

/// Fibonacci retracement: horizontal levels between two swing anchors. Level
/// 0 sits on the second anchor and level 1 on the first, so the levels
/// measure the pullback from the point just placed; `reverse` puts level 0 on
/// the first anchor instead.
#[derive(Clone, Debug)]
pub struct FibRetracement {
    base: DrawingBase,
    core: FibCore,
    levels_state: FibLevelsState,
    /// Level 0 on the first anchor (the layout drawings had before the default flipped).
    pub reverse: bool,
}

impl FibRetracement {
    pub const TYPE: &'static str = "fibretracement";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(classic_levels()),
            levels_state: FibLevelsState::default(),
            reverse: false,
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    pub fn levels_state(&self) -> &FibLevelsState {
        &self.levels_state
    }
}

impl FibRatios for FibRetracement {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        self.level_entry_lines(proj)
    }

    fn fill_bands(&self, proj: &dyn Projector) -> Vec<FibBand> {
        self.level_fill_bands(proj)
    }
}

impl FibLevels for FibRetracement {
    fn level_state(&self) -> &FibLevelsState {
        &self.levels_state
    }

    fn level_price(&self, ratio: f64, p1: f64, p2: f64) -> f64 {
        if self.reverse {
            p1 + ratio * (p2 - p1)
        } else {
            p2 + ratio * (p1 - p2)
        }
    }
}

impl Drawing for FibRetracement {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        self.level_price_range()
    }

    fn schema(&self) -> SettingsSchema {
        self.level_schema()
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = self.level_write_props();
        props.insert("reverse".into(), self.reverse.into());
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
        self.levels_state.read_props(props);
        // A document saved before the flag existed was laid out with level 0 on the first
        // anchor; keep it that way instead of letting it jump to the new default.
        self.reverse = props
            .get("reverse")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    }
}

/// Extension ratios: the swing plus projections beyond it (>1) for price
/// targets, with the rest of the ladder present but off. The furthest default
/// target breaks out of the shared hues to read as the outermost projection.
fn extension_levels() -> Vec<FibLevel> {
    ladder(
        &[
            0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.272, 1.414, 1.618, 2.0, 2.272, 2.414,
            2.618, 3.0, 3.272, 3.618, 4.236, 4.618,
        ],
        &[0.0, 0.382, 0.618, 1.0, 1.272, 1.618, 2.618],
        &[("2.618", LEVEL_PURPLE)],
    )
}

/// Fibonacci extension: the same two-anchor levels as a retracement, but with
/// ratios projecting beyond the swing (1.272 / 1.618 / 2.618) for price targets.
#[derive(Clone, Debug)]
pub struct FibExtension {
    base: DrawingBase,
    core: FibCore,
    levels_state: FibLevelsState,
}

impl FibExtension {
    pub const TYPE: &'static str = "fibextension";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(extension_levels()),
            levels_state: FibLevelsState::default(),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    pub fn levels_state(&self) -> &FibLevelsState {
        &self.levels_state
    }
}

impl FibRatios for FibExtension {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        self.level_entry_lines(proj)
    }

    fn fill_bands(&self, proj: &dyn Projector) -> Vec<FibBand> {
        self.level_fill_bands(proj)
    }
}

impl FibLevels for FibExtension {
    fn level_state(&self) -> &FibLevelsState {
        &self.levels_state
    }
}

impl Drawing for FibExtension {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        self.level_price_range()
    }

    fn schema(&self) -> SettingsSchema {
        self.level_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.level_write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
        self.levels_state.read_props(props);
    }
}

// ── trend-based extension ──

/// Trend-based ratios: the move plus projections for price targets.
fn trend_extension_levels() -> Vec<FibLevel> {
    fib_levels([
        LevelSpec::ratio(0.0),
        LevelSpec::ratio(0.382),
        LevelSpec::ratio(0.5),
        LevelSpec::ratio(0.618),
        LevelSpec::ratio(1.0),
        LevelSpec::ratio(1.618),
        LevelSpec::ratio(2.618).color(LEVEL_PURPLE),
    ])
}

/// Trend-based Fibonacci extension: three anchors A, B, C. Levels project the
/// A-B price move from the C anchor (`price = C.price + ratio * (B.price -
/// A.price)`), drawn as horizontal lines across the anchors' time span.
#[derive(Clone, Debug)]
pub struct FibExtensionTrend {
    base: DrawingBase,
    core: FibCore,
}

impl FibExtensionTrend {
    pub const TYPE: &'static str = "fibextensiontrend";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(trend_extension_levels()),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    fn abc(&self) -> Option<(DrawingPoint, DrawingPoint, DrawingPoint)> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let c = self.base.anchors.get(2)?;
        Some((*a, *b, *c))
    }
}

impl FibRatios for FibExtensionTrend {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let (a, b, c) = self.abc()?;
        let move_ = b.price - a.price;
        let xs = [proj.x_of(a.time), proj.x_of(b.time), proj.x_of(c.time)];
        let x1 = xs.iter().copied().fold(f64::INFINITY, js_min);
        let x2 = xs.iter().copied().fold(f64::NEG_INFINITY, js_max);
        let mut out = Vec::new();
        for lv in &self.core.levels {
            if !lv.enabled {
                continue;
            }
            let price = c.price + lv.ratio * move_;
            let Some(y) = y_px(&self.base, price, proj) else {
                continue;
            };
            out.push(entry(
                &lv.color,
                lv.label.as_ref(),
                [x1, y, x2, y],
                (
                    format!("{} ({})", js_number(lv.ratio), to_fixed(price, 2)),
                    x1 + 4.0,
                    y - 7.0,
                    FibAlign::Left,
                ),
                ((x1 + x2) / 2.0, y - 7.0),
            ));
        }
        Some(out)
    }
}

impl Drawing for FibExtensionTrend {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        free_schema(&["a", "b", "c"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        let (a, b, c) = self.abc()?;
        let move_ = b.price - a.price;
        let mut prices: Vec<f64> = self
            .core
            .levels
            .iter()
            .filter(|l| l.enabled)
            .map(|l| c.price + l.ratio * move_)
            .collect();
        prices.extend([a.price, b.price, c.price]);
        Span::of(prices)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── fan ──

/// A resolved fan ray in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FibFanLine {
    pub ratio: f64,
    pub color: String,
    pub label: Option<String>,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub label_x: f64,
    pub label_y: f64,
}

/// A Fibonacci fan: rays from the first anchor through the fib-divided price
/// levels at the second anchor's time, extended to the right edge. Each ray
/// is an editable level (color / enabled / label) via the gear panel.
#[derive(Clone, Debug)]
pub struct FibFan {
    base: DrawingBase,
    core: FibCore,
}

impl FibFan {
    pub const TYPE: &'static str = "fibfan";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[0.236, 0.382, 0.5, 0.618, 0.786, 1.0])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    pub fn fan_lines(&self, proj: &dyn Projector) -> Option<Vec<FibFanLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let x0 = proj.x_of(a.time);
        let y0 = y_px(&self.base, a.price, proj)?;
        let xb = proj.x_of(b.time);
        let delta = b.price - a.price;
        let mut out = Vec::new();
        for lv in &self.core.levels {
            if !lv.enabled {
                continue;
            }
            let Some(ty) = y_px(&self.base, a.price + lv.ratio * delta, proj) else {
                continue;
            };
            let [x1, y1, x2, y2] = extend_ray(
                x0,
                y0,
                xb,
                ty,
                DrawingExtend::Right,
                proj.width(),
                proj.height(),
            );
            out.push(FibFanLine {
                ratio: lv.ratio,
                color: lv.color.clone(),
                label: lv.label.clone(),
                x1,
                y1,
                x2,
                y2,
                label_x: xb + 5.0,
                label_y: ty,
            });
        }
        Some(out)
    }
}

impl FibRatios for FibFan {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let lines = self.fan_lines(proj)?;
        Some(
            lines
                .iter()
                .map(|l| {
                    entry(
                        &l.color,
                        l.label.as_ref(),
                        [l.x1, l.y1, l.x2, l.y2],
                        (js_number(l.ratio), l.label_x, l.label_y, FibAlign::Left),
                        (l.label_x, l.label_y + 13.0),
                    )
                })
                .collect(),
        )
    }
}

impl Drawing for FibFan {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── time zones ──

/// A resolved time-zone line: its Fibonacci index, color, label, and pixel x.
#[derive(Clone, Debug, PartialEq)]
pub struct FibZoneLine {
    pub n: f64,
    pub color: String,
    pub label: Option<String>,
    pub x: f64,
}

/// The vertical line entries shared by the time tools.
fn vertical_entry(
    color: &str,
    label: Option<&String>,
    x: f64,
    number: f64,
    height: f64,
) -> FibEntryLine {
    entry(
        color,
        label,
        [x, 0.0, x, height],
        (js_number(number), x, 9.0, FibAlign::Center),
        (x, 24.0),
    )
}

/// Fibonacci time zones: vertical lines at Fibonacci multiples of the base
/// interval (the time span between the two anchors), projecting forward. Each
/// line is an editable level (color / enabled / label) via the gear panel.
/// Price-agnostic: the lines span the full pane height and don't affect the
/// price scale.
#[derive(Clone, Debug)]
pub struct FibTimeZones {
    base: DrawingBase,
    core: FibCore,
}

impl FibTimeZones {
    pub const TYPE: &'static str = "fibtimezones";

    pub fn new(init: &DrawingInit) -> Self {
        // The Fibonacci sequence (the time multiples drawn as vertical lines).
        let levels = [0.0, 1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 21.0, 34.0]
            .iter()
            .map(|&n| FibLevel {
                ratio: n,
                color: DEFAULT_DRAWING_COLOR.to_string(),
                enabled: true,
                label: None,
            })
            .collect();
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(levels),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    pub fn zone_lines(&self, proj: &dyn Projector) -> Option<Vec<FibZoneLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let unit = b.time - a.time;
        if unit.abs() < 1e-9 {
            return None;
        }
        Some(
            self.core
                .levels
                .iter()
                .filter(|lv| lv.enabled)
                .map(|lv| FibZoneLine {
                    n: lv.ratio,
                    color: lv.color.clone(),
                    label: lv.label.clone(),
                    x: proj.x_of(a.time + lv.ratio * unit),
                })
                .collect(),
        )
    }
}

impl FibRatios for FibTimeZones {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let lines = self.zone_lines(proj)?;
        let h = proj.height();
        Some(
            lines
                .iter()
                .map(|l| vertical_entry(&l.color, l.label.as_ref(), l.x, l.n, h))
                .collect(),
        )
    }
}

impl Drawing for FibTimeZones {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    /// Vertical lines impose no price constraint.
    fn price_range(&self) -> Option<Span> {
        None
    }

    /// The zones project far forward, so they never time-cull.
    fn time_extent(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── channel ──

/// A Fibonacci channel: a baseline (p1 to p2) plus parallel level lines, each
/// the baseline translated by `ratio * (p3 - p1)` (a full 2D offset vector:
/// the third anchor sets both the width and the skew). Level 0 is the
/// baseline; level 1 passes through p3.
#[derive(Clone, Debug)]
pub struct FibChannel {
    base: DrawingBase,
    core: FibCore,
}

impl FibChannel {
    pub const TYPE: &'static str = "fibchannel";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[
                0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.618, 2.618,
            ])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl FibRatios for FibChannel {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let y0 = y_px(&self.base, a.price, proj)?;
        let y1 = y_px(&self.base, b.price, proj)?;
        let x0 = proj.x_of(a.time);
        let x1 = proj.x_of(b.time);
        // The offset vector p3 - p1 (zero while only the baseline is placed:
        // the levels collapse onto it).
        let (mut d_x, mut d_y) = (0.0, 0.0);
        if let Some(c) = self.base.anchors.get(2) {
            let y2 = y_px(&self.base, c.price, proj)?;
            d_x = proj.x_of(c.time) - x0;
            d_y = y2 - y0;
        }
        let mut out = Vec::new();
        for lv in &self.core.levels {
            if !lv.enabled {
                continue;
            }
            let ox = lv.ratio * d_x;
            let oy = lv.ratio * d_y;
            let lx2 = x1 + ox;
            let ly2 = y1 + oy;
            out.push(entry(
                &lv.color,
                lv.label.as_ref(),
                [x0 + ox, y0 + oy, lx2, ly2],
                (js_number(lv.ratio), lx2 + 5.0, ly2, FibAlign::Left),
                (lx2 + 5.0, ly2 + 13.0),
            ));
        }
        Some(out)
    }
}

impl Drawing for FibChannel {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        free_schema(&["p1", "p2", "offset"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let d_price = self.base.anchors.get(2).map_or(0.0, |c| c.price - a.price);
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for lv in self.core.levels.iter().filter(|l| l.enabled) {
            for base_price in [a.price, b.price] {
                let p = base_price + lv.ratio * d_price;
                if p < min {
                    min = p;
                }
                if p > max {
                    max = p;
                }
            }
        }
        (min <= max).then_some(Span { min, max })
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── speed/resistance fan ──

/// A Fibonacci speed/resistance fan: from the pivot (p1), each level emits TWO
/// rays that subdivide the edges of the p1-p2 box: a price ray to (full
/// width, ratio * height) and a time ray to (ratio * width, full height),
/// both extended to the chart edge. Purely pixel-space subdivision of the box
/// (it tracks the box on zoom), matching the reference.
#[derive(Clone, Debug)]
pub struct FibSpeedFan {
    base: DrawingBase,
    core: FibCore,
}

impl FibSpeedFan {
    pub const TYPE: &'static str = "fibspeedfan";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl FibRatios for FibSpeedFan {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let y1 = y_px(&self.base, a.price, proj)?;
        let y2 = y_px(&self.base, b.price, proj)?;
        let x1 = proj.x_of(a.time);
        let x2 = proj.x_of(b.time);
        let dx = x2 - x1;
        let dy = y2 - y1;
        let right = dx >= 0.0;
        let dir = if right {
            DrawingExtend::Right
        } else {
            DrawingExtend::Left
        };
        let (w, h) = (proj.width(), proj.height());
        let mut out = Vec::new();
        for lv in &self.core.levels {
            if !lv.enabled {
                continue;
            }
            // price ray: pivot to (full width, ratio * height): subdivides the box's right edge
            let pex = x1 + dx;
            let pey = y1 + dy * lv.ratio;
            out.push(entry(
                &lv.color,
                lv.label.as_ref(),
                extend_ray(x1, y1, pex, pey, dir, w, h),
                (
                    js_number(lv.ratio),
                    if right { pex + 5.0 } else { pex - 5.0 },
                    pey,
                    if right {
                        FibAlign::Left
                    } else {
                        FibAlign::Right
                    },
                ),
                (pex, pey),
            ));
            if lv.ratio == 1.0 {
                continue; // the time ray at ratio 1 is the same diagonal as the price ray
            }
            // time ray: pivot to (ratio * width, full height): subdivides the box's bottom edge
            let tex = x1 + dx * lv.ratio;
            let tey = y1 + dy;
            out.push(entry(
                &lv.color,
                lv.label.as_ref(),
                extend_ray(x1, y1, tex, tey, dir, w, h),
                (
                    js_number(lv.ratio),
                    tex,
                    tey + if dy >= 0.0 { 12.0 } else { -12.0 },
                    FibAlign::Center,
                ),
                (tex, tey + 12.0),
            ));
        }
        Some(out)
    }
}

impl Drawing for FibSpeedFan {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        two_point_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── trend-based time ──

/// Trend-based Fibonacci time: vertical lines at Fibonacci multiples of the
/// base interval (p1 to p2), projected forward from the origin p3:
/// `x_of(p3.time + ratio * (p2.time - p1.time))`. Time-only (the level lines
/// ignore price), like [`FibTimeZones`] but driven by a 3-point trend.
#[derive(Clone, Debug)]
pub struct TrendFibTime {
    base: DrawingBase,
    core: FibCore,
}

impl TrendFibTime {
    pub const TYPE: &'static str = "trendfibtime";

    pub fn new(init: &DrawingInit) -> Self {
        let levels = fib_levels([
            LevelSpec::ratio(0.0),
            LevelSpec::ratio(0.382),
            LevelSpec::ratio(0.5).enabled(false), // hidden by default
            LevelSpec::ratio(0.618),
            LevelSpec::ratio(1.0),
            LevelSpec::ratio(1.382),
            LevelSpec::ratio(1.618),
            LevelSpec::ratio(2.0),
            LevelSpec::ratio(2.382),
            LevelSpec::ratio(2.618),
            LevelSpec::ratio(3.0),
        ]);
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(levels),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl FibRatios for TrendFibTime {
    fib_ratios_accessors!();

    fn entry_lines(&self, proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let c = self.base.anchors.get(2)?;
        let unit = b.time - a.time; // base interval (signed)
        if unit.abs() < 1e-9 {
            return None;
        }
        let h = proj.height();
        Some(
            self.core
                .levels
                .iter()
                .filter(|lv| lv.enabled)
                .map(|lv| {
                    let x = proj.x_of(c.time + lv.ratio * unit);
                    vertical_entry(&lv.color, lv.label.as_ref(), x, lv.ratio, h)
                })
                .collect(),
        )
    }
}

impl Drawing for TrendFibTime {
    fib_drawing_common!();

    fn anchor_schema(&self) -> AnchorSchema {
        free_schema(&["p1", "p2", "p3"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.entry_hit_test(px, py, proj, tol)
    }

    /// Vertical lines impose no price constraint.
    fn price_range(&self) -> Option<Span> {
        None
    }

    /// The projected lines run far forward, so they never time-cull.
    fn time_extent(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        fib_schema()
    }

    fn write_props(&self) -> Option<Props> {
        Some(self.core.write_props())
    }

    fn read_props(&mut self, props: &Props) {
        self.core.read_props(props);
    }
}

// ── concentric-ring tools (circles, arcs, wedge) ──

/// Center + base pixel radius + the arc sweep `[a0, a1]` shared by every concentric-ring fib tool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialGeom {
    pub cx: f64,
    pub cy: f64,
    pub r0: f64,
    pub a0: f64,
    pub a1: f64,
}

/// True if a cursor angle falls within the arc sweep `[a0, a1]` (`a0 <= a1`,
/// going clockwise on canvas).
pub fn angle_in_sweep(ang: f64, a0: f64, a1: f64) -> bool {
    if a1 - a0 >= PI * 2.0 - 1e-3 {
        return true; // full circle
    }
    let two_pi = PI * 2.0;
    let mut d = ang - a0;
    while d < 0.0 {
        d += two_pi;
    }
    while d >= two_pi {
        d -= two_pi;
    }
    d <= a1 - a0 + 1e-6
}

/// Shared behavior of the concentric-ring Fibonacci tools. Each is the
/// Circle-style hybrid: a data-anchored center with PIXEL-space radii
/// (`r0 * ratio`). A tool supplies [`RadialFib::radial`] (center + base radius
/// + angular sweep) and any [`RadialFib::bounding_lines`]; the painter draws
///   each enabled level as an arc of radius `r0 * ratio` over the sweep.
pub trait RadialFib: FibRatios {
    /// Center, base pixel radius, and the arc sweep; `None` until enough anchors are placed.
    fn radial(&self, proj: &dyn Projector) -> Option<RadialGeom>;

    /// Straight lines drawn alongside the rings (baseline / wedge rays);
    /// previewed before `radial` resolves.
    fn bounding_lines(&self, _proj: &dyn Projector) -> Vec<[f64; 4]> {
        Vec::new()
    }

    /// Ring tools hit-test by radius, not line segments.
    fn radial_hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        if let Some(g) = self.radial(proj) {
            let dist = (px - g.cx).hypot(py - g.cy);
            let ang = (py - g.cy).atan2(px - g.cx);
            if angle_in_sweep(ang, g.a0, g.a1) {
                for lv in &self.fib_core().levels {
                    if lv.enabled && (dist - g.r0 * lv.ratio).abs() <= tol {
                        return true;
                    }
                }
            }
        }
        self.bounding_lines(proj)
            .iter()
            .any(|b| dist_to_segment(px, py, b[0], b[1], b[2], b[3]) <= tol)
    }

    fn radial_price_range(&self) -> Option<Span> {
        Span::of(self.base().anchors.iter().map(|a| a.price))
    }
}

/// Implements `FibRatios` (no entry lines) and `Drawing` for a ring tool.
macro_rules! radial_drawing {
    ($ty:ident, $schema:expr) => {
        impl FibRatios for $ty {
            fib_ratios_accessors!();

            /// Ring tools are painted from their radial geometry; there are no straight entries.
            fn entry_lines(&self, _proj: &dyn Projector) -> Option<Vec<FibEntryLine>> {
                Some(Vec::new())
            }
        }

        impl Drawing for $ty {
            fib_drawing_common!();

            fn anchor_schema(&self) -> AnchorSchema {
                $schema
            }

            fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
                self.radial_hit_test(px, py, proj, tol)
            }

            fn price_range(&self) -> Option<Span> {
                self.radial_price_range()
            }

            fn schema(&self) -> SettingsSchema {
                fib_schema()
            }

            fn write_props(&self) -> Option<Props> {
                Some(self.core.write_props())
            }

            fn read_props(&mut self, props: &Props) {
                self.core.read_props(props);
            }
        }
    };
}

/// Concentric full circles at Fibonacci-ratio pixel radii from a data-anchored center (center + edge).
#[derive(Clone, Debug)]
pub struct FibCircles {
    base: DrawingBase,
    core: FibCore,
}

impl FibCircles {
    pub const TYPE: &'static str = "fibcircles";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[
                0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.618, 2.618,
            ])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl RadialFib for FibCircles {
    fn radial(&self, proj: &dyn Projector) -> Option<RadialGeom> {
        let (cx, cy) = self.base.anchor_px(0, proj)?;
        let (ex, ey) = self.base.anchor_px(1, proj)?;
        Some(RadialGeom {
            cx,
            cy,
            r0: (ex - cx).hypot(ey - cy),
            a0: 0.0,
            a1: PI * 2.0,
        })
    }
}

radial_drawing!(FibCircles, two_point_schema());

/// Fibonacci speed/resistance arcs: concentric semicircles centered at the
/// pivot (p1), at radii `r0 * ratio` where `r0 = |p2 - p1|`, bulging toward
/// the far anchor (p2). The p1-p2 baseline is drawn too.
#[derive(Clone, Debug)]
pub struct FibArcs {
    base: DrawingBase,
    core: FibCore,
}

impl FibArcs {
    pub const TYPE: &'static str = "fibarcs";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[
                0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.618, 2.618,
            ])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// The pivot and far-anchor pixels.
    fn pts(&self, proj: &dyn Projector) -> Option<((f64, f64), (f64, f64))> {
        Some((self.base.anchor_px(0, proj)?, self.base.anchor_px(1, proj)?))
    }
}

impl RadialFib for FibArcs {
    fn radial(&self, proj: &dyn Projector) -> Option<RadialGeom> {
        let ((cx, cy), (ex, ey)) = self.pts(proj)?;
        let r0 = (ex - cx).hypot(ey - cy);
        let phi = (ey - cy).atan2(ex - cx); // direction toward the far anchor
        Some(RadialGeom {
            cx,
            cy,
            r0,
            a0: phi - PI / 2.0,
            a1: phi + PI / 2.0,
        })
    }

    /// The pivot-to-far baseline.
    fn bounding_lines(&self, proj: &dyn Projector) -> Vec<[f64; 4]> {
        self.pts(proj)
            .map(|((cx, cy), (ex, ey))| vec![[cx, cy, ex, ey]])
            .unwrap_or_default()
    }
}

radial_drawing!(FibArcs, two_point_schema());

/// A Fibonacci wedge: an apex (p1) with a base radius `r0 = |p2 - p1|` and an
/// angular sweep between the two rays p1-p2 and p1-p3 (p3 contributes only its
/// angle). Each level is a circular arc of radius `r0 * ratio` spanning the
/// (minor) wedge angle, plus the two bounding rays.
#[derive(Clone, Debug)]
pub struct FibWedge {
    base: DrawingBase,
    core: FibCore,
}

impl FibWedge {
    pub const TYPE: &'static str = "fibwedge";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            core: FibCore::new(fib_levels_of(&[0.236, 0.382, 0.5, 0.618, 0.786, 1.0])),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }
}

impl RadialFib for FibWedge {
    fn radial(&self, proj: &dyn Projector) -> Option<RadialGeom> {
        let p0 = self.base.anchor_px(0, proj)?;
        let p1 = self.base.anchor_px(1, proj)?;
        let p2 = self.base.anchor_px(2, proj)?;
        let r0 = (p1.0 - p0.0).hypot(p1.1 - p0.1);
        if r0 < 1.0 {
            return None;
        }
        let angle1 = (p1.1 - p0.1).atan2(p1.0 - p0.0);
        let angle2 = (p2.1 - p0.1).atan2(p2.0 - p0.0);
        let mut lo = js_min(angle1, angle2);
        let mut hi = js_max(angle1, angle2);
        if hi - lo > PI {
            // take the minor (wedge) sweep
            (lo, hi) = (hi, lo + PI * 2.0);
        }
        Some(RadialGeom {
            cx: p0.0,
            cy: p0.1,
            r0,
            a0: lo,
            a1: hi,
        })
    }

    fn bounding_lines(&self, proj: &dyn Projector) -> Vec<[f64; 4]> {
        let (Some(p0), Some(p1)) = (self.base.anchor_px(0, proj), self.base.anchor_px(1, proj))
        else {
            return Vec::new();
        };
        let mut out = vec![[p0.0, p0.1, p1.0, p1.1]]; // apex to radius ray
        if let Some(p2) = self.base.anchor_px(2, proj) {
            let r0 = (p1.0 - p0.0).hypot(p1.1 - p0.1);
            let len = (p2.0 - p0.0).hypot(p2.1 - p0.1);
            if len > 1e-6 {
                // the second ray re-projected to the same radius r0 (only its angle matters)
                out.push([
                    p0.0,
                    p0.1,
                    p0.0 + ((p2.0 - p0.0) / len) * r0,
                    p0.1 + ((p2.1 - p0.1) / len) * r0,
                ]);
            }
        }
        out
    }
}

radial_drawing!(FibWedge, free_schema(&["apex", "radius", "angle"]));

// ── spiral ──

/// The original's truncated constant, kept so the sampled points match it exactly.
#[allow(clippy::approx_constant)]
const PHI: f64 = 1.618_033_988_7;
/// Radius times phi per quarter turn: `r = r0 * e^(GROWTH * s)`.
fn growth() -> f64 {
    PHI.ln() / (PI / 2.0)
}
const STEP: f64 = PI / 64.0;
/// Sample a half-turn past the edge anchor.
const MAX_S: f64 = PI;
/// Stop winding inward once the radius is sub-pixel.
const MIN_R: f64 = 0.5;

/// A golden (logarithmic) Fibonacci spiral: a data-anchored center (p1, the
/// "eye") plus an edge anchor (p2) that fixes the initial pixel radius `r0 =
/// |p2 - p1|` and the rotation. The radius multiplies by phi every quarter
/// turn; sampled as a polyline, clockwise by default.
#[derive(Clone, Debug)]
pub struct FibSpiral {
    base: DrawingBase,
}

impl FibSpiral {
    pub const TYPE: &'static str = "fibspiral";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    /// The spiral sampled to pixel points (eye, through the edge anchor, a little beyond).
    pub fn spiral_points(&self, proj: &dyn Projector) -> Option<Vec<(f64, f64)>> {
        let (cx, cy) = self.base.anchor_px(0, proj)?;
        let (ex, ey) = self.base.anchor_px(1, proj)?;
        let r0 = (ex - cx).hypot(ey - cy);
        if r0 < 1.0 {
            return None;
        }
        let g = growth();
        let theta0 = (ey - cy).atan2(ex - cx);
        // Inward until sub-pixel (capped).
        let s_min = js_max((MIN_R / r0).ln() / g, -8.0 * PI * 2.0);
        let mut pts = Vec::new();
        let mut s = s_min;
        while s <= MAX_S {
            let r = r0 * (g * s).exp();
            let ang = theta0 + s; // clockwise on a y-down canvas
            pts.push((cx + r * ang.cos(), cy + r * ang.sin()));
            s += STEP;
        }
        Some(pts)
    }
}

impl Drawing for FibSpiral {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        free_schema(&["center", "edge"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.spiral_points(proj)
            .is_some_and(|pts| pts.len() >= 2 && dist_to_polyline(px, py, &pts) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        match (self.base.anchor_px(0, proj), self.base.anchor_px(1, proj)) {
            (Some(c), Some(e)) => vec![c, e],
            _ => Vec::new(),
        }
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let pts = self.handle_points(proj);
        if pts.len() < 2 {
            return None;
        }
        Bounds::of_points(&pts)
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ── registry ──

fn fib_style(line_style: LineStyle) -> DrawingStyle {
    DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, line_style)
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            FibRetracement::TYPE,
            "fibonacci",
            msg("drawing.fibretracement.label", "Fib Retracement"),
            svg24(
                r#"<line x1="3" y1="5" x2="21" y2="5"/><line x1="3" y1="10" x2="21" y2="10"/><line x1="3" y1="14" x2="21" y2="14"/><line x1="3" y1="19" x2="21" y2="19"/>"#,
            ),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibRetracement::new(init)),
        ),
        DrawingTypeMeta::new(
            FibExtension::TYPE,
            "fibonacci",
            msg("drawing.fibextension.label", "Fib Extension"),
            svg24(
                r#"<line x1="3" y1="6" x2="21" y2="6"/><line x1="3" y1="11" x2="21" y2="11"/><line x1="3" y1="15" x2="14" y2="15"/><line x1="3" y1="20" x2="14" y2="20"/>"#,
            ),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibExtension::new(init)),
        ),
        DrawingTypeMeta::new(
            FibExtensionTrend::TYPE,
            "fibonacci",
            msg(
                "drawing.fibextensiontrend.label",
                "Trend-Based Fib Extension",
            ),
            svg24(r#"<path d="M3 20 10 9l4 5"/><path d="M14 14h7M14 9h7M14 4h7"/>"#),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibExtensionTrend::new(init)),
        ),
        DrawingTypeMeta::new(
            FibFan::TYPE,
            "fibonacci",
            msg("drawing.fibfan.label", "Fib Fan"),
            svg24(r#"<path d="M4 20 20 5M4 20 21 11M4 20 21 17M4 20 20 20"/>"#),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibFan::new(init)),
        ),
        DrawingTypeMeta::new(
            FibTimeZones::TYPE,
            "fibonacci",
            msg("drawing.fibtimezones.label", "Fib Time Zones"),
            svg24(r#"<path d="M3 5v14M6 5v14M11 5v14M19 5v14"/>"#),
            fib_style(LineStyle::Dashed),
            |init| Box::new(FibTimeZones::new(init)),
        ),
        DrawingTypeMeta::new(
            FibChannel::TYPE,
            "fibonacci",
            msg("drawing.fibchannel.label", "Fib Channel"),
            svg24(r#"<path d="M3 18 18 9M5 21 20 12M6 13 21 4"/>"#),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibChannel::new(init)),
        ),
        DrawingTypeMeta::new(
            FibSpeedFan::TYPE,
            "fibonacci",
            msg("drawing.fibspeedfan.label", "Fib Speed Resistance Fan"),
            svg24(r#"<path d="M4 4 20 20M4 4 20 12M4 4 12 20M4 4 20 4M4 4 4 20"/>"#),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibSpeedFan::new(init)),
        ),
        DrawingTypeMeta::new(
            TrendFibTime::TYPE,
            "fibonacci",
            msg("drawing.trendfibtime.label", "Trend-Based Fib Time"),
            svg24(r#"<path d="M3 19 9 8M9 8 12 5v14M14 5v14M18 5v14M21 5v14"/>"#),
            fib_style(LineStyle::Dashed),
            |init| Box::new(TrendFibTime::new(init)),
        ),
        DrawingTypeMeta::new(
            FibCircles::TYPE,
            "fibonacci",
            msg("drawing.fibcircles.label", "Fib Circles"),
            svg24(
                r#"<circle cx="12" cy="12" r="3"/><circle cx="12" cy="12" r="6.5"/><circle cx="12" cy="12" r="10"/>"#,
            ),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibCircles::new(init)),
        ),
        DrawingTypeMeta::new(
            FibArcs::TYPE,
            "fibonacci",
            msg("drawing.fibarcs.label", "Fib Speed Resistance Arcs"),
            svg24(
                r#"<path d="M8 20a4 4 0 0 1 8 0"/><path d="M4.5 20a7.5 7.5 0 0 1 15 0"/><circle cx="12" cy="20" r="1.4" fill="currentColor" stroke="none"/>"#,
            ),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibArcs::new(init)),
        ),
        DrawingTypeMeta::new(
            FibWedge::TYPE,
            "fibonacci",
            msg("drawing.fibwedge.label", "Fib Wedge"),
            svg24(
                r#"<path d="M4 20 20 16M4 20 14 4"/><path d="M11.8 18.1A8 8 0 0 0 8.2 13.2"/><path d="M16.6 16.9A13 13 0 0 0 10.9 9"/>"#,
            ),
            fib_style(LineStyle::Solid),
            |init| Box::new(FibWedge::new(init)),
        ),
        DrawingTypeMeta::new(
            FibSpiral::TYPE,
            "fibonacci",
            msg("drawing.fibspiral.label", "Fib Spiral"),
            svg24(
                r#"<path d="M13 12a2 2 0 1 1-2-2 4 4 0 0 1 4 4 6 6 0 0 1-9 5.2 9 9 0 0 1 .5-15"/>"#,
            ),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 2.0, LineStyle::Solid),
            |init| Box::new(FibSpiral::new(init)),
        ),
    ]
}

/// Shared test scaffolding for the Fibonacci and Gann families.
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::json;

    use super::*;
    use crate::drawings::projector::LinearProjector;

    /// A data point at pixel x (time = x minutes) and a price.
    pub fn pt(x: f64, price: f64) -> DrawingPoint {
        DrawingPoint {
            time: x * 60_000.0,
            price,
        }
    }

    pub fn make(
        metas: &[DrawingTypeMeta],
        key: &str,
        anchors: Vec<DrawingPoint>,
        props: Option<Props>,
    ) -> Box<dyn Drawing> {
        let meta = metas.iter().find(|m| m.type_key == key).expect("meta");
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(meta.default_style.to_partial());
        init.props = props;
        (meta.create)(&init)
    }

    pub fn props(value: serde_json::Value) -> Props {
        value.as_object().cloned().unwrap_or_default()
    }

    /// Run every trait method on `d`; none may panic.
    pub fn exercise(d: &dyn Drawing) {
        let proj = LinearProjector::sample();
        let _ = d.hit_test(10.0, 10.0, &proj, 3.0);
        let _ = d.handle_points(&proj);
        let _ = d.hit_handle(10.0, 10.0, &proj, 3.0);
        let _ = d.bounds(&proj);
        let _ = d.price_range();
        let _ = d.time_extent();
        let _ = d.schema();
        let _ = d.write_props();
        let _ = d.editable_levels();
        let _ = d.serialize();
    }

    /// The conformance checks every type key owes: creation, schema, degraded
    /// anchor counts, and a lossless serialize round trip.
    pub fn check_family(metas: &[DrawingTypeMeta], expected: &[(&str, &[&str])]) {
        for (key, roles) in expected {
            let full: Vec<DrawingPoint> = (0..roles.len())
                .map(|i| pt(100.0 + 80.0 * i as f64, 100.0 + 20.0 * i as f64))
                .collect();
            let meta = metas.iter().find(|m| m.type_key == *key).expect("meta");
            assert_eq!(meta.group, "fibonacci", "{key}");
            for count in 0..=roles.len() {
                let d = make(metas, key, full[..count].to_vec(), None);
                assert_eq!(d.type_key(), *key);
                assert_eq!(d.base().style, meta.default_style, "{key}");
                let schema = d.anchor_schema();
                assert_eq!(schema.min, roles.len(), "{key}");
                assert_eq!(schema.max, roles.len(), "{key}");
                let got: Vec<&str> = schema.slots.iter().map(|s| s.role.as_str()).collect();
                assert_eq!(got, *roles, "{key}");
                assert_eq!(d.is_complete(), count == roles.len(), "{key}");
                exercise(d.as_ref());
            }
            let d = make(metas, key, full.clone(), None);
            let doc = d.serialize();
            let again = (meta.create)(&DrawingInit::from(doc.clone()));
            assert_eq!(again.serialize(), doc, "{key}");
            // A document whose props carry nothing useful still loads.
            let odd = make(
                metas,
                key,
                full,
                Some(props(
                    json!({ "levels": "x", "numbersSize": 3, "labelsSize": "tiny" }),
                )),
            );
            let levels =
                |d: &dyn Drawing| d.serialize().props.and_then(|p| p.get("levels").cloned());
            assert_eq!(levels(odd.as_ref()), levels(d.as_ref()), "{key}");
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::test_support::*;
    use super::*;
    use crate::drawings::projector::LinearProjector;

    const KEYS: [(&str, &[&str]); 12] = [
        ("fibretracement", &["p1", "p2"]),
        ("fibextension", &["p1", "p2"]),
        ("fibextensiontrend", &["a", "b", "c"]),
        ("fibfan", &["p1", "p2"]),
        ("fibtimezones", &["p1", "p2"]),
        ("fibchannel", &["p1", "p2", "offset"]),
        ("fibspeedfan", &["p1", "p2"]),
        ("trendfibtime", &["p1", "p2", "p3"]),
        ("fibcircles", &["p1", "p2"]),
        ("fibarcs", &["p1", "p2"]),
        ("fibwedge", &["apex", "radius", "angle"]),
        ("fibspiral", &["center", "edge"]),
    ];

    fn sample() -> LinearProjector {
        LinearProjector::sample()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn lines_of(d: &dyn Drawing) -> Vec<FibEntryLine> {
        let proj = sample();
        match d.type_key() {
            "fibretracement" => down::<FibRetracement>(d).entry_lines(&proj),
            "fibextensiontrend" => down::<FibExtensionTrend>(d).entry_lines(&proj),
            "fibfan" => down::<FibFan>(d).entry_lines(&proj),
            "fibtimezones" => down::<FibTimeZones>(d).entry_lines(&proj),
            "fibchannel" => down::<FibChannel>(d).entry_lines(&proj),
            "fibspeedfan" => down::<FibSpeedFan>(d).entry_lines(&proj),
            "trendfibtime" => down::<TrendFibTime>(d).entry_lines(&proj),
            _ => None,
        }
        .unwrap_or_default()
    }

    fn down<T: 'static>(d: &dyn Drawing) -> &T {
        d.as_any().downcast_ref::<T>().expect("downcast")
    }

    #[test]
    fn every_type_is_registered_with_its_schema_and_survives_sparse_anchors() {
        assert_eq!(metas().len(), 12);
        check_family(&metas(), &KEYS);
    }

    #[test]
    fn registry_entries_match_the_original() {
        let all = metas();
        let get = |k: &str| all.iter().find(|m| m.type_key == k).unwrap();
        assert_eq!(
            get("fibtimezones").default_style.line_style,
            LineStyle::Dashed
        );
        assert_eq!(
            get("trendfibtime").default_style.line_style,
            LineStyle::Dashed
        );
        assert_eq!(get("fibspiral").default_style.line_width, 2.0);
        assert_eq!(get("fibretracement").default_style.line_width, 1.0);
        assert_eq!(get("fibretracement").label.source_text(), "Fib Retracement");
        assert!(get("fibcircles").icon.contains(r#"r="6.5""#));
    }

    #[test]
    fn retracement_levels_and_handles() {
        let d = make(
            &metas(),
            "fibretracement",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = sample();
        let fib = down::<FibRetracement>(d.as_ref());
        let lines = fib.level_lines(&proj).unwrap();
        assert_eq!(lines.len(), 7);
        // Level 0 sits on the second anchor (price 150, y 200), level 1 on the first.
        assert!(close(lines[0].y, 200.0));
        assert!(close(lines[6].y, 400.0));
        let half = lines.iter().find(|l| l.ratio == 0.5).unwrap();
        assert!(close(half.price, 125.0) && close(half.y, 300.0));
        assert!(close(half.x1, 100.0) && close(half.x2, 300.0));
        let e = fib.entry_lines(&proj).unwrap();
        let half = e.iter().find(|l| l.y1 == 300.0).unwrap();
        assert_eq!(half.number_text, "0.5 (125.00)");
        assert_eq!((half.number_x, half.number_y), (104.0, 293.0));
        assert_eq!(half.number_align, FibAlign::Left);
        assert_eq!(half.label_x, 200.0);
        assert_eq!(d.handle_points(&proj), vec![(100.0, 400.0), (300.0, 200.0)]);
        assert_eq!(d.hit_handle(102.0, 402.0, &proj, 3.0), Some(0));
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            })
        );
        let range = d.price_range().unwrap();
        assert!(close(range.min, 100.0) && close(range.max, 150.0));
        assert!(d.hit_test(200.0, 300.0, &proj, 3.0));
        assert!(!d.hit_test(200.0, 310.0, &proj, 3.0));
        assert!(!d.hit_test(400.0, 300.0, &proj, 3.0));
        assert_eq!(
            d.time_extent().map(|s| (s.min, s.max)),
            Some((6_000_000.0, 18_000_000.0))
        );
    }

    #[test]
    fn retracement_extension_and_bands() {
        let mut d = make(
            &metas(),
            "fibretracement",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = sample();
        d.apply_setting("style.extendRight", &json!(true));
        assert!(d.hit_test(700.0, 300.0, &proj, 3.0));
        d.apply_setting("style.extendLeft", &json!(true));
        let fib = down::<FibRetracement>(d.as_ref());
        let l = &fib.level_lines(&proj).unwrap()[0];
        assert_eq!((l.x1, l.x2), (0.0, 1000.0));
        // Six bands join seven neighbouring levels; each takes the upper level's color.
        let bands = fib.fill_bands(&proj);
        assert_eq!(bands.len(), 6);
        assert!(close(bands[0].y, 200.0) && close(bands[0].h, 47.2));
        assert_eq!(bands[0].w, 1000.0);
        d.apply_setting("background", &json!(false));
        assert!(
            down::<FibRetracement>(d.as_ref())
                .fill_bands(&proj)
                .is_empty()
        );
    }

    #[test]
    fn level_text_follows_the_display_settings() {
        let mut d = make(
            &metas(),
            "fibretracement",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = sample();
        let text = |d: &dyn Drawing| {
            down::<FibRetracement>(d)
                .entry_lines(&proj)
                .unwrap()
                .iter()
                .find(|l| l.y1 == 300.0)
                .map(|l| l.number_text.clone())
                .unwrap()
        };
        d.apply_setting("showPrices", &json!(false));
        assert_eq!(text(d.as_ref()), "0.5");
        d.apply_settings(&props(json!({ "showPrices": true, "showRatios": false })));
        assert_eq!(text(d.as_ref()), "125.00");
        d.apply_setting("showPrices", &json!(false));
        assert_eq!(text(d.as_ref()), "");
        d.apply_setting("labelSide", &json!("right"));
        let first = down::<FibRetracement>(d.as_ref())
            .entry_lines(&proj)
            .unwrap()
            .remove(0);
        assert_eq!(first.number_align, FibAlign::Right);
        assert_eq!(first.number_x, 296.0);
        d.apply_setting("labelSide", &json!("middle"));
        assert_eq!(
            down::<FibRetracement>(d.as_ref()).levels_state().label_side,
            FibLabelSide::Right
        );
    }

    #[test]
    fn reverse_defaults_by_whether_props_are_present() {
        // A fresh drawing lays level 0 on the second anchor.
        let fresh = FibRetracement::new(&DrawingInit::on_pane("price"));
        assert!(!fresh.reverse);
        // Props present but without the flag: a pre-flag document, level 0 on the first anchor.
        let legacy = FibRetracement::new(
            &DrawingInit::on_pane("price").props(props(json!({ "background": false }))),
        );
        assert!(legacy.reverse);
        assert!(!legacy.levels_state().background);
        let explicit = FibRetracement::new(
            &DrawingInit::on_pane("price").props(props(json!({ "reverse": false }))),
        );
        assert!(!explicit.reverse);
        // A wrong-typed flag falls back like a missing one.
        let wrong = FibRetracement::new(
            &DrawingInit::on_pane("price").props(props(json!({ "reverse": "yes" }))),
        );
        assert!(wrong.reverse);
        let written = fresh.write_props().unwrap();
        assert_eq!(written["reverse"], json!(false));
        // Level 0 follows the flag.
        let proj = sample();
        let mut two = FibRetracement::new(
            &DrawingInit::on_pane("price").anchors(vec![pt(100.0, 100.0), pt(300.0, 150.0)]),
        );
        let zero = |d: &FibRetracement| d.level_lines(&proj).unwrap()[0].price;
        assert_eq!(zero(&two), 150.0);
        two.apply_setting("reverse", &json!(true));
        assert_eq!(zero(&two), 100.0);
    }

    #[test]
    fn level_settings_paths_edit_one_level_and_ignore_a_bad_index() {
        let mut d = FibRetracement::new(&DrawingInit::on_pane("price"));
        let before = d.fib_core().levels.clone();
        d.apply_setting("levels.1.color", &json!("#123456"));
        d.apply_setting("levels.2.enabled", &json!(false));
        d.apply_setting("levels.3.label", &json!("golden"));
        let lv = &d.fib_core().levels;
        assert_eq!(lv[1].color, "#123456");
        assert!(!lv[2].enabled);
        assert_eq!(lv[3].label.as_deref(), Some("golden"));
        assert_eq!(lv[0], before[0]);
        d.apply_setting("levels.3.label", &json!(""));
        assert_eq!(d.fib_core().levels[3].label, None);
        let snapshot = d.fib_core().levels.clone();
        d.apply_setting("levels.99.color", &json!("#ffffff"));
        d.apply_setting("levels.99.enabled", &json!(true));
        assert_eq!(d.fib_core().levels, snapshot);
        d.apply_setting("numbersSize", &json!("huge"));
        d.apply_setting("labelsSize", &json!("nope"));
        assert_eq!(d.fib_core().numbers_size, FibTextSize::Huge);
        assert_eq!(d.fib_core().labels_size, FibTextSize::Normal);
        assert_eq!(d.editable_levels().unwrap().len(), snapshot.len());
    }

    #[test]
    fn read_props_is_lenient_about_levels() {
        let d = FibExtension::new(&DrawingInit::on_pane("price").props(props(json!({
            "levels": [
                { "ratio": 0.5, "color": "#111111", "label": "mid" },
                { "ratio": "x", "color": "#222222" },
                { "ratio": 1, "color": 7 },
                { "ratio": 2, "color": "#333333", "enabled": false, "label": "" },
                "junk"
            ],
            "showRatios": "no",
            "numbersSize": "large"
        }))));
        assert_eq!(
            d.fib_core().levels,
            vec![
                FibLevel {
                    ratio: 0.5,
                    color: "#111111".into(),
                    enabled: true,
                    label: Some("mid".into())
                },
                FibLevel {
                    ratio: 2.0,
                    color: "#333333".into(),
                    enabled: false,
                    label: None
                },
            ]
        );
        assert!(d.levels_state().show_ratios);
        assert_eq!(d.fib_core().numbers_size, FibTextSize::Large);
        // An empty parse keeps the defaults.
        let kept = FibExtension::new(
            &DrawingInit::on_pane("price").props(props(json!({ "levels": [{ "ratio": 1 }] }))),
        );
        assert_eq!(kept.fib_core().levels.len(), 19);
    }

    #[test]
    fn extension_defaults() {
        let d = FibExtension::new(&DrawingInit::on_pane("price"));
        let on: Vec<f64> = d
            .fib_core()
            .levels
            .iter()
            .filter(|l| l.enabled)
            .map(|l| l.ratio)
            .collect();
        assert_eq!(on, [0.0, 0.382, 0.618, 1.0, 1.272, 1.618, 2.618]);
        let last = d
            .fib_core()
            .levels
            .iter()
            .find(|l| l.ratio == 2.618)
            .unwrap();
        assert_eq!(last.color, LEVEL_PURPLE);
        let two = FibExtension::new(
            &DrawingInit::on_pane("price").anchors(vec![pt(100.0, 100.0), pt(200.0, 150.0)]),
        );
        // Level 0 on the first anchor: 100 + 2.618 * 50.
        let range = two.price_range().unwrap();
        assert!(close(range.min, 100.0) && close(range.max, 230.9));
    }

    #[test]
    fn trend_extension_projects_the_ab_move_from_c() {
        let d = make(
            &metas(),
            "fibextensiontrend",
            vec![pt(100.0, 100.0), pt(200.0, 150.0), pt(300.0, 130.0)],
            None,
        );
        let proj = sample();
        let lines = lines_of(d.as_ref());
        assert_eq!(lines.len(), 7);
        let half = &lines[2];
        assert!(close(half.y1, (200.0 - 155.0) / 0.25));
        assert_eq!((half.x1, half.x2), (100.0, 300.0));
        assert_eq!(half.number_text, "0.5 (155.00)");
        let range = d.price_range().unwrap();
        assert!(close(range.min, 100.0) && close(range.max, 260.9));
        assert!(d.hit_test(200.0, 180.0, &proj, 3.0));
    }

    #[test]
    fn fan_rays_run_from_the_pivot_through_the_levels() {
        let d = make(
            &metas(),
            "fibfan",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let proj = sample();
        let fan = down::<FibFan>(d.as_ref()).fan_lines(&proj).unwrap();
        assert_eq!(fan.len(), 6);
        let half = fan.iter().find(|l| l.ratio == 0.5).unwrap();
        assert_eq!((half.x1, half.y1), (100.0, 400.0));
        assert!(close(half.x2, 1002.0) && close(half.y2, -51.0));
        assert_eq!((half.label_x, half.label_y), (305.0, 300.0));
        let e = lines_of(d.as_ref());
        let half = e.iter().find(|l| l.number_text == "0.5").unwrap();
        assert_eq!(half.label_y, 313.0);
        assert!(d.hit_test(200.0, 350.0, &proj, 3.0));
        assert!(!d.hit_test(200.0, 450.0, &proj, 3.0));
        assert!(
            down::<FibFan>(make(&metas(), "fibfan", vec![pt(1.0, 1.0)], None).as_ref())
                .fan_lines(&proj)
                .is_none()
        );
    }

    #[test]
    fn time_zones_project_forward_and_ignore_price() {
        let d = make(
            &metas(),
            "fibtimezones",
            vec![pt(100.0, 100.0), pt(200.0, 150.0)],
            None,
        );
        let proj = sample();
        let zones = down::<FibTimeZones>(d.as_ref()).zone_lines(&proj).unwrap();
        assert_eq!(zones.len(), 9);
        assert!(close(zones[0].x, 100.0) && close(zones[3].x, 400.0));
        assert_eq!(zones[3].n, 3.0);
        let e = lines_of(d.as_ref());
        assert_eq!((e[3].y1, e[3].y2), (0.0, 600.0));
        assert_eq!((e[3].number_y, e[3].label_y), (9.0, 24.0));
        assert_eq!(e[3].number_align, FibAlign::Center);
        assert!(d.price_range().is_none() && d.time_extent().is_none());
        assert!(d.hit_test(400.0, 50.0, &proj, 3.0));
        // Equal times give no unit, so no zones.
        let flat = make(
            &metas(),
            "fibtimezones",
            vec![pt(100.0, 100.0), pt(100.0, 150.0)],
            None,
        );
        assert!(
            down::<FibTimeZones>(flat.as_ref())
                .zone_lines(&proj)
                .is_none()
        );
        assert!(!flat.hit_test(100.0, 50.0, &proj, 3.0));
    }

    #[test]
    fn channel_offsets_the_baseline_by_the_third_anchor() {
        let d = make(
            &metas(),
            "fibchannel",
            vec![pt(100.0, 100.0), pt(300.0, 100.0), pt(100.0, 150.0)],
            None,
        );
        let lines = lines_of(d.as_ref());
        assert_eq!(lines.len(), 9);
        let one = lines.iter().find(|l| l.number_text == "1").unwrap();
        assert!(close(one.y1, 200.0) && close(one.y2, 200.0));
        assert_eq!((one.x1, one.x2), (100.0, 300.0));
        assert_eq!(
            (one.number_x, one.number_y, one.label_y),
            (305.0, 200.0, 213.0)
        );
        let range = d.price_range().unwrap();
        assert!(close(range.min, 100.0) && close(range.max, 230.9));
        // Without the offset anchor the levels collapse onto the baseline.
        let two = make(
            &metas(),
            "fibchannel",
            vec![pt(100.0, 100.0), pt(300.0, 100.0)],
            None,
        );
        assert!(lines_of(two.as_ref()).iter().all(|l| l.y1 == 400.0));
        let range = two.price_range().unwrap();
        assert_eq!((range.min, range.max), (100.0, 100.0));
        // No enabled level, no range.
        let mut none = make(
            &metas(),
            "fibchannel",
            vec![pt(100.0, 100.0), pt(300.0, 100.0)],
            None,
        );
        none.read_props(&props(
            json!({ "levels": [{ "ratio": 1, "color": "#fff", "enabled": false }] }),
        ));
        assert!(none.price_range().is_none());
    }

    #[test]
    fn speed_fan_emits_a_price_ray_and_a_time_ray_per_level() {
        let d = make(
            &metas(),
            "fibspeedfan",
            vec![pt(100.0, 100.0), pt(300.0, 150.0)],
            None,
        );
        let lines = lines_of(d.as_ref());
        // Seven levels, two rays each, except the ratio-1 time ray.
        assert_eq!(lines.len(), 13);
        let price_ray = &lines[6];
        assert_eq!(price_ray.number_text, "0.5");
        assert!(close(price_ray.x2, 1002.0) && close(price_ray.y2, -51.0));
        assert_eq!((price_ray.number_x, price_ray.number_y), (305.0, 300.0));
        assert_eq!(price_ray.number_align, FibAlign::Left);
        let time_ray = &lines[7];
        assert_eq!(time_ray.number_align, FibAlign::Center);
        assert_eq!((time_ray.number_x, time_ray.number_y), (200.0, 188.0));
        assert_eq!(time_ray.label_y, 212.0);
        // A pivot to the left of the box flips the direction.
        let back = make(
            &metas(),
            "fibspeedfan",
            vec![pt(300.0, 100.0), pt(100.0, 150.0)],
            None,
        );
        let l = lines_of(back.as_ref());
        assert_eq!(l[0].number_align, FibAlign::Right);
    }

    #[test]
    fn trend_time_projects_from_the_third_anchor() {
        let d = make(
            &metas(),
            "trendfibtime",
            vec![pt(100.0, 100.0), pt(200.0, 150.0), pt(500.0, 120.0)],
            None,
        );
        let proj = sample();
        let lines = lines_of(d.as_ref());
        assert_eq!(lines.len(), 10); // the 0.5 level is off by default
        assert!(close(lines[0].x1, 500.0));
        let one = lines.iter().find(|l| l.number_text == "1").unwrap();
        assert!(close(one.x1, 600.0));
        assert!(d.price_range().is_none() && d.time_extent().is_none());
        assert!(d.hit_test(600.0, 300.0, &proj, 3.0));
        let flat = make(
            &metas(),
            "trendfibtime",
            vec![pt(100.0, 100.0), pt(100.0, 150.0), pt(500.0, 120.0)],
            None,
        );
        assert!(!flat.hit_test(500.0, 300.0, &proj, 3.0));
    }

    #[test]
    fn sweeps_wrap_and_close() {
        assert!(angle_in_sweep(0.5, 0.0, 1.0));
        assert!(!angle_in_sweep(2.0, 0.0, 1.0));
        assert!(angle_in_sweep(-PI + 0.1, PI - 0.2, PI + 0.5));
        assert!(angle_in_sweep(3.0, 0.0, PI * 2.0));
        assert!(!angle_in_sweep(f64::NAN, 0.0, 1.0));
    }

    #[test]
    fn circles_hit_on_their_rings() {
        let d = make(
            &metas(),
            "fibcircles",
            vec![pt(200.0, 100.0), pt(300.0, 100.0)],
            None,
        );
        let proj = sample();
        let g = down::<FibCircles>(d.as_ref()).radial(&proj).unwrap();
        assert_eq!((g.cx, g.cy, g.r0, g.a0), (200.0, 400.0, 100.0, 0.0));
        assert!(close(g.a1, PI * 2.0));
        assert!(d.hit_test(250.0, 400.0, &proj, 3.0)); // ratio 0.5
        assert!(d.hit_test(200.0, 300.0, &proj, 3.0)); // ratio 1, above
        assert!(!d.hit_test(255.0, 400.0, &proj, 3.0));
        assert!(
            down::<FibCircles>(d.as_ref())
                .entry_lines(&proj)
                .unwrap()
                .is_empty()
        );
        let range = d.price_range().unwrap();
        assert_eq!((range.min, range.max), (100.0, 100.0));
        assert!(d.price_range().is_some());
    }

    #[test]
    fn arcs_bulge_toward_the_far_anchor() {
        let d = make(
            &metas(),
            "fibarcs",
            vec![pt(100.0, 100.0), pt(300.0, 100.0)],
            None,
        );
        let proj = sample();
        let arcs = down::<FibArcs>(d.as_ref());
        let g = arcs.radial(&proj).unwrap();
        assert!(close(g.r0, 200.0) && close(g.a0, -PI / 2.0) && close(g.a1, PI / 2.0));
        assert_eq!(
            arcs.bounding_lines(&proj),
            vec![[100.0, 400.0, 300.0, 400.0]]
        );
        // On the ratio-0.5 ring toward the far anchor, but not behind the pivot.
        assert!(
            d.hit_test(200.0, 400.0, &proj, 3.0) || d.hit_test(100.0 + 100.0, 400.0, &proj, 3.0)
        );
        assert!(d.hit_test(200.0, 400.0, &proj, 1.0)); // on the baseline
        assert!(!d.hit_test(0.0, 400.0, &proj, 3.0));
        let lone = make(&metas(), "fibarcs", vec![pt(100.0, 100.0)], None);
        assert!(
            down::<FibArcs>(lone.as_ref())
                .bounding_lines(&proj)
                .is_empty()
        );
    }

    #[test]
    fn wedge_takes_the_minor_sweep_and_two_rays() {
        let d = make(
            &metas(),
            "fibwedge",
            vec![pt(100.0, 100.0), pt(200.0, 100.0), pt(100.0, 150.0)],
            None,
        );
        let proj = sample();
        let wedge = down::<FibWedge>(d.as_ref());
        let g = wedge.radial(&proj).unwrap();
        assert_eq!((g.cx, g.cy, g.r0), (100.0, 400.0, 100.0));
        assert!(close(g.a0, -PI / 2.0) && close(g.a1, 0.0));
        let rays = wedge.bounding_lines(&proj);
        assert_eq!(rays.len(), 2);
        assert!(close(rays[1][2], 100.0) && close(rays[1][3], 300.0));
        assert!(d.hit_test(150.0, 400.0, &proj, 3.0)); // ratio 0.5 on the first ray
        assert!(!d.hit_test(150.0, 450.0, &proj, 3.0));
        // A wrap past pi takes the other way round.
        let wrap = make(
            &metas(),
            "fibwedge",
            vec![pt(100.0, 100.0), pt(0.0, 100.0), pt(100.0, 50.0)],
            None,
        );
        let g = down::<FibWedge>(wrap.as_ref()).radial(&proj).unwrap();
        assert!(g.a1 - g.a0 <= PI + 1e-9);
        // A base radius under a pixel is no wedge, but the first ray still shows.
        let tiny = make(
            &metas(),
            "fibwedge",
            vec![pt(100.0, 100.0), pt(100.0, 100.0), pt(100.0, 150.0)],
            None,
        );
        assert!(down::<FibWedge>(tiny.as_ref()).radial(&proj).is_none());
        let two = make(
            &metas(),
            "fibwedge",
            vec![pt(100.0, 100.0), pt(200.0, 100.0)],
            None,
        );
        assert!(down::<FibWedge>(two.as_ref()).radial(&proj).is_none());
        assert_eq!(
            down::<FibWedge>(two.as_ref()).bounding_lines(&proj).len(),
            1
        );
        assert!(two.hit_test(150.0, 400.0, &proj, 3.0));
    }

    #[test]
    fn spiral_winds_through_the_edge_anchor() {
        let d = make(
            &metas(),
            "fibspiral",
            vec![pt(200.0, 100.0), pt(300.0, 100.0)],
            None,
        );
        let proj = sample();
        let spiral = down::<FibSpiral>(d.as_ref());
        let pts = spiral.spiral_points(&proj).unwrap();
        assert!(pts.len() > 400);
        let first = pts[0];
        assert!(first.0.is_finite() && (first.0 - 200.0).hypot(first.1 - 400.0) < 1.0);
        assert!(d.hit_test(300.0, 400.0, &proj, 3.0));
        assert!(!d.hit_test(900.0, 100.0, &proj, 3.0));
        assert_eq!(d.handle_points(&proj), vec![(200.0, 400.0), (300.0, 400.0)]);
        let b = d.bounds(&proj).unwrap();
        assert_eq!((b.x, b.w, b.h), (200.0, 100.0, 0.0));
        let range = d.price_range().unwrap();
        assert_eq!((range.min, range.max), (100.0, 100.0));
        assert!(d.write_props().is_none());
        // A sub-pixel radius draws nothing.
        let dot = make(
            &metas(),
            "fibspiral",
            vec![pt(200.0, 100.0), pt(200.0, 100.0)],
            None,
        );
        assert!(
            down::<FibSpiral>(dot.as_ref())
                .spiral_points(&proj)
                .is_none()
        );
        assert!(!dot.hit_test(200.0, 400.0, &proj, 3.0));
        assert_eq!(
            d.schema()
                .fields
                .iter()
                .filter(|f| f.path.starts_with("text."))
                .count(),
            5
        );
    }

    #[test]
    fn the_shared_schema_drops_the_line_color() {
        let d = FibRetracement::new(&DrawingInit::on_pane("price"));
        let paths: Vec<String> = d.schema().fields.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [
                "style.lineWidth",
                "style.lineStyle",
                "style.extendLeft",
                "style.extendRight"
            ]
        );
        let fan = FibFan::new(&DrawingInit::on_pane("price"));
        assert_eq!(fan.schema().fields.len(), 2);
    }

    #[test]
    fn a_gone_pane_degrades() {
        let mut d = FibRetracement::new(&DrawingInit::on_pane("elsewhere"));
        d.base.anchors = vec![pt(100.0, 100.0), pt(300.0, 150.0)];
        let proj = sample();
        assert!(d.handle_points(&proj).is_empty());
        assert!(d.bounds(&proj).is_none());
        assert!(d.level_lines(&proj).unwrap().is_empty());
        assert!(!d.hit_test(200.0, 300.0, &proj, 3.0));
    }
}
