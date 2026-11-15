//! The ONE serializable shell-state document, shared by BOTH shells: the
//! workspace persists a multi-cell document; the widget persists the SAME
//! document with a single `c1` cell (`layout: "1"`). One format means one
//! mental model, one codec, and state that moves freely between shells (a saved
//! widget chart drops into a workspace slot as-is). This module is NEUTRAL: it
//! imports from neither shell.
//!
//! Design intent: the state SURFACE is the product (`getState()` /
//! `applyState()` / the `state:changed` notification); persistence is just an
//! adapter driven through the storage seam. Nothing here touches the URL: hosts
//! wanting shareable links compose them from `getState()` themselves.
//!
//! The JSON form matches the original field for field. Two representation
//! notes: objects whose keys the original keeps in insertion order are
//! `BTreeMap`s here (keys come back sorted; JSON object order carries no
//! meaning), and whole numbers are written without a fraction (`500`, never
//! `500.0`), like `JSON.stringify`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

/// The only document version this crate reads and writes.
pub const STATE_VERSION: u32 = 1;

// --- JSON number text ---------------------------------------------------------

/// A number as `JSON.stringify` would write it: whole values lose their
/// fraction, non-finite values become `null`.
fn js_number_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}

fn ser_num<S: Serializer>(n: &f64, s: S) -> Result<S::Ok, S::Error> {
    js_number_value(*n).serialize(s)
}

fn ser_opt_num<S: Serializer>(n: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match n {
        Some(n) => ser_num(n, s),
        None => s.serialize_none(),
    }
}

fn ser_opt_nums<S: Serializer>(v: &Option<Vec<f64>>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => s.collect_seq(v.iter().map(|n| js_number_value(*n))),
        None => s.serialize_none(),
    }
}

fn ser_opt_num_map<S: Serializer>(
    m: &Option<BTreeMap<String, f64>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match m {
        Some(m) => s.collect_map(m.iter().map(|(k, n)| (k, js_number_value(*n)))),
        None => s.serialize_none(),
    }
}

// --- types --------------------------------------------------------------------

/// The linkable dimensions. `crosshair` mirrors the pointer time onto
/// same-group cells as GHOST crosshairs (renderers without the optional
/// external-crosshair seam simply never display one). `drawings` copies each
/// NEWLY CREATED drawing onto same-group cells and keeps the set linked: edits
/// and removals of any member follow. Link membership is session-scoped and
/// survives a toggle-off (re-enabling resumes edit/delete for drawings paired
/// earlier; drawings created while off stay independent). After a reload, every
/// drawing is unpaired again. `style` mirrors the chart's presentation settings
/// (the Canvas and Scales-and-lines slice of the renderer config plus the
/// status-line display prefs) onto same-group cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncKind {
    Viewport,
    Symbol,
    Timeframe,
    Crosshair,
    Drawings,
    Style,
}

impl SyncKind {
    /// Every linkable dimension: the one list the codec and both shells iterate.
    pub const ALL: [SyncKind; 6] = [
        Self::Viewport,
        Self::Symbol,
        Self::Timeframe,
        Self::Crosshair,
        Self::Drawings,
        Self::Style,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewport => "viewport",
            Self::Symbol => "symbol",
            Self::Timeframe => "timeframe",
            Self::Crosshair => "crosshair",
            Self::Drawings => "drawings",
            Self::Style => "style",
        }
    }
}

/// One link's configuration: `Flag(false)`/absent = off; `Flag(true)` = ALL
/// cells linked (one implicit group); `Groups` maps cell id to group name, and
/// only cells sharing a group follow each other (a cell absent from the map is
/// unlinked).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SyncSetting {
    Flag(bool),
    Groups(BTreeMap<String, String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<SyncSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SyncSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<SyncSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crosshair: Option<SyncSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawings: Option<SyncSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<SyncSetting>,
}

impl SyncOptions {
    pub fn get(&self, kind: SyncKind) -> Option<&SyncSetting> {
        match kind {
            SyncKind::Viewport => self.viewport.as_ref(),
            SyncKind::Symbol => self.symbol.as_ref(),
            SyncKind::Timeframe => self.timeframe.as_ref(),
            SyncKind::Crosshair => self.crosshair.as_ref(),
            SyncKind::Drawings => self.drawings.as_ref(),
            SyncKind::Style => self.style.as_ref(),
        }
    }

    pub fn set(&mut self, kind: SyncKind, setting: Option<SyncSetting>) {
        let slot = match kind {
            SyncKind::Viewport => &mut self.viewport,
            SyncKind::Symbol => &mut self.symbol,
            SyncKind::Timeframe => &mut self.timeframe,
            SyncKind::Crosshair => &mut self.crosshair,
            SyncKind::Drawings => &mut self.drawings,
            SyncKind::Style => &mut self.style,
        };
        *slot = setting;
    }
}

/// Splitter track weights along each grid axis.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackSizes {
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_nums"
    )]
    pub cols: Option<Vec<f64>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_nums"
    )]
    pub rows: Option<Vec<f64>>,
}

/// The docked side panels: a SHELL-level pref (one dock serves every cell of a
/// workspace). `open` is the single panel showing (the dock is exclusive);
/// `widths` holds only the columns the user actually resized, by panel id, so a
/// panel's declared width stays in charge until then; `pinned` lists the
/// floating (overlay) panels the user pinned as columns. Absent altogether in
/// documents written before the dock existed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelsState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_num_map"
    )]
    pub widths: Option<BTreeMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<Vec<String>>,
}

/// The drawing tools' magnet mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Magnet {
    Off,
    Weak,
    Strong,
}

impl Magnet {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Weak => "weak",
            Self::Strong => "strong",
        }
    }
}

/// The drawing tools' shared preferences: one set per shell, like the favorite
/// stars: each tool's remembered settings (what its next drawing starts from),
/// the magnet, and stay-in-drawing-mode. `defaults` maps a tool type to its
/// settings and passes through opaquely; the drawing layer validates each entry
/// at restore time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingToolsState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defaults: Option<BTreeMap<String, Value>>,
    /// Each tool's saved looks: a list of `{ name, settings }`, by tool type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub templates: Option<BTreeMap<String, Vec<Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnet: Option<Magnet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stay: Option<bool>,
}

/// The status line's segments, by part, as the settings dialog's Status line
/// tab and the status line's right-click menu leave them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatuslineParts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ohlc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<bool>,
}

impl StatuslineParts {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One persisted manifest-instance entry (see [`CellState::indicators`]): the
/// bare name when every value sits on its declaration default and the
/// indicator is visible, else the name plus the input/prop DELTAS and a
/// `hidden` flag (defaults are never frozen into documents).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PersistedManifestEntry {
    Name(String),
    Entry {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        props: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hidden: Option<bool>,
    },
}

/// One persisted native-indicator entry (see [`CellState::indicators`]). The
/// bare TYPE is the historical shape and still the common case; the object form
/// carries the input DELTAS and/or the hidden flag so native settings and
/// visibility survive a reload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PersistedNativeEntry {
    Type(String),
    Entry {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hidden: Option<bool>,
    },
}

/// The indicator ledger: manifest entries plus native entries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndicatorsState {
    pub manifest: Vec<PersistedManifestEntry>,
    pub natives: Vec<PersistedNativeEntry>,
}

/// Per-chart (per-cell) state: the market, the display prefs, the content
/// documents, and the indicator ledger. The widget's whole chart state is ONE of
/// these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CellState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The symbol's venue. Mirrors the symbol's own `EXCHANGE:` prefix on new
    /// saves; pre-prefix documents stored it beside a BARE symbol:
    /// [`prefixed_symbol`] welds the two back into the one canonical form at
    /// restore time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_style: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_num"
    )]
    pub bars: Option<f64>,
    /// Trading session shown (`extended` persisted; absent = regular, the
    /// default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Symbol watermark visibility: a per-chart display pref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watermark: Option<bool>,
    /// The "Replay" line under the watermark while replaying: a per-chart
    /// display pref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_watermark: Option<bool>,
    /// Indicator titles (the in-chart legend rows) visibility: a per-chart
    /// display pref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicator_titles: Option<bool>,
    /// Plot values beside the legend titles visibility: a per-chart display
    /// pref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicator_values: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statusline_parts: Option<StatuslineParts>,
    /// The renderer's cosmetic config document (`renderer.getConfig()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer_config: Option<Value>,
    /// The user-drawings document (`drawings.toJSON()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawings: Option<Value>,
    /// The indicator ledger: manifest entries + native entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicators: Option<IndicatorsState>,
    /// Third-party per-chart state, by namespaced key (`vendor.feature`):
    /// written and read by registered state-persistence handlers (scope
    /// `cell`). Values are OPAQUE here: the codec preserves entries verbatim (a
    /// key whose handler is absent this session still round-trips) and each
    /// handler validates its own payload at restore. JSON-serializable values
    /// only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<BTreeMap<String, Value>>,
}

/// One entry of the document's `charts` array: a chart's state plus its cell
/// IDENTITY.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartState {
    /// The cell's durable identity: its declared name (`btc`), or `c<N>` for a
    /// slot no entry declared. Unique within the document; array position
    /// restores slot order.
    pub id: String,
    #[serde(flatten)]
    pub cell: CellState,
}

/// The versioned shell-state document: everything `applyState` restores.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub version: u32,
    /// The layout id: always `1` for a widget. Restoring an id that is not
    /// registered keeps the current layout: register custom layouts before
    /// applying a saved state.
    pub layout: String,
    /// Splitter track weights, per layout id (workspace only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_sizes: Option<BTreeMap<String, TrackSizes>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_id: Option<String>,
    /// Sync links (workspace only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncOptions>,
    /// Shared display timezone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Favorite drawing-tool types: a SHARED preference (one star set per
    /// shell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorites: Option<Vec<String>>,
    /// Favorite timeframes (the topbar's quick-switch chips): a SHARED
    /// preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeframe_favorites: Option<Vec<String>>,
    /// The drawing tools' remembered settings, magnet and stay mode: a SHARED
    /// preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawing_tools: Option<DrawingToolsState>,
    /// The docked side panels: which one is open, and the widths the user
    /// dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panels: Option<PanelsState>,
    /// Per-chart state, one entry per SLOT (a single `c1` entry for the
    /// widget). Ids are unique: the codec drops id-less entries and keeps the
    /// LAST duplicate.
    pub charts: Vec<ChartState>,
    /// Third-party document-level state, by namespaced key (`vendor.feature`):
    /// the `global` scope counterpart of [`CellState::ext`], same opacity
    /// contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<BTreeMap<String, Value>>,
}

impl WorkspaceState {
    /// An empty document for `layout`.
    pub fn new(layout: impl Into<String>) -> Self {
        Self {
            version: STATE_VERSION,
            layout: layout.into(),
            track_sizes: None,
            active_cell_id: None,
            sync: None,
            timezone: None,
            favorites: None,
            timeframe_favorites: None,
            drawing_tools: None,
            panels: None,
            charts: Vec::new(),
            ext: None,
        }
    }
}

// --- codec --------------------------------------------------------------------

/// A cell's symbol in the canonical PREFIXED form: pre-prefix documents stored
/// the venue in `provider` beside a BARE symbol, so weld the two back together
/// on restore. A symbol that already carries a prefix wins (new saves mirror it
/// into `provider`).
pub fn prefixed_symbol(cell: Option<&CellState>) -> Option<String> {
    let cell = cell?;
    let symbol = cell.symbol.as_deref().filter(|s| !s.is_empty())?;
    match cell.provider.as_deref() {
        Some(provider) if !provider.is_empty() && !symbol.contains(':') => {
            Some(format!("{provider}:{symbol}"))
        }
        _ => Some(symbol.to_owned()),
    }
}

/// Serialize a state document (the inverse of [`decode_state`]).
pub fn encode_state(state: &WorkspaceState) -> String {
    // Plain data with string keys: serialization cannot fail.
    serde_json::to_string(state).unwrap_or_default()
}

/// Parse and sanitize a persisted payload. `None` on anything unusable (wrong
/// version, not JSON, not an object): malformed FIELDS are dropped, never
/// failed on.
pub fn decode_state(raw: &str) -> Option<WorkspaceState> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|doc| sanitize_state(&doc))
}

/// `Object.entries` of a value JavaScript calls an object: a map, or an array
/// keyed by index. `None` for null and primitives.
fn object_entries(v: &Value) -> Option<Vec<(String, &Value)>> {
    match v {
        Value::Object(m) => Some(m.iter().map(|(k, v)| (k.clone(), v)).collect()),
        Value::Array(a) => Some(
            a.iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v))
                .collect(),
        ),
        _ => None,
    }
}

/// `typeof v === 'object' && v !== null` (arrays included).
fn is_js_object(v: &Value) -> bool {
    matches!(v, Value::Object(_) | Value::Array(_))
}

/// A string-keyed bag: an object that is neither null nor an array.
fn plain_bag(v: Option<&Value>) -> Option<Map<String, Value>> {
    match v {
        Some(Value::Object(m)) => Some(m.clone()),
        _ => None,
    }
}

/// A finite number above zero.
fn positive_number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite() && *n > 0.0)
}

fn string_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

fn bool_of(v: Option<&Value>) -> Option<bool> {
    v.and_then(Value::as_bool)
}

fn strings_in(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Validate an untrusted state document field by field (the `applyState`
/// gate). Unknown or malformed fields are dropped; nested renderer-config and
/// drawings documents pass through OPAQUELY: their own consumers validate them.
pub fn sanitize_state(doc: &Value) -> Option<WorkspaceState> {
    let d = doc.as_object()?;
    if d.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let mut out = WorkspaceState::new(d.get("layout")?.as_str()?);

    if let Some(Value::Array(raw_charts)) = d.get("charts") {
        // Unique by id, LAST duplicate wins (a replaced entry keeps its first-seen position).
        let mut position: HashMap<&str, usize> = HashMap::new();
        for raw in raw_charts {
            let Some(cell) = raw.as_object() else {
                continue;
            };
            let Some(id) = cell
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let chart = ChartState {
                id: id.to_owned(),
                cell: sanitize_cell(cell),
            };
            match position.get(id) {
                Some(&at) => out.charts[at] = chart,
                None => {
                    position.insert(id, out.charts.len());
                    out.charts.push(chart);
                }
            }
        }
    }
    out.active_cell_id = string_of(d.get("activeCellId"));
    out.timezone = string_of(d.get("timezone")).filter(|tz| !tz.is_empty());
    out.favorites = Some(strings_in(d.get("favorites"))).filter(|f| !f.is_empty());
    out.timeframe_favorites =
        Some(strings_in(d.get("timeframeFavorites"))).filter(|f| !f.is_empty());
    out.drawing_tools = d.get("drawingTools").and_then(sanitize_drawing_tools);
    out.sync = d.get("sync").and_then(sanitize_sync);
    out.track_sizes = d.get("trackSizes").and_then(sanitize_track_sizes);
    out.panels = d.get("panels").and_then(sanitize_panels);
    out.ext = d.get("ext").and_then(sanitize_ext);
    Some(out)
}

/// The `ext` bag passes through OPAQUELY (the rendererConfig/drawings
/// precedent): only object-ness and key shape are checked here; each entry's
/// payload is validated by the handler that owns the key, at restore time.
/// Entries whose handler is not registered this session survive verbatim, so a
/// document never loses a plugin's state just because the plugin was not loaded
/// when the document passed through.
fn sanitize_ext(raw: &Value) -> Option<BTreeMap<String, Value>> {
    let Value::Object(map) = raw else { return None };
    let out: BTreeMap<String, Value> = map
        .iter()
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Some(out).filter(|o| !o.is_empty())
}

/// Shapes only: each remembered-settings entry must be an object (its contents
/// are the restoring chart's to validate); the magnet must be a known mode.
fn sanitize_drawing_tools(raw: &Value) -> Option<DrawingToolsState> {
    let r = raw.as_object()?;
    let mut out = DrawingToolsState::default();
    if let Some(Value::Object(defaults)) = r.get("defaults") {
        let kept: BTreeMap<String, Value> = defaults
            .iter()
            .filter(|(tool, value)| !tool.is_empty() && value.is_object())
            .map(|(tool, value)| (tool.clone(), value.clone()))
            .collect();
        out.defaults = Some(kept).filter(|k| !k.is_empty());
    }
    if let Some(Value::Object(templates)) = r.get("templates") {
        let kept: BTreeMap<String, Vec<Value>> = templates
            .iter()
            .filter_map(|(tool, list)| match list {
                Value::Array(items) if !tool.is_empty() && !items.is_empty() => {
                    Some((tool.clone(), items.clone()))
                }
                _ => None,
            })
            .collect();
        out.templates = Some(kept).filter(|k| !k.is_empty());
    }
    out.magnet = match r.get("magnet").and_then(Value::as_str) {
        Some("off") => Some(Magnet::Off),
        Some("weak") => Some(Magnet::Weak),
        Some("strong") => Some(Magnet::Strong),
        _ => None,
    };
    out.stay = bool_of(r.get("stay"));
    Some(out).filter(|o| *o != DrawingToolsState::default())
}

/// A retired built-in native rewritten as the study that replaced it, so a
/// saved chart restores the same study instead of an unregistered type. The
/// combined Linear Regression (type `linear-regression`, a Curve/Slope `mode`)
/// became two studies.
fn successor_native(entry: PersistedNativeEntry) -> PersistedNativeEntry {
    let (kind, saved_inputs, hidden) = match &entry {
        PersistedNativeEntry::Type(kind) => (kind.as_str(), None, false),
        PersistedNativeEntry::Entry {
            kind,
            inputs,
            hidden,
        } => (kind.as_str(), inputs.as_ref(), *hidden == Some(true)),
    };
    if kind != "linear-regression" {
        return entry;
    }
    let mut rest = saved_inputs.cloned().unwrap_or_default();
    let mode = rest.remove("mode");
    let line_color = rest.remove("lineColor");
    let slope = mode.as_ref().and_then(Value::as_str) == Some("Slope");
    let mut inputs = Map::new();
    if slope {
        inputs.extend(rest);
        if let Some(color) = line_color {
            inputs.insert("lineColor".into(), color);
        }
    } else {
        // Saved deltas omit the curve's 100-bar default, which the LSMA's default is not.
        inputs.insert("length".into(), Value::from(100));
        inputs.extend(rest);
        if let Some(color) = line_color {
            inputs.insert("neutralColor".into(), color);
        }
    }
    let next = if slope {
        "linear-regression-slope"
    } else {
        "lsma"
    };
    if inputs.is_empty() && !hidden {
        return PersistedNativeEntry::Type(next.to_owned());
    }
    PersistedNativeEntry::Entry {
        kind: next.to_owned(),
        inputs: Some(inputs).filter(|i| !i.is_empty()),
        hidden: hidden.then_some(true),
    }
}

fn sanitize_manifest_entry(n: &Value) -> Option<PersistedManifestEntry> {
    if let Value::String(name) = n {
        return Some(PersistedManifestEntry::Name(name.clone()));
    }
    let e = n.as_object()?;
    let name = e.get("name")?.as_str()?;
    let inputs = plain_bag(e.get("inputs"));
    let props = plain_bag(e.get("props"));
    let hidden = e.get("hidden") == Some(&Value::Bool(true));
    if inputs.is_none() && props.is_none() && !hidden {
        return Some(PersistedManifestEntry::Name(name.to_owned()));
    }
    Some(PersistedManifestEntry::Entry {
        name: name.to_owned(),
        inputs,
        props,
        hidden: hidden.then_some(true),
    })
}

fn sanitize_native_entry(n: &Value) -> Option<PersistedNativeEntry> {
    if let Value::String(kind) = n {
        return Some(PersistedNativeEntry::Type(kind.clone()));
    }
    let e = n.as_object()?;
    let kind = e.get("type")?.as_str()?;
    let inputs = plain_bag(e.get("inputs"));
    let hidden = e.get("hidden") == Some(&Value::Bool(true));
    if inputs.is_none() && !hidden {
        return Some(PersistedNativeEntry::Type(kind.to_owned()));
    }
    Some(PersistedNativeEntry::Entry {
        kind: kind.to_owned(),
        inputs,
        hidden: hidden.then_some(true),
    })
}

fn sanitize_indicators(ind: &Value) -> Option<IndicatorsState> {
    match ind {
        // Bare names pass as-is; object entries keep only a string name and
        // plain-object value bags (the add path validates individual values
        // against the schema).
        Value::Object(m) => Some(IndicatorsState {
            manifest: match m.get("manifest") {
                Some(Value::Array(items)) => {
                    items.iter().filter_map(sanitize_manifest_entry).collect()
                }
                _ => Vec::new(),
            },
            // Native entries mirror the manifest shape: bare types pass as-is;
            // object entries keep only a string type, a plain-object input bag,
            // and a true hidden flag.
            natives: match m.get("natives") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(sanitize_native_entry)
                    .map(successor_native)
                    .collect(),
                _ => Vec::new(),
            },
        }),
        // An array is a JavaScript object with neither list.
        Value::Array(_) => Some(IndicatorsState::default()),
        _ => None,
    }
}

fn sanitize_cell(c: &Map<String, Value>) -> CellState {
    let mut out = CellState {
        symbol: string_of(c.get("symbol")),
        provider: string_of(c.get("provider")),
        timeframe: string_of(c.get("timeframe")),
        price_style: string_of(c.get("priceStyle")),
        bars: c.get("bars").and_then(positive_number),
        session: string_of(c.get("session")).filter(|s| s == "regular" || s == "extended"),
        watermark: bool_of(c.get("watermark")),
        replay_watermark: bool_of(c.get("replayWatermark")),
        indicator_titles: bool_of(c.get("indicatorTitles")),
        indicator_values: bool_of(c.get("indicatorValues")),
        ..CellState::default()
    };
    if let Some(Value::Object(bag)) = c.get("statuslineParts") {
        let parts = StatuslineParts {
            logo: bool_of(bag.get("logo")),
            name: bool_of(bag.get("name")),
            market: bool_of(bag.get("market")),
            ohlc: bool_of(bag.get("ohlc")),
            change: bool_of(bag.get("change")),
        };
        out.statusline_parts = Some(parts).filter(|p| !p.is_empty());
    }
    out.renderer_config = c.get("rendererConfig").filter(|v| is_js_object(v)).cloned();
    out.drawings = c.get("drawings").filter(|v| is_js_object(v)).cloned();
    out.indicators = c.get("indicators").and_then(sanitize_indicators);
    out.ext = c.get("ext").and_then(sanitize_ext);
    out
}

fn sanitize_sync(raw: &Value) -> Option<SyncOptions> {
    let s = raw.as_object()?;
    let mut out = SyncOptions::default();
    for kind in SyncKind::ALL {
        let Some(v) = s.get(kind.as_str()) else {
            continue;
        };
        let setting = match v {
            Value::Bool(true) => Some(SyncSetting::Flag(true)),
            v if is_js_object(v) => {
                let groups: BTreeMap<String, String> = object_entries(v)
                    .into_iter()
                    .flatten()
                    .filter_map(|(id, g)| g.as_str().map(|g| (id, g.to_owned())))
                    .collect();
                (!groups.is_empty()).then_some(SyncSetting::Groups(groups))
            }
            _ => None,
        };
        out.set(kind, setting);
    }
    Some(out).filter(|o| *o != SyncOptions::default())
}

fn sanitize_panels(raw: &Value) -> Option<PanelsState> {
    let p = raw.as_object()?;
    let mut out = PanelsState {
        open: string_of(p.get("open")).filter(|open| !open.is_empty()),
        ..PanelsState::default()
    };
    if let Some(entries) = p.get("widths").and_then(object_entries) {
        // Widths are clamped again by the panel that receives them; here we
        // only reject values that are not a usable number at all.
        let widths: BTreeMap<String, f64> = entries
            .into_iter()
            .filter_map(|(id, px)| positive_number(px).map(|px| (id, px)))
            .collect();
        out.widths = Some(widths).filter(|w| !w.is_empty());
    }
    if let Some(Value::Array(items)) = p.get("pinned") {
        let mut seen = HashSet::new();
        let pinned: Vec<String> = items
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty() && seen.insert(*id))
            .map(str::to_owned)
            .collect();
        out.pinned = Some(pinned).filter(|p| !p.is_empty());
    }
    Some(out).filter(|o| *o != PanelsState::default())
}

fn sanitize_track_sizes(raw: &Value) -> Option<BTreeMap<String, TrackSizes>> {
    let mut out = BTreeMap::new();
    for (layout_id, ts) in object_entries(raw)? {
        let Some(t) = ts.as_object() else { continue };
        let axis = |name: &str| -> Option<Vec<f64>> {
            let Some(Value::Array(weights)) = t.get(name) else {
                return None;
            };
            let weights: Option<Vec<f64>> = weights.iter().map(positive_number).collect();
            weights.filter(|w| !w.is_empty())
        };
        let entry = TrackSizes {
            cols: axis("cols"),
            rows: axis("rows"),
        };
        if entry.cols.is_some() || entry.rows.is_some() {
            out.insert(layout_id, entry);
        }
    }
    Some(out).filter(|o| !o.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The fixture of `workspace-persist.test.ts`.
    fn full_doc() -> Value {
        json!({
            "version": 1,
            "layout": "4",
            "activeCellId": "c2",
            "timezone": "Europe/Paris",
            "favorites": ["trendline", "hline"],
            "timeframeFavorites": ["15", "60", "D"],
            "drawingTools": {
                "defaults": { "trendline": { "style": { "lineColor": "#ff8800", "lineWidth": 3 } } },
                "magnet": "weak",
                "stay": true
            },
            "sync": { "viewport": true, "symbol": { "c1": "a", "c2": "a" }, "crosshair": true, "drawings": true, "style": true },
            "trackSizes": { "4": { "cols": [1.4, 0.6], "rows": [1, 1] } },
            "panels": { "open": "objects", "widths": { "objects": 320 }, "pinned": ["vendor.editor"] },
            "charts": [
                {
                    "id": "c1",
                    "symbol": "BTCUSDT",
                    "provider": "binance",
                    "timeframe": "60",
                    "priceStyle": "candles",
                    "bars": 500,
                    "watermark": false,
                    "indicatorTitles": false,
                    "indicatorValues": false,
                    "statuslineParts": { "logo": true, "name": true, "market": false, "ohlc": false, "change": true },
                    "rendererConfig": { "theme": "dark", "nested": { "any": ["shape"] } },
                    "drawings": { "version": 1, "items": [{ "type": "trendline" }] },
                    "indicators": { "manifest": ["EMA 20"], "natives": ["volume"] },
                    "ext": { "velapro.indicators": [{ "slug": "smart-money", "inputs": { "len": 20 } }] }
                },
                { "id": "c2", "symbol": "ETHUSDT", "timeframe": "15" }
            ],
            "ext": { "velapro.favorites": { "starred": ["smart-money"] } }
        })
    }

    fn sanitized(doc: Value) -> Value {
        serde_json::to_value(sanitize_state(&doc).expect("a usable document")).unwrap()
    }

    fn base() -> Value {
        json!({ "version": 1, "layout": "1", "charts": [] })
    }

    fn with(extra: Value) -> Value {
        let mut doc = base();
        for (k, v) in extra.as_object().unwrap() {
            doc[k] = v.clone();
        }
        doc
    }

    // ---- state codec round-trip ----

    #[test]
    fn decode_of_encode_preserves_a_full_valid_document() {
        let state = sanitize_state(&full_doc()).unwrap();
        let decoded = decode_state(&encode_state(&state)).unwrap();
        assert_eq!(decoded, state);
        // ... and the typed document is the original JSON, field for field.
        assert_eq!(serde_json::to_value(&decoded).unwrap(), full_doc());
        assert_eq!(
            serde_json::from_str::<Value>(&encode_state(&state)).unwrap(),
            full_doc()
        );
    }

    #[test]
    fn typed_deserialization_agrees_with_the_sanitizer_on_a_valid_document() {
        let typed: WorkspaceState = serde_json::from_value(full_doc()).unwrap();
        assert_eq!(typed, sanitize_state(&full_doc()).unwrap());
    }

    #[test]
    fn whole_numbers_are_written_without_a_fraction() {
        let mut state = WorkspaceState::new("1");
        state.charts.push(ChartState {
            id: "c1".into(),
            cell: CellState {
                bars: Some(500.0),
                ..CellState::default()
            },
        });
        state.track_sizes = Some(BTreeMap::from([(
            "2h".to_owned(),
            TrackSizes {
                cols: Some(vec![1.0, 0.5]),
                rows: None,
            },
        )]));
        state.panels = Some(PanelsState {
            widths: Some(BTreeMap::from([("p".to_owned(), 320.0)])),
            ..PanelsState::default()
        });
        assert_eq!(
            encode_state(&state),
            r#"{"version":1,"layout":"1","trackSizes":{"2h":{"cols":[1,0.5]}},"panels":{"widths":{"p":320}},"charts":[{"id":"c1","bars":500}]}"#
        );
    }

    #[test]
    fn a_picker_layout_id_survives_the_round_trip() {
        let mut state = sanitize_state(&full_doc()).unwrap();
        state.layout = "g3x3".into();
        assert_eq!(decode_state(&encode_state(&state)).unwrap().layout, "g3x3");
    }

    #[test]
    fn unusable_payloads_decode_to_none() {
        assert_eq!(decode_state("not json {"), None);
        assert_eq!(decode_state("\"a string\""), None);
        assert_eq!(decode_state("42"), None);
        assert_eq!(decode_state("null"), None);
        assert_eq!(decode_state(""), None);
        assert_eq!(decode_state("[]"), None);
        assert_eq!(
            decode_state(r#"{"version":2,"layout":"4","charts":[]}"#),
            None
        ); // future version
        assert_eq!(decode_state(r#"{"version":1,"charts":[]}"#), None); // no layout id
        assert_eq!(
            decode_state(r#"{"version":"1","layout":"1","charts":[]}"#),
            None
        );
        assert_eq!(
            decode_state(r#"{"version":1,"layout":1,"charts":[]}"#),
            None
        );
        assert_eq!(decode_state(r#"{"layout":"1"}"#), None);
    }

    #[test]
    fn version_one_may_be_written_with_a_fraction() {
        assert!(decode_state(r#"{"version":1.0,"layout":"1"}"#).is_some());
        assert!(decode_state(r#"{"version":1e0,"layout":"1"}"#).is_some());
    }

    #[test]
    fn missing_charts_default_to_empty() {
        let doc = sanitize_state(&json!({ "version": 1, "layout": "1" })).unwrap();
        assert!(doc.charts.is_empty());
        let doc =
            sanitize_state(&json!({ "version": 1, "layout": "1", "charts": "nope" })).unwrap();
        assert!(doc.charts.is_empty());
    }

    // ---- sanitize_state ----

    #[test]
    fn it_drops_malformed_fields_but_keeps_the_healthy_remainder() {
        let doc = json!({
            "version": 1,
            "layout": "2h",
            "activeCellId": 7,
            "timezone": "",
            "sync": { "viewport": "yes", "crosshair": true },
            "trackSizes": { "2h": { "cols": [1, -1] }, "4": { "cols": [2, 1] } },
            "charts": [
                { "id": "c1", "symbol": "BTCUSDT", "bars": -5, "rendererConfig": "oops" },
                null,
                { "symbol": "GHOST" },
                { "id": "c3", "indicators": { "manifest": ["EMA", 42], "natives": "volume" } },
                {
                    "id": "c4",
                    "indicators": {
                        "manifest": [
                            { "name": "RSI", "inputs": { "len": 21 }, "props": "oops" },
                            { "name": "MACD", "inputs": [1, 2] },
                            { "inputs": { "x": 1 } }
                        ],
                        "natives": []
                    }
                }
            ]
        });
        assert_eq!(
            sanitized(doc),
            json!({
                "version": 1,
                "layout": "2h",
                "sync": { "crosshair": true },
                "trackSizes": { "4": { "cols": [2, 1] } },
                "charts": [
                    { "id": "c1", "symbol": "BTCUSDT" },
                    { "id": "c3", "indicators": { "manifest": ["EMA"], "natives": [] } },
                    { "id": "c4", "indicators": { "manifest": [{ "name": "RSI", "inputs": { "len": 21 } }, "MACD"], "natives": [] } }
                ]
            })
        );
    }

    #[test]
    fn native_entries_carry_input_deltas_and_the_hidden_flag_and_manifest_entries_carry_hidden() {
        let doc = json!({
            "version": 1,
            "layout": "1",
            "charts": [{
                "id": "c1",
                "indicators": {
                    "natives": [
                        "volume",
                        { "type": "aroon", "inputs": { "length": 50 } },
                        { "type": "vpvr", "hidden": true },
                        { "type": "sma", "inputs": [9], "hidden": false },
                        { "inputs": { "length": 3 } }
                    ],
                    "manifest": [{ "name": "RSI", "hidden": true }, { "name": "MACD", "hidden": "yes" }]
                }
            }]
        });
        assert_eq!(
            sanitized(doc),
            json!({
                "version": 1,
                "layout": "1",
                "charts": [{
                    "id": "c1",
                    "indicators": {
                        "natives": ["volume", { "type": "aroon", "inputs": { "length": 50 } }, { "type": "vpvr", "hidden": true }, "sma"],
                        "manifest": [{ "name": "RSI", "hidden": true }, "MACD"]
                    }
                }]
            })
        );
    }

    #[test]
    fn an_empty_value_bag_is_kept_unlike_an_absent_one() {
        let doc = with(json!({ "charts": [{ "id": "c1", "indicators": {
            "manifest": [{ "name": "A", "inputs": {} }],
            "natives": [{ "type": "b", "inputs": {} }]
        } }] }));
        assert_eq!(
            sanitized(doc)["charts"][0]["indicators"],
            json!({ "manifest": [{ "name": "A", "inputs": {} }], "natives": [{ "type": "b", "inputs": {} }] })
        );
    }

    #[test]
    fn a_saved_linear_regression_curve_restores_as_an_lsma_and_its_slope_mode_as_a_slope_study() {
        let doc = with(json!({ "charts": [{ "id": "c1", "indicators": {
            "natives": [
                "linear-regression",
                { "type": "linear-regression", "inputs": { "length": 50, "lineColor": "#ffffff", "source": "HL2" }, "hidden": true },
                { "type": "linear-regression", "inputs": { "mode": "Slope" } },
                { "type": "linear-regression", "inputs": { "mode": "Slope", "length": 20, "lineColor": "#ffffff" } }
            ],
            "manifest": []
        } }] }));
        // The curve kept its 100-bar default when saved, which LSMA's own default no longer is.
        assert_eq!(
            sanitized(doc)["charts"][0]["indicators"]["natives"],
            json!([
                { "type": "lsma", "inputs": { "length": 100 } },
                { "type": "lsma", "inputs": { "length": 50, "neutralColor": "#ffffff", "source": "HL2" }, "hidden": true },
                "linear-regression-slope",
                { "type": "linear-regression-slope", "inputs": { "length": 20, "lineColor": "#ffffff" } }
            ])
        );
    }

    #[test]
    fn linear_regression_successor_edge_cases() {
        let natives = |entries: Value| {
            sanitized(with(json!({ "charts": [{ "id": "c", "indicators": { "natives": entries, "manifest": [] } }] })))["charts"][0]
                ["indicators"]["natives"]
                .clone()
        };
        // A saved length wins over the curve's 100-bar fallback.
        assert_eq!(
            natives(json!([{ "type": "linear-regression", "inputs": { "length": 10 } }])),
            json!([{ "type": "lsma", "inputs": { "length": 10 } }])
        );
        // A hidden slope with no inputs keeps only the flag.
        assert_eq!(
            natives(
                json!([{ "type": "linear-regression", "inputs": { "mode": "Slope" }, "hidden": true }])
            ),
            json!([{ "type": "linear-regression-slope", "hidden": true }])
        );
        // A different mode string is the curve; the mode itself is dropped.
        assert_eq!(
            natives(json!([{ "type": "linear-regression", "inputs": { "mode": "Curve" } }])),
            json!([{ "type": "lsma", "inputs": { "length": 100 } }])
        );
        // lineColor wins over a saved neutralColor; a null lineColor still counts as present.
        assert_eq!(
            natives(
                json!([{ "type": "linear-regression", "inputs": { "neutralColor": "#111", "lineColor": "#222" } }])
            ),
            json!([{ "type": "lsma", "inputs": { "length": 100, "neutralColor": "#222" } }])
        );
        assert_eq!(
            natives(json!([{ "type": "linear-regression", "inputs": { "lineColor": null } }])),
            json!([{ "type": "lsma", "inputs": { "length": 100, "neutralColor": null } }])
        );
        // Other types are untouched.
        assert_eq!(
            natives(json!(["linear-regression-slope", "lsma"])),
            json!(["linear-regression-slope", "lsma"])
        );
    }

    #[test]
    fn it_keeps_a_valid_session_value_and_drops_anything_else() {
        let doc = with(json!({ "charts": [
            { "id": "c1", "symbol": "NASDAQ:AAPL", "session": "extended" },
            { "id": "c2", "symbol": "BTCUSDT", "session": "after-hours" }
        ] }));
        assert_eq!(
            sanitized(doc)["charts"],
            json!([{ "id": "c1", "symbol": "NASDAQ:AAPL", "session": "extended" }, { "id": "c2", "symbol": "BTCUSDT" }])
        );
        let regular = sanitized(with(
            json!({ "charts": [{ "id": "c", "session": "regular" }] }),
        ));
        assert_eq!(regular["charts"][0]["session"], "regular");
    }

    #[test]
    fn favorites_are_filtered_by_type_and_empty_sets_disappear() {
        let doc =
            with(json!({ "favorites": ["trendline", 7], "timeframeFavorites": ["60", null, "D"] }));
        let out = sanitized(doc);
        assert_eq!(out["favorites"], json!(["trendline"]));
        assert_eq!(out["timeframeFavorites"], json!(["60", "D"]));
        let empty = sanitized(with(
            json!({ "timeframeFavorites": [42], "favorites": "x" }),
        ));
        assert!(empty.get("timeframeFavorites").is_none());
        assert!(empty.get("favorites").is_none());
        assert!(
            sanitized(with(json!({ "favorites": [1, 2] })))
                .get("favorites")
                .is_none()
        );
    }

    #[test]
    fn chart_entries_dedupe_by_id_and_the_last_duplicate_wins() {
        let doc = with(json!({ "layout": "2h", "charts": [
            { "id": "c1", "symbol": "OLD" },
            { "id": "c2", "symbol": "ETHUSDT" },
            { "id": "c1", "symbol": "NEW" }
        ] }));
        assert_eq!(
            sanitized(doc)["charts"],
            json!([{ "id": "c1", "symbol": "NEW" }, { "id": "c2", "symbol": "ETHUSDT" }])
        );
    }

    #[test]
    fn chart_entries_need_a_non_empty_string_id_and_an_object_shape() {
        let doc = with(json!({ "charts": [
            { "id": "" }, { "id": 3 }, "c1", 7, [1, 2], { "id": "ok" }
        ] }));
        assert_eq!(sanitized(doc)["charts"], json!([{ "id": "ok" }]));
    }

    #[test]
    fn ext_bags_pass_through_opaquely() {
        let doc = json!({
            "version": 1,
            "layout": "1",
            "ext": { "vendor.prefs": { "any": ["shape", 42] }, "unknown.plugin": "a bare string payload", "": "dropped (empty key)", "nothing": null },
            "charts": [
                { "id": "c1", "ext": { "vendor.indicators": [1, 2, 3] } },
                { "id": "c2", "ext": ["not", "an", "object"] },
                { "id": "c3", "ext": "nope" }
            ]
        });
        let out = sanitized(doc);
        assert_eq!(
            out["ext"],
            json!({ "vendor.prefs": { "any": ["shape", 42] }, "unknown.plugin": "a bare string payload", "nothing": null })
        );
        assert_eq!(
            out["charts"][0]["ext"],
            json!({ "vendor.indicators": [1, 2, 3] })
        );
        assert!(out["charts"][1].get("ext").is_none());
        assert!(out["charts"][2].get("ext").is_none());
        // an emptied bag disappears entirely rather than persisting `{}`
        assert!(sanitized(with(json!({ "ext": {} }))).get("ext").is_none());
        assert!(
            sanitized(with(json!({ "ext": { "": 1 } })))
                .get("ext")
                .is_none()
        );
    }

    #[test]
    fn renderer_config_and_drawings_pass_through_opaquely() {
        let config = json!({ "anything": { "the": ["renderer", "owns"] } });
        let out = sanitized(with(
            json!({ "charts": [{ "id": "c1", "rendererConfig": config, "drawings": config }] }),
        ));
        assert_eq!(out["charts"][0]["rendererConfig"], config);
        assert_eq!(out["charts"][0]["drawings"], config);
        // The codec only requires object-ness (arrays count, as in JavaScript), so primitives cannot masquerade.
        let out = sanitized(with(
            json!({ "charts": [{ "id": "c1", "rendererConfig": [1], "drawings": 5 }] }),
        ));
        assert_eq!(out["charts"][0]["rendererConfig"], json!([1]));
        assert!(out["charts"][0].get("drawings").is_none());
        let out = sanitized(with(
            json!({ "charts": [{ "id": "c1", "rendererConfig": null, "drawings": true }] }),
        ));
        assert!(out["charts"][0].get("rendererConfig").is_none());
        assert!(out["charts"][0].get("drawings").is_none());
    }

    #[test]
    fn sync_group_maps_are_kept_only_with_at_least_one_valid_member() {
        let doc = with(
            json!({ "layout": "4", "sync": { "symbol": { "c1": "a", "c2": 9 }, "timeframe": { "c1": 3 } } }),
        );
        assert_eq!(sanitized(doc)["sync"], json!({ "symbol": { "c1": "a" } }));
    }

    #[test]
    fn sync_settings_only_keep_true_or_group_maps() {
        let doc = with(json!({ "sync": {
            "viewport": false, "symbol": "yes", "timeframe": null, "crosshair": 1,
            "drawings": true, "style": { "c1": "g" }, "unknown": true
        } }));
        assert_eq!(
            sanitized(doc)["sync"],
            json!({ "drawings": true, "style": { "c1": "g" } })
        );
        assert!(
            sanitized(with(json!({ "sync": { "viewport": false } })))
                .get("sync")
                .is_none()
        );
        assert!(
            sanitized(with(json!({ "sync": "x" })))
                .get("sync")
                .is_none()
        );
        // An array reads as an object keyed by index, like `Object.entries`.
        assert_eq!(
            sanitized(with(json!({ "sync": { "symbol": ["a", 1, "b"] } })))["sync"],
            json!({ "symbol": { "0": "a", "2": "b" } })
        );
    }

    #[test]
    fn drawing_tool_preferences_drop_a_bad_magnet_a_non_boolean_stay_and_non_object_entries() {
        let doc = with(json!({ "drawingTools": {
            "defaults": { "trendline": { "style": { "lineColor": "#f00" } }, "box": "junk", "hline": [1], "": {}, "nul": null },
            "magnet": "sticky",
            "stay": "yes"
        } }));
        assert_eq!(
            sanitized(doc)["drawingTools"],
            json!({ "defaults": { "trendline": { "style": { "lineColor": "#f00" } } } })
        );
        assert!(
            sanitized(with(json!({ "drawingTools": { "magnet": 1 } })))
                .get("drawingTools")
                .is_none()
        );
        assert!(
            sanitized(with(json!({ "drawingTools": "junk" })))
                .get("drawingTools")
                .is_none()
        );
        assert!(
            sanitized(with(json!({ "drawingTools": [1] })))
                .get("drawingTools")
                .is_none()
        );
    }

    #[test]
    fn drawing_tool_templates_keep_non_empty_lists_and_magnet_modes() {
        let doc = with(json!({ "drawingTools": {
            "templates": { "trendline": [{ "name": "a", "settings": {} }, 7], "box": [], "hline": "x", "": [1] },
            "magnet": "strong",
            "stay": false
        } }));
        assert_eq!(
            sanitized(doc)["drawingTools"],
            json!({ "templates": { "trendline": [{ "name": "a", "settings": {} }, 7] }, "magnet": "strong", "stay": false })
        );
        for (text, magnet) in [
            ("off", Magnet::Off),
            ("weak", Magnet::Weak),
            ("strong", Magnet::Strong),
        ] {
            assert_eq!(magnet.as_str(), text);
            let doc = with(json!({ "drawingTools": { "magnet": text } }));
            assert_eq!(
                sanitize_state(&doc).unwrap().drawing_tools.unwrap().magnet,
                Some(magnet)
            );
        }
    }

    #[test]
    fn status_line_parts_keep_known_boolean_parts_only() {
        let doc = with(json!({ "charts": [
            { "id": "c1", "statuslineParts": { "ohlc": false, "change": false, "market": "no", "volume": false } },
            { "id": "c2", "statuslineParts": { "sparkline": true } },
            { "id": "c3", "statuslineParts": "off" },
            { "id": "c4", "statuslineParts": [true] }
        ] }));
        assert_eq!(
            sanitized(doc)["charts"],
            json!([
                { "id": "c1", "statuslineParts": { "ohlc": false, "change": false } },
                { "id": "c2" },
                { "id": "c3" },
                { "id": "c4" }
            ])
        );
    }

    #[test]
    fn favorites_and_display_toggles_are_filtered_by_type() {
        let doc = with(json!({
            "favorites": ["trendline", 7, null, "hline"],
            "charts": [
                { "id": "c1", "watermark": "yes", "replayWatermark": 1, "indicatorTitles": 0, "indicatorValues": "no" },
                { "id": "c2", "watermark": false, "replayWatermark": false, "indicatorTitles": false, "indicatorValues": true }
            ]
        }));
        let out = sanitized(doc);
        assert_eq!(out["favorites"], json!(["trendline", "hline"]));
        assert_eq!(out["charts"][0], json!({ "id": "c1" }));
        assert_eq!(
            out["charts"][1],
            json!({ "id": "c2", "watermark": false, "replayWatermark": false, "indicatorTitles": false, "indicatorValues": true })
        );
    }

    #[test]
    fn cell_scalars_are_type_checked() {
        let doc = with(json!({ "charts": [
            { "id": "a", "symbol": "", "provider": 3, "timeframe": null, "priceStyle": ["x"], "bars": 0 },
            { "id": "b", "symbol": "X", "bars": "5" },
            { "id": "c", "bars": null },
            { "id": "d", "bars": 12.5 }
        ] }));
        assert_eq!(
            sanitized(doc)["charts"],
            json!([{ "id": "a", "symbol": "" }, { "id": "b", "symbol": "X" }, { "id": "c" }, { "id": "d", "bars": 12.5 }])
        );
    }

    #[test]
    fn an_out_of_range_number_makes_the_payload_unusable() {
        // JSON.parse reads 1e400 as Infinity and the field is then dropped; serde_json refuses the text.
        assert_eq!(
            decode_state(r#"{"version":1,"layout":"1","charts":[{"id":"c","bars":1e400}]}"#),
            None
        );
    }

    #[test]
    fn active_cell_and_timezone() {
        let out = sanitized(with(json!({ "activeCellId": "", "timezone": "UTC" })));
        assert_eq!(out["activeCellId"], "");
        assert_eq!(out["timezone"], "UTC");
        assert!(
            sanitized(with(json!({ "timezone": 3 })))
                .get("timezone")
                .is_none()
        );
    }

    #[test]
    fn a_cell_with_a_non_object_indicators_field_has_none() {
        let out = sanitized(with(json!({ "charts": [
            { "id": "a", "indicators": "x" }, { "id": "b", "indicators": null }, { "id": "c", "indicators": [] }, { "id": "d", "indicators": {} }
        ] })));
        assert_eq!(out["charts"][0], json!({ "id": "a" }));
        assert_eq!(out["charts"][1], json!({ "id": "b" }));
        assert_eq!(
            out["charts"][2]["indicators"],
            json!({ "manifest": [], "natives": [] })
        );
        assert_eq!(
            out["charts"][3]["indicators"],
            json!({ "manifest": [], "natives": [] })
        );
    }

    #[test]
    fn track_sizes_need_a_whole_positive_axis() {
        let doc = with(json!({ "trackSizes": {
            "a": { "cols": [1, 2], "rows": [0, 1] },
            "b": { "cols": [] },
            "c": { "cols": [1, "2"] },
            "d": "junk",
            "e": [1],
            "f": { "rows": [0.5] }
        } }));
        assert_eq!(
            sanitized(doc)["trackSizes"],
            json!({ "a": { "cols": [1, 2] }, "f": { "rows": [0.5] } })
        );
        assert!(
            sanitized(with(json!({ "trackSizes": "x" })))
                .get("trackSizes")
                .is_none()
        );
        assert!(
            sanitized(with(json!({ "trackSizes": {} })))
                .get("trackSizes")
                .is_none()
        );
    }

    // ---- panels (side-panel-dock.test.ts) ----

    #[test]
    fn it_keeps_a_usable_dock_state() {
        let out = sanitized(with(
            json!({ "panels": { "open": "objects", "widths": { "objects": 320 }, "pinned": ["editor"] } }),
        ));
        assert_eq!(
            out["panels"],
            json!({ "open": "objects", "widths": { "objects": 320 }, "pinned": ["editor"] })
        );
    }

    #[test]
    fn dock_state_drops_malformed_fields_rather_than_failing() {
        let panels = |p: Value| {
            sanitized(with(json!({ "panels": p })))
                .get("panels")
                .cloned()
        };
        assert_eq!(
            panels(json!({ "open": 42, "widths": { "a": "wide", "b": -5, "c": 0, "d": 260 } })),
            Some(json!({ "widths": { "d": 260 } }))
        );
        assert_eq!(
            panels(json!({ "pinned": ["x", 7, "", "x", null] })),
            Some(json!({ "pinned": ["x"] }))
        );
        assert_eq!(panels(json!({ "pinned": "x" })), None);
        assert_eq!(panels(json!("nope")), None);
        assert_eq!(panels(json!({ "open": "", "widths": {} })), None);
        assert_eq!(panels(json!([1])), None);
        // widths as an array reads keyed by index, like `Object.entries`.
        assert_eq!(
            panels(json!({ "widths": [100, -1, 50] })),
            Some(json!({ "widths": { "0": 100, "2": 50 } }))
        );
    }

    #[test]
    fn a_document_written_before_the_dock_has_no_panels_field() {
        assert_eq!(sanitize_state(&base()).unwrap().panels, None);
    }

    // ---- prefixed_symbol ----

    fn cell(symbol: Option<&str>, provider: Option<&str>) -> CellState {
        CellState {
            symbol: symbol.map(str::to_owned),
            provider: provider.map(str::to_owned),
            ..CellState::default()
        }
    }

    #[test]
    fn a_bare_symbol_is_welded_to_its_provider() {
        assert_eq!(
            prefixed_symbol(Some(&cell(Some("BTCUSDT"), Some("binance")))).as_deref(),
            Some("binance:BTCUSDT")
        );
    }

    #[test]
    fn a_symbol_that_already_carries_a_prefix_wins() {
        assert_eq!(
            prefixed_symbol(Some(&cell(Some("NASDAQ:AAPL"), Some("alpaca")))).as_deref(),
            Some("NASDAQ:AAPL")
        );
    }

    #[test]
    fn prefixed_symbol_without_a_provider_or_symbol() {
        assert_eq!(
            prefixed_symbol(Some(&cell(Some("AAPL"), None))).as_deref(),
            Some("AAPL")
        );
        assert_eq!(
            prefixed_symbol(Some(&cell(Some("AAPL"), Some("")))).as_deref(),
            Some("AAPL")
        );
        assert_eq!(
            prefixed_symbol(Some(&cell(Some(""), Some("binance")))),
            None
        );
        assert_eq!(prefixed_symbol(Some(&cell(None, Some("binance")))), None);
        assert_eq!(prefixed_symbol(None), None);
    }

    // ---- misc ----

    #[test]
    fn sync_kinds_list_matches_the_original() {
        let names: Vec<&str> = SyncKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "viewport",
                "symbol",
                "timeframe",
                "crosshair",
                "drawings",
                "style"
            ]
        );
        let mut options = SyncOptions::default();
        options.set(SyncKind::Style, Some(SyncSetting::Flag(true)));
        assert_eq!(options.get(SyncKind::Style), Some(&SyncSetting::Flag(true)));
        assert_eq!(options.get(SyncKind::Viewport), None);
    }

    #[test]
    fn deeply_nested_hostile_input_does_not_panic() {
        let deep = format!("{}1{}", "[".repeat(400), "]".repeat(400));
        assert_eq!(decode_state(&deep), None);
        let doc =
            format!(r#"{{"version":1,"layout":"1","charts":[{{"id":"c","ext":{{"k":{deep}}}}}]}}"#);
        // Past serde_json's recursion limit the whole payload is unusable, never a panic.
        assert_eq!(decode_state(&doc), None);
    }
}
