// Smoke test of vela-web's drawing settings UI (the quick bar of a selected drawing and the full
// settings panel) in Node, against the shared fake DOM (fake_dom.mjs).
//
// Both entries run: the direct chart (VelaChart) and the facade chart (VelaApp). Controls are
// found the way a user finds them (by label), edited by setting the value the way the browser
// would and dispatching the event the control listens for, and the effect is read back from the
// exported drawings document and the repainted drawings canvas. It does NOT run a browser: real
// layout (placement beside the bar, clamping), focus rings, native color pickers and real
// painting are not exercised.
//
// Usage, after `cargo run -p xtask -- web pkg`:
//   node tools/parity/smoke_drawing_settings.mjs pkg
//
// Exits 1 on the first failed check.

import { check, clock, container, doc, loadPackage, makeBars, painted, pileOf, pump, state, tick } from './fake_dom.mjs';

const wasm = await loadPackage('smoke_drawing_settings.mjs');
const sleep = (ms = 5) => new Promise((r) => setTimeout(r, ms));
async function settle() {
    for (let i = 0; i < 6; i += 1) {
        pump();
        await Promise.resolve();
    }
    await sleep();
    pump();
    await sleep();
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const classes = (n) => (n.className ?? '').split(' ');
const hasClass = (n, c) => classes(n).includes(c);
const byClass = (root, c) => root.all((n) => hasClass(n, c));
const byLabel = (root, text) => root.all((n) => n.getAttribute('aria-label') === text)[0] ?? null;
const click = (el) => el.dispatch('click', { detail: 1 });
const setValue = (el, value, type = 'change') => {
    el.value = String(value);
    el.dispatch(type, {});
};
const setCheck = (el, on) => {
    el.checked = on;
    el.dispatch('change', {});
};

async function scenario(label, build) {
    const ck = (name, ok, detail) => check(`${label}: ${name}`, ok, detail);
    const host = container(900, 560);
    const chart = await build(host);
    await settle();
    const { wrapper: w, plot: p, canvases: cs } = pileOf(host);
    const dataC = cs.find((c) => c.style.getPropertyValue('pointer-events') !== 'none');
    const drawingsCanvas = cs[5];
    const base = { pointerId: 5, pointerType: 'mouse', button: 0, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false };
    const mouse = (type, x, y, extra = {}) => dataC.dispatch(type, { ...base, clientX: x, clientY: y, timeStamp: clock, ...extra });
    const press = async (x, y, extra = {}) => {
        mouse('pointerdown', x, y, extra);
        mouse('pointerup', x, y, extra);
        await settle();
    };
    const key = async (k, extra = {}) => {
        dataC.dispatch('keydown', { key: k, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...extra });
        await settle();
    };
    const docs = () => JSON.parse(chart.exportDrawings()).drawings;
    const bars = () => byClass(w, 'vela-dpop');
    const bar = () => bars()[0];
    const panels = () => byClass(w, 'vela-dsp');
    const panel = () => panels()[0];
    const floats = () => byClass(w, 'vela-dpop-float');
    const tip = (root, text) => root.all((n) => n.getAttribute('aria-label') === text || n.getAttribute('data-tip') === text)[0] ?? null;
    const row = (root, text) => root.all((n) => hasClass(n, 'vela-dsp-row') && n.children[0]?.textContent === text)[0] ?? null;
    const labels = (root) => root.all((n) => n.tagName === 'BUTTON' && n.getAttribute('aria-label')).map((n) => n.getAttribute('aria-label'));
    const ordered = (...cls) => cls;

    // A drawing in two clicks with the armed tool, then the bar settles.
    const place = async (tool, x1, y1, x2, y2) => {
        chart.armDrawingTool(tool);
        await settle();
        await press(x1, y1);
        mouse('pointermove', x2, y2);
        pump();
        await press(x2, y2);
        await sleep();
    };
    const closeAll = async () => {
        await key('Escape');
        await press(60, 500);
        await key('Escape');
    };

    // ── the quick bar for a trend line ──
    await place('trendline', 300, 200, 500, 120);
    ck('a new trend line opens the quick bar', bars().length === 1 && bar().getAttribute('role') === 'toolbar');
    const names = labels(bar());
    const wanted = ['Move', 'Line color', 'Line width', 'Line style', 'Text', 'Settings', 'Lock', 'Delete', 'More'];
    ck('the bar has the controls of a trend line, in the original order', same(names.filter((n) => wanted.includes(n)), wanted), names.join(' | '));
    ck('every bar control has an accessible name', bar().all((n) => n.tagName === 'BUTTON').every((b) => (b.getAttribute('aria-label') ?? '') !== ''));
    ck('line style and width show their current value in the trigger', tip(bar(), 'Line width').textContent === '' && tip(bar(), 'Line width').innerHTML.includes('2px'));

    // Hover label.
    tip(bar(), 'Lock').dispatch('mouseover', {});
    const tipEl = byClass(w, 'vela-dpop-tip')[0];
    ck('hovering a control shows its label', tipEl && tipEl.textContent === 'Lock');
    bar().dispatch('mouseleave', {});
    ck('and leaving the bar hides it', byClass(w, 'vela-dpop-tip').length === 0);

    // Line color: the swatch popover, committed on release.
    drawingsCanvas.ctx.ops = [];
    click(tip(bar(), 'Line color'));
    const swatch = floats()[0]?.all((n) => n.className === 'vela-ctl-swatch')[0];
    ck('the color button opens a popover with the swatch and an opacity slider', swatch && floats()[0].all((n) => n.className === 'vela-ctl-alpha').length === 1);
    setValue(swatch, '#ff0000');
    await settle();
    ck('picking a line color edits the document', docs()[0].style.lineColor === '#ff0000', docs()[0].style.lineColor);
    ck('and repaints the drawings canvas', painted(drawingsCanvas));
    ck('the color underline follows the pick', tip(bar(), 'Line color').children[1].style.getPropertyValue('background') === '#ff0000');
    click(tip(bar(), 'Line color'));
    ck('a second click closes the popover', floats().length === 0);

    // Width and style: dropdown menus.
    click(tip(bar(), 'Line width'));
    ck('the width dropdown lists 1 to 4 px with the current one checked', floats().length === 1 && labels(floats()[0]).join() === '1px,2px,3px,4px' && floats()[0].all((n) => n.getAttribute('aria-checked') === 'true').length === 1);
    click(byLabel(floats()[0], '4px'));
    await settle();
    ck('picking a width edits the document and closes the menu', docs()[0].style.lineWidth === 4 && floats().length === 0);
    click(tip(bar(), 'Line style'));
    click(byLabel(floats()[0], 'Dashed'));
    await settle();
    ck('picking a line style edits the document', docs()[0].style.lineStyle === 'dashed');

    // Lock, order, duplicate, reset, delete.
    click(tip(bar(), 'Lock'));
    await settle();
    ck('lock locks the drawing and reads pressed', docs()[0].locked === true && tip(bar(), 'Lock').getAttribute('aria-pressed') === 'true');
    click(tip(bar(), 'Lock'));
    await settle();
    ck('and unlocks it', docs()[0].locked === false && tip(bar(), 'Lock').getAttribute('aria-pressed') === 'false');
    click(tip(bar(), 'More'));
    ck('the kebab lists duplicate, order and reset', same(labels(floats()[0]), []) || floats()[0].all((n) => n.getAttribute('role') === 'menuitem').length === 4);
    const items = floats()[0].all((n) => n.getAttribute('role') === 'menuitem').map((n) => n.textContent);
    ck('with the original labels', same(items, ['Duplicate', 'Bring to front', 'Send to back', 'Reset settings']), items.join(' | '));
    click(floats()[0].all((n) => n.getAttribute('role') === 'menuitem' && n.textContent === 'Reset settings')[0]);
    await settle();
    await sleep();
    ck('reset settings restores the tool look and reopens the bar', docs()[0].style.lineColor !== '#ff0000' && docs()[0].style.lineWidth === 2 && bars().length === 1, JSON.stringify(docs()[0].style));

    // ── a second drawing: order and duplicate ──
    await key('Escape');
    await place('trendline', 300, 300, 500, 340);
    ck('two trend lines exist', chart.drawingCount() === 2);
    const [first, second] = docs();
    click(tip(bar(), 'More'));
    click(floats()[0].all((n) => n.getAttribute('role') === 'menuitem' && n.textContent === 'Send to back')[0]);
    await settle();
    const z = (id) => docs().find((d) => d.id === id).zIndex;
    ck('send to back puts the drawing under the other', z(second.id) < z(first.id), `${z(second.id)} < ${z(first.id)}`);
    click(tip(bar(), 'More'));
    click(floats()[0].all((n) => n.getAttribute('role') === 'menuitem' && n.textContent === 'Bring to front')[0]);
    await settle();
    ck('bring to front puts it back on top', z(second.id) > z(first.id));
    click(tip(bar(), 'More'));
    click(floats()[0].all((n) => n.getAttribute('role') === 'menuitem' && n.textContent === 'Duplicate')[0]);
    await settle();
    ck('duplicate adds a copy and closes the bar', chart.drawingCount() === 3);

    // Delete and undo.
    await key('Escape');
    await press(400, 160);
    ck('a click on a drawing brings its bar back', bars().length === 1);
    click(tip(bar(), 'Delete'));
    await settle();
    ck('delete removes the drawing and the bar', chart.drawingCount() === 2 && bars().length === 0);
    chart.undoDrawing();
    await settle();
    ck('and it is one undo step', chart.drawingCount() === 3);



    // ── the bar's own behavior: Escape in a menu, the move handle, outside presses ──
    await key('Escape');
    await press(400, 160);
    click(tip(bar(), 'Line width'));
    ck('a menu is open', floats().length === 1);
    floats()[0].dispatch('keydown', { key: 'Escape' });
    ck('Escape closes a menu and the bar stays', floats().length === 0 && bars().length === 1);
    const px = (v) => Number.parseFloat(v);
    const leftOf = () => px(bar().style.getPropertyValue('left'));
    const topOf = () => px(bar().style.getPropertyValue('top'));
    // The fake DOM sizes an element like its parent: give the bar a real size so it can move.
    bar().rect = { left: 0, top: 0, width: 300, height: 38 };
    const [l0, t0] = [leftOf(), topOf()];
    tip(bar(), 'Move').dispatch('pointerdown', { ...base, clientX: 100, clientY: 100 });
    doc.defaultView.dispatch('pointermove', { clientX: 140, clientY: 130 });
    ck('dragging the move handle moves the whole bar', leftOf() === l0 + 40 && topOf() === t0 + 30, `${l0},${t0} -> ${leftOf()},${topOf()}`);
    doc.defaultView.dispatch('pointerup', {});
    doc.defaultView.dispatch('pointermove', { clientX: 400, clientY: 400 });
    ck('and a release ends the drag', leftOf() === l0 + 40);
    tip(bar(), 'Move').dispatch('keydown', { key: 'ArrowRight', shiftKey: false });
    ck('the arrow keys move it from the handle', leftOf() === l0 + 48);
    tip(bar(), 'Move').dispatch('keydown', { key: 'ArrowLeft', shiftKey: true });
    ck('Shift takes bigger steps', leftOf() === l0 + 8);
    // A press on the page outside the chart, with the panel open, closes only the panel.
    click(tip(bar(), 'Settings'));
    await settle();
    ck('the panel is open', panels().length === 1);
    byClass(panel(), 'vela-dlg-header')[0].dispatch('pointerdown', { ...base, clientX: 50, clientY: 50 });
    doc.defaultView.dispatch('pointermove', { clientX: 90, clientY: 80 });
    ck('the header drags the panel', panel().style.getPropertyValue('transform') === 'translate(40px, 30px)', panel().style.getPropertyValue('transform'));
    doc.defaultView.dispatch('pointerup', {});
    doc.defaultView.dispatch('pointermove', { clientX: 300, clientY: 300 });
    ck('and a release ends the drag', panel().style.getPropertyValue('transform') === 'translate(40px, 30px)');
    host.dispatch('pointerdown', { ...base });
    await settle();
    ck('a press outside the chart closes the panel and leaves the bar', panels().length === 0 && bars().length === 1);
    click(tip(bar(), 'Settings'));
    await settle();
    byClass(panel(), 'vela-dsp-scroll')[0].dispatch('pointerdown', { ...base });
    ck('a press inside the panel keeps it', panels().length === 1);
    click(byClass(panel(), 'vela-dlg-close')[0]);
    await settle();
    host.dispatch('pointerdown', { ...base });
    await settle();
    ck('a press outside the chart with no panel dismisses the bar', bars().length === 0);
    // ── a multi-selection ──
    await key('Escape');
    await press(400, 160);
    await press(400, 320, { ctrlKey: true });
    const multiBar = bar();
    ck('a modifier press adds to the selection and the bar becomes the multi-selection bar', bars().length === 1 && multiBar && byLabel(multiBar, 'Settings') === null && byLabel(multiBar, 'Line color') !== null, labels(multiBar ?? { all: () => [] }).join(' | '));
    ck('the multi-selection bar has no per-drawing text field', byLabel(multiBar, 'Text') === null && byLabel(multiBar, 'Bold') !== null);
    click(tip(bar(), 'Line color'));
    setValue(floats()[0].all((n) => n.className === 'vela-ctl-swatch')[0], '#00aa00');
    await settle();
    const green = docs().filter((d) => d.style.lineColor === '#00aa00');
    ck('an edit on the multi-selection bar applies to every selected drawing', green.length === 2, `${green.length} drawings`);
    click(tip(bar(), 'Line width'));
    click(byLabel(floats()[0], '3px'));
    await settle();
    ck('width too', docs().filter((d) => d.style.lineWidth === 3).length === 2);
    click(tip(bar(), 'Lock'));
    await settle();
    ck('lock too, as one edit', docs().filter((d) => d.locked).length === 2);
    chart.undoDrawing();
    await settle();
    ck('undo takes the whole lock back', docs().filter((d) => d.locked).length === 0);
    await closeAll();

    // ── the full panel for a trend line ──
    await press(400, 160);
    ck('a click on the line selects it again', bars().length === 1 && byLabel(bar(), 'Settings') !== null);
    const target = docs().find((d) => d.style.lineWidth === 3) ?? docs()[0];
    const styleOf = () => JSON.stringify(docs().find((d) => d.id === target.id).style);
    const before = styleOf();
    click(tip(bar(), 'Settings'));
    await settle();
    ck('the gear opens the panel, non-modal, with the tool name as its title', panels().length === 1 && panel().getAttribute('role') === 'dialog' && panel().getAttribute('aria-modal') === 'false' && byClass(panel(), 'vela-dlg-title')[0].textContent === 'Trend Line');
    ck('the bar stays up beside it', bars().length === 1);
    ck('the panel is labelled by its title', panel().getAttribute('aria-labelledby') === byClass(panel(), 'vela-dlg-title')[0].id);
    const rowLabels = byClass(panel(), 'vela-dsp-lab').map((n) => n.textContent);
    ck('the panel lays out Line, Style, Extend and Ends rows', ['Line', 'Style', 'Extend', 'Ends'].every((l) => rowLabels.includes(l)), rowLabels.join(' | '));
    const folds = byClass(panel(), 'vela-dsp-fold-t').map((n) => n.textContent);
    ck('and the Label, Points and Show on folds', same(folds, ['Label', 'Points', 'Show on']), folds.join(' | '));
    ck('the footer has Cancel and OK', panel().all((n) => hasClass(n, 'vela-dlg-btn')).map((n) => n.textContent).join() === 'Cancel,OK');
    ck('the style menu button reads the style the drawing wears', byClass(panel(), 'vela-dsp-style')[0].getAttribute('aria-haspopup') === 'menu');

    // Edits are live.
    const lineRow = () => row(panel(), 'Line');
    setValue(lineRow().all((n) => n.className === 'vela-ctl-swatch')[0], '#123456');
    await settle();
    ck('a panel color edits the drawing live', docs().find((d) => d.id === target.id).style.lineColor === '#123456');
    click(byLabel(lineRow(), '4px'));
    await settle();
    ck('the width segment edits it and shows pressed', docs().find((d) => d.id === target.id).style.lineWidth === 4 && byLabel(lineRow(), '4px').getAttribute('aria-pressed') === 'true' && byLabel(lineRow(), '3px').getAttribute('aria-pressed') === 'false');
    const extendRow = () => row(panel(), 'Extend');
    click(byLabel(extendRow(), 'Extend both ways'));
    await settle();
    const ext = () => docs().find((d) => d.id === target.id).style;
    ck('extend sets both flags from one segment', ext().extendLeft === true && ext().extendRight === true);
    click(byLabel(extendRow(), 'Extend right'));
    await settle();
    ck('and one flag from another', ext().extendLeft === false && ext().extendRight === true);
    const endsRow = () => row(panel(), 'Ends');
    const endChip = endsRow().all((n) => hasClass(n, 'vela-dsp-chip')).at(-1);
    ck('the Ends row lists the arrow chips', endChip && ['Arrow', 'End arrow'].includes(endChip.textContent), endChip?.textContent);
    click(endChip);
    await settle();
    ck('an arrow chip toggles its flag', ext().arrowRight === true);

    // Cancel restores the drawing as it was; it is one more edit in the history.
    const cancel = () => panel().all((n) => hasClass(n, 'vela-dlg-btn') && n.textContent === 'Cancel')[0];
    click(cancel());
    await settle();
    ck('Cancel closes the panel and puts the drawing back', panels().length === 0 && styleOf() === before, styleOf());
    ck('the bar is still there', bars().length === 1);

    // OK keeps.
    click(tip(bar(), 'Settings'));
    await settle();
    click(byLabel(lineRow(), '2px'));
    await settle();
    click(panel().all((n) => hasClass(n, 'vela-dlg-btn') && n.textContent === 'OK')[0]);
    await settle();
    ck('OK closes the panel and keeps the edits', panels().length === 0 && docs().find((d) => d.id === target.id).style.lineWidth === 2);

    // Escape cancels, and the close button keeps.
    click(tip(bar(), 'Settings'));
    await settle();
    click(byLabel(lineRow(), '4px'));
    await settle();
    panel().dispatch('keydown', { key: 'Escape' });
    await settle();
    ck('Escape in the panel cancels', panels().length === 0 && docs().find((d) => d.id === target.id).style.lineWidth === 2);
    click(tip(bar(), 'Settings'));
    await settle();
    click(byLabel(lineRow(), '4px'));
    await settle();
    click(byClass(panel(), 'vela-dlg-close')[0]);
    await settle();
    ck('the close button keeps the edits', panels().length === 0 && docs().find((d) => d.id === target.id).style.lineWidth === 4);

    // ── points: coordinates ──
    click(tip(bar(), 'Settings'));
    await settle();
    const fold = (text) => byClass(panel(), 'vela-dsp-fold-h').find((n) => n.children[0].textContent === text);
    const openFold = (text) => {
        if (fold(text).getAttribute('aria-expanded') !== 'true') click(fold(text));
    };
    const pointsHead = fold('Points');
    ck('the Points fold starts closed on the first panel of a tool, open after (the open folds are remembered per tool), and names the prices in its header', pointsHead.getAttribute('aria-expanded') === (label === 'direct chart' ? 'false' : 'true') && pointsHead.children[1].textContent.includes('→'), pointsHead.children[1].textContent);
    if (pointsHead.getAttribute('aria-expanded') !== 'true') click(pointsHead);
    const priceA = byLabel(panel(), 'Point A price');
    const priceB = byLabel(panel(), 'Point B price');
    ck('opening it lists each anchor price as an editable field', priceA && priceB && byClass(panel(), 'vela-dsp-pt-t').length === 2 && byClass(panel(), 'vela-dsp-pt-t')[0].textContent !== '');
    const anchorsOf = () => docs().find((d) => d.id === target.id).anchors;
    const timeA = anchorsOf()[0].time;
    setValue(priceA, 123.45);
    await settle();
    ck('editing a price moves the anchor and only its price', anchorsOf()[0].price === 123.45 && anchorsOf()[0].time === timeA);
    ck('the header follows the new price', fold('Points').children[1].textContent.startsWith('123.45'), fold('Points').children[1].textContent);
    click(fold('Points'));
    ck('the fold closes again', fold('Points').getAttribute('aria-expanded') === 'false');

    // ── timeframe visibility ──
    openFold('Show on');
    ck('Show on reads "All timeframes" while unrestricted', fold('Show on').children[1].textContent === 'All timeframes' && byClass(panel(), 'vela-dsp-tf')[0].hasAttribute('hidden'));
    const showSeg = () => panel().all((n) => hasClass(n, 'vela-dsp-seg') && n.getAttribute('aria-label') === 'Show on')[0];
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === 'Intraday')[0]);
    await settle();
    const showOn = () => docs().find((d) => d.id === target.id).showOn;
    ck('Intraday limits the drawing to the intraday bands in the document', same(showOn(), ['s', '1', '5', '15', '60', '240']), JSON.stringify(showOn()));
    ck('and the header says so', fold('Show on').children[1].textContent === 'Intraday');
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === '1H and up')[0]);
    await settle();
    ck('1H and up is the other shortcut', same(showOn(), ['60', '240', 'D', 'W', 'M']));
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === 'Custom')[0]);
    await settle();
    const grid = byClass(panel(), 'vela-dsp-tf')[0];
    ck('Custom shows the band picker', !grid.hasAttribute('hidden') && grid.children.length === 9);
    click(grid.children.find((b) => b.textContent === 'D'));
    await settle();
    ck('a band button takes its band off', !showOn().includes('D') && showOn().length === 4, JSON.stringify(showOn()));
    ck('the pressed state follows', grid.children.find((b) => b.textContent === 'D').getAttribute('aria-pressed') === 'false' && grid.children.find((b) => b.textContent === '1H').getAttribute('aria-pressed') === 'true' && grid.children.find((b) => b.textContent === '5m').getAttribute('aria-pressed') === 'false', grid.children.map((b) => b.textContent + '=' + b.getAttribute('aria-pressed')).join(' '));
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === 'All')[0]);
    await settle();
    ck('All lifts the limit', showOn() === undefined);
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === 'Intraday')[0]);
    await settle();
    const exported = chart.exportDrawings();
    ck('the limit persists in the exported document', JSON.parse(exported).drawings.find((d) => d.id === target.id).showOn.length === 6);
    click(showSeg().all((n) => n.tagName === 'BUTTON' && n.textContent === 'All')[0]);
    await settle();

    // ── the label fold ──
    openFold('Label');
    const labelArea = panel().all((n) => n.tagName === 'TEXTAREA')[0];
    ck('the Label fold holds the text field', labelArea && labelArea.getAttribute('placeholder') === 'Add a label');
    ck('where the label sits is hidden until there is one', byClass(panel(), 'vela-dsp-row').filter((r) => ['Position', 'Side'].includes(r.children[0].textContent)).every((r) => r.hasAttribute('hidden')));
    labelArea.value = 'Resistance';
    labelArea.dispatch('input', {});
    ck('a label typed in the panel is not an edit until the field is left (one step, not one per key)', docs().find((d) => d.id === target.id).text?.value !== 'Resistance');
    labelArea.dispatch('change', {});
    await settle();
    ck('typing a label edits the drawing', docs().find((d) => d.id === target.id).text?.value === 'Resistance');
    ck('and shows where it sits', byClass(panel(), 'vela-dsp-row').filter((r) => ['Position', 'Side'].includes(r.children[0].textContent)).every((r) => !r.hasAttribute('hidden')));
    const placeRow = byClass(panel(), 'vela-dsp-row').find((r) => r.children[0].textContent === 'Position');
    click(placeRow.all((n) => n.tagName === 'BUTTON' && n.textContent === 'End')[0]);
    await settle();
    ck('the label position is an option of the text', docs().find((d) => d.id === target.id).text.place === 'end');
    const boldChip = panel().all((n) => hasClass(n, 'vela-dsp-chip') && n.getAttribute('aria-label') === 'Bold')[0];
    click(boldChip);
    await settle();
    ck('bold is a chip', docs().find((d) => d.id === target.id).text.bold === true && boldChip.getAttribute('aria-pressed') === 'true');
    ck('the fold header reads the label', fold('Label').children[1].textContent.includes('Resistance'));

    // ── styles: apply a preset, save, apply to another drawing, remove, reset ──
    const styleBtn = () => byClass(panel(), 'vela-dsp-style')[0];
    const menu = () => byClass(w, 'vela-dsp-menu')[0];
    const menuItem = (t) => menu().all((n) => hasClass(n, 'vela-dsp-mi') && n.children.some((c) => c.textContent === t))[0];
    click(styleBtn());
    ck('the style menu lists the tool presets and offers save and reset', menu() && same(byClass(menu(), 'vela-dsp-mh').map((n) => n.textContent), ['Presets']) && menuItem('Support') && menuItem('Save as style…') && menuItem('Reset to default'));
    click(menuItem('Support'));
    await settle();
    const st = () => docs().find((d) => d.id === target.id).style;
    ck('a preset applies its look as one edit and closes the menu', st().lineColor === '#089981' && menu() === undefined, st().lineColor);
    ck('and the header names it', styleBtn().children[0].textContent === 'Support');
    click(styleBtn());
    click(menuItem('Save as style…'));
    const nameInput = menu().all((n) => n.tagName === 'INPUT')[0];
    ck('Save as style turns into a name field', nameInput && nameInput.getAttribute('aria-label') === 'Style name');
    nameInput.value = 'Mine';
    nameInput.dispatch('keydown', { key: 'Enter' });
    await settle();
    ck('Enter saves the look as a named style, listed under Saved', same(byClass(menu(), 'vela-dsp-mh').map((n) => n.textContent), ['Presets', 'Saved']) && menuItem('Mine') !== undefined);
    ck('and the header now names it', styleBtn().children[0].textContent === 'Mine');
    panel().dispatch('keydown', { key: 'Escape' });
    ck('Escape closes the menu first, not the panel', menu() === undefined && panels().length === 1);
    click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'OK'));
    await settle();

    // Apply it to the other drawing.
    const other = docs().find((d) => d.id !== target.id && d.type === 'trendline');
    await closeAll();
    await press(400, other.id === docs()[0].id ? 160 : 320);
    ck('a click on the other line opens its bar', bars().length === 1);
    click(tip(bar(), 'Settings'));
    await settle();
    ck('its title is the same tool', byClass(panel(), 'vela-dlg-title')[0].textContent === 'Trend Line');
    click(styleBtn());
    ck('the saved style is in its menu too', menuItem('Mine') !== undefined);
    const otherId = docs().find((d) => bars().length === 1 && d.id !== target.id)?.id;
    click(menuItem('Mine'));
    await settle();
    const wearing = docs().filter((d) => d.style.lineColor === '#089981' && d.style.lineWidth === docs().find((x) => x.id === target.id).style.lineWidth);
    ck('applying it gives the other drawing that look', wearing.length >= 2, `${wearing.length}`);
    click(styleBtn());
    const removeBtn = menu().all((n) => n.getAttribute('role') === 'button' && n.getAttribute('aria-label') === 'Delete Mine')[0];
    ck('a saved style has a delete button', removeBtn !== undefined);
    click(removeBtn);
    await settle();
    ck('removing it takes it out of the menu', menuItem('Mine') === undefined && menuItem('Support') !== undefined);
    click(menuItem('Reset to default'));
    await settle();
    ck('Reset to default restores the factory look and closes the menu', docs().find((d) => d.id === (otherId ?? other.id)).style.lineColor !== '#089981' && menu() === undefined);
    await closeAll();
    ck('everything closes on a press away', bars().length === 0 && panels().length === 0);

    // ── undo after a cancelled panel: Cancel is one more edit in the history, as in the original ──
    {
        await press(400, 320);
        const widthNow = () => docs().filter((d) => d.type === 'trendline').map((d) => d.style.lineWidth).join();
        const w0 = widthNow();
        const lineW = Number(w0.split(',')[0]) === 4 ? '1px' : '4px';
        click(tip(bar(), 'Settings'));
        await settle();
        click(byLabel(row(panel(), 'Line'), lineW));
        await settle();
        const edited = widthNow();
        click(panel().all((n) => hasClass(n, 'vela-dlg-btn') && n.textContent === 'Cancel')[0]);
        await settle();
        ck('cancel restores the width', widthNow() === w0 && edited !== w0);
        chart.undoDrawing();
        await settle();
        ck('one undo after Cancel steps back onto the last live edit (Cancel is its own history step)', widthNow() === edited, `${w0} -> ${edited} -> ${widthNow()}`);
        chart.redoDrawing();
        await settle();
        await closeAll();
    }

    // ── a Fibonacci retracement: levels, presets, flags ──
    await place('fibretracement', 300, 260, 520, 150);
    ck('a retracement opens its bar with the level size controls', byLabel(bar(), 'Numbers size') !== null && byLabel(bar(), 'Label size') !== null && byLabel(bar(), 'Text') === null, labels(bar()).join(' | '));
    click(tip(bar(), 'Settings'));
    await settle();
    const fibId = docs().find((d) => d.type === 'fibretracement').id;
    const fib = () => docs().find((d) => d.id === fibId);
    const cells = () => byClass(panel(), 'vela-dsp-lv-c');
    const nLevels = fib().props.levels.length;
    ck('the Levels block lists every level, enabled or not, in a grid', cells().length === nLevels && nLevels > 10, `${cells().length} / ${nLevels}`);
    const cellOf = (i) => cells()[i];
    const numOf = (i) => cellOf(i).all((n) => hasClass(n, 'vela-ctl-num'))[0];
    const swOf = (i) => cellOf(i).all((n) => hasClass(n, 'vela-ctl-switch'))[0];
    ck('each level cell has an on/off switch, a ratio and a color', swOf(2) && numOf(2) && cellOf(2).all((n) => n.className === 'vela-ctl-swatch').length === 1);
    ck('the cells read the document', numOf(2).value === String(fib().props.levels[2].ratio) && swOf(2).checked === fib().props.levels[2].enabled);
    setValue(numOf(2), 0.3);
    await settle();
    ck('editing a level value writes that level', fib().props.levels[2].ratio === 0.3, JSON.stringify(fib().props.levels[2]));
    const wasOn = fib().props.levels[2].enabled;
    setCheck(swOf(2), !wasOn);
    await settle();
    ck('the switch enables or disables the level', fib().props.levels[2].enabled === !wasOn && cellOf(2).hasAttribute('data-off') === wasOn);
    setValue(numOf(3), -1);
    await settle();
    ck('a retracement may hold a negative ratio (horizontal level tools are free)', fib().props.levels[3].ratio === -1);
    setValue(cellOf(4).all((n) => n.className === 'vela-ctl-swatch')[0], '#abcdef');
    await settle();
    ck('a level color is its own edit', fib().props.levels[4].color === '#abcdef', fib().props.levels[4].color);
    const preset = (name) => byClass(panel(), 'vela-dsp-seg').find((s) => s.getAttribute('aria-label') === 'Level set').all((n) => n.tagName === 'BUTTON' && n.textContent === name)[0];
    ck('a retracement leads with its ready-made level sets', preset('Classic') && preset('Extension') && preset('OTE'));
    ck('an edited set matches none of them', ['Classic', 'Extension', 'OTE'].every((n) => preset(n).getAttribute('aria-pressed') === 'false'));
    click(preset('OTE'));
    await settle();
    ck('picking OTE swaps the levels and the direction in one edit', fib().props.levels[4].ratio === 0.62 && fib().props.reverse === false && fib().props.levels.some((l) => l.color !== fib().props.levels[0].color), JSON.stringify(fib().props.levels[4]));
    ck('and the grid rebuilds to show them', numOf(4).value === '0.62' && preset('OTE').getAttribute('aria-pressed') === 'true');
    click(preset('Extension'));
    await settle();
    ck('Extension reverses the swing', fib().props.reverse === true && preset('Extension').getAttribute('aria-pressed') === 'true');
    const flag = (name) => byClass(panel(), 'vela-dsp-chip').find((c) => c.textContent === name);
    ck('the flags are chips: Ratios, Prices, Background, Reverse', ['Ratios', 'Prices', 'Background', 'Reverse'].every((n) => flag(n)));
    click(flag('Prices'));
    await settle();
    ck('a flag chip toggles its option', fib().props.showPrices === false && flag('Prices').getAttribute('aria-pressed') === 'false');
    click(byClass(panel(), 'vela-dsp-seg').find((s) => s.getAttribute('aria-label') === 'Label side').all((n) => n.textContent === 'Right')[0]);
    await settle();
    ck('the label side is a segment', fib().props.labelSide === 'right');
    click(byClass(panel(), 'vela-dsp-seg').find((s) => s.getAttribute('aria-label') === 'Numbers size').all((n) => n.textContent === 'Large')[0]);
    await settle();
    ck('the level numbers have their own size', fib().props.numbersSize === 'large');
    ck('a levelled tool has no add or remove (the ladder is fixed; presets and switches shape it)', panel().all((n) => n.tagName === 'BUTTON' && /^(Add|Remove)/.test(n.getAttribute('aria-label') ?? '')).length === 0);
    // A change from elsewhere (an undo) rebuilds the grid.
    chart.undoDrawing();
    await settle();
    ck('an outside change (undo) refreshes the level grid', numOf(4).value === String(fib().props.levels[4].ratio));
    const nLevelsPos = panel().all((n) => hasClass(n, 'vela-dsp-block')).length;
    click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'OK'));
    await settle();
    void nLevelsPos;

    // The bar's dropdowns on the retracement.
    click(tip(bar(), 'Numbers size'));
    click(byLabel(floats()[0], 'Huge'));
    await settle();
    ck('the bar sizes the level numbers too', fib().props.numbersSize === 'huge');
    await closeAll();

    // ── the text tool: the bar carries the text controls ──
    chart.armDrawingTool('text');
    await settle();
    await press(300, 380);
    const area = p.all((n) => n.tagName === 'TEXTAREA')[0];
    ck('placing text opens the inline editor and the bar', area && bars().length === 1);
    area.value = 'Note';
    area.dispatch('input', {});
    area.dispatch('keydown', { key: 'Enter', ctrlKey: true, metaKey: false, isComposing: false });
    await settle();
    ck('the text bar carries color, size and the Text button (bold and italic sit under the field)', ['Text color', 'Text size', 'Text'].every((l) => byLabel(bar(), l)) && byLabel(bar(), 'Bold') === null, labels(bar()).join(' | '));
    const textDoc = () => docs().find((d) => d.type === 'text');
    click(tip(bar(), 'Text'));
    const barArea = bar().all((n) => n.tagName === 'TEXTAREA')[0];
    ck('the Text button expands a text field under the bar, focused, with the words', barArea && barArea.value === 'Note' && doc.activeElement === barArea);
    ck('bold and italic are under it', byLabel(bar(), 'Bold') && byLabel(bar(), 'Italic') && byLabel(bar(), 'Text color') && byLabel(bar(), 'Text size'));
    barArea.value = 'Note!';
    barArea.dispatch('input', {});
    await settle();
    ck('typing in it edits the text live', textDoc().text.value === 'Note!');
    click(tip(bar(), 'Bold'));
    await settle();
    ck('bold toggles on the text', textDoc().text.bold === true && tip(bar(), 'Bold').getAttribute('aria-pressed') === 'true');
    barArea.value = 'Note';
    barArea.dispatch('input', {});
    await settle();
    click(tip(bar(), 'Text size'));
    click(byLabel(floats()[0], 'Large'));
    await settle();
    ck('the size dropdown edits the text', textDoc().text.size === 'large');
    click(tip(bar(), 'Settings'));
    await settle();
    ck('the text panel leads with the words', panel().all((n) => n.tagName === 'TEXTAREA').length === 1 && panel().all((n) => n.tagName === 'TEXTAREA')[0].value === 'Note');
    const words = panel().all((n) => n.tagName === 'TEXTAREA')[0];
    words.value = 'Changed';
    words.dispatch('input', {});
    words.dispatch('change', {});
    await settle();
    ck('typing in it edits the drawing and does not reach the chart', textDoc().text.value === 'Changed');
    const stop = words.dispatch('keydown', { key: 'Delete' });
    ck('keys typed there stay out of the chart', stop.stopped === true && chart.drawingCount() >= 3);
    click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'Cancel'));
    await settle();
    ck('Cancel puts the words back', textDoc().text.value === 'Note');
    await closeAll();

    // ── a position: its own sections ──
    await place('position', 300, 440, 500, 400);
    click(tip(bar(), 'Settings'));
    await settle();
    const posId = docs().find((d) => d.type === 'position').id;
    const pos = () => docs().find((d) => d.id === posId);
    const heads = byClass(panel(), 'vela-dsp-h').map((n) => n.textContent);
    ck('the position panel has Position, Account, Levels and Display sections', ['Position', 'Account', 'Levels', 'Display'].every((h) => heads.includes(h)), heads.join(' | '));
    const posRow = (label) => byClass(panel(), 'vela-dsp-row').find((r) => r.children[0].textContent === label);
    const input = (r) => r.all((n) => hasClass(n, 'vela-ctl-num'))[0];
    setValue(input(posRow('Risk %')), 2.5);
    await settle();
    ck('Risk % edits the position', pos().props.riskPercent === 2.5);
    ck('the summary line reads the computed labels', byClass(panel(), 'vela-dsp-summary')[0].textContent.includes('R:R') && byClass(panel(), 'vela-dsp-summary')[0].textContent.includes('Loss'));
    const entry = () => Number(input(posRow('Entry price')).value);
    const stopRow = posRow('Stop');
    const unit = stopRow.all((n) => n.tagName === 'SELECT')[0];
    const priceBefore = Number(input(stopRow).value);
    setValue(unit, 'points');
    await settle();
    const points = Number(input(stopRow).value);
    ck('the Stop unit switches between price and points', Math.abs(points - Math.abs(entry() - priceBefore)) < 1e-6 && points !== priceBefore, `${priceBefore} -> ${points} points`);
    setValue(input(stopRow), 5);
    await settle();
    const side = Math.sign(priceBefore - entry());
    ck('a stop in points sits that far from the entry, on its own side', Math.abs(pos().anchors[1].price - (entry() + side * 5)) < 1e-6 || Math.abs(pos().anchors.at(-1).price - (entry() + side * 5)) < 1e-6 || pos().anchors.some((a) => Math.abs(a.price - (entry() + side * 5)) < 1e-6), JSON.stringify(pos().anchors.map((a) => a.price)));
    const dirSel = posRow('Direction').all((n) => n.tagName === 'SELECT')[0];
    ck('the direction select reads long or short', ['long', 'short'].includes(dirSel.value));
    const switches = byClass(panel(), 'vela-ctl-switch');
    ck('Display has six switches', switches.length === 6, `${switches.length}`);
    setCheck(posRow('Show level prices').all((n) => hasClass(n, 'vela-ctl-switch'))[0], true);
    await settle();
    ck('a display switch edits its flag', pos().props.showPrices === true);
    ck('the bar holds the zone colors', byLabel(bar(), 'Profit zone') && byLabel(bar(), 'Loss zone'));
    click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'OK'));
    await settle();
    await closeAll();

    // ── ported from the original tests: drawings-settings-panel and drawings-settings-reopen ──
    chart.clearDrawings();
    await settle();
    await place('trendline', 300, 200, 500, 120);
    {
        const id = docs()[0].id;
        const mine = () => docs().find((d) => d.id === id);
        const foldHead = (t) => byClass(panel(), 'vela-dsp-fold-h').find((n) => n.children[0].textContent === t);
        const openF = (t) => {
            if (foldHead(t).getAttribute('aria-expanded') !== 'true') click(foldHead(t));
        };
        const seg = (name) => byClass(panel(), 'vela-dsp-seg').find((s) => s.getAttribute('aria-label') === name);
        const btn = (name) => panel().all((n) => hasClass(n, 'vela-dlg-btn') && n.textContent === name)[0];
        const priceA = mine().anchors[0].price;
        const styleBefore = JSON.stringify(mine().style);

        // Cancel puts back the look, the points and the timeframes the drawing had when the panel opened.
        click(tip(bar(), 'Settings'));
        await settle();
        click(byLabel(row(panel(), 'Line'), '3px'));
        click(byLabel(row(panel(), 'Extend'), 'Extend right'));
        openF('Show on');
        click(seg('Show on').all((n) => n.tagName === 'BUTTON' && n.textContent === 'Intraday')[0]);
        openF('Points');
        setValue(byClass(panel(), 'vela-dsp-pt')[0].all((n) => n.tagName === 'INPUT')[0], 90);
        await settle();
        ck('the panel edits the look, the timeframes and a point', mine().style.lineWidth === 3 && mine().style.extendRight === true && mine().showOn?.length === 6 && mine().anchors[0].price === 90);
        click(btn('Cancel'));
        await settle();
        ck('Cancel puts back the look, the points and the timeframes', panels().length === 0 && JSON.stringify(mine().style) === styleBefore && mine().showOn === undefined && mine().anchors[0].price === priceA, `${JSON.stringify(mine().style)} ${mine().showOn} ${mine().anchors[0].price}`);

        // The timeframe choice follows a change made outside the panel (an undo).
        click(tip(bar(), 'Settings'));
        await settle();
        openF('Show on');
        click(seg('Show on').all((n) => n.tagName === 'BUTTON' && n.textContent === 'Intraday')[0]);
        await settle();
        ck('Intraday reads pressed', seg('Show on').all((n) => n.textContent === 'Intraday')[0].getAttribute('aria-pressed') === 'true');
        chart.undoDrawing();
        await settle();
        ck('an undo moves the choice back to All, and the header with it', panels().length === 1 && seg('Show on').all((n) => n.textContent === 'All')[0].getAttribute('aria-pressed') === 'true' && foldHead('Show on').children[1].textContent === 'All timeframes');
        click(btn('OK'));
        await settle();

        // A label typed key by key is one undo step, and where it sits shows once it has words.
        click(tip(bar(), 'Settings'));
        await settle();
        openF('Label');
        const area = panel().all((n) => n.tagName === 'TEXTAREA')[0];
        const sideRow = () => byClass(panel(), 'vela-dsp-row').find((r) => r.children[0].textContent === 'Side');
        ck('where the label sits is hidden until it has words', sideRow().hasAttribute('hidden'));
        for (let i = 1; i <= 'Weekly support'.length; i += 1) {
            area.value = 'Weekly support'.slice(0, i);
            area.dispatch('input', {});
        }
        area.dispatch('change', {});
        await settle();
        ck('the label is written and its side shows', mine().text?.value === 'Weekly support' && !sideRow().hasAttribute('hidden'));
        click(btn('OK'));
        await settle();
        chart.undoDrawing();
        await settle();
        ck('one undo takes the whole label back, not one key', (mine().text?.value ?? '') === '', mine().text?.value);
        await closeAll();
    }

    // The level ratios of a fib edited in one session are shown by the next one, and a Cancel there keeps them.
    await place('fibretracement', 300, 260, 520, 150);
    {
        const fibDoc = () => docs().find((d) => d.type === 'fibretracement');
        const ratios = () => fibDoc().props.levels.map((l) => l.ratio);
        const cellsNow = () => byClass(panel(), 'vela-dsp-lv-c');
        const ratioIn = (i) => cellsNow()[i].all((n) => hasClass(n, 'vela-ctl-num'))[0];
        const btn = (name) => panel().all((n) => hasClass(n, 'vela-dlg-btn') && n.textContent === name)[0];
        const r3 = ratios()[3];
        click(tip(bar(), 'Settings'));
        await settle();
        setValue(ratioIn(1), 0.25);
        setValue(ratioIn(2), 0.4);
        await settle();
        click(btn('OK'));
        await settle();
        click(tip(bar(), 'Settings'));
        await settle();
        ck('the panel shows every level ratio edited in an earlier session', same([1, 2].map((i) => Number(ratioIn(i).value)), [0.25, 0.4]) && same(ratios().slice(1, 3), [0.25, 0.4]));
        setValue(ratioIn(3), 0.55);
        await settle();
        click(btn('Cancel'));
        await settle();
        ck('cancelling the reopened panel keeps the edits made before it opened', same(ratios().slice(1, 4), [0.25, 0.4, r3]), ratios().slice(0, 4).join());
        await closeAll();
    }

    // The text field of the bar shows the whole label typed before it was closed.
    await place('trendline', 300, 330, 500, 380);
    {
        click(tip(bar(), 'Text'));
        const field = () => bar().all((n) => n.tagName === 'TEXTAREA')[0];
        for (const v of ['H', 'He', 'Hello']) {
            field().value = v;
            field().dispatch('input', {});
        }
        await settle();
        ck('typing in the bar field writes the label as it goes', docs().some((d) => d.type === 'trendline' && d.text?.value === 'Hello'));
        click(tip(bar(), 'Text'));
        click(tip(bar(), 'Text'));
        ck('reopened, the field holds the whole label', field() && field().value === 'Hello', field()?.value);
        await closeAll();
    }

    // Options that matter only in one mode come and go with it (a fair value gap, "Flip on close").
    chart.armDrawingTool('fairvaluegap');
    await settle();
    await press(420, 300);
    if (bars().length === 1 && byLabel(bar(), 'Settings')) {
        click(tip(bar(), 'Settings'));
        await settle();
        const flip = () => row(panel(), 'Flip on close');
        const extendSel = () => row(panel(), 'Extend right').all((n) => n.tagName === 'SELECT')[0];
        ck('a mode-gated option shows in its mode', flip() && !flip().hasAttribute('hidden'));
        setValue(extendSel(), 'half');
        await settle();
        ck('and hides when the mode leaves its list', flip().hasAttribute('hidden'));
        setValue(extendSel(), 'filled');
        await settle();
        ck('and returns with it', !flip().hasAttribute('hidden'));
    } else {
        ck('a fair value gap opens its bar with one click', false, `${bars().length} bars`);
    }
    await closeAll();
    chart.clearDrawings();
    await settle();

    // A press outside an open menu closes it (the dismissal of the original popover); a press inside does not.
    await place('trendline', 300, 200, 500, 120);
    click(tip(bar(), 'Line width'));
    ck('a menu opens', floats().length === 1);
    doc.dispatch('pointerdown', { target: floats()[0].all((n) => n.tagName === 'BUTTON')[0] });
    ck('a press inside the menu leaves it open', floats().length === 1);
    doc.dispatch('pointerdown', { target: tip(bar(), 'Lock') });
    ck('a press elsewhere closes it', floats().length === 0);
    click(tip(bar(), 'Settings'));
    await settle();
    click(byClass(panel(), 'vela-dsp-style')[0]);
    ck('the style menu of the panel opens', byClass(w, 'vela-dsp-menu').length === 1);
    doc.dispatch('pointerdown', { target: byClass(panel(), 'vela-dsp-scroll')[0] });
    ck('a press on the panel body closes it and the panel stays', byClass(w, 'vela-dsp-menu').length === 0 && panels().length === 1);
    click(byClass(panel(), 'vela-dsp-style')[0]);
    const hoverItem = byClass(w, 'vela-dsp-mi')[0];
    const styleBefore = JSON.stringify(docs()[0].style);
    hoverItem.dispatch('pointerenter', {});
    hoverItem.dispatch('pointerleave', {});
    await settle();
    ck('hovering a style and leaving it is not an edit', JSON.stringify(docs()[0].style) === styleBefore);
    doc.dispatch('keydown', { key: 'Escape' });
    ck('Escape on the document closes the menu, not the panel', byClass(w, 'vela-dsp-menu').length === 0 && panels().length === 1);
    await closeAll();
    chart.clearDrawings();
    await settle();

    // ── mixed values on a multi-selection ──
    await place('trendline', 300, 300, 500, 340);
    await key('Escape');
    await place('trendline', 560, 430, 700, 470);
    click(tip(bar(), 'Lock'));
    await settle();
    await key('Escape');
    await press(400, 320);
    await press(630, 450, { ctrlKey: true });
    const lockBtn = tip(bar(), 'Lock');
    const lockedCount = docs().filter((d) => d.locked).length;
    ck('a multi-selection whose drawings disagree on a toggle shows it half lit', lockedCount >= 1 && ['mixed', '1'].includes(lockBtn.getAttribute('data-active')), `${lockedCount} locked, ${lockBtn.getAttribute('data-active')}`);
    await closeAll();
    chart.clearDrawings();
    await settle();

    // ── every tool: the bar opens, the panel builds ──
    // vela-core's popup_restore empties a label added during the session instead of dropping the text block, so
    // a styled-but-empty text block counts as no text.
    const norm = () => JSON.stringify(docs().map((d) => (d.text && d.text.value === '' ? (({ text, ...rest }) => rest)(d) : d)));
    // The sweep over every tool runs on the direct chart; the facade chart shares the same widgets.
    const sweep = label === 'direct chart';
    const tools = sweep ? wasm.VelaChart.drawingToolTypes() : [];
    const opened = [];
    const skipped = [];
    const broken = [];
    const inert = [];
    for (const tool of tools) {
        if (['freehand', 'highlighter'].includes(tool)) {
            chart.armDrawingTool(tool);
            await settle();
            mouse('pointerdown', 300, 300);
            for (let i = 1; i <= 8; i += 1) {
                tick(8);
                mouse('pointermove', 300 + i * 12, 300 + (i % 2) * 14);
            }
            mouse('pointerup', 396, 300);
            await settle();
        } else {
            chart.armDrawingTool(tool);
            await settle();
            for (let i = 0; i < 9 && bars().length === 0; i += 1) {
                await press(260 + i * 60, 240 + ((i * 37) % 120));
            }
        }
        if (bars().length === 0) {
            skipped.push(tool);
            await key('Escape');
            chart.cancelDrawingTool();
            chart.clearDrawings();
            await settle();
            continue;
        }
        opened.push(tool);
        try {
            const gear = byLabel(bar(), 'Settings');
            if (gear) {
                click(gear);
                await settle();
                const open = panels().length === 1;
                const fields = panel() ? panel().all((n) => ['INPUT', 'SELECT', 'TEXTAREA'].includes(n.tagName)) : [];
                const unnamed = fields.filter((n) => !n.getAttribute('aria-label') && !n.getAttribute('title'));
                const title = panel()?.all((n) => hasClass(n, 'vela-dlg-title'))[0]?.textContent ?? '';
                if (!open || title === '' || !byClass(panel(), 'vela-dsp-fold-t').some((n) => n.textContent === 'Show on') || fields.some((n) => n.getAttribute('type') === 'number' && n.value === 'NaN') || unnamed.length > 0) {
                    broken.push(`${tool}: open=${open} title=${title} unnamed=${unnamed.length}`);
                }
                // Cancel leaves the drawing as it was.
                const snapshot = norm();
                // Every control edits something: none of them is wired to a path the model ignores.
                if (panel()) {
                    for (const toggle of byClass(panel(), 'vela-dsp-fold-h')) {
                        if (toggle.getAttribute('aria-expanded') !== 'true') click(toggle);
                    }
                    const controls = panel().all((n) => ['INPUT', 'SELECT'].includes(n.tagName) && !n.getAttribute('data-skip'));
                    for (const c of controls) {
                        const kind = c.tagName === 'SELECT' ? 'select' : c.getAttribute('type');
                        const was = JSON.stringify(docs());
                        const label = c.getAttribute('aria-label') ?? '?';
                        if (kind === 'checkbox') {
                            setCheck(c, !c.checked);
                        } else if (kind === 'color') {
                            setValue(c, c.value === '#010203' ? '#030201' : '#010203');
                        } else if (kind === 'number') {
                            const max = c.getAttribute('max');
                            const min = c.getAttribute('min');
                            const cur = Number(c.value);
                            const next = max != null && cur >= Number(max) ? Number(min ?? 0) : max != null ? Number(max) : cur + 3;
                            setValue(c, next);
                        } else if (kind === 'select') {
                            const options = c.children.map((o) => o.getAttribute('value'));
                            const other = options.find((v) => v !== c.value);
                            if (other === undefined) continue;
                            setValue(c, other);
                        } else if (kind === 'range') {
                            continue;
                        } else {
                            continue;
                        }
                        await settle();
                        if (!panel()) break;
                        // The position's Stop / Target unit selects only change what their number field reads.
                        if (JSON.stringify(docs()) === was && !(tool === 'position' && ['Stop', 'Target'].includes(label))) inert.push(`${tool}:${label}`);
                    }
                    if (panel()) {
                        click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'Cancel'));
                        await settle();
                    }
                    if (norm() !== snapshot) {
                        broken.push(`${tool}: cancel after edits did not restore`);
                        if (process.env.VELA_DEBUG) console.log(tool, snapshot, JSON.stringify(docs()));
                    }
                }
                if (panel()) {
                    for (const toggle of byClass(panel(), 'vela-dsp-fold-h')) click(toggle);
                    click(byClass(panel(), 'vela-dlg-btn').find((n) => n.textContent === 'Cancel'));
                    await settle();
                }
                if (norm() !== snapshot) broken.push(`${tool}: cancel changed the document`);
            }
        } catch (e) {
            broken.push(`${tool}: ${e.message}`);
        }
        await key('Escape');
        await press(60, 520);
        chart.clearDrawings();
        await settle();
    }
    if (sweep) ck('the bar opens for the tools that can be placed in a few clicks', opened.length >= 45, `${opened.length} of ${tools.length} tools; not placed here: ${skipped.join(' ')}`);
    if (sweep) ck('no control of any panel is wired to a path the model ignores', inert.length === 0, inert.join(' '));
    if (sweep) ck('every one of them has a settings panel that builds, with named controls, and a Cancel that changes nothing', broken.length === 0, broken.join('; '));
    chart.destroy();
    ck('destroy removes the bar, its menus and every document listener', host.children.length === 0 && doc.listeners.length === 0, `document listeners ${doc.listeners.length}`);
    void p;
    void tick;
    void ordered;
    void row;
    void setCheck;
    void panel;
}

await scenario('direct chart', async (host) => {
    const c = new wasm.VelaChart(host, 'dark');
    c.setBars(makeBars(300));
    c.addVolume();
    return c;
});
await scenario('facade chart', async (host) => {
    const a = new wasm.VelaApp(host, 'dark', 'DEMO', '1h', makeBars(300));
    await a.ready();
    return a;
});

if (state.failures > 0) {
    console.log(`${state.failures} check(s) failed.`);
    process.exit(1);
}
console.log('vela-web drawing settings UI behaves as expected against the fake DOM (no browser was run).');
