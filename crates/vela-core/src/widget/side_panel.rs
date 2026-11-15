//! Side panel: a docked (or floating) column in the shell's panel dock. This is the pure
//! half of the original `SidePanel`: the width rules ([`clamp_panel_width`]), the labels,
//! and the panel's state machine ([`SidePanelModel`]: open, maximized, overlay, width,
//! the resizer drag).
//!
//! Left for the browser crate: the panel element, its header (title, slot, close, pin and
//! maximize buttons) and CSS, the exit animation of a close (a reopen cancels it), surface
//! announcements, and pointer capture on the resizer. The model tells the view what to do
//! through [`ToggleOutcome`]; the view reports back when an exit animation ends.

use crate::i18n::{MessageLike, msg};

/// Width (px) a panel takes when it declares none.
pub const DEFAULT_PANEL_WIDTH: f64 = 280.0;
/// Bounds a RESIZABLE panel takes when it declares none.
pub const DEFAULT_PANEL_MIN_WIDTH: f64 = 200.0;
pub const DEFAULT_PANEL_MAX_WIDTH: f64 = 640.0;

pub fn close_label() -> MessageLike {
    msg("widget.sidePanel.close", "Close")
}
pub fn restore_label() -> MessageLike {
    msg("widget.sidePanel.restore", "Restore")
}
pub fn maximize_label() -> MessageLike {
    msg("widget.sidePanel.maximize", "Maximize")
}
pub fn unpin_label() -> MessageLike {
    msg(
        "widget.sidePanel.unpin",
        "Unpin \u{2014} float over the chart",
    )
}
pub fn pin_label() -> MessageLike {
    msg("widget.sidePanel.pin", "Pin beside the chart")
}

/// Clamp a width to its bounds and round to whole pixels. The drag, a programmatic
/// `set_width`, and the restore of a persisted width all go through it, so a bad stored
/// value (or a `max` below `min`) can never paint an unusable column.
pub fn clamp_panel_width(px: f64, min: f64, max: f64) -> f64 {
    let lo = if min.is_finite() && min > 0.0 {
        min
    } else {
        DEFAULT_PANEL_MIN_WIDTH
    };
    // An infinite max is "no upper bound" (a fixed panel); a finite one below the min is a
    // caller's mistake: the min wins rather than collapsing the range.
    let hi = if max.is_finite() {
        max.max(lo)
    } else {
        f64::INFINITY
    };
    if !px.is_finite() {
        return lo;
    }
    crate::indicators::classics::math::js_round(px.max(lo).min(hi))
}

/// [`clamp_panel_width`] with the default bounds.
pub fn clamp_panel_width_default(px: f64) -> f64 {
    clamp_panel_width(px, DEFAULT_PANEL_MIN_WIDTH, DEFAULT_PANEL_MAX_WIDTH)
}

/// The panel's declared options.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SidePanelOptions {
    /// Declared width in px: also where a double-click on the drag handle returns.
    pub width: Option<f64>,
    /// Let the user drag the panel's inner edge. Fixed width when false.
    pub resizable: bool,
    pub min_width: Option<f64>,
    pub max_width: Option<f64>,
    /// Float over the chart's right edge instead of docking beside it. A floating panel
    /// gets a header pin the user can dock it with.
    pub overlay: bool,
    /// A header button that MAXIMIZES the panel over every chart of the shell.
    pub maximizable: bool,
    /// No header row: the panel's content draws its own.
    pub headerless: bool,
}

/// What the view owes after [`SidePanelModel::toggle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToggleOutcome {
    /// Nothing changed (already in the requested state).
    pub unchanged: bool,
    /// A close announces on the surface bus while the panel still shows.
    pub announce_close: bool,
    /// An open cancels the pending exit animation of an earlier close.
    pub cancel_exit: bool,
    /// Show the element (`hidden = false`).
    pub show: bool,
    /// Hide the element now (an instant close).
    pub hide_now: bool,
    /// Hide it once the exit animation ends: call [`SidePanelModel::exit_finished`].
    pub hold_for_exit: bool,
    /// An already-closed panel was asked to close instantly: finish its running exit.
    pub finish_exit: bool,
    /// `on_open_change` with this value.
    pub notify_open: Option<bool>,
    /// After the host heard the open: announce it (unless a listener closed it again).
    pub announce_open: bool,
    /// The maximized state changed as a side effect (a close or reopen restores it).
    pub maximized_changed: bool,
}

/// The panel's state: open, width, overlay (floating), maximized.
#[derive(Clone, Debug)]
pub struct SidePanelModel {
    opts: SidePanelOptions,
    min_width: f64,
    max_width: f64,
    declared_width: f64,
    width: f64,
    overlay: bool,
    maximized: bool,
    open: bool,
    /// Set while the close is announced, so a listener's own close does not re-enter.
    closing: bool,
    /// An exit animation of a close is running.
    exiting: bool,
}

impl SidePanelModel {
    pub fn new(opts: SidePanelOptions) -> Self {
        let min_width = opts.min_width.unwrap_or(DEFAULT_PANEL_MIN_WIDTH);
        let max_width = if opts.resizable {
            opts.max_width.unwrap_or(DEFAULT_PANEL_MAX_WIDTH)
        } else {
            opts.max_width.unwrap_or(f64::INFINITY)
        };
        let declared = clamp_panel_width(
            opts.width.unwrap_or(DEFAULT_PANEL_WIDTH),
            min_width,
            max_width,
        );
        Self {
            opts,
            min_width,
            max_width,
            declared_width: declared,
            width: declared,
            overlay: opts.overlay,
            maximized: false,
            open: false,
            closing: false,
            exiting: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    pub fn overlay(&self) -> bool {
        self.overlay
    }

    pub fn maximized(&self) -> bool {
        self.maximized
    }

    pub fn resizable(&self) -> bool {
        self.opts.resizable
    }

    /// Open or close (`None` flips). `instant` skips the exit animation of a close.
    pub fn toggle(&mut self, open: Option<bool>, instant: bool) -> ToggleOutcome {
        let open = open.unwrap_or(!self.open);
        let mut out = ToggleOutcome::default();
        if open == self.open {
            out.unchanged = true;
            out.finish_exit = !open && instant && self.exiting;
            if out.finish_exit {
                self.exiting = false;
            }
            return out;
        }
        if !open && self.closing {
            out.unchanged = true;
            return out;
        }
        if !open {
            out.announce_close = true;
        }
        self.open = open;
        if open {
            if self.exiting {
                self.exiting = false;
                out.cancel_exit = true;
                out.maximized_changed = self.set_maximized(false);
            }
            out.show = true;
        } else if instant {
            out.hide_now = true;
            out.maximized_changed = self.set_maximized(false);
        } else {
            self.exiting = true;
            out.hold_for_exit = true;
        }
        out.notify_open = Some(open);
        out.announce_open = open;
        out
    }

    /// The close announcement is in flight: a listener's own close must not re-enter.
    pub fn set_closing(&mut self, closing: bool) {
        self.closing = closing;
    }

    /// The exit animation ended: the panel hides, and a closed panel reopens at its own
    /// size (maximizing is a moment, not a placement). Returns whether maximize changed.
    pub fn exit_finished(&mut self) -> bool {
        self.exiting = false;
        self.set_maximized(false)
    }

    /// Maximize the panel over the shell's charts, or restore it. A no-op on a panel not
    /// declared `maximizable`. Returns whether the state changed.
    pub fn set_maximized(&mut self, maximized: bool) -> bool {
        if !self.opts.maximizable || maximized == self.maximized {
            return false;
        }
        self.maximized = maximized;
        true
    }

    /// Resize the panel (clamped). Silent: the host reports user drags only. Returns
    /// whether the width changed.
    pub fn set_width(&mut self, px: f64) -> bool {
        let next = clamp_panel_width(px, self.min_width, self.max_width);
        if next == self.width {
            return false;
        }
        self.width = next;
        true
    }

    /// Float or dock the panel. Only a panel declared `overlay` can float; on any other this
    /// is a no-op. Returns whether the placement changed.
    pub fn set_overlay(&mut self, overlay: bool) -> bool {
        if !self.opts.overlay || overlay == self.overlay {
            return false;
        }
        self.overlay = overlay;
        true
    }

    /// The pin button's pressed state: docked is pinned.
    pub fn pinned(&self) -> bool {
        !self.overlay
    }

    /// The pin's label: unpin when pinned, pin when floating.
    pub fn pin_label(&self) -> MessageLike {
        if self.pinned() {
            unpin_label()
        } else {
            pin_label()
        }
    }

    /// The maximize button's label.
    pub fn maximize_button_label(&self) -> MessageLike {
        if self.maximized {
            restore_label()
        } else {
            maximize_label()
        }
    }

    /// The maximize button's icon id.
    pub fn maximize_button_icon(&self) -> &'static str {
        if self.maximized {
            "restore"
        } else {
            "maximize"
        }
    }

    /// Start a resizer drag at pointer x (the primary button only is the view's check).
    pub fn begin_resize(&self, client_x: f64) -> ResizeDrag {
        ResizeDrag {
            start_x: client_x,
            start_width: self.width,
        }
    }

    /// A drag move: the panel is docked right, so dragging AWAY from the edge (left)
    /// widens it. Returns whether the width changed.
    pub fn resize_to(&mut self, drag: &ResizeDrag, client_x: f64) -> bool {
        self.set_width(drag.start_width + (drag.start_x - client_x))
    }

    /// The drag ended: the new width to report, when the user actually changed it.
    pub fn end_resize(&self, drag: &ResizeDrag) -> Option<f64> {
        (self.width != drag.start_width).then_some(self.width)
    }

    /// A double-click on the handle returns to the declared width: the new width to
    /// report when it changed.
    pub fn reset_width(&mut self) -> Option<f64> {
        let before = self.width;
        self.set_width(self.declared_width);
        (self.width != before).then_some(self.width)
    }
}

/// A resizer drag in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeDrag {
    start_x: f64,
    start_width: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::text_of;

    #[test]
    fn widths_clamp_to_their_bounds_and_round_to_whole_pixels() {
        assert_eq!(clamp_panel_width_default(300.4), 300.0);
        assert_eq!(clamp_panel_width_default(50.0), 200.0);
        assert_eq!(clamp_panel_width_default(9999.0), 640.0);
        assert_eq!(clamp_panel_width(250.5, 100.0, 400.0), 251.0);
    }

    #[test]
    fn a_bad_stored_width_or_inverted_bounds_never_paint_an_unusable_column() {
        assert_eq!(clamp_panel_width_default(f64::NAN), 200.0);
        assert_eq!(clamp_panel_width_default(f64::INFINITY), 200.0);
        // A max below the min: the min wins rather than collapsing the range.
        assert_eq!(clamp_panel_width(300.0, 400.0, 250.0), 400.0);
        // A non-positive or non-finite min falls back to the default one.
        assert_eq!(clamp_panel_width(10.0, 0.0, 640.0), 200.0);
        assert_eq!(clamp_panel_width(10.0, f64::NAN, 640.0), 200.0);
        // An infinite max is "no upper bound".
        assert_eq!(clamp_panel_width(5000.0, 200.0, f64::INFINITY), 5000.0);
    }

    fn model(opts: SidePanelOptions) -> SidePanelModel {
        SidePanelModel::new(opts)
    }

    #[test]
    fn a_panel_declares_its_width_and_a_fixed_panel_has_no_upper_bound() {
        let m = model(SidePanelOptions {
            width: Some(1000.0),
            ..SidePanelOptions::default()
        });
        assert_eq!(m.width(), 1000.0);
        let resizable = model(SidePanelOptions {
            width: Some(1000.0),
            resizable: true,
            ..SidePanelOptions::default()
        });
        assert_eq!(resizable.width(), 640.0);
        assert_eq!(model(SidePanelOptions::default()).width(), 280.0);
    }

    #[test]
    fn opening_and_closing_report_through_the_outcome() {
        let mut m = model(SidePanelOptions::default());
        let open = m.toggle(None, false);
        assert!(
            open.show && open.announce_open && open.notify_open == Some(true) && !open.unchanged
        );
        assert!(m.is_open());
        assert!(m.toggle(Some(true), false).unchanged);
        let close = m.toggle(None, false);
        assert!(close.announce_close && close.hold_for_exit && close.notify_open == Some(false));
        assert!(!m.is_open());
        assert!(!m.exit_finished());
    }

    #[test]
    fn an_instant_close_hides_now_and_a_reopen_during_the_exit_cancels_it() {
        let mut m = model(SidePanelOptions::default());
        m.toggle(Some(true), false);
        let instant = m.toggle(Some(false), true);
        assert!(instant.hide_now && !instant.hold_for_exit);
        m.toggle(Some(true), false);
        m.toggle(Some(false), false); // exit animation running
        let reopen = m.toggle(Some(true), false);
        assert!(reopen.cancel_exit && reopen.show);
        // An instant close of an already-closed panel finishes a running exit.
        m.toggle(Some(false), false);
        let finish = m.toggle(Some(false), true);
        assert!(finish.unchanged && finish.finish_exit);
        assert!(!m.toggle(Some(false), true).finish_exit);
    }

    #[test]
    fn a_listeners_own_close_while_the_close_is_announced_does_not_re_enter() {
        let mut m = model(SidePanelOptions::default());
        m.toggle(Some(true), false);
        m.set_closing(true);
        assert!(m.toggle(Some(false), false).unchanged);
        m.set_closing(false);
        assert!(!m.toggle(Some(false), false).unchanged);
    }

    #[test]
    fn maximizing_needs_a_maximizable_panel_and_a_close_restores_it() {
        let mut plain = model(SidePanelOptions::default());
        assert!(!plain.set_maximized(true));
        let mut m = model(SidePanelOptions {
            maximizable: true,
            ..SidePanelOptions::default()
        });
        m.toggle(Some(true), false);
        assert!(m.set_maximized(true));
        assert!(!m.set_maximized(true));
        assert_eq!(m.maximize_button_icon(), "restore");
        assert_eq!(text_of(None, &m.maximize_button_label(), &[]), "Restore");
        let close = m.toggle(Some(false), true);
        assert!(close.maximized_changed && !m.maximized());
        assert_eq!(m.maximize_button_icon(), "maximize");
        // A slow close restores when the exit animation ends.
        m.toggle(Some(true), false);
        m.set_maximized(true);
        m.toggle(Some(false), false);
        assert!(m.maximized());
        assert!(m.exit_finished());
        assert!(!m.maximized());
    }

    #[test]
    fn only_an_overlay_panel_can_float_and_the_pin_follows_the_placement() {
        let mut docked = model(SidePanelOptions::default());
        assert!(!docked.set_overlay(true));
        assert!(docked.pinned());
        let mut floating = model(SidePanelOptions {
            overlay: true,
            ..SidePanelOptions::default()
        });
        assert!(floating.overlay() && !floating.pinned());
        assert_eq!(
            text_of(None, &floating.pin_label(), &[]),
            "Pin beside the chart"
        );
        assert!(floating.set_overlay(false));
        assert!(!floating.set_overlay(false));
        assert_eq!(
            text_of(None, &floating.pin_label(), &[]),
            "Unpin \u{2014} float over the chart"
        );
    }

    #[test]
    fn dragging_away_from_the_edge_widens_and_only_a_changed_width_reports() {
        let mut m = model(SidePanelOptions {
            resizable: true,
            ..SidePanelOptions::default()
        });
        let drag = m.begin_resize(500.0);
        assert!(m.resize_to(&drag, 450.0));
        assert_eq!(m.width(), 330.0);
        assert!(!m.resize_to(&drag, 450.0));
        assert_eq!(m.end_resize(&drag), Some(330.0));
        // Dragging far past the bounds clamps; coming back to the start reports nothing.
        m.resize_to(&drag, -5000.0);
        assert_eq!(m.width(), 640.0);
        m.resize_to(&drag, 500.0);
        assert_eq!(m.end_resize(&drag), None);
    }

    #[test]
    fn a_double_click_returns_to_the_declared_width() {
        let mut m = model(SidePanelOptions {
            width: Some(300.0),
            resizable: true,
            ..SidePanelOptions::default()
        });
        assert_eq!(m.reset_width(), None);
        m.set_width(500.0);
        assert_eq!(m.reset_width(), Some(300.0));
        assert!(m.resizable());
    }

    #[test]
    fn the_labels_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(close_label()), "Close");
        assert_eq!(t(maximize_label()), "Maximize");
    }
}
