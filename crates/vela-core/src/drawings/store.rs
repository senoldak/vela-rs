//! The single source of truth for a chart's user drawings.

use serde_json::Value;

use super::document::{DRAWINGS_DOC_VERSION, DrawingsDocument, migrate};
use super::drawing::{Drawing, Props};
use super::geometry::DrawingPoint;
use super::registry::DrawingRegistry;
use super::style::{DrawingStyle, DrawingText};

/// A (possibly partial) record applied onto an existing drawing. `None`
/// leaves a field alone; the doubly-optional fields distinguish "leave alone"
/// from "clear".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingPatch {
    pub anchors: Option<Vec<DrawingPoint>>,
    /// Replaces the style wholesale: a settings reset must be able to drop
    /// keys (a fill color) that are absent from the type default. Callers
    /// that want a partial style patch merge before calling update.
    pub style: Option<DrawingStyle>,
    /// `Some(None)` removes the text block.
    pub text: Option<Option<DrawingText>>,
    pub pane_id: Option<String>,
    pub locked: Option<bool>,
    pub visible: Option<bool>,
    pub z_index: Option<f64>,
    /// Per-type extras (e.g. fib levels).
    pub props: Option<Props>,
    /// `Some(None)` lifts a timeframe limit.
    pub show_on: Option<Option<Vec<String>>>,
}

/// Owns identity + paint order (z) and exposes CRUD + persistence.
/// Renderer-agnostic: it holds [`Drawing`] instances, but everything that
/// leaves is plain serialized data.
///
/// The original notifies `onChange` listeners after each mutation. Here every
/// mutation bumps [`revision`](Self::revision) instead; a controller compares
/// revisions to know when to re-sync its renderer.
pub struct DrawingStore {
    /// Insertion order: the tie-break of the paint order.
    drawings: Vec<Box<dyn Drawing>>,
    seq: u64,
    z_counter: f64,
    revision: u64,
}

impl Default for DrawingStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DrawingStore {
    pub fn new() -> Self {
        Self {
            drawings: Vec::new(),
            seq: 0,
            z_counter: 1.0,
            revision: 0,
        }
    }

    /// Bumped by every mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn emit(&mut self) {
        self.revision += 1;
    }

    /// Allocate a unique, stable drawing id.
    pub fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("dw-{}", self.seq)
    }

    /// Insert a drawing, assigning a mount-order z (later ⇒ painted in front)
    /// when it carries none. The counter only ever grows, so the fallback
    /// stays the front among drawings even after explicit keys landed below
    /// it. A drawing with an id already in the store replaces that one.
    pub fn add(&mut self, mut d: Box<dyn Drawing>) -> &dyn Drawing {
        if d.base().z_index == 0.0 || d.base().z_index.is_nan() {
            d.base_mut().z_index = self.z_counter;
        }
        self.z_counter = self.z_counter.max(d.base().z_index.ceil()) + 1.0;
        let index = match self
            .drawings
            .iter()
            .position(|x| x.base().id == d.base().id)
        {
            Some(index) => {
                self.drawings[index] = d;
                index
            }
            None => {
                self.drawings.push(d);
                self.drawings.len() - 1
            }
        };
        self.emit();
        self.drawings[index].as_ref()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.drawings.len();
        self.drawings.retain(|d| d.base().id != id);
        let removed = self.drawings.len() != before;
        if removed {
            self.emit();
        }
        removed
    }

    pub fn get(&self, id: &str) -> Option<&dyn Drawing> {
        self.drawings
            .iter()
            .find(|d| d.base().id == id)
            .map(|d| d.as_ref())
    }

    /// Direct mutable access. The caller owns the consequences: a change made
    /// through it does not bump the revision.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut dyn Drawing> {
        match self.drawings.iter_mut().find(|d| d.base().id == id) {
            Some(d) => Some(d.as_mut()),
            None => None,
        }
    }

    pub fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    pub fn len(&self) -> usize {
        self.drawings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drawings.is_empty()
    }

    /// Apply a (possibly partial) record onto an existing drawing in place.
    pub fn update(&mut self, id: &str, patch: DrawingPatch) -> Option<&dyn Drawing> {
        let index = self.drawings.iter().position(|d| d.base().id == id)?;
        let d = &mut self.drawings[index];
        let base = d.base_mut();
        if let Some(anchors) = patch.anchors {
            base.anchors = anchors;
        }
        if let Some(style) = patch.style {
            base.style = style;
        }
        if let Some(text) = patch.text {
            base.text = text;
        }
        if let Some(pane_id) = patch.pane_id {
            base.pane_id = pane_id;
        }
        if let Some(locked) = patch.locked {
            base.locked = locked;
        }
        if let Some(visible) = patch.visible {
            base.visible = visible;
        }
        if let Some(z_index) = patch.z_index {
            base.z_index = z_index;
        }
        if let Some(show_on) = patch.show_on {
            base.show_on = show_on.map(|keys| super::visibility::sanitize_show_on_keys(&keys));
        }
        if let Some(props) = patch.props {
            d.apply_props(&props);
        }
        self.emit();
        Some(self.drawings[index].as_ref())
    }

    /// Every drawing in paint order (ascending z; insertion order breaks ties).
    pub fn all(&self) -> Vec<&dyn Drawing> {
        let mut out: Vec<&dyn Drawing> = self.drawings.iter().map(|d| d.as_ref()).collect();
        out.sort_by(|a, b| {
            a.base()
                .z_index
                .partial_cmp(&b.base().z_index)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// Drawings on one pane, in paint order.
    pub fn by_pane(&self, pane_id: &str) -> Vec<&dyn Drawing> {
        self.all()
            .into_iter()
            .filter(|d| d.base().pane_id == pane_id)
            .collect()
    }

    /// Raise over every drawing, and over `floor_z`: the top of the pane's
    /// series stack when the renderer shares one z space with it, so "front"
    /// beats the candles too. Pass 0 when there is no such floor.
    pub fn bring_to_front(&mut self, id: &str, floor_z: f64) {
        let z = self.z_counter.max(floor_z.floor() + 1.0);
        let Some(d) = self.get_mut(id) else { return };
        d.base_mut().z_index = z;
        self.z_counter = z + 1.0;
        self.emit();
    }

    /// Drop under every drawing, and under `ceil_z`: the bottom of the pane's
    /// series stack, so "back" lands behind the candles and the indicators.
    /// Pass 1 when there is no such ceiling.
    pub fn send_to_back(&mut self, id: &str, ceil_z: f64) {
        let min = self
            .drawings
            .iter()
            .map(|d| d.base().z_index)
            .fold(ceil_z.ceil(), f64::min);
        let Some(d) = self.get_mut(id) else { return };
        d.base_mut().z_index = min - 1.0;
        self.emit();
    }

    pub fn set_locked(&mut self, id: &str, locked: bool) {
        if let Some(d) = self.get_mut(id) {
            d.base_mut().locked = locked;
            self.emit();
        }
    }

    pub fn set_visible(&mut self, id: &str, visible: bool) {
        if let Some(d) = self.get_mut(id) {
            d.base_mut().visible = visible;
            self.emit();
        }
    }

    pub fn clear(&mut self) {
        if self.drawings.is_empty() {
            return;
        }
        self.drawings.clear();
        self.emit();
    }

    pub fn serialize(&self) -> DrawingsDocument {
        DrawingsDocument {
            version: DRAWINGS_DOC_VERSION,
            drawings: self.all().into_iter().map(|d| d.serialize()).collect(),
        }
    }

    /// Replace all drawings from an untrusted document. Malformed entries and
    /// unknown types are skipped.
    pub fn load(&mut self, registry: &DrawingRegistry, doc: &Value) {
        self.load_document(registry, &migrate(doc));
    }

    /// Replace all drawings from a validated document (an undo snapshot).
    pub fn load_document(&mut self, registry: &DrawingRegistry, doc: &DrawingsDocument) {
        self.drawings.clear();
        self.seq = 0;
        self.z_counter = 1.0;
        for record in &doc.drawings {
            let Some(d) = registry.deserialize(record) else {
                continue;
            }; // unknown type: skip
            self.z_counter = self.z_counter.max(d.base().z_index) + 1.0;
            self.bump_seq_past(&d.base().id);
            match self
                .drawings
                .iter()
                .position(|x| x.base().id == d.base().id)
            {
                Some(index) => self.drawings[index] = d,
                None => self.drawings.push(d),
            }
        }
        self.emit();
    }

    /// Keep `next_id()` from colliding with a loaded `dw-N` id.
    fn bump_seq_past(&mut self, id: &str) {
        let digits = id
            .strip_prefix("dw-")
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        if let Some(n) = digits.and_then(|d| d.parse::<u64>().ok()) {
            self.seq = self.seq.max(n);
        }
    }
}
