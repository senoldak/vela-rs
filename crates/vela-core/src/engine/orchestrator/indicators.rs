//! The indicators section of the orchestrator: engine registration, add/remove/
//! list indicators (script and native), inputs and props, `update_code` and the
//! code-swap machinery, `start_indicator`, `execute_indicator`, the native
//! indicator lifecycle and placeholders, `apply_model`, `place_model`,
//! `route_pane`, script-run and context events, `fail`, visibility.
//!
//! # The native indicator context
//!
//! The original hands each native instance one context object (at `start`) whose
//! `emit` / `pushData` / `setStatus` call straight back into the orchestrator and
//! which is revoked when the instance is stopped. A Rust instance does not keep
//! its host: every lifecycle call takes a `&mut dyn NativeIndicatorContext`, and
//! the instance itself sits in a `RefCell` borrowed mutably for the call.
//!
//! The context lent to a call therefore BUFFERS what the instance pushes, in
//! order, and the orchestrator applies the buffer right after the call returns
//! (the instance borrow released), still inside the same orchestrator method.
//! What the renderer and the event bus see is the original's sequence: the
//! instance only touches the outside world through those three calls, so running
//! them after its own code instead of in the middle changes nothing observable
//! to them, and applying them with the instance unborrowed lets a listener
//! reacting to `indicator:added` (hide, remove, edit inputs) re-enter the same
//! instance, as it can in the original. Liveness is checked per applied op, so
//! an op after a revocation (the instance was stopped by a listener meanwhile)
//! is dropped; the pushes made inside `stop()` itself apply, because the
//! revocation comes after the stop call. The per-start data of the context
//! (symbol, timeframe, live, session, channel, the liveness flag) is captured at
//! start and kept in `State::native_contexts`.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::task::Poll;

use super::*;
use crate::engine::indicator_handle::{IndicatorAlert, IndicatorEvent};
use crate::engine::indicator_registry::{CodeSnapshot, CodeTrial, IndicatorRecord, NativeRecord};
use crate::events::types::{AlertEvent, ChartEventName};
use crate::i18n::{MessageLike, message_only, text_of};
use crate::indicators::{
    IndicatorStatus, NativeIndicator, NativeIndicatorContext, NativeIndicatorInfo,
    NativeIndicatorOutput, VisibleRange,
};
use crate::model::strategy::StrategyTrade;
use crate::model::{
    AnchorUpdate, DirtyRange, IndicatorModel, InputSchema, InputValue, NativeTag, PaneHint,
    ScenePatch, SeriesData, SeriesValueDelta, SeriesValues, ValuePatch,
};
use crate::ports::market_data::BarRange;
use crate::ports::scripting_engine::{ExecutionHandlers, ExecutionMode, ExecutionRequest};
use crate::runtime::{js_await, sleep};
use crate::script_run::{PlotSeriesPoint, ScriptKind, ScriptRun, SeriesLoader, TradesLoader};

/// Options of `add_native_indicator`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NativeIndicatorOptions {
    /// Input overrides on top of the descriptor's defaults.
    pub inputs: Option<Inputs>,
}

/// What a native instance was started with, plus its liveness: the original's
/// context object minus the per-call parts (see the module doc).
pub(super) struct NativeCtxMeta {
    id: String,
    symbol: String,
    timeframe: String,
    live: bool,
    session: Option<String>,
    /// The renderer-layer channel `push_data` lands on.
    channel: String,
    /// False once the instance is stopped: nothing it left in flight may reach
    /// the scene (a stopped market's payload, or a removed indicator's).
    alive: Cell<bool>,
}

/// What an instance pushed through its context during one call.
enum NativeOp {
    Emit(Box<NativeIndicatorOutput>),
    Push(Value),
    Status(IndicatorStatus),
}

/// The context lent to one lifecycle call.
struct NativeContext {
    meta: Rc<NativeCtxMeta>,
    bars: Vec<Ohlcv>,
    data: DataControl,
    ops: Vec<NativeOp>,
}

impl NativeIndicatorContext for NativeContext {
    fn id(&self) -> &str {
        &self.meta.id
    }

    fn symbol(&self) -> &str {
        &self.meta.symbol
    }

    fn timeframe(&self) -> &str {
        &self.meta.timeframe
    }

    fn live(&self) -> bool {
        self.meta.live
    }

    fn session(&self) -> Option<&str> {
        self.meta.session.as_deref()
    }

    fn bars(&self) -> &[Ohlcv] {
        &self.bars
    }

    fn data(&self) -> DataControl {
        self.data.clone()
    }

    fn emit(&mut self, out: NativeIndicatorOutput) {
        self.ops.push(NativeOp::Emit(Box::new(out)));
    }

    fn push_data(&mut self, data: Value) {
        self.ops.push(NativeOp::Push(data));
    }

    fn set_status(&mut self, status: IndicatorStatus) {
        self.ops.push(NativeOp::Status(status));
    }
}

/// `Promise.all`: every future is polled from the start, the answers keep their order.
async fn join_all<T>(mut futures: Vec<LocalBoxFuture<'static, T>>) -> Vec<T> {
    let mut out: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, slot) in futures.iter_mut().zip(out.iter_mut()) {
            if slot.is_none() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => *slot = Some(value),
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

fn ready<T: 'static>(value: T) -> LocalBoxFuture<'static, T> {
    Box::pin(std::future::ready(value))
}

/// Whether `prepared` is the very script `candidate` holds (the original compares
/// the objects; every `prepare` makes a fresh token).
fn same_prepared(
    candidate: &Option<crate::ports::scripting_engine::PreparedScript>,
    prepared: &crate::ports::scripting_engine::PreparedScript,
) -> bool {
    candidate
        .as_ref()
        .is_some_and(|c| Rc::ptr_eq(&c.token, &prepared.token))
}

impl EngineOrchestrator {
    /// Register a scripting engine for a language. Last registration wins:
    /// re-registering replaces the engine (with a dev-console warning). Running
    /// indicators keep the engine they were started with: only later
    /// `add_indicator` calls use the replacement, so a swap never disrupts live
    /// work.
    pub fn register_engine(&self, language: &str, engine: Rc<dyn ScriptingEngine>) {
        let mut state = self.inner.state.borrow_mut();
        insert_engine(&self.inner.platform, &mut state.engines, language, engine);
    }

    /// Add a script indicator. The handle is usable synchronously; data renders
    /// when execution resolves. A host-supplied id that is empty or already live
    /// on the chart is refused (never renamed behind the caller's back).
    pub fn add_indicator(
        &self,
        source: &str,
        options: AddIndicatorOptions,
    ) -> Result<Rc<IndicatorHandleImpl>, IndicatorIdError> {
        let id = self.claim_indicator_id(options.id.as_deref())?;
        let title = options
            .title
            .clone()
            .unwrap_or_else(|| "Indicator".to_string());
        // The same resolution `engine_for` applies at prepare: `default_language`
        // never changes.
        let language = options
            .language
            .clone()
            .unwrap_or_else(|| self.inner.default_language.clone());
        let handle = Rc::new(IndicatorHandleImpl::new(
            id.clone(),
            title.clone(),
            self.indicator_controller(),
            Some(source.to_string()),
            None,
            Some(language),
        ));
        self.inner
            .state
            .borrow_mut()
            .handles
            .push(Rc::clone(&handle));
        let mut record = IndicatorRecord::new(id.clone(), title, source);
        record.input_values = options.inputs.clone().unwrap_or_default();
        record.prop_values = options.props.clone().unwrap_or_default();
        record.options = Some(options.clone());
        self.inner.registry.add(record);
        // `void this.startIndicator(...)`: it awaits `ready` first, so nothing of
        // it runs before this returns.
        self.detach(self.start_indicator(&id, source, options, Rc::clone(&handle)));
        Ok(handle)
    }

    /// Add a NATIVE (core-computed, no script engine) indicator by registered
    /// type: a first-class indicator that shares the registry, handle, legend,
    /// settings, and lifecycle events. SINGLE INSTANCE per type unless the
    /// descriptor opts into `multi_instance`: a second add of a single-instance
    /// type returns the existing handle; a multi-instance type gets a fresh
    /// instance every time. Unknown type: a fail-soft handle that never mounts.
    /// Built-in single-instance types: `volume`, `vpvr`.
    pub fn add_native_indicator(
        &self,
        native_type: &str,
        options: NativeIndicatorOptions,
    ) -> Rc<IndicatorHandleImpl> {
        let descriptor = self
            .inner
            .native_registry
            .borrow()
            .get(native_type)
            .cloned();
        if !descriptor.as_ref().is_some_and(|d| d.multi_instance) {
            let existing = self.inner.registry.all().into_iter().find(|r| {
                r.borrow()
                    .native
                    .as_ref()
                    .is_some_and(|n| n.indicator_type == native_type)
            });
            if let Some(existing) = existing {
                let (id, title) = {
                    let r = existing.borrow();
                    (r.id.clone(), r.title.clone())
                };
                return self.handle_of(&id).unwrap_or_else(|| {
                    Rc::new(IndicatorHandleImpl::new(
                        id,
                        title,
                        self.indicator_controller(),
                        None,
                        Some(native_type.to_string()),
                        None,
                    ))
                });
            }
        }

        let id = self
            .inner
            .registry
            .next_id("native", |candidate| self.handle_of(candidate).is_some());
        let title = match &descriptor {
            Some(d) => text_of(self.translator().as_deref(), &d.title, &[]),
            None => native_type.to_string(),
        };
        let handle = Rc::new(IndicatorHandleImpl::new(
            id.clone(),
            title.clone(),
            self.indicator_controller(),
            None,
            Some(native_type.to_string()),
            None,
        ));
        self.inner
            .state
            .borrow_mut()
            .handles
            .push(Rc::clone(&handle));
        let Some(descriptor) = descriptor else {
            self.warn(&format!(
                "[vela] addNativeIndicator(\"{native_type}\") \u{2014} no native indicator registered for this type."
            ));
            return handle;
        };
        let mut input_values = descriptor.default_inputs();
        input_values.extend(options.inputs.unwrap_or_default());
        let instance = descriptor.create();
        let mut record = IndicatorRecord::new(id.clone(), title, native_type);
        record.input_values = input_values;
        record.native = Some(NativeRecord::new(native_type, instance, descriptor.clone()));
        self.inner.registry.add(record);
        handle.set_schema(descriptor.inputs_schema.clone());
        self.detach(self.start_native_indicator(&id, Rc::clone(&handle)));
        handle
    }

    /// The native types present on the chart, one entry per INSTANCE in
    /// registry order (a multi-instance type repeats): SYNC (registry state; no
    /// support probe).
    pub fn present_native_indicators(&self) -> Vec<String> {
        self.inner
            .registry
            .all()
            .iter()
            .filter_map(|r| r.borrow().native.as_ref().map(|n| n.indicator_type.clone()))
            .collect()
    }

    /// The catalog of registered native-indicator types with their live state on
    /// THIS chart: each entry's `supported` (applies to the current symbol) and
    /// `present` (at least one instance added: a second add of a single-instance
    /// type is a no-op). Powers a host "add native indicator" menu that can gate
    /// and de-duplicate. Async because a descriptor's support check may probe the
    /// provider for a required capability (`NativeIndicatorRegistry::is_supported`
    /// does the per-type rule: no check, always supported; a check but no symbol,
    /// unsupported; a failing check, unsupported).
    pub fn available_native_indicators(&self) -> LocalBoxFuture<'static, Vec<NativeIndicatorInfo>> {
        let symbol = self.qualified_symbol();
        let present: HashSet<String> = self.present_native_indicators().into_iter().collect();
        let i18n = self.translator();
        let descriptors = self.inner.native_registry.borrow().descriptors().to_vec();
        // Every support probe starts now, as `Promise.all` over the map does.
        // Each `await this.isNativeSupported(...)` is a turn of its own.
        let probes: Vec<LocalBoxFuture<'static, bool>> = {
            let registry = self.inner.native_registry.borrow();
            descriptors
                .iter()
                .map(|d| {
                    Box::pin(js_await(registry.is_supported(
                        &d.indicator_type,
                        symbol.as_deref(),
                        &self.inner.data_control,
                    ))) as LocalBoxFuture<'static, bool>
                })
                .collect()
        };
        Box::pin(async move {
            let supported = join_all(probes).await;
            descriptors
                .into_iter()
                .zip(supported)
                .map(|(d, supported)| NativeIndicatorInfo {
                    title: text_of(i18n.as_deref(), &d.title, &[]),
                    supported,
                    present: present.contains(&d.indicator_type),
                    multi_instance: d.multi_instance,
                    beta: d.beta,
                    indicator_type: d.indicator_type,
                })
                .collect()
        })
    }

    /// IndicatorController: re-run an indicator with merged input overrides
    /// (Pine session OR native instance).
    pub fn apply_inputs(&self, id: &str, values: Inputs) {
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        {
            let r = record.borrow();
            if r.session.is_none() && r.native.is_none() {
                return;
            }
        }
        let (render_handle, input_values) = {
            let mut r = record.borrow_mut();
            r.input_values.extend(values);
            (r.render_handle.clone(), r.input_values.clone())
        };
        // Reflect the new value in the open dialog immediately (the model arrives async).
        if let Some(handle) = &render_handle {
            self.inner
                .renderer
                .set_indicator_inputs(handle, &input_values, None);
        }
        // The next emitted model structurally remounts (input changes can restructure).
        let (hidden, is_native) = {
            let mut r = record.borrow_mut();
            r.pending_structural = true;
            (r.hidden, r.native.is_some())
        };
        // Spinner during the re-compute (Pine only: native indicators drive their own status).
        if !hidden && !is_native {
            self.set_loading(&record, true);
        }
        let session = {
            let mut r = record.borrow_mut();
            r.pending_cause = Some(ScriptRunCause::Inputs);
            r.session.clone()
        };
        if let Some(session) = session {
            session.update(&input_values, None);
        } else if is_native && !hidden {
            self.run_native(&record, move |instance, ctx| {
                instance.set_inputs(ctx, input_values);
            });
        }
        self.emit(ChartEvent::IndicatorInputs { id: id.to_string() });
    }

    /// IndicatorController: the CURRENT stored input values (defaults merged
    /// with edits).
    pub fn input_values_of(&self, id: &str) -> Inputs {
        self.inner
            .registry
            .get(id)
            .map(|r| r.borrow().input_values.clone())
            .unwrap_or_default()
    }

    /// IndicatorController: the CURRENT declaration-prop overrides.
    pub fn prop_values_of(&self, id: &str) -> Inputs {
        self.inner
            .registry
            .get(id)
            .map(|r| r.borrow().prop_values.clone())
            .unwrap_or_default()
    }

    /// IndicatorController: re-run an indicator with merged declaration-prop
    /// overrides. Same lifecycle as `apply_inputs`: a prop change replays the
    /// whole script. Script indicators only: natives have no declaration props.
    pub fn apply_props(&self, id: &str, values: Inputs) {
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        let session = match record.borrow().session.clone() {
            Some(session) => session,
            None => return,
        };
        let (render_handle, input_values, prop_values) = {
            let mut r = record.borrow_mut();
            r.prop_values.extend(values);
            (
                r.render_handle.clone(),
                r.input_values.clone(),
                r.prop_values.clone(),
            )
        };
        // Reflect the new value in the open dialog immediately (the model arrives async).
        if let Some(handle) = &render_handle {
            self.inner
                .renderer
                .set_indicator_inputs(handle, &input_values, Some(&prop_values));
        }
        let hidden = {
            let mut r = record.borrow_mut();
            r.pending_structural = true;
            r.hidden
        };
        if !hidden {
            self.set_loading(&record, true);
        }
        record.borrow_mut().pending_cause = Some(ScriptRunCause::Inputs);
        session.update(&input_values, Some(&prop_values));
        self.emit(ChartEvent::IndicatorInputs { id: id.to_string() });
    }

    /// IndicatorController: replace a SCRIPT indicator's source in place.
    /// Prepare-first: the running session keeps computing until the new source
    /// has compiled; then the new code runs on TRIAL until its first model
    /// lands, and a failure before that puts the previous code back. Natives
    /// have no script: warn and leave them alone.
    pub fn update_code(&self, id: &str, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult> {
        let (Some(record), Some(handle)) = (self.inner.registry.get(id), self.handle_of(id)) else {
            return ready(CodeUpdateResult::failure(format!(
                "[vela] updateCode(\"{id}\") \u{2014} the indicator is no longer on the chart."
            )));
        };
        if record.borrow().native.is_some() {
            let message = format!(
                "[vela] updateCode(\"{id}\") \u{2014} a native indicator has no script to update."
            );
            self.warn(&message);
            return ready(CodeUpdateResult::failure(message));
        }
        let (busy, same_source) = {
            let r = record.borrow();
            (
                r.pending_source.is_some() || r.code_trial.is_some(),
                source == r.source,
            )
        };
        if !busy && same_source {
            return ready(CodeUpdateResult::success());
        }
        let (tx, rx) = crate::runtime::oneshot::<CodeUpdateResult>();
        let joins = {
            let mut r = record.borrow_mut();
            r.code_waiters.push(Box::new(move |result| tx.send(result)));
            // Already on its way (being prepared, or on trial with nothing newer
            // pending): this call simply joins the chain.
            let joins = r.pending_source.as_deref() == Some(source)
                || (r.pending_source.is_none() && r.code_trial.is_some() && r.source == source);
            if !joins {
                r.pending_source = Some(source.to_string());
            }
            joins
        };
        if !joins {
            // `void this.swapSource(...)`: its first step (the prepare call) runs
            // inside `swap_source` before the future is built.
            self.spawn(self.swap_source(id, source, handle));
        }
        Box::pin(rx)
    }

    /// Prepare `source` and swap it in under the running indicator (the second
    /// half of `update_code`: dispatched detached, `void this.swapSource(...)`).
    pub(super) fn swap_source(
        &self,
        id: &str,
        source: &str,
        handle: Rc<IndicatorHandleImpl>,
    ) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        let (id, source) = (id.to_string(), source.to_string());
        // The engine lookup and the `prepare` call are the synchronous prefix.
        let (engine, prepare) = {
            let existing = this
                .inner
                .registry
                .get(&id)
                .and_then(|r| r.borrow().engine.clone());
            let language = this
                .inner
                .registry
                .get(&id)
                .and_then(|r| r.borrow().options.as_ref().and_then(|o| o.language.clone()));
            let engine = match existing {
                Some(engine) => Ok(engine),
                None => this.engine_for(language.as_deref()),
            };
            match engine {
                Ok(engine) => {
                    let prepare = engine.prepare(&source, &id);
                    (Ok(engine), Some(prepare))
                }
                Err(e) => (Err(e), None),
            }
        };
        Box::pin(async move {
            let prepared = match (engine.as_ref().ok(), prepare) {
                (Some(_), Some(prepare)) => js_await(prepare).await.map_err(|e| e.message),
                // An engine the language has none of: the same failure path as a
                // compile error (the original lets the rejection escape and the
                // waiters hang).
                _ => Err(engine
                    .as_ref()
                    .err()
                    .map(|e| e.message.clone())
                    .unwrap_or_default()),
            };
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(message) => {
                    if let Some(record) = this.inner.registry.get(&id) {
                        // Only the LATEST pending edit reports; a superseded one failing is noise.
                        let latest = record.borrow().pending_source.as_deref() == Some(&source);
                        if latest {
                            record.borrow_mut().pending_source = None;
                            this.reject_code(&id, &record, &handle, &message);
                        }
                    }
                    return;
                }
            };
            let Ok(engine) = engine else { return };
            let Some(record) = this.inner.registry.get(&id) else {
                return;
            };
            // Removed, or superseded by a later update_code, while preparing.
            if record.borrow().pending_source.as_deref() != Some(&source) {
                return;
            }
            let old_session = {
                let mut r = record.borrow_mut();
                r.pending_source = None;
                // The restore point is the last code KNOWN to run: an edit landing
                // while another is still on trial keeps that trial's restore point,
                // not the unproven trial.
                let previous: CodeSnapshot = match r.code_trial.as_ref() {
                    Some(trial) => trial.previous.clone(),
                    None => CodeSnapshot {
                        source: r.source.clone(),
                        engine: r.engine.clone(),
                        prepared: r.prepared.clone(),
                        input_values: r.input_values.clone(),
                        prop_values: r.prop_values.clone(),
                        inputs: handle.inputs(),
                        props: handle.props(),
                    },
                };
                // Values survive on the keys (or titles) the new schema still
                // declares, unless they still sit on the OLD declared default:
                // those follow the declaration. Settled here, before the first
                // run, so the new code computes once with its final values.
                let old_inputs: Vec<InputSchema> = r
                    .prepared
                    .as_ref()
                    .map(|p| p.inputs.clone())
                    .unwrap_or_default();
                let old_props: Vec<InputSchema> = r
                    .prepared
                    .as_ref()
                    .and_then(|p| p.props.clone())
                    .unwrap_or_default();
                let new_props = prepared.props.clone().unwrap_or_default();
                r.input_values = values_on_schema(&prepared.inputs, &r.input_values, &old_inputs);
                r.prop_values = values_on_schema(&new_props, &r.prop_values, &old_props);
                r.source = source.clone();
                r.engine = Some(engine);
                r.prepared = Some(prepared.clone());
                handle.set_source(source.clone());
                handle.set_schema(prepared.inputs.clone());
                handle.set_props_schema(new_props);
                let old_session = r.session.take();
                // The first model of the new source remounts over the old visuals.
                r.pending_structural = true;
                r.pending_cause = Some(ScriptRunCause::Code);
                // Stash the restore point; the trial itself starts below.
                r.code_trial = Some(CodeTrial { previous });
                old_session
            };
            if let Some(session) = old_session {
                session.stop();
            }
            // A hidden indicator computes nothing, so the compile check is all
            // there is to wait for: it adopts the new code now, and showing runs it.
            let (hidden, render_handle, input_values, prop_values) = {
                let mut r = record.borrow_mut();
                if r.hidden {
                    r.code_trial = None;
                }
                (
                    r.hidden,
                    r.render_handle.clone(),
                    r.input_values.clone(),
                    r.prop_values.clone(),
                )
            };
            if hidden {
                if let Some(rh) = &render_handle {
                    this.inner
                        .renderer
                        .set_indicator_inputs(rh, &input_values, Some(&prop_values));
                }
                this.settle_code(&record, CodeUpdateResult::success());
                return;
            }
            if let Some(rh) = &render_handle {
                this.inner
                    .renderer
                    .set_indicator_inputs(rh, &input_values, Some(&prop_values));
                this.set_loading(&record, true);
            } else {
                // Updated before its first prepare ever landed.
                this.mount_loading_placeholder(&id, &record);
            }
            this.execute_indicator(&id, &handle);
            // No bars, no run: the session emits nothing until bars land, so there
            // is no first model to wait for: the compile check stands in for it, as
            // for a hidden one.
            let no_bars = this.inner.state.borrow().bars.is_empty();
            if no_bars && record.borrow().code_trial.is_some() {
                record.borrow_mut().code_trial = None;
                this.settle_code(&record, CodeUpdateResult::success());
            }
        })
    }

    /// Resolve every `update_code` caller waiting on this record's chain of
    /// edits.
    pub(super) fn settle_code(&self, record: &RecordRef, result: CodeUpdateResult) {
        let waiters = std::mem::take(&mut record.borrow_mut().code_waiters);
        for waiter in waiters {
            waiter(result.clone());
        }
    }

    /// An update failed: at prepare, or on trial before its first model. A trial
    /// puts the previous code back (source, engine, prepared script, values,
    /// schema) and restarts it; its visuals were never dropped, so the chart
    /// keeps painting throughout. Then the failure reports the usual way and
    /// every waiting caller learns it.
    pub(super) fn reject_code(
        &self,
        id: &str,
        record: &RecordRef,
        handle: &Rc<IndicatorHandleImpl>,
        message: &str,
    ) {
        let trial = record.borrow_mut().code_trial.take();
        self.fail(id, Some(handle), message);
        if let Some(trial) = trial {
            let p = trial.previous;
            let old_session = record.borrow_mut().session.take();
            if let Some(session) = old_session {
                session.stop();
            }
            let (render_handle, input_values, prop_values, hidden) = {
                let mut r = record.borrow_mut();
                r.source = p.source.clone();
                r.engine = p.engine;
                r.prepared = p.prepared;
                r.input_values = p.input_values;
                r.prop_values = p.prop_values;
                r.pending_structural = true;
                r.pending_cause = None;
                (
                    r.render_handle.clone(),
                    r.input_values.clone(),
                    r.prop_values.clone(),
                    r.hidden,
                )
            };
            handle.set_source(p.source);
            handle.set_schema(p.inputs);
            handle.set_props_schema(p.props);
            if let Some(rh) = &render_handle {
                self.inner
                    .renderer
                    .set_indicator_inputs(rh, &input_values, Some(&prop_values));
            }
            if !hidden {
                self.execute_indicator(id, handle);
            }
        }
        self.settle_code(record, CodeUpdateResult::failure(message));
    }

    /// Live handles of every indicator on the chart (script + native), insertion
    /// order.
    pub fn list_indicators(&self) -> Vec<Rc<IndicatorHandleImpl>> {
        self.inner.state.borrow().handles.clone()
    }

    /// Read-only engine-context snapshot for one indicator (`Ok(None)`: no
    /// capability, or not run).
    pub fn get_indicator_context(
        &self,
        id: &str,
        select: Option<&[ContextKey]>,
    ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>> {
        let session = self
            .inner
            .registry
            .get(id)
            .and_then(|r| r.borrow().session.clone());
        match session.and_then(|s| s.get_context(select)) {
            Some(future) => future,
            None => ready(Ok(None)),
        }
    }

    /// IndicatorController: tear down an indicator and (if now empty) its pane.
    pub fn remove_indicator(&self, id: &str) {
        let record = self.inner.registry.remove(id);
        if let Some(record) = &record {
            // Removing the auto-added volume is a USER decision: make it stick. The
            // auto-add fires on every load (first paint of every market), so
            // without the opt-out a removed volume resurrected on the next
            // symbol/timeframe switch.
            if record
                .borrow()
                .native
                .as_ref()
                .is_some_and(|n| n.indicator_type == "volume")
            {
                self.inner.state.borrow_mut().volume_opted_out = true;
            }
            self.settle_code(
                record,
                CodeUpdateResult::failure(format!(
                    "[vela] updateCode(\"{id}\") \u{2014} the indicator was removed."
                )),
            );
            let session = record.borrow().session.clone();
            if let Some(session) = session {
                session.stop();
            }
            // A native added hidden and never started (a restored ledger entry) has
            // nothing to tear down: and its `stop()` may not expect to run without
            // a context.
            if record.borrow().native.is_some() {
                self.stop_native_instance(record);
            }
        }
        self.inner
            .state
            .borrow_mut()
            .handles
            .retain(|h| h.id() != id);
        if let Some(record) = &record {
            let render_handle = record.borrow().render_handle.clone();
            if let Some(handle) = render_handle {
                self.inner.renderer.remove_indicator(&handle);
            }
            let pane_id = record
                .borrow()
                .model
                .as_ref()
                .and_then(|m| m.pane_id.clone());
            // A pane can now hold several indicators: only drop it when the last one leaves.
            if let Some(pane_id) = pane_id
                && pane_id != "price"
                && !self.pane_still_used(&pane_id)
            {
                self.inner.renderer.remove_pane(&pane_id);
                self.forget_pane(&pane_id);
                self.emit(ChartEvent::PaneChanged);
            }
        }
        self.emit(ChartEvent::IndicatorRemoved { id: id.to_string() });
    }

    /// IndicatorController: hide or show an indicator. Hiding **suspends** it: a
    /// Pine indicator's session is stopped (and a native instance suspended), so
    /// it is never poked on bar/viewport/input and consumes no resources, while
    /// the renderer drops its visuals and keeps the legend row marked hidden.
    /// Showing resumes: a Pine indicator re-executes (cached prepared), a native
    /// instance resumes and re-emits. Visuals were dropped on hide, so the next
    /// model re-mounts.
    pub fn set_visible(&self, id: &str, visible: bool) {
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        {
            let mut r = record.borrow_mut();
            if r.hidden == !visible {
                return;
            }
            r.hidden = !visible;
        }
        let handle = self.handle_of(id);
        if let Some(handle) = &handle {
            handle.set_visible_state(visible);
        }
        if !visible {
            let session = record.borrow_mut().session.take();
            if let Some(session) = session {
                session.stop();
            }
            // Hidden mid-trial: the code compiled and a hidden indicator computes
            // nothing, so the update stands: the same outcome as updating a
            // hidden indicator.
            let trial = record.borrow_mut().code_trial.take();
            if trial.is_some() {
                self.settle_code(&record, CodeUpdateResult::success());
            }
            // Only a RUNNING instance is suspended: a record hidden right after its
            // add (a restored hidden ledger entry) has not started yet, so there is
            // nothing to suspend: and the instance's suspend() is entitled to
            // assume start() ran.
            let (started, render_handle, is_native) = {
                let r = record.borrow();
                (
                    r.native.as_ref().is_some_and(|n| n.started),
                    r.render_handle.clone(),
                    r.native.is_some(),
                )
            };
            if started {
                self.run_native(&record, |instance, _ctx| instance.suspend());
            }
            if let Some(rh) = render_handle {
                self.inner.renderer.set_indicator_visible(&rh, false);
            } else if is_native {
                // Hidden before anything mounted (a restored ledger entry hides the
                // record right after add, before start): the row must exist anyway,
                // or the indicator is unreachable: there is no eye to unhide it
                // with. Natives mount here; a Pine record mounts via
                // `mount_loading_placeholder` once its prepare completes (prepare
                // runs regardless of visibility).
                self.mount_hidden_native_row(id, &record);
            }
        } else {
            let render_handle = record.borrow().render_handle.clone();
            if let Some(rh) = render_handle {
                self.inner.renderer.set_indicator_visible(&rh, true);
            }
            // Visuals dropped on hide: the next model re-mounts.
            record.borrow_mut().pending_structural = true;
            let (is_native, fresh) = {
                let mut r = record.borrow_mut();
                let is_native = r.native.is_some();
                let fresh = r
                    .native
                    .as_mut()
                    .map(|n| {
                        let fresh = n.stale || !n.started;
                        if fresh {
                            n.stale = false;
                        }
                        fresh
                    })
                    .unwrap_or(false);
                (is_native, fresh)
            };
            if is_native {
                if fresh {
                    // The instance was re-created for a NEW market while hidden
                    // (`stale`), or was ADDED hidden and never started (a restored
                    // hidden ledger entry: `start_native_indicator` bails on hidden
                    // records): resume() would either revive the old market's
                    // compute or poke a context-less instance. Start the instance
                    // instead.
                    if let Some(handle) = handle {
                        self.detach(self.start_native_indicator(id, handle));
                    }
                } else {
                    self.run_native(&record, |instance, ctx| instance.resume(ctx));
                }
            } else {
                // Spinner on the kept legend row while re-executing.
                self.set_loading(&record, true);
                if let Some(handle) = handle {
                    self.execute_indicator(id, &handle);
                }
            }
        }
        // Announce the change so host UIs (object tree, custom legends) reflect the
        // new eye/dim state immediately: the toggle can come from the in-chart
        // legend, so a tree that only re-renders on its own actions would otherwise
        // lag until a poll.
        self.emit(ChartEvent::IndicatorVisibility {
            id: id.to_string(),
            visible,
        });
    }

    // ── internals ───────────────────────────────────────────

    /// The id a new script indicator runs under: the host's when supplied, else
    /// a minted one. A host id is opaque but must be a non-empty string, and it
    /// must not be live on this chart: a duplicate is a programming error
    /// surfaced synchronously, never renamed behind the caller's back (the whole
    /// point of supplying one is that `handle.id` equals what was passed).
    /// "Live" reads both the records and the handle list: a fail-soft handle
    /// never enters the registry but still owns its id.
    pub(super) fn claim_indicator_id(
        &self,
        requested: Option<&str>,
    ) -> Result<String, IndicatorIdError> {
        let Some(requested) = requested else {
            return Ok(self
                .inner
                .registry
                .next_id("ind", |candidate| self.handle_of(candidate).is_some()));
        };
        if requested.is_empty() {
            return Err(IndicatorIdError::Empty);
        }
        if self.handle_of(requested).is_some() || self.inner.registry.get(requested).is_some() {
            return Err(IndicatorIdError::Duplicate(requested.to_string()));
        }
        Ok(requested.to_string())
    }

    fn is_destroyed(&self) -> bool {
        self.inner.state.borrow().destroyed
    }

    /// Prepare a script, mount its loading placeholder and run it (dispatched
    /// detached from `add_indicator`).
    pub(super) fn start_indicator(
        &self,
        id: &str,
        source: &str,
        options: AddIndicatorOptions,
        handle: Rc<IndicatorHandleImpl>,
    ) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        let (id, source) = (id.to_string(), source.to_string());
        Box::pin(async move {
            let _ = js_await(this.inner.ready.wait()).await;
            if this.is_destroyed() || this.inner.registry.get(&id).is_none() {
                return;
            }

            // Let the renderer paint the post-fetch frame (e.g. the "Running..."
            // status) before the engine's synchronous transpile + execution
            // monopolizes the main thread: otherwise the UI stays frozen on the
            // pre-execution frame for the whole run. (No requestAnimationFrame in
            // the core: the original's fallback, a zero timeout.)
            let _ = sleep(&*this.inner.platform, 0.0).await;
            let Some(record) = this.inner.registry.get(&id) else {
                return; // removed during the yield
            };
            if this.is_destroyed() {
                return;
            }

            let engine = match this.engine_for(options.language.as_deref()) {
                Ok(engine) => engine,
                Err(e) => {
                    this.fail(&id, Some(&handle), &e.message);
                    return;
                }
            };
            record.borrow_mut().engine = Some(Rc::clone(&engine));

            let prepared = match js_await(engine.prepare(&source, &id)).await {
                Ok(prepared) => prepared,
                Err(e) => {
                    this.fail(&id, Some(&handle), &e.message);
                    return;
                }
            };
            // An update_code that landed during this prepare owns the record now:
            // the original source must not overwrite the replacement's schema and
            // session.
            if record.borrow().source != source || this.is_destroyed() {
                return;
            }
            let props = prepared.props.clone().unwrap_or_default();
            {
                let mut r = record.borrow_mut();
                r.prepared = Some(prepared.clone());
                let mut values: Inputs = prepared
                    .inputs
                    .iter()
                    .map(|i| (i.key.clone(), i.defval.clone()))
                    .collect();
                values.extend(std::mem::take(&mut r.input_values));
                r.input_values = values;
                let mut prop_values: Inputs = props
                    .iter()
                    .map(|p| (p.key.clone(), p.defval.clone()))
                    .collect();
                prop_values.extend(std::mem::take(&mut r.prop_values));
                r.prop_values = prop_values;
            }
            handle.set_schema(prepared.inputs.clone());
            handle.set_props_schema(props);

            // Mount a legend-only placeholder BEFORE the (possibly long) execution,
            // so the indicator appears immediately with a loading spinner instead
            // of the UI staying blank until the first computed model lands.
            this.mount_loading_placeholder(&id, &record);
            this.execute_indicator(&id, &handle);
        })
    }

    /// Start (or restart) the engine session for an already-prepared indicator
    /// and route its models to `apply_model`. Used by the initial run and by
    /// `set_visible` on show. No-op while the indicator is hidden: a hidden
    /// indicator must not hold a live session.
    pub(super) fn execute_indicator(&self, id: &str, handle: &Rc<IndicatorHandleImpl>) {
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        let (engine, prepared, inputs, props) = {
            let r = record.borrow();
            let (Some(engine), Some(prepared)) = (r.engine.clone(), r.prepared.clone()) else {
                return;
            };
            if r.hidden {
                return;
            }
            (
                engine,
                prepared,
                r.input_values.clone(),
                r.prop_values.clone(),
            )
        };
        // Stream only when the chart is live, the script is NOT viewport-dependent,
        // and the engine can stream. Viewport scripts + non-streaming engines take
        // the static path.
        let (live, history_state) = {
            let st = self.inner.state.borrow();
            (st.config.live, st.history_state)
        };
        let mode = if live && !prepared.reacts_to_viewport() && engine.capabilities().streaming {
            ExecutionMode::Live
        } else {
            ExecutionMode::Static
        };
        let weak = self.weak();
        let get_bars: Rc<dyn Fn() -> Vec<Ohlcv>> = {
            let weak = weak.clone();
            Rc::new(move || {
                weak.upgrade()
                    .map(|inner| inner.state.borrow().bars.clone())
                    .unwrap_or_default()
            })
        };
        let fetch_series: crate::ports::scripting_engine::FetchSeries = {
            let weak = weak.clone();
            Rc::new(
                move |symbol: &str, timeframe: &str, range: &BarRange| match upgrade(&weak) {
                    Some(o) => o.fetch_series(symbol, timeframe, range),
                    None => ready(Ok(Vec::new())),
                },
            )
        };
        let request = ExecutionRequest {
            prepared: prepared.clone(),
            market: self.market(),
            bars: self.view_bars(),
            get_bars: Some(get_bars),
            fetch_series: Some(fetch_series),
            inputs: Some(inputs),
            props: Some(props),
            visible_range: self.current_visible_range(),
            mode,
            // Mid-backfill session starts (add / re-show / price-style re-execute)
            // let the ENGINE decide whether to run now or hold for the 'complete'
            // notification.
            history_state: Some(history_state),
        };

        // The handlers hold the record weakly: the record owns its session, which
        // owns these.
        let record_weak = Rc::downgrade(&record);
        let id_owned = id.to_string();
        let on_model = {
            let (weak, record_weak, id) = (weak.clone(), record_weak.clone(), id_owned.clone());
            move |model: IndicatorModel| {
                let (Some(o), Some(record)) = (upgrade(&weak), record_weak.upgrade()) else {
                    return;
                };
                // Read BEFORE apply_model: it is what announces the indicator, and
                // the consumed cause must not leak into the next model.
                let (first, cause) = {
                    let r = record.borrow();
                    (
                        !r.announced,
                        r.pending_cause.unwrap_or(ScriptRunCause::History),
                    )
                };
                // A deferred (output-free while loading) model counts as no run at
                // all: the pending cause stays for the real run, and no events fire.
                if !o.apply_model(&id, model) {
                    return;
                }
                let trial = {
                    let mut r = record.borrow_mut();
                    r.pending_cause = None;
                    // The new code ran: it is the code now.
                    r.code_trial.take()
                };
                if trial.is_some() {
                    o.settle_code(&record, CodeUpdateResult::success());
                }
                o.emit_context_changed(&id); // throttled: streamed ticks collapse to ~1/s
                o.emit_script_run(&id, cause, first);
            }
        };
        let on_alert = {
            let (weak, record_weak, handle) =
                (weak.clone(), record_weak.clone(), Rc::clone(handle));
            move |alert: crate::ports::scripting_engine::EngineAlert| {
                let (Some(o), Some(record)) = (upgrade(&weak), record_weak.upgrade()) else {
                    return;
                };
                // A replayed bar runs the live path, so a script alerts on it as on
                // a live bar: but it is history: announcing it would pass the past
                // off as a live signal (toasts, bell, host notifications).
                {
                    let st = o.inner.state.borrow();
                    if st.replay_queue.is_some() || st.replay_carrying.is_some() {
                        return;
                    }
                }
                // The chart-level event names its source: the indicator's effective
                // display title (host override, else the script's own; the same rule
                // the legend announce uses): hosts render alerts from many
                // indicators on one surface (the shells' bell menu).
                let indicator = {
                    let r = record.borrow();
                    r.options
                        .as_ref()
                        .and_then(|opts| opts.title.clone())
                        .or_else(|| r.prepared.as_ref().map(|p| p.meta.title.clone()))
                        .unwrap_or_else(|| r.title.clone())
                };
                let for_handle = IndicatorAlert {
                    id: alert.id.clone(),
                    message: alert.message.clone(),
                    title: alert.title.clone(),
                    time: alert.time as f64,
                };
                o.emit(ChartEvent::Alert(AlertEvent {
                    alert,
                    indicator: Some(indicator),
                }));
                handle.emit(IndicatorEvent::Alert(for_handle));
            }
        };
        let on_warning = {
            let weak = weak.clone();
            move |warning| {
                if let Some(o) = upgrade(&weak) {
                    o.emit(ChartEvent::Warning(warning));
                }
            }
        };
        let on_error = {
            let (weak, record_weak, handle, id) = (
                weak.clone(),
                record_weak.clone(),
                Rc::clone(handle),
                id_owned.clone(),
            );
            move |message: String| {
                let (Some(o), Some(record)) = (upgrade(&weak), record_weak.upgrade()) else {
                    return;
                };
                // A failure while the code is on trial (before its first model)
                // rolls the update back; any later failure is the code's own.
                let on_trial = record.borrow().code_trial.is_some();
                if on_trial {
                    o.reject_code(&id, &record, &handle, &message);
                } else {
                    o.fail(&id, Some(&handle), &message);
                }
            }
        };
        let session = engine.execute(
            request,
            ExecutionHandlers {
                on_model: Box::new(on_model),
                on_alert: Some(Box::new(on_alert)),
                on_warning: Some(Box::new(on_warning)),
                on_error: Some(Box::new(on_error)),
                on_done: None,
            },
        );
        // A trial that failed synchronously inside `execute` has already been
        // rolled back, and the restored session is in place: the failed one must
        // not overwrite it.
        if !same_prepared(&record.borrow().prepared, &prepared) {
            session.stop();
            return;
        }
        record.borrow_mut().session = Some(session);
        // A synchronous engine can emit its model DURING the execute call above:
        // before record.session exists, so the gated emit inside on_model missed.
        // Fire once now that the session (and its optional get_context) is
        // attached; throttling dedupes.
        self.emit_context_changed(id);
    }

    /// Start a native indicator's compute (after first bars are loaded). It
    /// pushes its visuals via `ctx.emit`, which the orchestrator wraps into a
    /// full `IndicatorModel` and routes through the SAME `apply_model` mount/patch
    /// path as a script indicator: the legend, pane routing, settings dialog, and
    /// events all come for free. Never fails: errors go through `fail`.
    pub(super) fn start_native_indicator(
        &self,
        id: &str,
        _handle: Rc<IndicatorHandleImpl>,
    ) -> LocalBoxFuture<'static, ()> {
        let this = self.clone();
        let id = id.to_string();
        Box::pin(async move {
            let _ = js_await(this.inner.ready.wait()).await;
            let Some(record) = this.inner.registry.get(&id) else {
                return; // removed during the await
            };
            if this.is_destroyed() {
                return;
            }
            let waits_for_history = {
                let r = record.borrow();
                let Some(native) = r.native.as_ref() else {
                    return;
                };
                if r.hidden {
                    return; // hidden during the await
                }
                if native.started {
                    return; // a release raced another start of the same instance
                }
                native.descriptor.waits_for_history
            };
            let history_state = this.inner.state.borrow().history_state;
            if waits_for_history && history_state == HistoryState::Backfill {
                if let Some(native) = record.borrow_mut().native.as_mut() {
                    native.held_for_history = true;
                }
                this.mount_native_placeholder(&id, &record);
                return;
            }
            if let Some(native) = record.borrow_mut().native.as_mut() {
                native.held_for_history = false;
            }
            let channel = this.native_channel(&record);
            // One context per start: once the instance is stopped, nothing it left
            // in flight may reach the scene (a stopped market's payload, or a
            // removed indicator's).
            let meta = {
                let st = this.inner.state.borrow();
                Rc::new(NativeCtxMeta {
                    id: id.clone(),
                    symbol: st
                        .config
                        .market
                        .symbol
                        .clone()
                        .unwrap_or_else(|| "TEST".to_string()),
                    timeframe: st
                        .config
                        .market
                        .timeframe
                        .clone()
                        .unwrap_or_else(|| "60".to_string()),
                    live: st.config.live && st.replay_queue.is_none(),
                    session: st.config.market.session.map(|s| s.as_str().to_string()),
                    channel,
                    alive: Cell::new(true),
                })
            };
            let revoke: Rc<dyn Fn()> = {
                let meta = Rc::clone(&meta);
                Rc::new(move || meta.alive.set(false))
            };
            let inputs = {
                let mut r = record.borrow_mut();
                if let Some(native) = r.native.as_mut() {
                    native.revoke_context = Some(revoke);
                }
                r.input_values.clone()
            };
            this.inner
                .state
                .borrow_mut()
                .native_contexts
                .insert(id.clone(), Rc::clone(&meta));
            this.run_native(&record, move |instance, ctx| instance.start(ctx, inputs));
            // Not set when a listener stopped the instance during its own start.
            if meta.alive.get()
                && let Some(native) = record.borrow_mut().native.as_mut()
            {
                native.started = true;
            }
        })
    }

    /// Restart every native indicator over the new market: a
    /// `NativeIndicatorContext` captures symbol/timeframe at start, so the old
    /// instance is stopped and a fresh one created from the descriptor: same
    /// record, same id, same legend row. Hidden natives are only re-instantiated
    /// and marked STALE; showing them starts the fresh instance (a resume would
    /// revive the OLD market's compute).
    pub(super) fn restart_native_indicators(&self) {
        for record in self.inner.registry.all() {
            let Some(descriptor) = record
                .borrow()
                .native
                .as_ref()
                .map(|n| n.descriptor.clone())
            else {
                continue;
            };
            self.stop_native_instance(&record);
            // The factory is host code: called with no borrow held.
            let fresh = descriptor.create();
            let (id, hidden) = {
                let mut r = record.borrow_mut();
                if let Some(native) = r.native.as_mut() {
                    native.instance = Rc::new(RefCell::new(fresh));
                }
                // The next emitted model remounts over the old visuals.
                r.pending_structural = true;
                if r.hidden
                    && let Some(native) = r.native.as_mut()
                {
                    native.stale = true;
                }
                (r.id.clone(), r.hidden)
            };
            if !hidden && let Some(handle) = self.handle_of(&id) {
                self.detach(self.start_native_indicator(&id, handle));
            }
        }
    }

    /// Stop a running native instance, then revoke its context: the teardown
    /// inside `stop()` still reaches its layer, but work it left in flight (a
    /// fetch resolving later) does not.
    pub(super) fn stop_native_instance(&self, record: &RecordRef) {
        let started = match record.borrow().native.as_ref() {
            Some(native) => native.started,
            None => return,
        };
        if started {
            self.run_native(record, |instance, ctx| instance.stop(ctx));
        }
        let (revoke, id) = {
            let mut r = record.borrow_mut();
            let id = r.id.clone();
            let Some(native) = r.native.as_mut() else {
                return;
            };
            native.started = false;
            (native.revoke_context.take(), id)
        };
        if let Some(revoke) = revoke {
            revoke();
        }
        self.inner.state.borrow_mut().native_contexts.remove(&id);
    }

    /// Tell a native its bars changed (`NativeIndicator::on_bars` with the
    /// instance's context).
    pub(super) fn native_on_bars(&self, record: &RecordRef) {
        self.run_native(record, |instance, ctx| instance.on_bars(ctx));
    }

    /// Tell a native the viewport moved (`NativeIndicator::on_viewport` with the
    /// instance's context).
    pub(super) fn native_on_viewport(&self, record: &RecordRef, range: VisibleRange) {
        self.run_native(record, move |instance, ctx| {
            instance.on_viewport(ctx, range)
        });
    }

    /// One lifecycle call on a record's native instance, with a context lent for
    /// its duration (see the module doc). A call before the instance has started
    /// (or after it stopped) gets a dead context: whatever it pushes is dropped,
    /// as the original's revoked or absent context does.
    fn run_native(
        &self,
        record: &RecordRef,
        call: impl FnOnce(&mut dyn NativeIndicator, &mut dyn NativeIndicatorContext),
    ) {
        let (id, instance) = {
            let r = record.borrow();
            let Some(native) = r.native.as_ref() else {
                return;
            };
            (r.id.clone(), Rc::clone(&native.instance))
        };
        let meta = {
            let stored = self.inner.state.borrow().native_contexts.get(&id).cloned();
            stored.unwrap_or_else(|| {
                let st = self.inner.state.borrow();
                Rc::new(NativeCtxMeta {
                    id: id.clone(),
                    symbol: st
                        .config
                        .market
                        .symbol
                        .clone()
                        .unwrap_or_else(|| "TEST".to_string()),
                    timeframe: st
                        .config
                        .market
                        .timeframe
                        .clone()
                        .unwrap_or_else(|| "60".to_string()),
                    live: st.config.live && st.replay_queue.is_none(),
                    session: st.config.market.session.map(|s| s.as_str().to_string()),
                    channel: String::new(),
                    alive: Cell::new(false),
                })
            })
        };
        let mut ctx = NativeContext {
            meta: Rc::clone(&meta),
            bars: self.view_bars(),
            data: self.inner.data_control.clone(),
            ops: Vec::new(),
        };
        {
            // Only a lifecycle call re-entering the SAME instance can fail this:
            // pushes are buffered and applied below with the instance released.
            let Ok(mut guard) = instance.try_borrow_mut() else {
                self.warn(
                    "[vela] a native indicator was called while it was running; call skipped.",
                );
                return;
            };
            call(&mut **guard, &mut ctx);
        }
        self.flush_native_ops(record, &meta, std::mem::take(&mut ctx.ops));
    }

    /// Apply what an instance pushed during one call, in order. Liveness is read
    /// per op: a listener the first op reaches may stop the instance.
    fn flush_native_ops(&self, record: &RecordRef, meta: &NativeCtxMeta, ops: Vec<NativeOp>) {
        for op in ops {
            if !meta.alive.get() {
                continue;
            }
            match op {
                NativeOp::Emit(out) => {
                    let anchor = self.inner.state.borrow().bars.first().map(|b| b.time);
                    let model = self.build_native_model(record, *out, anchor);
                    self.apply_model(&meta.id, model);
                }
                NativeOp::Push(data) => {
                    self.inner.renderer.set_native_data(&meta.channel, &data);
                }
                NativeOp::Status(status) => {
                    let handle = {
                        let r = record.borrow();
                        if r.hidden {
                            None
                        } else {
                            r.render_handle.clone()
                        }
                    };
                    if let Some(handle) = handle {
                        self.inner.renderer.set_indicator_status(&handle, status);
                    }
                }
            }
        }
    }

    /// The renderer-layer channel a native instance's `push_data` lands on: the
    /// type itself for a single-instance type (the layer id doubles as the
    /// channel), a per-instance channel for a `multi_instance` type: otherwise
    /// every instance would overwrite the one layer. Stamped on the model so the
    /// renderer mounts a dedicated layer for it.
    pub(super) fn native_channel(&self, record: &RecordRef) -> String {
        let r = record.borrow();
        match r.native.as_ref() {
            Some(n) if n.descriptor.multi_instance => {
                crate::indicators::native_instance_channel(&n.indicator_type, &r.id)
            }
            Some(n) => n.indicator_type.clone(),
            None => String::new(),
        }
    }

    /// Wrap a native indicator's emitted visuals into a full `IndicatorModel`,
    /// tagged `native`. `anchor_time` is the first bar the output is
    /// index-aligned to: the chart's first bar when it emitted. Without it the
    /// renderer pins the output's first slot to whatever the first bar is NOW, so
    /// history landing in front of a computed output (or the oldest bars trimmed
    /// under it) would slide the whole plot by that many bars.
    pub(super) fn build_native_model(
        &self,
        record: &RecordRef,
        out: NativeIndicatorOutput,
        anchor_time: Option<Millis>,
    ) -> IndicatorModel {
        let channel = self.native_channel(record);
        let i18n = self.translator();
        let r = record.borrow();
        let (d, native_type) = match r.native.as_ref() {
            Some(n) => (Some(&n.descriptor), n.indicator_type.clone()),
            None => (None, String::new()),
        };
        let non_empty = |m: &&MessageLike| !matches!(m, MessageLike::Text(t) if t.is_empty());
        IndicatorModel {
            id: r.id.clone(),
            title: r.title.clone(),
            shorttitle: d
                .and_then(|d| d.short_title.as_ref())
                .filter(non_empty)
                .map(|m| text_of(i18n.as_deref(), m, &[])),
            display_title: d.and_then(|d| message_only(Some(&d.title)).cloned()),
            display_shorttitle: d.and_then(|d| message_only(d.short_title.as_ref()).cloned()),
            overlay: d.is_none_or(|d| d.overlay),
            pane_hint: d.map_or(PaneHint::Price, |d| d.pane_hint),
            native: Some(NativeTag {
                channel: (channel != native_type).then_some(channel),
                indicator_type: native_type,
            }),
            legend: d.and_then(|d| (d.legend == Some(false)).then_some(false)),
            pane_axis: out.pane_axis,
            pane_id: None,
            own_scale: false,
            anchor_time,
            series: out.series,
            fills: out.fills,
            backgrounds: out.backgrounds,
            price_lines: out.price_lines,
            lines: out.lines,
            boxes: out.boxes,
            labels: out.labels,
            polylines: out.polylines,
            linefills: out.linefills,
            tables: out.tables,
            bar_colors: Vec::new(),
            trades: Vec::new(),
            inputs: d.map(|d| d.inputs_schema.clone()).unwrap_or_default(),
            input_values: r.input_values.clone(),
            props: Vec::new(),
            prop_values: Inputs::new(),
        }
    }

    /// The options a record was added with (routing).
    fn options_of(record: &RecordRef) -> AddIndicatorOptions {
        record.borrow().options.clone().unwrap_or_default()
    }

    /// Mount an EMPTY model for a just-prepared script indicator: the legend row
    /// and its routed pane appear immediately with a loading spinner, while the
    /// engine computes. The first computed model remounts over it in place
    /// (`pending_structural`), clears the spinner, and only THEN fires
    /// `indicator:added`/`ready`. A HIDDEN record mounts too (dimmed, no
    /// spinner) and announces immediately: its session never starts while hidden.
    pub(super) fn mount_loading_placeholder(&self, id: &str, record: &RecordRef) {
        let mut model = {
            let r = record.borrow();
            if r.render_handle.is_some() {
                return;
            }
            let Some(prepared) = r.prepared.as_ref() else {
                return;
            };
            let meta = &prepared.meta;
            let (props, prop_values) = match &prepared.props {
                Some(props) => (props.clone(), r.prop_values.clone()),
                None => (Vec::new(), Inputs::new()),
            };
            IndicatorModel {
                id: id.to_string(),
                // Deliberately the FULL title, never a shorttitle: while the script
                // loads the legend identifies it by its full name; the compact
                // shorttitle arrives with the first computed model and takes over
                // from there.
                title: r
                    .options
                    .as_ref()
                    .and_then(|o| o.title.clone())
                    .unwrap_or_else(|| meta.title.clone()),
                shorttitle: None,
                display_title: None,
                display_shorttitle: None,
                overlay: meta.overlay,
                pane_hint: if meta.overlay {
                    PaneHint::Price
                } else {
                    PaneHint::New
                },
                native: None,
                legend: None,
                pane_axis: None,
                pane_id: None,
                own_scale: false,
                anchor_time: None,
                series: Vec::new(),
                fills: Vec::new(),
                backgrounds: Vec::new(),
                price_lines: Vec::new(),
                lines: Vec::new(),
                boxes: Vec::new(),
                labels: Vec::new(),
                polylines: Vec::new(),
                linefills: Vec::new(),
                tables: Vec::new(),
                bar_colors: Vec::new(),
                trades: Vec::new(),
                inputs: prepared.inputs.clone(),
                input_values: r.input_values.clone(),
                props,
                prop_values,
            }
        };
        let pane_id = self.route_pane(id, &model, &Self::options_of(record));
        self.place_model(&mut model, id, &pane_id);
        // Keeps pane routing + remove-time pane cleanup consistent.
        record.borrow_mut().model = Some(model.clone());
        let pane_created = self.ensure_pane_for(&pane_id);
        let render_handle = self.inner.renderer.mount_indicator(&model);
        {
            let mut r = record.borrow_mut();
            r.render_handle = Some(render_handle.clone());
            // The first computed model remounts over the placeholder.
            r.pending_structural = true;
        }
        if pane_created {
            self.emit(ChartEvent::PaneChanged);
        }
        let hidden = record.borrow().hidden;
        if hidden {
            self.inner
                .renderer
                .set_indicator_visible(&render_handle, false);
            self.announce(record, self.handle_of(id).as_ref());
            return;
        }
        self.set_loading(record, true);
    }

    /// Mount an empty native model on its routed pane (the part shared by the
    /// hidden row and the history placeholder).
    fn mount_empty_native_row(&self, id: &str, record: &RecordRef) {
        let mut model = self.build_native_model(record, NativeIndicatorOutput::default(), None);
        let pane_id = self.route_pane(id, &model, &Self::options_of(record));
        self.place_model(&mut model, id, &pane_id);
        record.borrow_mut().model = Some(model.clone());
        let pane_created = self.ensure_pane_for(&pane_id);
        let render_handle = self.inner.renderer.mount_indicator(&model);
        record.borrow_mut().render_handle = Some(render_handle);
        if pane_created {
            self.emit(ChartEvent::PaneChanged);
        }
    }

    /// Mount the legend row for a NATIVE indicator that is being hidden BEFORE it
    /// ever started (a restored-hidden ledger entry).
    pub(super) fn mount_hidden_native_row(&self, id: &str, record: &RecordRef) {
        {
            let r = record.borrow();
            if r.render_handle.is_some() || r.native.is_none() {
                return;
            }
        }
        self.mount_empty_native_row(id, record);
        // The first emit after show remounts over this row.
        let render_handle = {
            let mut r = record.borrow_mut();
            r.pending_structural = true;
            r.render_handle.clone()
        };
        if let Some(handle) = render_handle {
            self.inner.renderer.set_indicator_visible(&handle, false);
        }
        self.announce(record, self.handle_of(id).as_ref());
    }

    /// The legend row of a native HELD for the history (see
    /// `waits_for_history`): an empty model with the title and inputs schema,
    /// spinning, not announced.
    pub(super) fn mount_native_placeholder(&self, id: &str, record: &RecordRef) {
        let needs_row = {
            let mut r = record.borrow_mut();
            r.pending_structural = true;
            r.render_handle.is_none()
        };
        if needs_row {
            self.mount_empty_native_row(id, record);
        }
        self.set_loading(record, true);
    }

    /// Flip the record's loading state and reflect it in the legend row (spinner
    /// on/off).
    pub(super) fn set_loading(&self, record: &RecordRef, loading: bool) {
        let handle = {
            let mut r = record.borrow_mut();
            r.loading = loading;
            match &r.render_handle {
                Some(handle) if !r.hidden => Some(handle.clone()),
                _ => None,
            }
        };
        if let Some(handle) = handle {
            self.inner.renderer.set_indicator_status(
                &handle,
                if loading {
                    crate::ports::chart_renderer::IndicatorStatus::Loading
                } else {
                    crate::ports::chart_renderer::IndicatorStatus::Idle
                },
            );
        }
    }

    /// Fire `indicator:added` + the handle's `ready` once, on the first computed
    /// model.
    pub(super) fn announce(&self, record: &RecordRef, handle: Option<&Rc<IndicatorHandleImpl>>) {
        let id = {
            let mut r = record.borrow_mut();
            if r.announced {
                return;
            }
            r.announced = true;
            r.id.clone()
        };
        self.emit(ChartEvent::IndicatorAdded { id: id.clone() });
        self.emit_context_changed(&id);
        if let Some(handle) = handle {
            handle.emit(IndicatorEvent::Ready);
        }
    }

    /// Apply an emitted model. First emission mounts (and routes the pane); a
    /// pending structural change (after an input edit) remounts idempotently;
    /// everything else (live tick / viewport re-run) value-patches.
    ///
    /// Returns false when the model was DEFERRED: an output-free model arriving
    /// while the record is still loading and the chart has no bars; every other
    /// outcome, including the hidden drop, returns true so the caller's event
    /// semantics stay unchanged.
    pub(super) fn apply_model(&self, id: &str, mut model: IndicatorModel) -> bool {
        let Some(record) = self.inner.registry.get(id) else {
            return true;
        };
        // A model arriving for a just-hidden indicator is dropped.
        if record.borrow().hidden {
            return true;
        }

        // A model arriving MID-SWITCH was computed over the outgoing market (a run
        // already in flight when set_market quiesced: every poke is gated and the
        // re-executions only start after the load): applying it would repaint the
        // just-blanked scene with stale content. It counts as no run at all: the
        // pending cause stays for the post-load re-execution, whose model paints
        // the new market.
        let no_bars = {
            let st = self.inner.state.borrow();
            if st.switching_market {
                return false;
            }
            st.bars.is_empty()
        };

        // While the CHART ITSELF has no bars, an output-free model is the signature
        // of a run over zero bars (empty initial load: auth race, unresolved
        // symbol, transient feed failure): an engine may then fabricate default
        // metadata (generic title, overlay: false) because the script body never
        // executed. Letting it through while loading would finalize pane routing
        // off fabricated flags and clear the spinner, stranding the indicator on
        // the wrong pane with a value patch (no title, no pane, unmounted series
        // ids) as its only future. Keep the prepared placeholder up and the
        // one-shot pane re-route unconsumed; the session re-runs when bars arrive
        // and the first REAL model still routes. The bars check keeps the guard
        // narrow on purpose: loading ends when the run completes, not when it
        // produces output: a script that ran over real bars and legitimately
        // emitted nothing visual (e.g. alerts only) still clears its spinner and
        // announces.
        if record.borrow().loading && no_bars && !Self::model_has_output(&model) {
            return false;
        }

        let handle = self.handle_of(id);

        if record.borrow().render_handle.is_none() {
            let pane_id = self.route_pane(id, &model, &Self::options_of(&record));
            self.place_model(&mut model, id, &pane_id);
            record.borrow_mut().model = Some(model.clone());
            let pane_created = self.ensure_pane_for(&pane_id);
            let render_handle = self.inner.renderer.mount_indicator(&model);
            {
                let mut r = record.borrow_mut();
                r.render_handle = Some(render_handle);
                r.pending_structural = false;
            }
            if pane_created {
                self.emit(ChartEvent::PaneChanged);
            }
            self.announce(&record, handle.as_ref());
            return true;
        }

        let (mut pane_id, prev_own_scale, loading, locked) = {
            let r = record.borrow();
            (
                r.model
                    .as_ref()
                    .and_then(|m| m.pane_id.clone())
                    .unwrap_or_else(|| "price".to_string()),
                r.model.as_ref().is_some_and(|m| m.own_scale),
                r.loading,
                r.pane_locked,
            )
        };
        let mut rerouted = false;
        if loading && !locked {
            // First COMPUTED model after the placeholder. The placeholder's pane
            // came from the prepare-time overlay guess: if the real model routes
            // differently, move off the placeholder pane before mounting. (A
            // user-placed indicator is locked, so its pane is never re-derived
            // here.)
            let routed = self.route_pane(id, &model, &Self::options_of(&record));
            if routed != pane_id {
                // A placeholder's pane is its own; after a code update the pane may
                // also hold indicators merged into it since: those keep it.
                let shared = self.inner.registry.all().iter().any(|r| {
                    let r = r.borrow();
                    r.id != id
                        && r.model.as_ref().and_then(|m| m.pane_id.as_deref()) == Some(&pane_id)
                });
                if pane_id != "price" && !shared {
                    self.inner.renderer.remove_pane(&pane_id);
                    // Keep core pane_order in sync (was left stale: desynced list()/anchors/undo).
                    self.forget_pane(&pane_id);
                }
                self.ensure_pane_for(&routed);
                record.borrow_mut().pending_structural = true;
                pane_id = routed;
                rerouted = true;
            }
        }
        self.place_model(&mut model, id, &pane_id);
        if locked {
            // Carry the merge across the recompute.
            model.own_scale = prev_own_scale;
        }
        let structural = record.borrow().pending_structural;
        if structural {
            // Idempotent-by-id remount: refresh visuals while keeping the legend +
            // open settings dialog intact (an input change can restructure
            // series/drawings).
            record.borrow_mut().model = Some(model.clone());
            let render_handle = self.inner.renderer.mount_indicator(&model);
            let (input_values, prop_values) = {
                let mut r = record.borrow_mut();
                r.render_handle = Some(render_handle.clone());
                (r.input_values.clone(), r.prop_values.clone())
            };
            self.inner.renderer.set_indicator_inputs(
                &render_handle,
                &input_values,
                Some(&prop_values),
            );
            record.borrow_mut().pending_structural = false;
        } else {
            let patch = model_to_value_patch(&model);
            let render_handle = {
                let mut r = record.borrow_mut();
                r.model = Some(model);
                r.render_handle.clone()
            };
            if let Some(render_handle) = render_handle {
                self.inner
                    .renderer
                    .update_indicator(&render_handle, &ScenePatch::Value(patch));
            }
        }
        if rerouted {
            self.emit(ChartEvent::PaneChanged);
        }
        if record.borrow().loading {
            self.set_loading(&record, false);
        }
        self.announce(&record, handle.as_ref());
        true
    }

    /// True when the model carries ANY executed output: series, drawings, bar
    /// colors, or trades.
    pub(super) fn model_has_output(model: &IndicatorModel) -> bool {
        !model.series.is_empty()
            || !model.fills.is_empty()
            || !model.backgrounds.is_empty()
            || !model.price_lines.is_empty()
            || !model.lines.is_empty()
            || !model.boxes.is_empty()
            || !model.labels.is_empty()
            || !model.polylines.is_empty()
            || !model.linefills.is_empty()
            || !model.tables.is_empty()
            || !model.bar_colors.is_empty()
            || !model.trades.is_empty()
    }

    pub(super) fn route_pane(
        &self,
        id: &str,
        model: &IndicatorModel,
        options: &AddIndicatorOptions,
    ) -> String {
        match options.pane {
            Some(crate::options::AddIndicatorPane::New) => return format!("pane-{id}"),
            Some(crate::options::AddIndicatorPane::Price) => return "price".to_string(),
            None => {}
        }
        let overlay = options.overlay.unwrap_or(model.overlay);
        if overlay {
            "price".to_string()
        } else {
            format!("pane-{id}")
        }
    }

    /// Each named value series' value on the chart's last bar: the model is the
    /// one source every engine fills, so this works without an execution
    /// context. Points past the last bar (a series shifted forward) are skipped
    /// by time.
    pub(super) fn plots_at_last_bar(
        model: &IndicatorModel,
        last_bar_time: Option<Millis>,
    ) -> BTreeMap<String, Option<f64>> {
        let mut out = BTreeMap::new();
        for spec in &model.series {
            let Some(points) = spec.points() else {
                continue;
            };
            let mut end = points.len();
            if let Some(last) = last_bar_time {
                while end > 0 && points[end - 1].time > last {
                    end -= 1;
                }
            }
            let value = end.checked_sub(1).and_then(|i| points[i].value);
            out.insert(spec.title.clone(), value);
        }
        out
    }

    /// Stamp the routed pane id on the model and every item of it.
    pub(super) fn place_model(&self, model: &mut IndicatorModel, id: &str, pane_id: &str) {
        model.id = id.to_string();
        model.pane_id = Some(pane_id.to_string());
        // A `force_overlay` item (overlay: true) renders on the price pane
        // regardless of where its indicator routed: stamp it 'price' so the model
        // tells the truth.
        let pane_of = |overlay: bool| if overlay { "price" } else { pane_id }.to_string();
        for s in &mut model.series {
            s.pane_id = pane_of(s.overlay);
        }
        for f in &mut model.fills {
            f.pane_id = pane_of(f.overlay);
        }
        for b in &mut model.backgrounds {
            b.pane_id = pane_of(b.overlay);
        }
        for l in &mut model.price_lines {
            l.pane_id = pane_id.to_string();
        }
        for l in &mut model.lines {
            l.pane_id = pane_of(l.overlay);
        }
        for b in &mut model.boxes {
            b.pane_id = pane_of(b.overlay);
        }
        for l in &mut model.labels {
            l.pane_id = pane_of(l.overlay);
        }
        for p in &mut model.polylines {
            p.pane_id = pane_of(p.overlay);
        }
        for l in &mut model.linefills {
            l.pane_id = pane_of(l.overlay);
        }
        for t in &mut model.tables {
            t.pane_id = pane_of(t.overlay);
        }
    }

    /// Throttled (1/s per indicator) `context:changed`: streamed models re-emit
    /// per tick, consumers only need a coarse re-pull signal. Fires only for
    /// context-capable sessions.
    pub(super) fn emit_context_changed(&self, id: &str) {
        let session = self
            .inner
            .registry
            .get(id)
            .and_then(|r| r.borrow().session.clone());
        // The port has no capability flag: an empty selection asks for nothing
        // and the answer is dropped unread.
        let capable = session.is_some_and(|s| s.get_context(Some(&[])).is_some());
        if !capable {
            return;
        }
        let now = self.now();
        {
            let mut st = self.inner.state.borrow_mut();
            if now - st.context_emit_at.get(id).copied().unwrap_or(0.0) < 1000.0 {
                return;
            }
            st.context_emit_at.insert(id.to_string(), now);
        }
        self.emit(ChartEvent::ContextChanged { id: id.to_string() });
    }

    /// Build and emit `script:run`. Costs NOTHING when nobody listens: the
    /// engine-context pull that fills `vars`/`strategy` only happens for an
    /// actual subscriber. Throttling is deliberately asymmetric: forming-bar
    /// `Tick` runs collapse to ~1/s (`RUN_EMIT_THROTTLE_MS`), every other cause
    /// fires unconditionally.
    pub(super) fn emit_script_run(&self, id: &str, cause: ScriptRunCause, first: bool) {
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        if record.borrow().native.is_some()
            || !self.inner.events.has_listeners(&ChartEventName::ScriptRun)
        {
            return;
        }
        let now = self.now();
        {
            let mut st = self.inner.state.borrow_mut();
            if cause == ScriptRunCause::Tick
                && now - st.run_emit_at.get(id).copied().unwrap_or(0.0) < RUN_EMIT_THROTTLE_MS
            {
                return;
            }
            st.run_emit_at.insert(id.to_string(), now);
        }
        // Deferred one turn on purpose: a SYNCHRONOUS engine emits its first model
        // during `execute()`, before `record.session` has been assigned: pulling
        // the execution context right now would find no session and silently drop
        // `vars` and `strategy` from the first run.
        let this = self.clone();
        let id = id.to_string();
        self.spawn(Box::pin(async move {
            let run = this.build_script_run(record, cause, first).await;
            // The indicator may have been removed while the snapshot crossed the worker.
            if let Some(run) = run
                && this.inner.registry.get(&id).is_some()
            {
                this.emit(ChartEvent::ScriptRun(run));
            }
        }));
    }

    /// The neutral run payload: the model supplies what every engine has, the
    /// execution context the rest (absent for an engine that exposes none).
    pub(super) fn build_script_run(
        &self,
        record: RecordRef,
        cause: ScriptRunCause,
        first: bool,
    ) -> LocalBoxFuture<'static, Option<ScriptRun>> {
        let this = self.clone();
        // The model as it stands when the run is built: the original holds the
        // object while it waits for the context, and reads the plots (against
        // the chart's last bar) once the answer is in.
        let (id, model) = {
            let r = record.borrow();
            let Some(model) = r.model.as_ref() else {
                return ready(None);
            };
            (r.id.clone(), model.clone())
        };
        let pulled = self.pull_context(
            &record,
            &[
                ContextKey::Variables,
                ContextKey::Strategy,
                ContextKey::Warnings,
            ],
        );
        Box::pin(async move {
            let snapshot = js_await(pulled).await;
            let has_trades = !model.trades.is_empty();
            let (bar, time, forming, complete, plots) = {
                let st = this.inner.state.borrow();
                let last = st.bars.last();
                (
                    st.bars.len().saturating_sub(1) as i64,
                    last.map_or(0, |b| b.time),
                    // A live chart's last bar is the open one, so its values are
                    // provisional whatever produced this run.
                    st.config.live && !st.bars.is_empty(),
                    st.history_state != HistoryState::Backfill,
                    Self::plots_at_last_bar(&model, last.map(|b| b.time)),
                )
            };
            let has_strategy = snapshot.as_ref().is_some_and(|s| s.strategy.is_some());
            let weak = this.weak();
            let record_weak = Rc::downgrade(&record);
            let trades_loader: TradesLoader = {
                let (weak, record_weak) = (weak.clone(), record_weak.clone());
                Rc::new(move || {
                    let (Some(o), Some(record)) = (upgrade(&weak), record_weak.upgrade()) else {
                        return ready(Vec::<StrategyTrade>::new());
                    };
                    let pulled = o.pull_context(&record, &[ContextKey::Trades]);
                    Box::pin(async move {
                        js_await(pulled)
                            .await
                            .and_then(|s| s.trades)
                            .unwrap_or_default()
                    })
                })
            };
            let series_loader: SeriesLoader = Rc::new(move |key: &str| {
                let (Some(o), Some(record)) = (upgrade(&weak), record_weak.upgrade()) else {
                    return ready(Vec::<PlotSeriesPoint>::new());
                };
                let pulled = o.pull_context(&record, &[ContextKey::Plots]);
                let key = key.to_string();
                Box::pin(async move {
                    let points = js_await(pulled)
                        .await
                        .and_then(|mut s| s.plots.remove(&key))
                        .unwrap_or_default();
                    points
                        .into_iter()
                        .map(|p| PlotSeriesPoint {
                            time: p.time,
                            value: p.value.as_f64(),
                        })
                        .collect()
                })
            });
            Some(ScriptRun {
                id,
                title: model.title.clone(),
                kind: if has_strategy || has_trades {
                    ScriptKind::Strategy
                } else {
                    ScriptKind::Indicator
                },
                cause,
                first,
                // The CHART's frame of reference, the only one an engine and a host share.
                bar: bar.max(0),
                time,
                forming,
                complete,
                plots,
                vars: snapshot
                    .as_ref()
                    .map(|s| s.variables.clone())
                    .unwrap_or_default(),
                strategy: snapshot.as_ref().and_then(|s| s.strategy.clone()),
                warnings: snapshot.map(|s| s.warnings).unwrap_or_default(),
                trades_loader: Some(trades_loader),
                series_loader: Some(series_loader),
            })
        })
    }

    /// One guarded execution-context pull; `None` for an engine without the
    /// capability, or a session torn down mid-pull.
    pub(super) fn pull_context(
        &self,
        record: &RecordRef,
        select: &[ContextKey],
    ) -> LocalBoxFuture<'static, Option<EngineContextSnapshot>> {
        // The session is read, and the engine asked, at call time (the synchronous
        // prefix of the original's async method).
        let session = record.borrow().session.clone();
        let pending = session.and_then(|s| s.get_context(Some(select)));
        Box::pin(async move {
            match pending {
                // A torn-down session mid-pull is not an error worth surfacing.
                Some(future) => js_await(future).await.ok().flatten(),
                None => None,
            }
        })
    }

    /// An indicator failed: stop its spinner (the row stays so the user can
    /// remove it), log, emit `indicator:error` and the handle's `error`.
    pub(super) fn fail(&self, id: &str, handle: Option<&Rc<IndicatorHandleImpl>>, message: &str) {
        if let Some(record) = self.inner.registry.get(id) {
            let loading = record.borrow().loading;
            // Stop the spinner; the row stays so the user can remove it.
            if loading {
                self.set_loading(&record, false);
            }
        }
        self.warn(&format!("[vela] indicator \"{id}\" failed: {message}"));
        self.emit(ChartEvent::IndicatorError {
            id: id.to_string(),
            error: message.to_string(),
        });
        if let Some(handle) = handle {
            handle.emit(IndicatorEvent::Error {
                error: message.to_string(),
            });
        }
    }
}

/// Build a value-only patch from a freshly-run model (used on live ticks /
/// re-runs).
pub(super) fn model_to_value_patch(model: &IndicatorModel) -> ValuePatch {
    let mut series = Vec::new();
    let mut from: Option<Millis> = None;
    let mut to: Millis = 0;
    let mut widen = |time: Millis| {
        from = Some(from.map_or(time, |f| f.min(time)));
        to = to.max(time);
    };
    for s in &model.series {
        match &s.data {
            SeriesData::Candle {
                bars, bar_colors, ..
            } => {
                // ALWAYS stated, `[]` included: the renderer keeps colors a delta
                // omits, and they are index-aligned to the bars, so a stale set
                // would slide onto whatever bar now holds its index.
                series.push(SeriesValueDelta {
                    series_id: s.id.clone(),
                    values: SeriesValues::Bars {
                        bars: bars.clone(),
                        bar_colors: Some(bar_colors.clone().unwrap_or_default()),
                    },
                });
                for b in bars {
                    widen(b.time);
                }
            }
            SeriesData::LineLike { points, .. } => {
                series.push(SeriesValueDelta {
                    series_id: s.id.clone(),
                    values: SeriesValues::Points(points.clone()),
                });
                for p in points {
                    widen(p.time);
                }
            }
            SeriesData::Markers { .. } => {}
        }
    }
    ValuePatch {
        indicator_id: model.id.clone(),
        dirty: DirtyRange {
            from: from.unwrap_or(0),
            to,
        },
        // ALWAYS stated, cleared included: an omitted key leaves the renderer on
        // the previous run's offset, so a re-run that widened to the whole chart
        // could not clear it.
        anchor_time: match model.anchor_time {
            Some(t) => AnchorUpdate::Set(t),
            None => AnchorUpdate::Clear,
        },
        series,
        lines: Some(model.lines.clone()),
        boxes: Some(model.boxes.clone()),
        labels: Some(model.labels.clone()),
        polylines: Some(model.polylines.clone()),
        linefills: Some(model.linefills.clone()),
        tables: Some(model.tables.clone()),
        trades: Some(model.trades.clone()),
    }
}

/// Re-seat stored values on a NEW schema: every declared key takes its default,
/// then a previous value is kept where the schema still declares its key (or
/// title: add-time overrides may be title-keyed). Stale keys drop. Stored values
/// carry the merged defaults, so a value still equal to its `old_schema` default
/// is not an edit: it follows the declaration to the new default (the rule
/// persistence diffs by, too).
pub(super) fn values_on_schema(
    schema: &[InputSchema],
    previous: &Inputs,
    old_schema: &[InputSchema],
) -> Inputs {
    let mut out = Inputs::new();
    let mut declared: HashSet<&str> = HashSet::new();
    for s in schema {
        out.insert(s.key.clone(), s.defval.clone());
        declared.insert(&s.key);
        declared.insert(&s.title);
    }
    let mut old_defaults: BTreeMap<&str, &InputValue> = BTreeMap::new();
    for s in old_schema {
        old_defaults.insert(&s.key, &s.defval);
        old_defaults.entry(&s.title).or_insert(&s.defval);
    }
    for (k, v) in previous {
        if !declared.contains(k.as_str()) {
            continue;
        }
        if old_defaults
            .get(k.as_str())
            .is_some_and(|d| same_input_value(d, v))
        {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

pub(super) fn same_input_value(a: &InputValue, b: &InputValue) -> bool {
    a == b
}

/// Fixtures of the section tests: an engine that runs scripts by itself (the
/// original's `MockEngine` and the variants built on it), test native
/// indicators, and the helpers the tests share.
#[cfg(test)]
pub(super) mod fixtures {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::super::test_support::*;
    use super::*;
    use crate::indicators::NativeIndicatorDescriptor;
    use crate::model::strategy::StrategyState;
    use crate::model::{
        IndicatorMeta, InputType, LineLikeKind, LineLikeStyle, LineStyle, SeriesPoint, SeriesSpec,
    };
    use crate::ports::scripting_engine::{
        BarsChangeReason, ContextMeta, ContextPhase, ContextPlotPoint, EngineAlert,
        EngineCapabilities, ExecutionSession, PreparedScript, ScriptingEngine, VisibleBarRange,
    };

    /// Let everything pending run, including the zero-delay paint yield.
    pub(in crate::engine::orchestrator) fn flush(rig: &Rig) {
        for _ in 0..4 {
            rig.settle();
            rig.advance(0.0);
        }
        rig.settle();
    }

    /// A rig whose `pine` engine is a fresh [`AutoEngine`].
    pub(in crate::engine::orchestrator) fn auto_rig(builder: RigBuilder) -> (Rig, Rc<AutoEngine>) {
        let rig = builder.build();
        let engine = AutoEngine::new();
        rig.chart
            .register_engine("pine", Rc::clone(&engine) as Rc<dyn ScriptingEngine>);
        (rig, engine)
    }

    pub(in crate::engine::orchestrator) const OVERLAY: &str =
        "//@version=5\nindicator(\"A\", overlay=true)\nplot(close)";
    pub(in crate::engine::orchestrator) const STUDY: &str =
        "//@version=5\nindicator(\"A\")\nplot(close)";

    pub(in crate::engine::orchestrator) fn add(rig: &Rig, source: &str) -> Rc<IndicatorHandleImpl> {
        rig.chart
            .add_indicator(source, AddIndicatorOptions::default())
            .expect("added")
    }

    pub(in crate::engine::orchestrator) fn mounts(rig: &Rig, id: &str) -> Vec<IndicatorModel> {
        rig.renderer
            .mounted_models
            .borrow()
            .iter()
            .filter(|m| m.id == id)
            .cloned()
            .collect()
    }

    pub(in crate::engine::orchestrator) fn last_status(
        rig: &Rig,
        id: &str,
    ) -> Option<IndicatorStatus> {
        rig.renderer
            .statuses
            .borrow()
            .iter()
            .rev()
            .find(|(i, _)| i == id)
            .map(|(_, s)| *s)
    }

    pub(in crate::engine::orchestrator) fn patches(rig: &Rig, id: &str) -> usize {
        rig.renderer
            .updated
            .borrow()
            .iter()
            .filter(|(i, _)| i == id)
            .count()
    }

    pub(in crate::engine::orchestrator) fn count(
        map: &RefCell<HashMap<String, usize>>,
        id: &str,
    ) -> usize {
        map.borrow().get(id).copied().unwrap_or(0)
    }

    pub(in crate::engine::orchestrator) fn line_series(
        id: &str,
        title: &str,
        bars: &[Ohlcv],
        add: f64,
    ) -> SeriesSpec {
        SeriesSpec {
            id: id.to_string(),
            title: title.to_string(),
            display_title: None,
            pane_id: "unrouted".to_string(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::LineLike {
                kind: LineLikeKind::Line,
                points: bars
                    .iter()
                    .map(|b| SeriesPoint {
                        time: b.time,
                        value: Some(b.close + add),
                        color: None,
                    })
                    .collect(),
                style: LineLikeStyle {
                    color: "#f00".to_string(),
                    width: 1.0,
                    line_style: LineStyle::Solid,
                    base: None,
                },
            },
        }
    }

    // ── the auto-running script engine ──

    #[derive(Clone, Copy, PartialEq)]
    enum Broken {
        Sync,
        Async,
    }

    struct Token {
        instance: String,
        overlay: bool,
        broken: Option<Broken>,
    }

    pub(in crate::engine::orchestrator) struct Sink {
        id: String,
        prepared: PreparedScript,
        get_bars: Option<Rc<dyn Fn() -> Vec<Ohlcv>>>,
        bars: Vec<Ohlcv>,
        handlers: RefCell<Option<ExecutionHandlers>>,
        inputs: RefCell<Inputs>,
        stopped: Cell<bool>,
        deferred: Cell<bool>,
        /// A held (deferred-engine) session ignores every poke.
        inert: bool,
        engine: Weak<AutoEngine>,
    }

    impl Sink {
        fn with_handlers(&self, call: impl FnOnce(&mut ExecutionHandlers)) {
            let taken = self.handlers.borrow_mut().take();
            if let Some(mut handlers) = taken {
                call(&mut handlers);
                if self.handlers.borrow().is_none() {
                    *self.handlers.borrow_mut() = Some(handlers);
                }
            }
        }

        fn current_bars(&self) -> Vec<Ohlcv> {
            self.get_bars
                .as_ref()
                .map_or_else(|| self.bars.clone(), |f| f())
        }

        fn record_length(&self, engine: &AutoEngine) {
            let length = match self.inputs.borrow().get("Length") {
                Some(InputValue::Number(n)) => *n,
                _ => f64::NAN,
            };
            engine
                .run_lengths
                .borrow_mut()
                .entry(self.id.clone())
                .or_default()
                .push(length);
        }

        /// A run: counted, then its model through the handlers.
        fn emit(&self) {
            if self.stopped.get() {
                return;
            }
            let Some(engine) = self.engine.upgrade() else {
                return;
            };
            *engine
                .run_count
                .borrow_mut()
                .entry(self.id.clone())
                .or_default() += 1;
            self.record_length(&engine);
            let model =
                engine.build_model(&self.prepared, &self.current_bars(), &self.inputs.borrow());
            self.with_handlers(|h| {
                (h.on_model)(model);
                if let Some(done) = h.on_done.as_mut() {
                    done();
                }
            });
        }

        /// One streamed model (`emitStream`): not counted as a run.
        fn emit_stream(&self) {
            let Some(engine) = self.engine.upgrade() else {
                return;
            };
            self.record_length(&engine);
            let model =
                engine.build_model(&self.prepared, &self.current_bars(), &self.inputs.borrow());
            self.with_handlers(|h| (h.on_model)(model));
        }
    }

    struct AutoSession {
        sink: Rc<Sink>,
        live: bool,
        engine: Weak<AutoEngine>,
    }

    impl ExecutionSession for AutoSession {
        fn get_context(
            &self,
            select: Option<&[ContextKey]>,
        ) -> Option<LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>>>
        {
            let engine = self.engine.upgrade()?;
            let mut snapshot = engine.context.borrow().clone()?;
            if let Some(select) = select {
                // An empty selection is the capability probe: not a pull.
                if !select.is_empty() {
                    engine.context_calls.borrow_mut().push(
                        select
                            .iter()
                            .map(|k| k.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                }
                if !select.contains(&ContextKey::Plots) {
                    snapshot.plots.clear();
                }
                if !select.contains(&ContextKey::Trades) {
                    snapshot.trades = None;
                }
            }
            Some(Box::pin(async move { Ok(Some(snapshot)) }))
        }

        fn stop(&self) {
            self.sink.stopped.set(true);
            if self.live
                && let Some(engine) = self.engine.upgrade()
            {
                *engine
                    .stream_stops
                    .borrow_mut()
                    .entry(self.sink.id.clone())
                    .or_default() += 1;
            }
        }

        fn update(&self, inputs: &Inputs, props: Option<&Inputs>) {
            if let Some(engine) = self.engine.upgrade() {
                engine
                    .updates
                    .borrow_mut()
                    .push((inputs.clone(), props.cloned()));
            }
            *self.sink.inputs.borrow_mut() = inputs.clone();
            if !self.live && !self.sink.inert && !self.sink.deferred.get() {
                self.sink.emit();
            }
        }

        fn set_visible_range(&self, range: VisibleBarRange) {
            if let Some(engine) = self.engine.upgrade() {
                engine
                    .last_visible_range
                    .borrow_mut()
                    .insert(self.sink.id.clone(), range);
            }
            if !self.live && !self.sink.inert && !self.sink.deferred.get() {
                self.sink.emit();
            }
        }

        fn notify_bars(&self, reason: Option<BarsChangeReason>) {
            if self.live || self.sink.inert {
                return;
            }
            let Some(engine) = self.engine.upgrade() else {
                return;
            };
            if engine.policy_a.get() && reason == Some(BarsChangeReason::Backfill) {
                return;
            }
            if reason == Some(BarsChangeReason::Complete) {
                self.sink.deferred.set(false);
            }
            if !self.sink.deferred.get() {
                self.sink.emit();
            }
        }
    }

    /// The original's `MockEngine`: a static run emits at once and again whenever
    /// the session is poked; a live one emits when the test says so
    /// (`emit_stream`). Knobs cover the variants the original derives from it.
    pub(in crate::engine::orchestrator) struct AutoEngine {
        me: Weak<AutoEngine>,
        pub caps: Cell<EngineCapabilities>,
        /// Hold every static run under a `backfill` history state until `Complete`.
        pub policy_a: Cell<bool>,
        /// Emit a strategy-style trade pair with every model.
        pub emit_trades: Cell<bool>,
        /// Prepare-time meta declares this compact shorttitle (and the model carries it).
        pub short_title: RefCell<Option<String>>,
        /// Declaration props (a strategy's `initial_capital`, ...): the
        /// `PropsEngine` shape.
        pub props_schema: Cell<bool>,
        /// Add a candle series, colored per bar by the palette (None: uncolored).
        pub candles: Cell<bool>,
        pub palette: RefCell<Option<[&'static str; 2]>>,
        /// Defer every static run until `finish()` (the `DeferredEngine`).
        pub hold: Cell<bool>,
        /// Overlay flag the deferred model carries, when it differs from the prepare-time guess.
        pub emit_overlay: Cell<Option<bool>>,
        /// A run over zero bars fabricates the empty default shape (a pinets-shaped engine).
        pub empty_when_barless: Cell<bool>,
        /// What `get_context` answers (None: no capability).
        pub context: RefCell<Option<EngineContextSnapshot>>,
        pub context_calls: RefCell<Vec<String>>,
        pub run_count: RefCell<HashMap<String, usize>>,
        pub run_lengths: RefCell<HashMap<String, Vec<f64>>>,
        pub stream_starts: RefCell<HashMap<String, usize>>,
        pub stream_stops: RefCell<HashMap<String, usize>>,
        pub last_visible_range: RefCell<HashMap<String, VisibleBarRange>>,
        pub execute_props: RefCell<Vec<Option<Inputs>>>,
        pub updates: RefCell<Vec<(Inputs, Option<Inputs>)>>,
        sinks: RefCell<HashMap<String, Rc<Sink>>>,
        pending: RefCell<Vec<Rc<Sink>>>,
        async_failures: RefCell<Vec<Rc<Sink>>>,
    }

    impl AutoEngine {
        pub fn new() -> Rc<Self> {
            Rc::new_cyclic(|me| Self {
                me: me.clone(),
                caps: Cell::new(EngineCapabilities {
                    streaming: true,
                    visible_range: true,
                    inputs: true,
                    props: false,
                }),
                policy_a: Cell::new(false),
                emit_trades: Cell::new(false),
                short_title: RefCell::new(None),
                props_schema: Cell::new(false),
                candles: Cell::new(false),
                palette: RefCell::new(None),
                hold: Cell::new(false),
                emit_overlay: Cell::new(None),
                empty_when_barless: Cell::new(false),
                context: RefCell::new(None),
                context_calls: RefCell::default(),
                run_count: RefCell::default(),
                run_lengths: RefCell::default(),
                stream_starts: RefCell::default(),
                stream_stops: RefCell::default(),
                last_visible_range: RefCell::default(),
                execute_props: RefCell::default(),
                updates: RefCell::default(),
                sinks: RefCell::default(),
                pending: RefCell::default(),
                async_failures: RefCell::default(),
            })
        }

        /// A streamed model (initial run or a live tick) of `id`'s latest session.
        pub fn emit_stream(&self, id: &str) {
            let sink = self.sinks.borrow().get(id).cloned();
            if let Some(sink) = sink {
                sink.emit_stream();
            }
        }

        /// The deferred runs complete: each emits its model.
        pub fn finish(&self) {
            let pending = std::mem::take(&mut *self.pending.borrow_mut());
            for sink in pending {
                let overlay = self.emit_overlay.get().unwrap_or_else(|| {
                    sink.prepared
                        .token
                        .downcast_ref::<Token>()
                        .is_some_and(|t| t.overlay)
                });
                let mut model = empty_model(&sink.id, "Mock", overlay);
                model.shorttitle = self.short_title.borrow().clone();
                model.series.push(line_series(
                    &format!("{}:line:mock#0", sink.id),
                    "Mock",
                    &[bar_at(1, 1.0)],
                    0.0,
                ));
                sink.with_handlers(|h| {
                    (h.on_model)(model);
                    if let Some(done) = h.on_done.as_mut() {
                        done();
                    }
                });
            }
        }

        /// Fail every deferred run.
        pub fn fail_all(&self, message: &str) {
            let pending = std::mem::take(&mut *self.pending.borrow_mut());
            for sink in pending {
                let message = message.to_string();
                sink.with_handlers(|h| {
                    if let Some(f) = h.on_error.as_mut() {
                        f(message);
                    }
                });
            }
        }

        /// The runtime failure a `// runtime-broken-async` script raises a turn after its execute.
        pub fn fail_async(&self) {
            let sinks = std::mem::take(&mut *self.async_failures.borrow_mut());
            for sink in sinks {
                if sink.stopped.get() {
                    continue;
                }
                sink.with_handlers(|h| {
                    if let Some(f) = h.on_error.as_mut() {
                        f("mock: runtime error".to_string());
                    }
                });
            }
        }

        fn build_model(
            &self,
            prepared: &PreparedScript,
            bars: &[Ohlcv],
            inputs: &Inputs,
        ) -> IndicatorModel {
            let token = prepared
                .token
                .downcast_ref::<Token>()
                .expect("an AutoEngine token");
            if self.props_schema.get() {
                let mut model = empty_model(&token.instance, "Props", false);
                model.inputs = prepared.inputs.clone();
                model.input_values = inputs.clone();
                return model;
            }
            if self.empty_when_barless.get() && bars.is_empty() {
                return empty_model(&token.instance, "Indicator", false);
            }
            let length = match inputs.get("Length") {
                Some(InputValue::Number(n)) => *n,
                _ => 14.0,
            };
            let mut model = empty_model(&token.instance, "Mock", token.overlay);
            model.shorttitle = self.short_title.borrow().clone();
            model.series.push(line_series(
                &format!("{}:line:mock#0", token.instance),
                "Mock",
                bars,
                length,
            ));
            if self.candles.get() {
                let palette = *self.palette.borrow();
                model.series.push(SeriesSpec {
                    id: format!("{}:candle:trend#0", token.instance),
                    title: "Trend".to_string(),
                    display_title: None,
                    pane_id: "unrouted".to_string(),
                    z_order: None,
                    visible: None,
                    display: None,
                    overlay: false,
                    data: SeriesData::Candle {
                        kind: crate::model::CandleKind::Candle,
                        bars: bars.to_vec(),
                        style: None,
                        bar_colors: palette.map(|p| {
                            (0..bars.len())
                                .map(|i| {
                                    Some(crate::model::CandleBarColor {
                                        color: Some(p[i % 2].to_string()),
                                        ..Default::default()
                                    })
                                })
                                .collect()
                        }),
                    },
                });
            }
            if self.emit_trades.get() && bars.len() >= 2 {
                let (first, last) = (bars[0], bars[bars.len() - 1]);
                let trade = |b: Ohlcv, side, kind, label: &str| crate::model::TradeExecution {
                    time: b.time,
                    price: b.close,
                    side,
                    kind,
                    label: Some(label.to_string()),
                    qty: Some(2.0),
                    trade_id: Some("t1".to_string()),
                };
                model.trades = vec![
                    trade(
                        first,
                        crate::model::TradeSide::Buy,
                        crate::model::TradeKind::Entry,
                        "Long",
                    ),
                    trade(
                        last,
                        crate::model::TradeSide::Sell,
                        crate::model::TradeKind::Exit,
                        "Exit",
                    ),
                ];
            }
            model.inputs = prepared.inputs.clone();
            model.input_values = inputs.clone();
            model
        }
    }

    impl ScriptingEngine for AutoEngine {
        fn language(&self) -> &str {
            "pine"
        }

        fn capabilities(&self) -> EngineCapabilities {
            self.caps.get()
        }

        fn prepare(
            &self,
            source: &str,
            instance_id: &str,
        ) -> LocalBoxFuture<'static, Result<PreparedScript, EngineError>> {
            if source.contains("// broken") {
                return Box::pin(std::future::ready(Err(EngineError::new(
                    "mock: syntax error",
                ))));
            }
            let overlay = source.contains("overlay=true") || source.contains("overlay = true");
            let broken = if source.contains("// runtime-broken-async") {
                Some(Broken::Async)
            } else if source.contains("// runtime-broken") {
                Some(Broken::Sync)
            } else {
                None
            };
            let keys: Vec<String> = source
                .split("// inputs:")
                .nth(1)
                .and_then(|rest| rest.lines().next())
                .map(|list| {
                    list.trim()
                        .split(',')
                        .map(|k| k.trim().to_string())
                        .collect()
                })
                .unwrap_or_else(|| vec!["Length".to_string()]);
            let defval = source
                .split("// default:")
                .nth(1)
                .and_then(|rest| {
                    rest.trim_start()
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                })
                .and_then(|n| n.parse::<f64>().ok())
                .unwrap_or(14.0);
            let (inputs, props) = if self.props_schema.get() {
                (
                    vec![InputSchema::new("Length", "Length", InputType::Int, 14.0)],
                    Some(vec![
                        InputSchema::new(
                            "initial_capital",
                            "Initial capital",
                            InputType::Float,
                            50_000.0,
                        ),
                        InputSchema::new("pyramiding", "Pyramiding", InputType::Int, 0.0),
                    ]),
                )
            } else {
                (
                    keys.iter()
                        .map(|k| InputSchema::new(k.clone(), k.clone(), InputType::Int, defval))
                        .collect(),
                    None,
                )
            };
            let result = PreparedScript {
                language: "pine".to_string(),
                inputs,
                props,
                meta: IndicatorMeta {
                    title: "Mock".to_string(),
                    shorttitle: self.short_title.borrow().clone(),
                    overlay,
                    precision: None,
                    format: None,
                },
                reacts_to_viewport: Rc::new(Cell::new(source.to_lowercase().contains("visible"))),
                token: Rc::new(Token {
                    instance: instance_id.to_string(),
                    overlay,
                    broken,
                }),
            };
            Box::pin(std::future::ready(Ok(result)))
        }

        fn execute(
            &self,
            req: ExecutionRequest,
            handlers: ExecutionHandlers,
        ) -> Rc<dyn ExecutionSession> {
            let (instance, broken) = {
                let t = req
                    .prepared
                    .token
                    .downcast_ref::<Token>()
                    .expect("an AutoEngine token");
                (t.instance.clone(), t.broken)
            };
            self.execute_props.borrow_mut().push(req.props.clone());
            let live = req.mode == ExecutionMode::Live && broken.is_none();
            let deferred = self.policy_a.get() && req.history_state() == HistoryState::Backfill;
            let sink = Rc::new(Sink {
                id: instance.clone(),
                prepared: req.prepared.clone(),
                get_bars: req.get_bars.clone(),
                bars: req.bars.clone(),
                handlers: RefCell::new(Some(handlers)),
                inputs: RefCell::new(req.inputs.clone().unwrap_or_default()),
                stopped: Cell::new(false),
                deferred: Cell::new(deferred),
                inert: self.hold.get(),
                engine: self.me.clone(),
            });
            let session = Rc::new(AutoSession {
                sink: Rc::clone(&sink),
                live,
                engine: self.me.clone(),
            });
            match broken {
                Some(Broken::Sync) => {
                    sink.with_handlers(|h| {
                        if let Some(f) = h.on_error.as_mut() {
                            f("mock: runtime error".to_string());
                        }
                    });
                    return session;
                }
                Some(Broken::Async) => {
                    self.async_failures.borrow_mut().push(sink);
                    return session;
                }
                None => {}
            }
            if live {
                *self
                    .stream_starts
                    .borrow_mut()
                    .entry(instance.clone())
                    .or_default() += 1;
                self.sinks.borrow_mut().insert(instance, sink);
                return session;
            }
            if self.hold.get() {
                self.pending.borrow_mut().push(sink);
                return session;
            }
            self.sinks.borrow_mut().insert(instance, Rc::clone(&sink));
            if !sink.deferred.get() {
                sink.emit();
            }
            session
        }
    }

    /// What `get_context` answers in the `script:run` tests (the original's `RunEngine`).
    pub(in crate::engine::orchestrator) fn run_context() -> EngineContextSnapshot {
        use crate::model::strategy::{StrategyFill, StrategySide, StrategyTrade};
        let fill = |id: &str, time: i64, price: f64| StrategyFill {
            id: id.to_string(),
            time,
            price,
            comment: None,
        };
        EngineContextSnapshot {
            language: "pine".to_string(),
            phase: ContextPhase::Idle,
            bar_index: 9,
            meta: ContextMeta {
                title: "Ctx".to_string(),
                overlay: false,
                precision: None,
                shorttitle: None,
            },
            plots: [(
                "a".to_string(),
                vec![ContextPlotPoint {
                    time: 1,
                    value: serde_json::json!(2),
                }],
            )]
            .into(),
            variables: [
                ("posSize".to_string(), serde_json::json!(2)),
                ("len".to_string(), serde_json::json!(14)),
            ]
            .into(),
            strategy: Some(StrategyState {
                position: 2.0,
                equity: 10_000.0,
                ..StrategyState::default()
            }),
            trades: Some(vec![
                StrategyTrade {
                    id: "t0".to_string(),
                    side: StrategySide::Long,
                    qty: 2.0,
                    entry: fill("Long", 1, 100.0),
                    exit: Some(fill("Exit", 2, 110.0)),
                    open: false,
                    pnl: Some(9.5),
                    commission: Some(0.5),
                    max_drawdown: Some(2.5),
                    max_runup: Some(12.0),
                },
                StrategyTrade {
                    id: "t1".to_string(),
                    side: StrategySide::Long,
                    qty: 2.0,
                    entry: fill("Long", 3, 105.0),
                    exit: None,
                    open: true,
                    pnl: None,
                    commission: None,
                    max_drawdown: None,
                    max_runup: None,
                },
            ]),
            warnings: Vec::new(),
        }
    }

    pub(in crate::engine::orchestrator) fn alert(id: &str) -> EngineAlert {
        EngineAlert {
            id: id.to_string(),
            message: "cross".to_string(),
            title: None,
            time: 1,
            bar_index: 0,
            freq: None,
        }
    }

    // ── test native indicators ──

    #[derive(Clone, Debug, Default, PartialEq)]
    pub(in crate::engine::orchestrator) struct Calls {
        pub start: u32,
        pub on_bars: u32,
        pub on_viewport: u32,
        pub set_inputs: u32,
        pub suspend: u32,
        pub resume: u32,
        pub stop: u32,
        pub ctx_id: Option<String>,
        pub symbol: Option<String>,
    }

    pub(in crate::engine::orchestrator) type CallLog = Arc<Mutex<Calls>>;
    pub(in crate::engine::orchestrator) type Made = Arc<Mutex<Vec<CallLog>>>;

    pub(in crate::engine::orchestrator) fn made_calls(made: &Made, n: usize) -> Calls {
        made.lock().expect("made")[n].lock().expect("calls").clone()
    }

    pub(in crate::engine::orchestrator) fn made_len(made: &Made) -> usize {
        made.lock().expect("made").len()
    }

    #[derive(Clone, Copy, Debug)]
    pub(in crate::engine::orchestrator) enum Kind {
        /// Emits one line series of the closes; counts every hook.
        Line,
        /// Pushes `"live"` on start and `null` in stop; pushes `"late"` on every bars change.
        Late,
        /// A layer native: an empty emit, then `{from: <ctx id>}`.
        Layer,
        /// Emits only a categorical pane axis.
        Unscaled,
        /// Counts hooks; emits an empty model on start.
        Probe,
        /// Plots each bar's close, recomputing on every change.
        CloseLine,
    }

    pub(in crate::engine::orchestrator) struct TestNative {
        kind: Kind,
        log: CallLog,
        suspended: bool,
    }

    impl TestNative {
        fn line(ctx: &mut dyn NativeIndicatorContext, id: &str, title: &str) {
            let bars: Vec<Ohlcv> = ctx.bars().to_vec();
            ctx.emit(NativeIndicatorOutput {
                series: vec![line_series(id, title, &bars, 0.0)],
                ..NativeIndicatorOutput::default()
            });
        }

        fn bump(&self, f: impl FnOnce(&mut Calls)) {
            f(&mut self.log.lock().expect("calls"));
        }

        fn recompute(&self, ctx: &mut dyn NativeIndicatorContext) {
            match self.kind {
                Kind::Line => Self::line(ctx, "native:line:0", "Native"),
                Kind::CloseLine => Self::line(ctx, "close", "Close"),
                _ => {}
            }
        }
    }

    impl NativeIndicator for TestNative {
        fn start(&mut self, ctx: &mut dyn NativeIndicatorContext, _inputs: Inputs) {
            let (id, symbol) = (ctx.id().to_string(), ctx.symbol().to_string());
            self.bump(|c| {
                c.start += 1;
                c.ctx_id = Some(id.clone());
                c.symbol = Some(symbol);
            });
            match self.kind {
                Kind::Line | Kind::CloseLine => self.recompute(ctx),
                Kind::Late => {
                    ctx.emit(NativeIndicatorOutput::default());
                    ctx.push_data(serde_json::json!("live"));
                }
                Kind::Layer => {
                    ctx.emit(NativeIndicatorOutput::default());
                    ctx.push_data(serde_json::json!({ "from": id }));
                }
                Kind::Unscaled => ctx.emit(NativeIndicatorOutput {
                    pane_axis: Some(crate::model::PaneAxis::Bands(vec![
                        crate::model::PaneAxisBand {
                            frac: 0.25,
                            label: "Top".to_string(),
                        },
                        crate::model::PaneAxisBand {
                            frac: 0.75,
                            label: "Bottom".to_string(),
                        },
                    ])),
                    ..NativeIndicatorOutput::default()
                }),
                Kind::Probe => ctx.emit(NativeIndicatorOutput::default()),
            }
        }

        fn on_bars(&mut self, ctx: &mut dyn NativeIndicatorContext) {
            self.bump(|c| c.on_bars += 1);
            if let Kind::Late = self.kind {
                ctx.push_data(serde_json::json!("late"));
            } else {
                self.recompute(ctx);
            }
        }

        fn on_viewport(&mut self, ctx: &mut dyn NativeIndicatorContext, _range: VisibleRange) {
            self.bump(|c| c.on_viewport += 1);
            if let Kind::Line = self.kind {
                self.recompute(ctx);
            }
        }

        fn set_inputs(&mut self, ctx: &mut dyn NativeIndicatorContext, _inputs: Inputs) {
            self.bump(|c| c.set_inputs += 1);
            if !self.suspended {
                self.recompute(ctx);
            }
        }

        fn suspend(&mut self) {
            self.bump(|c| c.suspend += 1);
            self.suspended = true;
        }

        fn resume(&mut self, ctx: &mut dyn NativeIndicatorContext) {
            self.bump(|c| c.resume += 1);
            self.suspended = false;
            self.recompute(ctx);
        }

        fn stop(&mut self, ctx: &mut dyn NativeIndicatorContext) {
            self.bump(|c| c.stop += 1);
            if let Kind::Late = self.kind {
                ctx.push_data(Value::Null);
            }
        }
    }

    /// A descriptor of a test native type; every instance it makes is logged in `made`.
    pub(in crate::engine::orchestrator) fn descriptor(
        indicator_type: &str,
        kind: Kind,
        multi: bool,
        made: &Made,
    ) -> NativeIndicatorDescriptor {
        let made = Arc::clone(made);
        NativeIndicatorDescriptor {
            indicator_type: indicator_type.to_string(),
            title: crate::i18n::MessageLike::from(format!("Native {indicator_type}")),
            short_title: None,
            pane_hint: if matches!(kind, Kind::Unscaled) {
                crate::model::PaneHint::New
            } else {
                crate::model::PaneHint::Price
            },
            overlay: !matches!(kind, Kind::Unscaled),
            reacts_to_viewport: true,
            waits_for_history: false,
            beta: false,
            legend: None,
            multi_instance: multi,
            inputs_schema: vec![InputSchema::new("len", "Length", InputType::Int, 5.0)],
            factory: Arc::new(move || {
                let log: CallLog = Arc::default();
                made.lock().expect("made").push(Arc::clone(&log));
                Box::new(TestNative {
                    kind,
                    log,
                    suspended: false,
                })
            }),
        }
    }

    pub(in crate::engine::orchestrator) fn register(
        rig: &Rig,
        descriptor: NativeIndicatorDescriptor,
    ) {
        rig.registries
            .native_indicators
            .borrow_mut()
            .register(descriptor);
    }

    pub(in crate::engine::orchestrator) fn made() -> Made {
        Arc::default()
    }
}

/// A chart wired by hand around a renderer / feed WRAPPER: a renderer that
/// records the pane calls the shared `RecRenderer` does not, a feed that caps
/// the ranged chunk size.
#[cfg(test)]
pub(super) mod wiring {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::super::test_support::*;
    use super::fixtures::*;
    use super::*;
    use crate::model::Pane;
    use crate::options::MarketSwitch;
    use crate::ports::chart_renderer::{
        ChartTypeSettingsListener, ClickEvent, CrosshairEvent, IndicatorRenderHandle,
        InputChangeEvent, Listener, MoveIndicatorListener, PaneAction,
        PriceStyleWillChangeListener, RendererCapabilities, SetBarsOptions, ThemeId,
        ToggleVisibleListener, VisibleRange, VisibleRangeOptions,
    };
    use crate::ports::market_data::{
        BarRange, BarSink, MarketSession, SymbolInfo, UnresolvedSymbol,
    };
    use crate::runtime::testing::ManualPlatform;

    /// Every event of the chart, in order (the shared `EventLog` cannot attach to a hand-wired bus).
    #[derive(Clone, Default)]
    pub struct Events(pub Rc<RefCell<Vec<ChartEvent>>>);

    impl Events {
        pub fn attach(bus: &ChartEventBus) -> Self {
            let events = Self::default();
            for name in crate::events::types::ChartEventName::ALL {
                let sink = Rc::clone(&events.0);
                bus.on(*name, move |e| sink.borrow_mut().push(e.clone()));
            }
            events
        }

        pub fn count(&self, name: crate::events::types::ChartEventName) -> usize {
            self.0.borrow().iter().filter(|e| e.name() == name).count()
        }

        pub fn clear(&self) {
            self.0.borrow_mut().clear();
        }
    }

    /// The pane-aware renderer: forwards everything to the shared recorder and
    /// records the pane operations on top.
    pub struct PaneRec {
        pub inner: Rc<RecRenderer>,
        pub manage: bool,
        pub sizing: bool,
        pub weights: RefCell<HashMap<String, f64>>,
        pub set_pane_calls: RefCell<Vec<(String, String, bool)>>,
        pub order_calls: RefCell<Vec<Vec<String>>>,
        pub collapse_calls: RefCell<Vec<(String, bool)>>,
        pub maximize_calls: RefCell<Vec<Option<String>>>,
        pub ensure_calls: RefCell<Vec<Pane>>,
    }

    impl ChartRenderer for PaneRec {
        fn capabilities(&self) -> RendererCapabilities {
            RendererCapabilities {
                pane_management: self.manage,
                ..self.inner.capabilities()
            }
        }
        fn name(&self) -> &str {
            "pane-fake"
        }
        fn features(&self) -> Vec<String> {
            Vec::new()
        }
        fn apply_feature(&self, _key: &str, _value: &Value) {}
        fn read_feature(&self, _key: &str) -> Option<Value> {
            None
        }
        fn mount(&self, theme: &VelaTheme) {
            self.inner.mount(theme);
        }
        fn set_theme(&self, theme: &VelaTheme) {
            self.inner.set_theme(theme);
        }
        fn resize(&self) {}
        fn destroy(&self) {
            self.inner.destroy();
        }
        fn set_bars(&self, bars: &[Ohlcv], opts: SetBarsOptions) {
            self.inner.set_bars(bars, opts);
        }
        fn update_bar(&self, bar: &Ohlcv) {
            self.inner.update_bar(bar);
        }
        fn set_loading(&self, loading: bool) {
            self.inner.set_loading(loading);
        }
        fn set_indicator_status(&self, handle: &IndicatorRenderHandle, status: IndicatorStatus) {
            self.inner.set_indicator_status(handle, status);
        }
        fn set_indicator_visible(&self, handle: &IndicatorRenderHandle, visible: bool) {
            self.inner.set_indicator_visible(handle, visible);
        }
        fn ensure_pane(&self, pane: &Pane) {
            self.ensure_calls.borrow_mut().push(pane.clone());
            self.weights
                .borrow_mut()
                .entry(pane.id.clone())
                .or_insert(if pane.id == "price" { 3.0 } else { 1.0 });
            self.inner.ensure_pane(pane);
        }
        fn remove_pane(&self, id: &str) {
            self.weights.borrow_mut().remove(id);
            self.inner.remove_pane(id);
        }
        fn supports_set_indicator_pane(&self) -> bool {
            self.manage
        }
        fn set_indicator_pane(
            &self,
            handle: &IndicatorRenderHandle,
            pane_id: &str,
            own_scale: bool,
        ) {
            self.set_pane_calls.borrow_mut().push((
                handle.id.clone(),
                pane_id.to_string(),
                own_scale,
            ));
        }
        fn order_panes(&self, ordered_ids: &[String]) {
            self.order_calls.borrow_mut().push(ordered_ids.to_vec());
        }
        fn set_pane_collapsed(&self, pane_id: &str, collapsed: bool) {
            self.collapse_calls
                .borrow_mut()
                .push((pane_id.to_string(), collapsed));
        }
        fn set_pane_maximized(&self, pane_id: Option<&str>) {
            self.maximize_calls
                .borrow_mut()
                .push(pane_id.map(str::to_string));
        }
        fn supports_set_pane_height_weight(&self) -> bool {
            self.sizing
        }
        fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
            self.weights
                .borrow_mut()
                .insert(pane_id.to_string(), weight);
        }
        fn pane_height_weight(&self, pane_id: &str) -> Option<f64> {
            if self.sizing {
                self.weights.borrow().get(pane_id).copied()
            } else {
                None
            }
        }
        fn on_pane_action(&self, cb: Listener<PaneAction>) -> Option<Unsubscribe> {
            self.inner.on_pane_action(cb)
        }
        fn on_move_indicator(&self, cb: MoveIndicatorListener) -> Option<Unsubscribe> {
            self.inner.on_move_indicator(cb)
        }
        fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
            self.inner.mount_indicator(model)
        }
        fn update_indicator(
            &self,
            handle: &IndicatorRenderHandle,
            patch: &crate::model::ScenePatch,
        ) {
            self.inner.update_indicator(handle, patch);
        }
        fn remove_indicator(&self, handle: &IndicatorRenderHandle) {
            self.inner.remove_indicator(handle);
        }
        fn set_indicator_inputs(
            &self,
            handle: &IndicatorRenderHandle,
            values: &Inputs,
            props: Option<&Inputs>,
        ) {
            self.inner.set_indicator_inputs(handle, values, props);
        }
        fn on_price_style_change(&self, cb: Listener<PriceStyle>) -> Option<Unsubscribe> {
            self.inner.on_price_style_change(cb)
        }
        fn on_price_style_will_change(
            &self,
            cb: PriceStyleWillChangeListener,
        ) -> Option<Unsubscribe> {
            self.inner.on_price_style_will_change(cb)
        }
        fn on_chart_type_settings_change(
            &self,
            cb: ChartTypeSettingsListener,
        ) -> Option<Unsubscribe> {
            self.inner.on_chart_type_settings_change(cb)
        }
        fn on_theme_select(&self, cb: Listener<ThemeId>) -> Option<Unsubscribe> {
            self.inner.on_theme_select(cb)
        }
        fn on_toggle_indicator_visible(&self, cb: ToggleVisibleListener) -> Option<Unsubscribe> {
            self.inner.on_toggle_indicator_visible(cb)
        }
        fn on_input_change(&self, cb: Listener<InputChangeEvent>) -> Unsubscribe {
            self.inner.on_input_change(cb)
        }
        fn on_remove_indicator(&self, cb: Listener<String>) -> Unsubscribe {
            self.inner.on_remove_indicator(cb)
        }
        fn on_crosshair_move(&self, cb: Listener<CrosshairEvent>) -> Unsubscribe {
            self.inner.on_crosshair_move(cb)
        }
        fn on_click(&self, cb: Listener<ClickEvent>) -> Unsubscribe {
            self.inner.on_click(cb)
        }
        fn get_visible_range(&self) -> Option<VisibleRange> {
            self.inner.get_visible_range()
        }
        fn set_visible_range(&self, range: VisibleRange, opts: VisibleRangeOptions) {
            self.inner.set_visible_range(range, opts);
        }
        fn on_viewport_change(&self, cb: Listener<VisibleRange>) -> Unsubscribe {
            self.inner.on_viewport_change(cb)
        }
    }

    /// The shared fake feed, with every ranged chunk capped at `cap` bars.
    pub struct CappedFeed {
        pub inner: Rc<FakeFeed>,
        pub cap: usize,
    }

    impl MarketDataFeed for CappedFeed {
        fn supports_load_range(&self) -> bool {
            self.inner.supports_load_range()
        }
        fn supports_load_progressive(&self) -> bool {
            self.inner.supports_load_progressive()
        }
        fn load(
            &self,
            cfg: &MarketConfig,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.inner.load(cfg)
        }
        fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
            self.inner.subscribe(cfg, on_bar)
        }
        fn symbol_info(&self, cfg: &MarketConfig) -> Option<SymbolInfo> {
            self.inner.symbol_info(cfg)
        }
        fn load_range(
            &self,
            cfg: &MarketConfig,
            range: &BarRange,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
            let mut capped = range.clone();
            capped.limit = Some(range.limit.unwrap_or(self.cap).min(self.cap));
            self.inner.load_range(cfg, &capped)
        }
        fn on_unresolved(
            &self,
            callback: Box<dyn FnMut(&UnresolvedSymbol)>,
        ) -> Option<Unsubscribe> {
            self.inner.on_unresolved(callback)
        }
        fn destroy(&self) {
            self.inner.destroy();
        }
    }

    /// A hand-wired chart (no scripting engine of its own: pass the language in the options).
    pub struct Wired {
        pub platform: ManualPlatform,
        pub renderer: Rc<RecRenderer>,
        pub chart: EngineOrchestrator,
        pub events: Events,
        pub engine: Rc<AutoEngine>,
    }

    impl Wired {
        pub fn new(
            wrap_renderer: impl FnOnce(Rc<RecRenderer>) -> Rc<dyn ChartRenderer>,
            wrap_feed: impl FnOnce(Rc<FakeFeed>) -> Rc<dyn MarketDataFeed>,
            bars: Option<usize>,
            registries: Registries,
            feed_setup: impl FnOnce(&FakeFeed),
            listen: bool,
        ) -> Self {
            let platform = ManualPlatform::new(T0 as f64 + 100.0 * HOUR as f64);
            let feed = FakeFeed::new();
            feed_setup(&feed);
            let inner = RecRenderer::new();
            let engine = AutoEngine::new();
            let config = ResolvedConfig {
                market: MarketConfig {
                    symbol: Some("AAA".to_string()),
                    timeframe: Some("60".to_string()),
                    bars,
                    session: None::<MarketSession>,
                    visible_range: None,
                    data: None,
                },
                live: false,
                theme: crate::theme::dark_theme(),
                default_language: "pine".to_string(),
                drawings: None,
                volume: Some(false),
            };
            let chart = EngineOrchestrator::new(
                platform.handle(),
                wrap_renderer(Rc::clone(&inner)),
                wrap_feed(Rc::clone(&feed)),
                vec![Rc::clone(&engine) as Rc<dyn ScriptingEngine>],
                config,
                DataControl::new(None, platform.handle()),
                Services { registries },
            );
            // Without a listener on every name, `has_listeners` reads what a real host sees.
            let events = if listen {
                Events::attach(&chart.events())
            } else {
                Events::default()
            };
            Self {
                platform,
                renderer: inner,
                chart,
                events,
                engine,
            }
        }

        /// A chart on the pane-aware renderer.
        pub fn panes(manage: bool, sizing: bool) -> (Self, Rc<PaneRec>) {
            let slot: Rc<RefCell<Option<Rc<PaneRec>>>> = Rc::default();
            let sink = Rc::clone(&slot);
            let wired = Self::new(
                move |inner| {
                    let rec = Rc::new(PaneRec {
                        inner,
                        manage,
                        sizing,
                        weights: RefCell::default(),
                        set_pane_calls: RefCell::default(),
                        order_calls: RefCell::default(),
                        collapse_calls: RefCell::default(),
                        maximize_calls: RefCell::default(),
                        ensure_calls: RefCell::default(),
                    });
                    *sink.borrow_mut() = Some(Rc::clone(&rec));
                    rec as Rc<dyn ChartRenderer>
                },
                |feed| feed as Rc<dyn MarketDataFeed>,
                None,
                Registries::builtin(),
                |_| {},
                true,
            );
            let rec = slot.borrow().clone().expect("the wrapper was built");
            (wired, rec)
        }

        pub fn flush(&self) {
            for _ in 0..4 {
                self.platform.run();
                self.platform.advance(0.0);
            }
            self.platform.run();
        }

        pub fn ready(&self) {
            self.platform.block_on(self.chart.ready()).expect("ready");
            self.flush();
        }

        pub fn block_on<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> T {
            self.platform.block_on(future)
        }

        pub fn switch(&self, next: MarketSwitch) {
            let done = self.chart.set_market(next);
            self.platform.block_on(done).expect("switch");
        }

        pub fn add(&self, source: &str) -> Rc<IndicatorHandleImpl> {
            add_to(&self.chart, source)
        }
    }

    fn add_to(chart: &EngineOrchestrator, source: &str) -> Rc<IndicatorHandleImpl> {
        chart
            .add_indicator(source, AddIndicatorOptions::default())
            .expect("added")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::fixtures::*;
    use super::*;
    use crate::engine::indicator_handle::{IndicatorEventName, IndicatorHandle};
    use crate::engine::orchestrator::test_support::*;
    use crate::events::types::ChartEventName;
    use crate::model::{Background, DrawingExtend, DrawingLine, DrawingXLoc, LineStyle};
    use crate::options::MarketSwitch;
    use crate::ports::chart_renderer::{InputChangeEvent, InputChangeKind, VisibleRange};
    use crate::ports::scripting_engine::EngineCapabilities;

    fn num(n: f64) -> InputValue {
        InputValue::Number(n)
    }

    fn event_ids(events: Vec<ChartEvent>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::IndicatorAdded { id } | ChartEvent::IndicatorRemoved { id } => Some(id),
                _ => None,
            })
            .collect()
    }

    fn ready(rig: &Rig) {
        rig.ready().expect("ready");
        flush(rig);
    }

    fn warned(rig: &Rig, needle: &str) -> bool {
        rig.platform.warnings().iter().any(|w| w.contains(needle))
    }

    fn points_of(model: &IndicatorModel) -> usize {
        model
            .series
            .iter()
            .find_map(|s| s.points().map(<[_]>::len))
            .unwrap_or(0)
    }

    // ── add, remove, hide, show ──

    #[test]
    fn overlays_route_to_price_studies_to_their_own_pane_and_an_input_edit_remounts() {
        let (rig, _engine) = auto_rig(Rig::builder());
        let ema = add(
            &rig,
            "//@version=5\nindicator(\"EMA\", overlay=true)\nplot(close)",
        );
        add(&rig, "//@version=5\nindicator(\"RSI\")\nplot(close)");
        // The handle exposes the source it was added with.
        assert_eq!(
            ema.source().as_deref(),
            Some("//@version=5\nindicator(\"EMA\", overlay=true)\nplot(close)")
        );
        ready(&rig);

        assert_eq!(rig.renderer.bars.borrow().len(), 50);
        // Each indicator mounts twice: the legend placeholder, then the computed model.
        let mounted = rig.renderer.mounted_models.borrow().clone();
        assert_eq!(mounted.len(), 4);
        let computed: Vec<&IndicatorModel> =
            mounted.iter().filter(|m| !m.series.is_empty()).collect();
        assert_eq!(computed.len(), 2);
        let overlay = computed.iter().find(|m| m.overlay).expect("overlay model");
        let study = computed.iter().find(|m| !m.overlay).expect("study model");
        assert_eq!(overlay.pane_id.as_deref(), Some("price"));
        assert_ne!(study.pane_id.as_deref(), Some("price"));
        assert!(overlay.series.iter().all(|s| s.pane_id == "price"));

        let before = rig.renderer.mounted_models.borrow().len();
        ema.set_input("Length", num(50.0));
        flush(&rig);
        // An input change re-runs through the idempotent remount: no teardown.
        assert_eq!(rig.renderer.mounted_models.borrow().len(), before + 1);
        assert_eq!(
            rig.renderer
                .mounted_models
                .borrow()
                .last()
                .map(|m| m.id.clone()),
            Some(ema.id().to_string())
        );
        assert!(rig.renderer.removed.borrow().is_empty());
        assert_eq!(rig.log.count(ChartEventName::IndicatorInputs), 1);
    }

    #[test]
    fn the_legend_x_removes_the_indicator_stops_its_stream_and_emits_the_event() {
        let (rig, engine) = auto_rig(Rig::builder().live(true));
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        engine.emit_stream(ind.id()); // mount it
        flush(&rig);
        rig.log.clear();

        rig.renderer.fire_remove(ind.id());

        assert!(
            rig.renderer
                .removed
                .borrow()
                .contains(&ind.id().to_string())
        );
        assert_eq!(count(&engine.stream_stops, ind.id()), 1);
        assert_eq!(event_ids(rig.log.all()), [ind.id()]);
        assert!(rig.chart.list_indicators().is_empty());
    }

    #[test]
    fn hide_suspends_an_indicator_and_show_reruns_and_remounts_it() {
        let (rig, _engine) = auto_rig(Rig::builder());
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        assert!(ind.visible());
        let mounts_before = mounts(&rig, ind.id()).len();
        assert!(mounts_before > 0);

        ind.set_visible(false);
        assert!(!ind.visible());
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(ind.id()),
            Some(&false)
        );
        assert!(
            rig.renderer.removed.borrow().is_empty(),
            "hidden is not removed"
        );
        assert_eq!(rig.log.count(ChartEventName::IndicatorVisibility), 1);

        ind.set_visible(true);
        flush(&rig);
        assert!(ind.visible());
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(ind.id()),
            Some(&true)
        );
        assert!(
            mounts(&rig, ind.id()).len() > mounts_before,
            "re-executed and re-mounted"
        );
    }

    #[test]
    fn an_indicator_added_hidden_mounts_a_dimmed_row_and_showing_runs_it() {
        let (rig, engine) = auto_rig(Rig::builder());
        let ind = add(&rig, OVERLAY);
        ind.set_visible(false); // hidden before prepare/execute: the restore ordering
        ready(&rig);

        let rows = mounts(&rig, ind.id());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].series.is_empty());
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(ind.id()),
            Some(&false)
        );
        assert_eq!(
            count(&engine.run_count, ind.id()),
            0,
            "hidden: never executed"
        );
        // Announced at once so host UIs can list it.
        assert_eq!(rig.log.count(ChartEventName::IndicatorAdded), 1);

        rig.renderer.fire_toggle_visible(ind.id(), true);
        flush(&rig);
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(ind.id()),
            Some(&true)
        );
        assert!(mounts(&rig, ind.id()).iter().any(|m| !m.series.is_empty()));
    }

    #[test]
    fn a_hidden_indicator_does_not_rerun_on_viewport_changes_and_the_eye_toggles_it() {
        let (rig, engine) = auto_rig(Rig::builder());
        let vr = add(
            &rig,
            "//@version=6\nindicator(\"VR\", overlay=true)\nx = chart.right_visible_bar_time\nplot(close)",
        );
        ready(&rig);

        rig.renderer.fire_toggle_visible(vr.id(), false);
        assert!(!vr.visible());
        let before = count(&engine.run_count, vr.id());
        rig.renderer.fire_viewport(VisibleRange {
            from: 1_700_000_036_000,
            to: 1_700_000_144_000,
        });
        rig.advance(220.0);
        flush(&rig);
        assert_eq!(
            count(&engine.run_count, vr.id()),
            before,
            "suspended: no session, no viewport re-run"
        );

        rig.renderer.fire_toggle_visible(vr.id(), true);
        flush(&rig);
        assert!(vr.visible());
        assert!(count(&engine.run_count, vr.id()) > before);
    }

    #[test]
    fn viewport_changes_rerun_only_visible_range_scripts_with_the_new_window() {
        let (rig, engine) = auto_rig(Rig::builder());
        let vr = add(
            &rig,
            "//@version=6\nindicator(\"VR\", overlay=true)\nx = chart.left_visible_bar_time\nplot(close)",
        );
        let plain = add(
            &rig,
            "//@version=5\nindicator(\"Plain\", overlay=true)\nplot(close)",
        );
        ready(&rig);
        let (vr_before, plain_before) = (
            count(&engine.run_count, vr.id()),
            count(&engine.run_count, plain.id()),
        );

        rig.renderer.fire_viewport(VisibleRange {
            from: 1_700_000_036_000,
            to: 1_700_000_144_000,
        });
        rig.advance(220.0);
        flush(&rig);

        assert_eq!(count(&engine.run_count, vr.id()), vr_before + 1);
        assert_eq!(
            engine.last_visible_range.borrow().get(vr.id()).copied(),
            Some(VisibleBarRange {
                left: 1_700_000_036_000.0,
                right: 1_700_000_144_000.0
            })
        );
        assert_eq!(count(&engine.run_count, plain.id()), plain_before);
        // The re-run went through the value-patch path, not a remount.
        assert!(patches(&rig, vr.id()) > 0);
        assert_eq!(patches(&rig, plain.id()), 0);
    }

    #[test]
    fn a_burst_of_viewport_events_is_one_rerun() {
        let (rig, engine) = auto_rig(Rig::builder());
        let vr = add(
            &rig,
            "//@version=6\nindicator(\"VR\", overlay=true)\nx = chart.right_visible_bar_time\nplot(close)",
        );
        ready(&rig);
        let before = count(&engine.run_count, vr.id());
        for i in 0..8 {
            rig.renderer.fire_viewport(VisibleRange {
                from: 1_700_000_000_000 + i * 1000,
                to: 1_700_000_100_000,
            });
        }
        rig.advance(220.0);
        flush(&rig);
        assert_eq!(count(&engine.run_count, vr.id()), before + 1);
    }

    #[test]
    fn a_live_non_viewport_script_streams_mounts_on_the_first_emit_and_patches_after() {
        let (rig, engine) = auto_rig(Rig::builder().live(true));
        let ind = add(
            &rig,
            "//@version=5\nindicator(\"Stream\", overlay=true)\nplot(close)",
        );
        ready(&rig);

        assert_eq!(count(&engine.stream_starts, ind.id()), 1);
        assert_eq!(count(&engine.run_count, ind.id()), 0);
        let pre = mounts(&rig, ind.id());
        assert_eq!(
            pre.len(),
            1,
            "only the loading placeholder until the first emit"
        );
        assert!(pre[0].series.is_empty());

        engine.emit_stream(ind.id()); // the first model remounts over the placeholder
        flush(&rig);
        assert_eq!(mounts(&rig, ind.id()).len(), 2);

        engine.emit_stream(ind.id()); // a tick value-patches
        flush(&rig);
        assert_eq!(mounts(&rig, ind.id()).len(), 2);
        assert_eq!(patches(&rig, ind.id()), 1);

        ind.remove();
        assert_eq!(count(&engine.stream_stops, ind.id()), 1);
    }

    #[test]
    fn a_policy_a_engine_executes_exactly_once_over_the_full_backfilled_history() {
        let builder = Rig::builder()
            .bars(25_000)
            .feed(|f| f.set_depth("AAA", 40_000));
        let (rig, engine) = auto_rig(builder);
        engine.policy_a.set(true);
        let ind = add(
            &rig,
            "//@version=5\nindicator(\"Deep\", overlay=true)\nplot(close)",
        );
        rig.loaded();
        flush(&rig);

        assert_eq!(
            count(&engine.run_count, ind.id()),
            1,
            "held through the backfill, ran once on complete"
        );
        let computed: Vec<IndicatorModel> = mounts(&rig, ind.id())
            .into_iter()
            .filter(|m| !m.series.is_empty())
            .collect();
        assert_eq!(computed.len(), 1);
        assert_eq!(points_of(&computed[0]), 25_000);
    }

    // ── engines ──

    #[test]
    fn the_last_engine_registered_for_a_language_wins_with_a_warning() {
        let rig = Rig::builder().build();
        let second = AutoEngine::new();
        rig.chart
            .register_engine("pine", Rc::clone(&second) as Rc<dyn ScriptingEngine>);
        assert!(warned(&rig, "language \"pine\""));
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        // Last-declared engine ran it; the rig's fake engine was replaced.
        assert!(count(&second.run_count, ind.id()) > 0);
        assert!(rig.engine.executions.borrow().is_empty());
        // Registering the same engine again is not a replacement.
        let before = rig.platform.warnings().len();
        rig.chart
            .register_engine("pine", second as Rc<dyn ScriptingEngine>);
        assert_eq!(rig.platform.warnings().len(), before);
    }

    #[test]
    fn an_engine_registered_after_construction_serves_later_adds() {
        let rig = Rig::builder().build();
        let engine = AutoEngine::new();
        rig.chart
            .register_engine("other", Rc::clone(&engine) as Rc<dyn ScriptingEngine>);
        let options = AddIndicatorOptions {
            language: Some("other".to_string()),
            ..Default::default()
        };
        let ind = rig.chart.add_indicator(OVERLAY, options).expect("added");
        ready(&rig);
        assert_eq!(ind.language(), Some("other"));
        assert!(count(&engine.run_count, ind.id()) > 0);
        assert!(!mounts(&rig, ind.id()).is_empty());
    }

    #[test]
    fn a_script_handle_names_its_engine_language_and_updating_keeps_it() {
        let (rig, _engine) = auto_rig(Rig::builder());
        let other = AutoEngine::new();
        rig.chart
            .register_engine("other", Rc::clone(&other) as Rc<dyn ScriptingEngine>);
        let by_default = add(&rig, OVERLAY);
        let explicit = rig
            .chart
            .add_indicator(
                OVERLAY,
                AddIndicatorOptions {
                    language: Some("other".to_string()),
                    ..Default::default()
                },
            )
            .expect("added");
        // Synchronous: a host gate reads it before the script is even prepared.
        assert_eq!(by_default.language(), Some("pine"));
        assert_eq!(explicit.language(), Some("other"));
        ready(&rig);
        assert!(count(&other.run_count, explicit.id()) > 0);
        assert_eq!(count(&other.run_count, by_default.id()), 0);

        let result = rig.block_on(
            explicit.update_code("//@version=5\nindicator(\"B2\", overlay=true)\nplot(close)"),
        );
        assert!(result.ok);
        assert!(explicit.source().unwrap_or_default().contains("B2"));
        assert_eq!(explicit.language(), Some("other"));
    }

    #[test]
    fn a_script_without_a_language_option_takes_the_charts_default_language() {
        let rig = Rig::builder().build();
        // The rig's default language is `pine`; a handle reads it without waiting for the prepare.
        assert_eq!(add(&rig, OVERLAY).language(), Some("pine"));
    }

    #[test]
    fn with_no_engine_registered_candles_render_and_the_indicator_fails_actionably() {
        let rig = Rig::builder().build();
        rig.chart
            .register_engine("pine", AutoEngine::new() as Rc<dyn ScriptingEngine>);
        // A language nothing serves.
        let ind = rig
            .chart
            .add_indicator(
                "x",
                AddIndicatorOptions {
                    language: Some("lua".to_string()),
                    ..Default::default()
                },
            )
            .expect("added");
        ready(&rig);
        assert_eq!(rig.renderer.bars.borrow().len(), 50);
        let errors: Vec<(String, String)> = rig
            .log
            .of(ChartEventName::IndicatorError)
            .into_iter()
            .filter_map(|e| match e {
                ChartEvent::IndicatorError { id, error } => Some((id, error)),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, ind.id());
        assert!(errors[0].1.contains("no scripting engine registered"));
        assert!(errors[0].1.contains("registerEngine"));
        assert!(mounts(&rig, ind.id()).is_empty());
        assert!(warned(&rig, "failed"));
    }

    #[test]
    fn strategy_trade_executions_ride_the_model_into_mounts_value_patches_and_inspect() {
        let (rig, engine) = auto_rig(Rig::builder());
        engine.emit_trades.set(true);
        // `visible` makes the script viewport-dependent, so a viewport poke re-runs it.
        let ind = add(
            &rig,
            "//@version=6\nindicator(\"S\", overlay=true)\nx = chart.left_visible_bar_time\nplot(close)",
        );
        ready(&rig);

        let carried = mounts(&rig, ind.id()).iter().any(|m| {
            m.trades.iter().any(|t| {
                t.label.as_deref() == Some("Long")
                    && t.qty == Some(2.0)
                    && t.trade_id.as_deref() == Some("t1")
            })
        });
        assert!(carried);
        assert_eq!(
            rig.chart
                .inspect()
                .indicators
                .iter()
                .find(|s| s.id == ind.id())
                .map(|s| s.trades),
            Some(2)
        );
        assert_eq!(rig.chart.inspect().totals.trades, 2);

        rig.renderer.updated.borrow_mut().clear();
        rig.renderer.fire_viewport(VisibleRange {
            from: 1_700_000_036_000,
            to: 1_700_000_144_000,
        });
        rig.advance(220.0);
        flush(&rig);
        let patch = rig
            .renderer
            .updated
            .borrow()
            .iter()
            .find_map(|(id, p)| match p {
                crate::model::ScenePatch::Value(v) if id == ind.id() => Some(v.clone()),
                _ => None,
            });
        assert_eq!(patch.and_then(|p| p.trades).map(|t| t.len()), Some(2));
    }

    // ── per-bar candle colors ──

    fn bar_delta(
        rig: &Rig,
        id: &str,
    ) -> Option<(usize, Option<Vec<Option<crate::model::CandleBarColor>>>)> {
        rig.renderer
            .updated
            .borrow()
            .iter()
            .rev()
            .find_map(|(i, p)| match p {
                crate::model::ScenePatch::Value(v) if i == id => {
                    v.series.iter().find_map(|d| match &d.values {
                        SeriesValues::Bars { bars, bar_colors } => {
                            Some((bars.len(), bar_colors.clone()))
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
    }

    fn color_of(c: &str) -> Option<crate::model::CandleBarColor> {
        Some(crate::model::CandleBarColor {
            color: Some(c.to_string()),
            ..Default::default()
        })
    }

    #[test]
    fn a_tick_carries_the_colors_its_run_set_together_with_the_bars() {
        let (rig, engine) = auto_rig(Rig::builder().live(true));
        engine.candles.set(true);
        *engine.palette.borrow_mut() = Some(["#0000ff", "#ff8800"]);
        let ind = add(
            &rig,
            "//@version=5\nindicator(\"Trend\", overlay=true)\nplotcandle(open, high, low, close)",
        );
        ready(&rig);
        engine.emit_stream(ind.id()); // first model: mount
        flush(&rig);
        *engine.palette.borrow_mut() = Some(["#00ff00", "#ff0000"]); // the run after mount recolors every bar
        engine.emit_stream(ind.id());
        flush(&rig);

        let (n, colors) = bar_delta(&rig, ind.id()).expect("a bars delta");
        assert!(n > 0);
        let want: Vec<_> = (0..n)
            .map(|i| color_of(if i % 2 == 0 { "#00ff00" } else { "#ff0000" }))
            .collect();
        assert_eq!(colors, Some(want));
    }

    #[test]
    fn a_tick_whose_run_emits_no_colors_states_an_empty_set_so_the_renderer_clears_the_old_one() {
        let (rig, engine) = auto_rig(Rig::builder().live(true));
        engine.candles.set(true);
        *engine.palette.borrow_mut() = Some(["#0000ff", "#ff8800"]);
        let ind = add(
            &rig,
            "//@version=5\nindicator(\"Trend\", overlay=true)\nplotcandle(open, high, low, close)",
        );
        ready(&rig);
        engine.emit_stream(ind.id());
        flush(&rig);
        *engine.palette.borrow_mut() = None;
        engine.emit_stream(ind.id());
        flush(&rig);

        let (n, colors) = bar_delta(&rig, ind.id()).expect("a bars delta");
        assert!(n > 0);
        assert_eq!(colors, Some(Vec::new()));
    }

    // ── loading placeholder + legend status ──

    fn held() -> (Rig, Rc<AutoEngine>) {
        let (rig, engine) = auto_rig(Rig::builder());
        engine.hold.set(true);
        (rig, engine)
    }

    #[test]
    fn a_legend_placeholder_with_a_spinner_mounts_before_the_first_computed_model() {
        let (rig, engine) = held();
        let ind = add(&rig, STUDY);
        ready(&rig);

        let placeholder = mounts(&rig, ind.id())
            .into_iter()
            .next()
            .expect("a placeholder");
        assert!(placeholder.series.is_empty());
        assert_eq!(placeholder.title, "Mock");
        assert_ne!(placeholder.pane_id.as_deref(), Some("price"));
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));
        assert_eq!(rig.log.count(ChartEventName::IndicatorAdded), 0);
        assert!(rig.chart.inspect().indicators.is_empty());

        engine.finish();
        flush(&rig);
        let last = rig
            .renderer
            .mounted_models
            .borrow()
            .last()
            .cloned()
            .expect("mounted");
        assert_eq!(last.id, ind.id());
        assert_eq!(last.series.len(), 1);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            [ind.id()]
        );
        assert_eq!(rig.chart.inspect().indicators.len(), 1);
        assert!(rig.renderer.removed.borrow().is_empty());
    }

    #[test]
    fn the_full_title_shows_while_loading_and_the_shorttitle_arrives_with_the_first_model() {
        let (rig, engine) = held();
        *engine.short_title.borrow_mut() = Some("MK".to_string());
        let ind = add(&rig, STUDY);
        ready(&rig);

        let placeholder = mounts(&rig, ind.id())
            .into_iter()
            .next()
            .expect("a placeholder");
        assert_eq!(placeholder.title, "Mock");
        assert_eq!(placeholder.shorttitle, None);

        engine.finish();
        flush(&rig);
        let last = rig
            .renderer
            .mounted_models
            .borrow()
            .last()
            .cloned()
            .expect("mounted");
        assert_eq!(
            (last.title.as_str(), last.shorttitle.as_deref()),
            ("Mock", Some("MK"))
        );
        // The pane listing carries the compact name beside the full title.
        let entry = rig
            .chart
            .list_panes()
            .into_iter()
            .flat_map(|p| p.indicators)
            .find(|i| i.id == ind.id())
            .expect("listed");
        assert_eq!(
            (entry.title.as_str(), entry.shorttitle.as_deref()),
            ("Mock", Some("MK"))
        );
    }

    #[test]
    fn the_pane_is_rerouted_when_the_computed_overlay_differs_from_the_prepare_time_guess() {
        let (rig, engine) = held();
        engine.emit_overlay.set(Some(true)); // prepare sees no overlay=true, the REAL model is an overlay
        let ind = add(&rig, "//@version=5\nindicator(\"Guess\")\nplot(close)");
        ready(&rig);
        let placeholder_pane = mounts(&rig, ind.id())[0].pane_id.clone().expect("routed");
        assert_ne!(placeholder_pane, "price");

        engine.finish();
        flush(&rig);
        let last = rig
            .renderer
            .mounted_models
            .borrow()
            .last()
            .cloned()
            .expect("mounted");
        assert_eq!(last.pane_id.as_deref(), Some("price"));
        assert!(
            rig.renderer
                .removed_panes
                .borrow()
                .contains(&placeholder_pane)
        );
        assert_eq!(
            rig.chart
                .list_panes()
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            ["price"]
        );
        assert_eq!(
            rig.log.count(ChartEventName::PaneChanged),
            2,
            "the study pane's creation, then the re-route"
        );
    }

    #[test]
    fn an_input_change_shows_the_spinner_during_the_recompute() {
        let (rig, engine) = held();
        let ind = add(&rig, STUDY);
        ready(&rig);
        engine.finish();
        flush(&rig);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));

        ind.set_input("Length", num(50.0));
        flush(&rig);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));
    }

    #[test]
    fn a_failed_compute_stops_the_spinner_and_keeps_the_legend_row() {
        let (rig, engine) = held();
        let ind = add(&rig, STUDY);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let _off = ind.on(
            IndicatorEventName::Error,
            Box::new(move |e| sink.borrow_mut().push(e.clone())),
        );
        ready(&rig);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));

        engine.fail_all("kaboom");
        flush(&rig);
        assert_eq!(
            *seen.borrow(),
            [crate::engine::indicator_handle::IndicatorEvent::Error {
                error: "kaboom".to_string()
            }]
        );
        assert!(
            rig.log.all().iter().any(
                |e| matches!(e, ChartEvent::IndicatorError { error, .. } if error == "kaboom")
            )
        );
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert!(
            rig.renderer.removed.borrow().is_empty(),
            "the row stays; the user can remove it"
        );
        rig.renderer.fire_remove(ind.id());
        flush(&rig);
        assert!(
            rig.renderer
                .removed
                .borrow()
                .contains(&ind.id().to_string())
        );
    }

    // ── a no-output first model never finalizes routing ──

    /// A chart whose initial load is EMPTY (auth race, transient failure), driven by hand.
    fn barless() -> Rig {
        Rig::builder()
            .feed(|f| f.set_series("AAA", Vec::new()))
            .build()
    }

    #[test]
    fn a_no_output_first_model_keeps_the_prepared_pane_and_loading_state_until_a_real_one_lands() {
        let rig = barless();
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        let session = rig.engine.last_session().expect("session");
        assert_eq!(mounts(&rig, ind.id())[0].pane_id.as_deref(), Some("price"));
        assert_eq!(mounts(&rig, ind.id())[0].title, "Mock");

        // The first run resolved over zero bars: fabricated metadata, no output.
        session.push_model(empty_model(ind.id(), "Indicator", false));
        flush(&rig);
        assert_eq!(mounts(&rig, ind.id()).len(), 1);
        assert_eq!(mounts(&rig, ind.id())[0].pane_id.as_deref(), Some("price"));
        assert!(rig.renderer.panes.borrow().iter().all(|p| p.id == "price"));
        assert_eq!(
            rig.chart
                .list_panes()
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            ["price"]
        );
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));
        assert_eq!(rig.log.count(ChartEventName::IndicatorAdded), 0);
        assert!(rig.chart.inspect().indicators.is_empty());
        assert_eq!(
            rig.record(ind.id()).borrow().pending_cause,
            None,
            "no run happened, nothing consumed"
        );

        // The real model mounts on price with its series, announced exactly once.
        session.push_model(mock_model(ind.id(), true, &bars(3), 0.0));
        flush(&rig);
        let last = mounts(&rig, ind.id()).pop().expect("mounted");
        assert_eq!(last.pane_id.as_deref(), Some("price"));
        assert_eq!(last.series.len(), 1);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            [ind.id()]
        );
        assert_eq!(rig.chart.inspect().totals.series, 1);
    }

    #[test]
    fn the_one_shot_self_heal_still_moves_a_real_first_model_that_disagrees_with_the_guess() {
        let rig = Rig::builder().build();
        let ind = add(&rig, OVERLAY); // prepare says overlay: the wrong static guess
        ready(&rig);
        assert_eq!(mounts(&rig, ind.id())[0].pane_id.as_deref(), Some("price"));

        let session = rig.engine.last_session().expect("session");
        session.push_model(mock_model(ind.id(), false, &bars(3), 0.0));
        flush(&rig);
        let last = mounts(&rig, ind.id()).pop().expect("mounted");
        assert_eq!(last.pane_id, Some(format!("pane-{}", ind.id())));
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            [ind.id()]
        );
        let panes = rig.chart.list_panes();
        assert!(
            panes
                .iter()
                .find(|p| p.id == "price")
                .expect("price")
                .indicators
                .is_empty()
        );
        let study = panes
            .iter()
            .find(|p| p.id == format!("pane-{}", ind.id()))
            .expect("study pane");
        assert_eq!(
            study
                .indicators
                .iter()
                .map(|i| i.id.clone())
                .collect::<Vec<_>>(),
            [ind.id()]
        );
    }

    #[test]
    fn an_output_free_run_over_real_bars_still_completes_the_load() {
        let rig = Rig::builder().build();
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));

        // The script RAN (the chart has bars): it just plots nothing. Loading ends with the run.
        let session = rig.engine.last_session().expect("session");
        session.push_model(empty_model(ind.id(), "Mock", true));
        flush(&rig);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            [ind.id()]
        );
        assert_eq!(rig.chart.inspect().indicators.len(), 1);
        assert_eq!(rig.chart.inspect().totals.series, 0);
        assert_eq!(
            mounts(&rig, ind.id())
                .pop()
                .and_then(|m| m.pane_id)
                .as_deref(),
            Some("price")
        );
    }

    #[test]
    fn late_bars_end_to_end_an_empty_initial_load_then_bars() {
        let (rig, engine) = auto_rig(
            Rig::builder()
                .live(true)
                .feed(|f| f.set_series("AAA", Vec::new())),
        );
        engine.empty_when_barless.set(true);
        engine.caps.set(EngineCapabilities {
            streaming: false,
            visible_range: false,
            inputs: true,
            props: false,
        });
        let ind = add(&rig, OVERLAY);
        ready(&rig);

        // The empty first run left the placeholder untouched: price pane, still loading.
        assert_eq!(mounts(&rig, ind.id()).len(), 1);
        assert_eq!(mounts(&rig, ind.id())[0].pane_id.as_deref(), Some("price"));
        assert_eq!(rig.log.count(ChartEventName::IndicatorAdded), 0);
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Loading));

        // Bars arrive; the session re-runs over them.
        for b in bars(3) {
            rig.feed.push_tick(b);
        }
        flush(&rig);
        let last = mounts(&rig, ind.id()).pop().expect("mounted");
        assert_eq!(last.pane_id.as_deref(), Some("price"));
        assert_eq!(last.title, "Mock");
        assert_eq!(last.series.len(), 1);
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            [ind.id()]
        );
        assert_eq!(last_status(&rig, ind.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            rig.chart
                .list_panes()
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            ["price"]
        );
    }

    // ── handle.context and script:run ──

    #[test]
    fn handle_context_resolves_a_snapshot_honors_select_and_fires_context_changed() {
        let (rig, engine) = auto_rig(Rig::builder());
        *engine.context.borrow_mut() = Some(run_context());
        ready(&rig);
        let ind = add(&rig, "plot(close)");
        flush(&rig);
        let snap = rig
            .block_on(ind.context(None))
            .expect("ok")
            .expect("a snapshot");
        assert_eq!(snap.bar_index, 9);
        assert!(snap.strategy.is_some());
        let filtered = rig
            .block_on(ind.context(Some(&[ContextKey::Strategy])))
            .expect("ok")
            .expect("a snapshot");
        assert!(filtered.plots.is_empty(), "select honored");
        assert!(
            rig.log
                .all()
                .iter()
                .any(|e| matches!(e, ChartEvent::ContextChanged { id } if id == ind.id()))
        );
    }

    #[test]
    fn handle_context_resolves_none_on_an_engine_without_the_capability() {
        let (rig, _engine) = auto_rig(Rig::builder());
        ready(&rig);
        let ind = add(&rig, "plot(close)");
        flush(&rig);
        assert_eq!(rig.block_on(ind.context(None)), Ok(None));
        assert!(
            !rig.log
                .all()
                .iter()
                .any(|e| matches!(e, ChartEvent::ContextChanged { .. }))
        );
        // An unknown indicator has none either.
        assert_eq!(
            rig.block_on(rig.chart.get_indicator_context("nope", None)),
            Ok(None)
        );
    }

    fn collect_runs(rig: &Rig) -> Rc<RefCell<Vec<ScriptRun>>> {
        let runs: Rc<RefCell<Vec<ScriptRun>>> = Rc::default();
        let sink = Rc::clone(&runs);
        rig.chart.events().on(ChartEventName::ScriptRun, move |e| {
            if let ChartEvent::ScriptRun(run) = e {
                sink.borrow_mut().push(run.clone());
            }
        });
        runs
    }

    fn run_rig(builder: RigBuilder) -> (Rig, Rc<AutoEngine>) {
        let (rig, engine) = auto_rig(builder);
        *engine.context.borrow_mut() = Some(run_context());
        (rig, engine)
    }

    #[test]
    fn the_first_run_reports_the_declared_title_its_plots_and_cause_history() {
        let (rig, _engine) = run_rig(Rig::builder());
        ready(&rig);
        let runs = collect_runs(&rig);
        add(&rig, "plot(close)");
        flush(&rig);

        let runs = runs.borrow();
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.title, "Mock");
        assert_eq!(run.cause, ScriptRunCause::History);
        assert!(run.first);
        assert_eq!(
            run.kind,
            ScriptKind::Strategy,
            "the engine reported broker state"
        );
        assert!(run.strategy.is_some());
        assert_eq!(run.vars.get("posSize"), Some(&json!(2)));
        assert_eq!(run.vars.get("len"), Some(&json!(14)));
        assert!(matches!(run.plots.get("Mock"), Some(Some(_))));
        assert!(!run.forming, "not a live chart");
        assert!(run.complete);
        assert_eq!(run.bar, 49);
    }

    #[test]
    fn a_run_costs_nothing_when_nobody_listens() {
        // No event listener of any name: not even the rig's log.
        let w = wiring::Wired::new(
            |r| r as Rc<dyn ChartRenderer>,
            |f| f as Rc<dyn MarketDataFeed>,
            None,
            Registries::builtin(),
            |_| {},
            false,
        );
        *w.engine.context.borrow_mut() = Some(run_context());
        w.ready();
        let ind = w.add("plot(close)");
        w.flush();
        assert!(
            w.engine.context_calls.borrow().is_empty(),
            "no context pull without a subscriber"
        );

        w.chart.events().on(ChartEventName::ScriptRun, |_| {});
        ind.set_input("Length", num(21.0));
        w.flush();
        assert!(!w.engine.context_calls.borrow().is_empty());
    }

    #[test]
    fn the_unbounded_parts_stay_off_the_event_and_trades_are_an_explicit_pull() {
        let (rig, engine) = run_rig(Rig::builder());
        ready(&rig);
        let runs = collect_runs(&rig);
        add(&rig, "plot(close)");
        flush(&rig);

        assert!(
            engine
                .context_calls
                .borrow()
                .iter()
                .all(|s| !s.contains("trades"))
        );
        let run = runs.borrow()[0].clone();
        let trades = rig.block_on(run.trades());
        assert_eq!(trades.len(), 2);
        assert!(
            engine
                .context_calls
                .borrow()
                .iter()
                .any(|s| s.contains("trades"))
        );
        let closed = trades.iter().find(|t| t.id == "t0").expect("t0");
        assert_eq!(
            (
                closed.pnl,
                closed.commission,
                closed.max_drawdown,
                closed.max_runup
            ),
            (Some(9.5), Some(0.5), Some(2.5), Some(12.0))
        );
        let open = trades.iter().find(|t| t.id == "t1").expect("t1");
        assert_eq!((open.pnl, open.max_drawdown), (None, None));
        // A plot's full history is an explicit pull too.
        let series = rig.block_on(run.series("a"));
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].value, Some(2.0));
        assert!(rig.block_on(run.series("nope")).is_empty());
    }

    #[test]
    fn an_input_edit_is_attributed_to_inputs_and_a_market_switch_to_market() {
        let (rig, _engine) = run_rig(Rig::builder());
        ready(&rig);
        let runs = collect_runs(&rig);
        let ind = add(&rig, "plot(close)");
        flush(&rig);
        runs.borrow_mut().clear();

        ind.set_input("Length", num(21.0));
        flush(&rig);
        assert!(
            runs.borrow()
                .iter()
                .any(|r| r.cause == ScriptRunCause::Inputs)
        );

        runs.borrow_mut().clear();
        rig.switch(MarketSwitch {
            timeframe: Some("240".to_string()),
            ..MarketSwitch::default()
        })
        .expect("switch");
        flush(&rig);
        assert!(
            runs.borrow()
                .iter()
                .any(|r| r.cause == ScriptRunCause::Market)
        );
    }

    #[test]
    fn a_new_bar_is_bar_and_never_throttled_while_forming_bar_ticks_collapse() {
        let (rig, engine) = run_rig(Rig::builder().live(true));
        ready(&rig);
        let runs = collect_runs(&rig);
        let ind = add(&rig, "plot(close)");
        flush(&rig);
        runs.borrow_mut().clear();

        let last = *rig.raw().last().expect("bars");
        // Three refinements of the SAME bar, back to back: the stream emits one model each.
        for i in 1..=3 {
            rig.feed.push_tick(Ohlcv {
                close: last.close + f64::from(i),
                ..last
            });
            engine.emit_stream(ind.id());
            flush(&rig);
        }
        assert_eq!(
            runs.borrow()
                .iter()
                .filter(|r| r.cause == ScriptRunCause::Tick)
                .count(),
            1,
            "collapsed to ~1/s"
        );

        // A NEW bar opens: the previous one is final, and this must never be dropped.
        rig.feed.push_tick(Ohlcv {
            time: last.time + HOUR,
            ..last
        });
        engine.emit_stream(ind.id());
        flush(&rig);
        let closed: Vec<ScriptRun> = runs
            .borrow()
            .iter()
            .filter(|r| r.cause == ScriptRunCause::Bar)
            .cloned()
            .collect();
        assert_eq!(closed.len(), 1);
        assert!(
            closed[0].forming,
            "the newly opened bar is itself provisional"
        );

        // After the window the ticks flow again.
        rig.advance(1_000.0);
        rig.feed.push_tick(Ohlcv {
            time: last.time + HOUR,
            close: 5.0,
            ..last
        });
        engine.emit_stream(ind.id());
        flush(&rig);
        assert_eq!(
            runs.borrow()
                .iter()
                .filter(|r| r.cause == ScriptRunCause::Tick)
                .count(),
            2
        );
    }

    #[test]
    fn a_run_never_fires_for_a_native_indicator() {
        let (rig, _engine) = run_rig(Rig::builder());
        ready(&rig);
        let runs = collect_runs(&rig);
        let made = made();
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        rig.chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        flush(&rig);
        assert!(runs.borrow().is_empty());
        assert_eq!(made_calls(&made, 0).start, 1);
    }

    #[test]
    fn a_run_removed_before_its_snapshot_arrives_is_never_published() {
        let rig = Rig::builder().build();
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        let runs = collect_runs(&rig);
        let session = rig.engine.last_session().expect("session");
        // The model lands and defers its run build one turn; the host removes the indicator first.
        session.push_model(mock_model(ind.id(), true, &bars(3), 0.0));
        ind.remove();
        flush(&rig);
        assert!(runs.borrow().is_empty());
    }

    // ── host-supplied ids ──

    #[test]
    fn the_supplied_id_is_the_indicator_id_on_every_surface() {
        let (rig, _engine) = auto_rig(Rig::builder());
        ready(&rig);
        let runs = collect_runs(&rig);
        let options = AddIndicatorOptions {
            id: Some("doc:rsi-1".to_string()),
            ..Default::default()
        };
        let handle = rig
            .chart
            .add_indicator("//@version=5\nindicator(\"RSI\")\nplot(close)", options)
            .expect("added");
        assert_eq!(handle.id(), "doc:rsi-1");
        flush(&rig);

        assert_eq!(
            rig.chart
                .list_indicators()
                .iter()
                .map(|h| h.id().to_string())
                .collect::<Vec<_>>(),
            ["doc:rsi-1"]
        );
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorAdded)),
            ["doc:rsi-1"]
        );
        assert_eq!(
            runs.borrow()
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            ["doc:rsi-1"]
        );
        let model = mounts(&rig, "doc:rsi-1")
            .into_iter()
            .find(|m| !m.series.is_empty())
            .expect("computed");
        assert_eq!(model.pane_id.as_deref(), Some("pane-doc:rsi-1"));

        rig.log.clear();
        handle.remove();
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorRemoved)),
            ["doc:rsi-1"]
        );
        assert!(
            rig.renderer
                .removed
                .borrow()
                .contains(&"doc:rsi-1".to_string())
        );
    }

    #[test]
    fn a_minted_id_never_collides_with_a_host_id_already_live() {
        let (rig, _engine) = auto_rig(Rig::builder());
        let host = rig
            .chart
            .add_indicator(
                "plot(close)",
                AddIndicatorOptions {
                    id: Some("ind-2".to_string()),
                    ..Default::default()
                },
            )
            .expect("added");
        let first = add(&rig, "plot(close)");
        let second = add(&rig, "plot(close)");
        assert_eq!(
            (host.id(), first.id(), second.id()),
            ("ind-2", "ind-1", "ind-3")
        );
    }

    #[test]
    fn a_duplicate_or_empty_host_id_is_refused_and_leaves_the_live_indicator_untouched() {
        let (rig, _engine) = auto_rig(Rig::builder());
        ready(&rig);
        let with_id = |id: &str| AddIndicatorOptions {
            id: Some(id.to_string()),
            ..Default::default()
        };
        let first = rig
            .chart
            .add_indicator("plot(close)", with_id("dup"))
            .expect("added");
        flush(&rig);
        let err = rig
            .chart
            .add_indicator("plot(close)", with_id("dup"))
            .err()
            .expect("duplicate");
        assert_eq!(err, IndicatorIdError::Duplicate("dup".to_string()));
        assert!(err.to_string().contains("\"dup\" is already live"));
        assert_eq!(rig.chart.list_indicators().len(), 1);
        assert!(Rc::ptr_eq(&rig.chart.list_indicators()[0], &first));
        // Freed by removal, the id can be claimed again.
        first.remove();
        assert_eq!(
            rig.chart
                .add_indicator("plot(close)", with_id("dup"))
                .expect("again")
                .id(),
            "dup"
        );

        let empty = rig
            .chart
            .add_indicator("plot(close)", with_id(""))
            .err()
            .expect("empty");
        assert_eq!(empty, IndicatorIdError::Empty);
        assert_eq!(rig.chart.list_indicators().len(), 1);
    }

    // ── declaration props ──

    fn inputs_of(pairs: &[(&str, f64)]) -> Inputs {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), num(*v)))
            .collect()
    }

    #[test]
    fn declaration_props_merge_schema_defaults_with_add_time_overrides() {
        let (rig, engine) = auto_rig(Rig::builder());
        engine.props_schema.set(true);
        let options = AddIndicatorOptions {
            props: Some(inputs_of(&[("initial_capital", 25_000.0)])),
            ..Default::default()
        };
        let ind = rig
            .chart
            .add_indicator("indicator(\"P\")", options)
            .expect("added");
        ready(&rig);
        assert_eq!(
            engine.execute_props.borrow()[0],
            Some(inputs_of(&[
                ("initial_capital", 25_000.0),
                ("pyramiding", 0.0)
            ]))
        );
        assert_eq!(
            ind.props()
                .iter()
                .map(|p| p.key.clone())
                .collect::<Vec<_>>(),
            ["initial_capital", "pyramiding"]
        );
        assert_eq!(
            ind.prop_values(),
            inputs_of(&[("initial_capital", 25_000.0), ("pyramiding", 0.0)])
        );
    }

    #[test]
    fn set_props_reruns_the_session_with_merged_prop_overrides() {
        let (rig, engine) = auto_rig(Rig::builder());
        engine.props_schema.set(true);
        let ind = add(&rig, "indicator(\"P\")");
        ready(&rig);

        ind.set_props(inputs_of(&[("pyramiding", 3.0)]));
        assert_eq!(engine.updates.borrow().len(), 1);
        assert_eq!(
            engine.updates.borrow()[0].1,
            Some(inputs_of(&[
                ("initial_capital", 50_000.0),
                ("pyramiding", 3.0)
            ]))
        );
        // Inputs travel alongside: the session re-runs with both bags current.
        assert_eq!(engine.updates.borrow()[0].0, inputs_of(&[("Length", 14.0)]));

        ind.set_prop("initial_capital", num(10_000.0));
        assert_eq!(
            engine.updates.borrow()[1].1,
            Some(inputs_of(&[
                ("initial_capital", 10_000.0),
                ("pyramiding", 3.0)
            ]))
        );
    }

    #[test]
    fn a_renderer_prop_edit_goes_to_the_props_bag_and_an_input_edit_to_inputs() {
        let (rig, engine) = auto_rig(Rig::builder());
        engine.props_schema.set(true);
        let ind = add(&rig, "indicator(\"P\")");
        ready(&rig);
        let edit = |key: &str, value: f64, kind: Option<InputChangeKind>| InputChangeEvent {
            indicator_id: ind.id().to_string(),
            key: key.to_string(),
            value: num(value),
            kind,
        };

        rig.renderer.fire_input_change(edit(
            "initial_capital",
            75_000.0,
            Some(InputChangeKind::Prop),
        ));
        assert_eq!(
            engine.updates.borrow()[0].1,
            Some(inputs_of(&[
                ("initial_capital", 75_000.0),
                ("pyramiding", 0.0)
            ]))
        );

        rig.renderer.fire_input_change(edit("Length", 21.0, None));
        assert_eq!(engine.updates.borrow()[1].0, inputs_of(&[("Length", 21.0)]));
        assert_eq!(
            engine.updates.borrow()[1].1,
            None,
            "prop overrides are not resent with an input edit"
        );
        rig.renderer
            .fire_input_change(edit("pyramiding", 1.0, Some(InputChangeKind::Prop)));
        assert_eq!(
            engine.updates.borrow()[2].1,
            Some(inputs_of(&[
                ("initial_capital", 75_000.0),
                ("pyramiding", 1.0)
            ]))
        );
    }

    // ── force_overlay routing ──

    #[test]
    fn force_overlay_items_are_stamped_with_the_price_pane_while_own_items_keep_the_study_pane() {
        let rig = Rig::builder().build();
        let ind = add(&rig, STUDY);
        ready(&rig);
        let session = rig.engine.last_session().expect("session");
        let bars = bars(3);
        let mut model = mock_model(ind.id(), false, &bars, 0.0);
        let mut forced = line_series("forced", "forced", &bars, 0.0);
        forced.overlay = true;
        model.series.push(forced);
        model.series[0].title = "own".to_string();
        let line = |name: &str, overlay: bool| DrawingLine {
            id: format!("{}:line-drawing:{name}", ind.id()),
            pane_id: "unrouted".to_string(),
            xloc: DrawingXLoc::BarTime,
            x1: bars[0].time as f64,
            y1: 100.0,
            x2: bars[1].time as f64,
            y2: 101.0,
            extend: DrawingExtend::None,
            color: None,
            invisible: false,
            width: 1.0,
            style: LineStyle::Solid,
            arrow_left: false,
            arrow_right: false,
            overlay,
        };
        model.lines = vec![line("own", false), line("forced", true)];
        model.backgrounds.push(Background {
            id: "bg".to_string(),
            pane_id: "unrouted".to_string(),
            from: bars[0].time,
            to: bars[1].time,
            color: "#123456".to_string(),
            overlay: true,
        });
        session.push_model(model);
        flush(&rig);

        let mounted = mounts(&rig, ind.id()).pop().expect("mounted");
        let study = mounted.pane_id.clone().expect("routed");
        assert_ne!(
            study, "price",
            "the indicator itself still routes to its own pane"
        );
        assert_eq!(
            mounted
                .series
                .iter()
                .find(|s| s.title == "own")
                .map(|s| s.pane_id.clone()),
            Some(study.clone())
        );
        assert_eq!(
            mounted
                .series
                .iter()
                .find(|s| s.title == "forced")
                .map(|s| s.pane_id.clone()),
            Some("price".to_string())
        );
        assert_eq!(mounted.backgrounds[0].pane_id, "price");
        assert_eq!(
            mounted
                .lines
                .iter()
                .find(|l| l.id.ends_with(":own"))
                .map(|l| l.pane_id.clone()),
            Some(study)
        );
        assert_eq!(
            mounted
                .lines
                .iter()
                .find(|l| l.id.ends_with(":forced"))
                .map(|l| l.pane_id.clone()),
            Some("price".to_string())
        );
        // forced series + background + one forced line
        assert_eq!(
            rig.chart
                .inspect()
                .indicators
                .iter()
                .find(|s| s.id == ind.id())
                .map(|s| s.forced_overlay),
            Some(3)
        );
    }

    // ── handle.update_code ──

    const OVERLAY_V2: &str = "//@version=5\nindicator(\"A\", overlay=true)\nplot(open)";

    fn update_rig(live: bool) -> (Rig, Rc<AutoEngine>) {
        let (rig, engine) = auto_rig(Rig::builder().live(live));
        ready(&rig);
        (rig, engine)
    }

    fn ok() -> CodeUpdateResult {
        CodeUpdateResult::success()
    }

    fn collect_errors(handle: &IndicatorHandleImpl) -> Rc<RefCell<Vec<String>>> {
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&errors);
        let off = handle.on(
            IndicatorEventName::Error,
            Box::new(move |e| {
                if let crate::engine::indicator_handle::IndicatorEvent::Error { error } = e {
                    sink.borrow_mut().push(error.clone());
                }
            }),
        );
        std::mem::forget(off);
        errors
    }

    #[test]
    fn update_code_keeps_the_id_handle_and_legend_row_and_remounts_idempotently() {
        let (rig, engine) = update_rig(false);
        let runs = collect_runs(&rig);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        let mounts_before = mounts(&rig, ind.id()).len();
        let runs_before = count(&engine.run_count, ind.id());

        let _pending = ind.update_code(OVERLAY_V2);
        flush(&rig);

        assert_eq!(ind.source().as_deref(), Some(OVERLAY_V2));
        assert_eq!(
            count(&engine.run_count, ind.id()),
            runs_before + 1,
            "one fresh execution of the new source"
        );
        assert_eq!(
            mounts(&rig, ind.id()).len(),
            mounts_before + 1,
            "remount, same id"
        );
        assert!(
            rig.renderer.removed.borrow().is_empty(),
            "the row and the handle survive"
        );
        assert_eq!(rig.chart.list_indicators().len(), 1);
        assert_eq!(
            runs.borrow().last().map(|r| r.cause),
            Some(ScriptRunCause::Code)
        );
    }

    #[test]
    fn update_code_stops_the_previous_live_session_and_streams_the_new_script() {
        let (rig, engine) = update_rig(true);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        engine.emit_stream(ind.id());
        flush(&rig);
        assert_eq!(count(&engine.stream_starts, ind.id()), 1);

        let _pending = ind.update_code(OVERLAY_V2);
        flush(&rig);
        assert_eq!(
            count(&engine.stream_stops, ind.id()),
            1,
            "the old stream is stopped"
        );
        assert_eq!(
            count(&engine.stream_starts, ind.id()),
            2,
            "and a new one opened for the new code"
        );
        let mounts_before = mounts(&rig, ind.id()).len();
        engine.emit_stream(ind.id());
        flush(&rig);
        assert_eq!(
            mounts(&rig, ind.id()).len(),
            mounts_before + 1,
            "its first model remounts"
        );
    }

    #[test]
    fn a_source_that_fails_to_compile_leaves_the_running_script_untouched() {
        let (rig, engine) = update_rig(true);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        engine.emit_stream(ind.id());
        flush(&rig);
        let errors = collect_errors(&ind);

        let result = ind.update_code(&format!("{OVERLAY_V2}\n// broken"));
        let result = rig.block_on(result);
        assert_eq!(result, CodeUpdateResult::failure("mock: syntax error"));
        assert_eq!(*errors.borrow(), ["mock: syntax error"]);
        assert_eq!(
            ind.source().as_deref(),
            Some(OVERLAY),
            "still the working code"
        );
        assert_eq!(
            count(&engine.stream_stops, ind.id()),
            0,
            "the working session was never stopped"
        );
        assert_eq!(count(&engine.stream_starts, ind.id()), 1);
        assert_eq!(rig.chart.list_indicators().len(), 1);
    }

    #[test]
    fn update_code_keeps_declared_input_values_drops_the_rest_and_seeds_new_ones() {
        let (rig, _engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        ind.set_input("Length", num(50.0));
        flush(&rig);

        let _p = ind.update_code(&format!("{OVERLAY_V2}\n// inputs: Length,Speed"));
        flush(&rig);
        assert_eq!(
            ind.inputs()
                .iter()
                .map(|i| i.key.clone())
                .collect::<Vec<_>>(),
            ["Length", "Speed"]
        );
        assert_eq!(
            ind.input_values(),
            inputs_of(&[("Length", 50.0), ("Speed", 14.0)])
        );

        let _p = ind.update_code(&format!("{OVERLAY_V2}\n// inputs: Speed"));
        flush(&rig);
        assert_eq!(
            ind.input_values(),
            inputs_of(&[("Speed", 14.0)]),
            "a dropped input leaves no stale value"
        );
    }

    #[test]
    fn update_code_reroutes_the_pane_when_the_new_script_flips_overlay() {
        let (rig, _engine) = update_rig(false);
        let ind = add(&rig, STUDY);
        flush(&rig);
        let study_pane = mounts(&rig, ind.id())
            .pop()
            .and_then(|m| m.pane_id)
            .expect("routed");
        assert_ne!(study_pane, "price");

        let _p = ind.update_code(OVERLAY);
        flush(&rig);
        assert_eq!(
            mounts(&rig, ind.id())
                .pop()
                .and_then(|m| m.pane_id)
                .as_deref(),
            Some("price")
        );
        assert!(
            rig.renderer.removed_panes.borrow().contains(&study_pane),
            "its old pane had nothing else in it"
        );
        assert!(!rig.chart.list_panes().iter().any(|p| p.id == study_pane));

        let _p = ind.update_code(STUDY);
        flush(&rig);
        assert_eq!(
            mounts(&rig, ind.id()).pop().and_then(|m| m.pane_id),
            Some(study_pane),
            "back on its own pane"
        );
    }

    #[test]
    fn while_hidden_update_code_stays_suspended_and_showing_runs_the_new_source() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        ind.set_visible(false);
        let runs_before = count(&engine.run_count, ind.id());

        let _p = ind.update_code(OVERLAY_V2);
        flush(&rig);
        assert_eq!(
            ind.source().as_deref(),
            Some(OVERLAY_V2),
            "prepared and adopted"
        );
        assert_eq!(
            count(&engine.run_count, ind.id()),
            runs_before,
            "but a hidden indicator computes nothing"
        );

        ind.set_visible(true);
        flush(&rig);
        assert_eq!(count(&engine.run_count, ind.id()), runs_before + 1);
    }

    #[test]
    fn update_code_is_a_no_op_for_the_same_source_and_refused_for_a_native_indicator() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        let runs_before = count(&engine.run_count, ind.id());
        assert_eq!(rig.block_on(ind.update_code(OVERLAY)), ok());
        flush(&rig);
        assert_eq!(count(&engine.run_count, ind.id()), runs_before);

        let made = made();
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let native = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        flush(&rig);
        let result = rig.block_on(native.update_code(OVERLAY));
        assert!(!result.ok);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("native indicator has no script")
        );
        assert_eq!(native.source(), None);
        assert!(warned(&rig, "native indicator has no script"));
        // An unknown id has nothing to update.
        let gone = rig.block_on(rig.chart.update_code("nope", "x"));
        assert!(!gone.ok);
    }

    #[test]
    fn two_quick_edits_only_the_latest_source_lands_and_both_callers_settle_together() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        let runs_before = count(&engine.run_count, ind.id());
        let first = ind.update_code(&format!("{OVERLAY_V2}\n// inputs: A"));
        let second = ind.update_code(&format!("{OVERLAY_V2}\n// inputs: B"));
        assert_eq!(rig.block_on(first), ok());
        assert_eq!(rig.block_on(second), ok());
        assert_eq!(ind.source(), Some(format!("{OVERLAY_V2}\n// inputs: B")));
        assert_eq!(
            ind.inputs()
                .iter()
                .map(|i| i.key.clone())
                .collect::<Vec<_>>(),
            ["B"]
        );
        assert_eq!(
            count(&engine.run_count, ind.id()),
            runs_before + 1,
            "the superseded edit never executed"
        );
    }

    #[test]
    fn update_code_resolves_ok_once_the_new_code_ran_and_an_unchanged_source_at_once() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        let runs_before = count(&engine.run_count, ind.id());
        assert_eq!(rig.block_on(ind.update_code(OVERLAY_V2)), ok());
        assert_eq!(count(&engine.run_count, ind.id()), runs_before + 1);
        assert_eq!(rig.block_on(ind.update_code(OVERLAY_V2)), ok());
        assert_eq!(count(&engine.run_count, ind.id()), runs_before + 1);
    }

    #[test]
    fn an_edited_input_survives_and_one_on_its_old_default_takes_the_new_default_computed_once() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, &format!("{OVERLAY}\n// inputs: Length,Speed"));
        flush(&rig);
        ind.set_input("Speed", num(21.0)); // the user's edit
        flush(&rig);
        let runs_before = engine.run_lengths.borrow()[ind.id()].len();

        let result = rig.block_on(ind.update_code(&format!(
            "{OVERLAY_V2}\n// inputs: Length,Speed\n// default: 20"
        )));
        assert!(result.ok);
        assert_eq!(
            ind.input_values(),
            inputs_of(&[("Length", 20.0), ("Speed", 21.0)])
        );
        // Exactly one run of the new code, already with its final values.
        assert_eq!(engine.run_lengths.borrow()[ind.id()][runs_before..], [20.0]);
    }

    #[test]
    fn a_runtime_failure_on_trial_puts_the_previous_code_back_with_its_values() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        ind.set_input("Length", num(21.0));
        flush(&rig);
        let errors = collect_errors(&ind);
        let runs_before = count(&engine.run_count, ind.id());

        let result = rig
            .block_on(ind.update_code(&format!("{OVERLAY_V2}\n// default: 20\n// runtime-broken")));
        flush(&rig);
        assert_eq!(result, CodeUpdateResult::failure("mock: runtime error"));
        assert_eq!(*errors.borrow(), ["mock: runtime error"]);
        assert_eq!(
            ind.source().as_deref(),
            Some(OVERLAY),
            "the working code is back"
        );
        assert_eq!(
            ind.input_values(),
            inputs_of(&[("Length", 21.0)]),
            "with the user's values"
        );
        assert_eq!(
            ind.inputs()
                .iter()
                .map(|i| i.defval.clone())
                .collect::<Vec<_>>(),
            [num(14.0)],
            "and its own schema"
        );
        assert_eq!(
            count(&engine.run_count, ind.id()),
            runs_before + 1,
            "and it runs again"
        );
        assert_eq!(engine.run_lengths.borrow()[ind.id()].last(), Some(&21.0));
        assert_eq!(rig.chart.list_indicators().len(), 1);
        assert!(rig.renderer.removed.borrow().is_empty());
    }

    #[test]
    fn an_asynchronous_runtime_failure_on_a_live_chart_restarts_the_previous_stream() {
        let (rig, engine) = update_rig(true);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        engine.emit_stream(ind.id());
        flush(&rig);

        let pending = ind.update_code(&format!("{OVERLAY_V2}\n// runtime-broken-async"));
        flush(&rig);
        engine.fail_async();
        let result = rig.block_on(pending);
        assert_eq!(result, CodeUpdateResult::failure("mock: runtime error"));
        assert_eq!(ind.source().as_deref(), Some(OVERLAY));
        assert_eq!(
            count(&engine.stream_stops, ind.id()),
            1,
            "the original stream stopped for the trial"
        );
        assert_eq!(
            count(&engine.stream_starts, ind.id()),
            2,
            "and was opened again afterwards"
        );
        engine.emit_stream(ind.id());
        flush(&rig);
        assert_eq!(rig.chart.list_indicators().len(), 1);
    }

    #[test]
    fn a_hidden_indicator_resolves_after_the_compile_check_without_computing() {
        let (rig, engine) = update_rig(false);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        ind.set_visible(false);
        let runs_before = count(&engine.run_count, ind.id());
        assert_eq!(rig.block_on(ind.update_code(OVERLAY_V2)), ok());
        assert_eq!(ind.source().as_deref(), Some(OVERLAY_V2));
        assert_eq!(count(&engine.run_count, ind.id()), runs_before);
        // ...and a hidden update that does not compile still says so.
        let failed = rig.block_on(ind.update_code(&format!("{OVERLAY_V2}\n// broken")));
        assert!(!failed.ok);
        assert_eq!(ind.source().as_deref(), Some(OVERLAY_V2));
    }

    #[test]
    fn hiding_or_removing_mid_trial_settles_the_pending_update() {
        let (rig, engine) = update_rig(true);
        let a = add(&rig, OVERLAY);
        let b = add(&rig, OVERLAY);
        flush(&rig);
        engine.emit_stream(a.id());
        engine.emit_stream(b.id());
        flush(&rig);

        let hidden = a.update_code(OVERLAY_V2); // live: on trial until the stream emits
        let removed = b.update_code(OVERLAY_V2);
        flush(&rig);
        a.set_visible(false);
        b.remove();
        assert_eq!(rig.block_on(hidden), ok());
        let removed = rig.block_on(removed);
        assert!(!removed.ok);
        assert!(
            removed
                .error
                .unwrap_or_default()
                .contains("the indicator was removed")
        );
    }

    #[test]
    fn a_trial_ends_with_the_first_model_of_the_new_code() {
        let (rig, engine) = update_rig(true);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        engine.emit_stream(ind.id());
        flush(&rig);
        let pending = ind.update_code(OVERLAY_V2);
        flush(&rig);
        assert!(
            rig.record(ind.id()).borrow().code_trial.is_some(),
            "on trial until its first model"
        );
        engine.emit_stream(ind.id());
        assert_eq!(rig.block_on(pending), ok());
        assert!(rig.record(ind.id()).borrow().code_trial.is_none());
    }

    #[test]
    fn an_update_on_a_barless_chart_settles_on_the_compile_check() {
        let (rig, engine) = auto_rig(
            Rig::builder()
                .live(true)
                .feed(|f| f.set_series("AAA", Vec::new())),
        );
        ready(&rig);
        let ind = add(&rig, OVERLAY);
        flush(&rig);
        let result = rig.block_on(ind.update_code(OVERLAY_V2));
        assert_eq!(
            result,
            ok(),
            "no bars, no run: the compile check stands in for the first model"
        );
        assert_eq!(count(&engine.stream_starts, ind.id()), 2);
    }

    // ── inputs on a record ──

    #[test]
    fn inputs_merge_into_the_stored_values_and_a_native_is_told_too() {
        let made = made();
        let (rig, _engine) = auto_rig(Rig::builder());
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let h = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        ready(&rig);
        assert_eq!(h.input_values(), inputs_of(&[("len", 5.0)]));
        h.set_input("len", num(10.0));
        assert_eq!(made_calls(&made, 0).set_inputs, 1);
        assert_eq!(h.input_values(), inputs_of(&[("len", 10.0)]));
        assert_eq!(rig.log.count(ChartEventName::IndicatorInputs), 1);
        // Unknown ids, and indicators without a session or instance, are ignored.
        rig.chart.apply_inputs("nope", inputs_of(&[("len", 1.0)]));
        rig.chart.apply_props("nope", inputs_of(&[("len", 1.0)]));
        assert_eq!(rig.log.count(ChartEventName::IndicatorInputs), 1);
        // A hidden native only records: the instance is not poked.
        h.set_visible(false);
        h.set_input("len", num(11.0));
        assert_eq!(made_calls(&made, 0).set_inputs, 1);
        assert_eq!(h.input_values(), inputs_of(&[("len", 11.0)]));
    }

    // ── native indicators ──

    #[test]
    fn a_native_mounts_through_the_shared_pipeline_and_is_single_instance_per_type() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let h1 = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        ready(&rig);

        assert!(h1.visible());
        assert_eq!(made_calls(&made, 0).start, 1);
        let mounted = mounts(&rig, h1.id())
            .into_iter()
            .find(|m| m.native.is_some())
            .expect("mounted");
        assert_eq!(
            mounted.native.as_ref().map(|n| n.indicator_type.as_str()),
            Some("test-native")
        );
        assert_eq!(mounted.series.len(), 1);
        assert_eq!(
            h1.inputs()
                .iter()
                .map(|i| i.key.clone())
                .collect::<Vec<_>>(),
            ["len"]
        );

        // A second add returns the SAME handle, no re-create.
        let h2 = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        assert!(Rc::ptr_eq(&h1, &h2));
        assert_eq!(made_len(&made), 1);

        let summary = rig
            .chart
            .inspect()
            .indicators
            .into_iter()
            .find(|s| s.id == h1.id())
            .expect("inspected");
        assert!(summary.native);
        assert_eq!(summary.native_type.as_deref(), Some("test-native"));
        assert_eq!(h1.native_type(), Some("test-native"));
        assert_eq!(h1.source(), None);
        assert_eq!(h1.language(), None, "no engine runs a native");
    }

    #[test]
    fn a_multi_instance_type_creates_a_fresh_instance_per_add_each_removable_on_its_own() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("test-multi", Kind::Line, true, &made));
        let h1 = rig
            .chart
            .add_native_indicator("test-multi", NativeIndicatorOptions::default());
        let h2 = rig.chart.add_native_indicator(
            "test-multi",
            NativeIndicatorOptions {
                inputs: Some(inputs_of(&[("len", 20.0)])),
            },
        );
        ready(&rig);

        assert!(!Rc::ptr_eq(&h1, &h2));
        assert_ne!(h1.id(), h2.id());
        assert_eq!(made_len(&made), 2);
        assert!((0..2).all(|n| made_calls(&made, n).start == 1));
        let mut ids: Vec<String> = rig
            .renderer
            .mounted_models
            .borrow()
            .iter()
            .filter(|m| {
                m.native
                    .as_ref()
                    .is_some_and(|n| n.indicator_type == "test-multi")
            })
            .map(|m| m.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        let mut want = vec![h1.id().to_string(), h2.id().to_string()];
        want.sort();
        assert_eq!(ids, want);
        assert_eq!(h1.input_values(), inputs_of(&[("len", 5.0)]));
        assert_eq!(h2.input_values(), inputs_of(&[("len", 20.0)]));
        assert_eq!(
            rig.chart.present_native_indicators(),
            ["test-multi", "test-multi"]
        );
        let info = rig
            .block_on(rig.chart.available_native_indicators())
            .into_iter()
            .find(|i| i.indicator_type == "test-multi")
            .expect("catalogued");
        assert!(info.present && info.multi_instance);

        h1.remove();
        assert_eq!(made_calls(&made, 0).stop, 1);
        assert_eq!(made_calls(&made, 1).stop, 0);
        assert_eq!(rig.chart.present_native_indicators(), ["test-multi"]);
        assert_eq!(
            rig.chart
                .list_indicators()
                .iter()
                .map(|h| h.id().to_string())
                .collect::<Vec<_>>(),
            [h2.id()]
        );
    }

    #[test]
    fn a_removed_native_keeps_its_own_teardown_push_but_its_context_goes_dead() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("late-native", Kind::Late, false, &made));
        let h = rig
            .chart
            .add_native_indicator("late-native", NativeIndicatorOptions::default());
        ready(&rig);
        assert!(
            rig.renderer
                .pushes_to("late-native")
                .contains(&json!("live"))
        );
        let record = rig.record(h.id());

        h.remove();
        // stop() still clears its layer: the push inside stop applies.
        assert_eq!(
            rig.renderer.pushes_to("late-native").last(),
            Some(&Value::Null)
        );
        let pushes = rig.renderer.native_pushes.borrow().len();
        // Work the instance does after the removal (it pushes on every bars change) goes nowhere.
        rig.chart.native_on_bars(&record);
        assert_eq!(rig.renderer.native_pushes.borrow().len(), pushes);
        assert_eq!(made_calls(&made, 0).stop, 1);
    }

    #[test]
    fn a_native_hidden_before_it_started_is_neither_suspended_nor_stopped_until_first_shown() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("strict-native", Kind::Probe, true, &made));
        // The restored-ledger shape: add, then hide immediately, before ready/start.
        let h = rig
            .chart
            .add_native_indicator("strict-native", NativeIndicatorOptions::default());
        h.set_visible(false);
        ready(&rig);
        assert_eq!(
            made_calls(&made, 0),
            Calls::default(),
            "never started, never suspended"
        );
        assert!(
            !mounts(&rig, h.id()).is_empty(),
            "the hidden row still mounts"
        );
        assert_eq!(
            rig.log.count(ChartEventName::IndicatorAdded),
            1,
            "announced at once"
        );

        h.set_visible(true);
        flush(&rig);
        assert_eq!(made_calls(&made, 0).start, 1, "first show STARTS it");
        h.set_visible(false);
        assert_eq!(
            made_calls(&made, 0).suspend,
            1,
            "a running instance is suspended as usual"
        );

        // A second one, removed while still never started: no stop() either.
        let h2 = rig
            .chart
            .add_native_indicator("strict-native", NativeIndicatorOptions::default());
        h2.set_visible(false);
        h2.remove();
        assert_eq!(made_calls(&made, 1), Calls::default());

        // And destroy() only stops what ran.
        let h3 = rig
            .chart
            .add_native_indicator("strict-native", NativeIndicatorOptions::default());
        h3.set_visible(false);
        rig.chart.destroy();
        assert_eq!(made_calls(&made, 0).stop, 1);
        assert_eq!(made_calls(&made, 2).stop, 0);
    }

    #[test]
    fn each_native_context_knows_its_own_id_and_a_multi_instance_pushes_on_its_own_channel() {
        let (singles, multis) = (made(), made());
        let rig = Rig::builder().build();
        register(
            &rig,
            descriptor("layer-single", Kind::Layer, false, &singles),
        );
        register(&rig, descriptor("layer-multi", Kind::Layer, true, &multis));
        let s = rig
            .chart
            .add_native_indicator("layer-single", NativeIndicatorOptions::default());
        let m1 = rig
            .chart
            .add_native_indicator("layer-multi", NativeIndicatorOptions::default());
        let m2 = rig
            .chart
            .add_native_indicator("layer-multi", NativeIndicatorOptions::default());
        ready(&rig);

        assert_eq!(made_calls(&singles, 0).ctx_id.as_deref(), Some(s.id()));
        assert_eq!(
            [made_calls(&multis, 0).ctx_id, made_calls(&multis, 1).ctx_id],
            [Some(m1.id().to_string()), Some(m2.id().to_string())]
        );
        // Single-instance: the type is the channel, the model names no channel of its own.
        assert_eq!(
            rig.renderer.pushes_to("layer-single"),
            [json!({ "from": s.id() })]
        );
        let tag = mounts(&rig, s.id())
            .into_iter()
            .find_map(|m| m.native)
            .expect("native model");
        assert_eq!(
            (tag.indicator_type.as_str(), tag.channel),
            ("layer-single", None)
        );
        // Multi-instance: one channel per instance (never the bare type), carried by the model.
        for h in [&m1, &m2] {
            let channel = mounts(&rig, h.id())
                .into_iter()
                .find_map(|m| m.native)
                .and_then(|t| t.channel);
            assert_eq!(
                channel,
                Some(crate::indicators::native_instance_channel(
                    "layer-multi",
                    h.id()
                ))
            );
            assert_eq!(
                rig.renderer.pushes_to(&channel.expect("channel")),
                [json!({ "from": h.id() })]
            );
        }
        assert!(rig.renderer.pushes_to("layer-multi").is_empty());
    }

    #[test]
    fn an_outputs_pane_axis_override_is_stamped_onto_the_mounted_model_and_only_then() {
        let (a, b) = (made(), made());
        let rig = Rig::builder().build();
        register(&rig, descriptor("test-native", Kind::Line, false, &a));
        register(&rig, descriptor("test-unscaled", Kind::Unscaled, false, &b));
        let plain = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        let unscaled = rig
            .chart
            .add_native_indicator("test-unscaled", NativeIndicatorOptions::default());
        ready(&rig);

        let um = mounts(&rig, unscaled.id())
            .into_iter()
            .find(|m| m.native.is_some())
            .expect("mounted");
        assert_eq!(
            um.pane_axis,
            Some(crate::model::PaneAxis::Bands(vec![
                crate::model::PaneAxisBand {
                    frac: 0.25,
                    label: "Top".to_string()
                },
                crate::model::PaneAxisBand {
                    frac: 0.75,
                    label: "Bottom".to_string()
                },
            ]))
        );
        assert!(um.series.is_empty());
        let pm = mounts(&rig, plain.id())
            .into_iter()
            .find(|m| m.native.is_some())
            .expect("mounted");
        assert_eq!(pm.pane_axis, None);
    }

    #[test]
    fn natives_sort_to_the_top_of_inspect_even_when_added_last() {
        let made = made();
        let (rig, _engine) = auto_rig(Rig::builder());
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let pine = add(&rig, OVERLAY);
        let nat = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        ready(&rig);
        let ids: Vec<String> = rig
            .chart
            .inspect()
            .indicators
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids[0], nat.id());
        assert!(ids.iter().position(|i| i == nat.id()) < ids.iter().position(|i| i == pine.id()));
    }

    #[test]
    fn the_catalog_reports_supported_present_and_beta_per_type() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let mut beta = descriptor("test-beta", Kind::Line, false, &made);
        beta.beta = true;
        rig.registries
            .native_indicators
            .borrow_mut()
            .register_supported(beta, Rc::new(|_symbol, _data| Box::pin(async { Ok(true) })));
        let mut refused = descriptor("test-refused", Kind::Line, false, &made);
        refused.beta = false;
        rig.registries
            .native_indicators
            .borrow_mut()
            .register_supported(
                refused,
                Rc::new(|_symbol, _data| {
                    Box::pin(async { Err(DataError::other("no capability")) })
                }),
            );
        ready(&rig);

        let before = rig.block_on(rig.chart.available_native_indicators());
        let find = |list: &[NativeIndicatorInfo], t: &str| {
            list.iter()
                .find(|i| i.indicator_type == t)
                .cloned()
                .expect("listed")
        };
        let plain = find(&before, "test-native");
        assert_eq!(
            (
                plain.title.as_str(),
                plain.supported,
                plain.present,
                plain.beta
            ),
            ("Native test-native", true, false, false)
        );
        assert!(find(&before, "test-beta").supported && find(&before, "test-beta").beta);
        assert!(
            !find(&before, "test-refused").supported,
            "a failing check reads as unsupported"
        );

        rig.chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        flush(&rig);
        let after = rig.block_on(rig.chart.available_native_indicators());
        assert!(find(&after, "test-native").present);
        assert!(!find(&after, "test-beta").present);
    }

    #[test]
    fn a_native_follows_its_lifecycle_through_settings_hide_show_and_removal() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        let h = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        ready(&rig);
        let mounts_before = mounts(&rig, h.id()).len();

        h.set_input("len", num(10.0));
        assert_eq!(
            made_calls(&made, 0).set_inputs,
            1,
            "a settings edit: the instance recomputes"
        );

        h.set_visible(false);
        assert!(!h.visible());
        assert_eq!(made_calls(&made, 0).suspend, 1);
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(h.id()),
            Some(&false)
        );
        assert!(rig.renderer.removed.borrow().is_empty());

        h.set_visible(true);
        flush(&rig);
        assert!(h.visible());
        assert_eq!(made_calls(&made, 0).resume, 1);
        assert!(
            mounts(&rig, h.id()).len() > mounts_before,
            "show resumes, re-emits and re-mounts"
        );

        rig.log.clear();
        h.remove();
        assert_eq!(made_calls(&made, 0).stop, 1);
        assert_eq!(
            event_ids(rig.log.of(ChartEventName::IndicatorRemoved)),
            [h.id()]
        );
    }

    #[test]
    fn an_unregistered_native_type_returns_a_fail_soft_handle() {
        let rig = Rig::builder().build();
        let h = rig
            .chart
            .add_native_indicator("does-not-exist", NativeIndicatorOptions::default());
        ready(&rig);
        assert!(mounts(&rig, h.id()).is_empty());
        assert!(warned(&rig, "no native indicator registered"));
        assert_eq!(h.native_type(), Some("does-not-exist"));
        // The handle owns its id even though no record exists.
        assert_eq!(
            rig.chart.claim_indicator_id(Some(h.id())),
            Err(IndicatorIdError::Duplicate(h.id().to_string()))
        );
        assert!(rig.chart.present_native_indicators().is_empty());
    }

    #[test]
    fn a_legend_false_native_paints_its_output_with_no_row_and_no_pane_listing() {
        let made = made();
        let rig = Rig::builder().build();
        let mut legendless = descriptor("test-native-legendless", Kind::Line, false, &made);
        legendless.legend = Some(false);
        register(&rig, legendless);
        register(&rig, descriptor("test-native", Kind::Line, false, &made));
        ready(&rig);

        let h = rig
            .chart
            .add_native_indicator("test-native-legendless", NativeIndicatorOptions::default());
        flush(&rig);
        let model = mounts(&rig, h.id()).into_iter().next().expect("mounted");
        assert_eq!(model.legend, Some(false));
        assert_eq!(model.series.len(), 1, "the output still reaches the scene");
        let listed = |id: &str| {
            rig.chart
                .list_panes()
                .iter()
                .flat_map(|p| p.indicators.iter().map(|i| i.id.clone()))
                .any(|i| i == id)
        };
        assert!(!listed(h.id()));

        // The host still controls it through its handle.
        h.set_visible(false);
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(h.id()),
            Some(&false)
        );
        h.remove();
        assert!(rig.renderer.removed.borrow().contains(&h.id().to_string()));
        assert!(
            !rig.chart
                .inspect()
                .indicators
                .iter()
                .any(|s| s.id == h.id())
        );

        // An ordinary native keeps its row and its pane listing.
        let plain = rig
            .chart
            .add_native_indicator("test-native", NativeIndicatorOptions::default());
        flush(&rig);
        assert_eq!(mounts(&rig, plain.id())[0].legend, None);
        assert!(listed(plain.id()));
    }

    #[test]
    fn natives_present_synchronously_after_an_add_and_a_remove() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        ready(&rig);
        assert!(rig.chart.present_native_indicators().is_empty());
        let h = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        // Synchronous: an unload-time persist flush must see this immediately.
        assert_eq!(rig.chart.present_native_indicators(), ["probe"]);
        h.remove();
        assert!(rig.chart.present_native_indicators().is_empty());
    }

    // ── built-in volume ──

    fn volume_rig(volume: bool) -> Rig {
        let rig = Rig::builder().volume(volume).build();
        ready(&rig);
        rig
    }

    fn volume_model(rig: &Rig) -> Option<IndicatorModel> {
        rig.renderer
            .mounted_models
            .borrow()
            .iter()
            .find(|m| {
                m.native
                    .as_ref()
                    .is_some_and(|n| n.indicator_type == "volume")
            })
            .cloned()
    }

    #[test]
    fn the_volume_indicator_is_auto_added_and_pushes_its_layer_config() {
        let rig = volume_rig(true);
        let model = volume_model(&rig).expect("the legend row mounted");
        assert!(model.series.is_empty(), "the layer draws outside the model");
        assert_eq!(model.pane_id.as_deref(), Some("price"));
        let pushes = rig.renderer.pushes_to("volume");
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0]["heightFrac"], json!(0.2));
        assert!(pushes[0]["upColor"].is_string() && pushes[0]["downColor"].is_string());
        let summary = rig
            .chart
            .inspect()
            .indicators
            .into_iter()
            .find(|s| s.native_type.as_deref() == Some("volume"))
            .expect("inspected");
        assert!(summary.native);
        assert_eq!(
            summary.inputs, 3,
            "colors + height% drive the settings dialog"
        );
    }

    #[test]
    fn volume_false_opts_out_and_a_manual_add_stays_single_instance() {
        let rig = volume_rig(false);
        assert!(rig.renderer.pushes_to("volume").is_empty());
        assert!(
            !rig.chart
                .inspect()
                .indicators
                .iter()
                .any(|s| s.native_type.as_deref() == Some("volume"))
        );
        let h1 = rig
            .chart
            .add_native_indicator("volume", NativeIndicatorOptions::default());
        flush(&rig);
        assert_eq!(rig.renderer.pushes_to("volume").len(), 1);
        let h2 = rig
            .chart
            .add_native_indicator("volume", NativeIndicatorOptions::default());
        assert_eq!(h1.id(), h2.id());
        let rows = rig
            .renderer
            .mounted_models
            .borrow()
            .iter()
            .filter(|m| {
                m.native
                    .as_ref()
                    .is_some_and(|n| n.indicator_type == "volume")
            })
            .count();
        assert_eq!(rows, 1);
    }

    #[test]
    fn a_volume_input_change_repushes_the_resolved_config() {
        let rig = volume_rig(true);
        let handle = rig
            .chart
            .add_native_indicator("volume", NativeIndicatorOptions::default());
        let mut values = Inputs::new();
        values.insert(
            "upColor".to_string(),
            InputValue::Text("#112233".to_string()),
        );
        values.insert("heightPct".to_string(), num(35.0));
        handle.set_inputs(values);
        flush(&rig);
        let last = rig.renderer.pushes_to("volume").pop().expect("a push");
        assert_eq!(last["upColor"], json!("#112233"));
        assert_eq!(last["heightFrac"], json!(0.35));
    }

    #[test]
    fn the_vpvr_is_not_auto_added_and_adding_it_mounts_a_row_and_pushes_its_config() {
        let rig = volume_rig(true);
        assert!(rig.renderer.pushes_to("vpvr").is_empty());
        rig.chart
            .add_native_indicator("vpvr", NativeIndicatorOptions::default());
        flush(&rig);
        let model = rig
            .renderer
            .mounted_models
            .borrow()
            .iter()
            .find(|m| {
                m.native
                    .as_ref()
                    .is_some_and(|n| n.indicator_type == "vpvr")
            })
            .cloned()
            .expect("mounted");
        assert_eq!(model.shorttitle.as_deref(), Some("VRVP"));
        assert!(model.series.is_empty());
        assert_eq!(model.pane_id.as_deref(), Some("price"));
        let pushes = rig.renderer.pushes_to("vpvr");
        assert_eq!(pushes.len(), 1);
        assert_eq!(
            (
                &pushes[0]["rows"],
                &pushes[0]["widthFrac"],
                &pushes[0]["showPoc"]
            ),
            (&json!(24), &json!(0.3), &json!(true))
        );
    }

    #[test]
    fn hiding_volume_suspends_via_the_renderer_flag_and_showing_repushes_the_config() {
        let rig = volume_rig(true);
        let id = rig.chart.list_indicators()[0].id().to_string();
        let before = rig.renderer.pushes_to("volume").len();
        rig.renderer.fire_toggle_visible(&id, false);
        flush(&rig);
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(&id),
            Some(&false)
        );
        rig.renderer.fire_toggle_visible(&id, true);
        flush(&rig);
        assert_eq!(
            rig.renderer.indicator_visible.borrow().get(&id),
            Some(&true)
        );
        assert!(
            rig.renderer.pushes_to("volume").len() > before,
            "resume re-pushed the config"
        );
    }

    #[test]
    fn removing_volume_tears_down_its_row_and_the_opt_out_sticks_across_a_market_switch() {
        let rig = volume_rig(true);
        let id = rig.chart.list_indicators()[0].id().to_string();
        rig.renderer.fire_remove(&id);
        flush(&rig);
        assert!(rig.renderer.removed.borrow().contains(&id));
        assert!(rig.chart.list_indicators().is_empty());

        rig.switch(to_symbol("BBB")).expect("switch");
        flush(&rig);
        // The auto-add fires on every load's first paint: the user's removal wins.
        assert!(rig.chart.list_indicators().is_empty());
    }

    // ── native restart across a market switch ──

    fn price_rig() -> Rig {
        Rig::builder()
            .feed(|f| {
                for (s, p) in [("AAA", 100.0), ("BBB", 200.0), ("SLOW", 900.0)] {
                    f.set_price(s, p);
                }
            })
            .build()
    }

    #[test]
    fn a_native_restarts_with_a_fresh_context_over_the_new_market_keeping_its_record_id() {
        let made = made();
        let rig = price_rig();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        let h = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        ready(&rig);
        assert_eq!(made_len(&made), 1);
        assert_eq!(made_calls(&made, 0).symbol.as_deref(), Some("AAA"));

        rig.switch(to_symbol("BBB")).expect("switch");
        flush(&rig);
        assert_eq!(made_calls(&made, 0).stop, 1, "the old instance is released");
        assert_eq!(made_len(&made), 2, "a fresh instance from the descriptor");
        assert_eq!(
            made_calls(&made, 1).symbol.as_deref(),
            Some("BBB"),
            "started over the NEW market"
        );
        assert!(
            rig.chart.list_indicators().iter().any(|x| x.id() == h.id()),
            "same record and legend row"
        );
    }

    #[test]
    fn a_hidden_native_goes_stale_and_is_started_fresh_when_shown_never_resumed() {
        let made = made();
        let rig = price_rig();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        let h = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        ready(&rig);
        h.set_visible(false);
        assert_eq!(made_calls(&made, 0).suspend, 1);

        rig.switch(to_symbol("BBB")).expect("switch");
        flush(&rig);
        assert_eq!(made_len(&made), 2);
        assert_eq!(made_calls(&made, 1).start, 0, "hidden: not started yet");

        h.set_visible(true);
        flush(&rig);
        let fresh = made_calls(&made, 1);
        assert_eq!(
            (fresh.start, fresh.symbol.as_deref(), fresh.resume),
            (1, Some("BBB"), 0)
        );
    }

    #[test]
    fn an_identity_switch_stops_natives_blanks_their_channels_and_the_old_context_goes_dead() {
        let made = made();
        let rig = price_rig();
        rig.feed.gate_symbol("SLOW");
        register(&rig, descriptor("late-native", Kind::Late, false, &made));
        let h = rig
            .chart
            .add_native_indicator("late-native", NativeIndicatorOptions::default());
        ready(&rig);
        let record = rig.record(h.id());
        assert!(
            rig.renderer
                .pushes_to("late-native")
                .contains(&json!("live"))
        );

        let done = rig.chart.set_market(to_symbol("SLOW"));
        // Synchronously at the switch: the old instance is released and its channel blanked.
        assert_eq!(made_calls(&made, 0).stop, 1);
        assert_eq!(
            rig.renderer.pushes_to("late-native").last(),
            Some(&Value::Null)
        );
        let pushes = rig.renderer.native_pushes.borrow().len();
        // A change the stopped instance still reacts to goes nowhere.
        rig.chart.native_on_bars(&record);
        assert_eq!(rig.renderer.native_pushes.borrow().len(), pushes);

        rig.feed.release();
        rig.block_on(done).expect("switch");
        flush(&rig);
        assert_eq!(made_len(&made), 2);
        assert_eq!(
            made_calls(&made, 0).stop,
            1,
            "released once: the post-load restart does not stop it again"
        );
        assert_eq!(made_calls(&made, 1).symbol.as_deref(), Some("SLOW"));
        assert!(
            rig.renderer
                .pushes_to("late-native")
                .contains(&json!("live"))
        );
    }

    // ── re-entrancy and lifecycle ──

    #[test]
    fn a_listener_removing_a_native_from_indicator_added_does_not_deadlock_the_instance() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        let chart = rig.chart.clone();
        // The instance's own start() emits; its first emit announces; the host reacts by removing it.
        rig.chart
            .events()
            .on(ChartEventName::IndicatorAdded, move |e| {
                if let ChartEvent::IndicatorAdded { id } = e {
                    chart.remove_indicator(id);
                }
            });
        let h = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        ready(&rig);
        assert!(rig.chart.list_indicators().is_empty());
        assert_eq!(made_calls(&made, 0).start, 1);
        // The instance was stopped while its start was still being applied: never marked started.
        assert_eq!(made_calls(&made, 0).stop, 0);
        assert!(rig.renderer.removed.borrow().contains(&h.id().to_string()));
    }

    #[test]
    fn a_listener_hiding_a_native_from_indicator_added_may_suspend_it_after_its_start() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        let chart = rig.chart.clone();
        rig.chart
            .events()
            .on(ChartEventName::IndicatorAdded, move |e| {
                if let ChartEvent::IndicatorAdded { id } = e {
                    chart.set_visible(id, false);
                }
            });
        let h = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        ready(&rig);
        assert!(!h.visible());
        assert_eq!(made_calls(&made, 0).start, 1);
        // `started` is set after the start call returns (as in the original), so the hide
        // that ran inside it had nothing to suspend.
        assert_eq!(made_calls(&made, 0).suspend, 0);
    }

    #[test]
    fn a_script_started_after_destroy_does_nothing() {
        let (rig, engine) = auto_rig(Rig::builder());
        let ind = add(&rig, OVERLAY);
        rig.chart.destroy();
        flush(&rig);
        assert_eq!(count(&engine.run_count, ind.id()), 0);
        assert!(rig.renderer.mounted_models.borrow().is_empty());
    }

    #[test]
    fn a_native_started_after_destroy_does_nothing() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        rig.chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        rig.chart.destroy();
        flush(&rig);
        assert_eq!(made_calls(&made, 0).start, 0);
    }

    #[test]
    fn an_indicator_removed_during_the_paint_yield_is_never_prepared() {
        let (rig, engine) = auto_rig(Rig::builder());
        let ind = add(&rig, OVERLAY);
        rig.ready().expect("ready");
        rig.settle(); // past the ready wait, parked on the paint yield
        ind.remove();
        flush(&rig);
        assert_eq!(count(&engine.run_count, ind.id()), 0);
        assert!(mounts(&rig, ind.id()).is_empty());
    }

    #[test]
    fn an_engine_alert_reaches_the_bus_and_the_handle_unless_the_chart_is_replaying() {
        let (rig, _engine) = (Rig::builder().build(), ());
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        let session = rig.engine.last_session().expect("session");
        let heard = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&heard);
        let _off = ind.on(
            IndicatorEventName::Alert,
            Box::new(move |e| sink.borrow_mut().push(e.clone())),
        );

        session.push_alert(alert("a1"));
        let alerts: Vec<ChartEvent> = rig.log.of(ChartEventName::Alert);
        assert_eq!(alerts.len(), 1);
        // The chart-level event names its source: the declared title.
        assert!(
            matches!(&alerts[0], ChartEvent::Alert(a) if a.indicator.as_deref() == Some("Mock") && a.alert.id == "a1")
        );
        assert_eq!(heard.borrow().len(), 1);

        rig.chart.inner.state.borrow_mut().replay_queue = Some(Default::default());
        session.push_alert(alert("a2"));
        assert_eq!(
            rig.log.count(ChartEventName::Alert),
            1,
            "history must not pass for a live signal"
        );
        assert_eq!(heard.borrow().len(), 1);
        session.push_warning(crate::ports::scripting_engine::EngineWarning {
            message: "w".to_string(),
            method: None,
            bar: 0,
        });
        assert_eq!(rig.log.count(ChartEventName::Warning), 1);
    }

    #[test]
    fn a_model_for_a_hidden_or_removed_indicator_is_dropped() {
        let rig = Rig::builder().build();
        let ind = add(&rig, OVERLAY);
        ready(&rig);
        let session = rig.engine.last_session().expect("session");
        let model = mock_model(ind.id(), true, &bars(3), 0.0);
        assert!(rig.chart.apply_model(ind.id(), model.clone()));
        let mounted = mounts(&rig, ind.id()).len();
        ind.set_visible(false);
        assert!(
            rig.chart.apply_model(ind.id(), model.clone()),
            "hidden: dropped, still reported as handled"
        );
        assert_eq!(mounts(&rig, ind.id()).len(), mounted);
        ind.remove();
        assert!(rig.chart.apply_model("nope", model));
        session.push_model(mock_model(ind.id(), true, &bars(3), 0.0));
    }

    // ── helpers ──

    #[test]
    fn values_on_a_new_schema_follow_the_declaration_unless_the_user_edited_them() {
        let schema = |pairs: &[(&str, f64)]| -> Vec<InputSchema> {
            pairs
                .iter()
                .map(|(k, d)| {
                    InputSchema::new(*k, format!("T-{k}"), crate::model::InputType::Int, *d)
                })
                .collect()
        };
        let old = schema(&[("a", 1.0), ("b", 2.0)]);
        // `a` is edited, `b` still sits on its old default, `gone` is no longer declared.
        let previous = inputs_of(&[("a", 9.0), ("b", 2.0), ("gone", 5.0)]);
        let new = schema(&[("a", 10.0), ("b", 20.0), ("c", 30.0)]);
        assert_eq!(
            values_on_schema(&new, &previous, &old),
            inputs_of(&[("a", 9.0), ("b", 20.0), ("c", 30.0)])
        );
        // Title-keyed add-time overrides survive as well.
        let by_title = inputs_of(&[("T-a", 4.0)]);
        assert_eq!(
            values_on_schema(&new, &by_title, &[]).get("T-a"),
            Some(&num(4.0))
        );
        assert!(same_input_value(&num(1.0), &num(1.0)));
        assert!(!same_input_value(
            &num(1.0),
            &InputValue::Text("1".to_string())
        ));
    }

    #[test]
    fn a_value_patch_states_the_anchor_and_the_bars_and_colors_always() {
        let bars = bars(3);
        let mut model = mock_model("x", true, &bars, 0.0);
        model.anchor_time = Some(bars[1].time);
        let patch = model_to_value_patch(&model);
        assert_eq!(patch.anchor_time, AnchorUpdate::Set(bars[1].time));
        assert_eq!(
            patch.dirty,
            DirtyRange {
                from: bars[0].time,
                to: bars[2].time
            }
        );
        assert_eq!(patch.series.len(), 1);
        assert_eq!(patch.trades, Some(Vec::new()));
        model.anchor_time = None;
        assert_eq!(
            model_to_value_patch(&model).anchor_time,
            AnchorUpdate::Clear
        );
        assert_eq!(
            model_to_value_patch(&empty_model("y", "Y", true)).dirty,
            DirtyRange { from: 0, to: 0 }
        );
    }

    #[test]
    fn plots_at_last_bar_skips_points_past_the_last_bar() {
        let bars = bars(4);
        let model = mock_model("x", true, &bars, 1.0);
        let plots = EngineOrchestrator::plots_at_last_bar(&model, Some(bars[2].time));
        assert_eq!(plots.get("Mock"), Some(&Some(bars[2].close + 1.0)));
        let all = EngineOrchestrator::plots_at_last_bar(&model, None);
        assert_eq!(all.get("Mock"), Some(&Some(bars[3].close + 1.0)));
        let before = EngineOrchestrator::plots_at_last_bar(&model, Some(bars[0].time - 1));
        assert_eq!(before.get("Mock"), Some(&None));
    }
}

/// The ports of `set-market.test.ts` (sessions re-executing over a new market)
/// and `native-history-load.test.ts` (natives across a history load that is
/// still arriving): what they assert about the indicators section.
#[cfg(test)]
mod market_flow_tests {
    use std::cell::Cell;

    use super::fixtures::*;
    use super::wiring::*;
    use super::*;
    use crate::engine::indicator_handle::{IndicatorEventName, IndicatorHandle};
    use crate::engine::orchestrator::test_support::*;
    use crate::events::types::ChartEventName;
    use crate::indicators::classics::{classic_specs, register_classic_indicators};
    use crate::options::MarketSwitch;
    use crate::ports::market_data::MarketSession;

    fn flat_rig(builder: RigBuilder) -> Rig {
        builder
            .feed(|f| {
                for (s, p) in [("AAA", 100.0), ("BBB", 200.0), ("SLOW", 900.0)] {
                    f.set_price(s, p);
                }
            })
            .build()
    }

    fn switch_to(rig: &Rig, symbol: &str) {
        rig.switch(to_symbol(symbol)).expect("switch");
        flush(rig);
    }

    // ── set-market.test.ts ──

    #[test]
    fn sessions_re_execute_over_the_new_market() {
        let rig = flat_rig(Rig::builder());
        add(&rig, OVERLAY);
        rig.ready().expect("ready");
        flush(&rig);
        {
            let log = rig.engine.executions.borrow();
            assert_eq!(log.len(), 1);
            assert_eq!(
                (
                    log[0].market.symbol.as_str(),
                    log[0].market.timeframe.as_str(),
                    log[0].first_close
                ),
                ("AAA", "60", Some(100.0))
            );
        }

        switch_to(&rig, "BBB");
        assert_eq!(
            rig.engine.session(0).expect("first session").stops(),
            1,
            "the old session was torn down"
        );
        let log = rig.engine.executions.borrow();
        assert_eq!(log.len(), 2);
        assert_eq!(
            (log[1].market.symbol.as_str(), log[1].first_close),
            ("BBB", Some(200.0))
        );
    }

    #[test]
    fn the_charts_trading_session_rides_the_execution_market_and_a_switch_restarts_with_the_new_one()
     {
        let rig = Rig::builder().session(MarketSession::Extended).build();
        add(&rig, OVERLAY);
        rig.ready().expect("ready");
        flush(&rig);
        assert_eq!(
            rig.engine.executions.borrow()[0].market.session.as_deref(),
            Some("extended")
        );

        let plain = Rig::builder().build();
        add(&plain, OVERLAY);
        plain.ready().expect("ready");
        flush(&plain);
        assert_eq!(plain.engine.executions.borrow()[0].market.session, None);

        for (session, stops, runs) in [
            (MarketSession::Extended, 1, 2),
            (MarketSession::Regular, 2, 3),
        ] {
            plain
                .switch(MarketSwitch {
                    session: Some(session),
                    ..MarketSwitch::default()
                })
                .expect("switch");
            flush(&plain);
            let sessions = plain.engine.sessions.borrow().len();
            assert_eq!(sessions, runs);
            assert_eq!(
                plain.engine.executions.borrow()[runs - 1]
                    .market
                    .session
                    .as_deref(),
                Some(session.as_str())
            );
            assert_eq!(
                (0..sessions - 1)
                    .map(|n| plain.engine.session(n).map_or(0, |s| s.stops()))
                    .sum::<usize>(),
                stops
            );
        }
    }

    #[test]
    fn a_stale_model_landing_mid_switch_is_dropped_and_the_post_load_run_paints_the_new_market() {
        let rig = flat_rig(Rig::builder());
        rig.feed.gate_symbol("SLOW");
        let ind = add(&rig, OVERLAY);
        rig.ready().expect("ready");
        flush(&rig);
        let old = rig.engine.last_session().expect("session");

        let done = rig.chart.set_market(to_symbol("SLOW"));
        let mounted = rig.renderer.mounted_models.borrow().len();
        let patched = rig.renderer.updated.borrow().len();
        // The OLD session's run completes mid-switch, over the OLD bars.
        old.push_model(mock_model(ind.id(), true, &bars(5), 0.0));
        assert_eq!(
            rig.renderer.mounted_models.borrow().len(),
            mounted,
            "dropped, not remounted"
        );
        assert_eq!(
            rig.renderer.updated.borrow().len(),
            patched,
            "and never value-patched"
        );

        rig.feed.release();
        rig.block_on(done).expect("switch");
        flush(&rig);
        // The pending cause stays for the post-load re-execution.
        assert_eq!(
            rig.record(ind.id()).borrow().pending_cause,
            Some(ScriptRunCause::Market)
        );
        // The post-load re-execution delivers the new market's model.
        let fresh = rig.engine.last_session().expect("a new session");
        assert_eq!(rig.engine.sessions.borrow().len(), 2);
        let new_bars = rig.view();
        fresh.push_model(mock_model(ind.id(), true, &new_bars, 0.0));
        let last = rig
            .renderer
            .mounted_models
            .borrow()
            .last()
            .cloned()
            .expect("mounted");
        assert_eq!(
            last.series[0]
                .points()
                .and_then(|p| p.first())
                .map(|p| p.time),
            Some(new_bars[0].time)
        );
    }

    // ── native-history-load.test.ts ──

    fn universe(total: usize) -> Vec<Ohlcv> {
        (0..total)
            .map(|i| {
                let f = i as f64;
                let close = 100.0 + 10.0 * (f / 7.0).sin() + f / 50.0;
                let open = close - 2.0 * (f / 5.0).cos();
                Ohlcv::new(
                    T0 + i as i64 * HOUR,
                    open,
                    open.max(close) + 1.0,
                    open.min(close) - 1.0,
                    close,
                    Some(1.0),
                )
            })
            .collect()
    }

    fn tail(all: &[Ohlcv], n: usize) -> Vec<Ohlcv> {
        all[all.len() - n..].to_vec()
    }

    fn classics(r: &Registries) {
        register_classic_indicators(&mut r.native_indicators.borrow_mut());
    }

    fn hist_builder(total: usize, all: &[Ohlcv]) -> RigBuilder {
        let all = all.to_vec();
        Rig::builder()
            .bars(total)
            .registries(classics)
            .feed(move |f| {
                f.set_series("AAA", all.clone());
                f.set_series("OTHER", all.clone());
            })
    }

    fn line_points(model: &IndicatorModel) -> Vec<crate::model::SeriesPoint> {
        model
            .series
            .iter()
            .find_map(|s| s.points().map(<[_]>::to_vec))
            .unwrap_or_default()
    }

    fn model_of(chart: &EngineOrchestrator, id: &str) -> Option<IndicatorModel> {
        chart
            .inner
            .registry
            .get(id)
            .and_then(|r| r.borrow().model.clone())
    }

    /// The chart bar the renderer paints point 0 of `model` on (the anchor contract).
    fn paint_offset(model: &IndicatorModel, bars: &[Ohlcv]) -> isize {
        let (Some(anchor), Some(head)) = (model.anchor_time, bars.first().map(|b| b.time)) else {
            return 0;
        };
        if anchor < head {
            return -(line_points(model).iter().filter(|p| p.time < head).count() as isize);
        }
        bars.iter()
            .position(|b| b.time == anchor)
            .map_or(0, |i| i as isize)
    }

    /// Points that would be painted on a candle other than their own.
    fn misplaced_in(chart: &EngineOrchestrator, _renderer: &RecRenderer, id: &str) -> usize {
        let Some(model) = model_of(chart, id) else {
            return 0;
        };
        let bars = chart.view_bars();
        let offset = paint_offset(&model, &bars);
        line_points(&model)
            .iter()
            .enumerate()
            .filter(|(j, p)| {
                let i = *j as isize + offset;
                i >= 0 && bars.get(i as usize).map(|b| b.time) != Some(p.time)
            })
            .count()
    }

    fn misplaced(rig: &Rig, id: &str) -> usize {
        misplaced_in(&rig.chart, &rig.renderer, id)
    }

    fn painted_at(rig: &Rig, id: &str, i: usize) -> Option<f64> {
        let model = model_of(&rig.chart, id)?;
        let offset = paint_offset(&model, &rig.view());
        let at = i as isize - offset;
        usize::try_from(at)
            .ok()
            .and_then(|at| line_points(&model).get(at).and_then(|p| p.value))
    }

    fn ema_over(bars: &[Ohlcv]) -> Vec<f64> {
        let spec = classic_specs()
            .into_iter()
            .find(|s| s.indicator_type == "ema")
            .expect("ema");
        spec.compute(bars, &spec.default_inputs()).plots[0]
            .values
            .clone()
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    fn add_ema(rig: &Rig) -> Rc<IndicatorHandleImpl> {
        rig.chart
            .add_native_indicator("ema", NativeIndicatorOptions::default())
    }

    fn progressive_rig(total: usize, live: bool, volume: bool) -> (Rig, Vec<Ohlcv>) {
        let all = universe(total);
        let rig = hist_builder(total, &all)
            .progressive(true)
            .live(live)
            .volume(volume)
            .build();
        rig.settle();
        (rig, all)
    }

    #[test]
    fn the_ema_never_paints_on_the_wrong_candle_while_healed_history_lands() {
        let (rig, all) = progressive_rig(600, false, false);
        rig.feed.emit_batch(tail(&all, 60)); // the first confirmed stretch paints
        rig.ready().expect("ready");
        let ema = add_ema(&rig);
        flush(&rig);

        rig.feed.emit_batch(tail(&all, 250)); // older bars healed in behind the visible ones
        flush(&rig);
        assert_eq!(misplaced(&rig, ema.id()), 0);
        rig.feed.emit_batch(tail(&all, 500));
        flush(&rig);
        assert_eq!(misplaced(&rig, ema.id()), 0);

        rig.feed.finish_progressive(Ok(Some(all.clone())));
        rig.loaded();
        flush(&rig);
        let bars = rig.view();
        assert_eq!(bars.len(), 600);
        assert_eq!(misplaced(&rig, ema.id()), 0);
        // The plot reaches the newest candle and carries the FULL-history values.
        let truth = ema_over(&bars);
        for i in [bars.len() - 1, bars.len() - 200, 30] {
            assert!(close(painted_at(&rig, ema.id(), i), truth[i]), "bar {i}");
        }
    }

    #[test]
    fn the_ema_waits_for_the_complete_history_loading_row_first_one_compute_over_every_bar() {
        let (rig, all) = progressive_rig(600, false, false);
        rig.feed.emit_batch(tail(&all, 60));
        rig.ready().expect("ready");
        let ema = add_ema(&rig);
        let ready_fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&ready_fired);
        let _off = ema.on(IndicatorEventName::Ready, Box::new(move |_| flag.set(true)));
        flush(&rig);

        // Held: the legend row is up with a spinner, nothing is plotted yet, `ready` has not fired.
        let held = model_of(&rig.chart, ema.id()).expect("a row");
        assert_eq!(last_status(&rig, ema.id()), Some(IndicatorStatus::Loading));
        assert!(line_points(&held).is_empty());
        assert!(!ready_fired.get());
        assert!(
            rig.chart
                .inspect()
                .indicators
                .iter()
                .all(|s| s.id != ema.id())
        );
        assert!(
            rig.record(ema.id())
                .borrow()
                .native
                .as_ref()
                .is_some_and(|n| n.held_for_history)
        );

        rig.feed.emit_batch(tail(&all, 300));
        flush(&rig);
        assert!(line_points(&model_of(&rig.chart, ema.id()).expect("row")).is_empty());

        rig.feed.finish_progressive(Ok(Some(all.clone())));
        rig.loaded();
        flush(&rig);
        assert!(ready_fired.get());
        assert_eq!(last_status(&rig, ema.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            line_points(&model_of(&rig.chart, ema.id()).expect("row")).len(),
            600
        );
    }

    #[test]
    fn volume_paints_from_the_first_candles_only_the_computed_studies_wait() {
        let (rig, all) = progressive_rig(600, false, true);
        rig.feed.emit_batch(tail(&all, 60));
        rig.ready().expect("ready");
        let ema = add_ema(&rig);
        flush(&rig);
        let shown = rig.chart.inspect().indicators;
        assert!(
            shown
                .iter()
                .any(|s| s.native_type.as_deref() == Some("volume"))
        );
        assert!(!shown.iter().any(|s| s.id == ema.id()));
    }

    #[test]
    fn a_tick_during_the_wait_is_not_computed_on_its_own_and_after_completion_every_tick_updates_the_ema()
     {
        let (rig, all) = progressive_rig(600, true, false);
        rig.feed.emit_batch(tail(&all, 60));
        rig.ready().expect("ready");
        let ema = add_ema(&rig);
        flush(&rig);
        let forming = *all.last().expect("bars");
        rig.feed.push_tick(Ohlcv {
            close: forming.close + 5.0,
            ..forming
        }); // the forming candle refines mid-load
        flush(&rig);
        assert!(line_points(&model_of(&rig.chart, ema.id()).expect("row")).is_empty());

        rig.feed.finish_progressive(Ok(Some(all.clone())));
        rig.loaded();
        flush(&rig);
        assert_eq!(
            line_points(&model_of(&rig.chart, ema.id()).expect("row")).len(),
            600
        );

        rig.feed.push_tick(Ohlcv {
            time: forming.time + HOUR,
            close: forming.close + 3.0,
            ..forming
        });
        flush(&rig);
        assert_eq!(misplaced(&rig, ema.id()), 0);
        let bars = rig.view();
        let truth = ema_over(&bars);
        assert!(close(
            painted_at(&rig, ema.id(), bars.len() - 1),
            truth[bars.len() - 1]
        ));
    }

    #[test]
    fn a_native_with_the_default_policy_follows_each_snapshot_and_stays_on_its_candles() {
        let all = universe(400);
        let rig = hist_builder(400, &all).progressive(true).build();
        let made = made();
        register(
            &rig,
            descriptor("test-close-line", Kind::CloseLine, false, &made),
        );
        rig.settle();
        rig.feed.emit_batch(tail(&all, 40));
        rig.ready().expect("ready");
        let line = rig
            .chart
            .add_native_indicator("test-close-line", NativeIndicatorOptions::default());
        flush(&rig);
        assert_eq!(
            line_points(&model_of(&rig.chart, line.id()).expect("row")).len(),
            40
        );

        rig.feed.emit_batch(tail(&all, 150));
        flush(&rig);
        assert_eq!(misplaced(&rig, line.id()), 0);
        // It re-ran over the deeper snapshot: the healed bars carry values too.
        let bars = rig.view();
        assert_eq!(painted_at(&rig, line.id(), 0), Some(bars[0].close));
        assert_eq!(painted_at(&rig, line.id(), 149), Some(bars[149].close));
    }

    #[test]
    fn trimming_the_history_keeps_the_ema_on_its_candles() {
        let all = universe(300);
        let rig = hist_builder(300, &all).build();
        let ema = add_ema(&rig);
        rig.loaded();
        flush(&rig);
        assert_eq!(
            line_points(&model_of(&rig.chart, ema.id()).expect("row")).len(),
            300
        );

        rig.switch(MarketSwitch {
            bars: Some(100),
            ..MarketSwitch::default()
        })
        .expect("switch"); // trims in place
        flush(&rig);
        assert_eq!(rig.renderer.bars.borrow().len(), 100);
        assert_eq!(misplaced(&rig, ema.id()), 0);
    }

    #[test]
    fn a_shallower_depth_picked_while_the_history_is_loading_releases_the_waiting_ema() {
        let (rig, all) = progressive_rig(600, false, false);
        rig.feed.emit_batch(tail(&all, 60));
        rig.ready().expect("ready");
        let ema = add_ema(&rig);
        flush(&rig);
        assert_eq!(last_status(&rig, ema.id()), Some(IndicatorStatus::Loading));

        rig.switch(MarketSwitch {
            bars: Some(50),
            ..MarketSwitch::default()
        })
        .expect("switch");
        flush(&rig);
        assert_eq!(rig.renderer.bars.borrow().len(), 50);
        assert_eq!(last_status(&rig, ema.id()), Some(IndicatorStatus::Idle));
        assert_eq!(
            line_points(&model_of(&rig.chart, ema.id()).expect("row")).len(),
            50
        );
        assert_eq!(misplaced(&rig, ema.id()), 0);
    }

    #[test]
    fn a_depth_extension_during_a_market_switch_delivers_the_complete_poke_to_a_waiting_native() {
        let made = made();
        let mut waiter = descriptor("waiter", Kind::Line, false, &made);
        waiter.waits_for_history = true;
        let rig = Rig::builder()
            .bars(100)
            .feed(|f| f.set_depth("AAA", 900))
            .build();
        register(&rig, waiter);
        let handle = rig
            .chart
            .add_native_indicator("waiter", NativeIndicatorOptions::default());
        rig.loaded();
        let log = Arc::clone(&made.lock().expect("made")[0]);
        let before = log.lock().expect("calls").on_bars;
        drop(rig.chart.set_market(MarketSwitch {
            bars: Some(400),
            ..MarketSwitch::default()
        }));
        rig.loaded();
        assert_eq!(rig.renderer.bars.borrow().len(), 400);
        // The completion landed after the switch ended, so it reached the native:
        // one re-run over the deeper history (the backfill chunk itself is held).
        assert_eq!(log.lock().expect("calls").on_bars, before + 1);
        assert_eq!(
            line_points(&model_of(&rig.chart, handle.id()).expect("row")).len(),
            400
        );
    }

    #[test]
    fn a_fire_and_forget_start_runs_its_prefix_in_the_call_and_its_continuation_a_turn_later() {
        let made = made();
        let rig = Rig::builder().build();
        register(&rig, descriptor("probe", Kind::Probe, false, &made));
        rig.loaded();
        let _ = rig
            .chart
            .add_native_indicator("probe", NativeIndicatorOptions::default());
        // The record and its instance exist when the call returns; the start
        // (after `await readyPromise`, though ready long ago) has not run yet.
        assert_eq!(rig.chart.present_native_indicators(), ["probe"]);
        let log = Arc::clone(&made.lock().expect("made")[0]);
        assert_eq!(log.lock().expect("calls").start, 0);
        rig.settle();
        assert_eq!(log.lock().expect("calls").start, 1);
    }

    #[test]
    fn a_deeper_history_holds_the_ema_aligned_until_the_backfill_completes_then_recomputes_over_it()
    {
        let all = universe(400);
        let series = all.clone();
        let w = Wired::new(
            |r| r as Rc<dyn ChartRenderer>,
            |f| Rc::new(CappedFeed { inner: f, cap: 101 }) as Rc<dyn MarketDataFeed>, // 100 older bars per chunk
            Some(100),
            {
                let r = Registries::builtin();
                classics(&r);
                r
            },
            move |f| f.set_series("AAA", series),
            true,
        );
        let ema = w
            .chart
            .add_native_indicator("ema", NativeIndicatorOptions::default());
        w.ready();
        w.block_on(w.chart.history_complete());
        w.flush();
        let before = line_points(&model_of(&w.chart, ema.id()).expect("row")).len();

        // Sampled as each chunk lands: the plot computed over the first 100 bars stays put.
        let midway: Rc<RefCell<Vec<(usize, usize, usize)>>> = Rc::default();
        {
            let (sink, chart, renderer, id) = (
                Rc::clone(&midway),
                w.chart.clone(),
                Rc::clone(&w.renderer),
                ema.id().to_string(),
            );
            w.chart
                .events()
                .on(ChartEventName::HistoryProgress, move |_| {
                    let points = model_of(&chart, &id).map_or(0, |m| line_points(&m).len());
                    sink.borrow_mut().push((
                        renderer.bars.borrow().len(),
                        points,
                        misplaced_in(&chart, &renderer, &id),
                    ));
                });
        }
        w.switch(MarketSwitch {
            bars: Some(400),
            ..MarketSwitch::default()
        });
        w.block_on(w.chart.history_complete());
        assert_eq!(
            midway.borrow().iter().map(|m| m.0).collect::<Vec<_>>(),
            [200, 300, 400]
        );
        for (_, points, wrong) in midway.borrow().iter() {
            assert_eq!((*points, *wrong), (before, 0));
        }
        let bars = w.renderer.bars.borrow().clone();
        assert_eq!(bars.len(), 400);
        assert_eq!(misplaced_in(&w.chart, &w.renderer, ema.id()), 0);
        let model = model_of(&w.chart, ema.id()).expect("row");
        assert_eq!(line_points(&model).len(), 400);
        let truth = ema_over(&bars);
        let at = |i: usize| line_points(&model).get(i).and_then(|p| p.value);
        assert!(close(at(30), truth[30]) && close(at(399), truth[399]));
    }

    #[test]
    fn a_market_switch_blanks_the_ema_without_an_anchor() {
        let all = universe(200);
        let rig = hist_builder(200, &all).build();
        let ema = add_ema(&rig);
        rig.loaded();
        flush(&rig);
        let first_time = rig.renderer.bars.borrow()[0].time;
        assert_eq!(
            model_of(&rig.chart, ema.id()).and_then(|m| m.anchor_time),
            Some(first_time)
        );

        let mounts_before = rig.renderer.mounted_models.borrow().len();
        rig.switch(to_symbol("OTHER")).expect("switch");
        rig.loaded();
        flush(&rig);
        let blank = rig.renderer.mounted_models.borrow()[mounts_before..]
            .iter()
            .find(|m| m.id == ema.id() && line_points(m).is_empty() && !m.series.is_empty())
            .cloned();
        let blank = blank.expect("a blanked remount");
        assert_eq!(blank.anchor_time, None);
        assert_eq!(
            line_points(&model_of(&rig.chart, ema.id()).expect("row")).len(),
            200
        );
        assert_eq!(misplaced(&rig, ema.id()), 0);
    }

    #[test]
    fn switching_to_heikin_ashi_recomputes_the_ema_over_the_synthetic_candles_and_back() {
        let all = universe(200);
        let rig = hist_builder(200, &all).build();
        let ema = add_ema(&rig);
        rig.loaded();
        flush(&rig);
        let last = 199;

        rig.renderer.fire_price_style("heikinashi");
        flush(&rig);
        let ha = crate::price_styles::heikin_ashi_full(&all);
        assert_eq!(*rig.renderer.bars.borrow(), ha);
        assert!(close(painted_at(&rig, ema.id(), last), ema_over(&ha)[last]));
        assert!(
            (ema_over(&ha)[last] - ema_over(&all)[last]).abs() > 1e-3,
            "the two sources really differ"
        );

        rig.renderer.fire_price_style("candles");
        flush(&rig);
        assert!(close(
            painted_at(&rig, ema.id(), last),
            ema_over(&all)[last]
        ));
    }
}
