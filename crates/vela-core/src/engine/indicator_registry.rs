//! Per-indicator instance state held by the orchestrator, and the registry that
//! holds it (`IndicatorRegistry.ts`).
//!
//! The original hands out the record objects themselves and the orchestrator
//! mutates them in place from many callbacks. The port keeps that shape: the
//! registry stores [`RecordRef`]s (`Rc<RefCell<IndicatorRecord>>`) and clones of
//! one are the same record. Hold a borrow only for the read or write at hand:
//! never across a call into an engine, a renderer or a user callback.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::indicator_handle::CodeUpdateResult;
use crate::indicators::{NativeIndicator, NativeIndicatorDescriptor};
use crate::model::{IndicatorModel, InputSchema, Inputs};
use crate::options::AddIndicatorOptions;
use crate::ports::chart_renderer::IndicatorRenderHandle;
use crate::ports::scripting_engine::{ExecutionSession, PreparedScript, ScriptingEngine};
use crate::script_run::ScriptRunCause;

/// Everything `updateCode` swaps: what a failed update restores.
#[derive(Clone, Default)]
pub struct CodeSnapshot {
    pub source: String,
    pub engine: Option<Rc<dyn ScriptingEngine>>,
    pub prepared: Option<PreparedScript>,
    pub input_values: Inputs,
    pub prop_values: Inputs,
    pub inputs: Vec<InputSchema>,
    pub props: Vec<InputSchema>,
}

/// A running native instance, shared so a lifecycle call can run without a
/// borrow of the record.
pub type SharedNativeIndicator = Rc<RefCell<Box<dyn NativeIndicator>>>;

/// Settles one caller of `updateCode` awaiting the current chain of edits.
pub type CodeWaiter = Box<dyn FnOnce(CodeUpdateResult)>;

/// The state of a NATIVE (core-computed) indicator: its type, the running
/// instance, and the descriptor (metadata + capability). The two paths, native
/// and script, share the registry, handle, legend, and lifecycle events.
pub struct NativeRecord {
    /// The registered type (`volume`, `vpvr`, ...).
    pub indicator_type: String,
    pub instance: SharedNativeIndicator,
    pub descriptor: NativeIndicatorDescriptor,
    /// An instance re-created for a NEW market while the indicator was hidden:
    /// showing it must START the fresh instance instead of resuming the old
    /// market's compute.
    pub stale: bool,
    pub started: bool,
    /// A `waits_for_history` instance whose start is deferred until the chart's
    /// history completes (its legend row is a loading placeholder meanwhile).
    pub held_for_history: bool,
    /// Kills the running instance's context once it is stopped.
    pub revoke_context: Option<Rc<dyn Fn()>>,
}

impl NativeRecord {
    pub fn new(
        indicator_type: impl Into<String>,
        instance: Box<dyn NativeIndicator>,
        descriptor: NativeIndicatorDescriptor,
    ) -> Self {
        Self {
            indicator_type: indicator_type.into(),
            instance: Rc::new(RefCell::new(instance)),
            descriptor,
            stale: false,
            started: false,
            held_for_history: false,
            revoke_context: None,
        }
    }
}

/// A swapped-in source whose first run has not landed yet, with the state a
/// failure puts back: the last code KNOWN to run, kept across superseding edits.
pub struct CodeTrial {
    pub previous: CodeSnapshot,
}

/// Per-indicator instance state held by the orchestrator.
#[derive(Default)]
pub struct IndicatorRecord {
    pub id: String,
    pub title: String,
    pub source: String,
    /// Present for a NATIVE indicator; `None` is an ordinary script indicator
    /// (driven via `engine` / `prepared` / `session`).
    pub native: Option<NativeRecord>,
    /// Routing/inputs options from `addIndicator` (used to re-route on a fresh
    /// first model).
    pub options: Option<AddIndicatorOptions>,
    /// The engine selected for this indicator's language.
    pub engine: Option<Rc<dyn ScriptingEngine>>,
    pub prepared: Option<PreparedScript>,
    pub model: Option<IndicatorModel>,
    pub render_handle: Option<IndicatorRenderHandle>,
    pub input_values: Inputs,
    /// Declaration-prop values (effective defaults merged with user/add-time
    /// overrides). Stays empty for engines without props support and for natives.
    pub prop_values: Inputs,
    /// A source handed to `updateCode` that is still being prepared. The latest
    /// call wins: a prepare that resolves for any other source is discarded, so
    /// two quick edits never race the older one onto the chart.
    pub pending_source: Option<String>,
    /// Callers of `updateCode` awaiting the current chain of edits. One list on
    /// purpose: the latest edit's outcome settles every call it superseded.
    pub code_waiters: Vec<CodeWaiter>,
    pub code_trial: Option<CodeTrial>,
    /// The live execution session (static or streaming): poked on input,
    /// viewport and bar changes.
    pub session: Option<Rc<dyn ExecutionSession>>,
    /// What the NEXT emitted model should be attributed to on `script:run`. Set
    /// by whoever pokes the session (a bar change, an input edit, a viewport move,
    /// a market switch); a model that arrives with none was produced by the
    /// session's own first execution. Attribution is by last poke: the pokes are
    /// what make a model arrive at all.
    pub pending_cause: Option<ScriptRunCause>,
    /// The next emitted model should structurally remount (after an input
    /// change) rather than value-patch (live tick / viewport re-run). Consumed by
    /// the model handler.
    pub pending_structural: bool,
    /// Hidden via `handle.set_visible(false)` / the legend eye. A hidden
    /// indicator has its session torn down (no `session`) and its visuals dropped:
    /// it consumes no resources until shown again, which re-executes it.
    pub hidden: bool,
    /// A compute is in flight and the mounted visuals are a placeholder (or
    /// stale): the legend row shows a loading spinner, and `inspect()` skips the
    /// record. Cleared by the next emitted model (or a failure).
    pub loading: bool,
    /// `indicator:added` + the handle's `Ready` have fired (they fire once, on
    /// the first COMPUTED model).
    pub announced: bool,
    /// The user explicitly placed this indicator in a pane (via `moveIndicator` /
    /// `handle.move_to`). Once locked, recomputes (live ticks, input changes) must
    /// keep the chosen pane id and own-scale instead of re-deriving them from the
    /// freshly emitted model's routing.
    pub pane_locked: bool,
}

impl IndicatorRecord {
    /// A record with only the identity set; every other field at its "absent"
    /// value.
    pub fn new(id: impl Into<String>, title: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            source: source.into(),
            ..Self::default()
        }
    }

    /// A snapshot of what `updateCode` swaps, for [`CodeTrial::previous`].
    pub fn code_snapshot(&self) -> CodeSnapshot {
        CodeSnapshot {
            source: self.source.clone(),
            engine: self.engine.clone(),
            prepared: self.prepared.clone(),
            input_values: self.input_values.clone(),
            prop_values: self.prop_values.clone(),
            inputs: self
                .prepared
                .as_ref()
                .map(|p| p.inputs.clone())
                .unwrap_or_default(),
            props: self
                .prepared
                .as_ref()
                .and_then(|p| p.props.clone())
                .unwrap_or_default(),
        }
    }
}

/// A record shared between the registry and the orchestrator's callbacks.
pub type RecordRef = Rc<RefCell<IndicatorRecord>>;

/// The orchestrator's indicator records, in insertion order. Methods take
/// `&self`: the registry never calls out, so its interior borrows are short.
#[derive(Default)]
pub struct IndicatorRegistry {
    records: RefCell<Vec<RecordRef>>,
    counter: Cell<u64>,
}

impl IndicatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a unique per-instance id. Host-supplied ids share the namespace, so a
    /// minted id skips anything already recorded, and anything `taken` reports
    /// live elsewhere (a handle the orchestrator holds outside the records, e.g.
    /// a fail-soft native). The original's default prefix is `"ind"`.
    pub fn next_id(&self, prefix: &str, taken: impl Fn(&str) -> bool) -> String {
        loop {
            self.counter.set(self.counter.get() + 1);
            let id = format!("{prefix}-{}", self.counter.get());
            if self.get(&id).is_none() && !taken(&id) {
                return id;
            }
        }
    }

    /// Record an indicator (replacing one with the same id in place) and return
    /// its shared reference.
    pub fn add(&self, record: IndicatorRecord) -> RecordRef {
        let id = record.id.clone();
        let record = Rc::new(RefCell::new(record));
        let mut records = self.records.borrow_mut();
        match records.iter().position(|r| r.borrow().id == id) {
            Some(i) => records[i] = record.clone(),
            None => records.push(record.clone()),
        }
        record
    }

    pub fn get(&self, id: &str) -> Option<RecordRef> {
        self.records
            .borrow()
            .iter()
            .find(|r| r.borrow().id == id)
            .cloned()
    }

    pub fn remove(&self, id: &str) -> Option<RecordRef> {
        let mut records = self.records.borrow_mut();
        let i = records.iter().position(|r| r.borrow().id == id)?;
        Some(records.remove(i))
    }

    /// Every record, in insertion order.
    pub fn all(&self) -> Vec<RecordRef> {
        self.records.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::volume::volume_descriptor;

    fn record(id: &str) -> IndicatorRecord {
        IndicatorRecord::new(id, id.to_uppercase(), "src")
    }

    #[test]
    fn minted_ids_count_up_and_skip_taken_names() {
        let registry = IndicatorRegistry::new();
        assert_eq!(registry.next_id("ind", |_| false), "ind-1");
        assert_eq!(registry.next_id("ind", |_| false), "ind-2");
        assert_eq!(registry.next_id("nat", |_| false), "nat-3");
        // A host-supplied id shares the namespace.
        registry.add(record("ind-4"));
        assert_eq!(registry.next_id("ind", |_| false), "ind-5");
        // So does a handle held outside the records.
        assert_eq!(
            registry.next_id("ind", |id| id == "ind-6" || id == "ind-7"),
            "ind-8"
        );
    }

    #[test]
    fn records_are_shared_and_mutated_in_place() {
        let registry = IndicatorRegistry::new();
        let added = registry.add(record("a"));
        added.borrow_mut().hidden = true;
        let fetched = registry.get("a").expect("recorded");
        assert!(Rc::ptr_eq(&added, &fetched));
        assert!(fetched.borrow().hidden);
        fetched.borrow_mut().pending_cause = Some(ScriptRunCause::Tick);
        assert_eq!(added.borrow().pending_cause, Some(ScriptRunCause::Tick));
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn all_keeps_insertion_order_and_a_replacement_keeps_its_place() {
        let registry = IndicatorRegistry::new();
        for id in ["c", "a", "b"] {
            registry.add(record(id));
        }
        let ids = |r: &IndicatorRegistry| {
            r.all()
                .iter()
                .map(|x| x.borrow().id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&registry), ["c", "a", "b"]);
        let mut replacement = record("a");
        replacement.title = "Replaced".into();
        registry.add(replacement);
        assert_eq!(ids(&registry), ["c", "a", "b"]);
        assert_eq!(registry.get("a").unwrap().borrow().title, "Replaced");
    }

    #[test]
    fn remove_hands_back_the_record_once() {
        let registry = IndicatorRegistry::new();
        registry.add(record("a"));
        let removed = registry.remove("a").expect("was there");
        assert_eq!(removed.borrow().id, "a");
        assert!(registry.remove("a").is_none());
        assert!(registry.get("a").is_none());
        assert!(registry.all().is_empty());
    }

    #[test]
    fn a_new_record_has_every_flag_clear() {
        let r = IndicatorRecord::new("x", "X", "code");
        assert_eq!(
            (r.id.as_str(), r.title.as_str(), r.source.as_str()),
            ("x", "X", "code")
        );
        assert!(!r.hidden && !r.loading && !r.announced && !r.pane_locked && !r.pending_structural);
        assert!(r.native.is_none() && r.session.is_none() && r.model.is_none());
        assert!(r.pending_source.is_none() && r.pending_cause.is_none() && r.code_trial.is_none());
        assert!(r.code_waiters.is_empty());
    }

    #[test]
    fn a_native_record_wraps_a_fresh_instance() {
        let descriptor = volume_descriptor();
        let native = NativeRecord::new("volume", descriptor.create(), descriptor);
        assert_eq!(native.indicator_type, "volume");
        assert!(!native.stale && !native.started && !native.held_for_history);
        assert!(native.revoke_context.is_none());
    }

    #[test]
    fn a_code_snapshot_captures_what_update_code_swaps() {
        let mut r = IndicatorRecord::new("x", "X", "old source");
        r.input_values
            .insert("len".into(), crate::model::InputValue::Number(9.0));
        let snapshot = r.code_snapshot();
        assert_eq!(snapshot.source, "old source");
        assert_eq!(snapshot.input_values.len(), 1);
        assert!(
            snapshot.prepared.is_none() && snapshot.inputs.is_empty() && snapshot.props.is_empty()
        );
    }

    #[test]
    fn code_waiters_settle_once_with_the_outcome() {
        let outcome: Rc<RefCell<Vec<CodeUpdateResult>>> = Rc::default();
        let mut r = record("x");
        for _ in 0..2 {
            let sink = outcome.clone();
            r.code_waiters
                .push(Box::new(move |result| sink.borrow_mut().push(result)));
        }
        for waiter in std::mem::take(&mut r.code_waiters) {
            waiter(CodeUpdateResult::failure("bad"));
        }
        assert_eq!(outcome.borrow().len(), 2);
        assert!(r.code_waiters.is_empty());
    }
}
