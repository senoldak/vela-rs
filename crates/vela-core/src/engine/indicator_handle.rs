//! What `chart.addIndicator(...)` returns to the developer: the public
//! [`IndicatorHandle`] and its concrete [`IndicatorHandleImpl`], plus the
//! [`IndicatorController`] callback surface the orchestrator implements to be
//! driven by it (`IndicatorHandle.ts` and `IndicatorHandleImpl.ts`).
//!
//! # Ownership
//!
//! The orchestrator owns its handles (it looks them up by id), and a handle
//! drives the orchestrator back, so a strong reference both ways is a cycle. A
//! handle therefore holds its controller as a [`Weak`]: a handle kept by user
//! code after the chart is destroyed does not keep the engine alive. Once the
//! controller is gone every handle method is inert (reads answer empty,
//! mutations do nothing, `update_code` resolves `ok: false`, `context` resolves
//! `None`).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::events::TypedEventBus;
use crate::model::{InputSchema, InputValue, Inputs};
use crate::ports::chart_renderer::MoveTarget;
use crate::ports::scripting_engine::{ContextKey, EngineContextSnapshot, EngineError};
use crate::runtime::{LocalBoxFuture, Unsubscribe};

/// How an [`IndicatorHandle::update_code`] call ended. Never an error: a failure
/// is `ok == false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeUpdateResult {
    /// The new code is the one running (or, hidden, the one that will run when shown).
    pub ok: bool,
    /// Why the update did not land (the previous code is running again). `None`
    /// on success.
    pub error: Option<String>,
}

impl CodeUpdateResult {
    pub fn success() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
        }
    }
}

/// A per-indicator event name (`handle.on(...)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndicatorEventName {
    Ready,
    Error,
    Alert,
}

/// The `alert` payload of an indicator's own `on('alert')`.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorAlert {
    pub id: String,
    pub message: String,
    pub title: Option<String>,
    pub time: f64,
}

/// Per-indicator events emitted on `handle.on(...)`.
#[derive(Clone, Debug, PartialEq)]
pub enum IndicatorEvent {
    /// The first computed model landed (no payload).
    Ready,
    /// The script failed; the error's message.
    Error {
        error: String,
    },
    Alert(IndicatorAlert),
}

impl IndicatorEvent {
    /// The key to emit this event under.
    pub fn name(&self) -> IndicatorEventName {
        match self {
            Self::Ready => IndicatorEventName::Ready,
            Self::Error { .. } => IndicatorEventName::Error,
            Self::Alert(_) => IndicatorEventName::Alert,
        }
    }
}

/// A listener of one indicator event.
pub type IndicatorHandler = Box<dyn Fn(&IndicatorEvent)>;

/// What `chart.addIndicator(...)` returns to the developer. The handle is usable
/// synchronously; data renders when execution resolves (see the `Ready` event).
pub trait IndicatorHandle {
    fn id(&self) -> &str;
    fn title(&self) -> String;
    /// The script source this indicator currently runs (the one it was added
    /// with, until [`update_code`](Self::update_code) replaces it). `None` for a
    /// NATIVE (core-computed) indicator: there is no script to show. What a host
    /// editor opens when a legend action asks for "the code behind this row".
    fn source(&self) -> Option<String>;
    /// The registered type of a NATIVE (core-computed) indicator; `None` for a
    /// script indicator. The two are exclusive: a handle has a `source` or a
    /// `native_type`.
    fn native_type(&self) -> Option<&str>;
    /// The language id of the engine running this script indicator (e.g.
    /// `pine`): the `language` option it was added with, else the chart's
    /// `default_language`. Fixed for the indicator's life ([`update_code`]
    /// keeps it); `None` for a NATIVE indicator.
    ///
    /// [`update_code`]: Self::update_code
    fn language(&self) -> Option<&str>;
    /// Inputs parsed from the script source (populated once the script is prepared).
    fn inputs(&self) -> Vec<InputSchema>;
    /// Declaration-props schema (a strategy's `initial_capital`, an indicator's
    /// `precision`, ...; populated once the script is prepared). Empty when the
    /// engine exposes none.
    fn props(&self) -> Vec<InputSchema>;
    /// Whether the indicator is currently shown (vs hidden). Hidden indicators
    /// stop computing.
    fn visible(&self) -> bool;
    /// The CURRENT stored input values (declaration defaults merged with every
    /// edit so far): what state persistence diffs against the schema's `defval`s.
    fn input_values(&self) -> Inputs;
    /// The CURRENT stored declaration-prop values (overrides only: props keep no
    /// merged defaults; absent keys mean the declaration value).
    fn prop_values(&self) -> Inputs;
    fn set_input(&self, key: &str, value: InputValue);
    fn set_inputs(&self, values: Inputs);
    /// Override one declaration prop and re-run (a prop change replays the whole script).
    fn set_prop(&self, key: &str, value: InputValue);
    /// Override several declaration props at once and re-run.
    fn set_props(&self, values: Inputs);
    /// Replace the script and re-run it **in place**: same `id`, same legend row,
    /// same pane placement, same handle; the seam for a host editor's "run my
    /// edit" and for a library's "update to the new version". The new source is
    /// prepared first: only once it compiles is the running script stopped and
    /// the new one started. Until the new code's first run has landed, a failure
    /// (at compile time or at run time) puts the previous code back, with its
    /// input and prop values, and reports through `error`; the visuals on the
    /// chart are never dropped in between.
    ///
    /// An edited input or prop value survives where the new script still
    /// declares its key; a value still at its old declared default takes the NEW
    /// default, and anything else takes the new declaration default, all before
    /// the first run, so the new code computes once, with its final values.
    ///
    /// Resolves once the new code's first run has landed (`ok`), or with
    /// `ok == false` after a failure. A HIDDEN indicator resolves as soon as the
    /// source has compiled: it computes nothing until shown. An unchanged source
    /// resolves at once; a call superseded by a later one resolves with that
    /// later call's outcome. A native indicator resolves `ok == false`.
    fn update_code(&self, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult>;
    /// Hide or show the indicator. Hiding **suspends** it: its visuals are
    /// dropped (the legend row stays, marked hidden) and its computation stops
    /// (the engine session is torn down), so a hidden indicator consumes no
    /// resources. Showing re-runs it over the current bars.
    fn set_visible(&self, visible: bool);
    /// Move (or merge) this indicator to another pane: the main price pane, an
    /// existing pane, or a fresh pane (optionally placed relative to an existing
    /// one). Merging into a pane it does not own gives it its own scale column.
    /// A no-op (with a warning) on a renderer without pane management.
    fn move_to(&self, target: MoveTarget);
    /// Listen to one of this indicator's events. The returned function detaches it.
    fn on(&self, event: IndicatorEventName, handler: IndicatorHandler) -> Unsubscribe;
    /// Read-only snapshot of the engine's execution context (variables, plots,
    /// the script's return value, ...). Resolves `None` when the engine lacks the
    /// capability or nothing has run yet. Always a copy, never a live reference.
    /// `select` limits the extracted keys (keeps worker transfers small).
    fn context(
        &self,
        select: Option<&[ContextKey]>,
    ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>>;
    fn remove(&self);
}

/// Callback surface the handle uses to drive the orchestrator.
pub trait IndicatorController {
    fn get_indicator_context(
        &self,
        id: &str,
        select: Option<&[ContextKey]>,
    ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>>;
    fn apply_inputs(&self, id: &str, values: Inputs);
    fn input_values_of(&self, id: &str) -> Inputs;
    fn prop_values_of(&self, id: &str) -> Inputs;
    fn apply_props(&self, id: &str, values: Inputs);
    fn update_code(&self, id: &str, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult>;
    fn remove_indicator(&self, id: &str);
    fn set_visible(&self, id: &str, visible: bool);
    fn move_indicator(&self, id: &str, target: MoveTarget);
}

/// Concrete [`IndicatorHandle`] returned by `addIndicator`. Share it as
/// `Rc<IndicatorHandleImpl>`.
pub struct IndicatorHandleImpl {
    id: String,
    title: RefCell<String>,
    source: RefCell<Option<String>>,
    native_type: Option<String>,
    language: Option<String>,
    controller: Weak<dyn IndicatorController>,
    schema: RefCell<Vec<InputSchema>>,
    props_schema: RefCell<Vec<InputSchema>>,
    visible_state: Cell<bool>,
    bus: TypedEventBus<IndicatorEventName, IndicatorEvent>,
}

impl IndicatorHandleImpl {
    /// `source` is set for a script indicator, `native_type` for a native one,
    /// `language` for a script indicator's resolved engine.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        controller: Weak<dyn IndicatorController>,
        source: Option<String>,
        native_type: Option<String>,
        language: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: RefCell::new(title.into()),
            source: RefCell::new(source),
            native_type,
            language,
            controller,
            schema: RefCell::new(Vec::new()),
            props_schema: RefCell::new(Vec::new()),
            visible_state: Cell::new(true),
            bus: TypedEventBus::new(),
        }
    }

    // Internal (used by the orchestrator).

    /// Revise the display title (the declared title replaces the placeholder).
    pub fn set_title(&self, title: impl Into<String>) {
        *self.title.borrow_mut() = title.into();
    }

    pub fn set_schema(&self, schema: Vec<InputSchema>) {
        *self.schema.borrow_mut() = schema;
    }

    pub fn set_props_schema(&self, schema: Vec<InputSchema>) {
        *self.props_schema.borrow_mut() = schema;
    }

    /// Sync the public `source` once a code update has been prepared and taken
    /// over (never before).
    pub fn set_source(&self, source: impl Into<String>) {
        *self.source.borrow_mut() = Some(source.into());
    }

    /// Sync the public `visible` getter when the orchestrator changes visibility
    /// (API or legend eye).
    pub fn set_visible_state(&self, visible: bool) {
        self.visible_state.set(visible);
    }

    /// Emit an event to this handle's listeners.
    pub fn emit(&self, event: IndicatorEvent) {
        self.bus.emit(&event.name(), &event);
    }

    fn controller(&self) -> Option<Rc<dyn IndicatorController>> {
        self.controller.upgrade()
    }
}

impl IndicatorHandle for IndicatorHandleImpl {
    fn id(&self) -> &str {
        &self.id
    }

    fn title(&self) -> String {
        self.title.borrow().clone()
    }

    fn source(&self) -> Option<String> {
        self.source.borrow().clone()
    }

    fn native_type(&self) -> Option<&str> {
        self.native_type.as_deref()
    }

    fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    fn inputs(&self) -> Vec<InputSchema> {
        self.schema.borrow().clone()
    }

    fn props(&self) -> Vec<InputSchema> {
        self.props_schema.borrow().clone()
    }

    fn visible(&self) -> bool {
        self.visible_state.get()
    }

    fn input_values(&self) -> Inputs {
        self.controller()
            .map(|c| c.input_values_of(&self.id))
            .unwrap_or_default()
    }

    fn prop_values(&self) -> Inputs {
        self.controller()
            .map(|c| c.prop_values_of(&self.id))
            .unwrap_or_default()
    }

    fn set_input(&self, key: &str, value: InputValue) {
        self.set_inputs(Inputs::from([(key.to_string(), value)]));
    }

    fn set_inputs(&self, values: Inputs) {
        if let Some(controller) = self.controller() {
            controller.apply_inputs(&self.id, values);
        }
    }

    fn set_prop(&self, key: &str, value: InputValue) {
        self.set_props(Inputs::from([(key.to_string(), value)]));
    }

    fn set_props(&self, values: Inputs) {
        if let Some(controller) = self.controller() {
            controller.apply_props(&self.id, values);
        }
    }

    fn update_code(&self, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult> {
        match self.controller() {
            Some(controller) => controller.update_code(&self.id, source),
            None => Box::pin(std::future::ready(CodeUpdateResult::failure(
                "the chart was destroyed",
            ))),
        }
    }

    fn set_visible(&self, visible: bool) {
        if let Some(controller) = self.controller() {
            controller.set_visible(&self.id, visible);
        }
    }

    fn move_to(&self, target: MoveTarget) {
        if let Some(controller) = self.controller() {
            controller.move_indicator(&self.id, target);
        }
    }

    fn on(&self, event: IndicatorEventName, handler: IndicatorHandler) -> Unsubscribe {
        let subscription = self.bus.on(event, handler);
        let bus = self.bus.clone();
        Box::new(move || {
            bus.off(&subscription);
        })
    }

    fn context(
        &self,
        select: Option<&[ContextKey]>,
    ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>> {
        match self.controller() {
            Some(controller) => controller.get_indicator_context(&self.id, select),
            None => Box::pin(std::future::ready(Ok(None))),
        }
    }

    fn remove(&self) {
        if let Some(controller) = self.controller() {
            controller.remove_indicator(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::chart_renderer::NewPanePlacement;
    use crate::runtime::testing::ManualPlatform;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        stored: RefCell<Inputs>,
    }

    impl IndicatorController for Recorder {
        fn get_indicator_context(
            &self,
            id: &str,
            select: Option<&[ContextKey]>,
        ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>> {
            self.calls.borrow_mut().push(format!(
                "context {id} {}",
                select.map_or(0, <[ContextKey]>::len)
            ));
            Box::pin(std::future::ready(Err(EngineError::new("no session"))))
        }
        fn apply_inputs(&self, id: &str, values: Inputs) {
            self.calls.borrow_mut().push(format!(
                "inputs {id} {:?}",
                values.keys().collect::<Vec<_>>()
            ));
            self.stored.borrow_mut().extend(values);
        }
        fn input_values_of(&self, id: &str) -> Inputs {
            self.calls.borrow_mut().push(format!("read inputs {id}"));
            self.stored.borrow().clone()
        }
        fn prop_values_of(&self, id: &str) -> Inputs {
            self.calls.borrow_mut().push(format!("read props {id}"));
            Inputs::new()
        }
        fn apply_props(&self, id: &str, values: Inputs) {
            self.calls.borrow_mut().push(format!(
                "props {id} {:?}",
                values.keys().collect::<Vec<_>>()
            ));
        }
        fn update_code(&self, id: &str, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult> {
            self.calls.borrow_mut().push(format!("code {id} {source}"));
            Box::pin(std::future::ready(CodeUpdateResult::success()))
        }
        fn remove_indicator(&self, id: &str) {
            self.calls.borrow_mut().push(format!("remove {id}"));
        }
        fn set_visible(&self, id: &str, visible: bool) {
            self.calls
                .borrow_mut()
                .push(format!("visible {id} {visible}"));
        }
        fn move_indicator(&self, id: &str, target: MoveTarget) {
            self.calls
                .borrow_mut()
                .push(format!("move {id} {target:?}"));
        }
    }

    fn rig() -> (Rc<Recorder>, IndicatorHandleImpl) {
        let controller = Rc::new(Recorder::default());
        let as_dyn: Rc<dyn IndicatorController> = controller.clone();
        let handle = IndicatorHandleImpl::new(
            "ind-1",
            "SMA",
            Rc::downgrade(&as_dyn),
            Some("//@version=5".into()),
            None,
            Some("pine".into()),
        );
        (controller, handle)
    }

    #[test]
    fn identity_is_fixed_and_source_follows_the_orchestrator() {
        let (_controller, handle) = rig();
        assert_eq!(handle.id(), "ind-1");
        assert_eq!(handle.title(), "SMA");
        assert_eq!(handle.source().as_deref(), Some("//@version=5"));
        assert_eq!(handle.native_type(), None);
        assert_eq!(handle.language(), Some("pine"));
        assert!(handle.visible());
        assert!(handle.inputs().is_empty() && handle.props().is_empty());
        handle.set_source("//@version=6");
        handle.set_title("Declared");
        handle.set_visible_state(false);
        handle.set_schema(vec![InputSchema::new(
            "len",
            "Length",
            crate::model::InputType::Int,
            14.0,
        )]);
        handle.set_props_schema(vec![InputSchema::new(
            "precision",
            "Precision",
            crate::model::InputType::Int,
            2.0,
        )]);
        assert_eq!(handle.source().as_deref(), Some("//@version=6"));
        assert_eq!(handle.title(), "Declared");
        assert!(!handle.visible());
        assert_eq!(handle.inputs()[0].key, "len");
        assert_eq!(handle.props()[0].key, "precision");
    }

    #[test]
    fn a_native_handle_has_a_type_and_no_source_or_language() {
        let controller: Rc<dyn IndicatorController> = Rc::new(Recorder::default());
        let handle = IndicatorHandleImpl::new(
            "vol",
            "Volume",
            Rc::downgrade(&controller),
            None,
            Some("volume".into()),
            None,
        );
        assert_eq!(handle.native_type(), Some("volume"));
        assert_eq!((handle.source(), handle.language()), (None, None));
    }

    #[test]
    fn mutations_and_reads_go_through_the_controller_by_id() {
        let (controller, handle) = rig();
        handle.set_input("len", InputValue::Number(20.0));
        handle.set_inputs(Inputs::from([
            ("a".to_string(), InputValue::Bool(true)),
            ("b".to_string(), InputValue::Text("x".into())),
        ]));
        handle.set_prop("precision", InputValue::Number(3.0));
        handle.set_props(Inputs::from([("p".to_string(), InputValue::Number(1.0))]));
        handle.set_visible(false);
        handle.move_to(MoveTarget::NewPane(NewPanePlacement::default()));
        handle.move_to(MoveTarget::Price);
        handle.remove();
        assert_eq!(handle.input_values().len(), 3);
        assert!(handle.prop_values().is_empty());
        assert_eq!(
            *controller.calls.borrow(),
            [
                "inputs ind-1 [\"len\"]",
                "inputs ind-1 [\"a\", \"b\"]",
                "props ind-1 [\"precision\"]",
                "props ind-1 [\"p\"]",
                "visible ind-1 false",
                "move ind-1 NewPane(NewPanePlacement { before: None, after: None })",
                "move ind-1 Price",
                "remove ind-1",
                "read inputs ind-1",
                "read props ind-1",
            ]
        );
    }

    #[test]
    fn update_code_and_context_resolve_through_the_controller() {
        let platform = ManualPlatform::new(0.0);
        let (controller, handle) = rig();
        let result = platform.block_on(handle.update_code("new"));
        assert_eq!(result, CodeUpdateResult::success());
        let snapshot = platform.block_on(handle.context(Some(&[ContextKey::Plots])));
        assert_eq!(snapshot, Err(EngineError::new("no session")));
        assert_eq!(
            *controller.calls.borrow(),
            ["code ind-1 new", "context ind-1 1"]
        );
    }

    #[test]
    fn listeners_hear_their_event_only_and_detach() {
        let (_controller, handle) = rig();
        let heard: Rc<RefCell<Vec<IndicatorEvent>>> = Rc::default();
        let (ready, errors) = (heard.clone(), heard.clone());
        let off_ready = handle.on(
            IndicatorEventName::Ready,
            Box::new(move |e| ready.borrow_mut().push(e.clone())),
        );
        let _off_error = handle.on(
            IndicatorEventName::Error,
            Box::new(move |e| errors.borrow_mut().push(e.clone())),
        );
        handle.emit(IndicatorEvent::Ready);
        handle.emit(IndicatorEvent::Error {
            error: "boom".into(),
        });
        handle.emit(IndicatorEvent::Alert(IndicatorAlert {
            id: "a".into(),
            message: "m".into(),
            title: None,
            time: 1.0,
        }));
        off_ready();
        handle.emit(IndicatorEvent::Ready);
        assert_eq!(
            *heard.borrow(),
            [
                IndicatorEvent::Ready,
                IndicatorEvent::Error {
                    error: "boom".into()
                }
            ]
        );
    }

    #[test]
    fn a_handle_outliving_its_chart_is_inert_and_does_not_keep_it_alive() {
        let platform = ManualPlatform::new(0.0);
        let (controller, handle) = rig();
        let weak = Rc::downgrade(&controller);
        drop(controller);
        assert!(
            weak.upgrade().is_none(),
            "the handle must not own the controller"
        );
        handle.set_input("len", InputValue::Number(1.0));
        handle.set_visible(false);
        handle.move_to(MoveTarget::Price);
        handle.remove();
        assert!(handle.input_values().is_empty());
        assert!(handle.prop_values().is_empty());
        let result = platform.block_on(handle.update_code("x"));
        assert!(!result.ok);
        assert!(result.error.is_some());
        assert_eq!(platform.block_on(handle.context(None)), Ok(None));
    }
}
