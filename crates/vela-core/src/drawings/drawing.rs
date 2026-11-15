//! The parent of every user drawing: the shared state, the behavior contract
//! and the plain-JSON record that crosses the renderer and persistence
//! boundaries.
//!
//! The original is a class hierarchy with `Drawing` at its root. Here the
//! shared state is [`DrawingBase`], which every concrete type embeds, and the
//! behavior is the [`Drawing`] trait. Geometry behaviors stay PURE functions
//! of `(anchors, Projector)`, so the whole family is renderer-neutral.

use std::any::Any;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::i18n::MessageLike;

use super::geometry::{DrawingPoint, FreeAxis, Projector};
use super::hittest::handle_at;
use super::schema::SettingsSchema;
use super::style::{DrawingStyle, DrawingText, PartialDrawingStyle};
use super::visibility::{sanitize_show_on, sanitize_show_on_keys};

/// Per-type extras as plain JSON (e.g. a box's `extend`, a fib's levels).
pub type Props = serde_json::Map<String, Value>;

/// Extra pixels a handle's pick radius adds to the body tolerance.
pub const HANDLE_TOLERANCE_PAD: f64 = 3.0;

fn yes() -> bool {
    true
}

/// The host's clock, when it installed one.
static CLOCK: OnceLock<fn() -> f64> = OnceLock::new();

/// Install the wall clock [`now_ms`] reads: a function returning epoch ms.
/// A WebAssembly host must call this once (with `Date.now`), because the
/// standard library has no clock on `wasm32-unknown-unknown`. The first
/// installation wins; returns whether this call was it.
pub fn set_clock(clock: fn() -> f64) -> bool {
    CLOCK.set(clock).is_ok()
}

/// Wall-clock now, in epoch ms: the installed clock, else the system clock
/// on native targets, else 0.
pub fn now_ms() -> f64 {
    if let Some(clock) = CLOCK.get() {
        return clock();
    }
    system_now_ms()
}

#[cfg(not(target_arch = "wasm32"))]
fn system_now_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

#[cfg(target_arch = "wasm32")]
fn system_now_ms() -> f64 {
    0.0
}

fn lenient_show_on<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Vec<String>>, D::Error> {
    let raw = Value::deserialize(de)?;
    Ok(sanitize_show_on(&raw))
}

/// The plain-JSON shape of one drawing: the ONLY representation that crosses
/// the renderer port and the persistence boundary. Its serde form matches the
/// original's documents field for field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializedDrawing {
    pub id: String,
    /// The drawing type key (`trendline`, `fibretracement`, …).
    #[serde(rename = "type")]
    pub type_key: String,
    pub pane_id: String,
    /// time+price anchors: the single source of geometry truth.
    pub anchors: Vec<DrawingPoint>,
    pub style: DrawingStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<DrawingText>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Draw-order key. On a renderer with drawing depth it shares ONE space
    /// with the pane's series, so a drawing can sit anywhere in the stack.
    #[serde(default)]
    pub z_index: f64,
    #[serde(default = "now_ms")]
    pub created_at: f64,
    /// Per-type extras; keeps the base closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<Props>,
    /// The timeframe bands the drawing shows on; absent = every one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_show_on"
    )]
    pub show_on: Option<Vec<String>>,
}

/// What a drawing is built from: a (partial) record. Serves both a fresh
/// drawing and one rehydrated from a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingInit {
    pub id: Option<String>,
    pub pane_id: String,
    pub anchors: Vec<DrawingPoint>,
    /// Overrides on the type's default style.
    pub style: Option<PartialDrawingStyle>,
    pub text: Option<DrawingText>,
    pub locked: Option<bool>,
    pub visible: Option<bool>,
    pub z_index: Option<f64>,
    pub created_at: Option<f64>,
    pub props: Option<Props>,
    pub show_on: Option<Vec<String>>,
}

impl DrawingInit {
    /// An empty record on `pane_id`.
    pub fn on_pane(pane_id: impl Into<String>) -> Self {
        Self {
            pane_id: pane_id.into(),
            ..Default::default()
        }
    }

    pub fn anchors(mut self, anchors: Vec<DrawingPoint>) -> Self {
        self.anchors = anchors;
        self
    }

    pub fn props(mut self, props: Props) -> Self {
        self.props = Some(props);
        self
    }
}

impl From<SerializedDrawing> for DrawingInit {
    fn from(doc: SerializedDrawing) -> Self {
        Self {
            id: Some(doc.id),
            pane_id: doc.pane_id,
            anchors: doc.anchors,
            style: Some(doc.style.to_partial()),
            text: doc.text,
            locked: Some(doc.locked),
            visible: Some(doc.visible),
            z_index: Some(doc.z_index),
            created_at: Some(doc.created_at),
            props: doc.props,
            show_on: doc.show_on,
        }
    }
}

/// Fallback id counter: used only when no id is supplied (the store assigns real ids).
static FALLBACK_SEQ: AtomicU64 = AtomicU64::new(0);

/// The state every drawing shares.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingBase {
    pub id: String,
    pub pane_id: String,
    /// DATA-space anchors (time+price). The only geometry the model stores.
    pub anchors: Vec<DrawingPoint>,
    pub style: DrawingStyle,
    pub text: Option<DrawingText>,
    pub locked: bool,
    pub visible: bool,
    pub z_index: f64,
    /// The timeframe bands the drawing shows on; `None` = every timeframe.
    pub show_on: Option<Vec<String>>,
    pub created_at: f64,
}

impl DrawingBase {
    /// Build the shared state from a record. Per-type extras (`init.props`)
    /// are NOT read here: a concrete type's constructor sets its own defaults
    /// first, then calls its `read_props` when the record carries props.
    pub fn new(init: &DrawingInit) -> Self {
        Self {
            id: init.id.clone().unwrap_or_else(|| {
                format!("dw-{}", FALLBACK_SEQ.fetch_add(1, Ordering::Relaxed) + 1)
            }),
            pane_id: init.pane_id.clone(),
            anchors: init.anchors.clone(),
            style: init
                .style
                .as_ref()
                .map(|over| DrawingStyle::default().merged(over))
                .unwrap_or_default(),
            text: init.text.clone(),
            locked: init.locked.unwrap_or(false),
            visible: init.visible.unwrap_or(true),
            z_index: init.z_index.unwrap_or(0.0),
            show_on: init
                .show_on
                .as_ref()
                .map(|keys| sanitize_show_on_keys(keys)),
            created_at: init.created_at.unwrap_or_else(now_ms),
        }
    }

    /// Pixel position of anchor `index`; `None` when the anchor is missing or its pane is gone.
    pub fn anchor_px(&self, index: usize, proj: &dyn Projector) -> Option<(f64, f64)> {
        let a = self.anchors.get(index)?;
        Some((proj.x_of(a.time), proj.y_of(a.price, &self.pane_id)?))
    }

    /// Pixel positions of every anchor; `None` when the pane is gone.
    pub fn anchors_px(&self, proj: &dyn Projector) -> Option<Vec<(f64, f64)>> {
        self.anchors
            .iter()
            .map(|a| Some((proj.x_of(a.time), proj.y_of(a.price, &self.pane_id)?)))
            .collect()
    }

    /// Pixel y of a price on this drawing's pane.
    pub fn y_of(&self, price: f64, proj: &dyn Projector) -> Option<f64> {
        proj.y_of(price, &self.pane_id)
    }

    /// The price span of the anchors; `None` without anchors.
    pub fn anchor_price_range(&self) -> Option<Span> {
        Span::of(self.anchors.iter().map(|a| a.price))
    }
}

/// One anchor's role + which axes its handle may move along.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchorSlot {
    pub role: String,
    pub free: FreeAxis,
}

impl AnchorSlot {
    pub fn new(role: impl Into<String>, free: FreeAxis) -> Self {
        Self {
            role: role.into(),
            free,
        }
    }
}

/// How many points a drawing takes, and which axes each is free on.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchorSchema {
    pub min: usize,
    pub max: usize,
    pub slots: Vec<AnchorSlot>,
}

impl AnchorSchema {
    /// A fixed count of anchors, each free on both axes, with the given roles.
    pub fn free(roles: &[&str]) -> Self {
        Self {
            min: roles.len(),
            max: roles.len(),
            slots: roles
                .iter()
                .map(|r| AnchorSlot::new(*r, FreeAxis::Both))
                .collect(),
        }
    }
}

/// How a tool is placed. Drives the placement state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlacementMode {
    /// Click each anchor; a variable-count tool keeps adding until a finish gesture.
    Click,
    /// Press at the first corner, drag, release at the second.
    Drag,
    /// Press, drag to capture a path, release.
    Freehand,
}

/// A pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Bounds {
    /// The tight bounds of a set of pixel points; `None` when empty.
    pub fn of_points(points: &[(f64, f64)]) -> Option<Self> {
        let xs = Span::of(points.iter().map(|p| p.0))?;
        let ys = Span::of(points.iter().map(|p| p.1))?;
        Some(Self {
            x: xs.min,
            y: ys.min,
            w: xs.max - xs.min,
            h: ys.max - ys.min,
        })
    }
}

/// A closed interval (a price span, a time span).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub min: f64,
    pub max: f64,
}

impl Span {
    /// The extent of a set of values; `None` when empty. A NaN value poisons
    /// the result, as `Math.min` / `Math.max` do.
    pub fn of(values: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut span: Option<Span> = None;
        for v in values {
            span = Some(match span {
                None => Span { min: v, max: v },
                Some(s) => Span {
                    min: if s.min.is_nan() || v.is_nan() {
                        f64::NAN
                    } else {
                        s.min.min(v)
                    },
                    max: if s.max.is_nan() || v.is_nan() {
                        f64::NAN
                    } else {
                        s.max.max(v)
                    },
                },
            });
        }
        span
    }
}

/// One editable level of a rich settings panel (Fibonacci levels).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditableLevel {
    pub ratio: f64,
    pub color: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The behavior of a user drawing.
///
/// A concrete type embeds a [`DrawingBase`] and implements the required
/// methods; the provided ones carry the original base class's defaults.
pub trait Drawing: Any {
    fn base(&self) -> &DrawingBase;
    fn base_mut(&mut self) -> &mut DrawingBase;
    /// The registered type key (`trendline`, `hline`, …).
    fn type_key(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    // ── anchor schema: how many points, which axes are free ──
    fn anchor_schema(&self) -> AnchorSchema;

    /// True once enough anchors exist to be a real shape.
    fn is_complete(&self) -> bool {
        self.base().anchors.len() >= self.anchor_schema().min
    }

    fn placement_mode(&self) -> PlacementMode {
        PlacementMode::Click
    }

    /// Hook run once after interactive placement finishes: lets a type
    /// finalize its anchors against the live projector (e.g. a position
    /// deriving its stop/target in pixel space). Default: no-op.
    fn on_placed(&mut self, _proj: &dyn Projector) {}

    /// Move handle `index` to `pt`. By default handle `i` IS anchor `i`,
    /// moved along the axes its slot frees; a type with more handles than
    /// anchors maps each extra handle onto the anchor coordinates it edits.
    fn drag_handle(&mut self, index: usize, pt: DrawingPoint) {
        let free = self
            .anchor_schema()
            .slots
            .get(index)
            .map(|s| s.free)
            .unwrap_or(FreeAxis::Both);
        let Some(a) = self.base_mut().anchors.get_mut(index) else {
            return;
        };
        if matches!(free, FreeAxis::Both | FreeAxis::X) {
            a.time = pt.time;
        }
        if matches!(free, FreeAxis::Both | FreeAxis::Y) {
            a.price = pt.price;
        }
    }

    /// After a single handle is dragged, re-impose any cross-anchor invariant
    /// (a position keeps its stop and target on opposite sides of the entry).
    fn constrain_handle_drag(&mut self, _index: usize) {}

    /// Apply a whole-body drag: translate the original anchors by (dt, dp) in
    /// data space. Default moves every anchor together; a type can pin some.
    fn translate_body(&self, dt: f64, dp: f64, orig: &[DrawingPoint]) -> Vec<DrawingPoint> {
        orig.iter()
            .map(|o| DrawingPoint {
                time: o.time + dt,
                price: o.price + dp,
            })
            .collect()
    }

    // ── geometry behaviors: PURE functions of (anchors, projector) ──
    /// Is the pixel (px,py) on this drawing's body, within `tol` px?
    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool;

    /// Pixel positions of the draggable handles (for painting + hit-test).
    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)>;

    /// Index of the grabbed handle, or `None` for the body. The default picks
    /// among [`Drawing::handle_points`] with the usual padded radius.
    fn hit_handle(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> Option<usize> {
        handle_at(
            px,
            py,
            &self.handle_points(proj),
            tol + HANDLE_TOLERANCE_PAD,
        )
    }

    /// Tight pixel bounds (selection box), or `None` when unresolvable.
    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds>;

    /// Visible price span on its pane.
    fn price_range(&self) -> Option<Span>;

    /// Time span (epoch ms) the drawing occupies, for visible-range culling.
    /// Default is the anchor extent; a full-width drawing overrides to `None`
    /// meaning "all time" so it never culls.
    fn time_extent(&self) -> Option<Span> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let anchors = &self.base().anchors;
        if anchors.is_empty() {
            return None;
        }
        for a in anchors {
            if a.time < lo {
                lo = a.time;
            }
            if a.time > hi {
                hi = a.time;
            }
        }
        Some(Span { min: lo, max: hi })
    }

    // ── settings (data-driven) ──
    fn schema(&self) -> SettingsSchema;

    /// The text a fresh drawing of this type starts with ("Note", "Callout"),
    /// or `None` for a type that seeds none.
    fn placeholder_text(&self) -> Option<MessageLike> {
        None
    }

    /// Editable per-level config for a rich settings panel (Fibonacci levels):
    /// each entry is mutated via a `levels.<i>.<field>` settings path.
    fn editable_levels(&self) -> Option<Vec<EditableLevel>> {
        None
    }

    /// Per-type extras to serialize into `props`.
    fn write_props(&self) -> Option<Props> {
        None
    }

    /// Read per-type extras from a `props` bag. Each type validates what it
    /// reads; an unknown or mistyped entry is ignored.
    fn read_props(&mut self, _props: &Props) {}

    /// Apply ONE `dot.path = value` setting. The default handles the shared
    /// state (`style.*`, `text.*`, `locked`, …) and routes any other path
    /// through the type's own props (see [`apply_setting_default`]).
    fn apply_setting(&mut self, path: &str, value: &Value) {
        apply_setting_default(self, path, value);
    }

    /// Apply a `{ "dot.path": value }` patch (a settings popup emits these).
    fn apply_settings(&mut self, patch: &Props) {
        for (path, value) in patch {
            self.apply_setting(path, value);
        }
    }

    /// Re-read per-type extras onto this instance (the store uses it on an edit).
    fn apply_props(&mut self, props: &Props) {
        self.read_props(props);
    }

    // ── serialization ──
    fn serialize(&self) -> SerializedDrawing {
        let base = self.base();
        SerializedDrawing {
            id: base.id.clone(),
            type_key: self.type_key().to_string(),
            pane_id: base.pane_id.clone(),
            anchors: base.anchors.clone(),
            style: base.style.clone(),
            text: base.text.clone(),
            locked: base.locked,
            visible: base.visible,
            z_index: base.z_index,
            created_at: base.created_at,
            props: self.write_props(),
            show_on: base.show_on.clone(),
        }
    }
}

/// Patch one serde-backed block by a sub-path. A value the block's type
/// rejects leaves it unchanged (the original would store it as is).
fn patch_block<T: Serialize + DeserializeOwned>(target: &mut T, rest: Option<&str>, value: &Value) {
    let mut json = match rest {
        None => value.clone(),
        Some(rest) => {
            let Ok(mut json) = serde_json::to_value(&*target) else {
                return;
            };
            set_by_path(&mut json, rest, value.clone());
            json
        }
    };
    // `null` clears an optional field; serde reads a missing key the same way.
    if let Value::Object(map) = &mut json {
        map.retain(|_, v| !v.is_null());
    }
    if let Ok(parsed) = serde_json::from_value(json) {
        *target = parsed;
    }
}

/// The base behavior of [`Drawing::apply_setting`].
///
/// The original assigns the value straight onto the instance by path. Here
/// the shared state is typed, so: `style.*` and `text.*` patch those blocks,
/// the scalar base fields take a value of their own type, and every other
/// path is an edit of the type's props bag (written, patched, read back).
pub fn apply_setting_default<D: Drawing + ?Sized>(d: &mut D, path: &str, value: &Value) {
    let (root, rest) = match path.split_once('.') {
        Some((root, rest)) => (root, Some(rest)),
        None => (path, None),
    };
    match root {
        "style" => patch_block(&mut d.base_mut().style, rest, value),
        "text" => {
            let base = d.base_mut();
            if rest.is_none() && value.is_null() {
                base.text = None;
            } else {
                patch_block(
                    base.text.get_or_insert_with(DrawingText::default),
                    rest,
                    value,
                );
            }
        }
        "locked" => {
            if let Some(v) = value.as_bool() {
                d.base_mut().locked = v;
            }
        }
        "visible" => {
            if let Some(v) = value.as_bool() {
                d.base_mut().visible = v;
            }
        }
        "zIndex" => {
            if let Some(v) = value.as_f64() {
                d.base_mut().z_index = v;
            }
        }
        "paneId" => {
            if let Some(v) = value.as_str() {
                d.base_mut().pane_id = v.to_string();
            }
        }
        "showOn" => d.base_mut().show_on = sanitize_show_on(value),
        "anchors" => {
            if let Ok(anchors) = serde_json::from_value::<Vec<DrawingPoint>>(value.clone()) {
                d.base_mut().anchors = anchors;
            }
        }
        _ => {
            let mut props = Value::Object(d.write_props().unwrap_or_default());
            set_by_path(&mut props, path, value.clone());
            if let Value::Object(map) = props {
                d.read_props(&map);
            }
        }
    }
}

/// Set `obj[a][b][c] = value` for a dotted `path`, creating plain
/// intermediates. Tolerant by design: a path that cannot be followed is a
/// no-op rather than a panic (persisted or untrusted patches must never crash
/// the store). A numeric segment indexes into an array.
pub fn set_by_path(obj: &mut Value, path: &str, value: Value) {
    let mut keys = path.split('.').peekable();
    let mut cur = obj;
    while let Some(key) = keys.next() {
        let last = keys.peek().is_none();
        match cur {
            Value::Array(items) => {
                let Some(slot) = key.parse::<usize>().ok().and_then(|i| items.get_mut(i)) else {
                    return;
                };
                if last {
                    *slot = value;
                    return;
                }
                if !slot.is_object() && !slot.is_array() {
                    *slot = Value::Object(Props::new());
                }
                cur = slot;
            }
            Value::Object(map) => {
                if last {
                    map.insert(key.to_string(), value);
                    return;
                }
                let slot = map.entry(key.to_string()).or_insert(Value::Null);
                if !slot.is_object() && !slot.is_array() {
                    *slot = Value::Object(Props::new());
                }
                cur = slot;
            }
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn set_by_path_creates_intermediates_and_indexes_arrays() {
        let mut v = json!({ "levels": [{ "color": "#000" }, { "color": "#111" }] });
        set_by_path(&mut v, "levels.1.color", json!("#fff"));
        set_by_path(&mut v, "a.b.c", json!(1));
        set_by_path(&mut v, "levels.9.color", json!("ignored"));
        assert_eq!(
            v,
            json!({ "levels": [{ "color": "#000" }, { "color": "#fff" }], "a": { "b": { "c": 1 } } })
        );
        let mut scalar = json!({ "x": 5 });
        set_by_path(&mut scalar, "x.y", json!(true));
        assert_eq!(scalar, json!({ "x": { "y": true } }));
    }

    #[test]
    fn span_and_bounds_follow_their_points() {
        assert_eq!(Span::of([3.0, 1.0, 2.0]), Some(Span { min: 1.0, max: 3.0 }));
        assert_eq!(Span::of([]), None);
        assert!(Span::of([1.0, f64::NAN]).unwrap().min.is_nan());
        assert_eq!(
            Bounds::of_points(&[(1.0, 5.0), (4.0, 2.0)]),
            Some(Bounds {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 3.0
            })
        );
    }

    #[test]
    fn a_record_loads_leniently_and_round_trips() {
        let doc: SerializedDrawing = serde_json::from_value(json!({
            "id": "dw-7",
            "type": "trendline",
            "paneId": "price",
            "anchors": [{ "time": 1, "price": 2 }],
            "style": {},
            "showOn": ["W", "nope", "1"]
        }))
        .unwrap();
        assert!(doc.visible);
        assert!(!doc.locked);
        assert_eq!(doc.show_on, Some(vec!["1".to_string(), "W".to_string()]));
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["type"], "trendline");
        assert_eq!(json["paneId"], "price");
        assert!(json.get("text").is_none());
        assert_eq!(
            serde_json::from_value::<SerializedDrawing>(json).unwrap(),
            doc
        );
    }

    #[test]
    fn base_takes_defaults_and_a_fallback_id() {
        let base = DrawingBase::new(&DrawingInit::on_pane("price"));
        assert!(base.id.starts_with("dw-"));
        assert!(base.visible);
        assert_eq!(base.style, DrawingStyle::default());
        assert_eq!(base.anchor_price_range(), None);
    }
}
