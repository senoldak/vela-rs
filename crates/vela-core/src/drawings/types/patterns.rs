//! Pattern tools: harmonic XABCD / ABCD and the named harmonics, Elliott
//! waves and head & shoulders.
//!
//! The original is an abstract `PatternDrawing` (a fixed-vertex polyline whose
//! anchors are labelled, optionally tinted, optionally annotated with
//! consecutive-leg ratios) with `HarmonicPattern` below it. The subclasses
//! differ only by data (labels, bands), so here ONE struct holds a
//! [`PatternKind`] and each registered type key maps to a kind.

use std::any::Any;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Span,
};
use crate::drawings::geometry::{FreeAxis, Projector};
use crate::drawings::hittest::dist_to_polyline;
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{SettingsSchema, fill_fields, line_fields};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::i18n::{MessageLike, msg};
use crate::model::LineStyle;

/// A closed [min, max] Fibonacci-ratio band (tolerance is baked into the width).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RatioBand {
    pub min: f64,
    pub max: f64,
}

impl RatioBand {
    const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }

    /// Whether `v` is present and inside the band (a NaN is never in band).
    fn contains(self, v: Option<f64>) -> bool {
        v.is_some_and(|v| v >= self.min && v <= self.max)
    }
}

/// Ideal ratio bands for the four harmonic legs (B/X retrace, C/B retrace,
/// D/C extension, D/X completion).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HarmonicRanges {
    /// AB / XA
    pub ab: RatioBand,
    /// BC / AB
    pub bc: RatioBand,
    /// CD / BC
    pub cd: RatioBand,
    /// AD / XA (the non-adjacent completion ratio)
    pub ad: RatioBand,
}

/// A named leg ratio of a harmonic pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leg {
    Ab,
    Bc,
    Cd,
    Ad,
}

/// Cypher: C extends the XA leg.
const XC_OVER_XA: RatioBand = RatioBand::new(1.272, 1.414);
/// Cypher: D retraces the XC line by about 0.786.
const CD_OVER_XC: RatioBand = RatioBand::new(0.74, 0.83);

/// The vertex-triple tint of the harmonic body.
const HARMONIC_FILL: &[[usize; 3]] = &[[0, 1, 2], [2, 3, 4]];

/// Which pattern a [`PatternDrawing`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PatternKind {
    Xabcd,
    Abcd,
    ElliottImpulse,
    ElliottCorrection,
    HeadShoulders,
    Gartley,
    Bat,
    Butterfly,
    Crab,
    Shark,
    Cypher,
}

impl PatternKind {
    /// Every kind, in the original's registration order.
    pub const ALL: [PatternKind; 11] = [
        PatternKind::Xabcd,
        PatternKind::Abcd,
        PatternKind::ElliottImpulse,
        PatternKind::ElliottCorrection,
        PatternKind::HeadShoulders,
        PatternKind::Gartley,
        PatternKind::Bat,
        PatternKind::Butterfly,
        PatternKind::Crab,
        PatternKind::Shark,
        PatternKind::Cypher,
    ];

    /// The registered type key.
    pub fn type_key(self) -> &'static str {
        match self {
            PatternKind::Xabcd => PatternDrawing::XABCD,
            PatternKind::Abcd => PatternDrawing::ABCD,
            PatternKind::ElliottImpulse => PatternDrawing::ELLIOTT_IMPULSE,
            PatternKind::ElliottCorrection => PatternDrawing::ELLIOTT_CORRECTION,
            PatternKind::HeadShoulders => PatternDrawing::HEAD_SHOULDERS,
            PatternKind::Gartley => PatternDrawing::GARTLEY,
            PatternKind::Bat => PatternDrawing::BAT,
            PatternKind::Butterfly => PatternDrawing::BUTTERFLY,
            PatternKind::Crab => PatternDrawing::CRAB,
            PatternKind::Shark => PatternDrawing::SHARK,
            PatternKind::Cypher => PatternDrawing::CYPHER,
        }
    }

    /// The kind registered under `type_key`.
    pub fn from_type_key(type_key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.type_key() == type_key)
    }

    /// Whether this is one of the named harmonics with validated ratio bands.
    pub fn is_harmonic(self) -> bool {
        self.ranges().is_some()
    }

    /// The label drawn at each anchor (empty text means no label). Its
    /// length is the vertex count.
    pub fn vertex_labels(self) -> Vec<MessageLike> {
        match self {
            PatternKind::Abcd => ["A", "B", "C", "D"].map(MessageLike::from).to_vec(),
            PatternKind::ElliottImpulse => {
                ["1", "2", "3", "4", "5"].map(MessageLike::from).to_vec()
            }
            PatternKind::ElliottCorrection => ["A", "B", "C"].map(MessageLike::from).to_vec(),
            PatternKind::HeadShoulders => vec![
                "".into(),
                msg("drawing.headshoulders.leftShoulder", "LS"),
                "".into(),
                msg("drawing.headshoulders.head", "H"),
                "".into(),
                msg("drawing.headshoulders.rightShoulder", "RS"),
                "".into(),
            ],
            _ => ["X", "A", "B", "C", "D"].map(MessageLike::from).to_vec(),
        }
    }

    /// Show consecutive-leg retracement ratios (harmonic patterns).
    pub fn leg_ratios(self) -> bool {
        matches!(self, PatternKind::Xabcd | PatternKind::Abcd) || self.is_harmonic()
    }

    /// Vertex-index triples to tint (the harmonic body), or none.
    pub fn fill_triangles(self) -> &'static [[usize; 3]] {
        if self == PatternKind::Xabcd || self.is_harmonic() {
            HARMONIC_FILL
        } else {
            &[]
        }
    }

    /// Two vertex indices whose line is drawn as an extended neckline (head
    /// & shoulders): the two troughs flanking the head.
    pub fn neckline_indices(self) -> Option<[usize; 2]> {
        (self == PatternKind::HeadShoulders).then_some([2, 4])
    }

    /// The harmonic pattern's name (drawn as a badge), or `None` for a plain pattern.
    pub fn pattern_name(self) -> Option<MessageLike> {
        Some(match self {
            PatternKind::Gartley => msg("drawing.gartley.patternName", "Gartley"),
            PatternKind::Bat => msg("drawing.bat.patternName", "Bat"),
            PatternKind::Butterfly => msg("drawing.butterfly.patternName", "Butterfly"),
            PatternKind::Crab => msg("drawing.crab.patternName", "Crab"),
            PatternKind::Shark => msg("drawing.shark.patternName", "Shark"),
            PatternKind::Cypher => msg("drawing.cypher.patternName", "Cypher"),
            _ => return None,
        })
    }

    /// The pattern's ideal ratio bands; `None` for a plain pattern.
    pub fn ranges(self) -> Option<HarmonicRanges> {
        let b = RatioBand::new;
        Some(match self {
            // Gartley: AB~0.618 XA, CD 1.13-1.618 BC, completion AD~0.786 XA.
            PatternKind::Gartley => HarmonicRanges {
                ab: b(0.55, 0.68),
                bc: b(0.382, 0.886),
                cd: b(1.13, 1.618),
                ad: b(0.74, 0.83),
            },
            // Bat: AB 0.382-0.5 XA (never 0.618), CD 1.618-2.618 BC, AD~0.886 XA (the defining ratio).
            PatternKind::Bat => HarmonicRanges {
                ab: b(0.382, 0.5),
                bc: b(0.382, 0.886),
                cd: b(1.618, 2.618),
                ad: b(0.84, 0.92),
            },
            // Butterfly: AB~0.786 XA, CD 1.618-2.618 BC, AD 1.272-1.618 XA (D extends beyond X).
            PatternKind::Butterfly => HarmonicRanges {
                ab: b(0.74, 0.83),
                bc: b(0.382, 0.886),
                cd: b(1.618, 2.618),
                ad: b(1.272, 1.618),
            },
            // Crab: AB 0.382-0.618 XA, extreme CD 2.618-3.618 BC, AD~1.618 XA.
            PatternKind::Crab => HarmonicRanges {
                ab: b(0.382, 0.618),
                bc: b(0.382, 0.886),
                cd: b(2.618, 3.618),
                ad: b(1.55, 1.69),
            },
            // Shark: AB 0.382-0.618 XA, BC 1.13-1.618 AB, CD 1.618-2.24 BC, AD 0.886-1.13 XA.
            PatternKind::Shark => HarmonicRanges {
                ab: b(0.382, 0.618),
                bc: b(1.13, 1.618),
                cd: b(1.618, 2.24),
                ad: b(0.886, 1.13),
            },
            // Cypher: only AB/XA is a standard consecutive constraint; BC/CD/AD are
            // unused (its validity uses XC/XA and CD/XC instead).
            PatternKind::Cypher => HarmonicRanges {
                ab: b(0.382, 0.618),
                bc: b(0.0, f64::INFINITY),
                cd: b(0.0, f64::INFINITY),
                ad: b(0.0, f64::INFINITY),
            },
            _ => return None,
        })
    }
}

/// What a painter needs to know about a pattern to pick its fill color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatternFillInfo {
    /// The pattern tints triangles (`fillTriangles().length > 0`).
    pub has_fill_triangles: bool,
    /// `valid()`: `None` for a plain pattern or too few points.
    pub valid: Option<bool>,
}

/// The pattern view of a drawing, or `None` for a drawing that is not a pattern.
pub fn pattern_fill_info(d: &dyn Drawing) -> Option<PatternFillInfo> {
    let p = d.as_any().downcast_ref::<PatternDrawing>()?;
    Some(PatternFillInfo {
        has_fill_triangles: !p.fill_triangles().is_empty(),
        valid: p.valid(),
    })
}

/// A labelled fixed-vertex pattern polyline.
#[derive(Clone, Debug)]
pub struct PatternDrawing {
    base: DrawingBase,
    kind: PatternKind,
}

impl PatternDrawing {
    pub const XABCD: &'static str = "xabcd";
    pub const ABCD: &'static str = "abcd";
    pub const ELLIOTT_IMPULSE: &'static str = "elliottimpulse";
    pub const ELLIOTT_CORRECTION: &'static str = "elliottcorrection";
    pub const HEAD_SHOULDERS: &'static str = "headshoulders";
    pub const GARTLEY: &'static str = "gartley";
    pub const BAT: &'static str = "bat";
    pub const BUTTERFLY: &'static str = "butterfly";
    pub const CRAB: &'static str = "crab";
    pub const SHARK: &'static str = "shark";
    pub const CYPHER: &'static str = "cypher";

    pub fn new(kind: PatternKind, init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
            kind,
        }
    }

    pub fn kind(&self) -> PatternKind {
        self.kind
    }

    /// The label drawn at each anchor (empty text means no label).
    pub fn vertex_labels(&self) -> Vec<MessageLike> {
        self.kind.vertex_labels()
    }

    /// Show consecutive-leg retracement ratios.
    pub fn leg_ratios(&self) -> bool {
        self.kind.leg_ratios()
    }

    /// Vertex-index triples to tint, or none.
    pub fn fill_triangles(&self) -> &'static [[usize; 3]] {
        self.kind.fill_triangles()
    }

    /// Two vertex indices whose line is drawn as an extended neckline, or `None`.
    pub fn neckline_indices(&self) -> Option<[usize; 2]> {
        self.kind.neckline_indices()
    }

    /// The harmonic pattern's name (drawn as a badge), or `None` for a plain pattern.
    pub fn pattern_name(&self) -> Option<MessageLike> {
        self.kind.pattern_name()
    }

    /// Consecutive-leg ratio at vertex `i` (at least 2): |leg(i-1,i)| / |leg(i-2,i-1)| in price.
    pub fn ratio_at(&self, i: usize) -> Option<f64> {
        let a = self.base.anchors.get(i.checked_sub(2)?)?;
        let b = self.base.anchors.get(i - 1)?;
        let c = self.base.anchors.get(i)?;
        let prev = (b.price - a.price).abs();
        if prev < 1e-9 {
            None
        } else {
            Some((c.price - b.price).abs() / prev)
        }
    }

    /// A named leg ratio in price space (absolute), or `None` if a leg is degenerate.
    fn leg_value(&self, leg: Leg) -> Option<f64> {
        let [x, a, b, c, d] = <&[_; 5]>::try_from(self.base.anchors.get(..5)?).ok()?;
        let xa = (a.price - x.price).abs();
        let ab = (b.price - a.price).abs();
        let bc = (c.price - b.price).abs();
        match leg {
            Leg::Ab => (xa >= 1e-9).then(|| ab / xa),
            Leg::Bc => (ab >= 1e-9).then(|| bc / ab),
            Leg::Cd => (bc >= 1e-9).then(|| (d.price - c.price).abs() / bc),
            Leg::Ad => (xa >= 1e-9).then(|| (d.price - a.price).abs() / xa),
        }
    }

    /// Cypher's XC/XA: point C is a projection of the XA leg beyond A.
    fn xc_over_xa(&self) -> Option<f64> {
        let x = self.base.anchors.first()?;
        let a = self.base.anchors.get(1)?;
        let c = self.base.anchors.get(3)?;
        let xa = (a.price - x.price).abs();
        (xa >= 1e-9).then(|| (c.price - x.price).abs() / xa)
    }

    /// Cypher's CD/XC: D is a retracement of the XC line.
    fn cd_over_xc(&self) -> Option<f64> {
        let x = self.base.anchors.first()?;
        let c = self.base.anchors.get(3)?;
        let d = self.base.anchors.get(4)?;
        let xc = (c.price - x.price).abs();
        (xc >= 1e-9).then(|| (d.price - c.price).abs() / xc)
    }

    /// Whether the leg ratio shown at vertex `i` is within the pattern's
    /// ideal band (`None` = no rule).
    pub fn ratio_ok(&self, i: usize) -> Option<bool> {
        let r = self.kind.ranges()?;
        if self.kind == PatternKind::Cypher {
            // BC/AB and CD/BC are not Cypher's defining ratios.
            return (i == 2).then(|| r.ab.contains(self.leg_value(Leg::Ab)));
        }
        match i {
            2 => Some(r.ab.contains(self.leg_value(Leg::Ab))),
            3 => Some(r.bc.contains(self.leg_value(Leg::Bc))),
            4 => Some(r.cd.contains(self.leg_value(Leg::Cd))),
            _ => None,
        }
    }

    /// Whether ALL the pattern's defining ratios are satisfied (`None` = not
    /// enough points, or not a harmonic).
    pub fn valid(&self) -> Option<bool> {
        let r = self.kind.ranges()?;
        if self.base.anchors.len() < 5 {
            return None;
        }
        let ab = r.ab.contains(self.leg_value(Leg::Ab));
        if self.kind == PatternKind::Cypher {
            return Some(
                ab && XC_OVER_XA.contains(self.xc_over_xa())
                    && CD_OVER_XC.contains(self.cd_over_xc()),
            );
        }
        Some(
            ab && r.bc.contains(self.leg_value(Leg::Bc))
                && r.cd.contains(self.leg_value(Leg::Cd))
                && r.ad.contains(self.leg_value(Leg::Ad)),
        )
    }
}

impl Drawing for PatternDrawing {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        self.kind.type_key()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        let n = self.kind.vertex_labels().len();
        AnchorSchema {
            min: n,
            max: n,
            slots: (0..n)
                .map(|i| AnchorSlot::new(format!("p{i}"), FreeAxis::Both))
                .collect(),
        }
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchors_px(proj).unwrap_or_default()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let pts = self.handle_points(proj);
        pts.len() >= 2 && dist_to_polyline(px, py, &pts) <= tol
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        Bounds::of_points(&self.handle_points(proj))
    }

    fn price_range(&self) -> Option<Span> {
        self.base.anchor_price_range()
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        if !self.fill_triangles().is_empty() {
            fields.extend(fill_fields());
        }
        SettingsSchema::new(fields)
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    let style = || DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid);
    // Named harmonics share an XABCD-shaped icon; the label names them.
    let harmonic_icon =
        svg24(r#"<path d="M3 17 7 6 12 14 17 5 21 16"/><path d="M3 17 21 16" opacity="0.45"/>"#);
    let entry = |kind: PatternKind,
                 label: MessageLike,
                 icon: String,
                 create: fn(&DrawingInit) -> Box<dyn Drawing>| {
        DrawingTypeMeta::new(kind.type_key(), "patterns", label, icon, style(), create)
    };
    vec![
        entry(
            PatternKind::Xabcd,
            msg("drawing.xabcd.label", "XABCD Pattern"),
            svg24(r#"<path d="M3 18 7 8 11 14 15 6 20 16"/>"#),
            |init| Box::new(PatternDrawing::new(PatternKind::Xabcd, init)),
        ),
        entry(
            PatternKind::Abcd,
            msg("drawing.abcd.label", "ABCD Pattern"),
            svg24(r#"<path d="M4 17 9 7 14 15 20 5"/>"#),
            |init| Box::new(PatternDrawing::new(PatternKind::Abcd, init)),
        ),
        entry(
            PatternKind::ElliottImpulse,
            msg("drawing.elliottimpulse.label", "Elliott Impulse Wave (1-5)"),
            svg24(r#"<path d="M3 19 8 9 12 13 17 5 21 9"/>"#),
            |init| Box::new(PatternDrawing::new(PatternKind::ElliottImpulse, init)),
        ),
        entry(
            PatternKind::ElliottCorrection,
            msg(
                "drawing.elliottcorrection.label",
                "Elliott Correction Wave (ABC)",
            ),
            svg24(r#"<path d="M4 7 11 16 20 9"/>"#),
            |init| Box::new(PatternDrawing::new(PatternKind::ElliottCorrection, init)),
        ),
        entry(
            PatternKind::HeadShoulders,
            msg("drawing.headshoulders.label", "Head & Shoulders"),
            svg24(r#"<path d="M3 18 6 12 9 16 12 5 15 16 18 12 21 18"/>"#),
            |init| Box::new(PatternDrawing::new(PatternKind::HeadShoulders, init)),
        ),
        entry(
            PatternKind::Gartley,
            msg("drawing.gartley.label", "Gartley"),
            harmonic_icon.clone(),
            |init| Box::new(PatternDrawing::new(PatternKind::Gartley, init)),
        ),
        entry(
            PatternKind::Bat,
            msg("drawing.bat.label", "Bat"),
            harmonic_icon.clone(),
            |init| Box::new(PatternDrawing::new(PatternKind::Bat, init)),
        ),
        entry(
            PatternKind::Butterfly,
            msg("drawing.butterfly.label", "Butterfly"),
            harmonic_icon.clone(),
            |init| Box::new(PatternDrawing::new(PatternKind::Butterfly, init)),
        ),
        entry(
            PatternKind::Crab,
            msg("drawing.crab.label", "Crab"),
            harmonic_icon.clone(),
            |init| Box::new(PatternDrawing::new(PatternKind::Crab, init)),
        ),
        entry(
            PatternKind::Shark,
            msg("drawing.shark.label", "Shark"),
            harmonic_icon.clone(),
            |init| Box::new(PatternDrawing::new(PatternKind::Shark, init)),
        ),
        entry(
            PatternKind::Cypher,
            msg("drawing.cypher.label", "Cypher"),
            harmonic_icon,
            |init| Box::new(PatternDrawing::new(PatternKind::Cypher, init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::projector::LinearProjector;

    fn meta(key: &str) -> DrawingTypeMeta {
        metas()
            .into_iter()
            .find(|m| m.type_key == key)
            .expect("registered")
    }

    fn make(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    /// Anchors ten pixels apart (time = i * 10 min) at the given prices.
    fn anchors(prices: &[f64]) -> Vec<DrawingPoint> {
        prices
            .iter()
            .enumerate()
            .map(|(i, p)| DrawingPoint {
                time: i as f64 * 600_000.0,
                price: *p,
            })
            .collect()
    }

    fn pattern(key: &str, prices: &[f64]) -> PatternDrawing {
        let m = meta(key);
        let kind = PatternKind::from_type_key(key).expect("kind");
        let init = DrawingInit::on_pane("price").anchors(anchors(prices));
        let mut d = PatternDrawing::new(kind, &init);
        d.base.style = m.default_style.clone();
        d
    }

    fn exercise(d: &dyn Drawing, proj: &LinearProjector) {
        let _ = d.hit_test(5.0, 5.0, proj, 4.0);
        let _ = d.handle_points(proj);
        let _ = d.hit_handle(5.0, 5.0, proj, 4.0);
        let _ = d.bounds(proj);
        let _ = d.price_range();
        let _ = d.time_extent();
        let _ = d.schema();
        let _ = d.write_props();
        let _ = d.serialize();
        let _ = pattern_fill_info(d);
    }

    const KEYS: [(&str, usize); 11] = [
        ("xabcd", 5),
        ("abcd", 4),
        ("elliottimpulse", 5),
        ("elliottcorrection", 3),
        ("headshoulders", 7),
        ("gartley", 5),
        ("bat", 5),
        ("butterfly", 5),
        ("crab", 5),
        ("shark", 5),
        ("cypher", 5),
    ];

    #[test]
    fn every_key_is_registered_with_its_schema_and_survives_missing_anchors() {
        let proj = LinearProjector::sample();
        assert_eq!(metas().len(), KEYS.len());
        for (key, n) in KEYS {
            let m = meta(key);
            assert_eq!(m.group, "patterns");
            assert_eq!(m.default_style.line_width, 1.0);
            assert_eq!(m.default_style.line_color, DEFAULT_DRAWING_COLOR);
            assert!(!m.covers_series);
            let full: Vec<f64> = (0..n).map(|i| 100.0 + i as f64 * 7.0).collect();
            for count in 0..=n {
                let d = make(key, anchors(&full[..count]));
                assert_eq!(d.type_key(), key);
                let schema = d.anchor_schema();
                assert_eq!((schema.min, schema.max, schema.slots.len()), (n, n, n));
                assert_eq!(schema.slots[0].role, "p0");
                assert_eq!(d.is_complete(), count == n);
                exercise(d.as_ref(), &proj);
            }
            let d = make(key, anchors(&full));
            let ser = d.serialize();
            let again = (m.create)(&DrawingInit::from(ser.clone()));
            assert_eq!(again.serialize(), ser);
            assert_eq!(ser.type_key, key);
            assert!(ser.props.is_none());
        }
    }

    #[test]
    fn type_keys_map_to_kinds_and_back() {
        for kind in PatternKind::ALL {
            assert_eq!(PatternKind::from_type_key(kind.type_key()), Some(kind));
        }
        assert_eq!(PatternKind::from_type_key("trendline"), None);
    }

    #[test]
    fn xabcd_has_labels_fill_triangles_and_leg_ratios() {
        let d = pattern("xabcd", &[150.0, 175.0, 160.0, 180.0, 155.0]);
        let labels: Vec<_> = d.vertex_labels().iter().map(|l| l.source_text()).collect();
        assert_eq!(labels, ["X", "A", "B", "C", "D"]);
        assert!(d.leg_ratios());
        assert_eq!(d.fill_triangles(), &[[0, 1, 2], [2, 3, 4]]);
        // AB/XA = 15/25, BC/AB = 20/15
        assert!((d.ratio_at(2).unwrap() - 0.6).abs() < 1e-9);
        assert!((d.ratio_at(3).unwrap() - 20.0 / 15.0).abs() < 1e-9);
        assert_eq!(d.ratio_at(0), None);
        assert_eq!(d.ratio_at(1), None);
        assert_eq!(d.ratio_at(5), None);
        assert!(d.pattern_name().is_none());
        assert_eq!(d.valid(), None);
        assert_eq!(d.ratio_ok(2), None);
    }

    #[test]
    fn a_degenerate_leg_has_no_ratio() {
        let d = pattern("abcd", &[10.0, 10.0, 20.0, 30.0]);
        assert_eq!(d.ratio_at(2), None);
        assert!((d.ratio_at(3).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn geometry_follows_the_anchor_pixels() {
        let proj = LinearProjector::sample();
        // x = 0, 10, 20, 30, 40; y = (200 - price) * 4
        let d = make("xabcd", anchors(&[150.0, 175.0, 160.0, 180.0, 155.0]));
        assert_eq!(
            d.handle_points(&proj),
            [
                (0.0, 200.0),
                (10.0, 100.0),
                (20.0, 160.0),
                (30.0, 80.0),
                (40.0, 180.0)
            ]
        );
        // on the first leg's midpoint, then well off the polyline
        assert!(d.hit_test(5.0, 150.0, &proj, 1.0));
        assert!(!d.hit_test(5.0, 50.0, &proj, 1.0));
        assert_eq!(d.hit_handle(11.0, 101.0, &proj, 4.0), Some(1));
        assert_eq!(d.hit_handle(5.0, 150.0, &proj, 1.0), None);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 0.0,
                y: 80.0,
                w: 40.0,
                h: 120.0
            })
        );
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 150.0,
                max: 180.0
            })
        );
        assert_eq!(
            d.time_extent(),
            Some(Span {
                min: 0.0,
                max: 2_400_000.0
            })
        );
    }

    #[test]
    fn a_gone_pane_hides_every_handle() {
        let mut proj = LinearProjector::sample();
        proj.pane_id = "other".into();
        let d = make("abcd", anchors(&[1.0, 2.0, 3.0, 4.0]));
        assert!(d.handle_points(&proj).is_empty());
        assert!(!d.hit_test(0.0, 0.0, &proj, 100.0));
        assert_eq!(d.bounds(&proj), None);
    }

    #[test]
    fn schema_offers_fill_only_for_tinted_patterns() {
        let paths = |key: &str| -> Vec<String> {
            make(key, vec![])
                .schema()
                .fields
                .iter()
                .map(|f| f.path.clone())
                .collect()
        };
        let line = ["style.lineColor", "style.lineWidth", "style.lineStyle"];
        assert_eq!(paths("abcd"), line);
        assert_eq!(paths("headshoulders"), line);
        assert_eq!(paths("elliottimpulse"), line);
        for key in [
            "xabcd",
            "gartley",
            "bat",
            "butterfly",
            "crab",
            "shark",
            "cypher",
        ] {
            let p = paths(key);
            assert_eq!(p.len(), 4, "{key}");
            assert_eq!(p[3], "style.fillColor");
        }
    }

    #[test]
    fn elliott_waves_and_head_shoulders_labels() {
        let imp = pattern("elliottimpulse", &[10.0, 20.0, 15.0, 30.0, 25.0]);
        let labels: Vec<_> = imp
            .vertex_labels()
            .iter()
            .map(|l| l.source_text())
            .collect();
        assert_eq!(labels, ["1", "2", "3", "4", "5"]);
        assert!(!imp.leg_ratios());
        assert!(imp.fill_triangles().is_empty());
        let corr = pattern("elliottcorrection", &[30.0, 20.0, 26.0]);
        assert_eq!(corr.anchor_schema().min, 3);
        let hs = pattern("headshoulders", &[40.0, 60.0, 50.0, 70.0, 50.0, 60.0, 40.0]);
        let labels: Vec<_> = hs.vertex_labels().iter().map(|l| l.source_text()).collect();
        assert_eq!(labels, ["", "LS", "", "H", "", "RS", ""]);
        assert!(hs.vertex_labels()[1].is_message());
        assert!(!hs.vertex_labels()[0].is_message());
        assert_eq!(hs.neckline_indices(), Some([2, 4]));
        assert_eq!(imp.neckline_indices(), None);
    }

    #[test]
    fn a_gartley_is_valid_when_all_four_ratios_are_in_band() {
        // X0 A100 B40 C70 D22: AB/XA 0.6, BC/AB 0.5, CD/BC 1.6, AD/XA 0.78
        let g = pattern("gartley", &[0.0, 100.0, 40.0, 70.0, 22.0]);
        assert_eq!(g.pattern_name().unwrap().source_text(), "Gartley");
        assert_eq!(g.valid(), Some(true));
        assert_eq!(g.ratio_ok(2), Some(true));
        assert_eq!(g.ratio_ok(3), Some(true));
        assert_eq!(g.ratio_ok(4), Some(true));
        assert_eq!(g.ratio_ok(5), None);
        assert_eq!(g.ratio_ok(1), None);
        let off = pattern("gartley", &[0.0, 100.0, 10.0, 70.0, 22.0]);
        assert_eq!(off.ratio_ok(2), Some(false));
        assert_eq!(off.valid(), Some(false));
        assert_eq!(pattern("gartley", &[0.0, 100.0, 40.0]).valid(), None);
    }

    #[test]
    fn the_same_shape_is_a_gartley_but_not_a_bat() {
        let shape = [0.0, 100.0, 40.0, 70.0, 22.0];
        assert_eq!(pattern("gartley", &shape).valid(), Some(true));
        assert_eq!(pattern("bat", &shape).valid(), Some(false));
    }

    #[test]
    fn crab_butterfly_and_shark_validate_on_their_own_bands() {
        // Crab CD/BC = 154.8/43 = 3.6, AD/XA = 1.618; Butterfly AD/XA = 1.4, CD/BC = 2.56
        let crab = pattern("crab", &[0.0, 100.0, 50.0, 93.0, -61.8]);
        assert_eq!(crab.valid(), Some(true));
        let fly = pattern("butterfly", &[0.0, 100.0, 21.0, 60.0, -40.0]);
        assert_eq!(fly.valid(), Some(true));
        let shark = pattern("shark", &[0.0, 100.0, 50.0, 100.0, 5.0]);
        // AB/XA 0.5, BC/AB 1.0 (outside the 1.13-1.618 band)
        assert_eq!(shark.ratio_ok(3), Some(false));
        assert_eq!(shark.valid(), Some(false));
    }

    #[test]
    fn cypher_uses_its_non_adjacent_rules() {
        // X0 A100 B50 C130 D28: AB/XA 0.5, XC/XA 1.3, CD/XC 0.785
        let cy = pattern("cypher", &[0.0, 100.0, 50.0, 130.0, 28.0]);
        assert_eq!(cy.pattern_name().unwrap().source_text(), "Cypher");
        assert_eq!(cy.valid(), Some(true));
        assert_eq!(cy.ratio_ok(2), Some(true));
        assert_eq!(cy.ratio_ok(3), None);
        assert_eq!(cy.ratio_ok(4), None);
        let bad = pattern("cypher", &[0.0, 100.0, 50.0, 105.0, 28.0]);
        assert_eq!(bad.valid(), Some(false));
        // too few points for the leg value: the AB rule fails rather than being absent
        assert_eq!(pattern("cypher", &[0.0, 100.0]).ratio_ok(2), Some(false));
    }

    #[test]
    fn a_flat_xa_leg_never_validates() {
        let g = pattern("gartley", &[10.0, 10.0, 40.0, 70.0, 22.0]);
        assert_eq!(g.valid(), Some(false));
        assert_eq!(g.ratio_ok(2), Some(false));
    }

    #[test]
    fn fill_info_describes_patterns_only() {
        let g = make("gartley", anchors(&[0.0, 100.0, 40.0, 70.0, 22.0]));
        assert_eq!(
            pattern_fill_info(g.as_ref()),
            Some(PatternFillInfo {
                has_fill_triangles: true,
                valid: Some(true)
            })
        );
        let x = make("xabcd", anchors(&[0.0, 100.0, 40.0, 70.0, 22.0]));
        assert_eq!(
            pattern_fill_info(x.as_ref()),
            Some(PatternFillInfo {
                has_fill_triangles: true,
                valid: None
            })
        );
        let h = make("abcd", vec![]);
        assert_eq!(
            pattern_fill_info(h.as_ref()),
            Some(PatternFillInfo {
                has_fill_triangles: false,
                valid: None
            })
        );
        let line = crate::drawings::types::lines::TrendLine::new(&DrawingInit::on_pane("price"));
        assert_eq!(pattern_fill_info(&line), None);
    }

    #[test]
    fn settings_edit_the_shared_style() {
        let mut d = make("bat", vec![]);
        let mut patch = serde_json::Map::new();
        patch.insert("style.lineColor".into(), json!("#112233"));
        d.apply_settings(&patch);
        assert_eq!(d.base().style.line_color, "#112233");
    }
}
