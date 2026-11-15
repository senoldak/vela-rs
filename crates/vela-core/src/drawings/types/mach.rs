//! Geometry tools built from circles: the Dedekind tessellation and the Mach
//! wavefront figures (sonic, supersonic and their Fibonacci-spaced "golden"
//! variants).

use std::any::Any;

use serde_json::Value;

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Props, Span,
};
use crate::drawings::geometry::{FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_segment, point_in_box};
use crate::drawings::level_palette::{FibLevel, LEVEL_CYCLE, cycle_levels, fib_levels_of};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, SettingsField, SettingsSchema, line_fields, text_fields,
};
use crate::drawings::style::{DEFAULT_DRAWING_COLOR, DrawingStyle};
use crate::i18n::msg;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::LineStyle;

/// A number as the JSON a JS engine would write: integral values stay integers.
fn num(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        Value::from(v as i64)
    } else {
        Value::from(v)
    }
}

/// A finite JSON number, as `typeof x === 'number' && Number.isFinite(x)`.
fn finite_number(props: &Props, key: &str) -> Option<f64> {
    props.get(key)?.as_f64().filter(|v| v.is_finite())
}

// ── Dedekind tessellation ──────────────────────────────────────────────────

/// Allowed max-curvature presets shown in the settings dropdown.
pub const DEDEKIND_CURVATURE_OPTIONS: [u32; 8] = [4, 8, 12, 16, 24, 32, 48, 64];

const DEFAULT_MAX_CURVATURE: f64 = 24.0;
/// Semicircles smaller than this many pixels are culled.
const MIN_ARC_PX: f64 = 0.75;

/// Whether k/n is the center of a Dedekind circle of curvature n (radius 1/n),
/// per Kocik's algebraic characterization (arXiv:1912.05768).
pub fn is_dedekind_center(k: i64, n: i64) -> bool {
    if n <= 0 {
        return false;
    }
    let Some(k2m1) = k.checked_mul(k).map(|v| v - 1) else {
        return false;
    };
    if n % 2 == 1 {
        return k2m1 % n == 0;
    }
    if n % 8 == 0 {
        if k2m1 % n != 0 {
            return false;
        }
        // The quotient must be odd.
        return (k2m1 / n) % 2 != 0;
    }
    false
}

/// Numerators k in [0, n) whose circles of curvature n sit in the unit interval [0, 1).
pub fn dedekind_centers_in_unit(n: i64) -> Vec<i64> {
    (0..n).filter(|&k| is_dedekind_center(k, n)).collect()
}

/// Pixel-space layout of the placed box and the hyperbolic-plane scale used inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DedekindBox {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bot: f64,
    /// Pixels per hyperbolic unit, chosen so the unit semicircle (r = 1) reaches the box top.
    pub unit_px: f64,
    /// Real-axis start (math) mapped to `left`.
    pub x0: f64,
    /// Real-axis span covered by the box width.
    pub real_span: f64,
}

/// One stroked curve of the tessellation, in media pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DedekindGeom {
    Arc { cx: f64, cy: f64, r: f64 },
    VLine { x: f64, y0: f64, y1: f64 },
}

/// Dedekind tessellation: the modular-group tiling of the upper half-plane,
/// drawn as the Dedekind circles (semicircles on the real axis plus vertical
/// geodesics at half-integers).
///
/// Two corners define a time x price box: the bottom edge is the real axis and
/// the box is the visible domain (geometry is clipped to it). Density is
/// controlled by `max_curvature`.
#[derive(Clone, Debug)]
pub struct DedekindTessellation {
    base: DrawingBase,
    /// Max circle curvature n (denominators 1..n). Higher means a denser tessellation.
    pub max_curvature: f64,
}

impl DedekindTessellation {
    pub const TYPE: &'static str = "dedekind";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            max_curvature: DEFAULT_MAX_CURVATURE,
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// Pixel box + isotropic hyperbolic scale (`None` until both corners resolve).
    pub fn layout(&self, proj: &dyn Projector) -> Option<DedekindBox> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        let ya = self.base.y_of(a.price, proj)?;
        let yb = self.base.y_of(b.price, proj)?;
        let left = js_min(proj.x_of(a.time), proj.x_of(b.time));
        let right = js_max(proj.x_of(a.time), proj.x_of(b.time));
        let top = js_min(ya, yb);
        let bot = js_max(ya, yb);
        let w = right - left;
        let h = bot - top;
        if w < 1.0 || h < 1.0 {
            return None;
        }
        // The unit semicircle (r = 1) reaches the top of the box; the real span
        // follows the aspect ratio.
        let unit_px = h;
        let real_span = w / unit_px;
        Some(DedekindBox {
            left,
            right,
            top,
            bot,
            unit_px,
            x0: 0.0,
            real_span,
        })
    }

    /// Semicircles + vertical geodesics in media pixels, already culled to the
    /// box. Verticals are at every half-integer (Kocik case B); arcs use
    /// curvatures 1..max_curvature.
    pub fn geodesics(&self, proj: &dyn Projector) -> Option<Vec<DedekindGeom>> {
        let DedekindBox {
            left,
            top,
            bot,
            unit_px,
            x0,
            real_span,
            ..
        } = self.layout(proj)?;
        let max_n = js_max(1.0, js_min(64.0, js_round(self.max_curvature)));
        // Padded so edge arcs are not clipped away early.
        let x_min = x0 - 1.0 / max_n;
        let x_max = x0 + real_span + 1.0 / max_n;
        let right = left + real_span * unit_px;
        let mut out = Vec::new();

        // Vertical geodesics at half-integers (x = k/2, k odd).
        let k0 = (x_min * 2.0).floor() as i64;
        let k1 = (x_max * 2.0).ceil() as i64;
        for k in k0..=k1 {
            if k % 2 == 0 {
                continue;
            }
            let x = left + (k as f64 / 2.0 - x0) * unit_px;
            if x < left - 0.5 || x > right + 0.5 {
                continue;
            }
            out.push(DedekindGeom::VLine {
                x,
                y0: bot,
                y1: top,
            });
        }

        // Dedekind semicircles: centers k/n + Z, radius 1/n.
        let mut n = 1_i64;
        while n as f64 <= max_n {
            let r = 1.0 / n as f64;
            let r_px = r * unit_px;
            if r_px >= MIN_ARC_PX {
                let ks = dedekind_centers_in_unit(n);
                if !ks.is_empty() {
                    let t_lo = x_min.floor() as i64 - 1;
                    let t_hi = x_max.ceil() as i64 + 1;
                    for t in t_lo..=t_hi {
                        for &k in &ks {
                            let c = k as f64 / n as f64 + t as f64;
                            // Skip if the diameter misses the visible real span entirely.
                            if c + r < x_min || c - r > x_max {
                                continue;
                            }
                            let cx = left + (c - x0) * unit_px;
                            if cx + r_px < left - 1.0 || cx - r_px > right + 1.0 {
                                continue;
                            }
                            out.push(DedekindGeom::Arc {
                                cx,
                                cy: bot,
                                r: r_px,
                            });
                        }
                    }
                }
            }
            n += 1;
        }
        Some(out)
    }
}

impl Drawing for DedekindTessellation {
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
        AnchorSchema::free(&["c1", "c2"])
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.layout(proj)
            .is_some_and(|b| point_in_box(px, py, b.left, b.top, b.right, b.bot, tol))
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        self.layout(proj)
            .map(|b| vec![(b.left, b.top), (b.right, b.bot)])
            .unwrap_or_default()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let b = self.layout(proj)?;
        Some(Bounds {
            x: b.left,
            y: b.top,
            w: b.right - b.left,
            h: b.bot - b.top,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        Span::of([a.price, b.price])
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        fields.push(
            SettingsField::new(
                "maxCurvature",
                msg("drawing.dedekind.maxCurvature", "Max curvature"),
                FieldKind::Number,
            )
            .range(4.0, 64.0, 4.0)
            .group(FieldGroup::Behavior),
        );
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("maxCurvature".to_string(), num(self.max_curvature));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        if let Some(v) = finite_number(props, "maxCurvature") {
            self.max_curvature = js_max(1.0, js_min(64.0, js_round(v)));
        }
    }

    /// The original assigns the raw number; clamping happens where it is used.
    fn apply_setting(&mut self, path: &str, value: &Value) {
        if path == "maxCurvature" {
            if let Some(v) = value.as_f64() {
                self.max_curvature = v;
            }
            return;
        }
        crate::drawings::drawing::apply_setting_default(self, path, value);
    }
}

// ── Mach figures ───────────────────────────────────────────────────────────

/// Wave-count presets for the linear Mach construction.
pub const MACH_WAVE_COUNT_OPTIONS: [u32; 7] = [3, 4, 5, 6, 8, 10, 12];

/// Mach-number presets for the supersonic cone (M > 1).
pub const MACH_NUMBER_OPTIONS: [f64; 6] = [1.5, 2.0, 2.5, 3.0, 4.0, 5.0];

const DEFAULT_WAVES: f64 = 6.0;
const DEFAULT_MACH: f64 = 2.0;

/// Default linear circle steps 1..12 (ratio = radius / R); the first
/// `enabled_count` are on.
pub fn linear_mach_levels(enabled_count: usize) -> Vec<FibLevel> {
    cycle_levels(LEVEL_CYCLE.len(), enabled_count)
}

/// Fibonacci radius steps for Golden Mach tools (multiples of the user-drawn
/// first circle). Colors follow the conventional fib palette.
pub fn golden_mach_levels() -> Vec<FibLevel> {
    fib_levels_of(&[
        0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.618, 2.618, 4.236, 6.854, 11.09,
    ])
}

/// A level from a props entry: needs a numeric ratio and a string color.
fn sanitize_level(v: &Value) -> Option<FibLevel> {
    let o = v.as_object()?;
    let ratio = o.get("ratio")?.as_f64()?;
    let color = o.get("color")?.as_str()?.to_string();
    Some(FibLevel {
        ratio,
        color,
        enabled: o.get("enabled") != Some(&Value::Bool(false)),
        label: o
            .get("label")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .map(str::to_string),
    })
}

/// One wavefront circle, in media pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct MachCircle {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
    pub color: String,
    /// Radius / R, painted beside the circle.
    pub ratio: f64,
}

/// Pixel geometry of a Mach figure.
#[derive(Clone, Debug, PartialEq)]
pub struct MachGeom {
    /// Pixel midpoint of the user-drawn diameter (center of the first/newest circle).
    pub c0x: f64,
    pub c0y: f64,
    /// Pixel radius of the first circle (half the drawn diameter).
    pub r: f64,
    /// Unit expansion direction (first click to second click along the diameter).
    pub fx: f64,
    pub fy: f64,
    /// Shock origin / nose (on the first-click side of the first circle).
    pub nose_x: f64,
    pub nose_y: f64,
    pub circles: Vec<MachCircle>,
    /// Envelope rays from the nose, each `[x1, y1, x2, y2]`. Sonic: one
    /// shock-front segment (perpendicular wall). Supersonic: two Mach-cone
    /// generators.
    pub rays: Vec<[f64; 4]>,
}

/// The four Mach tools: they differ only in default levels, Mach number and
/// settings schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MachKind {
    Sonic,
    Supersonic,
    GoldenSonic,
    GoldenSupersonic,
}

impl MachKind {
    pub fn type_key(self) -> &'static str {
        match self {
            MachKind::Sonic => MachFigure::SONIC,
            MachKind::Supersonic => MachFigure::SUPERSONIC,
            MachKind::GoldenSonic => MachFigure::GOLDEN_SONIC,
            MachKind::GoldenSupersonic => MachFigure::GOLDEN_SUPERSONIC,
        }
    }

    /// Whether the cone has a user-set Mach number (M > 1).
    fn has_mach(self) -> bool {
        matches!(self, MachKind::Supersonic | MachKind::GoldenSupersonic)
    }

    /// Linear Sonic/Supersonic sync `wave_count` to the enabled levels; the
    /// golden tools leave the enabled fib levels to the gear panel.
    fn syncs_wave_count(self) -> bool {
        matches!(self, MachKind::Sonic | MachKind::Supersonic)
    }

    fn default_levels(self) -> Vec<FibLevel> {
        match self {
            MachKind::Sonic | MachKind::Supersonic => linear_mach_levels(DEFAULT_WAVES as usize),
            MachKind::GoldenSonic | MachKind::GoldenSupersonic => golden_mach_levels(),
        }
    }
}

/// Shared schema field: toggle on-chart ratio labels beside each circle.
pub fn show_ratios_field() -> SettingsField {
    SettingsField::new(
        "showRatios",
        msg("drawing.sonic.showRatios", "Show ratios"),
        FieldKind::Boolean,
    )
    .group(FieldGroup::Behavior)
}

/// Mach wavefront construction (Huygens circles along a flight path).
///
/// Two anchors are the diameter endpoints of the first (newest) circle.
/// Placement order sets the expansion direction: first click to second click.
/// Each enabled level is a circle with radius `ratio * R`; the gear panel
/// edits per-circle color / enabled.
#[derive(Clone, Debug)]
pub struct MachFigure {
    base: DrawingBase,
    kind: MachKind,
    /// Number of wavefront circles (linear tools); synced to `levels[].enabled`.
    pub wave_count: f64,
    /// Per-circle config (ratio / color / enabled); drives the settings gear.
    pub levels: Vec<FibLevel>,
    /// When true, paint each circle's ratio beside it.
    pub show_ratios: bool,
    /// Mach number of the supersonic cones (unused by the sonic tools).
    pub mach: f64,
}

impl MachFigure {
    pub const SONIC: &'static str = "sonic";
    pub const SUPERSONIC: &'static str = "supersonic";
    pub const GOLDEN_SONIC: &'static str = "goldensonic";
    pub const GOLDEN_SUPERSONIC: &'static str = "goldensupersonic";

    pub fn new(kind: MachKind, init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            kind,
            wave_count: DEFAULT_WAVES,
            levels: Vec::new(),
            show_ratios: true,
            mach: DEFAULT_MACH,
        };
        let levels_from_props = init
            .props
            .as_ref()
            .and_then(|p| p.get("levels"))
            .is_some_and(Value::is_array);
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        if d.levels.is_empty() {
            d.levels = kind.default_levels();
        }
        // The Waves control owns how many circles are on, unless props already set levels.
        if !levels_from_props && kind.syncs_wave_count() {
            d.sync_levels_to_wave_count();
        }
        d
    }

    pub fn sonic(init: &DrawingInit) -> Self {
        Self::new(MachKind::Sonic, init)
    }

    pub fn supersonic(init: &DrawingInit) -> Self {
        Self::new(MachKind::Supersonic, init)
    }

    pub fn golden_sonic(init: &DrawingInit) -> Self {
        Self::new(MachKind::GoldenSonic, init)
    }

    pub fn golden_supersonic(init: &DrawingInit) -> Self {
        Self::new(MachKind::GoldenSupersonic, init)
    }

    pub fn kind(&self) -> MachKind {
        self.kind
    }

    /// Clamp `wave_count` to the level count and enable the first N levels.
    fn sync_levels_to_wave_count(&mut self) {
        let n = js_max(
            1.0,
            js_min(self.levels.len() as f64, js_round(self.wave_count)),
        );
        self.wave_count = n;
        for (i, level) in self.levels.iter_mut().enumerate() {
            level.enabled = (i as f64) < n;
        }
    }

    /// 1 for the sonic tools; above 1 for the supersonic ones.
    pub fn mach_number(&self) -> f64 {
        if self.kind.has_mach() {
            js_max(1.01, self.mach)
        } else {
            1.0
        }
    }

    /// Pixel geometry for the current anchors, or `None` until both ends resolve.
    pub fn geom(&self, proj: &dyn Projector) -> Option<MachGeom> {
        let (ax, ay) = self.base.anchor_px(0, proj)?;
        let (bx, by) = self.base.anchor_px(1, proj)?;
        let c0x = (ax + bx) / 2.0;
        let c0y = (ay + by) / 2.0;
        let big_r = (bx - ax).hypot(by - ay) / 2.0;
        if big_r < 1.0 {
            return None;
        }

        // Expansion follows the diameter stroke (first click to second click):
        // circles grow toward d2.
        let mut fx = bx - ax;
        let mut fy = by - ay;
        let flen = fx.hypot(fy);
        if flen < 1e-9 {
            fx = 1.0;
            fy = 0.0;
        } else {
            fx /= flen;
            fy /= flen;
        }

        let mut active: Vec<&FibLevel> = self
            .levels
            .iter()
            .filter(|l| l.enabled && l.ratio > 0.0)
            .collect();
        active.sort_by(|p, q| {
            p.ratio
                .partial_cmp(&q.ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if active.is_empty() {
            return None;
        }

        let m = js_max(1.0, self.mach_number());
        let circles: Vec<MachCircle> = active
            .iter()
            .map(|lv| {
                let r = lv.ratio * big_r;
                MachCircle {
                    cx: c0x + m * (r - big_r) * fx,
                    cy: c0y + m * (r - big_r) * fy,
                    r,
                    color: lv.color.clone(),
                    ratio: lv.ratio,
                }
            })
            .collect();
        // The nose / shock origin sits on the d1 side of the first circle (ratio 1, r = R).
        let nose_x = c0x - m * big_r * fx;
        let nose_y = c0y - m * big_r * fy;

        let max_r = circles.last().map_or(0.0, |c| c.r);
        let ray_len = m * max_r + 2.0 * big_r;
        let mut rays = Vec::new();
        if m <= 1.0 + 1e-9 {
            let px = -fy;
            let py = fx;
            rays.push([
                nose_x - px * ray_len,
                nose_y - py * ray_len,
                nose_x + px * ray_len,
                nose_y + py * ray_len,
            ]);
        } else {
            let mu = (1.0 / m).asin();
            let (sin, cos) = mu.sin_cos();
            let rot = |dx: f64, dy: f64, s: f64| (dx * cos - dy * s * sin, dx * s * sin + dy * cos);
            let (d1x, d1y) = rot(fx, fy, 1.0);
            let (d2x, d2y) = rot(fx, fy, -1.0);
            rays.push([
                nose_x,
                nose_y,
                nose_x + d1x * ray_len,
                nose_y + d1y * ray_len,
            ]);
            rays.push([
                nose_x,
                nose_y,
                nose_x + d2x * ray_len,
                nose_y + d2y * ray_len,
            ]);
        }

        Some(MachGeom {
            c0x,
            c0y,
            r: big_r,
            fx,
            fy,
            nose_x,
            nose_y,
            circles,
            rays,
        })
    }
}

impl Drawing for MachFigure {
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

    fn editable_levels(&self) -> Option<Vec<FibLevel>> {
        Some(self.levels.clone())
    }

    fn anchor_schema(&self) -> AnchorSchema {
        AnchorSchema {
            min: 2,
            max: 2,
            slots: vec![
                AnchorSlot::new("d1", FreeAxis::Both),
                AnchorSlot::new("d2", FreeAxis::Both),
            ],
        }
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(g) = self.geom(proj) else {
            return false;
        };
        if g.circles
            .iter()
            .any(|c| ((px - c.cx).hypot(py - c.cy) - c.r).abs() <= tol)
        {
            return true;
        }
        g.rays
            .iter()
            .any(|r| dist_to_segment(px, py, r[0], r[1], r[2], r[3]) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        match (self.base.anchor_px(0, proj), self.base.anchor_px(1, proj)) {
            (Some(a), Some(b)) => vec![a, b],
            _ => Vec::new(),
        }
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let g = self.geom(proj)?;
        let mut lo_x = g.nose_x;
        let mut hi_x = g.nose_x;
        let mut lo_y = g.nose_y;
        let mut hi_y = g.nose_y;
        for c in &g.circles {
            lo_x = js_min(lo_x, c.cx - c.r);
            hi_x = js_max(hi_x, c.cx + c.r);
            lo_y = js_min(lo_y, c.cy - c.r);
            hi_y = js_max(hi_y, c.cy + c.r);
        }
        for r in &g.rays {
            lo_x = js_min(js_min(lo_x, r[0]), r[2]);
            hi_x = js_max(js_max(hi_x, r[0]), r[2]);
            lo_y = js_min(js_min(lo_y, r[1]), r[3]);
            hi_y = js_max(js_max(hi_y, r[1]), r[3]);
        }
        Some(Bounds {
            x: lo_x,
            y: lo_y,
            w: hi_x - lo_x,
            h: hi_y - lo_y,
        })
    }

    fn price_range(&self) -> Option<Span> {
        let a = self.base.anchors.first()?;
        let b = self.base.anchors.get(1)?;
        Span::of([a.price, b.price])
    }

    fn schema(&self) -> SettingsSchema {
        let mut fields = line_fields();
        let wave_count = |id: &'static str| {
            SettingsField::new("waveCount", msg(id, "Waves"), FieldKind::Number)
                .range(3.0, 12.0, 1.0)
                .group(FieldGroup::Behavior)
        };
        let mach = |id: &'static str| {
            SettingsField::new("mach", msg(id, "Mach number"), FieldKind::Number)
                .range(1.5, 5.0, 0.5)
                .group(FieldGroup::Behavior)
        };
        match self.kind {
            MachKind::Sonic => fields.push(wave_count("drawing.sonic.waveCount")),
            MachKind::Supersonic => {
                fields.push(mach("drawing.supersonic.mach"));
                fields.push(wave_count("drawing.supersonic.waveCount"));
            }
            // No wave-count dropdown: density is the enabled fib levels in the gear panel.
            MachKind::GoldenSonic => {}
            MachKind::GoldenSupersonic => fields.push(mach("drawing.goldensupersonic.mach")),
        }
        fields.push(show_ratios_field());
        fields.extend(text_fields());
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("waveCount".to_string(), num(self.wave_count));
        props.insert("showRatios".to_string(), Value::Bool(self.show_ratios));
        let levels = self
            .levels
            .iter()
            .map(|l| {
                let mut o = Props::new();
                o.insert("ratio".to_string(), num(l.ratio));
                o.insert("color".to_string(), Value::String(l.color.clone()));
                o.insert("enabled".to_string(), Value::Bool(l.enabled));
                if let Some(label) = &l.label {
                    o.insert("label".to_string(), Value::String(label.clone()));
                }
                Value::Object(o)
            })
            .collect();
        props.insert("levels".to_string(), Value::Array(levels));
        if self.kind.has_mach() {
            props.insert("mach".to_string(), num(self.mach));
        }
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        if let Some(v) = finite_number(props, "waveCount") {
            self.wave_count = js_max(2.0, js_min(12.0, js_round(v)));
        }
        if let Some(v) = props.get("showRatios").and_then(Value::as_bool) {
            self.show_ratios = v;
        }
        if let Some(Value::Array(items)) = props.get("levels") {
            let parsed: Vec<FibLevel> = items.iter().filter_map(sanitize_level).collect();
            if !parsed.is_empty() {
                self.levels = parsed;
            }
        }
        if let (true, Some(v)) = (self.kind.has_mach(), finite_number(props, "mach")) {
            self.mach = js_max(1.01, js_min(20.0, v));
        }
    }

    /// The original assigns raw numbers for the Waves and Mach controls; the
    /// Waves value is then synced onto `levels[].enabled`.
    fn apply_setting(&mut self, path: &str, value: &Value) {
        match path {
            "waveCount" => {
                if let Some(v) = value.as_f64() {
                    self.wave_count = v;
                    self.sync_levels_to_wave_count();
                }
            }
            "mach" => {
                if let (true, Some(v)) = (self.kind.has_mach(), value.as_f64()) {
                    self.mach = v;
                }
            }
            _ => crate::drawings::drawing::apply_setting_default(self, path, value),
        }
    }

    /// Syncs the Waves control onto `levels[].enabled` (first N on) after the
    /// whole patch is applied, so it wins over a same-patch `levels.<i>.enabled`.
    fn apply_settings(&mut self, patch: &Props) {
        for (path, value) in patch {
            self.apply_setting(path, value);
        }
        if patch.get("waveCount").is_some_and(Value::is_number) {
            self.sync_levels_to_wave_count();
        }
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    let style = || DrawingStyle::line(DEFAULT_DRAWING_COLOR, 1.0, LineStyle::Solid);
    vec![
        DrawingTypeMeta::new(
            DedekindTessellation::TYPE,
            "geometry",
            msg("drawing.dedekind.label", "Dedekind Tessellation"),
            svg24(
                r#"<path d="M2 20h20"/><path d="M4 20a8 8 0 0 1 16 0" opacity="0.9"/><path d="M6 20a4 4 0 0 1 8 0" opacity="0.75"/><path d="M10 20a2 2 0 0 1 4 0" opacity="0.6"/><path d="M8 20v-6M12 20v-9M16 20v-6" opacity="0.45"/>"#,
            ),
            style(),
            |init| Box::new(DedekindTessellation::new(init)),
        ),
        DrawingTypeMeta::new(
            MachFigure::SONIC,
            "geometry",
            msg("drawing.sonic.label", "Sonic"),
            svg24(
                r#"<circle cx="16" cy="12" r="5" opacity="0.55"/><circle cx="13" cy="12" r="3.5" opacity="0.75"/><circle cx="10.5" cy="12" r="2"/><path d="M8 4v16"/>"#,
            ),
            style(),
            |init| Box::new(MachFigure::sonic(init)),
        ),
        DrawingTypeMeta::new(
            MachFigure::SUPERSONIC,
            "geometry",
            msg("drawing.supersonic.label", "Supersonic"),
            svg24(
                r#"<circle cx="17" cy="12" r="4.5" opacity="0.45"/><circle cx="13" cy="12" r="3" opacity="0.7"/><circle cx="10" cy="12" r="1.6"/><path d="M6 12 16 5M6 12 16 19"/>"#,
            ),
            style(),
            |init| Box::new(MachFigure::supersonic(init)),
        ),
        DrawingTypeMeta::new(
            MachFigure::GOLDEN_SONIC,
            "geometry",
            msg("drawing.goldensonic.label", "Golden Sonic"),
            svg24(
                r#"<circle cx="16" cy="12" r="5" opacity="0.4"/><circle cx="13" cy="12" r="3.2" opacity="0.65"/><circle cx="10.8" cy="12" r="1.8"/><path d="M8 4v16"/><path d="M15 8.5h2M12.5 10h2" opacity="0.5"/>"#,
            ),
            style(),
            |init| Box::new(MachFigure::golden_sonic(init)),
        ),
        DrawingTypeMeta::new(
            MachFigure::GOLDEN_SUPERSONIC,
            "geometry",
            msg("drawing.goldensupersonic.label", "Golden Supersonic"),
            svg24(
                r#"<circle cx="17" cy="12" r="4.5" opacity="0.4"/><circle cx="12.5" cy="12" r="2.8" opacity="0.65"/><circle cx="9.8" cy="12" r="1.4"/><path d="M6 12 16 5M6 12 16 19"/>"#,
            ),
            style(),
            |init| Box::new(MachFigure::golden_supersonic(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::drawing::SerializedDrawing;
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::projector::LinearProjector;

    const MIN: f64 = 60_000.0;
    const KEYS: [&str; 5] = [
        "dedekind",
        "sonic",
        "supersonic",
        "goldensonic",
        "goldensupersonic",
    ];

    fn meta(key: &str) -> DrawingTypeMeta {
        metas().into_iter().find(|m| m.type_key == key).unwrap()
    }

    fn create(key: &str, anchors: Vec<DrawingPoint>, props: Option<Props>) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init = DrawingInit::on_pane("price");
        init.style = Some(m.default_style.to_partial());
        init.anchors = anchors;
        init.props = props;
        (m.create)(&init)
    }

    fn pt(minutes: f64, price: f64) -> DrawingPoint {
        DrawingPoint {
            time: minutes * MIN,
            price,
        }
    }

    fn props(v: Value) -> Option<Props> {
        v.as_object().cloned()
    }

    /// Diameter (100,400) to (300,400) in sample pixels: R = 100, center (200,400).
    fn diameter() -> Vec<DrawingPoint> {
        vec![pt(100.0, 100.0), pt(300.0, 100.0)]
    }

    fn mach(key: &str, p: Option<Props>) -> Box<dyn Drawing> {
        create(key, diameter(), p)
    }

    fn as_mach(d: &dyn Drawing) -> &MachFigure {
        d.as_any().downcast_ref::<MachFigure>().unwrap()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn every_type_is_created_with_its_key_schema_and_default_style() {
        for key in KEYS {
            let m = meta(key);
            assert_eq!(m.group, "geometry");
            assert_eq!(m.default_style.line_width, 1.0);
            let d = create(key, vec![], None);
            assert_eq!(d.type_key(), key);
            assert_eq!(d.base().style.line_color, DEFAULT_DRAWING_COLOR);
            let schema = d.anchor_schema();
            assert_eq!((schema.min, schema.max), (2, 2));
            let roles: Vec<_> = schema.slots.iter().map(|s| s.role.as_str()).collect();
            assert_eq!(
                roles,
                if key == "dedekind" {
                    ["c1", "c2"]
                } else {
                    ["d1", "d2"]
                }
            );
        }
    }

    #[test]
    fn degenerate_anchor_counts_never_panic() {
        let proj = LinearProjector::sample();
        for key in KEYS {
            for (n, anchors) in [
                vec![],
                vec![pt(100.0, 100.0)],
                vec![pt(100.0, 100.0), pt(100.0, 100.0)],
            ]
            .into_iter()
            .enumerate()
            {
                let d = create(key, anchors, None);
                assert!(!d.hit_test(10.0, 10.0, &proj, 4.0));
                assert_eq!(
                    d.handle_points(&proj).is_empty(),
                    n < 2 || key == "dedekind"
                );
                assert_eq!(d.hit_handle(10.0, 10.0, &proj, 4.0), None);
                assert_eq!(d.bounds(&proj), None);
                let _ = d.price_range();
                let _ = d.time_extent();
                assert!(!d.schema().fields.is_empty());
                assert!(d.write_props().is_some());
                let _ = d.serialize();
            }
        }
    }

    #[test]
    fn serialize_round_trips_unchanged() {
        for key in KEYS {
            let d = create(key, diameter(), None);
            let doc = d.serialize();
            let again = (meta(key).create)(&DrawingInit::from(doc.clone())).serialize();
            assert_eq!(again, doc, "{key}");
        }
    }

    #[test]
    fn kocik_centers_match_the_algebraic_characterization() {
        assert_eq!(dedekind_centers_in_unit(1), vec![0]);
        assert_eq!(dedekind_centers_in_unit(3), vec![1, 2]);
        assert_eq!(dedekind_centers_in_unit(8), vec![3, 5]);
        assert_eq!(dedekind_centers_in_unit(72), vec![19, 35, 37, 53]);
        assert!(!is_dedekind_center(1, 2));
        assert!(is_dedekind_center(0, 1));
        assert!(!is_dedekind_center(1, 0));
        assert!(!is_dedekind_center(1, -3));
    }

    fn dedekind(max: f64) -> DedekindTessellation {
        // Pixels: (0,400) and (400,200): a 400 x 200 box, unit_px 200, real span 2.
        let mut init = DrawingInit::on_pane("price");
        init.anchors = vec![pt(0.0, 100.0), pt(400.0, 150.0)];
        init.props = props(json!({ "maxCurvature": max }));
        DedekindTessellation::new(&init)
    }

    #[test]
    fn dedekind_box_handles_bounds_and_hit_test() {
        let proj = LinearProjector::sample();
        let d = dedekind(8.0);
        let b = d.layout(&proj).unwrap();
        assert_eq!(
            (b.left, b.right, b.top, b.bot, b.unit_px, b.x0, b.real_span),
            (0.0, 400.0, 200.0, 400.0, 200.0, 0.0, 2.0)
        );
        assert_eq!(d.handle_points(&proj), vec![(0.0, 200.0), (400.0, 400.0)]);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 0.0,
                y: 200.0,
                w: 400.0,
                h: 200.0
            })
        );
        assert!(d.hit_test(200.0, 300.0, &proj, 4.0));
        assert!(d.hit_test(-3.0, 300.0, &proj, 4.0));
        assert!(!d.hit_test(200.0, 450.0, &proj, 4.0));
        assert_eq!(d.hit_handle(2.0, 201.0, &proj, 4.0), Some(0));
        assert_eq!(d.hit_handle(398.0, 399.0, &proj, 4.0), Some(1));
        assert_eq!(d.hit_handle(200.0, 300.0, &proj, 4.0), None);
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 150.0
            })
        );
        // A box thinner than a pixel has no layout.
        let mut thin = dedekind(8.0);
        thin.base_mut().anchors[1].price = 99.9;
        assert!(thin.layout(&proj).is_none());
        assert!(thin.geodesics(&proj).is_none());
    }

    #[test]
    fn dedekind_geodesics_for_the_unit_curvature() {
        let proj = LinearProjector::sample();
        let g = dedekind(1.0).geodesics(&proj).unwrap();
        let verts: Vec<f64> = g
            .iter()
            .filter_map(|g| match g {
                DedekindGeom::VLine { x, y0, y1 } => {
                    assert_eq!((*y0, *y1), (400.0, 200.0));
                    Some(*x)
                }
                DedekindGeom::Arc { .. } => None,
            })
            .collect();
        assert_eq!(verts, vec![100.0, 300.0]);
        let arcs: Vec<f64> = g
            .iter()
            .filter_map(|g| match g {
                DedekindGeom::Arc { cx, cy, r } => {
                    assert_eq!((*cy, *r), (400.0, 200.0));
                    Some(*cx)
                }
                DedekindGeom::VLine { .. } => None,
            })
            .collect();
        assert_eq!(arcs, vec![-200.0, 0.0, 200.0, 400.0, 600.0]);
        // Verticals come first, then arcs.
        assert!(matches!(g[0], DedekindGeom::VLine { .. }));
        let denser = dedekind(24.0).geodesics(&proj).unwrap();
        let count = |v: &[DedekindGeom]| {
            v.iter()
                .filter(|g| matches!(g, DedekindGeom::Arc { .. }))
                .count()
        };
        assert!(count(&denser) > count(&dedekind(8.0).geodesics(&proj).unwrap()));
        assert!(denser.iter().any(|g| matches!(
            g,
            DedekindGeom::Arc { cx, r, .. } if *r == 200.0 && cx.abs() < 1e-9
        )));
    }

    #[test]
    fn dedekind_props_are_lenient_clamped_and_settable() {
        let mut d = dedekind(32.0);
        assert_eq!(d.write_props(), props(json!({ "maxCurvature": 32 })));
        d.read_props(&props(json!({ "maxCurvature": "x" })).unwrap());
        assert_eq!(d.max_curvature, 32.0);
        d.read_props(&props(json!({ "maxCurvature": 500 })).unwrap());
        assert_eq!(d.max_curvature, 64.0);
        d.read_props(&props(json!({ "maxCurvature": 0.2 })).unwrap());
        assert_eq!(d.max_curvature, 1.0);
        d.apply_settings(&props(json!({ "maxCurvature": 48 })).unwrap());
        assert_eq!(d.max_curvature, 48.0);
        assert_eq!(
            DedekindTessellation::new(&DrawingInit::on_pane("price")).max_curvature,
            24.0
        );
        d.apply_settings(&props(json!({ "style.lineWidth": 3 })).unwrap());
        assert_eq!(d.base().style.line_width, 3.0);
        let doc = d.serialize();
        assert_eq!(doc.type_key, "dedekind");
        assert_eq!(doc.props, props(json!({ "maxCurvature": 48 })));
    }

    #[test]
    fn sonic_sizes_the_first_circle_from_the_diameter() {
        let proj = LinearProjector::sample();
        let d = mach("sonic", props(json!({ "waveCount": 4 })));
        let m = as_mach(d.as_ref());
        assert_eq!(m.wave_count, 4.0);
        assert_eq!(m.levels.len(), 12);
        assert_eq!(m.levels.iter().filter(|l| l.enabled).count(), 4);
        let g = m.geom(&proj).unwrap();
        assert_eq!((g.c0x, g.c0y, g.r), (200.0, 400.0, 100.0));
        assert_eq!((g.fx, g.fy), (1.0, 0.0));
        assert_eq!((g.nose_x, g.nose_y), (100.0, 400.0));
        assert_eq!(g.circles.len(), 4);
        assert_eq!(g.circles[0].r, 100.0);
        close(g.circles[0].cx, 200.0);
        for (i, c) in g.circles.iter().enumerate() {
            close(c.ratio, (i + 1) as f64);
            close(c.cx - c.r, g.nose_x);
        }
        assert_eq!(g.rays, vec![[100.0, -200.0, 100.0, 1000.0]]);
        assert!(d.editable_levels().is_some());
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 100.0,
                max: 100.0
            })
        );
        assert_eq!(d.handle_points(&proj), vec![(100.0, 400.0), (300.0, 400.0)]);
    }

    #[test]
    fn expansion_follows_the_click_order() {
        let proj = LinearProjector::sample();
        let d = create("sonic", vec![pt(300.0, 100.0), pt(100.0, 100.0)], None);
        let g = as_mach(d.as_ref()).geom(&proj).unwrap();
        assert_eq!(g.fx, -1.0);
        close(g.nose_x, 300.0);
        assert!(g.circles[1].cx < g.circles[0].cx);
    }

    #[test]
    fn sonic_hit_test_bounds_and_handles() {
        let proj = LinearProjector::sample();
        let d = mach("sonic", None);
        // On the first circle (r 100 around (200,400)) and on the shock wall.
        assert!(d.hit_test(300.0, 400.0, &proj, 4.0));
        assert!(d.hit_test(101.0, 50.0, &proj, 4.0));
        assert!(!d.hit_test(200.0, 400.0, &proj, 4.0));
        assert_eq!(d.hit_handle(102.0, 401.0, &proj, 4.0), Some(0));
        assert_eq!(d.hit_handle(299.0, 399.0, &proj, 4.0), Some(1));
        assert_eq!(d.hit_handle(200.0, 300.0, &proj, 4.0), None);
        assert_eq!(
            d.bounds(&proj),
            Some(Bounds {
                x: 100.0,
                y: -400.0,
                w: 1200.0,
                h: 1600.0
            })
        );
    }

    #[test]
    fn supersonic_builds_a_cone_toward_the_second_click() {
        let proj = LinearProjector::sample();
        let d = mach("supersonic", props(json!({ "waveCount": 4, "mach": 2 })));
        let m = as_mach(d.as_ref());
        assert_eq!(m.mach_number(), 2.0);
        let g = m.geom(&proj).unwrap();
        assert_eq!((g.nose_x, g.nose_y), (0.0, 400.0));
        close(g.circles[1].cx, 200.0 + 2.0 * (200.0 - 100.0));
        assert_eq!(g.rays.len(), 2);
        // M = 2 gives mu = 30 degrees; the ray length is M * maxR + 2R.
        let len = 2.0 * 400.0 + 200.0;
        let [x1, y1, x2, y2] = g.rays[0];
        assert_eq!((x1, y1), (0.0, 400.0));
        close(x2, len * 30f64.to_radians().cos());
        close(y2, 400.0 + len * 0.5);
        let [_, _, _, y4] = g.rays[1];
        close(y4, 400.0 - len * 0.5);
        assert!(d.hit_test(x2, y2, &proj, 1.0));
        assert!(d.bounds(&proj).is_some());
    }

    #[test]
    fn mach_number_is_floored_and_props_are_clamped() {
        let d = mach("supersonic", props(json!({ "mach": 0.2 })));
        assert_eq!(as_mach(d.as_ref()).mach, 1.01);
        assert_eq!(as_mach(d.as_ref()).mach_number(), 1.01);
        let d = mach("supersonic", props(json!({ "mach": 99 })));
        assert_eq!(as_mach(d.as_ref()).mach, 20.0);
        let d = mach("sonic", props(json!({ "mach": 3 })));
        assert_eq!(as_mach(d.as_ref()).mach_number(), 1.0);
        assert!(!d.write_props().unwrap().contains_key("mach"));
        assert!(
            mach("goldensupersonic", None)
                .write_props()
                .unwrap()
                .contains_key("mach")
        );
        // The raw setting is stored and floored only where it is used.
        let mut d = mach("supersonic", None);
        d.apply_settings(&props(json!({ "mach": 0.5 })).unwrap());
        assert_eq!(as_mach(d.as_ref()).mach, 0.5);
        assert_eq!(as_mach(d.as_ref()).mach_number(), 1.01);
    }

    #[test]
    fn level_settings_flow_through_apply_settings() {
        let proj = LinearProjector::sample();
        let mut d = mach("sonic", None);
        d.apply_settings(&props(json!({ "waveCount": 3, "levels.1.color": "#abcdef" })).unwrap());
        let m = as_mach(d.as_ref());
        assert_eq!(m.wave_count, 3.0);
        assert_eq!(m.levels.iter().filter(|l| l.enabled).count(), 3);
        assert_eq!(m.levels[1].color, "#abcdef");
        let g = m.geom(&proj).unwrap();
        assert_eq!(
            g.circles
                .iter()
                .map(|c| c.color.clone())
                .collect::<Vec<_>>(),
            vec![
                m.levels[0].color.clone(),
                "#abcdef".to_string(),
                m.levels[2].color.clone()
            ]
        );
        d.apply_settings(&props(json!({ "levels.1.ratio": 3.5 })).unwrap());
        let g = as_mach(d.as_ref()).geom(&proj).unwrap();
        let c = g.circles.iter().find(|c| c.ratio == 3.5).unwrap();
        close(c.r, 350.0);
        // Sorted by ratio: 1, 3, 3.5.
        assert_eq!(
            g.circles.iter().map(|c| c.ratio).collect::<Vec<_>>(),
            vec![1.0, 3.0, 3.5]
        );
        d.apply_settings(&props(json!({ "levels.0.enabled": false })).unwrap());
        assert!(!as_mach(d.as_ref()).levels[0].enabled);
        // The Waves control wins over a same-patch enabled flag.
        d.apply_settings(&props(json!({ "levels.0.enabled": false, "waveCount": 2 })).unwrap());
        assert!(as_mach(d.as_ref()).levels[0].enabled);
        // The wave count is clamped to 1..level count and never shorter than one level.
        d.apply_settings(&props(json!({ "waveCount": 99 })).unwrap());
        assert_eq!(as_mach(d.as_ref()).wave_count, 12.0);
        d.apply_settings(&props(json!({ "waveCount": 0 })).unwrap());
        assert_eq!(as_mach(d.as_ref()).wave_count, 1.0);
        d.apply_settings(&props(json!({ "showRatios": false })).unwrap());
        assert!(!as_mach(d.as_ref()).show_ratios);
        d.apply_settings(&props(json!({ "style.lineColor": "#123456" })).unwrap());
        assert_eq!(d.base().style.line_color, "#123456");
        let doc = d.serialize();
        let again = (meta("sonic").create)(&DrawingInit::from(doc.clone()));
        assert_eq!(again.serialize(), doc);
        assert!(!as_mach(again.as_ref()).show_ratios);
    }

    #[test]
    fn props_keep_their_json_shape_and_levels_win_over_wave_count() {
        let d = mach("sonic", props(json!({ "waveCount": 2 })));
        let doc: SerializedDrawing = d.serialize();
        let p = doc.props.unwrap();
        assert_eq!(p["waveCount"], json!(2));
        assert_eq!(p["showRatios"], json!(true));
        assert_eq!(
            p["levels"][0],
            json!({ "ratio": 1, "color": LEVEL_CYCLE[0], "enabled": true, "label": "1" })
        );
        assert_eq!(p["levels"][2]["enabled"], json!(false));
        // Levels from props are kept as they are: no wave-count sync.
        let d = mach(
            "sonic",
            props(json!({
                "waveCount": 1,
                "levels": [
                    { "ratio": 2, "color": "#fff", "enabled": true },
                    { "ratio": 4, "color": "#000", "enabled": true, "label": "" },
                    { "ratio": "x", "color": "#000" },
                    { "color": "#000" },
                    7
                ]
            })),
        );
        let m = as_mach(d.as_ref());
        assert_eq!(m.wave_count, 2.0);
        assert_eq!(m.levels.len(), 2);
        assert!(m.levels.iter().all(|l| l.enabled && l.label.is_none()));
        // An unusable levels array falls back to the defaults (six on).
        let d = mach(
            "sonic",
            props(json!({ "levels": [1, "a"], "waveCount": 3 })),
        );
        let m = as_mach(d.as_ref());
        assert_eq!(m.levels.len(), 12);
        assert_eq!(m.levels.iter().filter(|l| l.enabled).count(), 6);
        // Wrong types are ignored.
        let d = mach("sonic", props(json!({ "waveCount": "9", "showRatios": 1 })));
        let m = as_mach(d.as_ref());
        assert_eq!(m.wave_count, 6.0);
        assert!(m.show_ratios);
        // A disabled flag is only `false`; anything else reads as enabled.
        let d = mach(
            "sonic",
            props(json!({ "levels": [{ "ratio": 1, "color": "#fff", "enabled": 0 }] })),
        );
        assert!(as_mach(d.as_ref()).levels[0].enabled);
    }

    #[test]
    fn golden_sonic_spaces_circles_by_fibonacci_ratios() {
        let proj = LinearProjector::sample();
        let d = mach("goldensonic", None);
        let m = as_mach(d.as_ref());
        assert_eq!(m.wave_count, 6.0);
        let g = m.geom(&proj).unwrap();
        let ratios: Vec<f64> = g
            .circles
            .iter()
            .map(|c| (c.r / g.r * 1000.0).round() / 1000.0)
            .collect();
        assert_eq!(
            ratios,
            vec![
                0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.618, 2.618, 4.236, 6.854, 11.09
            ]
        );
        for c in &g.circles {
            close(c.cx - c.r, g.nose_x);
        }
        assert_eq!(g.rays.len(), 1);
        let paths: Vec<_> = d.schema().fields.iter().map(|f| f.path.clone()).collect();
        assert!(!paths.contains(&"waveCount".to_string()));
        assert!(paths.contains(&"showRatios".to_string()));
    }

    #[test]
    fn golden_supersonic_uses_fib_spacing_with_a_cone() {
        let proj = LinearProjector::sample();
        let d = mach("goldensupersonic", props(json!({ "mach": 2 })));
        let m = as_mach(d.as_ref());
        assert_eq!(m.mach_number(), 2.0);
        let g = m.geom(&proj).unwrap();
        assert_eq!(g.rays.len(), 2);
        assert_eq!(g.circles.len(), 11);
        assert_eq!(g.circles[0].ratio, 0.236);
        let c = g.circles.iter().find(|c| c.ratio == 1.618).unwrap();
        close(c.r / g.r, 1.618);
        let doc = d.serialize();
        assert_eq!(doc.props.as_ref().unwrap()["mach"], json!(2));
        assert_eq!(
            (meta("goldensupersonic").create)(&DrawingInit::from(doc.clone())).serialize(),
            doc
        );
        let paths: Vec<_> = d.schema().fields.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths.iter().filter(|p| *p == "mach").count(), 1);
        assert!(!paths.contains(&"waveCount".to_string()));
    }

    #[test]
    fn schemas_list_the_expected_controls() {
        let paths = |key: &str| -> Vec<String> {
            create(key, vec![], None)
                .schema()
                .fields
                .into_iter()
                .map(|f| f.path)
                .collect()
        };
        let line = ["style.lineColor", "style.lineWidth", "style.lineStyle"];
        let text = [
            "text.value",
            "text.color",
            "text.size",
            "text.bold",
            "text.italic",
        ];
        let expect = |mid: &[&str]| -> Vec<String> {
            line.iter()
                .chain(mid)
                .chain(&text)
                .map(|s| s.to_string())
                .collect()
        };
        assert_eq!(paths("dedekind"), expect(&["maxCurvature"]));
        assert_eq!(paths("sonic"), expect(&["waveCount", "showRatios"]));
        assert_eq!(
            paths("supersonic"),
            expect(&["mach", "waveCount", "showRatios"])
        );
        assert_eq!(paths("goldensonic"), expect(&["showRatios"]));
        assert_eq!(paths("goldensupersonic"), expect(&["mach", "showRatios"]));
    }

    #[test]
    fn sub_pixel_diameter_has_no_geometry() {
        let proj = LinearProjector::sample();
        let d = create("sonic", vec![pt(100.0, 100.0), pt(100.5, 100.0)], None);
        assert!(as_mach(d.as_ref()).geom(&proj).is_none());
        assert_eq!(d.bounds(&proj), None);
        // With every level disabled there is nothing to draw either.
        let mut d = mach("sonic", None);
        d.apply_settings(
            &props(json!({ "levels.0.enabled": false, "levels.1.enabled": false })).unwrap(),
        );
        let m = as_mach(d.as_ref());
        assert!(m.levels.iter().any(|l| l.enabled));
        let off = props(json!({ "levels": [{ "ratio": 1, "color": "#fff", "enabled": false }] }))
            .unwrap();
        let mut d = mach("sonic", None);
        d.read_props(&off);
        assert!(as_mach(d.as_ref()).geom(&proj).is_none());
    }
}
