//! The stylesheet of the dialogs and the gear: the original's `vela-settings-controls` and
//! `vela-ind-dialog-styles` sheets plus the layout the `vela-ui` kit components (dialog,
//! field, tabs, checkbox, select, number input, color field) give them, re-expressed over the
//! `--vela-chrome-*` custom properties the host writes on the wrapper. Layered like the
//! host's own rules, so a host stylesheet can override any of them.

use web_sys::Document;

use crate::dom::create_element;

const STYLE_ID: &str = "vela-dialogs-styles";

const CSS: &str = r#"@layer vela.chrome {

.vela-gear, .vela-dlg-positioner, .vela-dsp-scope {
    --dlg-fg: var(--vela-chrome-text-color, #b2b5be);
    --dlg-bg: var(--vela-chrome-background, #151619);
    --dlg-border: var(--vela-chrome-border-color, #2a2b30);
    --dlg-bright: color-mix(in srgb, var(--dlg-fg) 100%, white 12%);
    --dlg-muted: color-mix(in srgb, var(--dlg-fg) 66%, transparent);
    --dlg-strong: color-mix(in srgb, var(--dlg-fg) 28%, transparent);
    --dlg-hover: color-mix(in srgb, var(--dlg-fg) 11%, transparent);
    --dlg-hover-strong: color-mix(in srgb, var(--dlg-fg) 16%, transparent);
    --dlg-active: color-mix(in srgb, var(--dlg-fg) 20%, transparent);
    --dlg-sunken: color-mix(in srgb, var(--dlg-fg) 6%, var(--dlg-bg));
    --dlg-focus: color-mix(in srgb, var(--dlg-fg) 70%, transparent);
    --dlg-danger: #f23645;
    --dlg-radius: 6px;
    font-family: var(--vela-chrome-font-family, sans-serif);
    box-sizing: border-box;
}

/* ── the gear ── */
.vela-gear { position: absolute; z-index: 6; width: 26px; height: 26px; padding: 0; display: flex; align-items: center; justify-content: center; cursor: pointer; border-radius: 50%; border: 1px solid var(--dlg-border); background: var(--dlg-bg); color: var(--dlg-fg); opacity: .75; pointer-events: auto; font-size: 14px; line-height: 1; bottom: calc(var(--vela-bottom-gutter, 32px) + 10px); right: calc(var(--vela-scale-gutter, 56px) + 10px); transition: opacity .12s ease; }
.vela-gear:hover, .vela-gear:focus-visible, .vela-gear[aria-expanded='true'] { opacity: 1; }
.vela-gear:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: 1px; }

/* ── the frame (the kit's dialog) ── */
.vela-dlg-positioner { position: absolute; inset: 0; z-index: 30; display: flex; justify-content: center; align-items: flex-start; padding-top: 8vh; pointer-events: auto; cursor: default; user-select: none; -webkit-user-select: none; }
.vela-dlg-positioner[data-align='center'] { align-items: center; padding-top: 0; }
.vela-dlg-panel { position: relative; background: var(--dlg-bg); color: var(--dlg-fg); border: 1px solid var(--dlg-strong); border-radius: 8px; box-shadow: 0 10px 32px rgba(0,0,0,.45); font-size: 13px; display: flex; flex-direction: column; overflow: hidden; outline: none; max-height: 70vh; user-select: text; -webkit-user-select: text; animation: vela-dlg-in .18s ease; }
.vela-dlg-panel[data-variant='settings'] { width: fit-content; min-width: min(560px, 94%); max-width: min(720px, 94%); }
.vela-dlg-panel[data-variant='form'] { width: fit-content; min-width: min(380px, 90%); max-width: min(640px, 94%); max-height: 82%; font-size: 14px; }
.vela-dlg-header { display: flex; align-items: center; padding: 9px 9px 9px 16px; border-bottom: 1px solid var(--dlg-border); user-select: none; -webkit-user-select: none; cursor: grab; }
.vela-dlg-panel[data-variant='form'] .vela-dlg-header { align-items: flex-start; padding: 16px 20px 20px; border-bottom: none; }
.vela-dlg-title { flex: 1; margin: 0; font-size: 17px; font-weight: 600; letter-spacing: .2px; color: var(--dlg-bright); }
.vela-dlg-panel[data-variant='form'] .vela-dlg-title { font-size: 20px; line-height: 28px; }
.vela-dlg-close, .vela-dlg-burger { cursor: pointer; display: inline-flex; align-items: center; justify-content: center; background: transparent; border: none; color: var(--dlg-muted); line-height: 0; width: 30px; height: 30px; padding: 0; border-radius: 4px; transition: background .12s ease, color .12s ease; }
.vela-dlg-close:hover, .vela-dlg-burger:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-dlg-panel[data-variant='form'] .vela-dlg-close:hover { color: var(--dlg-danger); }
.vela-dlg-burger { display: none; margin-inline-end: 2px; }
.vela-dlg-body { padding: 0; overflow: hidden; display: flex; flex-direction: column; min-height: 0; flex: 1 1 auto; }
.vela-dlg-footer { flex: 0 0 auto; display: flex; align-items: center; gap: 8px; }
.vela-dlg-panel[data-variant='settings'] .vela-dlg-footer { padding: 10px 14px; justify-content: flex-start; }
.vela-dlg-panel[data-variant='form'] .vela-dlg-footer { padding: 16px 20px; justify-content: flex-end; gap: 12px; border-top: 1px solid var(--dlg-border); }
.vela-dlg-btn { height: 30px; padding: 0 14px; font-size: 13px; color: var(--dlg-fg); background: var(--dlg-sunken); border: 1px solid var(--dlg-border); border-radius: var(--dlg-radius); cursor: pointer; font-family: inherit; transition: background .12s ease, border-color .12s ease, color .12s ease; }
.vela-dlg-btn:hover { background: var(--dlg-hover); border-color: var(--dlg-strong); color: var(--dlg-bright); }
.vela-dlg-btn[data-primary] { background: var(--dlg-fg); color: var(--dlg-bg); border-color: var(--dlg-fg); }
.vela-dlg-btn[data-primary]:hover { background: var(--dlg-bright); color: var(--dlg-bg); }
.vela-dlg-btn[data-start] { margin-inline-end: auto; }
.vela-dlg-panel button:focus-visible, .vela-dlg-panel select:focus-visible, .vela-dlg-panel textarea:focus-visible, .vela-dlg-panel input:focus-visible { outline: 2px solid var(--dlg-focus); outline-offset: 1px; }
@keyframes vela-dlg-in { from { opacity: 0; transform: translateY(6px) scale(.98); } to { opacity: 1; transform: translateY(0) scale(1); } }
.vela-dlg-hide, .vela-sd-hide { display: none !important; }

/* ── settings: tab rail, panes, field grid ── */
.vela-sd-shell { display: flex; min-height: 360px; max-height: calc(70vh - 100px); flex: 1 1 auto; position: relative; }
.vela-sd-rail { flex: 0 0 170px; display: flex; flex-direction: column; gap: 2px; padding: 10px 8px; border-inline-end: 1px solid var(--dlg-border); overflow-y: auto; box-sizing: border-box; }
.vela-sd-tab { text-align: start; padding: 9px 12px; background: transparent; border: none; border-radius: 6px; color: var(--dlg-muted); font-weight: 600; font-size: 13px; font-family: inherit; cursor: pointer; transition: background .12s ease, color .12s ease; }
.vela-sd-tab:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-sd-tab[aria-selected='true'] { background: var(--dlg-active); color: var(--dlg-bright); }
.vela-sd-tab-sub { padding-inline-start: 28px; font-size: 12px; }
.vela-sd-pane { flex: 1; overflow-y: auto; overflow-anchor: none; padding: 6px 18px 14px; min-width: 0; }
.vela-sd-grid { display: grid; grid-template-columns: max-content max-content; column-gap: 12px; row-gap: 16px; align-items: center; align-content: start; }
.vela-sd-grp, .vela-sd-row { display: contents; }
.vela-sd-title { grid-column: 1 / -1; margin: 24px 0 0; padding-bottom: 8px; font-size: 12px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--dlg-muted); }
.vela-sd-grid > .vela-sd-title:first-child, .vela-sd-grid > .vela-sd-grp:first-child > .vela-sd-title:first-child { margin-top: 12px; }
.vela-sd-sep { grid-column: 1 / -1; height: 4px; }
.vela-sd-label { opacity: .85; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.vela-sd-cell { display: flex; align-items: center; gap: 8px; justify-self: start; }
.vela-sd-cell[data-off] > * { opacity: .4; pointer-events: none; }
.vela-sd-bool { grid-column: 1 / -1; display: flex; align-items: center; gap: 8px; min-height: 22px; }
.vela-sd-tog { display: flex; align-items: center; gap: 8px; min-height: 22px; white-space: nowrap; }
.vela-sd-soft > * { opacity: .4 !important; pointer-events: none !important; }
.vela-sd-nested > :first-child { margin-inline-start: calc(var(--vela-sd-depth, 1) * 24px); }
.vela-sd-unit, .vela-sd-hint { color: var(--dlg-muted); }
.vela-sd-struct { display: flex; align-items: flex-start; padding-top: 14px; }
.vela-sd-toc { position: sticky; top: 0; flex: 0 0 auto; min-width: 104px; display: flex; flex-direction: column; gap: 2px; padding: 2px 14px 0 0; }
.vela-sd-struct > [data-sd-rows] { border-inline-start: 1px solid var(--dlg-border); padding-inline-start: 18px; flex: 1 1 auto; min-width: 0; }
.vela-sd-struct.no-toc > [data-sd-rows] { border-inline-start: none; padding-inline-start: 0; }
.vela-sd-toc-btn { text-align: start; padding: 6px 10px; background: transparent; border: none; border-radius: 4px; color: var(--dlg-muted); font-family: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
.vela-sd-toc-btn:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-sd-toc-btn[aria-pressed='true'] { background: var(--dlg-active); color: var(--dlg-bright); }
.vela-sd-itabs { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin: 14px 0 0; padding-bottom: 12px; border-bottom: 1px solid var(--dlg-border); }
.vela-sd-itab { display: inline-flex; align-items: center; gap: 7px; height: 30px; padding: 0 11px; background: transparent; border: 1px solid var(--dlg-border); border-radius: 6px; color: var(--dlg-muted); font-family: inherit; font-size: 13px; font-weight: 600; cursor: pointer; }
.vela-sd-itab:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-sd-itab[aria-selected='true'] { background: var(--dlg-active); color: var(--dlg-bright); border-color: var(--dlg-strong); }
.vela-sd-itab-add { border-style: dashed; min-width: 30px; justify-content: center; padding: 0; }
.vela-sd-ix { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; margin-inline-end: -4px; border-radius: 4px; color: var(--dlg-muted); font-size: 10px; line-height: 1; background: transparent; border: none; padding: 0; cursor: pointer; }
.vela-sd-ix:hover { background: var(--dlg-hover); color: var(--dlg-bright); }
.vela-sd-railscrim { display: none; }

/* ── controls (the kit's checkbox, number input, select, color field) ── */
.vela-ctl-check { width: 16px; height: 16px; margin: 0; flex: none; accent-color: var(--dlg-fg); cursor: pointer; }
.vela-ctl-switch { appearance: none; -webkit-appearance: none; position: relative; width: 30px; height: 18px; margin: 0; flex: none; border-radius: 9px; background: var(--dlg-strong); cursor: pointer; transition: background .12s ease; }
.vela-ctl-switch::after { content: ''; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--dlg-bg); transition: transform .12s ease; }
.vela-ctl-switch:checked { background: var(--dlg-fg); }
.vela-ctl-switch:checked::after { transform: translateX(12px); }
.vela-ctl-num, .vela-ctl-text, .vela-ctl-select, .vela-ctl-textarea, .vela-ctl-trigger { box-sizing: border-box; height: 30px; background: transparent; border: 1px solid var(--dlg-strong); color: var(--dlg-bright); border-radius: var(--dlg-radius); padding: 0 8px; font-size: 13px; font-family: inherit; }
.vela-ctl-num { width: 100px; }
.vela-ctl-num[data-compact] { width: 80px; }
.vela-ctl-select { min-width: 100px; max-width: min(260px, 55vw); cursor: pointer; padding-inline-end: 4px; }
.vela-ctl-select option { color: initial; }
.vela-ctl-num:hover, .vela-ctl-text:hover, .vela-ctl-select:hover, .vela-ctl-textarea:hover, .vela-ctl-trigger:hover { border-color: var(--dlg-muted); }
.vela-ind-body .vela-ctl-num, .vela-ind-body .vela-ctl-text, .vela-ind-body .vela-ctl-select, .vela-ind-body .vela-ctl-trigger { height: 34px; font-size: 14px; }
.vela-ind-body .vela-ctl-num, .vela-ind-body .vela-ctl-text { width: 100%; min-width: 100px; }
.vela-ctl-textarea { width: 100%; height: auto; min-height: 72px; padding: 6px 8px; resize: vertical; }
.vela-ctl-color { display: inline-flex; align-items: center; gap: 4px; flex: none; }
.vela-ctl-swatch { width: 26px; height: 22px; padding: 0; border: 1px solid var(--dlg-strong); border-radius: 4px; background: transparent; cursor: pointer; }
.vela-ctl-alpha { width: 44px; height: 14px; margin: 0; accent-color: var(--dlg-fg); cursor: pointer; }
.vela-ctl-hint { display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; background: transparent; border: none; border-radius: 50%; color: var(--dlg-muted); cursor: help; line-height: 0; flex: none; }
.vela-ctl-hint:hover { color: var(--dlg-bright); }
a.vela-ctl-hint { text-decoration: none; cursor: pointer; }
.vela-ctl-multi { position: relative; display: inline-block; min-width: 100px; max-width: 100%; }
.vela-ctl-trigger { display: flex; align-items: center; width: 100%; min-width: 0; padding-inline-end: 26px; cursor: pointer; text-align: start; position: relative; }
.vela-ctl-trigger > .vela-ctl-trigger-value { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vela-ctl-trigger > .vela-ctl-trigger-value[data-placeholder] { color: var(--dlg-muted); }
.vela-ctl-trigger > .vela-ctl-trigger-chev { position: absolute; inset-inline-end: 8px; top: 50%; transform: translateY(-50%); display: flex; opacity: .55; line-height: 0; pointer-events: none; }
.vela-ctl-pop { position: absolute; z-index: 5; top: calc(100% + 4px); inset-inline-start: 0; min-width: 100%; box-sizing: border-box; background: var(--dlg-bg); color: var(--dlg-fg); border: 1px solid var(--dlg-strong); border-radius: 6px; box-shadow: 0 6px 20px rgba(0,0,0,.35); padding: 4px; max-height: min(280px, 50vh); overflow-y: auto; font-size: 14px; }
.vela-ctl-pop-row { display: flex; align-items: center; gap: 10px; padding: 0 10px; border-radius: 4px; white-space: nowrap; cursor: pointer; }
.vela-ctl-pop-row:hover { background: var(--dlg-hover); }
.vela-ctl-pop-row label { flex: 1; padding: 7px 10px 7px 0; cursor: inherit; }
.vela-ctl-pop-row[data-locked] { cursor: default; color: var(--dlg-muted); }
.vela-ctl-combo { display: inline-flex; align-items: center; gap: 6px; flex: none; }

/* ── indicator dialog ── */
.vela-ind-tabs { display: flex; padding: 0 20px; flex: 0 0 auto; border-bottom: 1px solid var(--dlg-border); gap: 2px; }
.vela-ind-tab { background: transparent; border: none; border-bottom: 2px solid transparent; margin-bottom: -1px; padding: 8px 12px; color: var(--dlg-muted); font-family: inherit; font-size: 14px; font-weight: 600; cursor: pointer; }
.vela-ind-tab:hover { color: var(--dlg-bright); }
.vela-ind-tab[aria-selected='true'] { color: var(--dlg-bright); border-bottom-color: var(--dlg-fg); }
.vela-ind-tabs[data-single] .vela-ind-tab { cursor: default; }
.vela-ind-body { padding: 16px 20px; overflow-y: auto; overflow-x: hidden; overflow-anchor: none; flex: 1 1 auto; display: grid; grid-template-columns: max-content 1fr; align-items: center; align-content: start; column-gap: 16px; row-gap: 16px; }
.vela-ind-row { display: contents; }
.vela-ind-label { opacity: .9; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 14px; display: flex; align-items: center; gap: 6px; }
.vela-ind-cell { display: flex; align-items: center; gap: 8px; min-width: 0; }
.vela-ind-cell[data-fit] { justify-self: start; }
.vela-ind-cell > .vela-ctl-select, .vela-ind-cell > .vela-ctl-multi { flex: 1 1 auto; }
.vela-ind-bool { grid-column: 1 / -1; display: flex; align-items: center; gap: 8px; min-height: 22px; font-size: 14px; }
.vela-ind-stacked { grid-column: 1 / -1; display: flex; flex-direction: column; gap: 8px; }
.vela-ind-inline { grid-column: 1 / -1; display: flex; flex-wrap: wrap; align-items: center; gap: 12px; }
.vela-ind-inline-item { display: flex; align-items: center; gap: 8px; }
.vela-ind-section { grid-column: 1 / -1; margin: 0; padding: 20px 0 8px; font-size: 12px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--dlg-muted); display: flex; align-items: center; gap: 6px; }
.vela-ind-body > .vela-ind-section:first-child, .vela-ind-body > .vela-ind-grp:first-child > .vela-ind-section:first-child { padding-top: 0; }
.vela-ind-grp { display: contents; }

/* ── mobile: the dialog fills the chart, the rail becomes a sidebar, the TOC top tabs ── */
[data-vela-layout='mobile'] .vela-dlg-positioner { padding: 0; align-items: stretch; }
[data-vela-layout='mobile'] .vela-dlg-panel { width: 100% !important; min-width: 0 !important; max-width: none !important; max-height: none; flex: 1 1 auto; border: none; border-radius: 0; transform: none !important; }
[data-vela-layout='mobile'] .vela-dlg-header { cursor: default; }
[data-vela-layout='mobile'] .vela-dlg-close { width: 40px; height: 40px; }
[data-vela-layout='mobile'] .vela-dlg-burger { display: inline-flex; width: 36px; height: 36px; }
[data-vela-layout='mobile'] .vela-dlg-btn { height: 38px; }
[data-vela-layout='mobile'] .vela-sd-shell { min-height: 0; max-height: none; overflow: hidden; flex: 1 1 auto; }
[data-vela-layout='mobile'] .vela-sd-railscrim { position: absolute; inset: 0; z-index: 2; background: rgba(0,0,0,.45); }
[data-vela-layout='mobile'] .vela-sd-railscrim[data-open] { display: block; }
[data-vela-layout='mobile'] .vela-sd-rail { position: absolute; inset-inline-start: 0; top: 0; bottom: 0; z-index: 3; width: min(240px, 80%); background: var(--dlg-bg); box-shadow: 0 10px 32px rgba(0,0,0,.45); transform: translateX(-105%); transition: transform .2s ease; }
[data-vela-layout='mobile'] .vela-sd-rail[data-open] { transform: translateX(0); }
[data-vela-layout='mobile'] .vela-sd-tab { padding: 12px; }
[data-vela-layout='mobile'] .vela-sd-pane { padding: 6px 14px calc(14px + env(safe-area-inset-bottom, 0px)); }
[data-vela-layout='mobile'] .vela-sd-grid { grid-template-columns: minmax(0, 1fr) max-content; }
[data-vela-layout='mobile'] .vela-sd-label { white-space: normal; }
[data-vela-layout='mobile'] .vela-sd-struct { flex-direction: column; padding-top: 8px; }
[data-vela-layout='mobile'] .vela-sd-toc { flex-direction: row; overflow-x: auto; min-width: 0; width: 100%; box-sizing: border-box; gap: 4px; padding: 2px 0 8px; background: var(--dlg-bg); z-index: 1; }
[data-vela-layout='mobile'] .vela-sd-toc-btn { flex: none; padding: 8px 12px; font-size: 13px; }
[data-vela-layout='mobile'] .vela-sd-struct > [data-sd-rows] { border-inline-start: none; padding-inline-start: 0; width: 100%; }
[data-vela-layout='mobile'] .vela-sd-itabs { flex-wrap: nowrap; overflow-x: auto; }
[data-vela-layout='mobile'] .vela-sd-itab { height: 36px; flex: none; }
[data-vela-layout='mobile'] .vela-ctl-num, [data-vela-layout='mobile'] .vela-ctl-select { height: 34px; }
[data-vela-layout='mobile'] .vela-ind-body { grid-template-columns: minmax(0, 1fr) auto; }

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

#[cfg(test)]
mod tests {
    use super::CSS;

    /// The declarations of the first rule whose selector is exactly `selector`.
    fn rule(selector: &str) -> &'static str {
        CSS.lines()
            .find(|l| l.starts_with(&format!("{selector} {{")))
            .unwrap_or_else(|| panic!("no rule for {selector}"))
    }

    // Ported from `test/inputs-ui-tabs.test.ts` (`inputDialogBodyStyle`): the original builds
    // one tab's grid as an inline style; here it is the `.vela-ind-body` rule, which must keep
    // the same rhythm.
    #[test]
    fn rows_pack_at_the_start_so_a_flex_grown_body_keeps_the_16px_gap() {
        let body = rule(".vela-ind-body");
        assert!(body.contains("align-content: start"));
        assert!(body.contains("row-gap: 16px"));
        assert!(body.contains("column-gap: 16px"));
        assert!(body.contains("flex: 1 1 auto"));
    }

    #[test]
    fn the_desktop_grid_is_a_label_column_and_a_free_column_the_mobile_one_a_flexible_label() {
        assert!(rule(".vela-ind-body").contains("grid-template-columns: max-content 1fr"));
        assert!(
            CSS.contains(
                "[data-vela-layout='mobile'] .vela-ind-body { grid-template-columns: minmax(0, 1fr) auto; }"
            )
        );
    }
}
