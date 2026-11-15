//! The workspace stylesheet: `VelaWorkspace.ts`'s `CSS` (the root, the regions, the grid,
//! the active-cell ring, the splitter hover, the maximize and drop-target presentation) and
//! `toast.ts`'s pill, with the design tokens written as `var(--vela-*)` exactly as the
//! original's `${t.*}` template does. One `<style id="vela-workspace">` per document, in the
//! `vela.chrome` cascade layer.
//!
//! Differences from the original:
//!
//! - The shell is always laid out as the REGIONS grid (`data-vela-regions` is always set),
//!   because the regions exist from the start here; a region that holds nothing is
//!   `display: none`, so an empty shell is the original's plain column (the grid between a
//!   zero-size topbar and a zero-size bottom bar).
//! - The rules of the components that are not ported yet (topbar, bottom bar, sidebar,
//!   panels, mobile bar) are not here: each component injects its own.

use web_sys::Document;

use crate::dom::create_element;

pub const STYLE_ID: &str = "vela-workspace";

/// The stylesheet text.
pub const CSS: &str = r#"@layer vela.chrome {

.vela-workspace { position: relative; width: 100%; height: 100%; display: flex; flex-direction: column; background: var(--vela-bg); color: var(--vela-fg); font-family: var(--vela-font); outline: none; }
.vela-ws-main { position: relative; display: flex; flex-direction: row; flex: 1 1 auto; min-height: 0; }
.vela-ws-toolbar { position: relative; flex: none; }
.vela-ws-strips { position: relative; flex: none; display: flex; flex-direction: column; min-width: 0; }
.vela-ws-strips:empty { display: none; }
/* REGIONS: the shell is a grid: the drawing toolbar's column, the chart column (topbar,
   main row, strips, bottom bar, lower strips), the column-panel region, the edge, with the
   mobile bar across the foot. Every known child is placed explicitly, so one hidden by CSS
   (the topbar on mobile, an empty region) never shifts the others. */
.vela-workspace[data-vela-regions] { display: grid; grid-template-columns: auto minmax(0, 1fr) auto auto; grid-template-rows: auto minmax(0, 1fr) auto auto auto auto; }
[data-vela-regions] > .vela-widget-topbar { grid-area: 1 / 1 / 2 / 3; }
[data-vela-regions] > .vela-ws-toolbar { grid-area: 2 / 1 / 6 / 2; display: flex; min-height: 0; }
[data-vela-regions] > .vela-ws-main { grid-area: 2 / 2; }
[data-vela-regions] > .vela-ws-strips { grid-area: 3 / 2; }
[data-vela-regions] > .vela-widget-bottombar { grid-area: 4 / 2; }
[data-vela-regions] > .vela-ws-strips-below { grid-area: 5 / 2; }
[data-vela-regions] > .vela-mobilebar { grid-area: 6 / 1 / 7 / -1; }
[data-vela-regions] > .vela-ws-column { grid-area: 1 / 3 / 6 / 4; position: relative; display: flex; min-height: 0; }
[data-vela-regions] > .vela-ws-edge { grid-area: 1 / 4 / 6 / 5; position: relative; display: flex; min-height: 0; }
/* Covering, a docked strip takes the area (the chart column and the drawing toolbar)
   whatever height it set itself while docked. */
[data-vela-regions] > [data-vela-cover] { grid-area: 1 / 1 / 5 / 3; z-index: 30; min-height: 0; min-width: 0; height: auto !important; max-height: none !important; align-self: stretch; }
/* A region nobody mounted into takes no room (and the mobile bar only shows on mobile). */
[data-vela-regions] > .vela-ws-region:empty { display: none; }
[data-layout='mobile'][data-vela-regions] > .vela-ws-column { grid-area: 1 / 1 / 6 / -1; z-index: 25; pointer-events: none; }
[data-layout='mobile'][data-vela-regions] > .vela-ws-column > .vela-panel { pointer-events: auto; }
.vela-ws-grid { position: relative; flex: 1 1 auto; min-width: 0; display: grid; gap: 2px; background: var(--vela-border-soft); }
.vela-cell { background: var(--vela-bg); position: relative; }
/* Active-cell highlight: an overlay ring ABOVE the chart's own canvas stack, inert to the
   pointer. Scoped to multi-cell grids ([data-multi]): a single-cell layout always has an
   active cell, and ringing the only chart would just be noise. */
.vela-ws-grid[data-multi='1'] .vela-cell[data-active='1']::after {
    content: '';
    position: absolute;
    inset: 0;
    border: 2px solid var(--vela-fg-bright);
    pointer-events: none;
    z-index: 10;
}
/* Splitter hover mirrors the in-chart pane separator hover: a soft band over the whole grab
   target + a solid 2px line on the seam center. */
.vela-ws-splitter:hover { background: var(--vela-separator-hover-band); }
.vela-ws-splitter:hover::after { content: ''; position: absolute; background: var(--vela-separator-hover-line); }
.vela-ws-splitter[data-axis='cols']:hover::after { inset-inline-start: calc(50% - 1px); top: 0; width: 2px; height: 100%; }
.vela-ws-splitter[data-axis='rows']:hover::after { top: calc(50% - 1px); inset-inline-start: 0; height: 2px; width: 100%; }
/* Mobile: the docked drawing-toolbar column would eat a phone-width grid. */
[data-layout='mobile'] .vela-ws-toolbar { display: none; }
/* A maximized cell owns the whole grid: the splitter strips have no seams to grab and the
   active ring would just outline the only visible chart. */
.vela-ws-grid[data-maximized='1'] .vela-ws-splitter { display: none; }
.vela-ws-grid[data-maximized='1'] .vela-cell[data-active='1']::after { display: none; }
/* Drop-target preview while a cell's drag handle is held: a dashed ring + the same soft wash
   the splitter hover uses, over the chart, inert to the pointer. */
.vela-cell[data-drop-target='1']::before {
    content: '';
    position: absolute;
    inset: 0;
    border: 2px dashed var(--vela-fg-bright);
    background: var(--vela-separator-hover-band);
    pointer-events: none;
    z-index: 11;
}
/* The status pill (toast.ts): bottom-center over the grid, one message at a time. */
.vela-toast {
    position: absolute;
    inset-inline-start: 50%;
    bottom: 34px;
    transform: translateX(-50%);
    z-index: 30;
    display: none;
    align-items: center;
    gap: 8px;
    max-width: 70%;
    padding: 6px 14px;
    border-radius: 999px;
    background: var(--vela-surface-elev);
    border: 1px solid var(--vela-border);
    color: var(--vela-fg);
    font-size: var(--vela-font-size-md);
    box-shadow: var(--vela-shadow);
    pointer-events: none;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.vela-toast[data-open] { display: inline-flex; }
.vela-toast[data-type='error'] { color: var(--vela-down); border-color: var(--vela-down); }
.vela-toast[data-type='success'] { color: var(--vela-up); }

}"#;

/// Inject the stylesheet once per document.
pub fn ensure_styles(doc: &Document) {
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let style = create_element(doc, "style");
    style.set_id(STYLE_ID);
    style.set_text_content(Some(CSS));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    } else if let Some(root) = doc.document_element() {
        let _ = root.append_child(&style);
    }
}
