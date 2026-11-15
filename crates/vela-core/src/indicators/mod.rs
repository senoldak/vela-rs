//! Native (core-computed) indicators: the lifecycle an indicator instance is
//! driven through, the output it emits, and the catalog of registered types.
//!
//! Differences from the original `native-indicators/NativeIndicator.ts`:
//! - The host context is passed into each lifecycle call instead of being
//!   stored by the instance (an instance cannot hold a borrow of its host).
//! - The catalog is an explicit [`NativeIndicatorRegistry`] value, not a
//!   process-wide map.
//! - A descriptor's optional `isSupported` lives in the registry
//!   ([`NativeIndicatorRegistry::register_supported`]) rather than on the
//!   descriptor: adding a field would break every struct literal that builds a
//!   descriptor, the classic catalog's among them.
//! - `NativeIndicatorContext::data` and `push_data` have defaults ("no market
//!   data", "drop the payload") so a minimal host context stays small.

pub mod classics;
pub mod volume;
pub mod vpvr;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use crate::controls::data_control::DataControl;
use crate::i18n::MessageLike;
use crate::model::{
    Background, DrawingBox, DrawingLabel, DrawingLine, DrawingLinefill, DrawingPolyline,
    DrawingTable, Fill, InputSchema, Inputs, Millis, Ohlcv, PaneAxis, PaneHint, PriceLine,
    SeriesSpec,
};
use crate::runtime::{DataError, LocalBoxFuture};

/// An indicator's legend status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndicatorStatus {
    Idle,
    Loading,
    Live,
}

/// The visible time span of the chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleRange {
    pub from: Millis,
    pub to: Millis,
}

/// The renderer-neutral visual parts a native indicator emits each compute: a
/// subset of [`crate::model::IndicatorModel`]. The orchestrator wraps this
/// into a full model (stamping id / pane / native tag).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NativeIndicatorOutput {
    /// Value-axis override for the pane this native OWNS. `None` ⇒ the pane
    /// derives a scale from its content as usual.
    pub pane_axis: Option<PaneAxis>,
    pub series: Vec<SeriesSpec>,
    pub fills: Vec<Fill>,
    pub backgrounds: Vec<Background>,
    pub price_lines: Vec<PriceLine>,
    pub lines: Vec<DrawingLine>,
    pub boxes: Vec<DrawingBox>,
    pub labels: Vec<DrawingLabel>,
    pub polylines: Vec<DrawingPolyline>,
    pub linefills: Vec<DrawingLinefill>,
    pub tables: Vec<DrawingTable>,
}

/// Services the host gives a running native indicator.
pub trait NativeIndicatorContext {
    /// This instance's indicator id.
    fn id(&self) -> &str;
    fn symbol(&self) -> &str;
    fn timeframe(&self) -> &str;
    /// Whether the chart streams live data (false during a bar replay).
    fn live(&self) -> bool;
    /// The chart's trading session; `None` = regular / no session model.
    fn session(&self) -> Option<&str> {
        None
    }
    /// The canonical bar array, as it stands now.
    fn bars(&self) -> &[Ohlcv];
    /// Market-data access (trades, capabilities) for data-driven natives. The
    /// default is a detached control: every query answers empty.
    fn data(&self) -> DataControl {
        DataControl::detached()
    }
    /// Push a fresh render output; the chart mounts it (first call) or patches
    /// it (subsequent). Series payloads are index-aligned to `bars()` as it
    /// stands at the call.
    fn emit(&mut self, out: NativeIndicatorOutput);
    /// Push a BESPOKE render payload to the renderer's native layer for this
    /// indicator's type (for a native whose visuals are not ordinary series and
    /// fills: volume and VPVR push their layer config). The default drops it.
    fn push_data(&mut self, _data: Value) {}
    /// Set the indicator's legend status.
    fn set_status(&mut self, status: IndicatorStatus);
}

/// A core-computed indicator instance: the compute + lifecycle behind one
/// on-chart native indicator. The orchestrator owns one per instance and
/// drives it through these hooks.
pub trait NativeIndicator {
    /// Begin: compute + emit the first output.
    fn start(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs);
    /// The bars changed: recompute + emit.
    fn on_bars(&mut self, ctx: &mut dyn NativeIndicatorContext);
    /// The viewport changed (scroll/zoom): for range-aware natives.
    fn on_viewport(&mut self, ctx: &mut dyn NativeIndicatorContext, range: VisibleRange);
    /// Settings changed: recompute + emit. Before `start` this only records
    /// the inputs (a restore applies stored inputs the moment the handle exists).
    fn set_inputs(&mut self, ctx: &mut dyn NativeIndicatorContext, inputs: Inputs);
    /// Hidden: free resources; the instance and its state are kept for resume.
    fn suspend(&mut self);
    /// Shown again: resume + re-emit.
    fn resume(&mut self, ctx: &mut dyn NativeIndicatorContext);
    /// Removed or replaced: full teardown.
    fn stop(&mut self, ctx: &mut dyn NativeIndicatorContext);
}

/// Mints a fresh instance of a native-indicator type.
pub type NativeIndicatorFactory = Arc<dyn Fn() -> Box<dyn NativeIndicator> + Send + Sync>;

/// Static description + factory for a native-indicator TYPE.
#[derive(Clone)]
pub struct NativeIndicatorDescriptor {
    pub indicator_type: String,
    /// Full display name (picker, settings header, handle title).
    pub title: MessageLike,
    /// Compact name for the legend chip and the settings header. `None` ⇒ `title`.
    pub short_title: Option<MessageLike>,
    pub pane_hint: PaneHint,
    pub overlay: bool,
    /// Whether instances react to viewport changes.
    pub reacts_to_viewport: bool,
    /// Compute only over the chart's COMPLETE history, the way script
    /// indicators do, so values computed over a partial history never paint.
    pub waits_for_history: bool,
    /// Marks the type as beta.
    pub beta: bool,
    /// Whether instances get in-chart chrome. `None` ⇒ a legend row like any
    /// indicator; `Some(false)` ⇒ the output paints with no legend row.
    pub legend: Option<bool>,
    /// Allow several instances of this type on one chart.
    pub multi_instance: bool,
    pub inputs_schema: Vec<InputSchema>,
    pub factory: NativeIndicatorFactory,
}

impl NativeIndicatorDescriptor {
    /// The declaration default of every input, keyed by input key.
    pub fn default_inputs(&self) -> Inputs {
        self.inputs_schema
            .iter()
            .map(|i| (i.key.clone(), i.defval.clone()))
            .collect()
    }

    /// Mint a per-instance [`NativeIndicator`].
    pub fn create(&self) -> Box<dyn NativeIndicator> {
        (self.factory)()
    }
}

impl std::fmt::Debug for NativeIndicatorDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeIndicatorDescriptor")
            .field("indicator_type", &self.indicator_type)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

/// One entry of the "add native indicator" catalog: the type's static metadata
/// plus its live state on a specific chart (`chart.availableNativeIndicators()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeIndicatorInfo {
    pub indicator_type: String,
    /// The type's display name in the chart's language.
    pub title: String,
    /// Applies to the current symbol (a type may need data the provider lacks).
    pub supported: bool,
    /// At least one instance is on the chart. For a single-instance type a
    /// second add is a no-op; a multi-instance type adds another regardless.
    pub present: bool,
    /// The type allows several instances per chart.
    pub multi_instance: bool,
    /// The type is flagged beta (for a badge in the picker).
    pub beta: bool,
}

/// Whether a native-indicator type applies to `symbol` (a type may need a
/// provider capability). Used to gate auto-add and an "add native indicator"
/// menu. An `Err` reads as "not supported".
pub type IsSupportedFn =
    Rc<dyn Fn(&str, &DataControl) -> LocalBoxFuture<'static, Result<bool, DataError>>>;

#[derive(Clone)]
struct SupportCheck(IsSupportedFn);

impl std::fmt::Debug for SupportCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SupportCheck")
    }
}

/// The renderer-layer data channel of ONE instance of a multi-instance native
/// type. A single-instance type's channel is its type; a multi-instance type
/// needs one channel per instance.
pub fn native_instance_channel(indicator_type: &str, instance_id: &str) -> String {
    format!("{indicator_type}#{instance_id}")
}

/// A catalog of native-indicator types, in registration order.
#[derive(Clone, Debug, Default)]
pub struct NativeIndicatorRegistry {
    entries: Vec<NativeIndicatorDescriptor>,
    support: Vec<(String, SupportCheck)>,
}

impl NativeIndicatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a type. Re-registering a type replaces it in place, support
    /// check included (a plain `register` leaves the type always supported).
    pub fn register(&mut self, descriptor: NativeIndicatorDescriptor) {
        self.support
            .retain(|(t, _)| *t != descriptor.indicator_type);
        match self
            .entries
            .iter_mut()
            .find(|d| d.indicator_type == descriptor.indicator_type)
        {
            Some(slot) => *slot = descriptor,
            None => self.entries.push(descriptor),
        }
    }

    /// Register a type that applies only where `is_supported` says so.
    pub fn register_supported(
        &mut self,
        descriptor: NativeIndicatorDescriptor,
        is_supported: IsSupportedFn,
    ) {
        let indicator_type = descriptor.indicator_type.clone();
        self.register(descriptor);
        self.support
            .push((indicator_type, SupportCheck(is_supported)));
    }

    /// Whether `indicator_type` applies to `symbol`. No check registered: always
    /// supported. A check but no symbol: cannot decide, so unsupported. A check
    /// that fails: unsupported. An unregistered type: unsupported.
    pub fn is_supported(
        &self,
        indicator_type: &str,
        symbol: Option<&str>,
        data: &DataControl,
    ) -> LocalBoxFuture<'static, bool> {
        if self.get(indicator_type).is_none() {
            return Box::pin(std::future::ready(false));
        }
        let Some((_, SupportCheck(check))) = self.support.iter().find(|(t, _)| t == indicator_type)
        else {
            return Box::pin(std::future::ready(true));
        };
        let Some(symbol) = symbol.filter(|s| !s.is_empty()) else {
            return Box::pin(std::future::ready(false));
        };
        let pending = check(symbol, data);
        Box::pin(async move { pending.await.unwrap_or(false) })
    }

    /// Remove a registered type. Returns whether it was registered.
    pub fn unregister(&mut self, indicator_type: &str) -> bool {
        self.support.retain(|(t, _)| t != indicator_type);
        let before = self.entries.len();
        self.entries.retain(|d| d.indicator_type != indicator_type);
        self.entries.len() != before
    }

    /// Look up a registered descriptor.
    pub fn get(&self, indicator_type: &str) -> Option<&NativeIndicatorDescriptor> {
        self.entries
            .iter()
            .find(|d| d.indicator_type == indicator_type)
    }

    /// All registered types, in registration order.
    pub fn types(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|d| d.indicator_type.as_str())
            .collect()
    }

    /// All registered descriptors, in registration order.
    pub fn descriptors(&self) -> &[NativeIndicatorDescriptor] {
        &self.entries
    }
}
