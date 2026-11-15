//! The controller's painting: the interleave slices behind the series and the top drawings
//! layer (`prepareSlices` and `render` of the original).

use std::collections::HashSet;

use serde_json::json;

use crate::drawings::{Drawing, Projector, shown_on_timeframe};
use crate::i18n::text_of;
use crate::ports::chart_renderer::DrawingIntent;
use crate::render::canvas::Canvas2d;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::scene_graph::DrawingSlice;
use crate::render::drawings::interaction_logic::slice_key_for;
use crate::render::drawings::painter::{DrawingTheme, PaintTargets, SeriesLook, handle_ids_for};
use crate::render::native::{FrameView, NativeHost};

use super::state::{Ctl, Effect};

impl Ctl {
    /// Take what the frame hands over that gestures between frames reuse.
    fn adopt_frame(&mut self, view: &FrameView) {
        if let Some(ctx) = &view.drawings {
            self.ctx = Some(ctx.clone());
        }
        self.i18n = view.i18n.clone();
        self.painter.i18n = view.i18n.clone();
        self.painter.series_look = SeriesLook {
            style: view.series_look.style.clone(),
            up_color: view.series_look.up_color.clone(),
            down_color: view.series_look.down_color.clone(),
            line_color: view.series_look.line_color.clone(),
        };
    }

    /// Rebuild the interleave layers for this data frame: bucket the visible drawings whose
    /// z sits at or under a series boundary by the FIRST boundary at or above them (a tie
    /// paints under that series), and paint each bucket on its own cached plot-sized
    /// surface. Runs from the renderer's data paint, just before the backend composites.
    pub fn prepare_slices(
        &mut self,
        host: &dyn NativeHost,
        view: &FrameView,
        proj: &dyn Projector,
        pane_ids: &[String],
    ) -> std::collections::HashMap<String, Vec<DrawingSlice>> {
        self.adopt_frame(view);
        self.sliced.clear();
        self.last_bounds = pane_ids
            .iter()
            .map(|id| (id.clone(), view.scene.series_boundaries(id)))
            .collect();
        let mut out: std::collections::HashMap<String, Vec<DrawingSlice>> = Default::default();
        let dpr = view.coords.dpr();
        let theme = DrawingTheme::from_value(view.theme);
        let bar_ms = view.coords.bar_interval();
        // Buckets keyed `paneId|beforeZ`, in first-seen order.
        let mut buckets: Vec<(String, String, f64, Vec<usize>)> = Vec::new();
        for (i, d) in self.drawings.iter().enumerate() {
            let base = d.base();
            if !shown_on_timeframe(base.show_on.as_deref(), bar_ms)
                || !base.visible
                || self.lifted.contains(&base.id)
            {
                continue; // a dragged drawing rides the top canvas
            }
            let bounds = self
                .last_bounds
                .get(&base.pane_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let Some(before_z) = slice_key_for(base.z_index, bounds) else {
                continue; // over the stack: top canvas
            };
            let key = format!("{}|{}", base.pane_id, crate::util::js_number(before_z));
            match buckets.iter_mut().find(|b| b.0 == key) {
                Some(b) => b.3.push(i),
                None => buckets.push((key, base.pane_id.clone(), before_z, vec![i])),
            }
            self.sliced.insert(base.id.clone());
        }
        let size = (view.backing.0 as u32, view.backing.1 as u32);
        let live: HashSet<String> = buckets.iter().map(|b| b.0.clone()).collect();
        for (key, pane_id, before_z, members) in &buckets {
            let surface = self
                .slice_cache
                .entry(key.clone())
                .or_insert_with(|| host.create_slice_surface())
                .clone();
            if surface.size() != size {
                surface.resize(size.0, size.1);
            }
            let Some(sctx) = surface.context() else {
                continue;
            };
            begin_plot_frame(sctx.as_ref(), dpr);
            sctx.clear_rect(0.0, 0.0, view.backing.0 / dpr, view.backing.1 / dpr);
            let drawings: Vec<&dyn Drawing> = members
                .iter()
                .filter_map(|i| self.drawings.get(*i))
                .map(|d| d.as_ref())
                .collect();
            self.painter.paint_all(
                sctx.as_ref(),
                &drawings,
                proj,
                &theme,
                &PaintTargets::default(),
            );
            out.entry(pane_id.clone()).or_default().push(DrawingSlice {
                before_z: *before_z,
                canvas: surface.image(),
            });
        }
        self.slice_cache.retain(|key, _| live.contains(key)); // drop stale layers
        for slices in out.values_mut() {
            slices.sort_by(|a, b| a.before_z.total_cmp(&b.before_z));
        }
        out
    }

    /// Repaint the TOP drawings layer (every data frame and on internal changes): the
    /// drawings over the series stack, every selection handle (interleaved drawings'
    /// included, or a drawing sent under the candles could never show what you grabbed), and
    /// the transients: the placing ghost, anchor markers, the snap ring, the ruler.
    pub fn render(&mut self, view: &FrameView, proj: &dyn Projector) {
        self.adopt_frame(view);
        let Some(ctx) = view.drawings.clone() else {
            return;
        };
        let ctx: &dyn Canvas2d = ctx.as_ref();
        let dpr = view.coords.dpr();
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, view.backing.0 / dpr, view.backing.1 / dpr);
        let theme = DrawingTheme::from_value(view.theme);
        let bar_ms = view.coords.bar_interval();
        // A transparent inline editor overlays the label it edits: mute the canvas copy so
        // the typed text isn't drawn twice (the callout editor is opaque, so its label stays).
        let muted_label = self
            .text_editor
            .as_ref()
            .and_then(|ed| self.edited_drawing(&ed.id))
            .filter(|d| d.type_key() == "text")
            .map(|d| d.base().id.clone());
        let targets = PaintTargets {
            selected: self.selected.iter().cloned().collect(),
            hovered: self.hovered.clone(),
            dragged: self.interaction.active_drag_id().map(str::to_string),
            muted_label,
        };
        // Front (non-interleaved) drawings paint fully here; the ones interleaved into the
        // series stack painted their bodies on the backend layers, so only their handles
        // come back on top (buried under the candles they'd be unusable).
        let ghost = self.interaction.ghost(&self.registry);
        {
            let Ctl {
                painter,
                drawings,
                lifted,
                last_bounds,
                interaction,
                ..
            } = self;
            let is_interleaved = |d: &dyn Drawing| {
                let bounds = last_bounds
                    .get(&d.base().pane_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                slice_key_for(d.base().z_index, bounds).is_some()
            };
            let shown: Vec<&dyn Drawing> = drawings
                .iter()
                .filter(|d| shown_on_timeframe(d.base().show_on.as_deref(), bar_ms))
                .map(|d| d.as_ref())
                .collect();
            let on_top = |d: &&dyn Drawing| !is_interleaved(*d) || lifted.contains(&d.base().id);
            let front: Vec<&dyn Drawing> = shown.iter().copied().filter(on_top).collect();
            let behind: Vec<&dyn Drawing> = shown.iter().copied().filter(|d| !on_top(d)).collect();
            painter.paint_all(ctx, &front, proj, &theme, &targets);
            painter.paint_highlights(ctx, &behind, proj, &handle_ids_for(&targets));
            // A Ctrl-drag moves COPIES that are not in the store yet: paint them here, in
            // full and with handles, so they read as the real drawings they are about to
            // become.
            if let Some(clones) = interaction.drag_clones() {
                let ids: HashSet<String> = clones.iter().map(|c| c.base().id.clone()).collect();
                painter.paint_all(
                    ctx,
                    &clones,
                    proj,
                    &theme,
                    &PaintTargets {
                        selected: ids,
                        ..PaintTargets::default()
                    },
                );
            }
        }
        self.layout_text_editor(proj, view.theme);
        if let Some(g) = &ghost {
            self.painter.paint_ghost(ctx, g.as_ref(), proj, &theme);
        }
        // A remote placement mirrored here (drawings sync) paints as the same ghost.
        if let Some(g) = &self.external_ghost {
            self.painter.paint_ghost(ctx, g.as_ref(), proj, &theme);
        }
        // Every placing change re-renders, so this catches each anchor click, cursor move
        // and cancel; the fingerprint gate drops the no-change frames.
        self.emit_draft(ghost.as_deref());
        // Armed-tool placement prompt (a type's `placementHint`): a bottom-center pill
        // telling the user what gesture to perform; it clears once placement starts.
        if let Some(tool) = &self.active_tool
            && ghost.is_none()
            && let Some(hint) = self
                .registry
                .get(tool)
                .and_then(|m| m.placement_hint.clone())
        {
            let text = text_of(view.i18n.as_deref(), &hint, &[]);
            self.painter
                .paint_placement_hint(ctx, &text, &theme, proj.width(), proj.height());
        }
        // While placing, show control circles on the points clicked so far (so the user
        // sees where each anchor, a pitchfork's pivot say, landed before it completes).
        if let Some(markers) = self.interaction.placing_markers(proj) {
            self.painter.paint_handles(ctx, &markers);
        }
        // Magnet (Ctrl) affordance: a ring on the candle point the next anchor will snap to.
        if let Some(m) = self.interaction.snap_marker()
            && let Some(my) = proj.y_of(m.point.price, &m.pane_id)
        {
            self.painter
                .paint_snap_ring(ctx, proj.x_of(m.point.time), my, &theme);
        }
        // The transient ruler paints on top of everything (until cleared on the next
        // press, pan or zoom).
        if self.measure.is_active() {
            self.measure
                .paint(ctx, proj, view.theme, view.i18n.as_deref());
        }
        if let Some(r) = self.interaction.marquee_rect() {
            self.painter.paint_marquee(ctx, r.x, r.y, r.w, r.h);
        }
    }

    /// Report placement progress upstream (`draft` intent) so the drawings sync can mirror
    /// the ghost live on linked charts. Called from every render; only actual shape changes
    /// emit, and the end of a placement emits one `None`.
    fn emit_draft(&mut self, ghost: Option<&dyn Drawing>) {
        let key = ghost.map(|g| {
            let b = g.base();
            json!({ "t": g.type_key(), "p": b.pane_id, "a": b.anchors, "s": b.style, "x": b.text })
                .to_string()
        });
        if key == self.last_draft_key {
            return;
        }
        self.last_draft_key = key;
        self.fx.push(Effect::Emit(DrawingIntent::Draft {
            doc: ghost.map(|g| g.serialize()),
        }));
    }
}
