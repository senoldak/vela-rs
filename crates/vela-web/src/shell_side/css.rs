//! The stylesheet of the side surfaces: the sidebar, the docked panels, the data window, the
//! object tree, the menus, the shortcuts panel and the mobile surfaces. The rules are the
//! original's with `${t.*}` written as `var(--vela-*)`; one `<style id="vela-shell-side">` per
//! document, in the `vela.chrome` layer.

use web_sys::Document;

use crate::dom::create_element;

pub const STYLE_ID: &str = "vela-shell-side";

const DESKTOP: &str = r#"
/* ── sidebar ── */
.vela-sidebar {
    order: 1;
    width: 44px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    gap: 12px;
    padding: var(--vela-space-1) 0;
    border-inline-start: 1px solid var(--vela-border-soft);
    color: var(--vela-fg-muted);
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: none;
    user-select: none;
}
.vela-sidebar::-webkit-scrollbar { display: none; }
[data-layout='mobile'] .vela-sidebar { display: none; }
.vela-sidebar-section, .vela-sidebar-group { display: flex; flex-direction: column; align-items: center; gap: 2px; }
.vela-sidebar-section { gap: 6px; }
.vela-sidebar-sep { width: 20px; height: 1px; background: var(--vela-border); }
.vela-sidebar-btn {
    all: unset;
    box-sizing: border-box;
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 30px;
    border-radius: var(--vela-radius-sm);
    cursor: pointer;
    color: var(--vela-fg-muted);
    font-size: 18px;
}
.vela-sidebar-btn:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-sidebar-btn[data-active='1'] { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-sidebar-btn:focus-visible { outline: 2px solid var(--vela-focus); outline-offset: -2px; }
.vela-sidebar-badge {
    position: absolute;
    top: 1px;
    inset-inline-end: 1px;
    min-width: 13px;
    height: 13px;
    padding: 0 3px;
    box-sizing: border-box;
    border-radius: 7px;
    background: var(--vela-accent);
    color: var(--vela-fg-on-fill);
    font-size: 9px;
    font-weight: 700;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
}
.vela-icon { display: inline-flex; align-items: center; justify-content: center; width: 1em; height: 1em; }
.vela-icon svg { width: 100%; height: 100%; }

/* ── side panel ── */
.vela-panel[hidden] { display: none !important; }
.vela-panel {
    position: relative;
    width: var(--vela-panel-w, 280px);
    flex: none;
    border-inline-start: 1px solid var(--vela-border);
    display: flex;
    flex-direction: column;
    color: var(--vela-fg);
    font-size: 13px;
    box-sizing: border-box;
    background: var(--vela-bg);
}
.vela-panel-header[hidden] { display: none; }
.vela-panel-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 8px 10px 14px;
    border-bottom: 1px solid var(--vela-border);
    font-size: 14px;
    font-weight: 600;
    color: var(--vela-fg-bright);
}
.vela-panel-title { flex: none; }
.vela-panel-title:empty { display: none; }
.vela-panel-header-slot { flex: 1 1 auto; min-width: 0; display: flex; align-items: center; gap: 6px; }
.vela-panel-close, .vela-panel-pin, .vela-panel-max {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    width: 26px;
    height: 26px;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--vela-radius-sm);
    color: var(--vela-fg-muted);
}
.vela-panel-close { font-size: 16px; }
.vela-panel-close .vela-icon { width: 16px; height: 16px; }
.vela-panel-pin .vela-icon, .vela-panel-max .vela-icon { width: 15px; height: 15px; }
.vela-panel-close:hover, .vela-panel-pin:hover, .vela-panel-max:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-panel-pin[data-on='1'] { color: var(--vela-fg-bright); }
.vela-panel[data-maximized] {
    position: absolute;
    inset: 0;
    width: auto;
    max-width: none;
    z-index: 26;
    border-inline-start: none;
}
.vela-panel[data-maximized] .vela-panel-resizer,
.vela-panel[data-maximized] .vela-panel-pin { display: none; }
.vela-panel-body { flex: 1; overflow: auto; padding: 8px; }
.vela-panel-resizer {
    position: absolute;
    top: 0;
    bottom: 0;
    inset-inline-start: -3px;
    width: 7px;
    cursor: col-resize;
    touch-action: none;
    z-index: 1;
}
.vela-panel-resizer::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    inset-inline-start: 3px;
    width: 1px;
    background: transparent;
}
.vela-panel-resizer:hover::after,
.vela-panel-resizer[data-dragging]::after { background: var(--vela-separator-hover-line); }
.vela-panel[data-overlay] {
    position: absolute;
    top: 0;
    inset-inline-end: 0;
    bottom: 0;
    max-width: 100%;
    z-index: 25;
    box-shadow: var(--vela-shadow);
}
[data-layout='mobile'] .vela-panel {
    position: absolute;
    inset: 0;
    width: auto;
    z-index: 25;
    border-inline-start: none;
}
[data-layout='mobile'] .vela-panel-resizer { display: none; }
[data-layout='mobile'] .vela-panel-max { display: none; }
[data-vela-regions] > .vela-ws-column:has(> .vela-panel[data-maximized]) { grid-column: 1 / 4; z-index: 26; }

/* ── data window ── */
.vela-dw-group {
    padding: 10px 8px 4px;
    margin-top: 4px;
    border-top: 1px solid var(--vela-border);
    color: var(--vela-fg-muted);
    font-size: 10px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.08em;
}
.vela-dw-group:first-child { border-top: none; margin-top: 0; padding-top: 8px; }
.vela-dw-group, .vela-dw-row { user-select: text; -webkit-user-select: text; cursor: text; }
.vela-dw-row { display: flex; align-items: center; gap: 12px; padding: 4px 8px; border-radius: var(--vela-radius-sm); }
.vela-dw-row:hover { background: var(--vela-hover); }
.vela-dw-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--vela-fg-bright); }
.vela-dw-value { margin-inline-start: auto; font-variant-numeric: tabular-nums; white-space: nowrap; unicode-bidi: isolate; }
.vela-dw-empty { padding: 20px 10px; text-align: center; color: var(--vela-fg-muted); font-size: 12px; }

/* ── object tree ── */
.vela-ot-pane { border: 1px solid transparent; border-radius: var(--vela-radius-md); margin: 2px 0; }
.vela-ot-panehead { display: flex; align-items: center; gap: 2px; padding: 5px 6px; }
.vela-ot-panename {
    flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    color: var(--vela-fg-muted); font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.08em;
}
.vela-ot-panesep { height: 1px; background: var(--vela-border); margin: 6px 8px; }
.vela-ot-row { display: flex; align-items: center; gap: 9px; padding: 6px 6px; border-radius: var(--vela-radius-md); cursor: default; }
.vela-ot-row:hover { background: var(--vela-hover); }
.vela-ot-row > .vela-icon { color: var(--vela-fg-muted); width: 14px; height: 14px; font-size: 14px; justify-content: center; flex: none; }
.vela-ot-row > .vela-icon svg { width: 14px; height: 14px; }
.vela-ot-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-ot-row[data-hidden] .vela-ot-name,
.vela-ot-row[data-hidden] > .vela-icon { opacity: 0.45; }
.vela-ot-row[data-picked] { background: var(--vela-active); }
.vela-ot-row[data-picked] .vela-ot-name { color: var(--vela-fg-bright); }
.vela-ot-row[data-selected] { box-shadow: inset 2px 0 0 var(--vela-accent); }
.vela-ot-avatar {
    width: 14px; height: 14px; border-radius: 50%; flex: none; display: inline-flex; align-items: center; justify-content: center;
    color: var(--vela-fg-on-fill); font-size: 10px; font-weight: 700; background: var(--vela-accent);
}
.vela-ot-tag {
    flex: none; font-size: 9px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em;
    color: var(--vela-fg-muted); background: var(--vela-hover); border-radius: 3px; padding: 1px 4px;
}
.vela-ot-acts { display: flex; align-items: center; gap: 0; flex: none; }
.vela-ot-btn {
    all: unset; box-sizing: border-box; cursor: pointer; flex: none; width: 20px; height: 20px;
    display: inline-flex; align-items: center; justify-content: center; border-radius: 3px;
    color: var(--vela-fg-muted); font-size: 12px; visibility: hidden;
}
.vela-ot-row:hover .vela-ot-btn,
.vela-ot-panehead:hover .vela-ot-btn { visibility: visible; }
.vela-ot-row .vela-ot-btn[data-engaged] { visibility: visible; color: var(--vela-fg); }
.vela-ot-btn:hover:not(:disabled) { background: var(--vela-active); color: var(--vela-fg-bright); }
.vela-ot-btn:disabled { opacity: 0.35; cursor: default; }
.vela-ot-empty { padding: 20px 10px; text-align: center; color: var(--vela-fg-muted); font-size: 12px; }
.vela-ot-unit { border-radius: var(--vela-radius-md); }
.vela-ot-unit:has(> .vela-ot-row[data-picked]) + .vela-ot-unit > .vela-ot-row[data-picked] { border-top-left-radius: 0; border-top-right-radius: 0; }
.vela-ot-unit:has(+ .vela-ot-unit > .vela-ot-row[data-picked]) > .vela-ot-row[data-picked] { border-bottom-left-radius: 0; border-bottom-right-radius: 0; }
.vela-ot-row[data-row-kind='group'] > .vela-icon { width: 12px; font-size: 11px; }
.vela-ot-subrow { padding-inline-start: 26px; }
.vela-ot-grouphint {
    margin-block: 2px 4px; margin-inline: 26px 6px; padding: 4px 8px; border: 1px dashed var(--vela-border);
    border-radius: var(--vela-radius-sm); color: var(--vela-fg-muted); font-size: 11px;
}
.vela-ot-rename {
    flex: 1; min-width: 0; padding: 1px 4px; border: 1px solid var(--vela-accent); border-radius: var(--vela-radius-sm);
    background: var(--vela-surface-elev); color: var(--vela-fg-bright); font: inherit;
}
.vela-ot-selbar {
    position: sticky; top: -8px; z-index: 2; display: flex; align-items: center; gap: 2px; margin: -8px -8px 6px;
    padding: 6px 8px; border-bottom: 1px solid var(--vela-border); background: var(--vela-bg);
}
.vela-ot-selcount { flex: 1; min-width: 0; color: var(--vela-fg-muted); font-size: 11px; }
.vela-ot-selbar .vela-ot-btn { visibility: visible; width: 24px; height: 22px; }
.vela-ot-selbar .vela-ot-btn[data-icon='group'] .vela-icon { width: 16px; height: 16px; font-size: 16px; }
.vela-ot-row[data-drag] { cursor: grab; }
.vela-ot .vela-panel-body[data-dragging] .vela-ot-row[data-drag] { cursor: grabbing; }
.vela-ot-row[data-source] { opacity: 0.4; }
.vela-ot .vela-panel-body[data-dragging] .vela-ot-btn { visibility: hidden; }
.vela-ot-gap { position: relative; height: 11px; border-radius: 3px; margin: 0 8px; }
.vela-ot-gap::before {
    content: ''; position: absolute; inset-inline-start: 0; inset-inline-end: 0; top: 50%; height: 1px;
    background: var(--vela-border); transform: translateY(-50%);
}
.vela-ot-gap[data-drop] { height: 4px; background: var(--vela-fg-bright); }
.vela-ot-gap[data-drop]::before { display: none; }
.vela-ot [data-drop='target'] { border-color: var(--vela-fg-bright); background: var(--vela-hover); }
.vela-ot-unit[data-drop='target'] { outline: 1px solid var(--vela-fg-bright); outline-offset: -1px; }
.vela-ot [data-drop='before'] { box-shadow: inset 0 2px 0 var(--vela-fg-bright); }
.vela-ot [data-drop='after'] { box-shadow: inset 0 -2px 0 var(--vela-fg-bright); }
.vela-ot-ghost {
    position: fixed; z-index: 9999; pointer-events: none; max-width: 220px; padding: 3px 10px;
    border: 1px solid var(--vela-fg-bright); border-radius: var(--vela-radius-sm); background: var(--vela-surface-overlay);
    color: var(--vela-fg); box-shadow: var(--vela-shadow); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}

/* ── menus (the kit's dropdown / context menu list) ── */
.vela-smenu {
    position: absolute; z-index: var(--vela-z-menu, 50); min-width: 180px; max-width: 320px; box-sizing: border-box;
    padding: 4px; border: 1px solid var(--vela-border); border-radius: var(--vela-radius-md);
    background: var(--vela-surface-elev); color: var(--vela-fg); box-shadow: var(--vela-shadow); font-size: 12px; user-select: none;
}
.vela-smenu-sep { height: 1px; margin: 4px -4px; background: var(--vela-border); }
.vela-smenu-item {
    all: unset; box-sizing: border-box; display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 8px;
    border-radius: var(--vela-radius-sm); cursor: pointer; white-space: nowrap;
}
.vela-smenu-item:hover, .vela-smenu-item:focus-visible, .vela-smenu-item[data-open] { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-smenu-item[aria-disabled='true'] { opacity: 0.45; cursor: default; }
.vela-smenu-icon { width: 14px; height: 14px; flex: none; color: var(--vela-fg-muted); font-size: 14px; }
.vela-smenu-check { width: 14px; flex: none; font-size: 14px; }
.vela-smenu-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.vela-smenu-hint { margin-inline-start: auto; color: var(--vela-fg-muted); font-size: 11px; }
.vela-smenu-sub { position: relative; }
.vela-smenu-sub > .vela-smenu { position: absolute; inset-inline-start: 100%; top: -5px; }

/* the sidebar replaces the topbar's panel group on the desktop layout */
.vela-workspace[data-vela-sidebar]:not([data-layout='mobile']) .vela-widget-panels { display: none; }

/* ── shortcuts panel ── */
.vela-sc-backdrop { position: absolute; inset: 0; z-index: var(--vela-z-dialog, 40); background: var(--vela-backdrop); display: flex; align-items: center; justify-content: center; }
.vela-sc {
    box-sizing: border-box; width: min(520px, calc(100% - 24px)); max-height: calc(100% - 24px); display: flex; flex-direction: column;
    border: 1px solid var(--vela-border); border-radius: var(--vela-radius-lg); background: var(--vela-surface-elev);
    color: var(--vela-fg); box-shadow: var(--vela-shadow-dialog, var(--vela-shadow)); font-size: 13px;
}
.vela-sc-head { display: flex; align-items: center; gap: 8px; padding: 12px 14px; border-bottom: 1px solid var(--vela-border); font-size: 14px; font-weight: 600; color: var(--vela-fg-bright); }
.vela-sc-title { flex: 1; }
.vela-sc-close { all: unset; cursor: pointer; width: 26px; height: 26px; display: inline-flex; align-items: center; justify-content: center; border-radius: var(--vela-radius-sm); color: var(--vela-fg-muted); font-size: 16px; }
.vela-sc-close:hover { background: var(--vela-hover); color: var(--vela-fg-bright); }
.vela-sc-body { overflow: auto; padding: 8px 14px 14px; }
.vela-sh-cat { margin: 12px 0 4px; color: var(--vela-fg-muted); font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.08em; }
.vela-sh-row { display: flex; align-items: center; gap: 12px; padding: 5px 0; }
.vela-sh-label { flex: 1; min-width: 0; }
.vela-sh-keys { display: inline-flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
.vela-sh-static { color: var(--vela-fg-muted); font-size: var(--vela-font-size-sm); margin-top: var(--vela-space-2); }
.vela-sh-key { padding: 1px 6px; border: 1px solid var(--vela-border); border-radius: 4px; background: var(--vela-hover); font-size: 11px; font-family: inherit; }
"#;

const MOBILE: &str = r#"
/* ── mobile bar (and the flips that hide the desktop bars) ── */
.vela-mobilebar {
    display: none;
    align-items: stretch;
    gap: 2px;
    padding: 4px 6px calc(4px + env(safe-area-inset-bottom, 0px));
    border-top: 1px solid var(--vela-border);
    color: var(--vela-fg-muted);
    flex: none;
}
[data-layout='mobile'] .vela-mobilebar { display: flex; }
[data-layout='mobile'] .vela-widget-topbar { display: none; }
[data-layout='mobile'] .vela-widget-bottombar { display: none; }
.vela-mb-item {
    all: unset; box-sizing: border-box; flex: 1 1 0; min-width: 0; min-height: 44px; display: inline-flex;
    align-items: center; justify-content: center; gap: 6px; border-radius: var(--vela-radius-md); cursor: pointer;
    color: var(--vela-fg-bright); font-size: 13px; font-weight: 600; -webkit-tap-highlight-color: transparent;
}
.vela-mb-item[hidden] { display: none !important; }
.vela-mb-item:active { background: var(--vela-hover); }
.vela-mb-item .vela-icon { font-size: 18px; width: 18px; height: 18px; }
.vela-mb-item.vela-mb-on, .vela-mb-item.vela-mb-on:active { background: var(--vela-selected-bg); color: var(--vela-selected-fg); }
.vela-mb-symbol {
    flex: 1.6 1 0; font-size: 14px; letter-spacing: 0.3px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
    display: block; line-height: 44px; text-align: center;
}

/* ── the bottom sheet (the kit's drawer) ── */
.vela-sheet-backdrop { position: absolute; inset: 0; z-index: var(--vela-z-dialog, 40); background: var(--vela-backdrop); display: flex; align-items: flex-end; }
.vela-sheet {
    box-sizing: border-box; width: 100%; max-height: 80%; display: flex; flex-direction: column; outline: none;
    border-radius: 14px 14px 0 0; background: var(--vela-surface); color: var(--vela-fg);
    border-top: 1px solid var(--vela-border); box-shadow: var(--vela-shadow-dialog, var(--vela-shadow));
    padding: 0 12px calc(12px + env(safe-area-inset-bottom, 0px));
}
.vela-sheet-grab { flex: none; align-self: center; width: 36px; height: 4px; margin: 8px 0; border-radius: 2px; background: var(--vela-border-strong); }
.vela-sheet-title { flex: none; padding: 0 16px 10px; font-size: 15px; font-weight: 600; letter-spacing: 0.2px; color: var(--vela-fg-bright); user-select: none; }
.vela-sheet-title:empty { display: none; }
.vela-sheet-body { flex: 1 1 auto; overflow-y: auto; touch-action: pan-y; }

/* ── the three-dots drawer ── */
.vela-md-actions { display: flex; gap: 6px; padding: 2px 2px 10px; border-bottom: 1px solid var(--vela-border); }
.vela-md-action {
    all: unset; box-sizing: border-box; flex: 1 1 0; min-height: 48px; display: inline-flex; flex-direction: column;
    align-items: center; justify-content: center; gap: 4px; border-radius: 8px; font-size: 11px; color: var(--vela-fg);
    cursor: pointer; -webkit-tap-highlight-color: transparent;
}
.vela-md-action:active { background: var(--vela-hover); }
.vela-md-action:disabled { opacity: 0.35; cursor: default; }
.vela-md-action .vela-icon { font-size: 17px; width: 17px; height: 17px; }
.vela-md-list { padding: 6px 0 4px; }
.vela-md-row {
    display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 0 2px; border-radius: 8px; cursor: pointer;
    -webkit-tap-highlight-color: transparent;
}
.vela-md-row:active { background: var(--vela-hover); }
.vela-md-row[data-checked='1'] { background: var(--vela-hover-strong); }
.vela-md-row[data-disabled] { opacity: 0.4; cursor: default; }
.vela-md-row[data-disabled]:active { background: transparent; }
.vela-md-row .vela-icon { flex: none; font-size: 17px; width: 17px; height: 17px; color: var(--vela-fg-muted); }
.vela-md-row-label { flex: 1 1 auto; min-width: 0; font-size: 14px; color: var(--vela-fg-bright); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-md-row-value { flex: none; font-size: 13px; color: var(--vela-fg-muted); }
.vela-md-back {
    all: unset; display: flex; align-items: center; gap: 8px; min-height: 42px; padding: 0 2px; font-size: 14px; font-weight: 600;
    color: var(--vela-fg-bright); cursor: pointer; border-bottom: 1px solid var(--vela-border); width: 100%; box-sizing: border-box;
    -webkit-tap-highlight-color: transparent;
}
.vela-md-gridwrap { display: flex; justify-content: center; padding: 14px 0 10px; }
.vela-lp-grid { display: grid; grid-template-columns: repeat(4, 24px); grid-auto-rows: 24px; gap: 4px; }
.vela-md-gridwrap .vela-lp-grid { grid-template-columns: repeat(4, 44px); grid-auto-rows: 44px; gap: 8px; }
.vela-lp-sq { all: unset; box-sizing: border-box; border-radius: var(--vela-radius-sm); background: var(--vela-hover); border: 1px solid var(--vela-border-faint); cursor: pointer; }
.vela-lp-sq[data-on='1'] { background: var(--vela-selected-bg); border-color: var(--vela-selected-bg); }
.vela-md-section { padding: 12px 2px 4px; font-size: 12px; font-weight: 600; letter-spacing: 0.4px; text-transform: uppercase; color: var(--vela-fg-muted); }
.vela-md-empty { padding: 18px 2px; color: var(--vela-fg-muted); font-size: 13px; }

/* ── the drawings drawer ── */
.vela-dd-sticky { position: sticky; top: 0; z-index: 1; background: var(--vela-surface); padding-top: 2px; }
.vela-dd-section { padding: 10px 2px 6px; font-size: 12px; font-weight: 600; letter-spacing: 0.4px; text-transform: uppercase; color: var(--vela-fg-muted); }
.vela-psd-list { padding: 2px 0 4px; }
.vela-psd-row { display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 0 2px; border-radius: 8px; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.vela-psd-row:active { background: var(--vela-hover); }
.vela-psd-row[data-sep='1'] { margin-top: 6px; padding-top: 6px; border-top: 1px solid var(--vela-border); border-radius: 0 0 8px 8px; }
.vela-psd-row-label { flex: 1 1 auto; min-width: 0; font-size: 14px; color: var(--vela-fg-bright); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-psd-row .vela-icon { flex: none; color: var(--vela-fg-bright); }
.vela-psd-section { padding: 12px 2px 4px; font-size: 12px; font-weight: 600; letter-spacing: 0.4px; text-transform: uppercase; color: var(--vela-fg-muted); }
.vela-dd-search { box-sizing: border-box; width: calc(100% - 4px); margin: 2px 2px 8px; padding: 8px 12px; border: 1px solid var(--vela-border); border-radius: 8px; color: var(--vela-fg-bright); background: var(--vela-surface); font: inherit; font-size: 14px; }
.vela-dd-tabs { display: flex; gap: 4px; overflow-x: auto; scrollbar-width: none; touch-action: pan-x; padding: 0 2px 8px; border-bottom: 1px solid var(--vela-border); }
.vela-dd-tab { all: unset; flex: none; padding: 6px 10px; border-radius: 999px; font-size: 13px; cursor: pointer; color: var(--vela-fg-muted); }
.vela-dd-tab[aria-selected='true'] { background: var(--vela-selected-bg); color: var(--vela-selected-fg); }
.vela-dd-list { padding: 6px 0 4px; }
.vela-dd-row { display: flex; align-items: center; gap: 12px; min-height: 44px; padding: 0 2px; border-radius: 8px; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.vela-dd-row:active, .vela-dd-row[data-active='1'] { background: var(--vela-hover); }
.vela-dd-row[data-active='1'] .vela-dd-label { color: var(--vela-accent); }
.vela-dd-row > .vela-icon { flex: none; width: 24px; height: 24px; color: var(--vela-fg); }
.vela-dd-row > .vela-icon svg { width: 24px; height: 24px; }
.vela-dd-label { flex: 1 1 auto; min-width: 0; font-size: 14px; color: var(--vela-fg-bright); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-dd-star { all: unset; box-sizing: border-box; flex: none; width: 40px; height: 40px; display: inline-flex; align-items: center; justify-content: center; border-radius: 8px; color: var(--vela-fg-muted); cursor: pointer; }
.vela-dd-star[aria-pressed='true'] { color: var(--vela-highlight); }
.vela-dd-empty { padding: 18px 2px; color: var(--vela-fg-muted); font-size: 13px; }

/* ── the drawing pill ── */
.vela-drawpill { display: none; }
[data-layout='mobile'] .vela-drawpill {
    position: absolute; inset-inline-start: 50%; bottom: 10px; transform: translateX(-50%); z-index: 7; display: flex;
    align-items: center; gap: 2px; padding: 4px; border: 1px solid var(--vela-border-strong); border-radius: 999px;
    background: var(--vela-surface); box-shadow: var(--vela-shadow-dialog, var(--vela-shadow)); color: var(--vela-fg);
}
[data-layout='mobile'] .vela-drawpill[hidden] { display: none !important; }
.vela-drawpill-tool { display: inline-flex; align-items: center; justify-content: center; width: 36px; height: 36px; border-radius: 999px; background: var(--vela-hover); color: var(--vela-accent); }
.vela-drawpill-tool svg { width: 22px; height: 22px; }
.vela-drawpill-btn { all: unset; box-sizing: border-box; width: 36px; height: 36px; display: inline-flex; align-items: center; justify-content: center; border-radius: 999px; color: var(--vela-fg-muted); cursor: pointer; position: relative; }
.vela-drawpill-btn:active { background: var(--vela-hover); }
.vela-drawpill-btn[data-on='1'] { color: var(--vela-accent); background: var(--vela-hover); }
.vela-drawpill-btn .vela-icon { font-size: 17px; width: 17px; height: 17px; }
.vela-drawpill-badge { position: absolute; top: 5px; inset-inline-end: 5px; display: none; align-items: flex-end; gap: 1px; color: var(--vela-accent); }
.vela-drawpill-badge[data-strength] { display: inline-flex; }
.vela-drawpill-badge > i { display: block; width: 2px; height: 4px; border-radius: 1px; background: currentColor; }
.vela-drawpill-badge > i + i { height: 7px; opacity: 0.3; }
.vela-drawpill-badge[data-strength='strong'] > i + i { opacity: 1; }
"#;

/// The whole stylesheet text.
pub fn text() -> String {
    format!("@layer vela.chrome {{\n{DESKTOP}\n{MOBILE}\n}}")
}

/// Inject the stylesheet once per document.
pub fn ensure_styles(doc: &Document) {
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let style = create_element(doc, "style");
    style.set_id(STYLE_ID);
    style.set_text_content(Some(&text()));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    } else if let Some(root) = doc.document_element() {
        let _ = root.append_child(&style);
    }
}
