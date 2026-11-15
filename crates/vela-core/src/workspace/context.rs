//! The workspace's widget context (`context.ts`): the contract contributed actions and
//! attachments build against (`ctx.chart` resolves lazily to the ACTIVE cell's chart),
//! plus the additive multi-chart members. Every read resolves LIVE: the context is built
//! per invocation and an attachment that holds its context for the shell's whole life keeps
//! reading the truth after every symbol or active-cell switch (a snapshot here once named
//! screenshot files after the mount-time market).
//!
//! # Differences from the original
//!
//! The original `WidgetContext` also carries the shell chrome: `host` (the root element),
//! `openSymbolSearch`, `togglePanel`, `dockStrip`, `dockEdge`, `topbar`, `panels` and
//! `sidebar`, plus `toast`. Those are the browser shell's and belong to the widget
//! contributions port (`crate::widget`); a browser builds its context by wrapping this one.
//! What a workspace owns is here: the live active cell's market and chart, the cells, the
//! active-cell switch, the replay, the translator, the dirty mark and the toast surface.
//! `ctx.chart` is `Option<Vela>` (the original throws before the first cell exists).

use std::rc::Rc;

use crate::i18n::Translator;
use crate::vela::Vela;

use super::cell::{ChartCell, ExternalIndicatorEntry};
use super::host::ToastKind;
use super::replay::WorkspaceReplay;
use super::shell::{WeakWorkspace, Workspace};

/// One live cell as the context lists it.
#[derive(Clone)]
pub struct ContextCell {
    pub id: String,
    pub chart: Vela,
    pub symbol: String,
    pub timeframe: String,
}

/// The extended context a workspace hands to contributed actions and attachments. A cheap
/// handle; every method reads the workspace live.
#[derive(Clone)]
pub struct WorkspaceWidgetContext {
    ws: WeakWorkspace,
}

impl Workspace {
    /// A fresh context bound to the workspace; every read resolves to the CURRENT active
    /// cell.
    pub fn context(&self) -> WorkspaceWidgetContext {
        WorkspaceWidgetContext {
            ws: self.downgrade(),
        }
    }
}

impl WorkspaceWidgetContext {
    fn active(&self) -> Option<ChartCell> {
        self.ws.upgrade()?.active()
    }

    /// The ACTIVE cell's chart (`None` before the first cell exists, or after destroy).
    pub fn chart(&self) -> Option<Vela> {
        self.active()?.try_chart()
    }

    /// The active cell's symbol (`""` before the first cell).
    pub fn symbol(&self) -> String {
        self.active().map(|c| c.symbol()).unwrap_or_default()
    }

    /// The active cell's timeframe (`"60"` before the first cell).
    pub fn timeframe(&self) -> String {
        self.active().map_or_else(|| "60".into(), |c| c.timeframe())
    }

    /// The active cell's price style (`"candles"` before the first cell).
    pub fn price_style(&self) -> String {
        self.active()
            .map_or_else(|| "candles".into(), |c| c.price_style())
    }

    pub fn set_symbol(&self, symbol: &str) {
        if let Some(c) = self.active() {
            c.set_symbol(symbol);
        }
    }

    pub fn set_timeframe(&self, timeframe: &str) {
        if let Some(c) = self.active() {
            c.set_timeframe(timeframe);
        }
    }

    pub fn set_price_style(&self, style: &str) {
        if let Some(c) = self.active() {
            c.set_price_style(style);
        }
    }

    /// Add a script indicator to the active cell through the public seam.
    pub fn add_indicator(&self, entry: ExternalIndicatorEntry) {
        if let Some(c) = self.active() {
            c.add_external_indicator(entry);
        }
    }

    /// Add a native indicator to the active cell.
    pub fn add_native_indicator(&self, indicator_type: &str) {
        if let Some(c) = self.active() {
            c.add_native(indicator_type);
        }
    }

    /// Third-party persistable state changed (`ctx.stateChanged()`): a debounced dirty mark.
    pub fn state_changed(&self) {
        if let Some(ws) = self.ws.upgrade() {
            ws.mark_state_dirty();
        }
    }

    /// The workspace's translator.
    pub fn i18n(&self) -> Option<Rc<dyn Translator>> {
        Some(self.ws.upgrade()?.i18n())
    }

    pub fn toast(&self, message: &str, kind: ToastKind) {
        if let Some(ws) = self.ws.upgrade() {
            ws.toast(message, kind, 3000.0);
        }
    }

    /// Every live cell (layout order): id, LIVE chart and its market. A fresh list per
    /// read: read it at the point of use, never keep it.
    pub fn cells(&self) -> Vec<ContextCell> {
        let Some(ws) = self.ws.upgrade() else {
            return Vec::new();
        };
        ws.cells()
            .into_iter()
            .filter_map(|c| {
                Some(ContextCell {
                    id: c.id().to_string(),
                    chart: c.try_chart()?,
                    symbol: c.symbol(),
                    timeframe: c.timeframe(),
                })
            })
            .collect()
    }

    /// LIVE: follows every active-cell switch (`""` before the first cell).
    pub fn active_cell_id(&self) -> String {
        self.ws
            .upgrade()
            .and_then(|ws| ws.active_id())
            .unwrap_or_default()
    }

    pub fn set_active_cell(&self, id: &str) {
        if let Some(ws) = self.ws.upgrade() {
            ws.set_active_cell(Some(id));
        }
    }

    /// Bar replay across every cell on one clock: drive this rather than one cell's
    /// `chart.replay`, so every chart replays together.
    pub fn replay(&self) -> Option<WorkspaceReplay> {
        Some(self.ws.upgrade()?.replay())
    }
}

#[cfg(test)]
mod tests;
