//! Measurement tools: the Date & Price Range box, the long/short Position
//! tool and the Magnifier inset.
//!
//! The painted label text is built in English only. The original routes it
//! through the (unported) translator and number-format layer; here every
//! numeric result stays available as data (`measure_stats`, `rr`, `reward_pct`,
//! `dollar_loss`, ...) next to the English lines.

use std::any::Any;

use serde_json::{Value, json};

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, PlacementMode, Props,
    Span, apply_setting_default,
};
use crate::drawings::geometry::{DrawingPoint, FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_segment, point_in_box};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, FieldOption, SettingsField, SettingsSchema, border_fields, line_fields,
    text_size_options,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::i18n::{MessageLike, msg};
use crate::indicators::classics::math::{js_max, js_round};
use crate::model::LineStyle;
use crate::palette::{INVALID, VALID};
use crate::util::{js_number, to_fixed};

/// The group id of every tool in this family.
const GROUP: &str = "measure";

/// The pixel rectangle between the first two anchors.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CornerRect {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl CornerRect {
    fn of(base: &DrawingBase, proj: &dyn Projector) -> Option<Self> {
        let (x1, y1) = base.anchor_px(0, proj)?;
        let (x2, y2) = base.anchor_px(1, proj)?;
        Some(Self { x1, y1, x2, y2 })
    }

    fn handles(&self) -> Vec<(f64, f64)> {
        vec![(self.x1, self.y1), (self.x2, self.y2)]
    }

    fn bounds(&self) -> Bounds {
        Bounds {
            x: self.x1.min(self.x2),
            y: self.y1.min(self.y2),
            w: (self.x2 - self.x1).abs(),
            h: (self.y2 - self.y1).abs(),
        }
    }
}

/// The price span of the first two anchors.
fn two_anchor_price_range(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Some(Span {
        min: a.price.min(b.price),
        max: a.price.max(b.price),
    })
}

fn corner_anchor_schema() -> AnchorSchema {
    AnchorSchema::free(&["c1", "c2"])
}

// ── Date & Price Range ──

/// Signed value with 2 decimals (e.g. "+523.45").
fn signed(n: f64) -> String {
    format!("{}{}", if n >= 0.0 { "+" } else { "" }, to_fixed(n, 2))
}

/// A compact duration string for a millisecond span (e.g. "2d 4h", "3h 15m").
pub fn format_duration(ms: f64) -> String {
    const DAY: f64 = 86_400_000.0;
    const HR: f64 = 3_600_000.0;
    const MIN: f64 = 60_000.0;
    let m = ms.abs();
    // `h ? ... : ...` in the original: NaN and 0 are falsy.
    let nonzero = |v: f64| v != 0.0 && !v.is_nan();
    if m >= DAY {
        let d = (m / DAY).floor();
        let h = js_round((m % DAY) / HR);
        return if nonzero(h) {
            format!("{}d {}h", js_number(d), js_number(h))
        } else {
            format!("{}d", js_number(d))
        };
    }
    if m >= HR {
        let h = (m / HR).floor();
        let mn = js_round((m % HR) / MIN);
        return if nonzero(mn) {
            format!("{}h {}m", js_number(h), js_number(mn))
        } else {
            format!("{}h", js_number(h))
        };
    }
    if m >= MIN {
        return format!("{}m", js_number(js_round(m / MIN)));
    }
    format!("{}s", js_number(js_round(m / 1000.0)))
}

/// The numbers behind a measurement box's two label lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureStats {
    /// Later price minus earlier price.
    pub delta: f64,
    /// `delta` as a percent of the earlier price (0 when that price is 0).
    pub percent: f64,
    /// Later time minus earlier time, in ms (negative when drawn right to left).
    pub duration_ms: f64,
    /// Whole bars between the anchors; `None` when the projector cannot tell.
    pub bars: Option<f64>,
}

/// The measurement box (the persistent Date & Price Range drawing): a
/// two-corner box tinted green/red by direction, with a centered label of the
/// price delta and/or the date delta. Each line is toggled via `show_price` /
/// `show_date`, and the label is styled by the drawing's `text` color/size, so
/// the one tool covers the price-only, date-only and combined cases.
#[derive(Clone, Debug)]
pub struct DatePriceRange {
    base: DrawingBase,
    /// Show the `Δprice (Δ%)` line.
    pub show_price: bool,
    /// Show the `N bars, duration` line.
    pub show_date: bool,
}

/// The original's abstract base of the range tools; the Date & Price Range is
/// its only concrete type.
pub type MeasureBox = DatePriceRange;

impl DatePriceRange {
    pub const TYPE: &'static str = "datepricerange";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            show_price: true,
            show_date: true,
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// Up (later price >= earlier) is green; else red.
    pub fn is_up(&self) -> bool {
        match (self.base.anchors.first(), self.base.anchors.get(1)) {
            (Some(a), Some(b)) => b.price >= a.price,
            _ => false,
        }
    }

    /// The numbers between the two anchors; `None` without both.
    pub fn measure_stats(&self, proj: &dyn Projector) -> Option<MeasureStats> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let delta = b.price - a.price;
        Some(MeasureStats {
            delta,
            percent: if a.price != 0.0 {
                (delta / a.price) * 100.0
            } else {
                0.0
            },
            duration_ms: b.time - a.time,
            bars: proj.bars_between(a.time, b.time).map(js_round),
        })
    }

    /// The measurement lines (English), filtered by the enabled toggles.
    pub fn measure_label(&self, proj: &dyn Projector) -> Vec<String> {
        let mut lines = Vec::new();
        if self.show_price {
            lines.push(self.price_label());
        }
        if self.show_date {
            lines.push(self.time_label(proj));
        }
        lines
    }

    /// Formatted `Δprice (Δ%)` between the two anchors (empty without both).
    pub fn price_label(&self) -> String {
        let (Some(a), Some(b)) = (self.base.anchors.first(), self.base.anchors.get(1)) else {
            return String::new();
        };
        let delta = b.price - a.price;
        let percent = if a.price != 0.0 {
            (delta / a.price) * 100.0
        } else {
            0.0
        };
        format!("{} ({}%)", signed(delta), signed(percent))
    }

    /// Formatted `N bars, duration` between the two anchors (bars only when
    /// the projector knows); empty without both anchors.
    pub fn time_label(&self, proj: &dyn Projector) -> String {
        let (Some(a), Some(b)) = (self.base.anchors.first(), self.base.anchors.get(1)) else {
            return String::new();
        };
        let duration = format_duration(b.time - a.time);
        match proj.bars_between(a.time, b.time).map(js_round) {
            Some(bars) => {
                let unit = if bars == 1.0 { "bar" } else { "bars" };
                format!("{} {unit}, {duration}", js_number(bars))
            }
            None => duration,
        }
    }
}

impl Drawing for DatePriceRange {
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
        corner_anchor_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        // The shaded box is grabbable anywhere inside.
        CornerRect::of(&self.base, proj)
            .is_some_and(|r| point_in_box(px, py, r.x1, r.y1, r.x2, r.y2, tol))
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        CornerRect::of(&self.base, proj)
            .map(|r| r.handles())
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        CornerRect::of(&self.base, proj).map(|r| r.bounds())
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = vec![
            SettingsField::new(
                "showPrice",
                msg("drawing.measure.showPrice", "Show price"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior),
            SettingsField::new(
                "showDate",
                msg("drawing.measure.showDate", "Show date"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior),
        ];
        // The box border only: the fill is direction-tinted.
        fields.extend(
            line_fields()
                .into_iter()
                .filter(|f| f.path != "style.lineColor"),
        );
        fields.push(
            SettingsField::new(
                "text.color",
                msg("drawing.field.textColor", "Text color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Text),
        );
        fields.push(
            SettingsField::new(
                "text.size",
                msg("drawing.field.textSize", "Text size"),
                FieldKind::Select,
            )
            .options(text_size_options())
            .group(FieldGroup::Text),
        );
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("showPrice".into(), json!(self.show_price));
        props.insert("showDate".into(), json!(self.show_date));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        if let Some(v) = props.get("showPrice").and_then(Value::as_bool) {
            self.show_price = v;
        }
        if let Some(v) = props.get("showDate").and_then(Value::as_bool) {
            self.show_date = v;
        }
    }
}

// ── Long/Short Position ──

/// How the gear panel's stop/target inputs interpret their value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionLevelMode {
    Price,
    Points,
}

/// The stop or the target of a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionLevel {
    Stop,
    Target,
}

/// The trade's direction (long and short mirror the levels across the entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionDirection {
    Long,
    Short,
}

/// Choices for the direction select.
pub fn direction_options() -> Vec<FieldOption> {
    vec![
        FieldOption::new("long", msg("drawing.position.option.long", "Long")),
        FieldOption::new("short", msg("drawing.position.option.short", "Short")),
    ]
}

/// The three price levels of a position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionPrices {
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
}

/// Pixel layout for the painter: the box x-range plus each level's y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionLayout {
    pub x1: f64,
    pub x2: f64,
    pub ey: f64,
    pub sy: f64,
    pub ty: f64,
}

/// Default reward:risk used to derive the stop from the placed (or default) target.
const DEFAULT_RR: f64 = 2.0;
/// Below this vertical drag (media px) a placement is treated as a bare click: a default box.
const MIN_DRAG_PX: f64 = 6.0;
const DEFAULT_RISK_PX: f64 = 64.0;
const DEFAULT_WIDTH_PX: f64 = 150.0;
const MIN_WIDTH_PX: f64 = 48.0;
const DEFAULT_RISK_PERCENT: f64 = 1.0;
const DEFAULT_ACCOUNT_BALANCE: f64 = 10_000.0;

/// `Math.sign`: NaN stays NaN, zero stays zero.
fn js_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        x
    }
}

/// A long/short position tool. Two clicks place the entry (anchor 0) and the
/// target (anchor 1); `on_placed` derives the stop and rewrites the anchors as
/// `[entry, stop, target]`. The second click is the *profit* direction:
/// dragging higher from the entry paints the green reward zone above (a long),
/// lower paints it below (a short). The shaded reward (entry to target) and
/// risk (entry to stop) zones carry a computed risk:reward, target/stop
/// percentages and, from the account balance and risk % settings, the amount
/// lost at the stop and the position size.
#[derive(Clone, Debug)]
pub struct PositionTool {
    base: DrawingBase,
    /// Percent of account balance risked on the stop (drives loss amount + size).
    pub risk_percent: f64,
    /// Account balance, in no particular currency (so the loss label is a plain number).
    pub account_balance: f64,
    /// Master toggle for every painted label.
    pub show_text: bool,
    /// Show the `DIR · R:R` header line above the box.
    pub show_header: bool,
    /// Append the level price to the target/stop labels.
    pub show_prices: bool,
    /// Show the `Loss · Size` line above the box.
    pub show_loss_size: bool,
    /// Show the `Target +%` label inside the reward zone.
    pub show_target_label: bool,
    /// Show the `Stop -%` label inside the risk zone.
    pub show_stop_label: bool,
    /// Fill + line color of the reward zone.
    pub profit_color: String,
    /// Fill + line color of the risk zone.
    pub loss_color: String,
}

impl PositionTool {
    pub const TYPE: &'static str = "position";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            risk_percent: DEFAULT_RISK_PERCENT,
            account_balance: DEFAULT_ACCOUNT_BALANCE,
            show_text: true,
            show_header: true,
            show_prices: false,
            show_loss_size: true,
            show_target_label: true,
            show_stop_label: true,
            profit_color: VALID.to_string(),
            loss_color: INVALID.to_string(),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// `"LONG"` or `"SHORT"`, computed from geometry so it flips live as the
    /// drag crosses the entry (target above entry is long; below is short).
    pub fn direction_label(&self) -> &'static str {
        match self.prices() {
            Some(p) if p.target >= p.entry => "LONG",
            _ => "SHORT",
        }
    }

    /// The direction as read by the settings path: reflects the geometry.
    pub fn direction(&self) -> PositionDirection {
        if self.direction_label() == "LONG" {
            PositionDirection::Long
        } else {
            PositionDirection::Short
        }
    }

    /// Writing the opposite direction mirrors the stop AND the target across
    /// the entry, turning the trade around in place while preserving each
    /// side's distance (and therefore the R:R). Needs all three levels.
    pub fn set_direction(&mut self, v: PositionDirection) {
        if self.base.anchors.len() < 3 || v == self.direction() {
            return;
        }
        let entry = self.base.anchors[0].price;
        for a in &mut self.base.anchors[1..3] {
            a.price = 2.0 * entry - a.price;
        }
    }

    /// Position size as a settings path: the computed size.
    pub fn quantity(&self) -> f64 {
        self.position_size()
    }

    /// Writing back-solves the risk % so that `size x stop distance = balance
    /// x risk%` holds for the typed size.
    pub fn set_quantity(&mut self, v: f64) {
        let Some(p) = self.prices() else {
            return;
        };
        if !v.is_finite() || v < 0.0 {
            return;
        }
        let dist = (p.entry - p.stop).abs();
        if dist < 1e-9 || self.account_balance <= 0.0 {
            return;
        }
        self.risk_percent = ((v * dist) / self.account_balance) * 100.0;
    }

    // Price levels as settings paths (the gear panel patches these directly).

    pub fn entry_price(&self) -> f64 {
        self.base.anchors.first().map_or(0.0, |a| a.price)
    }

    pub fn set_entry_price(&mut self, v: f64) {
        self.set_level_price(0, v);
    }

    pub fn stop_price(&self) -> f64 {
        self.base.anchors.get(1).map_or(0.0, |a| a.price)
    }

    pub fn set_stop_price(&mut self, v: f64) {
        self.set_level_price(1, v);
    }

    pub fn target_price(&self) -> f64 {
        self.base.anchors.get(2).map_or(0.0, |a| a.price)
    }

    pub fn set_target_price(&mut self, v: f64) {
        self.set_level_price(2, v);
    }

    fn set_level_price(&mut self, index: usize, v: f64) {
        if !v.is_finite() {
            return;
        }
        if let Some(a) = self.base.anchors.get_mut(index) {
            a.price = v;
            self.constrain_handle_drag(index);
        }
    }

    /// The three levels (entry, stop, target), deriving the stop while only
    /// two anchors exist (the live drag preview, before `on_placed` sets the
    /// precise box). During preview the second anchor is the target; after
    /// placement the anchors are `[entry, stop, target]`.
    fn resolved(&self) -> Option<[DrawingPoint; 3]> {
        let e = *self.base.anchors.first()?;
        let a1 = *self.base.anchors.get(1)?;
        if let Some(a2) = self.base.anchors.get(2) {
            return Some([e, a1, *a2]);
        }
        // Live preview: a1 is the target; the stop sits opposite at 1/R:R of the reward.
        let stop = DrawingPoint {
            time: a1.time,
            price: e.price - (a1.price - e.price) / DEFAULT_RR,
        };
        Some([e, stop, a1])
    }

    /// The entry, stop and target prices; `None` below two anchors.
    pub fn prices(&self) -> Option<PositionPrices> {
        let [e, s, t] = self.resolved()?;
        Some(PositionPrices {
            entry: e.price,
            stop: s.price,
            target: t.price,
        })
    }

    /// Risk:reward ratio = |target - entry| / |entry - stop| (0 when risk is degenerate).
    pub fn rr(&self) -> f64 {
        let Some(p) = self.prices() else {
            return 0.0;
        };
        let risk = (p.entry - p.stop).abs();
        if risk < 1e-9 {
            0.0
        } else {
            (p.target - p.entry).abs() / risk
        }
    }

    /// Reward % = |target - entry| / entry * 100.
    pub fn reward_pct(&self) -> f64 {
        match self.prices() {
            Some(p) if p.entry != 0.0 => ((p.target - p.entry).abs() / p.entry) * 100.0,
            _ => 0.0,
        }
    }

    /// Risk % of price = |entry - stop| / entry * 100 (distinct from the account `risk_percent`).
    pub fn risk_pct(&self) -> f64 {
        match self.prices() {
            Some(p) if p.entry != 0.0 => ((p.entry - p.stop).abs() / p.entry) * 100.0,
            _ => 0.0,
        }
    }

    /// Amount risked at the stop, in the balance's units = balance x risk% / 100.
    pub fn dollar_loss(&self) -> f64 {
        js_max(0.0, self.account_balance) * (js_max(0.0, self.risk_percent) / 100.0)
    }

    /// Position size in units = `dollar_loss` / |entry - stop| (0 when risk is degenerate).
    pub fn position_size(&self) -> f64 {
        let Some(p) = self.prices() else {
            return 0.0;
        };
        let risk = (p.entry - p.stop).abs();
        if risk < 1e-9 {
            0.0
        } else {
            self.dollar_loss() / risk
        }
    }

    // Painted label lines (each respects its own toggle; the painter checks `show_text`).

    /// `DIR  ·  R:R x.xx`: the always-on header when text is shown.
    pub fn header_label(&self) -> String {
        format!(
            "{}  ·  R:R {}",
            self.direction_label(),
            to_fixed(self.rr(), 2)
        )
    }

    /// `Loss …  ·  Size …`; the painter hides it when `show_loss_size` is off.
    pub fn loss_size_label(&self) -> String {
        format!(
            "Loss {}  ·  Size {}",
            format_money(self.dollar_loss()),
            format_qty(self.position_size())
        )
    }

    /// `Target +x.xx%`, with the level price appended when `show_prices` is on.
    pub fn target_label(&self) -> String {
        let percent = to_fixed(self.reward_pct(), 2);
        match self.prices() {
            Some(p) if self.show_prices => {
                format!("Target +{percent}%  @ {}", format_price(p.target))
            }
            _ => format!("Target +{percent}%"),
        }
    }

    /// `Stop −x.xx%` (a true minus sign), with the level price appended when
    /// `show_prices` is on.
    pub fn stop_label(&self) -> String {
        let percent = to_fixed(self.risk_pct(), 2);
        match self.prices() {
            Some(p) if self.show_prices => {
                format!("Stop \u{2212}{percent}%  @ {}", format_price(p.stop))
            }
            _ => format!("Stop \u{2212}{percent}%"),
        }
    }

    // Stop/target input units (the gear panel's Price / Points dropdowns).

    /// The stop or target in the given unit: the absolute price, or the
    /// distance from the entry in price points.
    pub fn level_display_value(&self, level: PositionLevel, mode: PositionLevelMode) -> f64 {
        let Some(p) = self.prices() else {
            return 0.0;
        };
        let price = match level {
            PositionLevel::Stop => p.stop,
            PositionLevel::Target => p.target,
        };
        match mode {
            PositionLevelMode::Price => round_float(price),
            PositionLevelMode::Points => round_float((price - p.entry).abs()),
        }
    }

    /// The absolute price a typed value in the given unit resolves to. Points
    /// measure the distance from the entry; the level keeps its current side
    /// (target defaults to the profit side, stop to the loss side, when it
    /// sits exactly on the entry).
    pub fn level_price_from_display(
        &self,
        level: PositionLevel,
        mode: PositionLevelMode,
        value: f64,
    ) -> f64 {
        let Some(p) = self.prices() else {
            return value;
        };
        if mode == PositionLevelMode::Price {
            return value;
        }
        let current = match level {
            PositionLevel::Stop => p.stop,
            PositionLevel::Target => p.target,
        };
        let mut side = js_sign(current - p.entry);
        if side == 0.0 {
            let toward_profit = if level == PositionLevel::Target {
                1.0
            } else {
                -1.0
            };
            let long = if self.direction() == PositionDirection::Long {
                1.0
            } else {
                -1.0
            };
            side = toward_profit * long;
        }
        p.entry + side * value.abs()
    }

    /// Pixel layout for the painter: the box x-range plus each level's y.
    pub fn layout(&self, proj: &dyn Projector) -> Option<PositionLayout> {
        let pts = self.handle_points(proj);
        if pts.len() < 3 {
            return None;
        }
        let xs = Span::of(pts.iter().map(|p| p.0))?;
        Some(PositionLayout {
            x1: xs.min,
            x2: xs.max,
            ey: pts[0].1,
            sy: pts[1].1,
            ty: pts[2].1,
        })
    }
}

impl PositionLayout {
    /// The vertical extent of the three levels.
    fn y_span(&self) -> Option<Span> {
        Span::of([self.ey, self.sy, self.ty])
    }
}

impl Drawing for PositionTool {
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

    /// Click-move-click: click 1 is the entry, click 2 the target (2 clicks
    /// finalize). `on_placed` then derives the stop and rewrites the anchors as
    /// `[entry, stop, target]`; the 3 slots give all three handles their free
    /// axes after placement.
    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema {
            min: 2,
            max: 2,
            slots: vec![
                AnchorSlot::new("entry", FreeAxis::Both),
                AnchorSlot::new("stop", FreeAxis::Both),
                AnchorSlot::new("target", FreeAxis::Both),
            ],
        }
    }

    /// Resolve the two placed anchors (entry + target) into the final
    /// entry/stop/target box, computing the stop/width in PIXEL space so the
    /// box has a consistent visual size at any scale. A target above the
    /// entry yields a long (stop below); below yields a short. A degenerate
    /// second click (about the entry) falls back to a default-sized
    /// long-facing box.
    fn on_placed(&mut self, proj: &dyn Projector) {
        let Some(e) = self.base.anchors.first().copied() else {
            return;
        };
        let ex = proj.x_of(e.time);
        let Some(ey) = proj.y_of(e.price, &self.base.pane_id) else {
            return;
        };
        let t = self.base.anchors.get(1).copied();
        let ty = t.and_then(|t| proj.y_of(t.price, &self.base.pane_id));
        let (target_px, end_px) = match (t, ty) {
            // A real drag: the target (profit) is where the user released.
            (Some(t), Some(ty)) if (ty - ey).abs() >= MIN_DRAG_PX => (
                ty,
                ex + js_max((proj.x_of(t.time) - ex).abs(), MIN_WIDTH_PX),
            ),
            _ => (ey - DEFAULT_RISK_PX * DEFAULT_RR, ex + DEFAULT_WIDTH_PX),
        };
        // Stop on the opposite side of the entry at 1/R:R of the reward distance.
        let stop_px = ey - (target_px - ey) / DEFAULT_RR;
        let pane = &self.base.pane_id;
        let end_time = proj.px_to_point(end_px, ey, pane).time;
        let stop_price = proj.px_to_point(ex, stop_px, pane).price;
        let target_price = proj.px_to_point(ex, target_px, pane).price;
        self.base.anchors = vec![
            DrawingPoint {
                time: e.time,
                price: e.price,
            },
            DrawingPoint {
                time: end_time,
                price: stop_price,
            },
            DrawingPoint {
                time: end_time,
                price: target_price,
            },
        ];
    }

    /// Keep the stop (loss) and target (reward) on OPPOSITE sides of the
    /// entry. When a handle drag would put them on the same side (dragging the
    /// stop of a long up past the entry, say), reflect the *other* side across
    /// the entry so the two never collapse onto one direction. R:R (each
    /// side's distance from the entry) is preserved.
    fn constrain_handle_drag(&mut self, index: usize) {
        if self.base.anchors.len() < 3 {
            return;
        }
        let entry = self.base.anchors[0].price;
        let stop_side = js_sign(self.base.anchors[1].price - entry);
        let target_side = js_sign(self.base.anchors[2].price - entry);
        if stop_side == 0.0 || target_side == 0.0 || stop_side != target_side {
            return; // already opposed (or on the line)
        }
        // Same side: flip whichever side was NOT the one being dragged across the entry.
        let flip = if index == 2 { 1 } else { 2 };
        let a = &mut self.base.anchors[flip];
        a.price = 2.0 * entry - a.price;
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(b) = self.layout(proj) else {
            return false;
        };
        let Some(ys) = b.y_span() else {
            return false;
        };
        point_in_box(px, py, b.x1, ys.min, b.x2, ys.max, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        let Some(r) = self.resolved() else {
            return Vec::new();
        };
        let mut pts = Vec::with_capacity(3);
        for a in &r {
            let Some(y) = proj.y_of(a.price, &self.base.pane_id) else {
                return Vec::new();
            };
            pts.push((proj.x_of(a.time), y));
        }
        pts
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let b = self.layout(proj)?;
        let ys = b.y_span()?;
        Some(Bounds {
            x: b.x1,
            y: ys.min,
            w: b.x2 - b.x1,
            h: ys.max - ys.min,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let r = self.resolved()?;
        Span::of(r.iter().map(|a| a.price))
    }

    fn schema(&self) -> SettingsSchema {
        let behavior = |path: &'static str, label: MessageLike, kind: FieldKind| {
            SettingsField::new(path, label, kind).group(FieldGroup::Behavior)
        };
        let mut fields = vec![
            behavior(
                "riskPercent",
                msg("drawing.position.riskPercent", "Risk %"),
                FieldKind::Number,
            )
            .range(0.0, 100.0, 0.1),
            behavior(
                "accountBalance",
                msg("drawing.position.accountBalance", "Account balance"),
                FieldKind::Number,
            )
            .range(0.0, 1e12, 1.0),
            behavior(
                "quantity",
                msg("drawing.position.quantity", "Position size"),
                FieldKind::Number,
            ),
            behavior(
                "direction",
                msg("drawing.position.direction", "Direction"),
                FieldKind::Select,
            )
            .options(direction_options()),
            behavior(
                "entryPrice",
                msg("drawing.position.entryPrice", "Entry price"),
                FieldKind::Number,
            ),
            behavior(
                "stopPrice",
                msg("drawing.position.stopPrice", "Stop price"),
                FieldKind::Number,
            ),
            behavior(
                "targetPrice",
                msg("drawing.position.targetPrice", "Target price"),
                FieldKind::Number,
            ),
            behavior(
                "showText",
                msg("drawing.position.showText", "Show text"),
                FieldKind::Boolean,
            ),
            behavior(
                "showHeader",
                msg("drawing.position.showHeader", "Show direction & ratio"),
                FieldKind::Boolean,
            ),
            behavior(
                "showLossSize",
                msg("drawing.position.showLossSize", "Show loss & size"),
                FieldKind::Boolean,
            ),
            behavior(
                "showTargetLabel",
                msg("drawing.position.showTargetLabel", "Show target label"),
                FieldKind::Boolean,
            ),
            behavior(
                "showStopLabel",
                msg("drawing.position.showStopLabel", "Show stop label"),
                FieldKind::Boolean,
            ),
            behavior(
                "showPrices",
                msg("drawing.position.showPrices", "Show level prices"),
                FieldKind::Boolean,
            ),
            SettingsField::new(
                "profitColor",
                msg("drawing.position.profitColor", "Profit zone"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
            SettingsField::new(
                "lossColor",
                msg("drawing.position.lossColor", "Loss zone"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
        ];
        fields.extend(
            line_fields()
                .into_iter()
                .filter(|f| f.path != "style.lineColor"),
        );
        fields.push(
            SettingsField::new(
                "text.color",
                msg("drawing.position.textColor", "Text color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Text),
        );
        fields.push(
            SettingsField::new(
                "text.size",
                msg("drawing.position.textSize", "Text size"),
                FieldKind::Select,
            )
            .options(text_size_options())
            .group(FieldGroup::Text),
        );
        SettingsSchema::new(fields)
    }

    /// `direction`, `quantity` and the three price levels are settable paths
    /// that are NOT mirrored in props (they edit the anchors / risk %).
    fn apply_setting(&mut self, path: &str, value: &Value) {
        match path {
            "direction" => match value.as_str() {
                Some("long") => self.set_direction(PositionDirection::Long),
                Some("short") => self.set_direction(PositionDirection::Short),
                _ => {}
            },
            "quantity" => {
                if let Some(v) = value.as_f64() {
                    self.set_quantity(v);
                }
            }
            "entryPrice" => {
                if let Some(v) = value.as_f64() {
                    self.set_entry_price(v);
                }
            }
            "stopPrice" => {
                if let Some(v) = value.as_f64() {
                    self.set_stop_price(v);
                }
            }
            "targetPrice" => {
                if let Some(v) = value.as_f64() {
                    self.set_target_price(v);
                }
            }
            _ => apply_setting_default(self, path, value),
        }
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("riskPercent".into(), json!(self.risk_percent));
        props.insert("accountBalance".into(), json!(self.account_balance));
        props.insert("showText".into(), json!(self.show_text));
        props.insert("showHeader".into(), json!(self.show_header));
        props.insert("showPrices".into(), json!(self.show_prices));
        props.insert("showLossSize".into(), json!(self.show_loss_size));
        props.insert("showTargetLabel".into(), json!(self.show_target_label));
        props.insert("showStopLabel".into(), json!(self.show_stop_label));
        props.insert("profitColor".into(), json!(self.profit_color));
        props.insert("lossColor".into(), json!(self.loss_color));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        let finite = |key: &str| {
            props
                .get(key)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
        };
        let flag = |key: &str| props.get(key).and_then(Value::as_bool);
        let text = |key: &str| props.get(key).and_then(Value::as_str).map(str::to_string);
        if let Some(v) = finite("riskPercent") {
            self.risk_percent = v;
        }
        if let Some(v) = finite("accountBalance") {
            self.account_balance = v;
        }
        if let Some(v) = flag("showText") {
            self.show_text = v;
        }
        if let Some(v) = flag("showHeader") {
            self.show_header = v;
        }
        if let Some(v) = flag("showPrices") {
            self.show_prices = v;
        }
        if let Some(v) = flag("showLossSize") {
            self.show_loss_size = v;
        }
        if let Some(v) = flag("showTargetLabel") {
            self.show_target_label = v;
        }
        if let Some(v) = flag("showStopLabel") {
            self.show_stop_label = v;
        }
        if let Some(v) = text("profitColor") {
            self.profit_color = v;
        }
        if let Some(v) = text("lossColor") {
            self.loss_color = v;
        }
    }
}

/// Compact money string (drops trailing .00; keeps two decimals otherwise).
fn format_money(n: f64) -> String {
    if !n.is_finite() {
        return "0".to_string();
    }
    let rounded = js_round(n * 100.0) / 100.0;
    if rounded == rounded.trunc() {
        js_number(rounded)
    } else {
        to_fixed(rounded, 2)
    }
}

/// Compact quantity string for the position size readout.
fn format_qty(n: f64) -> String {
    if !n.is_finite() || n == 0.0 {
        return "0".to_string();
    }
    if n >= 100.0 {
        return to_fixed(n, 2);
    }
    if n >= 1.0 {
        return strip_trailing_zeros(&to_fixed(n, 4)).to_string();
    }
    strip_trailing_zeros(&to_precision4(n)).to_string()
}

/// Compact price string for the level readouts (two decimals >= 1, four significant below).
fn format_price(n: f64) -> String {
    if !n.is_finite() {
        return "0".to_string();
    }
    if n.abs() >= 1.0 {
        return to_fixed(n, 2);
    }
    strip_trailing_zeros(&to_precision4(n)).to_string()
}

/// `s.replace(/\.?0+$/, '')`: the leftmost run of zeros reaching the end of
/// the string, together with the dot right before it.
fn strip_trailing_zeros(s: &str) -> &str {
    let bytes = s.as_bytes();
    let zeros = bytes.iter().rev().take_while(|b| **b == b'0').count();
    if zeros == 0 {
        return s;
    }
    let start = bytes.len() - zeros;
    if start > 0 && bytes[start - 1] == b'.' {
        &s[..start - 1]
    } else {
        &s[..start]
    }
}

/// `n.toPrecision(4)` for a finite number: fixed notation for exponents in
/// -6..4, exponential outside.
fn to_precision4(n: f64) -> String {
    if n == 0.0 {
        return "0.000".to_string();
    }
    let sci = format!("{:.3e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-6..4).contains(&exp) {
        return to_fixed(n, (3 - exp) as usize);
    }
    format!(
        "{}{mantissa}e{}{}",
        if n < 0.0 { "-" } else { "" },
        if exp < 0 { "-" } else { "+" },
        exp.abs()
    )
}

/// Trim binary float noise from a converted value (points arithmetic).
fn round_float(n: f64) -> f64 {
    js_round(n * 1e8) / 1e8
}

// ── Magnifier ──

/// One timeframe choice of the magnifier. `ms` is the option's bar duration
/// so pickers can drop the choices not strictly below the chart's timeframe
/// (`auto` carries 0: it always resolves lower, or the gateway reports that
/// nothing lower exists).
#[derive(Clone, Debug, PartialEq)]
pub struct MagnifierTimeframeOption {
    pub value: &'static str,
    pub label: MessageLike,
    pub ms: f64,
}

/// The timeframe choices the magnifier offers.
pub fn magnifier_timeframe_options() -> Vec<MagnifierTimeframeOption> {
    let option = |value, label, ms| MagnifierTimeframeOption { value, label, ms };
    vec![
        option("auto", msg("drawing.magnifier.option.auto", "Auto"), 0.0),
        option("1", msg("drawing.magnifier.option.n1", "1m"), 60_000.0),
        option("5", msg("drawing.magnifier.option.n5", "5m"), 300_000.0),
        option("15", msg("drawing.magnifier.option.n15", "15m"), 900_000.0),
        option(
            "30",
            msg("drawing.magnifier.option.n30", "30m"),
            1_800_000.0,
        ),
        option("60", msg("drawing.magnifier.option.n60", "1h"), 3_600_000.0),
        option(
            "240",
            msg("drawing.magnifier.option.n240", "4h"),
            14_400_000.0,
        ),
        option("D", msg("drawing.magnifier.option.d", "1D"), 86_400_000.0),
    ]
}

/// The choices strictly below a chart bar of `chart_ms` (all of them when it
/// is unknown); `auto` rides along while at least one concrete lower step exists.
pub fn magnifier_timeframes_below(chart_ms: f64) -> Vec<MagnifierTimeframeOption> {
    let all = magnifier_timeframe_options();
    // `!(chartMs > 0)`: also true for NaN.
    if chart_ms.is_nan() || chart_ms <= 0.0 {
        return all;
    }
    let mut it = all.into_iter();
    let Some(auto) = it.next() else {
        return Vec::new();
    };
    let lower: Vec<_> = it.filter(|o| o.ms > 0.0 && o.ms < chart_ms).collect();
    if lower.is_empty() {
        return Vec::new();
    }
    std::iter::once(auto).chain(lower).collect()
}

/// Display label for a magnifier timeframe value (`"60"` is `1h`, `"auto"` is `Auto`).
pub fn magnifier_timeframe_label(value: &str) -> MessageLike {
    if let Some(opt) = magnifier_timeframe_options()
        .into_iter()
        .find(|o| o.value == value)
    {
        return opt.label;
    }
    // `Number(value)`: surrounding whitespace is ignored.
    if let Ok(n) = value.trim().parse::<f64>()
        && n.is_finite()
        && n > 0.0
    {
        return if n % 1440.0 == 0.0 {
            format!("{}D", js_number(n / 1440.0)).into()
        } else if n % 60.0 == 0.0 {
            format!("{}h", js_number(n / 60.0)).into()
        } else {
            format!("{}m", js_number(n)).into()
        };
    }
    value.to_string().into()
}

/// Behavior + candle cosmetics of the magnifier (round-trips through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct MagnifierStyle {
    /// `"auto"` (resolve against the chart timeframe) or a canonical value (`"5"`, `"60"`, `"D"`).
    pub timeframe: String,
    /// Rising lower-timeframe candles; empty means the chart series' own up color, resolved at paint time.
    pub up_color: String,
    /// Falling lower-timeframe candles; empty means the chart series' own down color.
    pub down_color: String,
}

impl Default for MagnifierStyle {
    /// Empty candle colors follow the CHART series' own up/down at paint
    /// time, so the inset reads as a finer copy of the main series until the
    /// user explicitly recolors it.
    fn default() -> Self {
        Self {
            timeframe: "auto".to_string(),
            up_color: String::new(),
            down_color: String::new(),
        }
    }
}

/// A **magnifier**: a rectangle (two corner anchors, press-drag-release
/// placement) whose interior shows the chart's own market at a FINER
/// timeframe. Each chart candle subdivides in place into its lower-timeframe
/// candles, drawn at their true time/price positions on the pane's scales. The
/// bars come through `Projector::series_in_range` (cache-backed); while they
/// load, or when no lower timeframe applies, the painter shows a short notice
/// inside the area instead.
#[derive(Clone, Debug)]
pub struct Magnifier {
    base: DrawingBase,
    /// Behavior + candle cosmetics (seeded from defaults, persisted via props).
    pub magnifier: MagnifierStyle,
    /// Pixel rect of the timeframe chip as painted last frame, caret included:
    /// the chip is an interactive dropdown trigger, so the interaction layer
    /// needs the exact rect the painter measured. Renderer-transient: never
    /// serialized, `None` while unpainted.
    pub chip_rect: Option<Bounds>,
}

impl Magnifier {
    pub const TYPE: &'static str = "magnifier";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            magnifier: MagnifierStyle::default(),
            chip_rect: None,
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// The pixel rectangle between the two corner anchors (painter + hit-test
    /// share it) as `(x1, y1, x2, y2)`.
    pub fn rect(&self, proj: &dyn Projector) -> Option<(f64, f64, f64, f64)> {
        CornerRect::of(&self.base, proj).map(|r| (r.x1, r.y1, r.x2, r.y2))
    }
}

impl Drawing for Magnifier {
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
        corner_anchor_schema()
    }

    fn placement_mode(&self) -> PlacementMode {
        PlacementMode::Drag
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(r) = CornerRect::of(&self.base, proj) else {
            return false;
        };
        // The interior is an opaque inset: grabbable anywhere, like a filled box.
        if point_in_box(px, py, r.x1, r.y1, r.x2, r.y2, 0.0) {
            return true;
        }
        let edges = [
            [r.x1, r.y1, r.x2, r.y1],
            [r.x2, r.y1, r.x2, r.y2],
            [r.x2, r.y2, r.x1, r.y2],
            [r.x1, r.y2, r.x1, r.y1],
        ];
        edges
            .iter()
            .any(|e| dist_to_segment(px, py, e[0], e[1], e[2], e[3]) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        CornerRect::of(&self.base, proj)
            .map(|r| r.handles())
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        CornerRect::of(&self.base, proj).map(|r| r.bounds())
    }

    fn price_range(&self) -> Option<Span> {
        two_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let options = magnifier_timeframe_options()
            .into_iter()
            .map(|o| FieldOption::new(o.value, o.label))
            .collect();
        let mut fields = vec![
            SettingsField::new(
                "magnifier.timeframe",
                msg("drawing.magnifier.magnifierTimeframe", "Timeframe"),
                FieldKind::Select,
            )
            .options(options)
            .group(FieldGroup::Behavior),
        ];
        fields.extend(border_fields());
        fields.push(
            SettingsField::new(
                "magnifier.upColor",
                msg("drawing.magnifier.magnifierUpColor", "Up candles"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
        );
        fields.push(
            SettingsField::new(
                "magnifier.downColor",
                msg("drawing.magnifier.magnifierDownColor", "Down candles"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
        );
        SettingsSchema::new(fields)
    }

    /// The `magnifier.*` paths edit the style block, which props store flat
    /// (`timeframe`, `upColor`, `downColor`).
    fn apply_setting(&mut self, path: &str, value: &Value) {
        let slot = match path {
            "magnifier.timeframe" => &mut self.magnifier.timeframe,
            "magnifier.upColor" => &mut self.magnifier.up_color,
            "magnifier.downColor" => &mut self.magnifier.down_color,
            _ => return apply_setting_default(self, path, value),
        };
        if let Some(v) = value.as_str() {
            *slot = v.to_string();
        }
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("timeframe".into(), json!(self.magnifier.timeframe));
        props.insert("upColor".into(), json!(self.magnifier.up_color));
        props.insert("downColor".into(), json!(self.magnifier.down_color));
        Some(props)
    }

    /// The props overlay the defaults wholesale: a key the bag lacks resets to
    /// its default rather than keeping the current value.
    fn read_props(&mut self, props: &Props) {
        let text = |key: &str, default: String| {
            props
                .get(key)
                .and_then(Value::as_str)
                .map_or(default, str::to_string)
        };
        let defaults = MagnifierStyle::default();
        self.magnifier = MagnifierStyle {
            timeframe: text("timeframe", defaults.timeframe),
            up_color: text("upColor", defaults.up_color),
            down_color: text("downColor", defaults.down_color),
        };
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            DatePriceRange::TYPE,
            GROUP,
            msg("drawing.datepricerange.label", "Date & Price Range"),
            svg24(
                r#"<rect x="4" y="6" width="16" height="12" rx="1"/><path d="M4 18 20 6" opacity="0.5"/>"#,
            ),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid),
            |init| Box::new(DatePriceRange::new(init)),
        ),
        // Shaded target zone above the entry edge, hollow stop zone below it.
        DrawingTypeMeta::new(
            PositionTool::TYPE,
            GROUP,
            msg("drawing.position.label", "Long/Short Position"),
            svg24(concat!(
                r#"<rect x="4.5" y="4.5" width="15" height="7.5" rx="1" fill="currentColor" fill-opacity="0.25"/>"#,
                r#"<rect x="4.5" y="12" width="15" height="7.5" rx="1"/>"#,
            )),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid),
            |init| Box::new(PositionTool::new(init)),
        ),
        // An empty border color means the THEME's contrast ink (white on dark,
        // black on light), resolved at paint time so it follows theme switches.
        DrawingTypeMeta::new(
            Magnifier::TYPE,
            GROUP,
            msg("drawing.magnifier.label", "Magnifier"),
            svg24(concat!(
                r#"<circle cx="10.5" cy="10.5" r="6.5"/><path d="m15.3 15.3 5.2 5.2"/>"#,
                r#"<path d="M8 12.5v-3M10.5 13.5v-5.5M13 12v-2"/>"#,
            )),
            DrawingStyle::line("", 1.0, LineStyle::Solid),
            |init| Box::new(Magnifier::new(init)),
        )
        // The inset's backdrop must sit over the base candles it replaces.
        .covers_series()
        .placement_hint(msg(
            "drawing.magnifier.placementHint",
            "Drag an area on the chart to view it at a lower timeframe",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::projector::LinearProjector;

    const MIN: f64 = 60_000.0;

    fn p(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    fn meta(key: &str) -> DrawingTypeMeta {
        metas().into_iter().find(|m| m.type_key == key).unwrap()
    }

    fn make(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    /// The projector with no `bars_between`.
    struct NoBars(LinearProjector);

    impl Projector for NoBars {
        fn x_of(&self, t: f64) -> f64 {
            self.0.x_of(t)
        }
        fn y_of(&self, price: f64, pane: &str) -> Option<f64> {
            self.0.y_of(price, pane)
        }
        fn px_to_point(&self, x: f64, y: f64, pane: &str) -> DrawingPoint {
            self.0.px_to_point(x, y, pane)
        }
        fn pane_id_at_y(&self, y: f64) -> Option<String> {
            self.0.pane_id_at_y(y)
        }
        fn width(&self) -> f64 {
            self.0.width()
        }
        fn height(&self) -> f64 {
            self.0.height()
        }
    }

    fn position(entry: f64, stop: f64, target: f64) -> PositionTool {
        let init = DrawingInit::on_pane("price").anchors(vec![
            p(0.0, entry),
            p(5.0 * MIN, stop),
            p(10.0 * MIN, target),
        ]);
        PositionTool::new(&init)
    }

    fn range(a: DrawingPoint, b: DrawingPoint) -> DatePriceRange {
        DatePriceRange::new(&DrawingInit::on_pane("price").anchors(vec![a, b]))
    }

    fn all_keys() -> [&'static str; 3] {
        [DatePriceRange::TYPE, PositionTool::TYPE, Magnifier::TYPE]
    }

    // ── every type ──

    #[test]
    fn metas_carry_the_registry_entries() {
        let metas = metas();
        assert_eq!(metas.len(), 3);
        for m in &metas {
            assert_eq!(m.group, "measure");
            let d = (m.create)(&DrawingInit::on_pane("price"));
            assert_eq!(d.type_key(), m.type_key);
            assert!(m.icon.starts_with("<svg"));
        }
        let mag = meta("magnifier");
        assert!(mag.covers_series);
        assert!(mag.placement_hint.is_some());
        assert_eq!(mag.default_style.line_color, "");
        assert!(!meta("position").covers_series);
        assert!(meta("position").placement_hint.is_none());
        assert_eq!(meta("datepricerange").default_style.line_width, 1.0);
        assert_eq!(meta("position").label.source_text(), "Long/Short Position");
    }

    #[test]
    fn anchor_schemas_and_placement() {
        let range = make("datepricerange", vec![]);
        let s = range.anchor_schema();
        assert_eq!((s.min, s.max), (2, 2));
        assert_eq!(s.slots[0].role, "c1");
        assert_eq!(s.slots[1].role, "c2");
        assert_eq!(range.placement_mode(), PlacementMode::Click);

        let pos = make("position", vec![]);
        let s = pos.anchor_schema();
        assert_eq!((s.min, s.max), (2, 2));
        let roles: Vec<_> = s.slots.iter().map(|s| s.role.as_str()).collect();
        assert_eq!(roles, ["entry", "stop", "target"]);
        assert_eq!(pos.placement_mode(), PlacementMode::Click);

        let mag = make("magnifier", vec![]);
        assert_eq!(mag.placement_mode(), PlacementMode::Drag);
        assert_eq!(mag.anchor_schema().min, 2);
    }

    #[test]
    fn survive_missing_anchors() {
        let proj = LinearProjector::sample();
        for key in all_keys() {
            for anchors in [vec![], vec![p(0.0, 100.0)]] {
                let mut d = make(key, anchors);
                assert!(!d.hit_test(5.0, 5.0, &proj, 4.0), "{key}");
                assert!(d.handle_points(&proj).is_empty(), "{key}");
                assert_eq!(d.hit_handle(5.0, 5.0, &proj, 4.0), None);
                assert_eq!(d.bounds(&proj), None);
                assert_eq!(d.price_range(), None);
                let _ = d.time_extent();
                assert!(!d.schema().fields.is_empty());
                assert!(d.write_props().is_some());
                let _ = d.serialize();
                d.drag_handle(1, p(1.0, 1.0));
                d.constrain_handle_drag(2);
                d.on_placed(&proj);
            }
        }
        let r = DatePriceRange::new(&DrawingInit::on_pane("price"));
        assert!(!r.is_up());
        assert_eq!(r.price_label(), "");
        assert_eq!(r.time_label(&proj), "");
        assert!(r.measure_stats(&proj).is_none());
        let pos = PositionTool::new(&DrawingInit::on_pane("price"));
        assert_eq!(pos.rr(), 0.0);
        assert_eq!(pos.position_size(), 0.0);
        assert_eq!(pos.direction_label(), "SHORT");
        assert_eq!(pos.layout(&proj), None);
        assert_eq!(pos.entry_price(), 0.0);
    }

    #[test]
    fn serialize_round_trips() {
        for key in all_keys() {
            let m = meta(key);
            let d = make(
                key,
                vec![p(0.0, 50.0), p(5.0 * MIN, 40.0), p(10.0 * MIN, 70.0)],
            );
            let doc = d.serialize();
            assert_eq!(doc.type_key, key);
            let again = (m.create)(&DrawingInit::from(doc.clone()));
            assert_eq!(again.serialize(), doc, "{key}");
        }
    }

    // ── Date & Price Range ──

    #[test]
    fn range_labels_and_geometry() {
        // price 100 -> 90 over 3 hours; sample px: x = 0..180, y = 400..440
        let proj = LinearProjector::sample();
        let d = range(p(0.0, 100.0), p(180.0 * MIN, 90.0));
        let lines = d.measure_label(&proj);
        assert_eq!(lines, ["-10.00 (-10.00%)", "180 bars, 3h"]);
        assert!(!d.is_up());
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 90.0,
                max: 100.0
            })
        );
        assert_eq!(d.handle_points(&proj), [(0.0, 400.0), (180.0, 440.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 0.0,
                y: 400.0,
                w: 180.0,
                h: 40.0
            })
        );
        assert!(d.hit_test(90.0, 420.0, &proj, 4.0));
        assert!(d.hit_test(183.0, 420.0, &proj, 4.0));
        assert!(!d.hit_test(90.0, 500.0, &proj, 4.0));
        assert_eq!(d.hit_handle(2.0, 402.0, &proj, 4.0), Some(0));
        assert_eq!(d.hit_handle(181.0, 441.0, &proj, 4.0), Some(1));
        assert_eq!(d.hit_handle(90.0, 420.0, &proj, 4.0), None);
        let stats = d.measure_stats(&proj).unwrap();
        assert_eq!(stats.delta, -10.0);
        assert_eq!(stats.percent, -10.0);
        assert_eq!(stats.duration_ms, 180.0 * MIN);
        assert_eq!(stats.bars, Some(180.0));
    }

    #[test]
    fn range_toggles_drop_either_line() {
        let proj = LinearProjector::sample();
        let mut d = range(p(0.0, 50.0), p(300.0 * MIN, 60.0));
        assert_eq!(d.measure_label(&proj), ["+10.00 (+20.00%)", "300 bars, 5h"]);
        d.show_date = false;
        assert_eq!(d.measure_label(&proj), ["+10.00 (+20.00%)"]);
        d.show_date = true;
        d.show_price = false;
        assert_eq!(d.measure_label(&proj), ["300 bars, 5h"]);
        d.show_date = false;
        assert!(d.measure_label(&proj).is_empty());
        assert!(d.is_up());
    }

    #[test]
    fn range_time_label_variants() {
        let proj = LinearProjector::sample();
        // a single bar takes the singular form
        let d = range(p(0.0, 1.0), p(MIN, 2.0));
        assert_eq!(d.time_label(&proj), "1 bar, 1m");
        // a projector that cannot count bars: the duration alone
        let none = NoBars(LinearProjector::sample());
        assert_eq!(d.time_label(&none), "1m");
        assert_eq!(d.measure_stats(&none).unwrap().bars, None);
        // zero price: percent 0, not NaN
        let z = range(p(0.0, 0.0), p(MIN, 5.0));
        assert_eq!(z.price_label(), "+5.00 (+0.00%)");
    }

    #[test]
    fn format_duration_matches_the_original() {
        const HR: f64 = 3_600_000.0;
        assert_eq!(format_duration(2.0 * 86_400_000.0 + 4.0 * HR), "2d 4h");
        assert_eq!(format_duration(2.0 * 86_400_000.0), "2d");
        assert_eq!(format_duration(3.0 * HR + 15.0 * MIN), "3h 15m");
        assert_eq!(format_duration(3.0 * HR), "3h");
        assert_eq!(format_duration(45.0 * MIN), "45m");
        assert_eq!(format_duration(-45.0 * MIN), "45m");
        assert_eq!(format_duration(30_000.0), "30s");
        assert_eq!(format_duration(500.0), "1s"); // Math.round(0.5) rounds up
        assert_eq!(format_duration(0.0), "0s");
        // 23.5h of the last day rounds to 24h, as the original does
        assert_eq!(format_duration(86_400_000.0 + 23.5 * HR), "1d 24h");
    }

    #[test]
    fn range_props_leniency_and_round_trip() {
        let mut props = Props::new();
        props.insert("showPrice".into(), json!("no"));
        props.insert("showDate".into(), json!(false));
        let d = DatePriceRange::new(&DrawingInit::on_pane("price").props(props));
        assert!(d.show_price);
        assert!(!d.show_date);
        assert_eq!(
            d.write_props().unwrap(),
            json!({ "showPrice": true, "showDate": false })
                .as_object()
                .unwrap()
                .clone()
        );
        let mut d = d;
        d.apply_settings(
            json!({ "showPrice": false, "text.color": "#ff0000", "text.size": "large" })
                .as_object()
                .unwrap(),
        );
        assert!(!d.show_price);
        let doc = d.serialize();
        let round = DatePriceRange::new(&DrawingInit::from(doc.clone()));
        assert!(!round.show_price);
        assert_eq!(
            round.base().text.as_ref().unwrap().color.as_deref(),
            Some("#ff0000")
        );
        assert_eq!(round.serialize(), doc);
    }

    #[test]
    fn range_schema_drops_the_line_color() {
        let d = DatePriceRange::new(&DrawingInit::on_pane("price"));
        let paths: Vec<_> = d.schema().fields.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [
                "showPrice",
                "showDate",
                "style.lineWidth",
                "style.lineStyle",
                "text.color",
                "text.size"
            ]
        );
    }

    // ── Position ──

    #[test]
    fn position_geometry() {
        // entry 50 / stop 40 / target 70 at x = 0 / 5 / 10: y = 600 / 640 / 520
        let proj = LinearProjector::sample();
        let d = position(50.0, 40.0, 70.0);
        assert_eq!(
            d.handle_points(&proj),
            [(0.0, 600.0), (5.0, 640.0), (10.0, 520.0)]
        );
        assert_eq!(
            d.layout(&proj),
            Some(PositionLayout {
                x1: 0.0,
                x2: 10.0,
                ey: 600.0,
                sy: 640.0,
                ty: 520.0
            })
        );
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 0.0,
                y: 520.0,
                w: 10.0,
                h: 120.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 40.0,
                max: 70.0
            })
        );
        assert!(d.hit_test(5.0, 580.0, &proj, 4.0));
        assert!(d.hit_test(13.0, 580.0, &proj, 4.0));
        assert!(!d.hit_test(5.0, 700.0, &proj, 4.0));
        assert_eq!(d.hit_handle(10.0, 522.0, &proj, 4.0), Some(2));
        assert_eq!(d.hit_handle(5.0, 639.0, &proj, 4.0), Some(1));
        assert_eq!(d.hit_handle(5.0, 580.0, &proj, 4.0), None);
        assert_eq!(
            d.time_extent(),
            Some(Span {
                min: 0.0,
                max: 10.0 * MIN
            })
        );
    }

    #[test]
    fn position_stats() {
        let d = position(50.0, 40.0, 70.0);
        assert_eq!(d.direction_label(), "LONG");
        assert!((d.rr() - 2.0).abs() < 1e-9);
        assert!((d.reward_pct() - 40.0).abs() < 1e-9);
        assert!((d.risk_pct() - 20.0).abs() < 1e-9);
        assert_eq!(d.dollar_loss(), 100.0);
        assert!((d.position_size() - 10.0).abs() < 1e-9);
        let short = position(50.0, 60.0, 30.0);
        assert_eq!(short.direction_label(), "SHORT");
        assert!((short.rr() - 2.0).abs() < 1e-9);
        // degenerate risk
        let flat = position(50.0, 50.0, 70.0);
        assert_eq!(flat.rr(), 0.0);
        assert_eq!(flat.position_size(), 0.0);
        // zero entry price
        let zero = position(0.0, -1.0, 2.0);
        assert_eq!(zero.reward_pct(), 0.0);
        assert_eq!(zero.risk_pct(), 0.0);
    }

    #[test]
    fn position_account_settings_drive_loss_and_size() {
        let mut d = position(50.0, 40.0, 70.0);
        assert_eq!(d.risk_percent, 1.0);
        assert_eq!(d.account_balance, 10_000.0);
        d.apply_settings(
            json!({ "riskPercent": 2, "accountBalance": 5000 })
                .as_object()
                .unwrap(),
        );
        assert_eq!(d.dollar_loss(), 100.0);
        assert!((d.position_size() - 10.0).abs() < 1e-9);
        assert_eq!(d.loss_size_label(), "Loss 100  ·  Size 10");
        d.risk_percent = -5.0;
        assert_eq!(d.dollar_loss(), 0.0);
    }

    #[test]
    fn position_preview_derives_the_stop() {
        // only entry + target: the stop sits opposite at half the reward
        let proj = LinearProjector::sample();
        let d = DrawingInit::on_pane("price").anchors(vec![p(0.0, 50.0), p(5.0 * MIN, 70.0)]);
        let d = PositionTool::new(&d);
        assert_eq!(d.direction_label(), "LONG");
        assert_eq!(
            d.prices(),
            Some(PositionPrices {
                entry: 50.0,
                stop: 40.0,
                target: 70.0
            })
        );
        assert!((d.rr() - 2.0).abs() < 1e-9);
        let l = d.layout(&proj).unwrap();
        assert!(l.ty < l.ey);
        assert!(l.sy > l.ey);
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 40.0,
                max: 70.0
            })
        );
    }

    #[test]
    fn position_on_placed_finalizes_against_the_projector() {
        let proj = LinearProjector::sample();
        // a real drag: entry (x 0, y 400), target released at (x 10, y 360)
        let init = DrawingInit::on_pane("price").anchors(vec![p(0.0, 100.0), p(10.0 * MIN, 110.0)]);
        let mut d = PositionTool::new(&init);
        d.on_placed(&proj);
        let a = &d.base().anchors;
        assert_eq!(a.len(), 3);
        assert_eq!(a[0], p(0.0, 100.0));
        // width max(10, 48) = 48 px; stop at y 420 (price 95); target y 360 (price 110)
        assert_eq!(a[1], p(48.0 * MIN, 95.0));
        assert_eq!(a[2], p(48.0 * MIN, 110.0));
        assert_eq!(d.direction_label(), "LONG");

        // a wide drag keeps its own width; a drag downward is a short
        let init = DrawingInit::on_pane("price").anchors(vec![p(0.0, 100.0), p(100.0 * MIN, 90.0)]);
        let mut d = PositionTool::new(&init);
        d.on_placed(&proj);
        let a = &d.base().anchors;
        assert_eq!(a[1], p(100.0 * MIN, 105.0)); // stop above the entry
        assert_eq!(a[2], p(100.0 * MIN, 90.0));
        assert_eq!(d.direction_label(), "SHORT");

        // a bare click: default 150 px wide box, target 128 px above the entry
        let init = DrawingInit::on_pane("price").anchors(vec![p(0.0, 100.0), p(MIN, 100.5)]);
        let mut d = PositionTool::new(&init);
        d.on_placed(&proj);
        let a = &d.base().anchors;
        assert_eq!(a[1], p(150.0 * MIN, 84.0)); // stop at y 464
        assert_eq!(a[2], p(150.0 * MIN, 132.0)); // target at y 272
    }

    #[test]
    fn position_on_placed_without_a_second_anchor_uses_the_default_box() {
        let proj = LinearProjector::sample();
        let init = DrawingInit::on_pane("price").anchors(vec![p(0.0, 100.0)]);
        let mut d = PositionTool::new(&init);
        d.on_placed(&proj);
        assert_eq!(d.base().anchors.len(), 3);
        assert_eq!(d.direction_label(), "LONG");
    }

    #[test]
    fn position_constrains_stop_and_target_to_opposite_sides() {
        // stop dragged above the entry: the target mirrors below
        let mut d = position(50.0, 40.0, 70.0);
        d.base.anchors[1].price = 60.0;
        d.constrain_handle_drag(1);
        assert_eq!(d.base().anchors[2].price, 30.0);
        assert_eq!(d.direction_label(), "SHORT");
        // target dragged below the entry: the stop mirrors above
        let mut d = position(50.0, 40.0, 70.0);
        d.base.anchors[2].price = 45.0;
        d.constrain_handle_drag(2);
        assert_eq!(d.base().anchors[1].price, 60.0);
        // already opposed: untouched
        let mut d = position(50.0, 40.0, 70.0);
        d.base.anchors[1].price = 45.0;
        d.constrain_handle_drag(1);
        assert_eq!(d.base().anchors[2].price, 70.0);
        // a level exactly on the entry is "on the line": untouched
        let mut d = position(50.0, 50.0, 70.0);
        d.constrain_handle_drag(1);
        assert_eq!(d.base().anchors[2].price, 70.0);
    }

    #[test]
    fn position_price_level_settings_edit_the_anchors() {
        let mut d = position(50.0, 40.0, 70.0);
        d.apply_setting("entryPrice", &json!(52));
        assert_eq!(d.base().anchors[0].price, 52.0);
        d.apply_setting("targetPrice", &json!(82));
        assert_eq!(d.base().anchors[2].price, 82.0);
        assert!((d.rr() - 30.0 / 12.0).abs() < 1e-9);
        // a stop past the entry flips the target, as a drag would
        d.apply_setting("stopPrice", &json!(60));
        assert_eq!(d.base().anchors[1].price, 60.0);
        assert!(d.base().anchors[2].price < 52.0);
        assert_eq!(d.direction_label(), "SHORT");
        // wrong types are ignored
        d.apply_setting("entryPrice", &json!("x"));
        assert_eq!(d.entry_price(), 52.0);
        assert_eq!(d.stop_price(), 60.0);
        assert_eq!(d.target_price(), d.base().anchors[2].price);
    }

    #[test]
    fn position_direction_setting_mirrors_the_levels() {
        let mut d = position(50.0, 40.0, 70.0);
        assert_eq!(d.direction(), PositionDirection::Long);
        d.apply_setting("direction", &json!("short"));
        assert_eq!(d.direction(), PositionDirection::Short);
        assert_eq!(d.base().anchors[1].price, 60.0);
        assert_eq!(d.base().anchors[2].price, 30.0);
        assert!((d.rr() - 2.0).abs() < 1e-9);
        d.apply_setting("direction", &json!("short")); // same value: no-op
        assert_eq!(d.base().anchors[1].price, 60.0);
        d.apply_setting("direction", &json!("sideways")); // unknown: ignored
        assert_eq!(d.base().anchors[1].price, 60.0);
        d.apply_setting("direction", &json!("long"));
        assert_eq!(d.base().anchors[1].price, 40.0);
        assert_eq!(d.base().anchors[2].price, 70.0);
    }

    #[test]
    fn position_quantity_back_solves_the_risk_percent() {
        let mut d = position(50.0, 40.0, 70.0);
        assert!((d.quantity() - 10.0).abs() < 1e-9);
        d.apply_setting("quantity", &json!(20));
        assert!((d.risk_percent - 2.0).abs() < 1e-9);
        assert!((d.dollar_loss() - 200.0).abs() < 1e-9);
        assert!((d.position_size() - 20.0).abs() < 1e-9);
        d.apply_setting("quantity", &json!(-5)); // invalid: ignored
        assert!((d.risk_percent - 2.0).abs() < 1e-9);
        d.account_balance = 0.0;
        d.apply_setting("quantity", &json!(7));
        assert!((d.risk_percent - 2.0).abs() < 1e-9);
    }

    #[test]
    fn position_labels() {
        let mut d = position(50.0, 40.0, 70.0);
        assert_eq!(d.header_label(), "LONG  ·  R:R 2.00");
        assert_eq!(d.target_label(), "Target +40.00%");
        assert_eq!(d.stop_label(), "Stop \u{2212}20.00%");
        d.apply_setting("showPrices", &json!(true));
        assert_eq!(d.target_label(), "Target +40.00%  @ 70.00");
        assert_eq!(d.stop_label(), "Stop \u{2212}20.00%  @ 40.00");
        d.apply_setting("showText", &json!(false));
        d.apply_setting("showLossSize", &json!(false));
        assert!(!d.show_text);
        assert!(!d.show_loss_size);
        assert_eq!(
            position(50.0, 60.0, 30.0).header_label(),
            "SHORT  ·  R:R 2.00"
        );
    }

    #[test]
    fn position_number_formatting() {
        assert_eq!(format_money(100.0), "100");
        assert_eq!(format_money(100.456), "100.46");
        assert_eq!(format_money(f64::NAN), "0");
        assert_eq!(format_qty(0.0), "0");
        assert_eq!(format_qty(250.0), "250.00");
        assert_eq!(format_qty(10.0), "10");
        assert_eq!(format_qty(5.25), "5.25");
        assert_eq!(format_qty(0.5), "0.5");
        assert_eq!(format_qty(0.012345), "0.01235");
        assert_eq!(format_qty(0.00001234), "0.00001234");
        assert_eq!(format_price(70.0), "70.00");
        assert_eq!(format_price(-3.456), "-3.46");
        assert_eq!(format_price(0.5), "0.5");
        assert_eq!(format_price(0.0), "0");
        assert_eq!(format_price(0.000000123), "1.230e-7");
        assert_eq!(format_price(f64::INFINITY), "0");
        assert_eq!(strip_trailing_zeros("10.0000"), "10");
        assert_eq!(strip_trailing_zeros("10.5000"), "10.5");
        assert_eq!(strip_trailing_zeros("100"), "1");
        assert_eq!(strip_trailing_zeros("0.000"), "0");
    }

    #[test]
    fn position_level_units() {
        let d = position(50.0, 40.0, 70.0);
        use PositionLevel::{Stop, Target};
        use PositionLevelMode::{Points, Price};
        assert_eq!(d.level_display_value(Stop, Price), 40.0);
        assert_eq!(d.level_display_value(Stop, Points), 10.0);
        assert_eq!(d.level_display_value(Target, Points), 20.0);
        assert_eq!(d.level_price_from_display(Stop, Price, 45.0), 45.0);
        assert_eq!(d.level_price_from_display(Stop, Points, 5.0), 45.0);
        assert_eq!(d.level_price_from_display(Target, Points, 5.0), 55.0);
        // a level sitting on the entry takes its default side
        let flat_stop = position(50.0, 50.0, 70.0);
        assert_eq!(flat_stop.level_price_from_display(Stop, Points, 5.0), 45.0);
        let flat_target = position(50.0, 40.0, 50.0);
        // direction: target (50) >= entry (50) is long, so the target side is +1
        assert_eq!(
            flat_target.level_price_from_display(Target, Points, 5.0),
            55.0
        );
        // float noise from points arithmetic is trimmed
        let noisy = position(0.1, 0.2, 0.0);
        assert_eq!(noisy.level_display_value(Stop, Points), 0.1);
    }

    #[test]
    fn position_props_leniency_and_round_trip() {
        let mut props = Props::new();
        props.insert("riskPercent".into(), json!("lots"));
        props.insert("accountBalance".into(), json!(25_000));
        props.insert("showText".into(), json!(1));
        props.insert("showPrices".into(), json!(true));
        props.insert("profitColor".into(), json!(5));
        props.insert("lossColor".into(), json!("#ff0000"));
        let d = PositionTool::new(&DrawingInit::on_pane("price").props(props));
        assert_eq!(d.risk_percent, 1.0);
        assert_eq!(d.account_balance, 25_000.0);
        assert!(d.show_text);
        assert!(d.show_prices);
        assert_eq!(d.profit_color, "#0ecb81");
        assert_eq!(d.loss_color, "#ff0000");

        let mut d = position(50.0, 40.0, 70.0);
        d.apply_settings(
            json!({
                "riskPercent": 1.5,
                "accountBalance": 25_000,
                "showHeader": false,
                "profitColor": "#00ff00",
                "text.color": "#ffcc00",
                "text.size": "large",
            })
            .as_object()
            .unwrap(),
        );
        let doc = d.serialize();
        let round = PositionTool::new(&DrawingInit::from(doc.clone()));
        assert_eq!(round.risk_percent, 1.5);
        assert!(!round.show_header);
        assert_eq!(round.profit_color, "#00ff00");
        assert_eq!(
            round.base().text.as_ref().unwrap().color.as_deref(),
            Some("#ffcc00")
        );
        assert_eq!(round.serialize(), doc);
        assert_eq!(doc.type_key, "position");
    }

    #[test]
    fn position_schema_paths() {
        let d = position(50.0, 40.0, 70.0);
        let schema = d.schema();
        let paths: Vec<_> = schema.fields.iter().map(|f| f.path.as_str()).collect();
        for want in [
            "riskPercent",
            "accountBalance",
            "quantity",
            "direction",
            "entryPrice",
            "stopPrice",
            "targetPrice",
            "showText",
            "showHeader",
            "showPrices",
            "showLossSize",
            "showTargetLabel",
            "showStopLabel",
            "profitColor",
            "lossColor",
            "text.color",
            "text.size",
        ] {
            assert!(paths.contains(&want), "{want}");
        }
        assert!(!paths.contains(&"text.bold"));
        assert!(!paths.contains(&"style.lineColor"));
        let risk = &schema.fields[0];
        assert_eq!(
            (risk.min, risk.max, risk.step),
            (Some(0.0), Some(100.0), Some(0.1))
        );
        assert_eq!(schema.fields[3].options.as_ref().unwrap().len(), 2);
    }

    // ── Magnifier ──

    fn magnifier() -> Magnifier {
        // px (0, 400) to (10, 440)
        Magnifier::new(
            &DrawingInit::on_pane("price").anchors(vec![p(0.0, 100.0), p(10.0 * MIN, 90.0)]),
        )
    }

    #[test]
    fn magnifier_defaults_and_geometry() {
        let proj = LinearProjector::sample();
        let d = magnifier();
        assert_eq!(d.magnifier.timeframe, "auto");
        assert_eq!(d.magnifier.up_color, "");
        assert_eq!(d.magnifier.down_color, "");
        assert!(d.chip_rect.is_none());
        assert_eq!(d.rect(&proj), Some((0.0, 400.0, 10.0, 440.0)));
        assert!(d.hit_test(5.0, 420.0, &proj, 3.0)); // interior
        assert!(d.hit_test(0.0, 400.0, &proj, 3.0)); // corner
        assert!(d.hit_test(12.0, 420.0, &proj, 3.0)); // within tol of the right edge
        assert!(!d.hit_test(13.5, 420.0, &proj, 3.0));
        assert!(!d.hit_test(50.0, 420.0, &proj, 3.0));
        assert_eq!(d.handle_points(&proj), [(0.0, 400.0), (10.0, 440.0)]);
        assert_eq!(d.hit_handle(1.0, 401.0, &proj, 3.0), Some(0));
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 0.0,
                y: 400.0,
                w: 10.0,
                h: 40.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 90.0,
                max: 100.0
            })
        );
    }

    #[test]
    fn magnifier_props_round_trip_and_reset() {
        let mut d = magnifier();
        d.magnifier.timeframe = "15".into();
        d.magnifier.up_color = "#112233".into();
        let doc = d.serialize();
        let props = doc.props.clone().unwrap();
        assert_eq!(props["timeframe"], "15");
        assert_eq!(props["upColor"], "#112233");
        assert_eq!(props["downColor"], "");
        let restored = Magnifier::new(&DrawingInit::from(doc.clone()));
        assert_eq!(restored.magnifier.timeframe, "15");
        assert_eq!(restored.magnifier.up_color, "#112233");
        assert_eq!(restored.magnifier.down_color, "");
        assert_eq!(restored.serialize(), doc);

        // a bag lacking a key resets it to the default; wrong types are ignored
        let mut d = magnifier();
        d.magnifier.up_color = "#fff".into();
        d.read_props(
            json!({ "timeframe": 5, "downColor": "#000" })
                .as_object()
                .unwrap(),
        );
        assert_eq!(d.magnifier.timeframe, "auto");
        assert_eq!(d.magnifier.up_color, "");
        assert_eq!(d.magnifier.down_color, "#000");
    }

    #[test]
    fn magnifier_settings_paths_reach_the_style_block() {
        let mut d = magnifier();
        d.apply_setting("magnifier.timeframe", &json!("30"));
        d.apply_setting("magnifier.upColor", &json!("#0f0"));
        d.apply_setting("magnifier.downColor", &json!(3)); // ignored
        d.apply_setting("style.lineColor", &json!("#abcdef"));
        assert_eq!(d.magnifier.timeframe, "30");
        assert_eq!(d.magnifier.up_color, "#0f0");
        assert_eq!(d.magnifier.down_color, "");
        assert_eq!(d.base().style.line_color, "#abcdef");
        assert_eq!(d.write_props().unwrap()["timeframe"], "30");
    }

    #[test]
    fn magnifier_schema() {
        let schema = magnifier().schema();
        let paths: Vec<_> = schema.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "magnifier.timeframe",
                "style.lineColor",
                "style.lineWidth",
                "style.lineStyle",
                "magnifier.upColor",
                "magnifier.downColor"
            ]
        );
        let tf = &schema.fields[0];
        assert_eq!(tf.kind, FieldKind::Select);
        assert_eq!(tf.options.as_ref().unwrap().len(), 8);
        assert_eq!(schema.fields[1].label.source_text(), "Border color");
    }

    #[test]
    fn magnifier_timeframe_helpers() {
        let label = |v: &str| magnifier_timeframe_label(v).source_text();
        assert_eq!(label("auto"), "Auto");
        assert_eq!(label("1"), "1m");
        assert_eq!(label("60"), "1h");
        assert_eq!(label("240"), "4h");
        assert_eq!(label("D"), "1D");
        assert_eq!(label("120"), "2h"); // outside the option list still formats
        assert_eq!(label("2880"), "2D");
        assert_eq!(label("45"), "45m");
        assert_eq!(label("nonsense"), "nonsense");
        assert_eq!(label("0"), "0");

        let values = |chart_ms: f64| -> Vec<&'static str> {
            magnifier_timeframes_below(chart_ms)
                .iter()
                .map(|o| o.value)
                .collect()
        };
        assert_eq!(values(0.0).len(), 8);
        assert_eq!(values(f64::NAN).len(), 8);
        assert_eq!(values(900_000.0), ["auto", "1", "5"]);
        assert_eq!(values(3_600_000.0), ["auto", "1", "5", "15", "30"]);
        assert!(values(60_000.0).is_empty());
    }
}
