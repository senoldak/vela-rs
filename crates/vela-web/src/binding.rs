//! What one chart gets on a page, whoever owns it: the single `VelaApp` chart or one cell of
//! a workspace.
//!
//! Binding a chart has three parts, in this order:
//!
//! 1. [`bind_renderer`]: the DOM that needs only the renderer (the drawings widgets, the
//!    legend and the chart dialogs). Available as soon as the renderer exists.
//! 2. [`ChartInputs::attach`]: the pointer, wheel and keyboard listeners and the size
//!    observers. The renderer must be mounted (the data canvas exists).
//! 3. [`bind_chart`]: what needs the engine (`Vela`): the translators, the live indicator
//!    values and the symbol facts the dialogs read. A workspace host receives the renderer
//!    first and the chart later (it must not call back into the workspace while it is being
//!    called), so this part is separate.

use std::cell::RefCell;
use std::rc::Rc;

use vela_core::chart_types::ChartTypeRegistry;
use vela_core::render::native::NativeRenderer;
use vela_core::runtime::Platform;
use vela_core::vela::Vela;

use crate::host::BrowserHost;
use crate::input::{InputWiring, SizeWatch};

/// Tell the drawings widgets, the legend and the dialogs which renderer they act on (and
/// which chart-type registry the settings tabs read).
pub(crate) fn bind_renderer(
    host: &Rc<BrowserHost>,
    renderer: &Rc<NativeRenderer>,
    registry: Rc<RefCell<ChartTypeRegistry>>,
) {
    host.bind_renderer(renderer);
    host.dialogs.bind(renderer, registry);
}

/// The chart's language for every widget, and the engine-side reads of the dialogs: the
/// current indicator values (the mounted model keeps its open-time ones) and the symbol
/// facts that grey out the styles a symbol cannot draw. Edits reach the engine through the
/// events it subscribed to on the renderer (input change, remove, hide, move, theme, price
/// style): nothing to wire for them.
pub(crate) fn bind_chart(host: &Rc<BrowserHost>, chart: &Vela) {
    host.dialogs.set_translator(Some(chart.i18n()));
    host.chrome().set_translator(Some(chart.i18n()));
    host.drawings_ui().set_translator(Some(chart.i18n()));
    host.set_translator(Some(chart.i18n()));
    let weak = chart.downgrade();
    host.dialogs
        .set_values_source(Some(Rc::new(move |id: &str| {
            let chart = weak.upgrade()?;
            chart
                .indicators()
                .into_iter()
                .find(|h| h.id() == id)
                .map(|h| (h.input_values(), h.prop_values()))
        })));
    let weak = chart.downgrade();
    host.dialogs
        .set_facts_source(Some(Rc::new(move || weak.upgrade()?.symbol_facts())));
}

/// Only the translators of [`bind_chart`] (a language change re-reads them).
pub(crate) fn rebind_translators(host: &Rc<BrowserHost>, chart: &Vela) {
    host.dialogs.set_translator(Some(chart.i18n()));
    host.chrome().set_translator(Some(chart.i18n()));
    host.drawings_ui().set_translator(Some(chart.i18n()));
    host.set_translator(Some(chart.i18n()));
}

/// The input listeners and the size observers of one chart. Dropping it removes them all.
pub(crate) struct ChartInputs {
    _wiring: Option<InputWiring>,
    _watch: SizeWatch,
}

impl ChartInputs {
    pub fn attach(
        renderer: Rc<NativeRenderer>,
        host: Rc<BrowserHost>,
        platform: Rc<dyn Platform>,
    ) -> Self {
        let wiring = InputWiring::attach(renderer.clone(), host.clone(), platform);
        let watch = SizeWatch::start(renderer, host);
        Self {
            _wiring: wiring,
            _watch: watch,
        }
    }
}
