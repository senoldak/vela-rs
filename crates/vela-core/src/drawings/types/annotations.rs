//! Annotation tools: pinned text labels, two-anchor callout boxes, fixed-size
//! arrow markers and glyph stamps.
//!
//! The original shares code through four abstract bases (`PinnedLabel`,
//! `CalloutBase`, `ArrowMark`, `GlyphStamp`) whose subclasses differ only by
//! data (a placeholder, a direction, a default glyph). Each base is one struct
//! here with a kind field; every registered type key keeps its own `TYPE`
//! constant and registry entry.

use std::any::Any;

use serde_json::Value;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Props, Span,
};
use crate::drawings::geometry::{DrawingPoint, FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_segment, point_in_box, point_in_polygon};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, FieldOption, SettingsField, SettingsSchema, fill_fields, line_fields,
    text_fields,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle, DrawingText};
use crate::i18n::{MessageLike, msg, source_text};
use crate::model::{BoxTextSize, LineStyle};
use crate::palette::{ACCENT, BEARISH, BULLISH};

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

/// Approximate px font size for a named text size (the painter measures the glyphs exactly).
fn size_px(size: Option<BoxTextSize>) -> f64 {
    match size.unwrap_or(BoxTextSize::Normal) {
        BoxTextSize::Tiny => 10.0,
        BoxTextSize::Small => 12.0,
        BoxTextSize::Normal | BoxTextSize::Auto => 14.0,
        BoxTextSize::Large => 18.0,
        BoxTextSize::Huge => 24.0,
    }
}

/// The widest line's length in UTF-16 units (at least 1) and the line count.
fn text_extent(label: &str) -> (f64, usize) {
    let mut cols = 1;
    let mut lines = 0;
    for line in label.split('\n') {
        cols = cols.max(line.encode_utf16().count());
        lines += 1;
    }
    (cols as f64, lines)
}

/// The label of an auto-price annotation: the first anchor's price, to two decimals.
fn auto_price_label(base: &DrawingBase) -> String {
    match base.anchors.first() {
        Some(a) => crate::util::to_fixed(a.price, 2),
        None => "0.00".to_string(),
    }
}

/// Text fields without the typed-text one (the label is computed, not typed).
fn styling_only_text_fields() -> Vec<SettingsField> {
    text_fields()
        .into_iter()
        .filter(|f| f.path != "text.value")
        .collect()
}

fn line_fill_text_fields() -> Vec<SettingsField> {
    let mut fields = line_fields();
    fields.extend(fill_fields());
    fields.extend(text_fields());
    fields
}

fn single_anchor_price_range(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    Some(Span {
        min: a.price,
        max: a.price,
    })
}

// ───────────────────────── pinned labels ─────────────────────────

/// Which pinned label a [`PinnedLabel`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinnedKind {
    /// Free text.
    Text,
    /// Free text on a small rounded plate.
    Note,
    /// A tag auto-rendering the anchor's price.
    PriceLabel,
}

/// A one-anchor, price-pinned label: the shared tool behind plain Text, a Note
/// plate and a Price Label tag. The anchor geometry, hit-test, handle, bounds
/// and price range are common; the painter renders the precise glyphs.
#[derive(Clone, Debug)]
pub struct PinnedLabel {
    base: DrawingBase,
    kind: PinnedKind,
}

impl PinnedLabel {
    pub const TEXT: &'static str = "text";
    pub const NOTE: &'static str = "note";
    pub const PRICE_LABEL: &'static str = "pricelabel";

    pub fn new(kind: PinnedKind, init: &DrawingInit) -> Self {
        let mut label = Self {
            base: DrawingBase::new(init),
            kind,
        };
        if label.base.text.is_none() {
            match kind {
                // Starts empty on purpose: placing one opens an inline editor whose
                // placeholder invites the text. Large, because a free-floating
                // annotation is meant to be read at a glance.
                PinnedKind::Text => {
                    label.base.text = Some(DrawingText {
                        size: BoxTextSize::Large,
                        ..DrawingText::new("")
                    });
                }
                // No color seed: the creation path fixes a theme-contrast ink.
                PinnedKind::Note => {
                    label.base.text = Some(DrawingText::new(source_text(&label.default_label())));
                }
                PinnedKind::PriceLabel => {}
            }
        }
        label
    }

    pub fn text(init: &DrawingInit) -> Self {
        Self::new(PinnedKind::Text, init)
    }

    pub fn note(init: &DrawingInit) -> Self {
        Self::new(PinnedKind::Note, init)
    }

    pub fn price_label(init: &DrawingInit) -> Self {
        Self::new(PinnedKind::PriceLabel, init)
    }

    pub fn kind(&self) -> PinnedKind {
        self.kind
    }

    /// The placeholder text when no text has been entered yet.
    fn default_label(&self) -> MessageLike {
        match self.kind {
            PinnedKind::Note => msg("drawing.note.placeholder", "Note"),
            PinnedKind::Text | PinnedKind::PriceLabel => msg("drawing.text.placeholder", "Text"),
        }
    }

    /// The string used to size the approximate hit box.
    fn label_text(&self) -> String {
        match self.kind {
            // Sized from the approximate price string; the painter formats it precisely.
            PinnedKind::PriceLabel => auto_price_label(&self.base),
            _ => match &self.base.text {
                Some(t) => t.value.clone(),
                None => source_text(&self.default_label()),
            },
        }
    }

    /// Approximate pixel box of the label.
    pub fn pixel_box(&self, proj: &dyn Projector) -> Option<Bounds> {
        let a = self.base.anchors.first()?;
        let y = self.base.y_of(a.price, proj)?;
        let fs = size_px(self.base.text.as_ref().map(|t| t.size));
        let (cols, lines) = text_extent(&self.label_text());
        Some(Bounds {
            x: proj.x_of(a.time),
            y,
            w: (cols * fs * 0.6).max(12.0) + 8.0,
            h: lines as f64 * fs * 1.4 + 4.0,
        })
    }
}

impl Drawing for PinnedLabel {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        match self.kind {
            PinnedKind::Text => Self::TEXT,
            PinnedKind::Note => Self::NOTE,
            PinnedKind::PriceLabel => Self::PRICE_LABEL,
        }
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p"])
    }

    fn placeholder_text(&self) -> Option<MessageLike> {
        Some(self.default_label())
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, _tol: f64) -> bool {
        self.pixel_box(proj)
            .is_some_and(|b| point_in_box(px, py, b.x, b.y, b.x + b.w, b.y + b.h, 0.0))
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        self.pixel_box(proj)
    }

    fn price_range(&self) -> Option<Span> {
        single_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        match self.kind {
            PinnedKind::Text => SettingsSchema {
                fields: text_fields(),
                text_is_content: true,
            },
            PinnedKind::Note => SettingsSchema {
                fields: line_fill_text_fields(),
                text_is_content: true,
            },
            // The label value is auto (the price): expose only styling.
            PinnedKind::PriceLabel => {
                let mut fields = line_fields();
                fields.extend(fill_fields());
                fields.extend(styling_only_text_fields());
                SettingsSchema::new(fields)
            }
        }
    }
}

// ───────────────────────── callout family ─────────────────────────

/// Which two-anchor pinned-box annotation a [`CalloutTool`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutKind {
    /// A speech bubble.
    Callout,
    /// A leader + box showing the pinned point's auto-formatted price.
    PriceNote,
    /// A rounded balloon, a softer-cornered callout.
    Comment,
    /// A sign plate on a pole rising from a pinned level.
    Signpost,
}

/// Shared rig of the two-anchor pinned-box annotations: a text box (anchor 1)
/// tied to a target point (anchor 0). The geometry, hit-test, the pin-the-target
/// body drag and the schema are common; the kinds differ in placeholder and
/// painter only.
#[derive(Clone, Debug)]
pub struct CalloutTool {
    base: DrawingBase,
    kind: CalloutKind,
}

impl CalloutTool {
    pub const CALLOUT: &'static str = "callout";
    pub const PRICE_NOTE: &'static str = "pricenote";
    pub const COMMENT: &'static str = "comment";
    pub const SIGNPOST: &'static str = "signpost";

    pub fn new(kind: CalloutKind, init: &DrawingInit) -> Self {
        let mut tool = Self {
            base: DrawingBase::new(init),
            kind,
        };
        // No color seed: the creation path fixes a theme-contrast ink.
        if tool.base.text.is_none() {
            tool.base.text = Some(DrawingText::new(source_text(&tool.default_label())));
        }
        tool
    }

    pub fn callout(init: &DrawingInit) -> Self {
        Self::new(CalloutKind::Callout, init)
    }

    pub fn price_note(init: &DrawingInit) -> Self {
        Self::new(CalloutKind::PriceNote, init)
    }

    pub fn comment(init: &DrawingInit) -> Self {
        Self::new(CalloutKind::Comment, init)
    }

    pub fn signpost(init: &DrawingInit) -> Self {
        Self::new(CalloutKind::Signpost, init)
    }

    pub fn kind(&self) -> CalloutKind {
        self.kind
    }

    /// The placeholder text seeded for a fresh annotation.
    fn default_label(&self) -> MessageLike {
        match self.kind {
            CalloutKind::Callout => msg("drawing.callout.placeholder", "Callout"),
            // No free text: the painter renders the pinned price.
            CalloutKind::PriceNote => MessageLike::from(""),
            CalloutKind::Comment => msg("drawing.comment.placeholder", "Comment"),
            CalloutKind::Signpost => msg("drawing.signpost.placeholder", "Signpost"),
        }
    }

    /// The string used to size the approximate hit box.
    fn label_text(&self) -> String {
        match self.kind {
            // Sized from the approximate pinned price; the painter formats it precisely.
            CalloutKind::PriceNote => auto_price_label(&self.base),
            _ => match &self.base.text {
                Some(t) => t.value.clone(),
                None => source_text(&self.default_label()),
            },
        }
    }

    /// Approximate pixel box around the box anchor.
    pub fn pixel_box(&self, proj: &dyn Projector) -> Option<Bounds> {
        let c = self.base.anchors.get(1)?;
        let cy = self.base.y_of(c.price, proj)?;
        let fs = size_px(self.base.text.as_ref().map(|t| t.size));
        let (cols, lines) = text_extent(&self.label_text());
        let w = (cols * fs * 0.6).max(20.0) + 16.0;
        let h = lines as f64 * fs * 1.4 + 12.0;
        Some(Bounds {
            x: proj.x_of(c.time) - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
        })
    }

    /// The target tip and box-center pixels (drives the painter, hit-test and the tip handle).
    pub fn points(&self, proj: &dyn Projector) -> Option<[(f64, f64); 2]> {
        Some([self.base.anchor_px(0, proj)?, self.base.anchor_px(1, proj)?])
    }
}

impl Drawing for CalloutTool {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        match self.kind {
            CalloutKind::Callout => Self::CALLOUT,
            CalloutKind::PriceNote => Self::PRICE_NOTE,
            CalloutKind::Comment => Self::COMMENT,
            CalloutKind::Signpost => Self::SIGNPOST,
        }
    }

    fn placeholder_text(&self) -> Option<MessageLike> {
        let label = self.default_label();
        (!source_text(&label).is_empty()).then_some(label)
    }

    /// Click the target (the pointer), move to position the box, click again to
    /// place it: the box follows the cursor between the two clicks.
    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema {
            min: 2,
            max: 2,
            slots: vec![
                AnchorSlot::new("target", FreeAxis::Both),
                AnchorSlot::new("box", FreeAxis::Both),
            ],
        }
    }

    /// A body drag moves only the box; the pointer tip stays pinned at its target.
    fn translate_body(&self, dt: f64, dp: f64, orig: &[DrawingPoint]) -> Vec<DrawingPoint> {
        match (orig.first(), orig.get(1)) {
            (Some(t), Some(b)) => vec![
                *t,
                DrawingPoint {
                    time: b.time + dt,
                    price: b.price + dp,
                },
            ],
            _ => orig
                .iter()
                .map(|o| DrawingPoint {
                    time: o.time + dt,
                    price: o.price + dp,
                })
                .collect(),
        }
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        if let Some(b) = self.pixel_box(proj)
            && point_in_box(px, py, b.x, b.y, b.x + b.w, b.y + b.h, tol)
        {
            return true; // the box body
        }
        // the tail / leader
        self.points(proj)
            .is_some_and(|p| dist_to_segment(px, py, p[0].0, p[0].1, p[1].0, p[1].1) <= tol)
    }

    /// Only the pointer tip is a draggable handle: the box moves by dragging its
    /// body, so no handle dot sits over the text.
    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.points(proj).map(|p| vec![p[0]]).unwrap_or_default()
    }

    /// Floats the settings popup beside the text box.
    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        self.pixel_box(proj)
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        Span::of([a.price, b.price])
    }

    fn schema(&self) -> SettingsSchema {
        match self.kind {
            // The label is the auto price: expose only styling.
            CalloutKind::PriceNote => {
                let mut fields = line_fields();
                fields.extend(fill_fields());
                fields.extend(styling_only_text_fields());
                SettingsSchema::new(fields)
            }
            _ => SettingsSchema {
                fields: line_fill_text_fields(),
                text_is_content: true,
            },
        }
    }
}

/// Whether a drawing belongs to the callout family (a text box tied to a target
/// point): the original's `d instanceof CalloutBase`.
pub fn is_callout(d: &dyn Drawing) -> bool {
    d.as_any().is::<CalloutTool>()
}

// ───────────────────────── arrow markers ─────────────────────────

/// Pixel glyph dimensions (constant on-screen size: the marker never scales with zoom).
const GLYPH_HEAD: f64 = 11.0;
const GLYPH_HEAD_W: f64 = 9.0;
const GLYPH_STEM_W: f64 = 3.5;
const GLYPH_LEN: f64 = 22.0;

/// Which way an [`ArrowMark`] points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowDir {
    /// Tip on the anchor, body hanging below.
    Up,
    /// Tip on the anchor, body rising above.
    Down,
}

/// A fixed-size directional arrow marker stamped at a single anchor, tip ON the
/// anchor and the body extending away from it. The glyph is a constant-pixel
/// polygon, so it never scales or distorts with zoom.
#[derive(Clone, Debug)]
pub struct ArrowMark {
    base: DrawingBase,
    dir: ArrowDir,
}

impl ArrowMark {
    pub const UP: &'static str = "arrowmarkup";
    pub const DOWN: &'static str = "arrowmarkdown";

    pub fn new(dir: ArrowDir, init: &DrawingInit) -> Self {
        Self {
            base: DrawingBase::new(init),
            dir,
        }
    }

    pub fn up(init: &DrawingInit) -> Self {
        Self::new(ArrowDir::Up, init)
    }

    pub fn down(init: &DrawingInit) -> Self {
        Self::new(ArrowDir::Down, init)
    }

    pub fn dir(&self) -> ArrowDir {
        self.dir
    }

    /// The filled glyph polygon in pixel space, tip at the anchor.
    pub fn glyph_points(&self, proj: &dyn Projector) -> Option<Vec<(f64, f64)>> {
        let (x, y) = self.base.anchor_px(0, proj)?;
        // The base glyph points UP (tip at the origin, body extends down +y).
        let base = [
            (0.0, 0.0),
            (-GLYPH_HEAD_W, GLYPH_HEAD),
            (-GLYPH_STEM_W, GLYPH_HEAD),
            (-GLYPH_STEM_W, GLYPH_LEN),
            (GLYPH_STEM_W, GLYPH_LEN),
            (GLYPH_STEM_W, GLYPH_HEAD),
            (GLYPH_HEAD_W, GLYPH_HEAD),
        ];
        Some(
            base.iter()
                .map(|&(bx, by)| match self.dir {
                    ArrowDir::Up => (x + bx, y + by),
                    ArrowDir::Down => (x + bx, y - by),
                })
                .collect(),
        )
    }
}

impl Drawing for ArrowMark {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        match self.dir {
            ArrowDir::Up => Self::UP,
            ArrowDir::Down => Self::DOWN,
        }
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, _tol: f64) -> bool {
        self.glyph_points(proj)
            .is_some_and(|g| point_in_polygon(px, py, &g))
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        Bounds::of_points(&self.glyph_points(proj)?)
    }

    fn price_range(&self) -> Option<Span> {
        single_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = vec![
            SettingsField::new(
                "style.lineColor",
                msg("drawing.arrowmarkup.lineColor", "Color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Line),
        ];
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }
}

// ───────────────────────── glyph stamps ─────────────────────────

/// The selectable glyph set (monochrome symbols tint with the stamp color).
pub const GLYPH_OPTIONS: [&str; 12] = ["★", "☆", "●", "◆", "▲", "▼", "➤", "⚑", "✚", "✕", "❤", "♦"];

/// Named stamp sizes. Their on-screen px are constant: a stamp never scales with zoom.
pub const STAMP_SIZE_OPTIONS: [&str; 4] = ["small", "normal", "large", "huge"];

fn stamp_px(size: &str) -> f64 {
    match size {
        "small" => 16.0,
        "large" => 32.0,
        "huge" => 44.0,
        _ => 22.0,
    }
}

/// Which stamp a [`GlyphStamp`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampKind {
    Flag,
    Icon,
}

/// A single-anchor glyph stamp: a fixed-pixel unicode symbol drawn at one point
/// (centered on the anchor), never scaling with zoom. The glyph and size are
/// per-instance props chosen from [`GLYPH_OPTIONS`] / [`STAMP_SIZE_OPTIONS`].
#[derive(Clone, Debug)]
pub struct GlyphStamp {
    base: DrawingBase,
    kind: StampKind,
    pub glyph: String,
    pub size: String,
}

impl GlyphStamp {
    pub const FLAG: &'static str = "flagmark";
    pub const ICON: &'static str = "iconstamp";

    pub fn new(kind: StampKind, init: &DrawingInit) -> Self {
        let mut stamp = Self {
            base: DrawingBase::new(init),
            kind,
            glyph: String::new(),
            size: String::new(),
        };
        if let Some(props) = &init.props {
            stamp.read_props(props);
        }
        // An empty value falls back to the default, as the original's falsy check does.
        if stamp.glyph.is_empty() {
            stamp.glyph = stamp.default_glyph().to_string();
        }
        if stamp.size.is_empty() {
            stamp.size = "normal".to_string();
        }
        stamp
    }

    pub fn flag(init: &DrawingInit) -> Self {
        Self::new(StampKind::Flag, init)
    }

    pub fn icon(init: &DrawingInit) -> Self {
        Self::new(StampKind::Icon, init)
    }

    pub fn kind(&self) -> StampKind {
        self.kind
    }

    fn default_glyph(&self) -> &'static str {
        match self.kind {
            StampKind::Flag => "⚑",
            StampKind::Icon => "★",
        }
    }

    /// The on-screen glyph size in px for the current named size.
    pub fn size_px(&self) -> f64 {
        stamp_px(&self.size)
    }

    /// The anchor pixel (the glyph box is `size_px()` square, centered here).
    pub fn center(&self, proj: &dyn Projector) -> Option<(f64, f64)> {
        self.base.anchor_px(0, proj)
    }
}

impl Drawing for GlyphStamp {
    drawing_plumbing!();

    fn type_key(&self) -> &'static str {
        match self.kind {
            StampKind::Flag => Self::FLAG,
            StampKind::Icon => Self::ICON,
        }
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema::free(&["p"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some((cx, cy)) = self.center(proj) else {
            return false;
        };
        let r = self.size_px() / 2.0 + tol;
        (px - cx).abs() <= r && (py - cy).abs() <= r
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.center(proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let (cx, cy) = self.center(proj)?;
        let s = self.size_px();
        Some(Bounds {
            x: cx - s / 2.0,
            y: cy - s / 2.0,
            w: s,
            h: s,
        })
    }

    fn price_range(&self) -> Option<Span> {
        single_anchor_price_range(&self.base)
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = vec![
            SettingsField::new(
                "glyph",
                msg("drawing.flagmark.glyph", "Icon"),
                FieldKind::Select,
            )
            .options(
                GLYPH_OPTIONS
                    .iter()
                    .map(|g| FieldOption::new(*g, *g))
                    .collect(),
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "size",
                msg("drawing.flagmark.size", "Size"),
                FieldKind::Select,
            )
            .options(
                STAMP_SIZE_OPTIONS
                    .iter()
                    .map(|s| FieldOption::new(*s, *s))
                    .collect(),
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "style.lineColor",
                msg("drawing.flagmark.lineColor", "Color"),
                FieldKind::Color,
            )
            .group(FieldGroup::Line),
        ];
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("glyph".into(), Value::String(self.glyph.clone()));
        props.insert("size".into(), Value::String(self.size.clone()));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        if let Some(glyph) = props.get("glyph").and_then(Value::as_str) {
            self.glyph = glyph.to_string();
        }
        if let Some(size) = props.get("size").and_then(Value::as_str) {
            self.size = size.to_string();
        }
    }
}

// ───────────────────────── registry ─────────────────────────

fn plate_style() -> DrawingStyle {
    DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid)
        .fill(format!("{DEFAULT_DRAWING_COLOR}d9"))
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            PinnedLabel::TEXT,
            "annotations",
            msg("drawing.text.label", "Text"),
            svg24(r#"<path d="M5 6h14M12 6v13"/>"#),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid),
            |init| Box::new(PinnedLabel::text(init)),
        ),
        DrawingTypeMeta::new(
            CalloutTool::CALLOUT,
            "annotations",
            msg("drawing.callout.label", "Callout"),
            svg24(
                r#"<path d="M5 4h14a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-6l-5 5v-5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z"/>"#,
            ),
            plate_style(),
            |init| Box::new(CalloutTool::callout(init)),
        ),
        DrawingTypeMeta::new(
            PinnedLabel::NOTE,
            "annotations",
            msg("drawing.note.label", "Note"),
            svg24(
                r#"<path d="M5 4h14a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-7l-4 4v-4H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1Z"/><path d="M8 8.5h8M8 12h5"/>"#,
            ),
            plate_style(),
            |init| Box::new(PinnedLabel::note(init)),
        ),
        DrawingTypeMeta::new(
            CalloutTool::PRICE_NOTE,
            "annotations",
            msg("drawing.pricenote.label", "Price Note"),
            svg24(
                r#"<rect x="10" y="4.5" width="11" height="8" rx="1.5"/><path d="M10 8.5 4 16"/><circle cx="3.5" cy="16.5" r="1.6" fill="currentColor" stroke="none"/>"#,
            ),
            plate_style(),
            |init| Box::new(CalloutTool::price_note(init)),
        ),
        DrawingTypeMeta::new(
            CalloutTool::COMMENT,
            "annotations",
            msg("drawing.comment.label", "Comment"),
            svg24(
                r#"<path d="M5 4.5h14a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-7l-4 4v-4H5a2 2 0 0 1-2-2v-6a2 2 0 0 1 2-2Z"/>"#,
            ),
            plate_style(),
            |init| Box::new(CalloutTool::comment(init)),
        ),
        DrawingTypeMeta::new(
            PinnedLabel::PRICE_LABEL,
            "annotations",
            msg("drawing.pricelabel.label", "Price Label"),
            svg24(
                r#"<path d="M3 12 7 8h12a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H7Z"/><path d="M10.5 12.5h6" stroke-width="1.4"/>"#,
            ),
            plate_style(),
            |init| Box::new(PinnedLabel::price_label(init)),
        ),
        DrawingTypeMeta::new(
            CalloutTool::SIGNPOST,
            "annotations",
            msg("drawing.signpost.label", "Signpost"),
            svg24(
                r#"<path d="M12 11.5V21"/><rect x="5" y="4" width="14" height="7" rx="1"/><path d="M9 7.5h6"/>"#,
            ),
            plate_style(),
            |init| Box::new(CalloutTool::signpost(init)),
        ),
        DrawingTypeMeta::new(
            ArrowMark::UP,
            "shapes",
            msg("drawing.arrowmarkup.label", "Arrow Mark Up"),
            svg24(r#"<path d="M12 5 12 19"/><path d="M6 11 12 5 18 11"/>"#),
            DrawingStyle::line(BULLISH, 1.0, LineStyle::Solid),
            |init| Box::new(ArrowMark::up(init)),
        ),
        DrawingTypeMeta::new(
            ArrowMark::DOWN,
            "shapes",
            msg("drawing.arrowmarkdown.label", "Arrow Mark Down"),
            svg24(r#"<path d="M12 5 12 19"/><path d="M6 13 12 19 18 13"/>"#),
            DrawingStyle::line(BEARISH, 1.0, LineStyle::Solid),
            |init| Box::new(ArrowMark::down(init)),
        ),
        DrawingTypeMeta::new(
            GlyphStamp::FLAG,
            "stamps",
            msg("drawing.flagmark.label", "Flag"),
            svg24(
                r#"<path d="M6 21V4"/><path d="M6 5h11l-2.5 3.5L17 12H6" fill="currentColor" stroke="none"/>"#,
            ),
            DrawingStyle::line(ACCENT, 1.0, LineStyle::Solid),
            |init| Box::new(GlyphStamp::flag(init)),
        ),
        DrawingTypeMeta::new(
            GlyphStamp::ICON,
            "stamps",
            msg("drawing.iconstamp.label", "Icon"),
            svg24(
                r#"<path d="m12 3 2.6 5.8 6.4.6-4.8 4.2 1.4 6.2L12 16.9 6.4 19.8 7.8 13.6 3 9.4l6.4-.6Z"/>"#,
            ),
            DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid),
            |init| Box::new(GlyphStamp::icon(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::drawing::SerializedDrawing;
    use crate::drawings::projector::LinearProjector;

    const KEYS: [&str; 11] = [
        "text",
        "note",
        "pricelabel",
        "callout",
        "pricenote",
        "comment",
        "signpost",
        "arrowmarkup",
        "arrowmarkdown",
        "flagmark",
        "iconstamp",
    ];

    const MIN: f64 = 60_000.0;

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

    fn create_with(key: &str, mut init: DrawingInit) -> Box<dyn Drawing> {
        let m = meta(key);
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    fn create(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        create_with(key, DrawingInit::on_pane("price").anchors(anchors))
    }

    fn patch(v: Value) -> Props {
        v.as_object().cloned().expect("object")
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
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
            let single = !matches!(key, "callout" | "pricenote" | "comment" | "signpost");
            assert_eq!((s.min, s.max), if single { (1, 1) } else { (2, 2) });
            assert_eq!(d.base().style, meta(key).default_style);
        }
        assert_eq!(meta("flagmark").group, "stamps");
        assert_eq!(meta("arrowmarkup").group, "shapes");
        assert_eq!(meta("text").group, "annotations");
        assert_eq!(meta("arrowmarkup").default_style.line_color, BULLISH);
        assert_eq!(
            meta("callout").default_style.fill_color.as_deref(),
            Some("#38c0fdd9")
        );
    }

    #[test]
    fn degenerate_anchor_counts_never_panic() {
        let proj = LinearProjector::sample();
        let mut gone = LinearProjector::sample();
        gone.pane_id = "elsewhere".into();
        for key in KEYS {
            for n in 0..2 {
                let anchors: Vec<_> = (0..n).map(|i| pt(10.0 + i as f64, 100.0)).collect();
                for p in [&proj, &gone] {
                    let d = create(key, anchors.clone());
                    let _ = d.hit_test(10.0, 400.0, p, 4.0);
                    let _ = d.handle_points(p);
                    let _ = d.hit_handle(10.0, 400.0, p, 4.0);
                    let _ = d.bounds(p);
                    let _ = d.price_range();
                    let _ = d.time_extent();
                    let _ = d.schema();
                    let _ = d.write_props();
                    let _ = d.serialize();
                    let _ = d.is_complete();
                    let _ = d.translate_body(1.0, 1.0, &anchors);
                }
                let d = create(key, anchors);
                assert!(d.bounds(&proj).is_some() == (n >= d.anchor_schema().min));
            }
        }
    }

    #[test]
    fn serialization_round_trips_for_every_type() {
        for key in KEYS {
            let d = create(key, vec![pt(10.0, 100.0), pt(20.0, 120.0)]);
            let doc = d.serialize();
            let again = (meta(key).create)(&DrawingInit::from(doc.clone())).serialize();
            assert_eq!(again, doc, "{key}");
            assert_eq!(doc.type_key, key);
        }
    }

    #[test]
    fn text_starts_empty_large_with_a_placeholder() {
        let d = create("text", vec![pt(10.0, 100.0)]);
        let t = d.base().text.as_ref().expect("seeded");
        assert_eq!(t.value, "");
        assert_eq!(t.size, BoxTextSize::Large);
        assert_eq!(
            d.placeholder_text(),
            Some(msg("drawing.text.placeholder", "Text"))
        );
        assert!(d.schema().text_is_content);
        // 1 col at 18px: w = max(12, 10.8) + 8, h = 18 * 1.4 + 4
        let proj = LinearProjector::sample();
        let b = d.bounds(&proj).expect("box");
        assert_eq!((b.x, b.y, b.w), (10.0, 400.0, 20.0));
        close(b.h, 29.2);
        assert!(d.hit_test(15.0, 410.0, &proj, 0.0));
        assert!(!d.hit_test(5.0, 410.0, &proj, 0.0));
        assert_eq!(d.handle_points(&proj), vec![(10.0, 400.0)]);
        assert_eq!(d.hit_handle(12.0, 402.0, &proj, 0.0), Some(0));
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 100.0
            })
        );
    }

    #[test]
    fn note_seeds_its_word_and_keeps_given_text() {
        let d = create("note", vec![pt(10.0, 100.0)]);
        assert_eq!(
            d.base().text.as_ref().map(|t| t.value.as_str()),
            Some("Note")
        );
        assert_eq!(
            d.base().text.as_ref().map(|t| t.size),
            Some(BoxTextSize::Normal)
        );
        let mut init = DrawingInit::on_pane("price");
        init.text = Some(DrawingText::new("kept"));
        let d = create_with("note", init);
        assert_eq!(
            d.base().text.as_ref().map(|t| t.value.as_str()),
            Some("kept")
        );
        // "Note": 4 cols at 14px: w = max(12, 33.6) + 8
        let d = create("note", vec![pt(10.0, 100.0)]);
        let b = d.bounds(&LinearProjector::sample()).expect("box");
        close(b.w, 41.6);
        close(b.h, 14.0 * 1.4 + 4.0);
        let fields: Vec<_> = d.schema().fields.iter().map(|f| f.path.clone()).collect();
        assert!(fields.contains(&"style.fillColor".to_string()));
        assert!(fields.contains(&"text.value".to_string()));
    }

    #[test]
    fn price_label_shows_the_auto_price_without_a_text_value_field() {
        let d = create("pricelabel", vec![pt(10.0, 100.0)]);
        assert!(d.base().text.is_none());
        let schema = d.schema();
        assert!(!schema.text_is_content);
        assert!(schema.fields.iter().all(|f| f.path != "text.value"));
        assert!(schema.fields.iter().any(|f| f.path == "text.color"));
        // "100.00": 6 cols at 14px: w = 50.4 + 8
        let b = d.bounds(&LinearProjector::sample()).expect("box");
        close(b.w, 58.4);
        assert_eq!(
            d.placeholder_text(),
            Some(msg("drawing.text.placeholder", "Text"))
        );
    }

    #[test]
    fn multi_line_text_sizes_the_box_by_lines_and_widest_row() {
        let mut init = DrawingInit::on_pane("price").anchors(vec![pt(10.0, 100.0)]);
        init.text = Some(DrawingText::new("ab\nabcdef\n"));
        let d = create_with("text", init);
        let b = d.bounds(&LinearProjector::sample()).expect("box");
        close(b.w, 6.0 * 14.0 * 0.6 + 8.0);
        close(b.h, 3.0 * 14.0 * 1.4 + 4.0);
    }

    #[test]
    fn callout_geometry_matches_the_box_and_leader() {
        let proj = LinearProjector::sample();
        // target px (0, 400); box center px (10, 320)
        let d = create("callout", vec![pt(0.0, 100.0), pt(10.0, 120.0)]);
        assert_eq!(
            d.base().text.as_ref().map(|t| t.value.as_str()),
            Some("Callout")
        );
        // "Callout": 7 cols: w = 58.8 + 16, h = 14 * 1.4 + 12
        let b = d.bounds(&proj).expect("box");
        close(b.w, 74.8);
        close(b.h, 31.6);
        close(b.x + b.w / 2.0, 10.0);
        close(b.y + b.h / 2.0, 320.0);
        assert!(d.hit_test(10.0, 320.0, &proj, 2.0));
        assert!(d.hit_test(0.0, 400.0, &proj, 2.0)); // the target tip, on the leader
        assert!(d.hit_test(5.0, 360.0, &proj, 2.0)); // the leader midpoint
        assert!(!d.hit_test(30.0, 360.0, &proj, 2.0));
        assert!(!d.hit_test(500.0, 80.0, &proj, 2.0));
        assert_eq!(d.handle_points(&proj), vec![(0.0, 400.0)]);
        assert_eq!(d.hit_handle(1.0, 399.0, &proj, 2.0), Some(0));
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 120.0
            })
        );
        assert!(is_callout(d.as_ref()));
        assert!(!is_callout(create("text", vec![]).as_ref()));
    }

    #[test]
    fn a_callout_body_drag_moves_only_the_box() {
        let d = create("callout", vec![pt(0.0, 100.0), pt(10.0, 120.0)]);
        let orig = d.base().anchors.clone();
        let moved = d.translate_body(2.0 * MIN, -5.0, &orig);
        assert_eq!(moved, vec![pt(0.0, 100.0), pt(12.0, 115.0)]);
        let one = [pt(1.0, 1.0)];
        assert_eq!(d.translate_body(MIN, 1.0, &one), vec![pt(2.0, 2.0)]);
    }

    #[test]
    fn callout_family_placeholders_and_schemas() {
        let text_of = |key| {
            create(key, vec![pt(0.0, 100.0), pt(10.0, 120.0)])
                .base()
                .text
                .as_ref()
                .map(|t| t.value.clone())
        };
        assert_eq!(text_of("comment").as_deref(), Some("Comment"));
        assert_eq!(text_of("signpost").as_deref(), Some("Signpost"));
        assert_eq!(text_of("pricenote").as_deref(), Some(""));
        let pn = create("pricenote", vec![]);
        assert_eq!(pn.placeholder_text(), None);
        assert!(pn.schema().fields.iter().all(|f| f.path != "text.value"));
        assert!(!pn.schema().text_is_content);
        let c = create("comment", vec![]);
        assert_eq!(
            c.placeholder_text(),
            Some(msg("drawing.comment.placeholder", "Comment"))
        );
        assert!(c.schema().text_is_content);
        assert!(is_callout(pn.as_ref()) && is_callout(c.as_ref()));
        // price note sized from "120.00"
        let pn = create("pricenote", vec![pt(0.0, 100.0), pt(10.0, 120.0)]);
        close(
            pn.bounds(&LinearProjector::sample()).expect("box").w,
            6.0 * 14.0 * 0.6 + 16.0,
        );
        let empty = create("pricenote", vec![]);
        assert_eq!(empty.bounds(&LinearProjector::sample()), None);
    }

    #[test]
    fn applying_settings_edits_the_text() {
        let mut d = create("callout", vec![pt(0.0, 100.0), pt(10.0, 120.0)]);
        d.apply_settings(&patch(
            json!({ "text.value": "Buy zone", "style.fillColor": "#101010" }),
        ));
        assert_eq!(
            d.base().text.as_ref().map(|t| t.value.as_str()),
            Some("Buy zone")
        );
        assert_eq!(d.base().style.fill_color.as_deref(), Some("#101010"));
        let doc: SerializedDrawing = d.serialize();
        let round = (meta("callout").create)(&DrawingInit::from(doc.clone()));
        assert_eq!(round.serialize(), doc);
    }

    #[test]
    fn arrow_marks_hang_from_or_rise_to_the_anchor() {
        let proj = LinearProjector::sample();
        let up = create("arrowmarkup", vec![pt(10.0, 100.0)]);
        let g = ArrowMark::up(&DrawingInit::on_pane("price").anchors(vec![pt(10.0, 100.0)]))
            .glyph_points(&proj)
            .expect("glyph");
        assert_eq!(g[0], (10.0, 400.0));
        assert_eq!(g[3], (6.5, 422.0));
        assert_eq!(
            up.bounds(&proj),
            Some(Bounds {
                x: 1.0,
                y: 400.0,
                w: 18.0,
                h: 22.0
            })
        );
        assert!(up.hit_test(10.0, 410.0, &proj, 0.0));
        assert!(!up.hit_test(3.0, 420.0, &proj, 0.0)); // beside the stem
        assert_eq!(up.handle_points(&proj), vec![(10.0, 400.0)]);

        let down = create("arrowmarkdown", vec![pt(10.0, 100.0)]);
        assert_eq!(
            down.bounds(&proj),
            Some(Bounds {
                x: 1.0,
                y: 378.0,
                w: 18.0,
                h: 22.0
            })
        );
        assert!(down.hit_test(10.0, 390.0, &proj, 0.0));
        assert!(!down.hit_test(10.0, 410.0, &proj, 0.0));
        assert_eq!(down.schema().fields[0].path, "style.lineColor");
        assert_eq!(
            down.price_range(),
            Some(Span {
                min: 100.0,
                max: 100.0
            })
        );
    }

    #[test]
    fn stamps_seed_defaults_and_round_trip_props() {
        let proj = LinearProjector::sample();
        let flag = create("flagmark", vec![pt(10.0, 100.0)]);
        let icon = create("iconstamp", vec![pt(10.0, 100.0)]);
        let stamp = |d: &dyn Drawing| {
            d.as_any()
                .downcast_ref::<GlyphStamp>()
                .cloned()
                .expect("stamp")
        };
        assert_eq!(stamp(flag.as_ref()).glyph, "⚑");
        assert_eq!(stamp(icon.as_ref()).glyph, "★");
        assert_eq!(stamp(icon.as_ref()).size, "normal");
        let paths: Vec<_> = icon
            .schema()
            .fields
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert!(paths.contains(&"glyph".to_string()) && paths.contains(&"size".to_string()));
        assert_eq!(
            icon.bounds(&proj),
            Some(Bounds {
                x: -1.0,
                y: 389.0,
                w: 22.0,
                h: 22.0
            })
        );
        assert!(icon.hit_test(21.0, 400.0, &proj, 0.0));
        assert!(!icon.hit_test(22.0, 400.0, &proj, 0.0));
        assert!(icon.hit_test(22.0, 400.0, &proj, 1.0));

        let mut icon = icon;
        icon.apply_settings(&patch(json!({ "glyph": "❤", "size": "huge" })));
        assert_eq!(stamp(icon.as_ref()).size_px(), 44.0);
        let doc = icon.serialize();
        assert_eq!(
            doc.props,
            json!({ "glyph": "❤", "size": "huge" }).as_object().cloned()
        );
        let round = (meta("iconstamp").create)(&DrawingInit::from(doc));
        assert_eq!(stamp(round.as_ref()).glyph, "❤");
    }

    #[test]
    fn stamp_props_are_read_leniently() {
        let props = patch(json!({ "glyph": 5, "size": "" }));
        let init = DrawingInit::on_pane("price").props(props);
        let d = GlyphStamp::icon(&init);
        assert_eq!(d.glyph, "★");
        assert_eq!(d.size, "normal");
        assert_eq!(stamp_px("nonsense"), 22.0);
        assert_eq!(GLYPH_OPTIONS.len(), 12);
        assert_eq!(STAMP_SIZE_OPTIONS, ["small", "normal", "large", "huge"]);
    }
}
