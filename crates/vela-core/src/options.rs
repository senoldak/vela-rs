//! The chart's options and the small types they are made of (`options.ts`).
//!
//! # Differences from the original
//!
//! - `VelaOptions extends MarketConfig, LocalizationOptions` becomes two nested
//!   members, [`VelaOptions::market`] and [`VelaOptions::localization`], rather
//!   than a flat bag of fields.
//! - `renderer?: RendererConstructor` (a class Vela instantiates with the
//!   resolved display options) is a factory closure, [`RendererConstructor`].
//!   The orchestrator itself takes a renderer INSTANCE (`Rc<dyn ChartRenderer>`);
//!   this is only the option a host facade turns into one.
//! - `theme` is a [`ThemeSpec`]: a theme id, or a theme object (the chart's
//!   seven colors, or a `createTheme` result) as opaque JSON.
//! - A TypeScript union such as `boolean | number` is a small enum
//!   ([`AnimationValue`], [`IntroSetting`], [`AnimationsOption`],
//!   [`FreePanTrigger`], [`HeightOption`]).
//! - [`resolve_ease_ms`] and [`resolve_intro`] take a `serde_json::Value`, the
//!   original's `unknown`: a renderer's `apply_feature` hands them whatever the
//!   host passed.
//! - `MarketConfig`, `MarketSession`, `InitialRange`, `MoveTarget`, `VelaTheme`,
//!   `ThemeId` and `PriceStyle` live in the port modules and are re-exported.

use std::rc::Rc;

use serde_json::{Value, json};

pub use crate::drawings::toolbar::DrawingsOption;
use crate::i18n::types::LocalizationOptions;
use crate::model::Ohlcv;
use crate::model::{Inputs, PaneKind};
use crate::ports::chart_renderer::ChartRenderer;
pub use crate::ports::chart_renderer::{
    MoveTarget, NewPanePlacement, PriceStyle, SettingsVisibility as SettingsVisibilityPolicy,
    ThemeId, VelaTheme,
};
pub use crate::ports::market_data::{InitialRange, MarketConfig, MarketSession};

/// A registered data-provider name (any string; matched case-insensitively).
pub type ProviderName = String;

/// Narrow an untrusted value (persisted document, URL parameter) to a
/// [`MarketSession`]: only the exact strings `"regular"` and `"extended"` pass.
pub fn normalize_session(value: &Value) -> Option<MarketSession> {
    value.as_str().and_then(MarketSession::normalize)
}

/// One in-place market switch: the argument of `chart.setMarket(next)`. Only the
/// fields given change; the rest of the market keeps its current value. `data`
/// switches to offline bars (and giving `symbol` WITHOUT `data` drops a previous
/// offline dataset: back to the provider path). `visible_range` frames the FIRST
/// paint of the new market (a range chip switching timeframe, depth and window in
/// one call).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarketSwitch {
    /// Bare ticker (provider resolved by declaration order) or `EXCHANGE:`-prefixed.
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub bars: Option<usize>,
    /// Switch the shown trading session (reloads like a timeframe change).
    pub session: Option<MarketSession>,
    pub data: Option<Vec<Ohlcv>>,
    pub visible_range: Option<InitialRange>,
}

/// The chart's current market identity (`chart.market`), the read counterpart of
/// `setMarket`. A SNAPSHOT of the requested market (mutating it changes nothing):
/// it reflects a switch as soon as `setMarket` is called, not when the load lands.
/// `offline` is true when the chart runs on an inline `data` array instead of a
/// provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketSnapshot {
    pub symbol: Option<String>,
    /// The venue the symbol PINS (its `EXCHANGE:` prefix, lower-cased); `None`
    /// for a bare symbol. The venue that actually served it is
    /// `chart.data.resolve(symbol)`.
    pub provider: Option<ProviderName>,
    pub timeframe: Option<String>,
    pub bars: Option<usize>,
    /// The shown trading session; `None` is the provider's default (regular).
    pub session: Option<MarketSession>,
    pub offline: bool,
}

/// A built-in theme's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeName {
    Dark,
    Light,
}

impl ThemeName {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Dark => "dark",
            ThemeName::Light => "light",
        }
    }
}

/// The `theme` option: a built-in theme's name or the id of one added with
/// `registerTheme`, or a whole theme object (the chart's seven colors and font,
/// with optional `ui` settings, or a `createTheme` result).
#[derive(Clone, Debug, PartialEq)]
pub enum ThemeSpec {
    Id(ThemeId),
    Theme(VelaTheme),
}

impl From<&str> for ThemeSpec {
    fn from(id: &str) -> Self {
        Self::Id(id.to_string())
    }
}

impl From<String> for ThemeSpec {
    fn from(id: String) -> Self {
        Self::Id(id)
    }
}

impl From<VelaTheme> for ThemeSpec {
    fn from(theme: VelaTheme) -> Self {
        Self::Theme(theme)
    }
}

/// The `height` option: CSS pixels, or any CSS length.
#[derive(Clone, Debug, PartialEq)]
pub enum HeightOption {
    Px(f64),
    Css(String),
}

/// A renderer FACTORY: Vela calls it with the resolved display options. The
/// built-in default is the native renderer; a host crate supplies it.
pub type RendererConstructor = Rc<dyn Fn(RendererDisplayOptions) -> Rc<dyn ChartRenderer>>;

/// The chart's options. Its [`LocalizationOptions`] (`locale`, `direction`,
/// `messages`, `numbering_system`, `hour_cycle`, `week_start`, `formatters`)
/// change at runtime through `chart.setLocale`.
#[derive(Clone, Default)]
pub struct VelaOptions {
    /// How the chart obtains its candles (`symbol`, `timeframe`, `bars`,
    /// `session`, `visibleRange`, `data`).
    pub market: MarketConfig,
    pub localization: LocalizationOptions,
    /// false = static history; true = history + live forming candle.
    pub live: Option<bool>,
    /// A built-in or registered theme by id, the chart's seven colors and font
    /// (with `ui` settings), or a whole theme from `createTheme`.
    pub theme: Option<ThemeSpec>,
    pub height: Option<HeightOption>,
    /// Rendering backend as a renderer factory. Omit for the built-in native
    /// renderer (default).
    pub renderer: Option<RendererConstructor>,
    /// Scripting language used when `addIndicator` does not specify one. Default
    /// `"pine"` (or the first injected engine's language).
    pub default_language: Option<String>,
    /// Show the dashed line + axis label at the latest price (default true).
    pub current_price_line: Option<bool>,
    /// Use a logarithmic price scale on the price pane (default false).
    pub log_scale: Option<bool>,
    /// Native geometry backend: `Auto` (WebGL2 if available, else canvas2d), or
    /// forced. Native renderer only.
    pub native_backend: Option<NativeBackend>,
    /// Native-renderer animations. `Enabled(true/false)` toggles all; a config
    /// sets each motion independently: on/off, or its ease duration in ms.
    /// Default: eased **zoom on**, inertial **pan on but snappy** (short glide),
    /// gliding **autoscale on**, first-paint **reveal on**, live-bar glide
    /// **off**.
    pub animations: Option<AnimationsOption>,
    /// Neon glow/bloom intensity for line series (0 = off, ~0.6 = strong). WebGL2
    /// only: the canvas2d backend ignores it. Default 0.
    pub glow: Option<f64>,
    /// Bullish candle body/wick color (native renderer). Defaults to the theme's
    /// up color, which a theme switch then follows.
    pub up_color: Option<String>,
    /// Bearish candle body/wick color (native renderer). Defaults to the theme's
    /// down color, which a theme switch then follows.
    pub down_color: Option<String>,
    /// How the base price series is drawn (native renderer). Default `"candles"`.
    pub price_style: Option<PriceStyle>,
    /// Interactive user drawings (native renderer). Default: toolbar VISIBLE with
    /// the default tool set. `Bool(false)` hides the toolbar (the `chart.drawings`
    /// API still works headlessly); a config picks tools or groups and toggles
    /// the toolbar.
    pub drawings: Option<DrawingsOption>,
    /// The built-in volume indicator: per-bar volume columns anchored to the
    /// bottom of the price pane, on their own scale. Added automatically on chart
    /// creation (native renderer): pass `false` to opt out.
    pub volume: Option<bool>,
    /// Settings-dialog visibility policy. Default: everything visible.
    pub settings: Option<SettingsVisibilityPolicy>,
    /// Price-axis customization (native renderer).
    pub price_axis: Option<PriceAxisOptions>,
    /// Free panning (native renderer): unlock manual scaling on drag so one
    /// gesture pans time and price together. `false` by default. Seeds the
    /// runtime `freePan` feature and `priceScale.freePan`.
    pub free_pan: Option<FreePanTrigger>,
}

// ── free panning and the price axis ──

/// Pointer press state handed to a custom [`FreePanTrigger`] predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreePanEvent {
    pub button: i32,
    pub alt_key: bool,
    pub shift_key: bool,
    pub ctrl_key: bool,
    pub meta_key: bool,
    pub pointer_type: String,
}

/// Which pointer gesture unlocks manual scaling to pan time and price together on
/// an auto-scaled pane.
#[derive(Clone)]
pub enum FreePanTrigger {
    /// `true` is a plain primary drag, `false` is off.
    Enabled(bool),
    /// Plain primary drag (`'drag'`).
    Drag,
    /// Alt + primary drag.
    Alt,
    /// Shift + primary drag.
    Shift,
    /// Modifier (ctrl / meta) + primary drag (`'mod'`).
    Mod,
    /// Any custom button or modifier chord.
    Custom(Rc<dyn Fn(&FreePanEvent) -> bool>),
}

impl std::fmt::Debug for FreePanTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enabled(on) => f.debug_tuple("Enabled").field(on).finish(),
            Self::Drag => f.write_str("Drag"),
            Self::Alt => f.write_str("Alt"),
            Self::Shift => f.write_str("Shift"),
            Self::Mod => f.write_str("Mod"),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// Price-axis customization (see [`VelaOptions::price_axis`]).
#[derive(Clone, Default)]
pub struct PriceAxisOptions {
    /// Supplies the ticks of each pane's price axis: the labels and the
    /// horizontal gridlines both follow them. Called per pane when the pane's
    /// range, size or mode changes. Returning `None` keeps the built-in ticks.
    /// `None` (the default) = built-in ticks.
    pub ticks: Option<PriceAxisTicksFn>,
}

/// One price-axis tick: a gridline at `price` and its axis label.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceAxisTick {
    /// Where the tick sits, in prices (also in percent/indexed mode).
    pub price: f64,
    /// The axis label, drawn as given.
    pub label: String,
    /// `Some(true)` = emphasized (semibold), `Some(false)` = de-emphasized
    /// (muted), `None` = the regular label.
    pub major: Option<bool>,
}

/// The pane a [`PriceAxisTickContext`] is built for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceAxisPane {
    pub id: String,
    pub kind: PaneKind,
}

/// The mode the axis labels render in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PriceAxisMode {
    Price,
    Percent,
    Indexed,
}

/// What a [`PriceAxisTicksFn`] receives for one pane.
#[derive(Clone)]
pub struct PriceAxisTickContext {
    pub pane: PriceAxisPane,
    /// The pane's visible value range, in prices (also in percent/indexed mode).
    pub min: f64,
    pub max: f64,
    /// Whether the pane's scale is logarithmic.
    pub log: bool,
    /// Pane height in CSS px.
    pub height: f64,
    pub mode: PriceAxisMode,
    /// The percent/indexed baseline price (`None` in price mode).
    pub baseline: Option<f64>,
    /// The symbol's tick size, when known.
    pub mintick: Option<f64>,
    /// The axis font size in px.
    pub font_size: f64,
    /// A price's pane-relative y in CSS px (honours log and inverted scales).
    pub price_to_y: Rc<dyn Fn(f64) -> f64>,
    /// Vela's own ticks for this frame: return them, or a filtered/extended
    /// copy, to build on the default.
    pub defaults: Vec<PriceAxisTick>,
}

/// Builds a pane's price-axis ticks. Return the ticks to draw (an empty list
/// draws none), or `None` to keep the built-in ones. A renderer drops entries
/// with a non-finite `price`.
pub type PriceAxisTicksFn = Rc<dyn Fn(&PriceAxisTickContext) -> Option<Vec<PriceAxisTick>>>;

// ── animations ──

/// An `animations` entry that is a switch or a duration: `false`/`0` = off (the
/// motion is instant), `true` = the built-in duration, a number = its ease
/// time-constant in ms: the motion covers ~63% of the remaining distance per
/// time-constant and is visually settled after about three of them (clamped to
/// [`ANIMATION_EASE_MAX_MS`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimationValue {
    Enabled(bool),
    Ms(f64),
}

impl AnimationValue {
    /// The value as the renderer's feature channel carries it.
    pub fn to_value(self) -> Value {
        match self {
            Self::Enabled(on) => Value::Bool(on),
            Self::Ms(ms) => json!(ms),
        }
    }
}

impl From<bool> for AnimationValue {
    fn from(on: bool) -> Self {
        Self::Enabled(on)
    }
}

impl From<f64> for AnimationValue {
    fn from(ms: f64) -> Self {
        Self::Ms(ms)
    }
}

/// Per-feature native-renderer animation settings. Each field left `None` takes
/// its default (see the fields).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnimationConfig {
    /// Eased cursor-anchored wheel-zoom: the bar spacing glides toward each wheel
    /// notch's target instead of jumping. Default on ([`ZOOM_EASE_DEFAULT_MS`]).
    pub zoom: Option<AnimationValue>,
    /// Inertial/kinetic pan on touch: the velocity a finger flick releases with
    /// decays over this time-constant (a short, snappy glide by default; `false`
    /// stops dead). A mouse release always stops dead. Default on
    /// ([`PAN_INERTIA_DEFAULT_MS`]).
    pub pan: Option<AnimationValue>,
    /// The programmatic scroll glide: the scroll-to-latest button, `chart.panBy`,
    /// the keyboard pan keys, easing the view toward its target at constant zoom.
    /// Default: follows `pan` on/off, at [`SCROLL_EASE_DEFAULT_MS`] when on.
    pub scroll: Option<AnimationValue>,
    /// Autoscale glide: while a zoom or fling is in flight the price scale eases
    /// toward its new window instead of snapping every frame. Default on
    /// ([`AUTOSCALE_EASE_DEFAULT_MS`]).
    pub autoscale: Option<AnimationValue>,
    /// Glide of the forming bar: on a live tick the displayed high/low/close ease
    /// toward the new values instead of snapping. `true` uses
    /// [`LIVE_BAR_EASE_DEFAULT_MS`]; a number is the ease time-constant in ms
    /// (clamped to [`LIVE_BAR_EASE_MAX_MS`]); `false`/`0` snaps. A new bar always
    /// snaps. Default off: the painted candle is then never behind the real data.
    pub live_bar: Option<AnimationValue>,
    /// The first-paint reveal: candles draw themselves in, left to right, when
    /// they first appear. Default on.
    pub intro: Option<IntroSetting>,
}

/// The `animations` option: every motion at once, or each on its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimationsOption {
    Enabled(bool),
    Config(AnimationConfig),
}

/// The first-paint reveal styles: `Settle` overshoots and eases back, `Grow`
/// eases out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntroStyle {
    Settle,
    Grow,
}

impl IntroStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Settle => "settle",
            Self::Grow => "grow",
        }
    }
}

/// Object form of `animations.intro`: style and/or sweep duration (ms).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntroConfig {
    pub style: Option<IntroStyle>,
    pub duration: Option<f64>,
}

/// `animations.intro`: `Enabled(true)` is the default `settle` style; a style
/// name picks it; a config picks the style and/or the `duration` of the whole
/// sweep in ms (default [`INTRO_DURATION_DEFAULT_MS`], clamped to
/// [`INTRO_DURATION_MAX_MS`]); `Enabled(false)` is no reveal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IntroSetting {
    Enabled(bool),
    Style(IntroStyle),
    Config(IntroConfig),
}

impl IntroSetting {
    /// The setting as the renderer's feature channel carries it.
    pub fn to_value(self) -> Value {
        match self {
            Self::Enabled(on) => Value::Bool(on),
            Self::Style(style) => json!(style.as_str()),
            Self::Config(config) => {
                let mut object = serde_json::Map::new();
                if let Some(style) = config.style {
                    object.insert("style".into(), json!(style.as_str()));
                }
                if let Some(duration) = config.duration {
                    object.insert("duration".into(), json!(duration));
                }
                Value::Object(object)
            }
        }
    }
}

/// A resolved `animations.intro`: `style == None` is no reveal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntroAnimation {
    pub style: Option<IntroStyle>,
    pub duration: f64,
}

impl IntroAnimation {
    /// No reveal.
    pub const OFF: IntroAnimation = IntroAnimation {
        style: None,
        duration: 0.0,
    };

    /// `{ style: false | 'settle' | 'grow', duration }`, the original's shape.
    pub fn to_value(self) -> Value {
        let style = match self.style {
            Some(style) => json!(style.as_str()),
            None => Value::Bool(false),
        };
        json!({ "style": style, "duration": self.duration })
    }
}

// Built-in ease time-constants (ms): what `true` means for each `animations` entry.
/// Wheel-zoom glide.
pub const ZOOM_EASE_DEFAULT_MS: f64 = 70.0;
/// Inertial-pan velocity decay: short and snappy (the drift is about v0 times tau), not a long coast.
pub const PAN_INERTIA_DEFAULT_MS: f64 = 110.0;
/// Scroll-to-latest / `panBy` glide toward its target offset.
pub const SCROLL_EASE_DEFAULT_MS: f64 = 130.0;
/// Autoscale glide during a zoom/fling.
pub const AUTOSCALE_EASE_DEFAULT_MS: f64 = 80.0;
/// Time-constant (ms) of the live-bar glide when `animations.liveBar` is `true`.
pub const LIVE_BAR_EASE_DEFAULT_MS: f64 = 90.0;
/// Whole-sweep duration of the first-paint reveal.
pub const INTRO_DURATION_DEFAULT_MS: f64 = 650.0;
/// Upper bound (ms) for any ease time-constant: past this the chart visibly lags its input.
pub const ANIMATION_EASE_MAX_MS: f64 = 1000.0;
/// Upper bound (ms) for `animations.liveBar`: past this the candle visibly lags the feed.
pub const LIVE_BAR_EASE_MAX_MS: f64 = ANIMATION_EASE_MAX_MS;
/// Upper bound (ms) for the reveal sweep.
pub const INTRO_DURATION_MAX_MS: f64 = 5000.0;

/// Normalize a `boolean | number` animation entry to an ease time-constant in ms
/// (0 = off): `true` is `default_ms`, a positive finite number is itself clamped
/// to `max_ms`, anything else (`false`, `0`, negative, NaN, junk) is 0.
pub fn resolve_ease_ms_max(value: &Value, default_ms: f64, max_ms: f64) -> f64 {
    if value == &Value::Bool(true) {
        return default_ms;
    }
    match value.as_f64() {
        Some(n) if n.is_finite() && n > 0.0 => n.min(max_ms),
        _ => 0.0,
    }
}

/// [`resolve_ease_ms_max`] with the default ceiling, [`ANIMATION_EASE_MAX_MS`].
pub fn resolve_ease_ms(value: &Value, default_ms: f64) -> f64 {
    resolve_ease_ms_max(value, default_ms, ANIMATION_EASE_MAX_MS)
}

/// Normalize an `animations.liveBar` value to an ease time-constant in ms (0 = snap).
pub fn resolve_live_bar_ease_ms(value: &Value) -> f64 {
    resolve_ease_ms_max(value, LIVE_BAR_EASE_DEFAULT_MS, LIVE_BAR_EASE_MAX_MS)
}

/// Normalize an `animations.intro` value (also what the renderer's `intro`
/// feature accepts): `true` is the default settle reveal, a style name is that
/// style at the default duration, an object is its style/duration (missing
/// fields default), and anything else (`false`, `"none"`, `"off"`, null, junk) is
/// off. Like [`resolve_ease_ms`], the "absent = default" rule lives in
/// [`resolve_animations`].
pub fn resolve_intro(value: &Value) -> IntroAnimation {
    let default_duration = |style| IntroAnimation {
        style: Some(style),
        duration: INTRO_DURATION_DEFAULT_MS,
    };
    match value {
        Value::Bool(true) => default_duration(IntroStyle::Settle),
        Value::String(s) if s == "settle" => default_duration(IntroStyle::Settle),
        Value::String(s) if s == "grow" => default_duration(IntroStyle::Grow),
        Value::Object(_) | Value::Array(_) => {
            let style = if value.get("style").and_then(Value::as_str) == Some("grow") {
                IntroStyle::Grow
            } else {
                IntroStyle::Settle
            };
            let duration = match value.get("duration").and_then(Value::as_f64) {
                Some(d) if d.is_finite() && d > 0.0 => d.min(INTRO_DURATION_MAX_MS),
                _ => INTRO_DURATION_DEFAULT_MS,
            };
            IntroAnimation {
                style: Some(style),
                duration,
            }
        }
        _ => IntroAnimation::OFF,
    }
}

/// The renderer's resolved per-motion animation values (see [`RendererDisplayOptions`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedAnimations {
    pub anim_zoom: f64,
    pub anim_pan: f64,
    pub anim_scroll: f64,
    pub anim_autoscale: f64,
    pub anim_live_bar: f64,
    pub anim_intro: IntroAnimation,
}

fn ease_of(entry: Option<AnimationValue>, default_ms: f64) -> f64 {
    entry.map_or(0.0, |v| resolve_ease_ms(&v.to_value(), default_ms))
}

/// Resolve `VelaOptions::animations` to the renderer's per-motion values. `None`
/// (or `Enabled(true)`) is every motion at its default: the live-bar glide's
/// default is off. `Enabled(false)` disables everything, the reveal included. A
/// config sets each on its own; `scroll` left unset follows `pan` on/off.
pub fn resolve_animations(animations: Option<&AnimationsOption>) -> ResolvedAnimations {
    let cfg = match animations {
        Some(AnimationsOption::Enabled(false)) => {
            return ResolvedAnimations {
                anim_zoom: 0.0,
                anim_pan: 0.0,
                anim_scroll: 0.0,
                anim_autoscale: 0.0,
                anim_live_bar: 0.0,
                anim_intro: IntroAnimation::OFF,
            };
        }
        Some(AnimationsOption::Config(cfg)) => *cfg,
        Some(AnimationsOption::Enabled(true)) | None => AnimationConfig::default(),
    };
    let on = Some(AnimationValue::Enabled(true));
    let anim_pan = ease_of(cfg.pan.or(on), PAN_INERTIA_DEFAULT_MS);
    ResolvedAnimations {
        anim_zoom: ease_of(cfg.zoom.or(on), ZOOM_EASE_DEFAULT_MS),
        anim_pan,
        anim_scroll: match cfg.scroll {
            None if anim_pan > 0.0 => SCROLL_EASE_DEFAULT_MS,
            None => 0.0,
            Some(_) => ease_of(cfg.scroll, SCROLL_EASE_DEFAULT_MS),
        },
        anim_autoscale: ease_of(cfg.autoscale.or(on), AUTOSCALE_EASE_DEFAULT_MS),
        anim_live_bar: cfg
            .live_bar
            .map_or(0.0, |v| resolve_live_bar_ease_ms(&v.to_value())),
        anim_intro: resolve_intro(&cfg.intro.unwrap_or(IntroSetting::Enabled(true)).to_value()),
    }
}

/// Native geometry-layer backend selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NativeBackend {
    /// WebGL2 if available, else canvas2d.
    #[default]
    Auto,
    Canvas2d,
    Webgl2,
}

impl NativeBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Canvas2d => "canvas2d",
            Self::Webgl2 => "webgl2",
        }
    }
}

/// Display options passed to a renderer at construction.
#[derive(Clone)]
pub struct RendererDisplayOptions {
    pub current_price_line: bool,
    pub log_scale: bool,
    pub native_backend: NativeBackend,
    // Ease time-constants in ms; 0 = that motion is off (instant). See `AnimationConfig`.
    pub anim_zoom: f64,
    pub anim_pan: f64,
    pub anim_scroll: f64,
    pub anim_autoscale: f64,
    pub anim_live_bar: f64,
    pub anim_intro: IntroAnimation,
    pub glow: f64,
    pub up_color: String,
    pub down_color: String,
    pub price_style: PriceStyle,
    /// Host price-axis ticks (`VelaOptions::price_axis`); `None` = built-in ticks.
    pub price_axis_ticks: Option<PriceAxisTicksFn>,
    /// Free panning (`VelaOptions::free_pan`); `None` = off.
    pub free_pan: Option<FreePanTrigger>,
}

// ── panes and indicators ──

/// An indicator listed in a pane (`PaneInfo::indicators`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneIndicatorInfo {
    pub id: String,
    pub title: String,
    pub shorttitle: Option<String>,
    pub own_scale: bool,
}

/// A pane and the indicators it holds: a `chart.panes.list()` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub id: String,
    pub kind: PaneKind,
    /// Display order, top-to-bottom (0 = topmost, the price pane).
    pub order: usize,
    pub collapsed: bool,
    pub maximized: bool,
    /// The pane's share of the plot height relative to the other panes (the price
    /// pane defaults to 3, study panes to 1 on the native renderer). `None` on a
    /// renderer that does not report pane sizes.
    pub height_weight: Option<f64>,
    pub indicators: Vec<PaneIndicatorInfo>,
}

/// Where `AddIndicatorOptions::pane` places an indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AddIndicatorPane {
    Price,
    New,
}

/// Options for `chart.addIndicator(source, options?)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddIndicatorOptions {
    /// The indicator's id: an opaque, non-empty string the host owns (a document
    /// key, an editor tab, a UUID). It is what `handle.id`, `chart.indicators()`,
    /// the `indicator:*` events, `script:run`, and the legend/pane surfaces carry,
    /// so a host that supplies it can address the indicator without keeping a side
    /// map. Omit it and the chart mints one (stable for the chart's lifetime, not
    /// across reloads). An id already live on this chart is rejected: `addIndicator`
    /// fails, `runIndicator`/`runScript` resolve `ok: false`; never renamed silently.
    pub id: Option<String>,
    /// Which registered engine runs this script (by language id). Default: the
    /// chart's `default_language`.
    pub language: Option<String>,
    /// Input overrides, keyed by input title or varId.
    pub inputs: Option<Inputs>,
    /// Declaration-property overrides (a strategy's `initial_capital`, an
    /// indicator's `precision`, ...), keyed like the engine's props schema.
    /// Ignored by engines without props support.
    pub props: Option<Inputs>,
    /// Force overlay-vs-pane placement (default: read from `indicator(overlay=...)`).
    pub overlay: Option<bool>,
    /// Explicit pane placement.
    pub pane: Option<AddIndicatorPane>,
    /// Display title override.
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::visible_range::VisibleRangePreset;

    const ALL_DEFAULTS: ResolvedAnimations = ResolvedAnimations {
        anim_zoom: ZOOM_EASE_DEFAULT_MS,
        anim_pan: PAN_INERTIA_DEFAULT_MS,
        anim_scroll: SCROLL_EASE_DEFAULT_MS,
        anim_autoscale: AUTOSCALE_EASE_DEFAULT_MS,
        anim_live_bar: 0.0,
        anim_intro: IntroAnimation {
            style: Some(IntroStyle::Settle),
            duration: INTRO_DURATION_DEFAULT_MS,
        },
    };

    fn junk() -> Vec<Value> {
        vec![
            json!(false),
            json!(0),
            json!(-1),
            json!("70"),
            Value::Null,
            json!({}),
            json!([]),
        ]
    }

    #[test]
    fn ease_true_is_the_default_a_positive_number_is_itself_clamped_anything_else_is_off() {
        assert_eq!(resolve_ease_ms(&json!(true), 70.0), 70.0);
        assert_eq!(resolve_ease_ms(&json!(150), 70.0), 150.0);
        assert_eq!(resolve_ease_ms(&json!(99_999), 70.0), ANIMATION_EASE_MAX_MS);
        assert_eq!(resolve_ease_ms_max(&json!(50), 70.0, 40.0), 40.0);
        for value in junk() {
            assert_eq!(resolve_ease_ms(&value, 70.0), 0.0, "{value}");
        }
        // JSON has no NaN or Infinity; the typed route carries them.
        for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                resolve_ease_ms(&AnimationValue::Ms(n).to_value(), 70.0),
                0.0,
                "{n}"
            );
        }
    }

    #[test]
    fn live_bar_has_its_own_default_and_ceiling() {
        assert_eq!(
            resolve_live_bar_ease_ms(&json!(true)),
            LIVE_BAR_EASE_DEFAULT_MS
        );
        assert_eq!(resolve_live_bar_ease_ms(&json!(250)), 250.0);
        assert_eq!(resolve_live_bar_ease_ms(&json!(5000)), LIVE_BAR_EASE_MAX_MS);
        assert_eq!(resolve_live_bar_ease_ms(&Value::Null), 0.0);
    }

    #[test]
    fn intro_true_is_settle_a_style_name_an_object_or_off() {
        let settle = IntroAnimation {
            style: Some(IntroStyle::Settle),
            duration: INTRO_DURATION_DEFAULT_MS,
        };
        assert_eq!(resolve_intro(&json!(true)), settle);
        assert_eq!(
            resolve_intro(&json!("grow")),
            IntroAnimation {
                style: Some(IntroStyle::Grow),
                duration: INTRO_DURATION_DEFAULT_MS
            }
        );
        assert_eq!(
            resolve_intro(&json!({ "duration": 1200 })),
            IntroAnimation {
                style: Some(IntroStyle::Settle),
                duration: 1200.0
            }
        );
        assert_eq!(
            resolve_intro(&json!({ "style": "grow", "duration": 99_999 })),
            IntroAnimation {
                style: Some(IntroStyle::Grow),
                duration: INTRO_DURATION_MAX_MS
            }
        );
        assert_eq!(
            resolve_intro(&json!({ "style": "bounce", "duration": -5 })),
            settle
        );
        // `undefined` is off here, like resolve_ease_ms: resolve_animations owns the
        // "absent = default" rule.
        for off in [
            json!(false),
            json!("none"),
            json!("off"),
            Value::Null,
            json!(0),
            json!("junk"),
        ] {
            assert_eq!(resolve_intro(&off).style, None, "{off}");
        }
    }

    #[test]
    fn absent_empty_and_true_are_every_motion_at_its_builtin_duration() {
        assert_eq!(resolve_animations(None), ALL_DEFAULTS);
        assert_eq!(
            resolve_animations(Some(&AnimationsOption::Config(AnimationConfig::default()))),
            ALL_DEFAULTS
        );
        assert_eq!(
            resolve_animations(Some(&AnimationsOption::Enabled(true))),
            ALL_DEFAULTS
        );
    }

    #[test]
    fn false_is_everything_off_the_reveal_included() {
        assert_eq!(
            resolve_animations(Some(&AnimationsOption::Enabled(false))),
            ResolvedAnimations {
                anim_zoom: 0.0,
                anim_pan: 0.0,
                anim_scroll: 0.0,
                anim_autoscale: 0.0,
                anim_live_bar: 0.0,
                anim_intro: IntroAnimation::OFF,
            }
        );
    }

    #[test]
    fn each_motion_is_configured_on_its_own() {
        let out = resolve_animations(Some(&AnimationsOption::Config(AnimationConfig {
            zoom: Some(150.0.into()),
            pan: Some(false.into()),
            autoscale: Some(true.into()),
            intro: Some(IntroSetting::Style(IntroStyle::Grow)),
            ..AnimationConfig::default()
        })));
        assert_eq!(out.anim_zoom, 150.0);
        assert_eq!(out.anim_pan, 0.0);
        assert_eq!(out.anim_autoscale, AUTOSCALE_EASE_DEFAULT_MS);
        assert_eq!(
            out.anim_intro,
            IntroAnimation {
                style: Some(IntroStyle::Grow),
                duration: INTRO_DURATION_DEFAULT_MS
            }
        );
    }

    #[test]
    fn scroll_follows_pan_on_off_unless_set_on_its_own() {
        let scroll = |cfg: AnimationConfig| {
            resolve_animations(Some(&AnimationsOption::Config(cfg))).anim_scroll
        };
        let pan = |v: AnimationValue| AnimationConfig {
            pan: Some(v),
            ..AnimationConfig::default()
        };
        // `{ pan: false }` is an instant pan, panBy included.
        assert_eq!(scroll(pan(false.into())), 0.0);
        // On/off follows; the duration is scroll's own.
        assert_eq!(scroll(pan(300.0.into())), SCROLL_EASE_DEFAULT_MS);
        assert_eq!(
            scroll(AnimationConfig {
                pan: Some(false.into()),
                scroll: Some(200.0.into()),
                ..AnimationConfig::default()
            }),
            200.0
        );
        let scroll_off = AnimationConfig {
            scroll: Some(false.into()),
            ..AnimationConfig::default()
        };
        assert_eq!(scroll(scroll_off), 0.0);
        assert_eq!(
            resolve_animations(Some(&AnimationsOption::Config(scroll_off))).anim_pan,
            PAN_INERTIA_DEFAULT_MS
        );
    }

    #[test]
    fn live_bar_is_off_unless_asked_for() {
        let live = |v: Option<AnimationValue>| {
            resolve_animations(Some(&AnimationsOption::Config(AnimationConfig {
                live_bar: v,
                ..AnimationConfig::default()
            })))
            .anim_live_bar
        };
        assert_eq!(live(None), 0.0);
        assert_eq!(live(Some(true.into())), LIVE_BAR_EASE_DEFAULT_MS);
        assert_eq!(live(Some(250.0.into())), 250.0);
        assert_eq!(live(Some(false.into())), 0.0);
    }

    #[test]
    fn intro_values_round_trip_through_the_feature_channel() {
        assert_eq!(IntroSetting::Enabled(true).to_value(), json!(true));
        assert_eq!(
            IntroSetting::Style(IntroStyle::Grow).to_value(),
            json!("grow")
        );
        let config = IntroSetting::Config(IntroConfig {
            style: Some(IntroStyle::Grow),
            duration: Some(300.0),
        });
        assert_eq!(
            config.to_value(),
            json!({ "style": "grow", "duration": 300.0 })
        );
        assert_eq!(
            resolve_intro(&config.to_value()),
            IntroAnimation {
                style: Some(IntroStyle::Grow),
                duration: 300.0
            }
        );
        assert_eq!(
            IntroAnimation::OFF.to_value(),
            json!({ "style": false, "duration": 0.0 })
        );
    }

    #[test]
    fn sessions_narrow_only_the_exact_strings() {
        assert_eq!(
            normalize_session(&json!("regular")),
            Some(MarketSession::Regular)
        );
        assert_eq!(
            normalize_session(&json!("extended")),
            Some(MarketSession::Extended)
        );
        for other in [
            json!("Regular"),
            json!(""),
            json!(1),
            json!(true),
            Value::Null,
            json!(["regular"]),
        ] {
            assert_eq!(normalize_session(&other), None, "{other}");
        }
    }

    #[test]
    fn the_market_types_default_to_nothing_set() {
        let switch = MarketSwitch::default();
        assert_eq!(switch, MarketSwitch::default());
        assert!(switch.symbol.is_none() && switch.data.is_none() && switch.visible_range.is_none());
        let snapshot = MarketSnapshot::default();
        assert!(!snapshot.offline && snapshot.provider.is_none());
        let preset = InitialRange::Preset(VisibleRangePreset::YearToDate);
        assert_eq!(preset, InitialRange::Preset(VisibleRangePreset::YearToDate));
    }

    #[test]
    fn options_default_to_every_field_absent() {
        let options = VelaOptions::default();
        assert!(options.live.is_none() && options.theme.is_none() && options.renderer.is_none());
        assert!(options.market.symbol.is_none() && options.localization.locale.is_none());
        assert!(options.drawings.is_none() && options.free_pan.is_none());
        let theme: ThemeSpec = "dark".into();
        assert_eq!(theme, ThemeSpec::Id("dark".into()));
        assert_eq!(ThemeName::Light.as_str(), "light");
        assert_eq!(NativeBackend::default().as_str(), "auto");
    }

    #[test]
    fn a_free_pan_predicate_is_called_with_the_press() {
        let seen = Rc::new(Cell::new(false));
        let flag = seen.clone();
        let trigger = FreePanTrigger::Custom(Rc::new(move |e| {
            flag.set(true);
            e.button == 1 && e.alt_key
        }));
        let FreePanTrigger::Custom(predicate) = &trigger else {
            unreachable!()
        };
        let press = FreePanEvent {
            button: 1,
            alt_key: true,
            shift_key: false,
            ctrl_key: false,
            meta_key: false,
            pointer_type: "mouse".into(),
        };
        assert!(predicate(&press));
        assert!(seen.get());
        assert_eq!(format!("{trigger:?}"), "Custom(..)");
    }

    #[test]
    fn move_targets_and_panes_keep_their_shapes() {
        let target = MoveTarget::NewPane(NewPanePlacement {
            before: Some("p1".into()),
            after: None,
        });
        assert_eq!(
            serde_json::to_value(&target).unwrap(),
            json!({ "newPane": { "before": "p1" } })
        );
        let info = PaneInfo {
            id: "price".into(),
            kind: PaneKind::Price,
            order: 0,
            collapsed: false,
            maximized: false,
            height_weight: Some(3.0),
            indicators: vec![PaneIndicatorInfo {
                id: "ind-1".into(),
                title: "Volume".into(),
                shorttitle: None,
                own_scale: true,
            }],
        };
        assert_eq!(info.clone(), info);
        assert_eq!(AddIndicatorOptions::default().pane, None);
    }
}
