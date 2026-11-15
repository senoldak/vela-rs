//! The object tree AS DATA. This half takes a snapshot of the chart (its panes, the
//! indicators in them, the editable stacking order, the drawings) and lays it out as the
//! blocks and rows the panel paints. No DOM and no chart calls, so the layout rules (what
//! sits in front of what, which pane a drawing belongs to, how a group folds) are all
//! unit-testable on plain values.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::drawings::drawing::SerializedDrawing;
use crate::drawings::registry::DrawingRegistry;
use crate::i18n::{MessageArg, MessageLike, Translator, msg, source_text, text_of};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::PaneKind;
use crate::options::{PaneIndicatorInfo, PaneInfo};

/// A user-made bundle of drawings that hide, lock, delete and move as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawGroup {
    pub id: String,
    pub name: String,
    pub ids: Vec<String>,
    /// The pane an EMPTY group sits in. A group made before it holds anything has no member
    /// to take a pane from; once drawings join, theirs is where it shows. `None`: the main
    /// chart.
    pub pane: Option<String>,
}

impl DrawGroup {
    pub fn new(id: &str, name: &str, ids: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            pane: None,
        }
    }
}

/// The candles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceRow {
    pub label: String,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndicatorRow {
    pub id: String,
    pub label: String,
    pub visible: bool,
    /// Draws against its own price scale rather than the pane's: surfaced as a tag.
    pub own_scale: bool,
}

/// One series row in a pane's stack. Study panes hold indicator rows only; the price pane
/// also holds the candles, stacked among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    Price(PriceRow),
    Indicator(IndicatorRow),
}

/// A pane's drawings as top-level units, front-most first: a lone drawing, or a group
/// block surfacing at its front-most member's slot with the members nested inside.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawUnit {
    Draw(Box<SerializedDrawing>),
    Group {
        group: DrawGroup,
        members: Vec<SerializedDrawing>,
    },
}

/// One entry in a pane's rendered stack: a drawing unit, or a series row.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeItem {
    Unit(DrawUnit),
    Row(TreeRow),
}

/// One pane's block: its stack read top to bottom as front to back.
#[derive(Clone, Debug, PartialEq)]
pub struct TreePane {
    pub id: String,
    pub kind: PaneKind,
    pub label: String,
    pub order: usize,
    pub collapsed: bool,
    pub maximized: bool,
    /// The pane's stack, front-most first. With `interleave` the drawings and the series
    /// merge into ONE z-ordered column (a drawing can sit under the candles or between two
    /// indicators); otherwise the drawings lead (they always paint over) and the series
    /// follow.
    pub items: Vec<TreeItem>,
}

/// One entry of the renderer's `seriesOrder` feature.
#[derive(Clone, Debug, PartialEq)]
pub struct ZEntry {
    pub id: String,
    pub z: f64,
}

/// Everything the layout needs, pulled from the chart by the panel before it renders.
pub struct TreeSnapshot<'a> {
    pub panes: &'a [PaneInfo],
    /// The shell's translator (pane names). `None`: English.
    pub i18n: Option<&'a dyn Translator>,
    /// The drawing type registry (labels of the drawing rows).
    pub registry: Option<&'a DrawingRegistry>,
    /// Live visibility per indicator id: the pane model doesn't carry it.
    pub indicator_visible: &'a dyn Fn(&str) -> bool,
    /// A handle's title, used only as a FALLBACK: the pane model carries the resolved
    /// display title, while a handle can still report the generic placeholder for a script
    /// that names itself late.
    pub handle_title: &'a dyn Fn(&str) -> Option<String>,
    /// Whether the renderer exposes editable stacking (`seriesOrder` + `candleZOrder`).
    pub stackable: bool,
    /// Whether drawings share the series' draw-order space on this renderer
    /// (`drawingDepth`): a drawing then takes a slot anywhere in its pane's column. Without
    /// it the panel keeps every drawing in front of the series, matching how such a
    /// renderer paints.
    pub interleave: bool,
    /// `renderer.get("seriesOrder")`: ignored when `stackable` is false.
    pub z_order: &'a [ZEntry],
    /// `renderer.get("candleZOrder")`: ignored when `stackable` is false.
    pub candle_z: f64,
    pub price_label: String,
    pub price_visible: bool,
    pub drawings: &'a [SerializedDrawing],
    pub groups: &'a [DrawGroup],
}

pub const PRICE_PANE_ID: &str = "price";

fn main_chart() -> MessageLike {
    msg("widget.objectTree.mainChart", "Main chart")
}

/// The price pane's name in English (the tree shows it in the shell's language).
pub fn price_pane_label() -> String {
    source_text(&main_chart())
}

/// Upper-case the first non-space character of the string and of every word.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_start = true;
    for c in s.chars() {
        if at_start && !c.is_whitespace() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        at_start = c.is_whitespace();
    }
    out
}

/// A drawing type's display name and icon markup, from the type registry. Unregistered
/// types fall back to a title-cased key so a plugin drawing still reads as a name.
pub fn drawing_meta(
    type_key: &str,
    i18n: Option<&dyn Translator>,
    registry: Option<&DrawingRegistry>,
) -> (String, Option<String>) {
    if let Some(meta) = registry.and_then(|r| r.get(type_key)) {
        return (text_of(i18n, &meta.label, &[]), Some(meta.icon.clone()));
    }
    let name = if type_key.is_empty() {
        "drawing"
    } else {
        type_key
    };
    (title_case(name), None)
}

/// A drawing's row label: its type's name, plus its own text when it carries any.
pub fn drawing_label(
    d: &SerializedDrawing,
    i18n: Option<&dyn Translator>,
    registry: Option<&DrawingRegistry>,
) -> String {
    let (label, _) = drawing_meta(&d.type_key, i18n, registry);
    let text = d.text.as_ref().map(|t| t.value.trim()).unwrap_or_default();
    if text.is_empty() {
        return label;
    }
    text_of(
        i18n,
        &msg(
            "widget.objectTree.drawingWithText",
            "{name} \u{2014} {text}",
        ),
        &[
            ("name", MessageArg::from(label)),
            ("text", MessageArg::from(text.to_string())),
        ],
    )
}

/// A pane's display name: the price pane is the main chart, a study pane borrows the title
/// of the indicator that owns its scale (falling back to its slot number).
pub fn pane_label(
    pane: &PaneInfo,
    fallback_title: &dyn Fn(&str) -> Option<String>,
    i18n: Option<&dyn Translator>,
) -> String {
    if pane.kind == PaneKind::Price {
        return text_of(i18n, &main_chart(), &[]);
    }
    let master = pane
        .indicators
        .iter()
        .find(|i| !i.own_scale)
        .or_else(|| pane.indicators.first());
    if let Some(master) = master {
        return indicator_label(master, fallback_title);
    }
    text_of(
        i18n,
        &msg("widget.objectTree.pane", "Panel {n}"),
        &[("n", MessageArg::from((pane.order + 1).to_string()))],
    )
}

/// An indicator's full display name.
fn indicator_label(
    i: &PaneIndicatorInfo,
    fallback_title: &dyn Fn(&str) -> Option<String>,
) -> String {
    if !i.title.is_empty() {
        return i.title.clone();
    }
    fallback_title(&i.id)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| i.id.clone())
}

/// An indicator row's label: the compact name it declares (matching its legend chip), or
/// else its full name.
fn indicator_row_label(
    i: &PaneIndicatorInfo,
    fallback_title: &dyn Fn(&str) -> Option<String>,
) -> String {
    match i.shorttitle.as_deref() {
        Some(short) if !short.is_empty() => short.to_string(),
        _ => indicator_label(i, fallback_title),
    }
}

/// The pane's series rows, extracted from its stack in rendered (front-first) order.
pub fn pane_rows(pane: &TreePane) -> Vec<&TreeRow> {
    pane.items
        .iter()
        .filter_map(|it| match it {
            TreeItem::Row(row) => Some(row),
            TreeItem::Unit(_) => None,
        })
        .collect()
}

// ── the stack as tokens ────────────────────────────────────────────────────
// A reorder works on the pane's stack FLATTENED to one token per z key: the candles, each
// indicator, each drawing (a group contributes one token per member, so the bundle stays
// contiguous). Front-most first, mirroring how the panel reads.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackToken {
    Price,
    Indicator(String),
    Drawing(String),
}

impl StackToken {
    /// The token's id; the candles have none.
    pub fn id(&self) -> Option<&str> {
        match self {
            StackToken::Price => None,
            StackToken::Indicator(id) | StackToken::Drawing(id) => Some(id),
        }
    }
}

pub fn same_token(a: &StackToken, b: &StackToken) -> bool {
    a == b
}

fn unit_tokens(u: &DrawUnit) -> Vec<StackToken> {
    match u {
        DrawUnit::Draw(d) => vec![StackToken::Drawing(d.id.clone())],
        DrawUnit::Group { members, .. } => members
            .iter()
            .map(|m| StackToken::Drawing(m.id.clone()))
            .collect(),
    }
}

fn item_tokens(item: &TreeItem) -> Vec<StackToken> {
    match item {
        TreeItem::Unit(u) => unit_tokens(u),
        TreeItem::Row(TreeRow::Price(_)) => vec![StackToken::Price],
        TreeItem::Row(TreeRow::Indicator(r)) => vec![StackToken::Indicator(r.id.clone())],
    }
}

/// The pane's whole stack as tokens, front-most first.
pub fn pane_tokens(pane: &TreePane) -> Vec<StackToken> {
    pane.items.iter().flat_map(item_tokens).collect()
}

/// Where a slot between rendered items lands in the token list: a group is one item but
/// as many tokens as it has members.
pub fn token_index_of_slot(pane: &TreePane, slot: usize) -> usize {
    pane.items
        .iter()
        .take(slot.min(pane.items.len()))
        .map(|item| item_tokens(item).len())
        .sum()
}

/// Where a drop inside a group lands in the token list: at the member it points at, or
/// (past the last member) right after the group's run.
pub fn group_token_index(pane: &TreePane, group_id: &str, member_slot: usize) -> usize {
    let mut at = 0;
    for item in &pane.items {
        if let TreeItem::Unit(DrawUnit::Group { group, members }) = item
            && group.id == group_id
        {
            return at + member_slot.min(members.len());
        }
        at += item_tokens(item).len();
    }
    at
}

/// Place `dragged` (as one contiguous run) at token index `at`, measured against the stack
/// AS RENDERED: lifting the dragged tokens out first shifts everything below them up, so
/// dropping a row just beneath itself means "stay put", not "move down one". Tokens arriving
/// from another pane aren't in the list yet and are simply inserted. `at` may be negative
/// or past the end: it clamps.
pub fn place_tokens(tokens: &[StackToken], dragged: &[StackToken], at: i64) -> Vec<StackToken> {
    let is_dragged = |t: &StackToken| dragged.iter().any(|d| same_token(d, t));
    let mut adjusted = at;
    for (i, t) in tokens.iter().enumerate() {
        if (i as i64) < at && is_dragged(t) {
            adjusted -= 1;
        }
    }
    let mut rest: Vec<StackToken> = tokens.iter().filter(|t| !is_dragged(t)).cloned().collect();
    let index = adjusted.clamp(0, rest.len() as i64) as usize;
    for (offset, token) in dragged.iter().enumerate() {
        rest.insert(index + offset, token.clone());
    }
    rest
}

pub fn tokens_equal(a: &[StackToken], b: &[StackToken]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_token(x, y))
}

/// The z keys [`stack_writes`] derives.
#[derive(Clone, Debug, PartialEq)]
pub struct StackWrites {
    /// `None` when the candles aren't in the stack (a study pane).
    pub candle_z: Option<f64>,
    /// Feeds `seriesOrder`.
    pub series: Vec<ZEntry>,
    /// Feeds `zIndex` patches.
    pub drawings: Vec<ZEntry>,
}

/// z keys for a FRONT-FIRST token stack: the front-most token takes the highest key, so
/// one write renormalizes the whole pane into contiguous, tie-free values.
pub fn stack_writes(tokens: &[StackToken]) -> StackWrites {
    let n = tokens.len();
    let mut out = StackWrites {
        candle_z: None,
        series: Vec::new(),
        drawings: Vec::new(),
    };
    for (i, t) in tokens.iter().enumerate() {
        let z = (n - i) as f64;
        match t {
            StackToken::Price => out.candle_z = Some(z),
            StackToken::Indicator(id) => out.series.push(ZEntry { id: id.clone(), z }),
            StackToken::Drawing(id) => out.drawings.push(ZEntry { id: id.clone(), z }),
        }
    }
    out
}

/// The extremes of a pane's stacking: what "bring to front" and "send to back" have to
/// beat. `extra` folds in the drawings' z keys when they share the space. Zero is always
/// in range so an empty stack still has bounds. Returns `(top, bottom)`.
pub fn z_stack_bounds(z_order: &[ZEntry], candle_z: f64, extra: &[f64]) -> (f64, f64) {
    let zs = z_order.iter().map(|e| e.z).chain(extra.iter().copied());
    let (mut top, mut bottom) = (js_max(candle_z, 0.0), js_min(candle_z, 0.0));
    for z in zs {
        top = js_max(top, z);
        bottom = js_min(bottom, z);
    }
    (top, bottom)
}

/// A pane's drawings, FRONT-MOST FIRST. Orphans (drawings left behind by a pane that no
/// longer exists) fold into the price pane so they stay reachable rather than vanishing.
pub fn pane_drawings(
    drawings: &[SerializedDrawing],
    pane_id: &str,
    pane_ids: &HashSet<String>,
) -> Vec<SerializedDrawing> {
    let mut mine: Vec<SerializedDrawing> = drawings
        .iter()
        .filter(|d| {
            if pane_id == PRICE_PANE_ID {
                d.pane_id == PRICE_PANE_ID || !pane_ids.contains(&d.pane_id)
            } else {
                d.pane_id == pane_id
            }
        })
        .cloned()
        .collect();
    mine.reverse(); // the store lists paint order (back to front); the tree leads with the front
    mine
}

/// The group a drawing belongs to, or `None`.
pub fn group_of<'a>(groups: &'a [DrawGroup], drawing_id: &str) -> Option<&'a DrawGroup> {
    groups
        .iter()
        .find(|g| g.ids.iter().any(|i| i == drawing_id))
}

/// A group's aggregate state: what its own eye and padlock report. Both read as "on"
/// only when EVERY member agrees: one hidden member must not make the whole bundle look
/// hidden, or the group's eye would offer to show what is already showing.
/// Returns `(all_hidden, all_locked)`.
pub fn group_state(members: &[SerializedDrawing]) -> (bool, bool) {
    (
        !members.is_empty() && members.iter().all(|d| !d.visible),
        !members.is_empty() && members.iter().all(|d| d.locked),
    )
}

/// Whether a selection can form a fresh group: something is selected, and none of it is
/// already grouped (a drawing belongs to at most one group).
pub fn can_group(ids: &[String], groups: &[DrawGroup]) -> bool {
    !ids.is_empty() && ids.iter().all(|id| group_of(groups, id).is_none())
}

/// The next default group name: the lowest `Group N` no existing group has taken. The name
/// is the user's from then on (it persists, and can be renamed), so it is written once, in
/// the shell's language at the time.
pub fn next_group_name(groups: &[DrawGroup], i18n: Option<&dyn Translator>) -> String {
    let taken: HashSet<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    let name_of = |n: usize| {
        text_of(
            i18n,
            &msg("widget.objectTree.group", "Group {n}"),
            &[("n", MessageArg::from(n.to_string()))],
        )
    };
    // One of the first n+1 candidates is always free, since n groups can occupy at most n.
    for n in 1..=groups.len() {
        let name = name_of(n);
        if !taken.contains(name.as_str()) {
            return name;
        }
    }
    name_of(groups.len() + 1)
}

/// Whether a group survives a membership change. One the change emptied disappears rather
/// than lingering as an empty row; one that was empty all along is a fresh group still
/// waiting for its first drawing, and stays.
fn survives(before: &DrawGroup, after: &DrawGroup) -> bool {
    !after.ids.is_empty() || before.ids.is_empty()
}

/// Rebuild a group list with only the members that pass `keep`.
fn with_members(groups: &[DrawGroup], keep: impl Fn(&str) -> bool) -> Vec<DrawGroup> {
    groups
        .iter()
        .filter_map(|g| {
            let next = DrawGroup {
                ids: g.ids.iter().filter(|id| keep(id)).cloned().collect(),
                ..g.clone()
            };
            survives(g, &next).then_some(next)
        })
        .collect()
}

/// Drop members that no longer exist, and the groups left empty by that.
pub fn prune_groups(groups: &[DrawGroup], live: &HashSet<String>) -> Vec<DrawGroup> {
    with_members(groups, |id| live.contains(id))
}

/// Take drawings out of whatever group they were in.
pub fn remove_from_groups(groups: &[DrawGroup], ids: &[String]) -> Vec<DrawGroup> {
    with_members(groups, |id| !ids.iter().any(|i| i == id))
}

/// Put drawings into one group, taking them out of any other on the way: membership is
/// exclusive, so a drawing can never show up under two groups at once.
pub fn assign_to_group(groups: &[DrawGroup], group_id: &str, ids: &[String]) -> Vec<DrawGroup> {
    groups
        .iter()
        .filter_map(|g| {
            let mut kept: Vec<String> = g
                .ids
                .iter()
                .filter(|id| !ids.contains(id))
                .cloned()
                .collect();
            if g.id == group_id {
                kept.extend(ids.iter().cloned());
            }
            let next = DrawGroup {
                ids: kept,
                ..g.clone()
            };
            survives(g, &next).then_some(next)
        })
        .collect()
}

/// The groups still waiting for their first drawing that sit in this pane. Like an
/// orphaned drawing, one whose pane is gone folds into the main chart.
pub fn empty_groups_in<'a>(
    groups: &'a [DrawGroup],
    pane_id: &str,
    pane_ids: &HashSet<String>,
) -> Vec<&'a DrawGroup> {
    groups
        .iter()
        .filter(|g| {
            if !g.ids.is_empty() {
                return false;
            }
            let at = g.pane.as_deref().unwrap_or(PRICE_PANE_ID);
            if pane_id == PRICE_PANE_ID {
                at == PRICE_PANE_ID || !pane_ids.contains(at)
            } else {
                at == pane_id
            }
        })
        .collect()
}

/// Fold a pane's front-first drawings into top-level units. A grouped drawing surfaces as
/// part of its group's block at the front-most member's slot, so a group never scatters.
pub fn drawing_units(pane_draws: &[SerializedDrawing], groups: &[DrawGroup]) -> Vec<DrawUnit> {
    let mut units = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for d in pane_draws {
        if seen.contains(d.id.as_str()) {
            continue;
        }
        if let Some(g) = group_of(groups, &d.id) {
            let members: Vec<SerializedDrawing> = pane_draws
                .iter()
                .filter(|x| g.ids.contains(&x.id))
                .cloned()
                .collect();
            for m in pane_draws.iter().filter(|x| g.ids.contains(&x.id)) {
                seen.insert(m.id.as_str());
            }
            units.push(DrawUnit::Group {
                group: g.clone(),
                members,
            });
        } else {
            seen.insert(d.id.as_str());
            units.push(DrawUnit::Draw(Box::new(d.clone())));
        }
    }
    units
}

/// A unit's z key: its front-most member's. That is the slot a group surfaces at, so a
/// bundle whose members straddle a series still reads as one block. An empty group has no
/// member to go by and leads the pane, where a new group is easiest to find.
fn unit_z(u: &DrawUnit) -> f64 {
    match u {
        DrawUnit::Draw(d) => d.z_index,
        DrawUnit::Group { members, .. } => members.first().map_or(f64::INFINITY, |m| m.z_index),
    }
}

/// One pane's stack. With interleaving, drawings and series merge into a single column
/// sorted by z (a drawing tying a series paints under it, so the series row comes first);
/// without it, the drawings lead (they always paint over the series) and the series follow.
fn pane_items(snap: &TreeSnapshot<'_>, pane: &PaneInfo, units: Vec<DrawUnit>) -> Vec<TreeItem> {
    let price_row = || {
        TreeRow::Price(PriceRow {
            label: snap.price_label.clone(),
            visible: snap.price_visible,
        })
    };
    let indicator_row = |i: &PaneIndicatorInfo| {
        TreeRow::Indicator(IndicatorRow {
            id: i.id.clone(),
            label: indicator_row_label(i, snap.handle_title),
            visible: (snap.indicator_visible)(&i.id),
            own_scale: i.own_scale,
        })
    };
    if !snap.stackable {
        let mut rows: Vec<TreeItem> = pane
            .indicators
            .iter()
            .map(|i| TreeItem::Row(indicator_row(i)))
            .collect();
        if pane.kind == PaneKind::Price {
            rows.insert(0, TreeItem::Row(price_row()));
        }
        let mut out: Vec<TreeItem> = units.into_iter().map(TreeItem::Unit).collect();
        out.extend(rows);
        return out;
    }
    let z_of: HashMap<&str, f64> = snap.z_order.iter().map(|e| (e.id.as_str(), e.z)).collect();
    // `z_order` lists the indicators back to front as they paint, so on a shared key its
    // order (not the pane's listing order) says which one is in front.
    let rank: HashMap<&str, f64> = snap
        .z_order
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i as f64))
        .collect();
    struct SeriesEntry {
        item: TreeItem,
        z: f64,
        rank: f64,
    }
    let mut rows: Vec<SeriesEntry> = pane
        .indicators
        .iter()
        .map(|i| SeriesEntry {
            item: TreeItem::Row(indicator_row(i)),
            z: z_of.get(i.id.as_str()).copied().unwrap_or(0.0),
            rank: rank.get(i.id.as_str()).copied().unwrap_or(-1.0),
        })
        .collect();
    // The candles paint before an indicator sharing their key.
    if pane.kind == PaneKind::Price {
        rows.push(SeriesEntry {
            item: TreeItem::Row(price_row()),
            z: snap.candle_z,
            rank: f64::NEG_INFINITY,
        });
    }
    let desc = |a: f64, b: f64| b.partial_cmp(&a).unwrap_or(Ordering::Equal);
    rows.sort_by(|a, b| desc(a.z, b.z).then(desc(a.rank, b.rank)));
    if !snap.interleave {
        let mut out: Vec<TreeItem> = units.into_iter().map(TreeItem::Unit).collect();
        out.extend(rows.into_iter().map(|r| r.item));
        return out;
    }
    struct Entry {
        item: TreeItem,
        z: f64,
        is_unit: bool,
    }
    let mut entries: Vec<Entry> = rows
        .into_iter()
        .map(|r| Entry {
            item: r.item,
            z: r.z,
            is_unit: false,
        })
        .collect();
    entries.extend(units.into_iter().map(|unit| Entry {
        z: unit_z(&unit),
        item: TreeItem::Unit(unit),
        is_unit: true,
    }));
    // Front-first; on a tie the series row leads: a drawing at a series' own z paints under it.
    entries.sort_by(|a, b| {
        if a.z == b.z {
            a.is_unit.cmp(&b.is_unit)
        } else {
            desc(a.z, b.z)
        }
    });
    entries.into_iter().map(|e| e.item).collect()
}

/// The whole tree, top pane first.
pub fn build_tree(snap: &TreeSnapshot<'_>) -> Vec<TreePane> {
    let pane_ids: HashSet<String> = snap.panes.iter().map(|p| p.id.clone()).collect();
    snap.panes
        .iter()
        .map(|pane| {
            let mut units: Vec<DrawUnit> = empty_groups_in(snap.groups, &pane.id, &pane_ids)
                .into_iter()
                .map(|group| DrawUnit::Group {
                    group: group.clone(),
                    members: Vec::new(),
                })
                .collect();
            units.extend(drawing_units(
                &pane_drawings(snap.drawings, &pane.id, &pane_ids),
                snap.groups,
            ));
            TreePane {
                id: pane.id.clone(),
                kind: pane.kind,
                label: pane_label(pane, snap.handle_title, snap.i18n),
                order: pane.order,
                collapsed: pane.collapsed,
                maximized: pane.maximized,
                items: pane_items(snap, pane, units),
            }
        })
        .collect()
}

/// True when the tree has nothing to show: no indicators beyond the candles, no drawings.
pub fn tree_is_empty(panes: &[TreePane]) -> bool {
    panes.iter().all(|p| {
        p.items
            .iter()
            .all(|it| matches!(it, TreeItem::Row(TreeRow::Price(_))))
    })
}

#[cfg(test)]
mod tests;
