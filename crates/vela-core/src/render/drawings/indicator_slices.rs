//! The per-indicator cached slices of script drawings.
//!
//! Prepaints each indicator's script drawings (line/box/label/polyline/linefill) AND its
//! tables into interleave-layer canvases the geometry backend composites into the pane's
//! series stack: the same [`DrawingSlice`] contract user drawings ride. Keyed at the
//! first series boundary STRICTLY ABOVE the owning model's z, they paint over the
//! model's own series but under everything stacked higher (candles included), so the
//! whole model moves as one unit when its object-tree row is reordered. `force_overlay`
//! content paints on the price pane above its whole stack (`Infinity`), mirroring where
//! `force_overlay` series paint.
//!
//! Also collects the label and table-cell tooltip hit-rects (plot space) each frame.
//!
//! Left for the browser crate: the offscreen canvases themselves. The original creates
//! `HTMLCanvasElement`s and reads `ref.width` / `ref.height` off the data canvas; here a
//! [`SliceSurfaceFactory`] creates them (a [`SliceSurface`] is one canvas: resize,
//! `getContext('2d')`, and the opaque handle the backend composites) and the data
//! canvas's bitmap size is the `ref_size` argument.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use crate::indicators::classics::math::js_round;
use crate::intl::Intl;
use crate::model::drawings::DrawingTable;
use crate::model::scene::PaneKind;
use crate::render::canvas::{Canvas2d, CanvasImage};
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::scene_graph::DrawingSlice;
use crate::render::core::{CoordinateSystem, PaneBounds, PriceScale, SceneGraph};
use crate::render::shared::drawing_scene_renderer::{
    BarHighLow, DrawingSceneRenderer, DrawingSet, LabelTipRegion, LayerDeps, drawing_set_empty,
    model_drawing_set,
};
use crate::render::shared::table_canvas_renderer::{TablePaintArgs, paint_table};
use crate::util::js_number;
use std::sync::Arc;

/// One offscreen canvas a slice is painted on.
pub trait SliceSurface {
    /// The bitmap size in device px.
    fn size(&self) -> (u32, u32);
    /// Set the bitmap size (which clears it, as assigning `canvas.width` does).
    fn resize(&self, width: u32, height: u32);
    /// The canvas's 2D context, `None` when the platform cannot open one.
    fn context(&self) -> Option<Rc<dyn Canvas2d>>;
    /// The opaque handle the backend composites (the original's `HTMLCanvasElement`).
    fn image(&self) -> CanvasImage;
}

/// The "create an offscreen canvas" capability.
pub trait SliceSurfaceFactory {
    fn create_surface(&self) -> Rc<dyn SliceSurface>;
}

/// The slice key for an indicator's drawings: the first series boundary STRICTLY above
/// the model's z (the drawings composite above the model's own series, where a tie would
/// put them under it, but below the next slot), or `Infinity` when the model tops its
/// pane's stack (composited above the `force_overlay` series).
pub fn indicator_slice_key(z: f64, boundaries: &[f64]) -> f64 {
    boundaries
        .iter()
        .copied()
        .find(|b| *b > z)
        .unwrap_or(f64::INFINITY)
}

/// One model's contribution to a slice canvas: its drawing set and tables painted
/// against a pane (whose scale is already the model's effective one) at its index offset.
struct SliceEntry<'a> {
    set: DrawingSet,
    tables: Vec<&'a DrawingTable>,
    scale: PriceScale,
    bounds: PaneBounds,
    index_offset: f64,
}

struct Bucket<'a> {
    pane_id: String,
    before_z: f64,
    entries: Vec<SliceEntry<'a>>,
}

fn add_entry<'a>(
    buckets: &mut Vec<(String, Bucket<'a>)>,
    pane_id: &str,
    before_z: f64,
    entry: SliceEntry<'a>,
) {
    let key = format!("{pane_id}|{}", js_number(before_z));
    match buckets.iter_mut().find(|(k, _)| *k == key) {
        Some((_, bucket)) => bucket.entries.push(entry),
        None => buckets.push((
            key,
            Bucket {
                pane_id: pane_id.to_string(),
                before_z,
                entries: vec![entry],
            },
        )),
    }
}

/// The slice builder. Owned by the renderer (`&mut self`): it keeps the slice canvases
/// and the width cache across frames.
pub struct IndicatorDrawingSlices {
    draw_scene: DrawingSceneRenderer,
    /// Slice canvas cache, keyed `paneId|beforeZ`: same lifecycle as the user-drawing
    /// cache.
    slice_cache: HashMap<String, Rc<dyn SliceSurface>>,
    /// Tooltip hit-rects of every label drawn this frame, in plot coords (rebuilt per
    /// prepare).
    tips: Vec<LabelTipRegion>,
}

impl Default for IndicatorDrawingSlices {
    fn default() -> Self {
        Self::new()
    }
}

impl IndicatorDrawingSlices {
    /// A slice builder wrapping text with the built-in English `Intl` backend.
    pub fn new() -> Self {
        Self::from_renderer(DrawingSceneRenderer::new())
    }

    /// A slice builder wrapping text with the platform's `Intl` backend.
    pub fn with_intl(intl: Arc<dyn Intl>) -> Self {
        Self::from_renderer(DrawingSceneRenderer::with_intl(intl))
    }

    fn from_renderer(draw_scene: DrawingSceneRenderer) -> Self {
        Self {
            draw_scene,
            slice_cache: HashMap::new(),
            tips: Vec::new(),
        }
    }

    /// Rebuild the per-indicator drawing slices for this data frame. `ref_size` is the
    /// data canvas's bitmap size, which the slices must match pixel for pixel (the
    /// backend composites them 1:1). Runs from the renderer's data paint, just before
    /// the backend composites the scene. The result maps a pane id to its slices sorted
    /// by `before_z`.
    pub fn prepare(
        &mut self,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &Value,
        ref_size: (u32, u32),
        factory: &dyn SliceSurfaceFactory,
    ) -> HashMap<String, Vec<DrawingSlice>> {
        self.tips = Vec::new();
        let mut out: HashMap<String, Vec<DrawingSlice>> = HashMap::new();
        if ref_size.0 == 0 || ref_size.1 == 0 {
            self.slice_cache.clear();
            return out;
        }
        let time_to_logical = |ms: f64| coords.time_to_logical(ms);
        let bar_at = |logical: f64| -> Option<BarHighLow> {
            let at = js_round(logical);
            if !at.is_finite() || at < 0.0 {
                return None;
            }
            scene.bars.get(at as usize).map(|b| BarHighLow {
                high: b.high,
                low: b.low,
            })
        };
        let deps = LayerDeps {
            time_to_logical: &time_to_logical,
            bar_at: &bar_at,
            theme,
        };
        let dpr = coords.dpr();
        let data_w = coords.width();

        // Bucket the models' drawing sets by `paneId|beforeZ`. Own models iterate in
        // ascending z (their relative stacking inside a shared canvas), force_overlay
        // sets append last: they paint over the price pane's whole pile, as before.
        let mut buckets: Vec<(String, Bucket)> = Vec::new();
        for pane in scene.ordered_panes() {
            if pane.collapsed {
                continue; // collapsed strip: legend only, no drawings
            }
            let boundaries = scene.series_boundaries(&pane.id);
            for m in scene.ordered_indicators_for_pane(&pane.id) {
                let set = model_drawing_set(m, false);
                let tables: Vec<&DrawingTable> = m.tables.iter().filter(|t| !t.overlay).collect();
                if drawing_set_empty(&set) && tables.is_empty() {
                    continue;
                }
                // A merged (own-scale) indicator's drawings follow its own scale column.
                let scale = scene.scale_for(m, pane);
                let before_z = indicator_slice_key(scene.z_of(&m.id), &boundaries);
                add_entry(
                    &mut buckets,
                    &pane.id,
                    before_z,
                    SliceEntry {
                        set,
                        tables,
                        scale,
                        bounds: pane.bounds,
                        index_offset: scene.offset_of(&m.id) as f64,
                    },
                );
            }
            if pane.kind == PaneKind::Price {
                for m in scene.indicators.values() {
                    let set = model_drawing_set(m, true);
                    let tables: Vec<&DrawingTable> =
                        m.tables.iter().filter(|t| t.overlay).collect();
                    if drawing_set_empty(&set) && tables.is_empty() {
                        continue;
                    }
                    add_entry(
                        &mut buckets,
                        &pane.id,
                        f64::INFINITY,
                        SliceEntry {
                            set,
                            tables,
                            scale: pane.scale,
                            bounds: pane.bounds,
                            index_offset: scene.offset_of(&m.id) as f64,
                        },
                    );
                }
            }
        }

        let mut live: Vec<String> = Vec::new();
        for (key, bucket) in buckets {
            live.push(key.clone());
            let surface = self
                .slice_cache
                .entry(key)
                .or_insert_with(|| factory.create_surface())
                .clone();
            if surface.size() != ref_size {
                surface.resize(ref_size.0, ref_size.1);
            }
            let Some(ctx) = surface.context() else {
                continue;
            };
            begin_plot_frame(ctx.as_ref(), dpr);
            let (w, h) = surface.size();
            ctx.clear_rect(0.0, 0.0, f64::from(w) / dpr, f64::from(h) / dpr);
            for e in bucket.entries {
                self.paint_entry(ctx.as_ref(), e, coords, data_w, &deps);
            }
            out.entry(bucket.pane_id).or_default().push(DrawingSlice {
                before_z: bucket.before_z,
                canvas: surface.image(),
            });
        }
        // drop stale layers
        self.slice_cache.retain(|key, _| live.contains(key));
        for slices in out.values_mut() {
            slices.sort_by(|a, b| {
                a.before_z
                    .partial_cmp(&b.before_z)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        out
    }

    fn paint_entry(
        &mut self,
        ctx: &dyn Canvas2d,
        e: SliceEntry,
        coords: &CoordinateSystem,
        data_w: f64,
        deps: &LayerDeps,
    ) {
        let (scale, bounds) = (e.scale, e.bounds);
        let mut pane_tips: Vec<LabelTipRegion> = Vec::new();
        ctx.save();
        ctx.translate(0.0, bounds.top); // pane-relative space (drawings use [0, H])
        ctx.begin_path();
        ctx.rect(0.0, 0.0, data_w, bounds.height);
        ctx.clip();
        self.draw_scene.set_set(e.set, e.index_offset);
        self.draw_scene.render(
            deps,
            ctx,
            data_w,
            bounds.height,
            &|l| coords.logical_to_x(l),
            &|price| Some(coords.price_to_y(price, &scale, &bounds) - bounds.top),
        );
        pane_tips.extend_from_slice(self.draw_scene.label_tip_regions());
        // Tables paint over the model's other drawings: the pane-corner dashboard reads
        // above its own lines/labels, as the DOM overlay used to.
        for t in &e.tables {
            paint_table(
                ctx,
                t,
                &TablePaintArgs::new(bounds.height, data_w, deps.theme),
                &mut pane_tips,
            );
        }
        ctx.restore();
        // Collect this entry's tooltip rects, shifted from pane space into plot space.
        for r in pane_tips {
            self.tips.push(LabelTipRegion {
                top: r.top + bounds.top,
                bottom: r.bottom + bounds.top,
                ..r
            });
        }
    }

    /// Tooltip of the topmost label or table cell under a plot-space point, or `None`.
    /// Fed by the last [`prepare`](Self::prepare).
    pub fn label_tooltip_at(&self, x: f64, y: f64) -> Option<&str> {
        self.tips
            .iter()
            .rev()
            .find(|r| x >= r.left && x <= r.right && y >= r.top && y <= r.bottom)
            .map(|r| r.text.as_str())
    }
}

/// Merge the indicator slices with the user-drawing slices into one per-pane, z-sorted
/// list for the backend. On a shared `before_z` the indicator content composites first
/// (user drawings stay on top of engine output at the same slot).
pub fn merge_slices(
    indicator: &HashMap<String, Vec<DrawingSlice>>,
    user: &HashMap<String, Vec<DrawingSlice>>,
) -> HashMap<String, Vec<DrawingSlice>> {
    let mut out: HashMap<String, Vec<DrawingSlice>> = HashMap::new();
    for (pane_id, slices) in indicator {
        out.insert(pane_id.clone(), slices.clone());
    }
    for (pane_id, slices) in user {
        out.entry(pane_id.clone())
            .or_default()
            .extend(slices.iter().cloned());
    }
    for slices in out.values_mut() {
        // stable: ties keep indicator-first
        slices.sort_by(|a, b| {
            a.before_z
                .partial_cmp(&b.before_z)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaneHint;
    use crate::model::drawings::{
        BoxFontFamily, BoxHAlign, BoxTextSize, BoxVAlign, DrawingExtend, DrawingLabel, DrawingLine,
        DrawingXLoc, LabelStyle, LabelYLoc, TableCell, TablePosition, TableTextSize,
    };
    use crate::model::indicator::IndicatorModel;
    use crate::model::series::LineStyle;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    // keying (indicator-drawing-slices.test.ts)

    #[test]
    fn a_model_under_the_candles_keys_at_the_candle_boundary() {
        // Default mount: candles z 0, indicator z -1 => boundaries [-1, 0].
        assert_eq!(indicator_slice_key(-1.0, &[-1.0, 0.0]), 0.0);
    }

    #[test]
    fn a_model_raised_above_the_candles_keys_past_every_boundary() {
        assert_eq!(indicator_slice_key(1.0, &[0.0, 1.0]), f64::INFINITY);
    }

    #[test]
    fn a_model_between_two_others_keys_at_the_next_series_up() {
        assert_eq!(indicator_slice_key(-2.0, &[-2.0, -1.0, 0.0]), -1.0);
    }

    #[test]
    fn it_never_keys_at_the_models_own_z() {
        assert!(indicator_slice_key(-1.0, &[-1.0, 0.0]) > -1.0);
        assert_eq!(indicator_slice_key(0.0, &[]), f64::INFINITY);
    }

    #[test]
    fn it_follows_a_reorder_end_to_end_through_scene_graph_z_keys() {
        let mut scene = SceneGraph::new();
        scene.candle_z = 0.0;
        scene.assign_indicator_z("pine"); // mounts at the bottom: z -1
        let sorted = |scene: &SceneGraph| {
            let mut b = vec![scene.z_of("pine"), scene.candle_z];
            b.sort_by(|a, b| a.partial_cmp(b).unwrap());
            b
        };
        let below = indicator_slice_key(scene.z_of("pine"), &sorted(&scene));
        assert_eq!(below, scene.candle_z); // drawings composite under the candles
        scene.bring_indicator_to_front("pine"); // the object tree drags the row above Candles
        let above = indicator_slice_key(scene.z_of("pine"), &sorted(&scene));
        assert_eq!(above, f64::INFINITY); // drawings composite over the whole stack
    }

    // merging

    fn tagged(before_z: f64, tag: &'static str) -> DrawingSlice {
        DrawingSlice {
            before_z,
            canvas: Rc::new(tag),
        }
    }

    fn tag(s: &DrawingSlice) -> &'static str {
        s.canvas.downcast_ref::<&'static str>().unwrap()
    }

    fn map(pane: &str, slices: Vec<DrawingSlice>) -> HashMap<String, Vec<DrawingSlice>> {
        HashMap::from([(pane.to_string(), slices)])
    }

    #[test]
    fn merges_both_sources_per_pane_sorted_by_before_z() {
        let ind = map("price", vec![tagged(5.0, "i5")]);
        let usr = map("price", vec![tagged(0.0, "u0")]);
        let merged = merge_slices(&ind, &usr);
        let zs: Vec<f64> = merged["price"].iter().map(|s| s.before_z).collect();
        assert_eq!(zs, vec![0.0, 5.0]);
    }

    #[test]
    fn on_a_shared_key_the_indicator_slice_composites_first() {
        let ind = map("price", vec![tagged(0.0, "ind")]);
        let usr = map("price", vec![tagged(0.0, "usr")]);
        let merged = merge_slices(&ind, &usr);
        let tags: Vec<&str> = merged["price"].iter().map(tag).collect();
        assert_eq!(tags, vec!["ind", "usr"]);
    }

    #[test]
    fn keeps_panes_only_one_source_contributes_to() {
        let ind = map("study-1", vec![tagged(-2.0, "i")]);
        let usr = map("price", vec![tagged(1.0, "u")]);
        let merged = merge_slices(&ind, &usr);
        assert_eq!(merged["study-1"].len(), 1);
        assert_eq!(merged["price"].len(), 1);
    }

    // preparing

    struct FakeSurface {
        size: Cell<(u32, u32)>,
        canvas: Rc<RecordingCanvas>,
        resizes: Cell<u32>,
    }

    /// `RecordingCanvas` behind the `Rc<dyn Canvas2d>` the surface hands out.
    struct Shared(Rc<RecordingCanvas>);

    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
            $(fn $name(&self, $($arg: $ty),*) $(-> $ret)? { self.0.$name($($arg),*) })*
        };
    }

    impl Canvas2d for Shared {
        forward! {
            save(); restore();
            translate(x: f64, y: f64); rotate(a: f64); scale(x: f64, y: f64);
            set_transform(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64);
            set_fill_style(p: &crate::render::canvas::Paint);
            set_stroke_style(p: &crate::render::canvas::Paint);
            set_global_alpha(a: f64); global_alpha() -> f64;
            set_line_width(w: f64); line_width() -> f64;
            set_line_dash(s: &[f64]);
            set_line_cap(c: crate::render::canvas::LineCap);
            set_line_join(j: crate::render::canvas::LineJoin);
            set_font(f: &str);
            set_text_align(a: crate::render::canvas::TextAlign);
            set_text_baseline(b: crate::render::canvas::TextBaseline);
            set_direction(d: &str);
            begin_path(); move_to(x: f64, y: f64); line_to(x: f64, y: f64); close_path();
            rect(x: f64, y: f64, w: f64, h: f64);
            round_rect(x: f64, y: f64, w: f64, h: f64, r: f64);
            arc(x: f64, y: f64, r: f64, s: f64, e: f64, ccw: bool);
            arc_to(a: f64, b: f64, c: f64, d: f64, r: f64);
            ellipse(x: f64, y: f64, rx: f64, ry: f64, rot: f64, s: f64, e: f64, ccw: bool);
            quadratic_curve_to(a: f64, b: f64, c: f64, d: f64);
            bezier_curve_to(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64);
            fill(); stroke(); clip();
            fill_rect(x: f64, y: f64, w: f64, h: f64);
            stroke_rect(x: f64, y: f64, w: f64, h: f64);
            clear_rect(x: f64, y: f64, w: f64, h: f64);
            fill_text(t: &str, x: f64, y: f64);
            stroke_text(t: &str, x: f64, y: f64);
            measure_text(t: &str) -> crate::render::canvas::TextMetrics;
            draw_image(i: &CanvasImage, x: f64, y: f64, w: f64, h: f64);
        }
    }

    impl SliceSurface for FakeSurface {
        fn size(&self) -> (u32, u32) {
            self.size.get()
        }
        fn resize(&self, w: u32, h: u32) {
            self.size.set((w, h));
            self.resizes.set(self.resizes.get() + 1);
        }
        fn context(&self) -> Option<Rc<dyn Canvas2d>> {
            Some(Rc::new(Shared(self.canvas.clone())))
        }
        fn image(&self) -> CanvasImage {
            Rc::new("surface")
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        made: RefCell<Vec<Rc<FakeSurface>>>,
    }

    impl SliceSurfaceFactory for FakeFactory {
        fn create_surface(&self) -> Rc<dyn SliceSurface> {
            let s = Rc::new(FakeSurface {
                size: Cell::new((0, 0)),
                canvas: Rc::new(RecordingCanvas::new()),
                resizes: Cell::new(0),
            });
            self.made.borrow_mut().push(s.clone());
            s
        }
    }

    fn model(id: &str) -> IndicatorModel {
        IndicatorModel {
            id: id.into(),
            title: id.into(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: true,
            pane_hint: PaneHint::Price,
            native: None,
            legend: None,
            pane_axis: None,
            pane_id: Some("price".into()),
            own_scale: false,
            anchor_time: None,
            series: vec![],
            fills: vec![],
            backgrounds: vec![],
            price_lines: vec![],
            lines: vec![],
            boxes: vec![],
            labels: vec![],
            polylines: vec![],
            linefills: vec![],
            tables: vec![],
            bar_colors: vec![],
            trades: vec![],
            inputs: vec![],
            input_values: Default::default(),
            props: vec![],
            prop_values: Default::default(),
        }
    }

    fn hline() -> DrawingLine {
        DrawingLine {
            id: "l".into(),
            pane_id: "price".into(),
            xloc: DrawingXLoc::BarIndex,
            x1: 0.0,
            y1: 50.0,
            x2: 4.0,
            y2: 50.0,
            extend: DrawingExtend::None,
            color: Some("#00ff00".into()),
            invisible: false,
            width: 1.0,
            style: LineStyle::Solid,
            arrow_left: false,
            arrow_right: false,
            overlay: false,
        }
    }

    fn scene_with(models: Vec<IndicatorModel>) -> (SceneGraph, CoordinateSystem) {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("price", PaneKind::Price, 0, None);
        {
            let pane = scene.panes.get_mut("price").unwrap();
            pane.bounds = PaneBounds {
                top: 10.0,
                height: 200.0,
            };
            pane.scale = PriceScale::new(0.0, 100.0);
        }
        for m in models {
            let id = m.id.clone();
            scene.indicators.insert(id.clone(), m);
            scene.assign_indicator_z(&id);
        }
        let mut coords = CoordinateSystem::new();
        coords.set_size(400.0, 300.0, 2.0);
        coords.set_bars(&[1000, 2000, 3000, 4000, 5000]);
        (scene, coords)
    }

    #[test]
    fn nothing_is_made_for_a_zero_sized_reference_canvas_and_the_cache_is_dropped() {
        let (scene, coords) = scene_with(vec![]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let out = slices.prepare(&scene, &coords, &json!({}), (0, 600), &factory);
        assert!(out.is_empty());
        assert!(factory.made.borrow().is_empty());
    }

    #[test]
    fn a_model_without_drawings_makes_no_slice() {
        let (scene, coords) = scene_with(vec![model("a")]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let out = slices.prepare(&scene, &coords, &json!({}), (800, 600), &factory);
        assert!(out.is_empty());
    }

    #[test]
    fn a_models_drawings_paint_on_a_slice_keyed_above_its_z_with_the_exact_call_stream() {
        let mut m = model("pine");
        m.lines.push(hline());
        let (scene, coords) = scene_with(vec![m]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let theme = json!({ "textColor": "#fff", "fontFamily": "Inter" });
        let out = slices.prepare(&scene, &coords, &theme, (800, 600), &factory);

        // The only series on the pane is the model itself (candle z 0 is a boundary
        // too, and the model mounted at the bottom z = -1): keyed at the candles.
        let price = &out["price"];
        assert_eq!(price.len(), 1);
        assert_eq!(price[0].before_z, scene.candle_z);

        let made = factory.made.borrow();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].size.get(), (800, 600));
        let x = |l: f64| coords.logical_to_x(l);
        // y(50) = top + height * (1 - 0.5) = 110, minus the pane top = 100.
        assert_eq!(
            made[0].canvas.to_json(),
            json!([
                ["setTransform", 2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
                ["direction", "ltr"],
                ["clearRect", 0.0, 0.0, 400.0, 300.0],
                ["save"],
                ["translate", 0.0, 10.0],
                ["beginPath"],
                ["rect", 0.0, 0.0, 400.0, 200.0],
                ["clip"],
                ["save"],
                ["beginPath"],
                ["moveTo", x(0.0), 100.0],
                ["lineTo", x(4.0), 100.0],
                ["strokeStyle", "#00ff00"],
                ["lineWidth", 1.0],
                ["setLineDash", []],
                ["stroke"],
                ["setLineDash", []],
                ["restore"],
                ["restore"],
            ])
        );
    }

    #[test]
    fn force_overlay_content_keys_at_infinity_on_the_price_pane() {
        let mut m = model("pine");
        let mut line = hline();
        line.overlay = true;
        m.lines.push(line);
        let (scene, coords) = scene_with(vec![m]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let out = slices.prepare(&scene, &coords, &json!({}), (800, 600), &factory);
        assert_eq!(out["price"].len(), 1);
        assert_eq!(out["price"][0].before_z, f64::INFINITY);
    }

    #[test]
    fn slices_are_cached_between_frames_and_stale_ones_dropped() {
        let mut m = model("pine");
        m.lines.push(hline());
        let (mut scene, coords) = scene_with(vec![m]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let theme = json!({});
        slices.prepare(&scene, &coords, &theme, (800, 600), &factory);
        slices.prepare(&scene, &coords, &theme, (800, 600), &factory);
        assert_eq!(factory.made.borrow().len(), 1, "the canvas is reused");
        assert_eq!(factory.made.borrow()[0].resizes.get(), 1, "resized once");
        // a changed reference size resizes the cached canvas
        slices.prepare(&scene, &coords, &theme, (400, 300), &factory);
        assert_eq!(factory.made.borrow()[0].resizes.get(), 2);
        // dropping the drawings drops the layer: the next one is a fresh canvas
        scene.indicators.get_mut("pine").unwrap().lines.clear();
        let out = slices.prepare(&scene, &coords, &theme, (400, 300), &factory);
        assert!(out.is_empty());
        scene
            .indicators
            .get_mut("pine")
            .unwrap()
            .lines
            .push(hline());
        slices.prepare(&scene, &coords, &theme, (400, 300), &factory);
        assert_eq!(factory.made.borrow().len(), 2);
    }

    #[test]
    fn two_models_sharing_a_slot_paint_on_one_canvas_in_z_order() {
        let mut a = model("a");
        a.lines.push(hline());
        let mut b = model("b");
        b.lines.push(hline());
        let (scene, coords) = scene_with(vec![a, b]);
        // `a` mounted first (lower z), `b` above it: keys differ (-1 for a at b's slot).
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let out = slices.prepare(&scene, &coords, &json!({}), (800, 600), &factory);
        let zs: Vec<f64> = out["price"].iter().map(|s| s.before_z).collect();
        let mut sorted = zs.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(zs, sorted);
        assert_eq!(zs.len(), 2);
    }

    fn label_with_tooltip() -> DrawingLabel {
        DrawingLabel {
            id: "lb".into(),
            pane_id: "price".into(),
            xloc: DrawingXLoc::BarIndex,
            x: 2.0,
            y: 50.0,
            yloc: LabelYLoc::Price,
            text: Some("AB".into()),
            style: LabelStyle::LabelDown,
            color: Some("#2962ff".into()),
            text_color: None,
            size: BoxTextSize::Normal,
            text_align: BoxHAlign::Center,
            tooltip: Some("hello".into()),
            font_family: BoxFontFamily::Default,
            bold: false,
            italic: false,
            no_fill: false,
            overlay: false,
        }
    }

    fn table_with_tooltip() -> DrawingTable {
        DrawingTable {
            id: "t".into(),
            pane_id: "price".into(),
            position: TablePosition::TopLeft,
            columns: 1,
            rows: 1,
            bg_color: None,
            frame_color: None,
            frame_width: 0.0,
            border_color: None,
            border_width: 0.0,
            cells: vec![vec![Some(TableCell {
                text: Some("T".into()),
                text_color: None,
                bg_color: None,
                h_align: BoxHAlign::Center,
                v_align: BoxVAlign::Center,
                text_size: TableTextSize::Named(BoxTextSize::Normal),
                font_family: BoxFontFamily::Default,
                tooltip: Some("cell".into()),
                bold: false,
                italic: false,
                width: None,
                height: None,
                merged: false,
            })]],
            merges: vec![],
            overlay: false,
        }
    }

    #[test]
    fn tooltips_are_collected_in_plot_space_and_the_topmost_wins() {
        let mut m = model("pine");
        m.labels.push(label_with_tooltip());
        m.tables.push(table_with_tooltip());
        let (scene, coords) = scene_with(vec![m]);
        let factory = FakeFactory::default();
        let mut slices = IndicatorDrawingSlices::new();
        let theme = json!({ "textColor": "#fff", "fontFamily": "sans-serif" });
        slices.prepare(&scene, &coords, &theme, (800, 600), &factory);
        // The table cell sits at the pane top-left (margin 6, pane top 10): 13px text,
        // one char "T" at 13px => 7.8 wide => ceil 8 + 12 = 20 wide,
        // ceil(15.6) + 4 = 20 tall. Cell rect: x 6..26, y 16..36 in plot space.
        assert_eq!(slices.label_tooltip_at(10.0, 20.0), Some("cell"));
        // The label bubble is at x(2) (centered), y = 100 + 10 above its anchor.
        let lx = coords.logical_to_x(2.0);
        assert_eq!(slices.label_tooltip_at(lx, 110.0 - 30.0), Some("hello"));
        assert_eq!(slices.label_tooltip_at(399.0, 299.0), None);
    }
}
