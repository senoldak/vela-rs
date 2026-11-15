//! The timeline-mark LANE's geometry: pure functions. Where each mark snaps on the bar
//! grid, how marks fold into clusters and stacks, where every glyph sits, and which glyph
//! is under a point. The painter and the renderer's hit-tests both read the same
//! [`MarkLaneLayout`], so what is drawn is exactly what is clickable.
//!
//! Marks are shared as `Rc<TimelineMark>` (a cluster refers to the marks of the snapshot
//! instead of copying their popup content every frame). The original's `Map`s iterate in
//! insertion order and the layout depends on it, so they are kept as an insertion-ordered
//! vector with a lookup index.
// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::collections::HashMap;
use std::rc::Rc;

use crate::i18n::{MessageArg, Translator, localize_number_text, msg, text_of};
use crate::marks::{MarkGroup, TimelineMark};

/// Token size of a single mark, px.
pub const MARK_GLYPH_PX: f64 = 16.0;
/// Token size of a cluster (several marks of one group sharing a glyph), a little larger,
/// px; also the width one cluster must clear before the next starts
/// ([`fold_overlapping_clusters`]).
pub const MARK_CLUSTER_PX: f64 = 20.0;
/// Air between a glyph's bottom edge and the time-axis line, px.
pub const MARK_LANE_INSET: f64 = 4.0;
/// In a collapsed stack (several groups on one bar) each deeper group peeks out this far
/// above the one over it, px.
pub const MARK_DECK_STEP: f64 = 3.0;
/// Gap between the fanned-out glyphs of an expanded stack, px.
pub const MARK_FAN_GAP: f64 = 4.0;
/// Forgiveness around a glyph for hover and click, px.
pub const MARK_HIT_PAD: f64 = 3.0;
/// Extra reach above a fanned stack's top glyph before the fan folds: a pointer
/// overshooting the top by a few px keeps it open, px.
pub const MARK_FAN_HOLD: f64 = 8.0;

/// The marks of one visibility group that share a glyph: those that snapped onto one bar,
/// plus, once zoomed out far enough that neighbouring glyphs would overlap, the runs of
/// adjacent bars folded together by [`fold_overlapping_clusters`].
#[derive(Clone, Debug, PartialEq)]
pub struct MarkCluster {
    /// `{bar}|{group}`: stable across frames, what an open popup is keyed by. A folded run
    /// keys by its earliest bar.
    pub key: String,
    /// The snapped bar index: a folded run's earliest bar (may lie past the loaded range:
    /// the extrapolated grid).
    pub bar: usize,
    pub group: Option<String>,
    /// Earliest time first, then insertion order.
    pub marks: Vec<Rc<TimelineMark>>,
}

/// One glyph as laid out for this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub cluster: Rc<MarkCluster>,
    /// Center, plot-space px.
    pub x: f64,
    pub y: f64,
    pub size: f64,
    /// The stack (bar index) this glyph belongs to.
    pub stack: usize,
    /// Its depth in the stack (0 = the top group).
    pub depth: usize,
    /// The stack holds several groups and is drawn collapsed (a deck): only its top glyph
    /// is interactive.
    pub decked: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkLaneLayout {
    /// In paint order (a deck's deeper glyphs first, its top glyph last).
    pub glyphs: Vec<PlacedGlyph>,
    /// Per stack (bar index), depth-ordered, in the order the stacks were laid out.
    pub stacks: Vec<(usize, Vec<PlacedGlyph>)>,
}

impl MarkLaneLayout {
    /// The depth-ordered glyphs of the stack on `bar`.
    pub fn stack(&self, bar: usize) -> Option<&[PlacedGlyph]> {
        self.stacks
            .iter()
            .find(|(b, _)| *b == bar)
            .map(|(_, glyphs)| glyphs.as_slice())
    }
}

pub struct MarkLaneInput<'a> {
    pub marks: &'a [Rc<TimelineMark>],
    /// The defined groups, in definition order: the deck order of a multi-group stack.
    pub groups: &'a [MarkGroup],
    /// Whether a group's marks are hidden (settings); never called for ungrouped marks.
    pub hidden: &'a dyn Fn(&str) -> bool,
    /// The chart's bar open times, ascending.
    pub bar_times: &'a [i64],
    pub interval_ms: f64,
    /// Bar index to plot x of the bar's center.
    pub x_of: &'a dyn Fn(usize) -> f64,
    /// The y of the time-axis line (the plot's data height).
    pub axis_y: f64,
    pub data_w: f64,
    /// The stack (bar index) fanned out by hover or tap, if any.
    pub expanded: Option<usize>,
}

/// The bar a mark time belongs to: the bar whose `[open, open + interval)` span contains
/// it; a time in a gap (weekend, closed session) goes to the first bar that follows; a
/// time past the newest bar's span lands on the extrapolated grid (a virtual bar in the
/// right whitespace). `None` before the first loaded bar: nothing to anchor to yet.
pub fn snap_mark_bar(time: i64, bar_times: &[i64], interval_ms: f64) -> Option<usize> {
    let n = bar_times.len();
    if n == 0 || !(interval_ms > 0.0) {
        return None;
    }
    let time = time as f64;
    if time < bar_times[0] as f64 {
        return None;
    }
    let last = bar_times[n - 1] as f64;
    if time >= last + interval_ms {
        return Some(n - 1 + f64::floor((time - last) / interval_ms) as usize);
    }
    // Greatest i with bar_times[i] <= time.
    let (mut lo, mut hi) = (0, n - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if bar_times[mid] as f64 <= time {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if time < bar_times[lo] as f64 + interval_ms {
        return Some(lo);
    }
    Some(lo + 1) // in a gap: `time < last + interval` guarantees lo < n - 1
}

/// Fold the visible marks into clusters keyed by (snapped bar, group); each cluster's
/// marks run earliest-first, then insertion order.
pub fn cluster_marks(
    marks: &[Rc<TimelineMark>],
    bar_times: &[i64],
    interval_ms: f64,
    hidden: &dyn Fn(&str) -> bool,
) -> Vec<MarkCluster> {
    struct Building {
        cluster: MarkCluster,
        seq: Vec<usize>,
    }
    let mut order: Vec<Building> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (seq, m) in marks.iter().enumerate() {
        if let Some(group) = &m.group
            && hidden(group)
        {
            continue;
        }
        let Some(bar) = snap_mark_bar(m.time, bar_times, interval_ms) else {
            continue;
        };
        let key = format!("{bar}|{}", m.group.as_deref().unwrap_or(""));
        let at = *by_key.entry(key.clone()).or_insert_with(|| {
            order.push(Building {
                cluster: MarkCluster {
                    key,
                    bar,
                    group: m.group.clone(),
                    marks: Vec::new(),
                },
                seq: Vec::new(),
            });
            order.len() - 1
        });
        order[at].cluster.marks.push(Rc::clone(m));
        order[at].seq.push(seq);
    }
    order
        .into_iter()
        .map(|b| {
            let mut ranked: Vec<(Rc<TimelineMark>, usize)> =
                b.cluster.marks.into_iter().zip(b.seq).collect();
            ranked.sort_by(|a, b| a.0.time.cmp(&b.0.time).then(a.1.cmp(&b.1)));
            MarkCluster {
                key: b.cluster.key,
                bar: b.cluster.bar,
                group: b.cluster.group,
                marks: ranked.into_iter().map(|(m, _)| m).collect(),
            }
        })
        .collect()
}

/// Fold same-group clusters whose glyphs would overlap at the current bar spacing.
/// Walking each group's clusters in bar order, a cluster joins the one before it while
/// its centre sits within `bucket_px` of that cluster's ANCHOR (its earliest bar); the
/// first cluster to clear that width starts the next one. So a dense lane becomes a row
/// of clusters spaced at least a glyph apart: never one giant glyph at the left edge,
/// never a band of overlapping ones. Anchoring at the earliest bar keeps a cluster's key
/// stable while zooming out adds members on the right, so an open popup keeps following
/// it; zooming back in separates the marks again. Groups never fold into each other.
/// Per-bar clusters arrive earliest-first and bars ascend, so concatenating keeps the
/// marks earliest-first.
pub fn fold_overlapping_clusters(
    clusters: &[MarkCluster],
    x_of: &dyn Fn(usize) -> f64,
    bucket_px: f64,
) -> Vec<MarkCluster> {
    let mut groups: Vec<Vec<&MarkCluster>> = Vec::new();
    let mut index: HashMap<Option<&str>, usize> = HashMap::new();
    for c in clusters {
        let at = *index.entry(c.group.as_deref()).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[at].push(c);
    }
    let mut out = Vec::new();
    for mut list in groups {
        list.sort_by_key(|c| c.bar);
        let mut runs: Vec<MarkCluster> = Vec::new();
        // Centre of the current cluster's earliest bar: what the next glyph must clear.
        let mut anchor_x = f64::NAN;
        for c in list {
            let x = x_of(c.bar);
            match runs.last_mut() {
                Some(run) if x.is_finite() && anchor_x.is_finite() && x - anchor_x < bucket_px => {
                    // `run.marks` is this function's own vector, so appending in place is
                    // safe, and keeps a bucket that swallows thousands of marks at an
                    // extreme zoom-out linear.
                    run.marks.extend(c.marks.iter().cloned());
                }
                _ => {
                    runs.push(MarkCluster {
                        key: c.key.clone(),
                        bar: c.bar,
                        group: c.group.clone(),
                        marks: c.marks.clone(),
                    });
                    anchor_x = x;
                }
            }
        }
        out.extend(runs);
    }
    out
}

/// Deck order of the groups: defined groups first (definition order), then undefined ones
/// by first appearance, ungrouped marks last.
struct GroupRank(HashMap<String, i64>);

impl GroupRank {
    const MAX_SAFE: i64 = 9_007_199_254_740_991;

    fn new(groups: &[MarkGroup], clusters: &[MarkCluster]) -> Self {
        let mut rank: HashMap<String, i64> = HashMap::new();
        for (i, g) in groups.iter().enumerate() {
            rank.insert(g.id.clone(), i as i64);
        }
        for c in clusters {
            if let Some(g) = &c.group
                && !rank.contains_key(g)
            {
                let next = rank.len() as i64;
                rank.insert(g.clone(), next);
            }
        }
        Self(rank)
    }

    fn of(&self, group: Option<&str>) -> i64 {
        match group {
            None => Self::MAX_SAFE,
            Some(g) => self.0.get(g).copied().unwrap_or(Self::MAX_SAFE - 1),
        }
    }
}

/// Lay the lane out for one frame.
pub fn layout_mark_lane(input: &MarkLaneInput) -> MarkLaneLayout {
    let clusters = fold_overlapping_clusters(
        &cluster_marks(
            input.marks,
            input.bar_times,
            input.interval_ms,
            input.hidden,
        ),
        input.x_of,
        MARK_CLUSTER_PX,
    );
    let rank = GroupRank::new(input.groups, &clusters);
    let mut by_bar: Vec<(usize, Vec<MarkCluster>)> = Vec::new();
    let mut bar_index: HashMap<usize, usize> = HashMap::new();
    for c in clusters {
        let at = *bar_index.entry(c.bar).or_insert_with(|| {
            by_bar.push((c.bar, Vec::new()));
            by_bar.len() - 1
        });
        by_bar[at].1.push(c);
    }
    let mut glyphs = Vec::new();
    let mut stacks = Vec::new();
    for (bar, mut list) in by_bar {
        let x = (input.x_of)(bar);
        if !x.is_finite() || x < -MARK_CLUSTER_PX || x > input.data_w + MARK_CLUSTER_PX {
            continue;
        }
        list.sort_by_key(|c| rank.of(c.group.as_deref()));
        let multi = list.len() > 1;
        let expanded = multi && input.expanded == Some(bar);
        let decked = multi && !expanded;
        // A deck draws every glyph at the TOP glyph's size so the peeking edges line up.
        let deck_size = if list[0].marks.len() > 1 {
            MARK_CLUSTER_PX
        } else {
            MARK_GLYPH_PX
        };
        // Bottom edge of the next fanned glyph.
        let mut bottom = input.axis_y - MARK_LANE_INSET;
        let mut placed: Vec<PlacedGlyph> = Vec::with_capacity(list.len());
        for (depth, cluster) in list.into_iter().enumerate() {
            let size = if decked {
                deck_size
            } else if cluster.marks.len() > 1 {
                MARK_CLUSTER_PX
            } else {
                MARK_GLYPH_PX
            };
            let y = if expanded {
                let y = bottom - size / 2.0;
                bottom -= size + MARK_FAN_GAP;
                y
            } else {
                input.axis_y - MARK_LANE_INSET - size / 2.0 - depth as f64 * MARK_DECK_STEP
            };
            placed.push(PlacedGlyph {
                cluster: Rc::new(cluster),
                x,
                y,
                size,
                stack: bar,
                depth,
                decked,
            });
        }
        // Deeper glyphs paint first so the top group ends up on top of the deck.
        glyphs.extend(placed.iter().rev().cloned());
        stacks.push((bar, placed));
    }
    MarkLaneLayout { glyphs, stacks }
}

/// The interactive glyph under a plot point, topmost first; a collapsed deck answers with
/// its top glyph only.
pub fn mark_glyph_at(layout: &MarkLaneLayout, x: f64, y: f64) -> Option<&PlacedGlyph> {
    layout.glyphs.iter().rev().find(|g| {
        if g.decked && g.depth != 0 {
            return false;
        }
        let r = g.size / 2.0 + MARK_HIT_PAD;
        (x - g.x).abs() <= r && (y - g.y).abs() <= r
    })
}

/// The stack (bar index) whose fanned or decked glyphs cover a plot point: what keeps a
/// fan open while the pointer climbs it.
pub fn mark_stack_at(layout: &MarkLaneLayout, x: f64, y: f64) -> Option<usize> {
    for (bar, placed) in &layout.stacks {
        for g in placed {
            let r = g.size / 2.0 + MARK_HIT_PAD;
            if (x - g.x).abs() <= r && (y - g.y).abs() <= r {
                return Some(*bar);
            }
        }
        // The gaps between fanned glyphs count too (a pointer climbing the fan must not
        // collapse it), and so does a short reach past the top glyph, so overshooting it
        // by a few pixels does not fold the fan under the pointer.
        if let (Some(base), Some(top)) = (placed.first(), placed.last())
            && placed.len() > 1
            && !base.decked
        {
            let r = f64::max(top.size, base.size) / 2.0 + MARK_HIT_PAD;
            if (x - base.x).abs() <= r
                && y >= top.y - top.size / 2.0 - MARK_FAN_HOLD
                && y <= base.y + base.size / 2.0 + MARK_HIT_PAD
            {
                return Some(*bar);
            }
        }
    }
    None
}

/// Hover text for a glyph: the mark's own tooltip (or title) alone; a cluster names its
/// group and size.
pub fn cluster_tooltip(
    cluster: &MarkCluster,
    groups: &[MarkGroup],
    i18n: Option<&dyn Translator>,
) -> Option<String> {
    let first = cluster.marks.first()?;
    if cluster.marks.len() == 1 {
        return first.tooltip.clone().or_else(|| first.title.clone());
    }
    let label = match &cluster.group {
        Some(group) => mark_group_label(group, groups),
        None => first
            .title
            .clone()
            .or_else(|| first.tooltip.clone())
            .unwrap_or_else(|| text_of(i18n, &msg("chart.marks.marks", "Marks"), &[])),
    };
    let count = localize_number_text(&cluster.marks.len().to_string(), i18n);
    // A cluster's hover text: its group (or first mark) and how many marks it gathers.
    Some(text_of(
        i18n,
        &msg("chart.marks.cluster", "{label} \u{b7} {count}"),
        &[
            ("label", MessageArg::from(label)),
            ("count", MessageArg::from(count)),
        ],
    ))
}

/// A group's display label: its definition, else the capitalized id.
pub fn mark_group_label(group_id: &str, groups: &[MarkGroup]) -> String {
    if let Some(def) = groups.iter().find(|g| g.id == group_id) {
        return def.label.clone();
    }
    let mut chars = group_id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Every group the chart knows about: the defined ones (definition order), then the ids
/// marks name without a definition (first appearance), each with its display label.
pub fn effective_mark_groups(marks: &[Rc<TimelineMark>], groups: &[MarkGroup]) -> Vec<MarkGroup> {
    let mut out: Vec<MarkGroup> = groups.to_vec();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|g| g.id.clone()).collect();
    for m in marks {
        let Some(group) = &m.group else { continue };
        if !seen.insert(group.clone()) {
            continue;
        }
        out.push(MarkGroup {
            id: group.clone(),
            label: mark_group_label(group, groups),
            ..MarkGroup::default()
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intl::days_from_civil;
    use crate::marks::MarkGlyph;

    const H: i64 = 3_600_000;
    const HF: f64 = 3_600_000.0;

    fn t0() -> i64 {
        days_from_civil(2024, 6, 10) * 86_400_000
    }

    /// Hourly bars with a closed session after bar 4: bars 0 to 4 are contiguous, bar 5
    /// opens four hours after bar 4.
    fn times() -> Vec<i64> {
        [0, 1, 2, 3, 4, 8, 9, 10]
            .iter()
            .map(|h| t0() + h * H)
            .collect()
    }

    fn mark(id: &str, time: i64, group: Option<&str>) -> Rc<TimelineMark> {
        let mut m = TimelineMark::new(
            id,
            time,
            MarkGlyph {
                color: "#2962ff".into(),
                letter: id.chars().next().map(String::from),
                ..MarkGlyph::default()
            },
        );
        m.group = group.map(String::from);
        Rc::new(m)
    }

    fn group(id: &str, label: &str) -> MarkGroup {
        MarkGroup {
            id: id.into(),
            label: label.into(),
            ..MarkGroup::default()
        }
    }

    fn ids(c: &MarkCluster) -> Vec<&str> {
        c.marks.iter().map(|m| m.id.as_str()).collect()
    }

    fn no_hidden(_: &str) -> bool {
        false
    }

    // marks: snapMarkBar

    #[test]
    fn snaps_inside_the_bar_whose_span_contains_the_time() {
        let t = times();
        assert_eq!(snap_mark_bar(t0() + 2 * H + 15 * 60_000, &t, HF), Some(2));
        assert_eq!(snap_mark_bar(t0() + 2 * H, &t, HF), Some(2)); // exactly at the open
        assert_eq!(snap_mark_bar(t0() + 3 * H - 1, &t, HF), Some(2)); // last ms of the span
    }

    #[test]
    fn a_time_in_a_gap_goes_to_the_first_bar_after_it() {
        let t = times();
        assert_eq!(snap_mark_bar(t0() + 5 * H, &t, HF), Some(5));
        assert_eq!(snap_mark_bar(t0() + 6 * H + H / 2, &t, HF), Some(5));
    }

    #[test]
    fn past_the_newest_bar_it_projects_onto_the_extrapolated_grid() {
        let t = times();
        assert_eq!(snap_mark_bar(t0() + 10 * H + 59 * 60_000, &t, HF), Some(7));
        assert_eq!(snap_mark_bar(t0() + 11 * H, &t, HF), Some(8));
        assert_eq!(snap_mark_bar(t0() + 13 * H + H / 2, &t, HF), Some(10));
    }

    #[test]
    fn nothing_to_anchor_to_before_the_first_bar_without_bars_or_a_cadence() {
        let t = times();
        assert_eq!(snap_mark_bar(t0() - 1, &t, HF), None);
        assert_eq!(snap_mark_bar(t0(), &[], HF), None);
        assert_eq!(snap_mark_bar(t0(), &t, 0.0), None);
        assert_eq!(snap_mark_bar(t0(), &t, f64::NAN), None);
    }

    #[test]
    fn a_single_bar_series_snaps_to_it() {
        assert_eq!(snap_mark_bar(5, &[0], 10.0), Some(0));
        assert_eq!(snap_mark_bar(10, &[0], 10.0), Some(1));
    }

    // marks: clusterMarks

    #[test]
    fn folds_same_bar_same_group_marks_and_orders_them_by_time_then_insertion() {
        let marks = [
            mark("b", t0() + 2 * H + 30 * 60_000, Some("g")),
            mark("a", t0() + 2 * H + 5 * 60_000, Some("g")),
            mark("c", t0() + 2 * H + 30 * 60_000, Some("g")),
        ];
        let out = cluster_marks(&marks, &times(), HF, &no_hidden);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "2|g");
        assert_eq!(ids(&out[0]), ["a", "b", "c"]);
    }

    #[test]
    fn a_coarser_timeframe_folds_marks_that_were_bars_apart_on_a_finer_one() {
        let five_min = 300_000;
        let fine: Vec<i64> = (0..24).map(|i| t0() + i * five_min).collect();
        let marks = [
            mark("a", t0() + 5 * five_min, None),
            mark("b", t0() + 9 * five_min, None),
        ];
        assert_eq!(
            cluster_marks(&marks, &fine, five_min as f64, &no_hidden).len(),
            2
        );
        assert_eq!(cluster_marks(&marks, &times(), HF, &no_hidden).len(), 1);
    }

    #[test]
    fn keeps_groups_apart_and_drops_hidden_ones() {
        let marks = [
            mark("a", t0() + H, Some("x")),
            mark("b", t0() + H, Some("y")),
            mark("c", t0() + H, None),
        ];
        let keys = |hidden: &dyn Fn(&str) -> bool| {
            let mut k: Vec<String> = cluster_marks(&marks, &times(), HF, hidden)
                .into_iter()
                .map(|c| c.key)
                .collect();
            k.sort();
            k
        };
        assert_eq!(keys(&no_hidden), ["1|", "1|x", "1|y"]);
        assert_eq!(keys(&|g| g == "y"), ["1|", "1|x"]);
    }

    #[test]
    fn marks_before_the_first_bar_are_dropped() {
        let marks = [mark("a", t0() - H, None)];
        assert!(cluster_marks(&marks, &times(), HF, &no_hidden).is_empty());
    }

    // marks: layoutMarkLane

    struct Lane {
        groups: Vec<MarkGroup>,
        times: Vec<i64>,
        interval: f64,
        axis_y: f64,
        data_w: f64,
    }

    fn lane() -> Lane {
        Lane {
            groups: vec![group("x", "X"), group("y", "Y")],
            times: times(),
            interval: HF,
            axis_y: 400.0,
            data_w: 500.0,
        }
    }

    impl Lane {
        fn layout(
            &self,
            marks: &[Rc<TimelineMark>],
            x_of: &dyn Fn(usize) -> f64,
            expanded: Option<usize>,
        ) -> MarkLaneLayout {
            layout_mark_lane(&MarkLaneInput {
                marks,
                groups: &self.groups,
                hidden: &no_hidden,
                bar_times: &self.times,
                interval_ms: self.interval,
                x_of,
                axis_y: self.axis_y,
                data_w: self.data_w,
                expanded,
            })
        }
    }

    fn x100(bar: usize) -> f64 {
        100.0 + bar as f64 * 10.0
    }

    #[test]
    fn a_lone_mark_is_a_16px_token_centered_on_its_bar() {
        let l = lane().layout(&[mark("a", t0() + 3 * H + 10, Some("x"))], &x100, None);
        assert_eq!(l.glyphs.len(), 1);
        let g = &l.glyphs[0];
        assert_eq!(g.size, MARK_GLYPH_PX);
        assert_eq!(g.x, 130.0);
        assert_eq!(g.y + g.size / 2.0, 400.0 - MARK_LANE_INSET);
        assert!(!g.decked);
    }

    #[test]
    fn a_cluster_is_the_larger_token() {
        let marks = [
            mark("a", t0() + 3 * H, Some("x")),
            mark("b", t0() + 3 * H + 1, Some("x")),
        ];
        let l = lane().layout(&marks, &x100, None);
        assert_eq!(l.glyphs.len(), 1);
        assert_eq!(l.glyphs[0].size, MARK_CLUSTER_PX);
        assert_eq!(ids(&l.glyphs[0].cluster), ["a", "b"]);
    }

    #[test]
    fn several_groups_on_one_bar_deck_first_defined_group_on_top() {
        let marks = [
            mark("b", t0() + 3 * H, Some("y")),
            mark("a", t0() + 3 * H, Some("x")),
            mark("c", t0() + 3 * H, None),
        ];
        let l = lane().layout(&marks, &x100, None);
        let stack = l.stack(3).unwrap();
        let groups: Vec<Option<&str>> = stack.iter().map(|g| g.cluster.group.as_deref()).collect();
        assert_eq!(groups, [Some("x"), Some("y"), None]);
        assert!(stack.iter().all(|g| g.decked));
        assert_eq!(stack[0].y - stack[1].y, MARK_DECK_STEP);
        assert_eq!(stack[1].y - stack[2].y, MARK_DECK_STEP);
        let depths: Vec<usize> = l.glyphs.iter().map(|g| g.depth).collect();
        assert_eq!(depths, [2, 1, 0]);
    }

    #[test]
    fn an_expanded_stack_fans_upward_with_a_gap_between_tokens() {
        let marks = [
            mark("a", t0() + 3 * H, Some("x")),
            mark("b", t0() + 3 * H, Some("y")),
            mark("c", t0() + 3 * H, None),
        ];
        let l = lane().layout(&marks, &x100, Some(3));
        let s = l.stack(3).unwrap();
        assert!(s.iter().all(|g| !g.decked));
        assert_eq!(s[0].y + s[0].size / 2.0, 400.0 - MARK_LANE_INSET);
        assert_eq!(
            s[0].y - s[0].size / 2.0 - (s[1].y + s[1].size / 2.0),
            MARK_FAN_GAP
        );
        assert_eq!(
            s[1].y - s[1].size / 2.0 - (s[2].y + s[2].size / 2.0),
            MARK_FAN_GAP
        );
    }

    #[test]
    fn a_single_group_bar_never_decks_or_fans() {
        let l = lane().layout(&[mark("a", t0() + 3 * H, Some("x"))], &x100, Some(3));
        assert!(!l.glyphs[0].decked);
        assert_eq!(
            l.glyphs[0].y + l.glyphs[0].size / 2.0,
            400.0 - MARK_LANE_INSET
        );
    }

    #[test]
    fn skips_stacks_off_the_plot() {
        let m = [mark("a", t0() + 3 * H, None)];
        assert!(lane().layout(&m, &|_| -100.0, None).glyphs.is_empty());
        assert!(lane().layout(&m, &|_| 800.0, None).glyphs.is_empty());
        assert!(lane().layout(&m, &|_| f64::NAN, None).glyphs.is_empty());
    }

    #[test]
    fn no_marks_lay_out_nothing() {
        let l = lane().layout(&[], &x100, None);
        assert!(l.glyphs.is_empty() && l.stacks.is_empty());
    }

    // marks: hit-testing

    fn hit_marks() -> [Rc<TimelineMark>; 2] {
        [
            mark("a", t0() + 3 * H, Some("x")),
            mark("b", t0() + 3 * H, Some("y")),
        ]
    }

    #[test]
    fn glyph_at_answers_the_topmost_glyph_and_ignores_a_decks_buried_cards() {
        let l = lane().layout(&hit_marks(), &x100, None);
        let top = &l.stack(3).unwrap()[0];
        let buried = &l.stack(3).unwrap()[1];
        assert_eq!(
            mark_glyph_at(&l, top.x, top.y).and_then(|g| g.cluster.group.as_deref()),
            Some("x")
        );
        // The buried card's own center is covered by the top card; a point on its peeking
        // edge is NOT interactive either: the deck opens as a whole, not by card.
        assert!(mark_glyph_at(&l, buried.x, buried.y - buried.size / 2.0 - 1.0).is_none());
        assert!(mark_glyph_at(&l, 300.0, 300.0).is_none());
    }

    #[test]
    fn a_fanned_stack_exposes_every_glyph_and_stack_at_keeps_the_fan_open_across_its_gaps() {
        let l = lane().layout(&hit_marks(), &x100, Some(3));
        let s = l.stack(3).unwrap();
        let (bottom, top) = (&s[0], &s[1]);
        let group_at =
            |g: &PlacedGlyph| mark_glyph_at(&l, g.x, g.y).and_then(|g| g.cluster.group.clone());
        assert_eq!(group_at(top).as_deref(), Some("y"));
        assert_eq!(group_at(bottom).as_deref(), Some("x"));
        let gap_y = (bottom.y - bottom.size / 2.0 + (top.y + top.size / 2.0)) / 2.0;
        assert_eq!(mark_stack_at(&l, bottom.x, gap_y), Some(3));
        // A short overshoot past the top glyph keeps the fan; further up it folds.
        assert_eq!(
            mark_stack_at(&l, bottom.x, top.y - top.size / 2.0 - MARK_FAN_HOLD),
            Some(3)
        );
        assert_eq!(
            mark_stack_at(&l, bottom.x, top.y - top.size / 2.0 - MARK_FAN_HOLD - 1.0),
            None
        );
    }

    // marks: labels

    fn cluster_of(group: &str, marks: Vec<Rc<TimelineMark>>) -> MarkCluster {
        MarkCluster {
            key: format!("1|{group}"),
            bar: 1,
            group: Some(group.into()),
            marks,
        }
    }

    #[test]
    fn a_lone_mark_shows_its_tooltip_else_its_title_and_a_cluster_names_its_group_and_size() {
        let groups = [group("dividends", "Dividends")];
        let with = |title: Option<&str>, tooltip: Option<&str>| {
            let mut m = TimelineMark::new("a", t0(), MarkGlyph::default());
            m.title = title.map(String::from);
            m.tooltip = tooltip.map(String::from);
            Rc::new(m)
        };
        let single = cluster_of(
            "dividends",
            vec![with(Some("Dividend"), Some("Div \u{b7} 0.01"))],
        );
        assert_eq!(
            cluster_tooltip(&single, &groups, None).as_deref(),
            Some("Div \u{b7} 0.01")
        );
        let title_only = cluster_of("dividends", vec![with(Some("Dividend"), None)]);
        assert_eq!(
            cluster_tooltip(&title_only, &groups, None).as_deref(),
            Some("Dividend")
        );
        let bare = cluster_of("dividends", vec![with(None, None)]);
        assert_eq!(cluster_tooltip(&bare, &groups, None), None);
        let cluster = cluster_of(
            "dividends",
            vec![mark("a", t0(), None), mark("b", t0(), None)],
        );
        assert_eq!(
            cluster_tooltip(&cluster, &groups, None).as_deref(),
            Some("Dividends \u{b7} 2")
        );
        let splits = MarkCluster {
            group: Some("splits".into()),
            key: "1|splits".into(),
            ..cluster
        };
        assert_eq!(
            cluster_tooltip(&splits, &groups, None).as_deref(),
            Some("Splits \u{b7} 2")
        );
    }

    #[test]
    fn an_empty_cluster_has_no_tooltip() {
        let empty = cluster_of("g", vec![]);
        assert_eq!(cluster_tooltip(&empty, &[], None), None);
    }

    #[test]
    fn an_ungrouped_cluster_falls_back_to_the_first_marks_title_then_marks() {
        let mut titled = TimelineMark::new("a", t0(), MarkGlyph::default());
        titled.title = Some("News".into());
        let cluster = MarkCluster {
            key: "1|".into(),
            bar: 1,
            group: None,
            marks: vec![Rc::new(titled), mark("b", t0(), None)],
        };
        assert_eq!(
            cluster_tooltip(&cluster, &[], None).as_deref(),
            Some("News \u{b7} 2")
        );
        let plain = MarkCluster {
            marks: vec![mark("a", t0(), None), mark("b", t0(), None)],
            ..cluster
        };
        assert_eq!(
            cluster_tooltip(&plain, &[], None).as_deref(),
            Some("Marks \u{b7} 2")
        );
    }

    #[test]
    fn an_undefined_group_falls_back_to_its_capitalized_id_and_defined_groups_list_first() {
        let groups = [group("dividends", "Dividends")];
        assert_eq!(mark_group_label("splits", &groups), "Splits");
        assert_eq!(mark_group_label("dividends", &groups), "Dividends");
        assert_eq!(mark_group_label("", &groups), "");
        assert_eq!(mark_group_label("\u{e9}clair", &groups), "\u{c9}clair");
        let marks = [
            mark("a", t0(), Some("splits")),
            mark("b", t0(), Some("dividends")),
            mark("c", t0(), None),
        ];
        assert_eq!(
            effective_mark_groups(&marks, &groups),
            vec![group("dividends", "Dividends"), group("splits", "Splits")]
        );
    }

    // marks: glyphs that would overlap fold into one cluster

    const M: i64 = 60_000;

    fn minute_bars() -> Vec<i64> {
        (0..8).map(|i| t0() + i * M).collect()
    }

    fn dense() -> Vec<Rc<TimelineMark>> {
        minute_bars()
            .iter()
            .enumerate()
            .map(|(i, t)| mark(&format!("n{i}"), *t, Some("news")))
            .collect()
    }

    fn dense_lane(
        px_per_bar: f64,
        marks: &[Rc<TimelineMark>],
        groups: Vec<MarkGroup>,
    ) -> MarkLaneLayout {
        let x_of = move |bar: usize| 100.0 + bar as f64 * px_per_bar;
        layout_mark_lane(&MarkLaneInput {
            marks,
            groups: &groups,
            hidden: &no_hidden,
            bar_times: &minute_bars(),
            interval_ms: M as f64,
            x_of: &x_of,
            axis_y: 400.0,
            data_w: 2000.0,
            expanded: None,
        })
    }

    fn glyph_ids(l: &MarkLaneLayout) -> Vec<Vec<String>> {
        l.glyphs
            .iter()
            .map(|g| g.cluster.marks.iter().map(|m| m.id.clone()).collect())
            .collect()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn zoomed_out_adjacent_same_group_marks_fold_into_clusters() {
        let l = dense_lane(6.0, &dense(), vec![group("news", "News")]);
        assert_eq!(
            glyph_ids(&l),
            vec![s(&["n0", "n1", "n2", "n3"]), s(&["n4", "n5", "n6", "n7"])]
        );
        assert!(l.glyphs.iter().all(|g| g.size == MARK_CLUSTER_PX));
    }

    #[test]
    fn a_dense_lane_becomes_a_row_of_clusters_that_never_overlap() {
        let bars: Vec<i64> = (0..200).map(|i| t0() + i * H).collect();
        let marks: Vec<Rc<TimelineMark>> = bars
            .iter()
            .enumerate()
            .map(|(i, t)| mark(&format!("s{i}"), *t, Some("news")))
            .collect();
        let x_of = |bar: usize| 10.0 + bar as f64 * 9.0;
        let l = layout_mark_lane(&MarkLaneInput {
            marks: &marks,
            groups: &[],
            hidden: &no_hidden,
            bar_times: &bars,
            interval_ms: HF,
            x_of: &x_of,
            axis_y: 400.0,
            data_w: 2000.0,
            expanded: None,
        });
        assert!(l.glyphs.len() > 50);
        assert!(l.glyphs.len() < 200);
        let mut xs: Vec<f64> = l.glyphs.iter().map(|g| g.x).collect();
        xs.sort_by(f64::total_cmp);
        for w in xs.windows(2) {
            assert!(w[1] - w[0] >= MARK_CLUSTER_PX);
        }
        let total: usize = l.glyphs.iter().map(|g| g.cluster.marks.len()).sum();
        assert_eq!(total, 200); // every story is still reachable
    }

    #[test]
    fn zoomed_in_the_same_marks_stand_apart_again() {
        assert_eq!(
            dense_lane(MARK_CLUSTER_PX, &dense(), vec![]).glyphs.len(),
            8
        );
        assert_eq!(dense_lane(40.0, &dense(), vec![]).glyphs.len(), 8);
    }

    #[test]
    fn a_merged_cluster_sits_on_its_earliest_bar_and_keeps_that_key() {
        let l = dense_lane(6.0, &dense(), vec![group("news", "News")]);
        let g = &l.glyphs[0];
        assert_eq!(g.cluster.bar, 0);
        assert_eq!(g.cluster.key, "0|news");
        assert_eq!(g.x, 100.0);
        assert_eq!(g.stack, 0);
    }

    #[test]
    fn a_wide_gap_in_the_marks_ends_a_cluster() {
        let bars: Vec<i64> = (0..44).map(|i| t0() + i * M).collect();
        let marks: Vec<Rc<TimelineMark>> = [0usize, 1, 2, 3, 40, 41, 42, 43]
            .iter()
            .map(|i| mark(&format!("m{i}"), bars[*i], Some("news")))
            .collect();
        let x_of = |bar: usize| 100.0 + bar as f64 * 4.0;
        let l = layout_mark_lane(&MarkLaneInput {
            marks: &marks,
            groups: &[],
            hidden: &no_hidden,
            bar_times: &bars,
            interval_ms: M as f64,
            x_of: &x_of,
            axis_y: 400.0,
            data_w: 2000.0,
            expanded: None,
        });
        assert_eq!(
            glyph_ids(&l),
            vec![
                s(&["m0", "m1", "m2", "m3"]),
                s(&["m40", "m41", "m42", "m43"])
            ]
        );
        let keys: Vec<&str> = l.glyphs.iter().map(|g| g.cluster.key.as_str()).collect();
        assert_eq!(keys, ["0|news", "40|news"]);
    }

    #[test]
    fn marks_of_different_groups_never_fold_together() {
        let mixed: Vec<Rc<TimelineMark>> = minute_bars()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                mark(
                    &format!("x{i}"),
                    *t,
                    Some(if i % 2 == 1 { "earnings" } else { "news" }),
                )
            })
            .collect();
        let l = dense_lane(
            6.0,
            &mixed,
            vec![group("news", "News"), group("earnings", "Earnings")],
        );
        let collect = |g: &str| -> Vec<String> {
            l.glyphs
                .iter()
                .filter(|p| p.cluster.group.as_deref() == Some(g))
                .flat_map(|p| p.cluster.marks.iter().map(|m| m.id.clone()))
                .collect()
        };
        assert_eq!(collect("news"), s(&["x0", "x2", "x4", "x6"]));
        assert_eq!(collect("earnings"), s(&["x1", "x3", "x5", "x7"]));
        for g in &l.glyphs {
            let groups: std::collections::HashSet<_> =
                g.cluster.marks.iter().map(|m| m.group.clone()).collect();
            assert_eq!(groups.len(), 1);
        }
    }

    #[test]
    fn the_folded_glyph_is_one_click_target_naming_every_story_under_it() {
        let l = dense_lane(6.0, &dense(), vec![group("news", "News")]);
        let g = &l.glyphs[0];
        let hit = mark_glyph_at(&l, g.x, g.y).unwrap();
        assert_eq!(ids(&hit.cluster), ["n0", "n1", "n2", "n3"]);
        assert_eq!(
            cluster_tooltip(&g.cluster, &[group("news", "News")], None).as_deref(),
            Some("News \u{b7} 4")
        );
    }

    #[test]
    fn fold_overlapping_clusters_is_a_pure_step_over_per_bar_clusters() {
        let clusters = cluster_marks(&dense(), &minute_bars(), M as f64, &no_hidden);
        assert_eq!(clusters.len(), 8);
        let sizes = |x: &dyn Fn(usize) -> f64| -> Vec<usize> {
            fold_overlapping_clusters(&clusters, x, MARK_CLUSTER_PX)
                .iter()
                .map(|c| c.marks.len())
                .collect()
        };
        // 0, 5, 10, 15 | 20, 25, 30, 35
        assert_eq!(sizes(&|bar| bar as f64 * 5.0), [4, 4]);
        // Exactly a glyph apart: clear.
        assert_eq!(sizes(&|bar| bar as f64 * MARK_CLUSTER_PX).len(), 8);
        // One pixel short: pairs.
        assert_eq!(
            sizes(&|bar| bar as f64 * (MARK_CLUSTER_PX - 1.0)),
            [2, 2, 2, 2]
        );
    }

    #[test]
    fn a_non_finite_x_never_folds() {
        let clusters = cluster_marks(&dense(), &minute_bars(), M as f64, &no_hidden);
        let out = fold_overlapping_clusters(&clusters, &|_| f64::NAN, MARK_CLUSTER_PX);
        assert_eq!(out.len(), 8);
    }
}
