//! The workspace SYNC model: pure data and pure functions (`sync.ts`).
//!
//! Links between cells are GROUP-based from day one: `true` is sugar for "every cell in
//! one implicit group", and a record maps cell ids to named groups (a colored-group UI is
//! pure presentation over this same model). Propagation itself lives in the workspace (it
//! owns the cells); this module answers the one pure question, "which cells follow this
//! origin?", plus the epsilon test that keeps viewport loops quiet, the drawings-link
//! pairing and the style-link patch.
//!
//! The link types ([`SyncKind`], [`SyncSetting`], [`SyncOptions`]) live in the shared
//! state document, which persists them; they are re-exported here unchanged.
//!
//! # Differences from the original
//!
//! - [`SyncSetting::Flag`] covers both `true` and `false`; an absent setting is `None`.
//! - `styleConfigPatch` works on `serde_json` documents; only JSON OBJECTS count as
//!   records (the original also lets an array through `typeof === "object"`, which no
//!   renderer config produces).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub use crate::state::{SyncKind, SyncOptions, SyncSetting};

use crate::drawings::SerializedDrawing;
use crate::render::core::chart_config::CANDLE_OVERRIDE_KEYS;

/// The cells that follow `origin_id` under `setting`. Never includes the origin.
pub fn sync_targets(
    origin_id: &str,
    setting: Option<&SyncSetting>,
    cell_ids: &[String],
) -> Vec<String> {
    match setting {
        None | Some(SyncSetting::Flag(false)) => Vec::new(),
        Some(SyncSetting::Flag(true)) => cell_ids
            .iter()
            .filter(|id| id.as_str() != origin_id)
            .cloned()
            .collect(),
        Some(SyncSetting::Groups(groups)) => {
            // The origin is unlinked: nothing follows it.
            let Some(group) = groups.get(origin_id) else {
                return Vec::new();
            };
            cell_ids
                .iter()
                .filter(|id| id.as_str() != origin_id && groups.get(id.as_str()) == Some(group))
                .cloned()
                .collect()
        }
    }
}

/// The cells sharing a link with at least one other cell under `setting`, as groups in
/// `cell_ids` order. `true` is one group of every cell; a record yields one group per
/// name; unlinked and lone cells drop out.
pub fn sync_groups(setting: Option<&SyncSetting>, cell_ids: &[String]) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for id in cell_ids {
        if seen.contains(id) {
            continue;
        }
        let peers = sync_targets(id, setting, cell_ids);
        if peers.is_empty() {
            continue;
        }
        let mut group = vec![id.clone()];
        group.extend(peers);
        seen.extend(group.iter().cloned());
        groups.push(group);
    }
    groups
}

/// The content a drawings-link copy shares with its original: exactly what a linked copy
/// is created with and what a linked edit pushes (type, pane, anchors, style, text,
/// per-type props). Lock, visibility, draw order and timeframe limits stay per chart, so
/// they are not part of it. Canonical (object keys sorted, absent = omitted), so the same
/// drawing yields the same key whatever order its document lists the keys in.
pub fn drawing_link_content(d: &SerializedDrawing) -> String {
    let mut content = Map::new();
    content.insert("type".into(), Value::String(d.type_key.clone()));
    content.insert("paneId".into(), Value::String(d.pane_id.clone()));
    content.insert(
        "anchors".into(),
        serde_json::to_value(&d.anchors).unwrap_or(Value::Null),
    );
    content.insert(
        "style".into(),
        serde_json::to_value(&d.style).unwrap_or(Value::Null),
    );
    if let Some(text) = &d.text {
        content.insert(
            "text".into(),
            serde_json::to_value(text).unwrap_or(Value::Null),
        );
    }
    if let Some(props) = &d.props {
        content.insert(
            "props".into(),
            serde_json::to_value(props).unwrap_or(Value::Null),
        );
    }
    canonical_json(&Value::Object(content))
}

/// JSON with object keys sorted at every depth.
fn canonical_json(v: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = Map::new();
                for k in keys {
                    out.insert(k.clone(), sorted(&map[k]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sorted(v)).unwrap_or_default()
}

/// The drawing ids of one content, per cell, in each cell's order.
type PerCell = Vec<(String, Vec<String>)>;

/// One chart's drawings, for [`pair_identical_drawings`].
#[derive(Clone, Debug)]
pub struct CellDrawings<'a> {
    pub cell_id: String,
    pub drawings: &'a [SerializedDrawing],
}

/// Pair the drawings that are identical copies of each other across one link group's
/// cells. Two drawings pair when their [`drawing_link_content`] matches; equal duplicates
/// on the same chart pair one to one, in the order each chart lists them (its paint
/// order), so three identical lines facing two pair as two links and one loner. Returns
/// each link as cell id to drawing id (in cell order), two members or more.
pub fn pair_identical_drawings(cells: &[CellDrawings<'_>]) -> Vec<Vec<(String, String)>> {
    // content -> (cell order of first appearance, per-cell drawing ids in order)
    let mut by_content: Vec<(String, PerCell)> = Vec::new();
    for CellDrawings { cell_id, drawings } in cells {
        for d in *drawings {
            let content = drawing_link_content(d);
            let slot = match by_content.iter().position(|(c, _)| *c == content) {
                Some(i) => i,
                None => {
                    by_content.push((content, Vec::new()));
                    by_content.len() - 1
                }
            };
            let per_cell = &mut by_content[slot].1;
            match per_cell.iter_mut().find(|(c, _)| c == cell_id) {
                Some((_, ids)) => ids.push(d.id.clone()),
                None => per_cell.push((cell_id.clone(), vec![d.id.clone()])),
            }
        }
    }
    let mut links = Vec::new();
    for (_, per_cell) in by_content {
        if per_cell.len() < 2 {
            continue;
        }
        let depth = per_cell.iter().map(|(_, ids)| ids.len()).max().unwrap_or(0);
        for k in 0..depth {
            let link: Vec<(String, String)> = per_cell
                .iter()
                .filter_map(|(cell, ids)| ids.get(k).map(|id| (cell.clone(), id.clone())))
                .collect();
            if link.len() > 1 {
                links.push(link);
            }
        }
    }
    links
}

/// Whether two visible ranges agree within `eps_ms` on both edges: the short-circuit that
/// stops viewport echo (a followers' re-emission never re-propagates).
pub fn ranges_within(a: (f64, f64), b: (f64, f64), eps_ms: f64) -> bool {
    (a.0 - b.0).abs() <= eps_ms && (a.1 - b.1).abs() <= eps_ms
}

/// The renderer-config blocks the STYLE link mirrors whole: every look the settings
/// dialog edits: the Symbol tab's per-style cosmetics (`candles` ... `columns`) and
/// animation switches, the Canvas tab (`layout` = background/text, `panes` = separators,
/// `grid`, `margins`), the Scales-and-lines tab (`priceScale`, `crosshair`), and the
/// session shading colors. Two blocks travel in part (see [`style_config_patch`]); the
/// rest stays per cell: the draw-order keys name each cell's own indicators, timeline
/// marks and trade markers are what the host feeds that cell's market, and `timeScale` is
/// already workspace-global.
pub const STYLE_SYNC_CONFIG_KEYS: [&str; 14] = [
    "layout",
    "panes",
    "grid",
    "margins",
    "priceScale",
    "crosshair",
    "animations",
    "candles",
    "bars",
    "line",
    "area",
    "baseline",
    "columns",
    "sessions",
];

fn as_record(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// The patch that brings `follower` to `origin`'s style-link slice (`None` when the two
/// already agree, or when either document is shapeless). Whole blocks travel as the
/// origin resolved them. `series` contributes only its bar spacing: the chart type stays
/// per cell (a candles cell and a line cell keep their own type) and the baseline price
/// belongs to the cell's market. A plugin chart type contributes only the candle
/// cosmetics it stores in its bag (its own settings mix looks with market-bound
/// computation: sessions, row size), and a candle key the origin inherits travels as
/// `null`, so the follower inherits it too.
pub fn style_config_patch(origin: &Value, follower: &Value) -> Option<Map<String, Value>> {
    let from = as_record(origin)?;
    let to = as_record(follower)?;
    let mut patch = Map::new();
    for key in STYLE_SYNC_CONFIG_KEYS {
        let Some(block) = from.get(key).filter(|b| b.is_object()) else {
            continue;
        };
        if to.get(key) != Some(block) {
            patch.insert(key.to_string(), block.clone());
        }
    }
    let spacing = from
        .get("series")
        .and_then(as_record)
        .and_then(|s| s.get("spacing"))
        .filter(|s| s.is_number());
    if let Some(spacing) = spacing {
        let held = to
            .get("series")
            .and_then(as_record)
            .and_then(|s| s.get("spacing"));
        if held != Some(spacing) {
            let mut series = Map::new();
            series.insert("spacing".into(), spacing.clone());
            patch.insert("series".into(), Value::Object(series));
        }
    }
    let empty = Map::new();
    let from_types = from.get("chartTypes").and_then(as_record).unwrap_or(&empty);
    let to_types = to.get("chartTypes").and_then(as_record).unwrap_or(&empty);
    let mut type_ids: Vec<&String> = from_types.keys().chain(to_types.keys()).collect();
    type_ids.sort();
    type_ids.dedup();
    let mut types: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for type_id in type_ids {
        let src = from_types
            .get(type_id)
            .and_then(as_record)
            .unwrap_or(&empty);
        let dst = to_types.get(type_id).and_then(as_record).unwrap_or(&empty);
        for key in CANDLE_OVERRIDE_KEYS {
            let value = src.get(key).cloned().unwrap_or(Value::Null);
            let held = dst.get(key).cloned().unwrap_or(Value::Null);
            if value != held {
                types
                    .entry(type_id.clone())
                    .or_default()
                    .insert(key.to_string(), value);
            }
        }
    }
    if !types.is_empty() {
        let mut out = Map::new();
        for (id, bag) in types {
            out.insert(id, Value::Object(bag));
        }
        patch.insert("chartTypes".into(), Value::Object(out));
    }
    if patch.is_empty() { None } else { Some(patch) }
}

#[cfg(test)]
mod tests;
