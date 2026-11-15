//! Channel and pitchfork tools: multi-line drawings whose line math lives in
//! one `geometry(proj)` (pixel segments plus an optional fill polygon). Body
//! hit-testing, handles and bounds derive from it through the shared helpers
//! of `segment.rs` (the original's `SegmentDrawing` base class).

use std::any::Any;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Span,
};
use crate::drawings::geometry::{DrawingPoint, FreeAxis, Projector, SegmentGeometry};
use crate::drawings::hittest::extend_ray;
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{SettingsSchema, fill_fields, line_fields, text_fields};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::drawings::types::segment::{segment_bounds, segment_handle_points, segment_hit_test};
use crate::i18n::msg;
use crate::model::{DrawingExtend, LineStyle};

macro_rules! drawing_plumbing {
    () => {
        fn base(&self) -> &DrawingBase {
            &self.base
        }

        fn base_mut(&mut self) -> &mut DrawingBase {
            &mut self.base
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    };
}

/// The span of every anchor's price; `None` without anchors.
fn all_anchor_prices(base: &DrawingBase) -> Option<Span> {
    base.anchor_price_range()
}

/// The time span of the first two anchors (the baseline), ignoring the third.
fn baseline_time_extent(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Span::of([a.time, b.time])
}

/// Handles of a baseline channel: the two baseline ends plus a third handle at
/// the baseline's midpoint time, at the third anchor's price.
fn baseline_channel_handles(base: &DrawingBase, proj: &dyn Projector) -> Vec<(f64, f64)> {
    let (Some(a), Some(b)) = (base.anchors.first(), base.anchors.get(1)) else {
        return Vec::new();
    };
    let (Some(ya), Some(yb)) = (base.y_of(a.price, proj), base.y_of(b.price, proj)) else {
        return Vec::new();
    };
    let mut pts = vec![(proj.x_of(a.time), ya), (proj.x_of(b.time), yb)];
    if let Some(c) = base.anchors.get(2)
        && let Some(yc) = base.y_of(c.price, proj)
    {
        pts.push((proj.x_of((a.time + b.time) / 2.0), yc));
    }
    pts
}

fn channel_schema() -> SettingsSchema {
    let mut fields = line_fields();
    fields.extend(fill_fields());
    fields.extend(text_fields());
    SettingsSchema::new(fields)
}

/// The anchor handle drawn at pixel position of a data point.
fn point_px(base: &DrawingBase, p: &DrawingPoint, proj: &dyn Projector) -> Option<(f64, f64)> {
    Some((proj.x_of(p.time), base.y_of(p.price, proj)?))
}

// ───────────────────────── parallel channel ─────────────────────────

/// A parallel channel: a baseline through p1 to p2 plus a second line parallel
/// to it. The offset is a constant PRICE delta measured at the baseline
/// MIDPOINT (scale-aware, and the third handle always sits on the channel):
/// only the third anchor's price matters, never its time, which keeps the
/// handle from drifting off to one side after editing.
#[derive(Clone, Debug)]
pub struct ParallelChannel {
    base: DrawingBase,
}

impl ParallelChannel {
    pub const TYPE: &'static str = "parallelchannel";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    /// Price delta of the parallel line from the baseline, measured at the midpoint.
    fn offset(&self) -> f64 {
        match (
            self.base.anchors.first(),
            self.base.anchors.get(1),
            self.base.anchors.get(2),
        ) {
            (Some(a), Some(b), Some(c)) => c.price - (a.price + b.price) / 2.0,
            _ => 0.0,
        }
    }

    /// Pixel segments (the baseline and its parallel) and the fill between them.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let d = self.offset();
        let xa = proj.x_of(a.time);
        let xb = proj.x_of(b.time);
        let ya = self.base.y_of(a.price, proj)?;
        let yb = self.base.y_of(b.price, proj)?;
        let ya2 = self.base.y_of(a.price + d, proj)?;
        let yb2 = self.base.y_of(b.price + d, proj)?;
        Some(SegmentGeometry {
            segments: vec![[xa, ya, xb, yb], [xa, ya2, xb, yb2]],
            fill: Some(vec![(xa, ya), (xb, yb), (xb, yb2), (xa, ya2)]),
        })
    }
}

impl Drawing for ParallelChannel {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema {
            min: 3,
            max: 3,
            slots: vec![
                AnchorSlot::new("p1", FreeAxis::Both),
                AnchorSlot::new("p2", FreeAxis::Both),
                // width only: drag the midpoint handle vertically
                AnchorSlot::new("offset", FreeAxis::Y),
            ],
        }
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
    }

    /// The two baseline ends plus the offset handle ON the parallel line at the midpoint.
    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        baseline_channel_handles(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        Bounds::of_points(&self.handle_points(proj))
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let d = self.offset();
        let mut prices = vec![a.price, b.price, a.price + d, b.price + d];
        if let Some(c) = self.base.anchors.get(2) {
            prices.push(c.price);
        }
        Span::of(prices)
    }

    /// The channel spans the baseline's time only (the offset anchor's time is ignored).
    fn time_extent(&self) -> Option<Span> {
        baseline_time_extent(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        channel_schema()
    }
}

// ───────────────────────── disjoint channel ─────────────────────────

/// A disjoint channel: two independent line segments (top p1 to p2, bottom p3
/// to p4) with an optional fill between them. Unlike a parallel channel the
/// two lines need not be parallel: every corner is free.
#[derive(Clone, Debug)]
pub struct DisjointChannel {
    base: DrawingBase,
}

impl DisjointChannel {
    pub const TYPE: &'static str = "disjointchannel";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    fn seg(&self, i: usize, j: usize, proj: &dyn Projector) -> Option<[f64; 4]> {
        let (x1, y1) = self.base.anchor_px(i, proj)?;
        let (x2, y2) = self.base.anchor_px(j, proj)?;
        Some([x1, y1, x2, y2])
    }

    /// The top and bottom segments that resolve, plus the fill when both do.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let top = self.seg(0, 1, proj);
        let bot = self.seg(2, 3, proj);
        let segments: Vec<[f64; 4]> = [top, bot].into_iter().flatten().collect();
        if segments.is_empty() {
            return None;
        }
        let fill = match (top, bot) {
            (Some(t), Some(b)) => {
                Some(vec![(t[0], t[1]), (t[2], t[3]), (b[2], b[3]), (b[0], b[1])])
            }
            _ => None,
        };
        Some(SegmentGeometry { segments, fill })
    }
}

impl Drawing for DisjointChannel {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["top1", "top2", "bot1", "bot2"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        segment_handle_points(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        segment_bounds(&self.base, proj)
    }

    fn price_range(&self) -> Option<Span> {
        all_anchor_prices(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        channel_schema()
    }
}

// ───────────────────────── flat top / bottom ─────────────────────────

/// A flat top/bottom channel: a sloped baseline p1 to p2 plus a FLAT
/// (horizontal) side at a constant price (p3's price). Only p3's price is used,
/// its time is ignored, and the flat side always spans the baseline's time
/// range. Optional fill between the two sides.
#[derive(Clone, Debug)]
pub struct FlatTopBottom {
    base: DrawingBase,
}

impl FlatTopBottom {
    pub const TYPE: &'static str = "flattopbottom";

    pub fn new(init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
        }
    }

    /// The sloped baseline, plus the flat side and the fill once the third anchor exists.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let xa = proj.x_of(a.time);
        let xb = proj.x_of(b.time);
        let ya = self.base.y_of(a.price, proj)?;
        let yb = self.base.y_of(b.price, proj)?;
        let Some(c) = self.base.anchors.get(2) else {
            // baseline only (placing)
            return Some(SegmentGeometry {
                segments: vec![[xa, ya, xb, yb]],
                fill: None,
            });
        };
        let flat_y = self.base.y_of(c.price, proj)?;
        Some(SegmentGeometry {
            segments: vec![
                [xa, ya, xb, yb],         // sloped side
                [xa, flat_y, xb, flat_y], // flat side (constant price)
            ],
            fill: Some(vec![(xa, ya), (xb, yb), (xb, flat_y), (xa, flat_y)]),
        })
    }
}

impl Drawing for FlatTopBottom {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema {
            min: 3,
            max: 3,
            slots: vec![
                AnchorSlot::new("p1", FreeAxis::Both),
                AnchorSlot::new("p2", FreeAxis::Both),
                // only the flat side's price matters
                AnchorSlot::new("flat", FreeAxis::Y),
            ],
        }
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
    }

    /// The two baseline ends plus the flat-side handle at the baseline midpoint.
    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        baseline_channel_handles(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        Bounds::of_points(&self.handle_points(proj))
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let mut prices = vec![a.price, b.price];
        if let Some(c) = self.base.anchors.get(2) {
            prices.push(c.price);
        }
        Span::of(prices)
    }

    /// The channel spans the baseline's time only (the flat anchor's time is ignored).
    fn time_extent(&self) -> Option<Span> {
        baseline_time_extent(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        channel_schema()
    }
}

// ───────────────────────── pitchforks ─────────────────────────

/// Which pitchfork a [`Pitchfork`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchforkKind {
    /// Andrews' pitchfork: the median runs from the pivot through the midpoint
    /// of p2/p3, with two tines parallel to it through p2 and p3.
    Andrews,
    /// The median origin is shifted in PRICE only: halfway between the pivot
    /// and the first tine point, at the pivot's time.
    Schiff,
    /// The median origin is the full midpoint of the pivot and the first tine
    /// point (shifted in both time and price).
    ModifiedSchiff,
    /// The median is anchored at M = midpoint(p2, p3) but takes its slope from
    /// d = p3 minus midpoint(p1, p2) (the modified base B).
    Inside,
}

/// An Andrews' pitchfork or one of its variants. All three lines extend to the
/// right edge and a base line joins p2 and p3. The Schiff variants start the
/// median at a shifted origin S (computed in data space) and keep the pivot
/// attached through a p1 to p2 construction line; the inside variant also
/// draws a B to p3 construction line.
#[derive(Clone, Debug)]
pub struct Pitchfork {
    base: DrawingBase,
    kind: PitchforkKind,
}

impl Pitchfork {
    pub const TYPE: &'static str = "pitchfork";
    pub const SCHIFF_TYPE: &'static str = "schiffpitchfork";
    pub const MODIFIED_SCHIFF_TYPE: &'static str = "modifiedschiffpitchfork";
    pub const INSIDE_TYPE: &'static str = "insidepitchfork";

    pub fn new(kind: PitchforkKind, init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
            kind,
        }
    }

    pub fn andrews(init: &DrawingInit) -> Self {
        Self::new(PitchforkKind::Andrews, init)
    }

    pub fn schiff(init: &DrawingInit) -> Self {
        Self::new(PitchforkKind::Schiff, init)
    }

    pub fn modified_schiff(init: &DrawingInit) -> Self {
        Self::new(PitchforkKind::ModifiedSchiff, init)
    }

    pub fn inside(init: &DrawingInit) -> Self {
        Self::new(PitchforkKind::Inside, init)
    }

    pub fn kind(&self) -> PitchforkKind {
        self.kind
    }

    /// The median's start point in DATA space for the Schiff variants.
    fn median_start(&self, p0: &DrawingPoint, p1: &DrawingPoint) -> DrawingPoint {
        match self.kind {
            PitchforkKind::ModifiedSchiff => DrawingPoint {
                time: (p0.time + p1.time) / 2.0,
                price: (p0.price + p1.price) / 2.0,
            },
            _ => DrawingPoint {
                time: p0.time,
                price: (p0.price + p1.price) / 2.0,
            },
        }
    }

    /// Pixel segments of the fork. With only the pivot and the cursor placed,
    /// the pivot to cursor line; `None` with a single anchor.
    pub fn geometry(&self, proj: &dyn Projector) -> Option<SegmentGeometry> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let p0 = point_px(&self.base, a, proj)?;
        let p1 = point_px(&self.base, b, proj)?;
        let Some(c) = self.base.anchors.get(2) else {
            // pivot to cursor only (placing)
            return Some(SegmentGeometry {
                segments: vec![[p0.0, p0.1, p1.0, p1.1]],
                fill: None,
            });
        };
        let p2 = point_px(&self.base, c, proj)?;
        let (w, h) = (proj.width(), proj.height());
        let ray = |x1: f64, y1: f64, x2: f64, y2: f64| {
            extend_ray(x1, y1, x2, y2, DrawingExtend::Right, w, h)
        };
        // the median target: midpoint of the two tine anchors
        let mx = (p1.0 + p2.0) / 2.0;
        let my = (p1.1 + p2.1) / 2.0;
        let base_line = [p1.0, p1.1, p2.0, p2.1];
        let pivot_line = [p0.0, p0.1, p1.0, p1.1];

        let segments = match self.kind {
            PitchforkKind::Andrews => {
                let dx = mx - p0.0;
                let dy = my - p0.1;
                vec![
                    ray(p0.0, p0.1, mx, my),               // median
                    ray(p1.0, p1.1, p1.0 + dx, p1.1 + dy), // upper tine
                    ray(p2.0, p2.1, p2.0 + dx, p2.1 + dy), // lower tine
                    base_line,                             // base line p2-p3
                ]
            }
            PitchforkKind::Schiff | PitchforkKind::ModifiedSchiff => {
                let s = point_px(&self.base, &self.median_start(a, b), proj)?;
                let dx = mx - s.0;
                let dy = my - s.1; // median direction from the shifted origin
                vec![
                    ray(s.0, s.1, mx, my),                 // median
                    ray(p1.0, p1.1, p1.0 + dx, p1.1 + dy), // upper tine
                    ray(p2.0, p2.1, p2.0 + dx, p2.1 + dy), // lower tine
                    base_line,
                    // The median leaves from the shifted origin, not the pivot: without
                    // this construction line the pivot handle would float unattached.
                    pivot_line,
                ]
            }
            PitchforkKind::Inside => {
                let bp = point_px(
                    &self.base,
                    &DrawingPoint {
                        time: (a.time + b.time) / 2.0,
                        price: (a.price + b.price) / 2.0,
                    },
                    proj,
                )?; // modified base
                let dx = p2.0 - bp.0;
                let dy = p2.1 - bp.1; // median slope d = P2 - B
                vec![
                    ray(mx, my, mx + dx, my + dy),         // median through M
                    ray(p1.0, p1.1, p1.0 + dx, p1.1 + dy), // upper tine
                    ray(p2.0, p2.1, p2.0 + dx, p2.1 + dy), // lower tine
                    base_line,
                    [bp.0, bp.1, p2.0, p2.1], // construction: modified base to p3
                    pivot_line,               // construction: the back trend line
                ]
            }
        };
        Some(SegmentGeometry {
            segments,
            fill: None,
        })
    }
}

impl Drawing for Pitchfork {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        match self.kind {
            PitchforkKind::Andrews => Self::TYPE,
            PitchforkKind::Schiff => Self::SCHIFF_TYPE,
            PitchforkKind::ModifiedSchiff => Self::MODIFIED_SCHIFF_TYPE,
            PitchforkKind::Inside => Self::INSIDE_TYPE,
        }
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["pivot", "upper", "lower"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        segment_hit_test(&self.base, self.geometry(proj).as_ref(), px, py, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        segment_handle_points(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        segment_bounds(&self.base, proj)
    }

    fn price_range(&self) -> Option<Span> {
        all_anchor_prices(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ───────────────────────── registry ─────────────────────────

fn channel_style() -> DrawingStyle {
    DrawingStyle::line(DEFAULT_DRAWING_COLOR, 2.0, LineStyle::Solid)
        .fill(format!("{DEFAULT_DRAWING_COLOR}1a"))
}

fn fork_style() -> DrawingStyle {
    DrawingStyle::line(DEFAULT_DRAWING_COLOR, 2.0, LineStyle::Solid)
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            ParallelChannel::TYPE,
            "channels",
            msg("drawing.parallelchannel.label", "Parallel Channel"),
            svg24(r#"<line x1="3" y1="16" x2="21" y2="8"/><line x1="3" y1="21" x2="21" y2="13"/>"#),
            channel_style(),
            |init| Box::new(ParallelChannel::new(init)),
        ),
        DrawingTypeMeta::new(
            DisjointChannel::TYPE,
            "channels",
            msg("drawing.disjointchannel.label", "Disjoint Channel"),
            svg24(r#"<line x1="3" y1="7" x2="21" y2="10"/><line x1="3" y1="18" x2="21" y2="14"/>"#),
            channel_style(),
            |init| Box::new(DisjointChannel::new(init)),
        ),
        DrawingTypeMeta::new(
            FlatTopBottom::TYPE,
            "channels",
            msg("drawing.flattopbottom.label", "Flat Top/Bottom"),
            svg24(r#"<line x1="3" y1="7" x2="21" y2="7"/><line x1="3" y1="18" x2="21" y2="11"/>"#),
            channel_style(),
            |init| Box::new(FlatTopBottom::new(init)),
        ),
        DrawingTypeMeta::new(
            Pitchfork::TYPE,
            "pitchforks",
            msg("drawing.pitchfork.label", "Pitchfork"),
            svg24(
                r#"<circle cx="4" cy="12" r="1.6" fill="currentColor"/><line x1="4" y1="12" x2="10" y2="12"/><line x1="10" y1="6" x2="10" y2="18"/><line x1="10" y1="6" x2="21" y2="6"/><line x1="10" y1="12" x2="21" y2="12"/><line x1="10" y1="18" x2="21" y2="18"/>"#,
            ),
            fork_style(),
            |init| Box::new(Pitchfork::andrews(init)),
        ),
        DrawingTypeMeta::new(
            Pitchfork::SCHIFF_TYPE,
            "pitchforks",
            msg("drawing.schiffpitchfork.label", "Schiff Pitchfork"),
            svg24(
                r#"<circle cx="4" cy="8" r="1.6" fill="currentColor"/><line x1="4" y1="8" x2="10" y2="12"/><line x1="10" y1="6" x2="10" y2="18"/><line x1="10" y1="6" x2="21" y2="6"/><line x1="10" y1="12" x2="21" y2="12"/><line x1="10" y1="18" x2="21" y2="18"/>"#,
            ),
            fork_style(),
            |init| Box::new(Pitchfork::schiff(init)),
        ),
        DrawingTypeMeta::new(
            Pitchfork::MODIFIED_SCHIFF_TYPE,
            "pitchforks",
            msg(
                "drawing.modifiedschiffpitchfork.label",
                "Modified Schiff Pitchfork",
            ),
            svg24(
                r#"<circle cx="6" cy="9" r="1.6" fill="currentColor"/><line x1="6" y1="9" x2="10" y2="12"/><line x1="10" y1="6" x2="10" y2="18"/><line x1="10" y1="6" x2="21" y2="6"/><line x1="10" y1="12" x2="21" y2="12"/><line x1="10" y1="18" x2="21" y2="18"/>"#,
            ),
            fork_style(),
            |init| Box::new(Pitchfork::modified_schiff(init)),
        ),
        DrawingTypeMeta::new(
            Pitchfork::INSIDE_TYPE,
            "pitchforks",
            msg("drawing.insidepitchfork.label", "Inside Pitchfork"),
            svg24(
                r#"<line x1="4" y1="12" x2="10" y2="12"/><line x1="10" y1="8" x2="10" y2="16"/><line x1="10" y1="8" x2="21" y2="8"/><line x1="10" y1="12" x2="21" y2="12"/><line x1="10" y1="16" x2="21" y2="16"/>"#,
            ),
            fork_style(),
            |init| Box::new(Pitchfork::inside(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::projector::LinearProjector;

    const KEYS: [&str; 7] = [
        "parallelchannel",
        "disjointchannel",
        "flattopbottom",
        "pitchfork",
        "schiffpitchfork",
        "modifiedschiffpitchfork",
        "insidepitchfork",
    ];

    const MIN: f64 = 60_000.0;

    /// Minutes and price: pixel x = minutes, pixel y = (200 - price) * 4.
    fn pt(minutes: f64, price: f64) -> DrawingPoint {
        DrawingPoint {
            time: minutes * MIN,
            price,
        }
    }

    fn meta(key: &str) -> DrawingTypeMeta {
        metas()
            .into_iter()
            .find(|m| m.type_key == key)
            .expect("registered")
    }

    fn create(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    fn proj() -> LinearProjector {
        LinearProjector::sample()
    }

    fn geometry(d: &dyn Drawing, p: &dyn Projector) -> Option<SegmentGeometry> {
        let any = d.as_any();
        if let Some(c) = any.downcast_ref::<ParallelChannel>() {
            c.geometry(p)
        } else if let Some(c) = any.downcast_ref::<DisjointChannel>() {
            c.geometry(p)
        } else if let Some(c) = any.downcast_ref::<FlatTopBottom>() {
            c.geometry(p)
        } else {
            any.downcast_ref::<Pitchfork>().and_then(|c| c.geometry(p))
        }
    }

    #[test]
    fn every_type_is_registered_with_its_key_and_schema() {
        let keys: Vec<_> = metas().iter().map(|m| m.type_key).collect();
        assert_eq!(keys.len(), KEYS.len());
        for key in KEYS {
            assert!(keys.contains(&key), "{key}");
            let d = create(key, vec![]);
            assert_eq!(d.type_key(), key);
            let s = d.anchor_schema();
            let want = if key == "disjointchannel" { 4 } else { 3 };
            assert_eq!((s.min, s.max), (want, want), "{key}");
            assert_eq!(d.base().style, meta(key).default_style);
            assert!(!meta(key).covers_series);
        }
        assert_eq!(meta("parallelchannel").group, "channels");
        assert_eq!(meta("insidepitchfork").group, "pitchforks");
        assert_eq!(
            meta("parallelchannel").default_style.fill_color.as_deref(),
            Some("#38c0fd1a")
        );
        assert!(meta("pitchfork").default_style.fill_color.is_none());
        let frees = |key| {
            create(key, vec![])
                .anchor_schema()
                .slots
                .iter()
                .map(|s| s.free)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            frees("parallelchannel"),
            [FreeAxis::Both, FreeAxis::Both, FreeAxis::Y]
        );
        assert_eq!(
            frees("flattopbottom"),
            [FreeAxis::Both, FreeAxis::Both, FreeAxis::Y]
        );
        let roles = |key| {
            create(key, vec![])
                .anchor_schema()
                .slots
                .into_iter()
                .map(|s| s.role)
                .collect::<Vec<_>>()
        };
        assert_eq!(roles("pitchfork"), ["pivot", "upper", "lower"]);
        assert_eq!(roles("disjointchannel"), ["top1", "top2", "bot1", "bot2"]);
        assert_eq!(roles("parallelchannel"), ["p1", "p2", "offset"]);
        assert_eq!(roles("flattopbottom"), ["p1", "p2", "flat"]);
    }

    #[test]
    fn schemas_offer_fill_only_to_channels() {
        let has_fill = |key| {
            create(key, vec![])
                .schema()
                .fields
                .iter()
                .any(|f| f.path == "style.fillColor")
        };
        assert!(
            has_fill("parallelchannel") && has_fill("disjointchannel") && has_fill("flattopbottom")
        );
        assert!(!has_fill("pitchfork") && !has_fill("insidepitchfork"));
        assert!(!create("pitchfork", vec![]).schema().text_is_content);
    }

    #[test]
    fn degenerate_anchor_counts_never_panic() {
        let p = proj();
        let mut gone = proj();
        gone.pane_id = "elsewhere".into();
        for key in KEYS {
            for n in 0..4 {
                let anchors: Vec<_> = (0..n)
                    .map(|i| pt(10.0 + 10.0 * i as f64, 100.0 + i as f64))
                    .collect();
                for pr in [&p, &gone] {
                    let d = create(key, anchors.clone());
                    let _ = d.hit_test(20.0, 400.0, pr, 4.0);
                    let _ = d.handle_points(pr);
                    let _ = d.hit_handle(20.0, 400.0, pr, 4.0);
                    let _ = d.bounds(pr);
                    let _ = d.price_range();
                    let _ = d.time_extent();
                    let _ = d.schema();
                    let _ = d.write_props();
                    let _ = d.serialize();
                    let _ = geometry(d.as_ref(), pr);
                }
            }
            let d = create(key, vec![]);
            assert!(d.bounds(&p).is_none() && d.price_range().is_none());
            assert!(!d.hit_test(0.0, 0.0, &p, 5.0));
        }
    }

    #[test]
    fn serialization_round_trips_for_every_type() {
        for key in KEYS {
            let d = create(
                key,
                vec![
                    pt(10.0, 100.0),
                    pt(50.0, 120.0),
                    pt(30.0, 90.0),
                    pt(60.0, 80.0),
                ],
            );
            let doc = d.serialize();
            let again =
                (meta(key).create)(&crate::drawings::drawing::DrawingInit::from(doc.clone()))
                    .serialize();
            assert_eq!(again, doc, "{key}");
            assert_eq!(doc.type_key, key);
            assert!(doc.props.is_none());
        }
    }

    // Parallel channel: baseline minutes 0..50 prices 100..110; midpoint price 105;
    // offset anchor price 115 => parallel line +10.
    fn parallel(c_minutes: f64) -> Box<dyn Drawing> {
        create(
            "parallelchannel",
            vec![pt(0.0, 100.0), pt(50.0, 110.0), pt(c_minutes, 115.0)],
        )
    }

    #[test]
    fn parallel_channel_geometry_and_hits() {
        let p = proj();
        let d = parallel(25.0);
        // baseline px (0,400)-(50,360); parallel px (0,360)-(50,320)
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(
            g.segments,
            vec![[0.0, 400.0, 50.0, 360.0], [0.0, 360.0, 50.0, 320.0]]
        );
        assert_eq!(
            g.fill,
            Some(vec![
                (0.0, 400.0),
                (50.0, 360.0),
                (50.0, 320.0),
                (0.0, 360.0)
            ])
        );
        assert!(d.hit_test(25.0, 380.0, &p, 1.0)); // baseline midpoint
        assert!(d.hit_test(25.0, 340.0, &p, 1.0)); // parallel midpoint
        assert!(d.hit_test(25.0, 360.0, &p, 0.0)); // inside the fill
        assert!(!d.hit_test(25.0, 200.0, &p, 4.0));
        assert!(!d.hit_test(500.0, 360.0, &p, 4.0));
    }

    #[test]
    fn parallel_channel_offset_handle_stays_on_the_midpoint() {
        let p = proj();
        for c_minutes in [25.0, 500.0] {
            let d = parallel(c_minutes);
            assert_eq!(
                d.handle_points(&p),
                vec![(0.0, 400.0), (50.0, 360.0), (25.0, 340.0)]
            );
            assert_eq!(d.hit_handle(25.0, 341.0, &p, 2.0), Some(2));
            assert_eq!(
                d.time_extent(),
                Some(Span {
                    min: 0.0,
                    max: 50.0 * MIN
                })
            );
        }
        let d = parallel(25.0);
        assert_eq!(
            d.bounds(&p),
            Some(Bounds {
                x: 0.0,
                y: 340.0,
                w: 50.0,
                h: 60.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 120.0
            })
        );
    }

    #[test]
    fn parallel_channel_with_two_anchors_has_no_offset() {
        let p = proj();
        let d = create("parallelchannel", vec![pt(0.0, 100.0), pt(50.0, 110.0)]);
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments[0], g.segments[1]);
        assert_eq!(d.handle_points(&p).len(), 2);
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 110.0
            })
        );
        assert!(
            create("parallelchannel", vec![pt(0.0, 1.0)])
                .time_extent()
                .is_none()
        );
    }

    #[test]
    fn disjoint_channel_geometry() {
        let p = proj();
        // top at price 120 (y 320), bottom at price 100 (y 400), minutes 0..50
        let d = create(
            "disjointchannel",
            vec![
                pt(0.0, 120.0),
                pt(50.0, 120.0),
                pt(0.0, 100.0),
                pt(50.0, 100.0),
            ],
        );
        assert!(d.hit_test(25.0, 320.0, &p, 1.0));
        assert!(d.hit_test(25.0, 400.0, &p, 1.0));
        assert!(d.hit_test(25.0, 360.0, &p, 0.0)); // fill
        assert!(!d.hit_test(120.0, 450.0, &p, 4.0));
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 120.0
            })
        );
        assert_eq!(d.handle_points(&p).len(), 4);
        assert_eq!(
            d.bounds(&p),
            Some(Bounds {
                x: 0.0,
                y: 320.0,
                w: 50.0,
                h: 80.0
            })
        );

        // only the top pair placed: one segment, no fill
        let half = create(
            "disjointchannel",
            vec![pt(0.0, 120.0), pt(50.0, 120.0), pt(0.0, 100.0)],
        );
        let g = geometry(half.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments, vec![[0.0, 320.0, 50.0, 320.0]]);
        assert!(g.fill.is_none());
        let one = create("disjointchannel", vec![pt(0.0, 120.0)]);
        assert!(geometry(one.as_ref(), &p).is_none());
    }

    #[test]
    fn disjoint_channel_without_a_fill_color_hits_only_its_lines() {
        let p = proj();
        let mut d = create(
            "disjointchannel",
            vec![
                pt(0.0, 120.0),
                pt(50.0, 120.0),
                pt(0.0, 100.0),
                pt(50.0, 100.0),
            ],
        );
        d.base_mut().style.fill_color = None;
        assert!(!d.hit_test(25.0, 360.0, &p, 0.0));
        d.base_mut().style.fill_color = Some(String::new());
        assert!(!d.hit_test(25.0, 360.0, &p, 0.0));
    }

    // Flat top/bottom: baseline minutes 10..50, prices 80 and 100; flat side at price 120.
    fn flat() -> Box<dyn Drawing> {
        create(
            "flattopbottom",
            vec![pt(10.0, 80.0), pt(50.0, 100.0), pt(30.0, 120.0)],
        )
    }

    #[test]
    fn flat_top_bottom_geometry_and_handles() {
        let p = proj();
        let d = flat();
        // baseline px (10,480)-(50,400); flat y = 320
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(
            g.segments,
            vec![[10.0, 480.0, 50.0, 400.0], [10.0, 320.0, 50.0, 320.0]]
        );
        assert!(d.hit_test(30.0, 360.0, &p, 0.0)); // inside the channel
        assert!(d.hit_test(30.0, 320.0, &p, 1.0)); // flat side
        assert!(!d.hit_test(30.0, 500.0, &p, 2.0)); // below the sloped side
        assert_eq!(
            d.handle_points(&p),
            vec![(10.0, 480.0), (50.0, 400.0), (30.0, 320.0)]
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 80.0,
                max: 120.0
            })
        );
        assert_eq!(
            d.time_extent(),
            Some(Span {
                min: 10.0 * MIN,
                max: 50.0 * MIN
            })
        );

        let baseline = create("flattopbottom", vec![pt(10.0, 80.0), pt(50.0, 100.0)]);
        let g = geometry(baseline.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments.len(), 1);
        assert!(g.fill.is_none());
        assert_eq!(baseline.handle_points(&p).len(), 2);
    }

    // Pivot minutes 10 price 160 -> px (10,160); upper (30,180) -> (30,80); lower (30,140) -> (30,240).
    fn fork_anchors() -> Vec<DrawingPoint> {
        vec![pt(10.0, 160.0), pt(30.0, 180.0), pt(30.0, 140.0)]
    }

    #[test]
    fn andrews_pitchfork_median_and_tines_run_to_the_right_edge() {
        let p = proj();
        // pivot px (10,160), tines (30,80) and (30,240); M = (30,160), horizontal median.
        let d = create("pitchfork", fork_anchors());
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments.len(), 4);
        assert_eq!(g.segments[3], [30.0, 80.0, 30.0, 240.0]);
        assert!(d.hit_test(500.0, 160.0, &p, 2.0));
        assert!(d.hit_test(500.0, 80.0, &p, 2.0));
        assert!(d.hit_test(500.0, 240.0, &p, 2.0));
        assert!(!d.hit_test(500.0, 300.0, &p, 2.0));
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 140.0,
                max: 180.0
            })
        );
        assert_eq!(
            d.handle_points(&p),
            vec![(10.0, 160.0), (30.0, 80.0), (30.0, 240.0)]
        );
    }

    #[test]
    fn schiff_starts_the_median_at_the_price_shifted_origin() {
        let p = proj();
        // S = (10, mean(160,180) = 170 -> y 120)
        let d = create("schiffpitchfork", fork_anchors());
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments.len(), 5);
        assert_eq!(g.segments[4], [10.0, 160.0, 30.0, 80.0]); // pivot to first tine
        assert_eq!(g.segments[0][..2], [10.0, 120.0]);
        assert!(d.hit_test(10.0, 120.0, &p, 1.0));
        assert!(!d.hit_test(20.0, 160.0, &p, 1.0)); // the Andrews median would pass here
        assert!(d.hit_test(20.0, 140.0, &p, 1.0)); // on the shifted median
    }

    #[test]
    fn modified_schiff_starts_the_median_at_the_full_midpoint() {
        let p = proj();
        // S = (20, 170 -> y 120)
        let d = create("modifiedschiffpitchfork", fork_anchors());
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments[0][..2], [20.0, 120.0]);
        assert_eq!(g.segments.len(), 5);
        assert!(d.hit_test(20.0, 120.0, &p, 1.0));
        assert!(!d.hit_test(20.0, 140.0, &p, 1.0));
        assert!(d.hit_test(20.0, 120.0, &p, 0.0));
    }

    #[test]
    fn inside_pitchfork_takes_its_slope_from_p3_minus_the_modified_base() {
        let p = proj();
        // B = (20, 170 -> y 120); M = (30, 160); d = P2 - B = (10, 120).
        let d = create("insidepitchfork", fork_anchors());
        let g = geometry(d.as_ref(), &p).expect("geometry");
        assert_eq!(g.segments.len(), 6);
        assert_eq!(g.segments[4], [20.0, 120.0, 30.0, 240.0]);
        assert_eq!(g.segments[5], [10.0, 160.0, 30.0, 80.0]);
        assert_eq!(g.segments[0][..2], [30.0, 160.0]);
        // the median from M with slope 12 reaches x = 35 at y = 220
        assert!(d.hit_test(35.0, 220.0, &p, 1.0));
        assert!(d.hit_test(20.0, 120.0, &p, 1.0));
        assert!(!d.hit_test(500.0, 20.0, &p, 1.0));
    }

    #[test]
    fn pitchforks_preview_the_pivot_to_cursor_line_while_placing() {
        let p = proj();
        for key in [
            "pitchfork",
            "schiffpitchfork",
            "modifiedschiffpitchfork",
            "insidepitchfork",
        ] {
            let d = create(key, fork_anchors()[..2].to_vec());
            let g = geometry(d.as_ref(), &p).expect("geometry");
            assert_eq!(g.segments, vec![[10.0, 160.0, 30.0, 80.0]], "{key}");
            assert!(g.fill.is_none());
            assert!(d.hit_test(20.0, 120.0, &p, 2.0));
            let one = create(key, fork_anchors()[..1].to_vec());
            assert!(geometry(one.as_ref(), &p).is_none(), "{key}");
        }
    }

    #[test]
    fn a_missing_pane_degrades_to_nothing() {
        let mut gone = proj();
        gone.pane_id = "elsewhere".into();
        for key in KEYS {
            let d = create(
                key,
                vec![pt(0.0, 1.0), pt(1.0, 2.0), pt(2.0, 3.0), pt(3.0, 4.0)],
            );
            assert!(geometry(d.as_ref(), &gone).is_none(), "{key}");
            assert!(d.handle_points(&gone).is_empty());
            assert!(d.bounds(&gone).is_none());
        }
    }
}
