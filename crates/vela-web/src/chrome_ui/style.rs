//! The stylesheet of the in-chart chrome (legend, pane controls, axis buttons, scroll button,
//! tooltips, menus, popups), written once per document.
//!
//! Two families of colors:
//!
//! - the PLOT surface (legend rows, the fold chip, the axis-button strip, the scroll button)
//!   reads `--vela-lg-bg|fg|border|up`, which [`ChromeUi`](super::ChromeUi) writes on its root
//!   from the renderer's live plot theme (the original tints these with the chart theme, not
//!   the stable app chrome);
//! - the CHROME surface (menus, tooltips, popups) reads the `--vela-chrome-*` properties the
//!   host writes on the wrapper, like the drawings widgets.
//!
//! Hover and active tints are CSS so they never lag or stick. Layered like the host's own
//! rules, so a host stylesheet can override any of them.

use web_sys::Document;

use crate::dom::create_element;

const STYLE_ID: &str = "vela-chrome-ui-styles";

const CSS: &str = r#"@layer vela.chrome {

.vela-chrome-ui {
    --vc-fg: var(--vela-chrome-text-color, #b2b5be);
    --vc-bg: var(--vela-chrome-background, #151619);
    --vc-border: var(--vela-chrome-border-color, #2a2b30);
    --vc-muted: color-mix(in srgb, var(--vc-fg) 72%, transparent);
    --vc-hover: color-mix(in srgb, var(--vc-fg) 14%, transparent);
    --vc-active: color-mix(in srgb, var(--vc-fg) 22%, transparent);
    font-family: var(--vela-chrome-font-family, -apple-system, 'Segoe UI', sans-serif);
    box-sizing: border-box;
}
.vela-chrome-ui button { font: inherit; }
.vela-chrome-ui button:focus-visible,
.vela-ind-row:focus-visible,
.vela-callout[role='button']:focus-visible { outline: 2px solid color-mix(in srgb, var(--vc-fg) 70%, transparent); outline-offset: 1px; }

/* legend */
.vela-legend { position: absolute; left: 10px; z-index: 5; display: flex; flex-direction: column; align-items: flex-start; gap: 0; pointer-events: none; font: 12px -apple-system, 'Segoe UI', sans-serif; color: var(--vela-lg-fg, #d1d4dc); }
.vela-ind-row { pointer-events: auto; display: flex; align-items: center; background: color-mix(in srgb, var(--vela-lg-bg, #131722) 60%, transparent); border-radius: 4px; padding: 2px 6px; margin-left: -6px; max-width: calc(100% + 6px); min-height: 22px; box-sizing: border-box; color: var(--vela-lg-fg, #d1d4dc); user-select: none; -webkit-user-select: none; cursor: default; }
.vela-ind-row[data-open='1'] { background: var(--vela-lg-bg, #131722); box-shadow: inset 0 0 0 1px var(--vc-border); }
.vela-ind-row[data-hidden='1'] { opacity: 0.5; }
.vela-ind-title-wrap { white-space: nowrap; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.vela-ind-title { font-weight: 600; }
.vela-ind-callouts { display: none; align-items: center; gap: 4px; flex: none; margin-left: 8px; }
.vela-ind-status { display: none; box-sizing: border-box; flex: none; }
.vela-ind-status[data-status='loading'] { display: inline-flex; align-items: center; gap: 3px; margin-left: 8px; line-height: 0; transform: translateY(1px); }
.vela-ind-status[data-status='loading'] > span { width: 4px; height: 4px; border-radius: 50%; background: currentColor; opacity: 0.15; flex: none; animation: vela-ind-load 800ms ease-in-out infinite alternate; }
.vela-ind-status[data-status='loading'] > span:nth-child(2) { animation-delay: -260ms; }
.vela-ind-status[data-status='loading'] > span:nth-child(3) { animation-delay: -520ms; }
.vela-ind-status[data-status='live'] { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-left: 8px; transform: translateY(1px); background: var(--vela-lg-up, #26a69a); animation: vela-ind-pulse 1.2s ease-in-out infinite; }
.vela-ind-values { display: none; flex: 1 1 0%; min-width: 0; overflow: hidden; text-overflow: ellipsis; margin-inline-start: 6px; white-space: nowrap; font-variant-numeric: tabular-nums; }
.vela-ind-values > span + span { margin-inline-start: 5px; }
.vela-ind-values > span > span { unicode-bidi: isolate; }
.vela-ind-controls { display: none; align-items: center; gap: 6px; flex: none; margin-left: 6px; }
.vela-ind-row[data-open='1'] .vela-ind-controls, .vela-ind-row[data-hidden='1'] .vela-ind-controls { display: inline-flex; }
.vela-ind-row:not([data-open='1']) .vela-ind-controls > *:not(.vela-ind-eye) { display: none; }
.vela-ind-extras { display: contents; }
.vela-ind-ctl, .vela-ind-close { cursor: pointer; display: inline-flex; align-items: center; justify-content: center; border: none; line-height: 0; padding: 0; width: 18px; height: 18px; border-radius: 3px; box-sizing: border-box; flex: none; background: transparent; color: inherit; font-size: 16px; }
.vela-ind-ctl:hover, .vela-ind-close:hover { background: color-mix(in srgb, var(--vela-lg-fg, #d1d4dc) 16%, transparent); }
.vela-ind-ctl svg, .vela-ind-close svg { display: block; }
.vela-ind-fold { pointer-events: auto; display: inline-flex; align-items: center; gap: 4px; background: var(--vela-lg-bg, #131722); color: var(--vela-lg-fg, #d1d4dc); border: 1px solid var(--vela-lg-border, #2a2e39); border-radius: 4px; padding: 2px 6px; margin-top: 6px; cursor: pointer; font: inherit; line-height: 0; }
.vela-ind-fold-icon { display: inline-flex; align-items: center; line-height: 0; opacity: 0.7; font-size: 16px; }
.vela-ind-fold-count { font-weight: 600; line-height: normal; font-size: 12px; }
.vela-callout { display: inline-grid; place-items: center; border-radius: 50%; flex: none; line-height: 0; box-sizing: border-box; cursor: default; width: 16px; height: 16px; font-size: 12px; user-select: none; }
.vela-callout[role='button'] { cursor: pointer; }
.vela-callout svg { display: block; }
@keyframes vela-ind-pulse { 0%, 100% { opacity: 1; transform: translateY(1px) scale(1); } 50% { opacity: 0.3; transform: translateY(1px) scale(0.6); } }
@keyframes vela-ind-load { from { opacity: 0.12; } to { opacity: 0.55; } }

/* pane controls */
.vela-pc { position: absolute; z-index: 6; display: flex; gap: 2px; padding: 2px; border-radius: 6px; background: rgba(0,0,0,0.65); pointer-events: auto; }
.vela-pc[data-rest='1'] { opacity: 0; pointer-events: none; }
.vela-pc[data-rest='1']:focus-within { opacity: 1; pointer-events: auto; }
.vela-pc-btn { display: inline-flex; align-items: center; justify-content: center; padding: 0; border: none; border-radius: 4px; background: transparent; line-height: 0; font-size: 12px; width: 20px; height: 20px; color: var(--vc-muted); }
.vela-pc-btn svg { display: block; }
.vela-pc-btn:not(:disabled):hover { opacity: 1 !important; background: var(--vc-active); color: var(--vc-fg); }
.vela-pc-on, .vela-pc-on:not(:disabled):hover { background: var(--vc-fg); color: var(--vc-bg); }

/* axis scale buttons */
.vela-axis { position: absolute; z-index: 6; display: none; justify-content: center; align-items: center; gap: 4px; padding: 8px 0 6px; box-sizing: border-box; pointer-events: auto; background: var(--vela-lg-bg, #131722); }
.vela-axis[data-rest='1'] { display: flex; opacity: 0; pointer-events: none; }
.vela-axis[data-rest='1']:focus-within { opacity: 1; pointer-events: auto; }
.vela-axis-btn { display: inline-flex; align-items: center; justify-content: center; padding: 0; border: none; border-radius: 4px; background: transparent; cursor: pointer; font: 600 10px/1 -apple-system, 'Segoe UI', sans-serif; width: 18px; height: 18px; color: color-mix(in srgb, var(--vela-lg-fg, #d1d4dc) 72%, transparent); }
.vela-axis-btn:hover { background: color-mix(in srgb, var(--vela-lg-fg, #d1d4dc) 22%, transparent); color: var(--vela-lg-fg, #d1d4dc); }
.vela-axis-on, .vela-axis-on:hover { background: var(--vela-lg-fg, #d1d4dc); color: var(--vela-lg-bg, #131722); }

/* scroll to latest */
.vela-scroll-latest { position: absolute; z-index: 6; box-sizing: border-box; display: none; align-items: center; justify-content: center; cursor: pointer; border-radius: 5px; border: 1px solid var(--vela-lg-border, #2a2e39); box-shadow: 0 2px 8px rgba(0,0,0,0.4); background: color-mix(in srgb, var(--vela-lg-bg, #131722) 50%, transparent); color: var(--vela-lg-fg, #d1d4dc); line-height: 0; opacity: 0.85; pointer-events: auto; padding: 0; font-size: 12px; }
.vela-scroll-latest:hover { opacity: 1; }
.vela-scroll-latest svg { display: block; }

/* tooltips, menus, popups */
.vela-chrome-tip { position: absolute; z-index: 60; pointer-events: none; background: var(--vc-bg); border: 1px solid var(--vc-border); color: var(--vc-fg); border-radius: 6px; padding: 4px 9px; box-shadow: 0 6px 20px rgba(0,0,0,.35); font-size: 12px; white-space: nowrap; }
.vela-chrome-tip[data-wrap='1'] { white-space: normal; max-width: 260px; }
.vela-cmenu { position: absolute; z-index: 30; display: flex; flex-direction: column; gap: 2px; padding: 6px; border-radius: 6px; background: var(--vc-bg); border: 1px solid var(--vc-border); box-shadow: 0 6px 20px rgba(0,0,0,.35); color: var(--vc-fg); pointer-events: auto; font-size: 13px; min-width: 180px; max-height: 60vh; overflow-y: auto; box-sizing: border-box; }
.vela-cmenu-item { display: flex; align-items: center; gap: 8px; padding: 5px 10px 5px 8px; cursor: pointer; color: var(--vc-fg); border-radius: 4px; white-space: nowrap; background: transparent; border: none; text-align: start; }
.vela-cmenu-item:hover, .vela-cmenu-item:focus-visible { background: var(--vc-hover); outline: none; }
.vela-cmenu-item[aria-disabled='true'] { opacity: 0.45; cursor: default; }
.vela-cmenu-label { flex: 1 1 auto; text-align: start; }
.vela-cmenu-check { width: 14px; height: 14px; display: inline-flex; align-items: center; justify-content: center; flex: none; font-size: 14px; }
.vela-cmenu-sep { height: 1px; margin: 4px 8px; background: var(--vc-border); }
.vela-pop { position: absolute; z-index: 30; display: flex; flex-direction: column; gap: 8px; padding: 10px 12px; max-width: 280px; box-sizing: border-box; background: var(--vc-bg); border: 1px solid var(--vc-border); border-radius: 6px; box-shadow: 0 6px 20px rgba(0,0,0,.35); color: var(--vc-fg); font-size: 12px; pointer-events: auto; }
.vela-callout-panel { display: flex; flex-direction: column; gap: 8px; }
.vela-callout-title { font-weight: 600; color: var(--vc-fg); }
.vela-callout-text { color: var(--vc-muted); line-height: 1.45; white-space: pre-line; }
.vela-callout-actions { display: flex; flex-wrap: wrap; gap: 8px; }
.vela-callout-btn { cursor: pointer; height: 26px; padding: 0 10px; border-radius: 5px; border: 1px solid var(--vc-border); background: transparent; color: var(--vc-fg); font-size: 12px; font-family: inherit; }
.vela-callout-btn:hover { background: var(--vc-hover); }
.vela-callout-btn[data-primary] { border-color: var(--vc-fg); background: var(--vc-fg); color: var(--vc-bg); }
.vela-callout-btn[data-primary]:hover { opacity: 0.85; background: var(--vc-fg); }
.vela-marks-pop { padding: 0; gap: 0; max-width: 320px; }
.vela-marks-panel { padding: 0; gap: 0; min-width: 220px; max-width: 320px; max-height: 320px; overflow-y: auto; overscroll-behavior: contain; }
.vela-marks-section { display: flex; flex-direction: column; gap: 8px; padding: 10px 12px; }
.vela-marks-section + .vela-marks-section { border-top: 1px solid var(--vc-border); }
.vela-marks-field { display: flex; justify-content: space-between; gap: 16px; line-height: 1.45; }
.vela-marks-field-label { color: var(--vc-muted); }
.vela-marks-field-value { color: var(--vc-fg); text-align: end; font-variant-numeric: tabular-nums; }
.vela-marks-html { color: var(--vc-fg); line-height: 1.45; overflow-wrap: anywhere; }
.vela-marks-html p { margin: 0 0 6px; }
.vela-marks-html p:last-child { margin-bottom: 0; }
.vela-marks-html a { color: inherit; text-decoration: underline; }
.vela-marks-html img { max-width: 100%; height: auto; }
.vela-marks-html table { border-collapse: collapse; }
.vela-marks-html td, .vela-marks-html th { padding: 2px 6px; border: 1px solid var(--vc-border); }
.vela-marks-html pre, .vela-marks-html code { font-family: var(--vela-font-mono, monospace); font-size: 0.92em; }

}
"#;

/// Install the stylesheet once per document.
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
