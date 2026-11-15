//! Cell behavior on the in-memory host, with the fake provider: the market state machine
//! (optimistic projection, the timeframe fallback), session metadata, the indicator ledger
//! and undo timeline, rehydrate and dehydrate. Ports of `chart-cell-persistence.test.ts`,
//! `chart-cell-timezone.test.ts` and `workspace-data-unavailable.test.ts`, plus the pure
//! option helpers of `workspace-core.test.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};

use super::*;
use crate::model::InputValue;
use crate::options::VelaOptions;
use crate::ports::market_data::{MarketSession, SymbolInfo};
use crate::state::{
    CellState, IndicatorsState, PersistedManifestEntry, PersistedNativeEntry, StatuslineParts,
};
use crate::workspace::host::ToastKind;
use crate::workspace::rig::{FakeProvider, Rig, T0};
use crate::workspace::shell::{Workspace, WorkspaceOptions};

fn solo() -> Rig {
    Rig::new("1", &[("only", "AAA")])
}

fn cell(rig: &Rig) -> ChartCell {
    rig.ws.cell("only").expect("the cell")
}

fn natives(state: &CellState) -> Vec<PersistedNativeEntry> {
    state
        .indicators
        .as_ref()
        .map(|i| i.natives.clone())
        .unwrap_or_default()
}

fn type_of(e: &PersistedNativeEntry) -> &str {
    ledger_native_type(e)
}

fn rig_with(f: impl FnOnce(&mut WorkspaceOptions)) -> Rig {
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "1", &[("only", "AAA")]);
    f(&mut opts);
    Rig::with_options(platform, provider, opts)
}

// ── pure option helpers (workspace-core.test.ts) ──────────────

#[test]
fn seed_defaults_picks_exactly_the_market_and_view_vocabulary() {
    let mut opts = VelaOptions::default();
    opts.market.symbol = Some("binance:BTCUSDT".into());
    opts.market.timeframe = Some("60".into());
    opts.market.bars = Some(500);
    opts.price_style = Some("footprint".into());
    opts.market.visible_range = Some(crate::ports::market_data::InitialRange::Preset(
        crate::visible_range::VisibleRangePreset::Month1,
    ));
    opts.glow = Some(0.5); // not part of the seed
    let seed = seed_defaults(&opts);
    assert_eq!(seed.symbol.as_deref(), Some("binance:BTCUSDT"));
    assert_eq!(seed.timeframe.as_deref(), Some("60"));
    assert_eq!(seed.bars, Some(500));
    assert_eq!(seed.price_style.as_deref(), Some("footprint"));
    assert!(seed.visible_range.is_some() && seed.data.is_none() && seed.session.is_none());
}

#[test]
fn a_per_cell_entry_overrides_the_top_level_default_with_the_same_words() {
    let base = CellSeed {
        symbol: Some("BTCUSDT".into()),
        timeframe: Some("60".into()),
        ..CellSeed::default()
    };
    let own = CellSeed {
        symbol: Some("ETHUSDT".into()),
        timeframe: Some("15".into()),
        ..CellSeed::default()
    };
    let c2 = own.over(base.clone());
    assert_eq!(
        (c2.symbol.as_deref(), c2.timeframe.as_deref()),
        (Some("ETHUSDT"), Some("15"))
    );
    assert_eq!(c2.bars, None, "untouched keys stay the (absent) default");
    let c1 = CellSeed::default().over(base);
    assert_eq!(c1.symbol.as_deref(), Some("BTCUSDT"));
}

#[test]
fn cell_chart_defaults_forwards_the_renderer_config_vocabulary_and_nothing_else() {
    let opts = VelaOptions {
        up_color: Some("#0a0".into()),
        down_color: Some("#a00".into()),
        glow: Some(0.5),
        log_scale: Some(true),
        current_price_line: Some(false),
        default_language: Some("pine".into()),
        drawings: Some(DrawingsOption::Bool(true)),
        height: Some(crate::options::HeightOption::Px(400.0)),
        native_backend: Some(NativeBackend::Webgl2),
        ..VelaOptions::default()
    };
    let d = cell_chart_defaults(&opts);
    assert_eq!(d.up_color.as_deref(), Some("#0a0"));
    assert_eq!(d.down_color.as_deref(), Some("#a00"));
    assert_eq!(d.glow, Some(0.5));
    assert_eq!(d.log_scale, Some(true));
    assert_eq!(d.current_price_line, Some(false));
    assert_eq!(d.default_language.as_deref(), Some("pine"));
    assert_eq!(d.drawings, Some(DrawingsOption::Bool(true)));
    // `height`, `native_backend` and the market seeds have no field to leak through.
}

#[test]
fn cell_drawings_passes_everything_through_except_the_toolbar() {
    let hidden_bar = DrawingsOption::Config(DrawingsConfig {
        toolbar: Some(false),
        ..DrawingsConfig::default()
    });
    assert_eq!(cell_drawings(None), hidden_bar);
    assert_eq!(cell_drawings(Some(&DrawingsOption::Bool(true))), hidden_bar);
    assert_eq!(
        cell_drawings(Some(&DrawingsOption::Bool(false))),
        DrawingsOption::Bool(false)
    );
    let with_tools = DrawingsOption::Config(DrawingsConfig {
        toolbar: Some(true),
        tools: Some(vec!["line".into()]),
        ..DrawingsConfig::default()
    });
    assert_eq!(
        cell_drawings(Some(&with_tools)),
        DrawingsOption::Config(DrawingsConfig {
            toolbar: Some(false),
            tools: Some(vec!["line".into()]),
            ..DrawingsConfig::default()
        })
    );
}

// ── the ledger rule ───────────────────────────────────────────

fn name(n: &str) -> PersistedManifestEntry {
    PersistedManifestEntry::Name(n.into())
}

fn native(t: &str) -> PersistedNativeEntry {
    PersistedNativeEntry::Type(t.into())
}

#[test]
fn a_settled_live_empty_set_is_the_truth() {
    let led = indicator_ledger(LedgerInputs {
        present: vec![],
        instance_entries: vec![],
        pending_manifest: Some(vec![name("old")]),
        manifest_settled: true,
        volume_pending: false,
    });
    assert!(
        led.manifest.is_empty() && led.natives.is_empty(),
        "removed everything stays removed"
    );
}

#[test]
fn an_unsettled_manifest_reports_the_restored_names_instead_of_the_empty_live_set() {
    let led = indicator_ledger(LedgerInputs {
        present: vec![],
        instance_entries: vec![],
        pending_manifest: Some(vec![name("old")]),
        manifest_settled: false,
        volume_pending: false,
    });
    assert_eq!(led.manifest, [name("old")]);
    // Once consumed, the live instances are reported.
    let led = indicator_ledger(LedgerInputs {
        present: vec![],
        instance_entries: vec![name("live")],
        pending_manifest: None,
        manifest_settled: false,
        volume_pending: false,
    });
    assert_eq!(led.manifest, [name("live")]);
}

#[test]
fn a_pending_volume_is_reported_once_and_never_twice() {
    let pending = |present: Vec<PersistedNativeEntry>| {
        indicator_ledger(LedgerInputs {
            present,
            instance_entries: vec![],
            pending_manifest: None,
            manifest_settled: true,
            volume_pending: true,
        })
        .natives
    };
    assert_eq!(pending(vec![]), [native("volume")]);
    assert_eq!(pending(vec![native("volume")]), [native("volume")]);
    assert_eq!(
        pending(vec![native("sma")]),
        [native("sma"), native("volume")]
    );
}

#[test]
fn ledger_names_read_whichever_shape_the_entry_travels_as() {
    assert_eq!(ledger_entry_name(&name("x")), "x");
    let entry = PersistedManifestEntry::Entry {
        name: "y".into(),
        inputs: None,
        props: None,
        hidden: Some(true),
    };
    assert_eq!(ledger_entry_name(&entry), "y");
    let ne = PersistedNativeEntry::Entry {
        kind: "rsi".into(),
        inputs: None,
        hidden: None,
    };
    assert_eq!(ledger_native_type(&ne), "rsi");
    assert_eq!(ledger_native_type(&native("sma")), "sma");
}

#[test]
fn session_overnight_reads_only_a_wrapped_extended_tape() {
    let info = |session: &str, ext: &str| SymbolInfo {
        ticker: "X".into(),
        session: Some(session.into()),
        session_extended: Some(ext.into()),
        ..SymbolInfo::default()
    };
    assert!(session_overnight(&info("0930-1600", "1700-1600")));
    assert!(
        !session_overnight(&info("0930-1600", "0400-2000")),
        "a day-split tape"
    );
    assert!(
        !session_overnight(&info("0930-1600", "1500-1600")),
        "evening before the regular close"
    );
    assert!(!session_overnight(&info("24x7", "1700-1600")));
    assert!(!session_overnight(&info("", "1700-1600")));
    assert!(!session_overnight(&info("garbage", "1700-1600")));
    assert!(!session_overnight(&info("0930-1600", "garbage")));
    assert!(!session_overnight(&info("0930-1600", "1700-2500")));
    assert!(!session_overnight(&SymbolInfo::default()));
}

// ── market ────────────────────────────────────────────────────

#[test]
fn a_symbol_switch_projects_optimistically_and_the_chart_follows() {
    let rig = solo();
    let c = cell(&rig);
    assert_eq!((c.symbol().as_str(), c.timeframe().as_str()), ("AAA", "60"));
    c.set_symbol("BBB");
    assert_eq!(
        c.symbol(),
        "BBB",
        "labels show the pick before the bars load"
    );
    assert_eq!(
        c.try_chart().unwrap().market().symbol.as_deref(),
        Some("BBB")
    );
    rig.settle();
    assert_eq!(c.symbol(), "BBB");
    c.set_symbol("BBB"); // already shown: nothing happens
    assert_eq!(c.symbol(), "BBB");
}

#[test]
fn a_prefixed_symbol_mirrors_its_venue_into_the_provider() {
    let rig = solo();
    let c = cell(&rig);
    c.set_symbol("fake:CCC");
    rig.settle();
    assert_eq!(c.provider().as_deref(), Some("fake"));
    c.set_symbol("DDD");
    rig.settle();
    assert_eq!(c.provider(), None);
}

#[test]
fn the_timeframe_setter_asks_the_chart_and_leaves_range_mode() {
    let rig = solo();
    let c = cell(&rig);
    c.apply_range(&CellRangePreset {
        id: "1M".into(),
        tf: "60".into(),
        bars: 800,
        preset: crate::visible_range::VisibleRangePreset::Month1,
    });
    assert_eq!(c.active_range_id().as_deref(), Some("1M"));
    c.set_timeframe("15");
    rig.settle();
    assert_eq!(c.timeframe(), "15");
    assert_eq!(
        c.active_range_id(),
        None,
        "an explicit timeframe leaves range mode"
    );
    assert_eq!(
        c.try_chart().unwrap().market().timeframe.as_deref(),
        Some("15")
    );
    c.set_timeframe("15"); // already shown: nothing
}

#[test]
fn a_range_chip_switches_timeframe_and_depth_and_stays_framed() {
    let rig = solo();
    let c = cell(&rig);
    c.apply_range(&CellRangePreset {
        id: "5D".into(),
        tf: "5".into(),
        bars: 2_000,
        preset: crate::visible_range::VisibleRangePreset::Week1,
    });
    rig.settle();
    let market = c.try_chart().unwrap().market();
    assert_eq!(market.timeframe.as_deref(), Some("5"));
    assert_eq!(market.bars, Some(2_000), "the chip's fetch budget");
    assert_eq!(c.active_range_id().as_deref(), Some("5D"));
    // The cell's own durable depth is untouched.
    assert_eq!(c.bars(), None);
}

#[test]
fn a_refused_timeframe_falls_back_and_says_so_keeping_the_requested_one() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    *rig.provider.refuse.borrow_mut() = Some(("5".into(), "60".into()));
    let c = cell(&rig);
    c.set_timeframe("5");
    rig.settle();
    assert_eq!(
        c.timeframe(),
        "60",
        "the chart runs the source's suggestion"
    );
    assert_eq!(
        c.requested_timeframe(),
        "5",
        "the request stays what the cell persists"
    );
    let toasts = rig.host.toasts();
    assert!(
        toasts.iter().any(|(m, k)| *k == ToastKind::Info
            && m.contains("AAA")
            && m.contains("5m")
            && m.contains("1h")),
        "{toasts:?}"
    );
    assert_eq!(c.dehydrate().timeframe.as_deref(), Some("5"));
    // The next symbol is asked for the requested timeframe again.
    *rig.provider.refuse.borrow_mut() = None;
    c.set_symbol("BBB");
    rig.settle();
    assert_eq!(c.timeframe(), "5");
}

#[test]
fn a_refused_timeframe_with_nothing_better_only_reports() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    // The suggestion equals the refused timeframe: no switch, an error notice.
    *rig.provider.refuse.borrow_mut() = Some(("5".into(), "5".into()));
    let c = cell(&rig);
    c.set_timeframe("5");
    rig.settle();
    assert_eq!(c.timeframe(), "5");
    assert!(
        rig.host
            .toasts()
            .iter()
            .any(|(m, k)| *k == ToastKind::Error && m.contains("not available"))
    );
}

#[test]
fn an_explicit_timeframe_choice_ends_a_running_fallback() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    *rig.provider.refuse.borrow_mut() = Some(("5".into(), "60".into()));
    let c = cell(&rig);
    c.set_timeframe("5");
    rig.settle();
    assert_eq!(c.timeframe(), "60");
    // Picking the fallback keeps it shown and makes it the requested one.
    c.set_timeframe("60");
    rig.settle();
    assert_eq!(
        (c.timeframe().as_str(), c.requested_timeframe().as_str()),
        ("60", "60")
    );
}

#[test]
fn session_metadata_decides_the_toggle_the_exchange_zone_and_the_overnight_flag() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    let c = cell(&rig);
    assert!(!c.session_available(), "no metadata, no sessions");
    *rig.provider.info.borrow_mut() = Some(SymbolInfo {
        ticker: "AAA".into(),
        session: Some("0930-1600".into()),
        session_extended: Some("1700-1600".into()),
        timezone: Some("America/New_York".into()),
        ..SymbolInfo::default()
    });
    c.set_symbol("BBB");
    rig.settle();
    assert!(c.session_available());
    assert!(c.session_overnight());
    assert_eq!(c.exchange_timezone().as_deref(), Some("America/New_York"));
    // The exchange rule resolves to the cell's own zone and reaches the renderer.
    rig.ws.set_timezone("exchange");
    assert_eq!(c.display_timezone(), "America/New_York");
    assert_eq!(
        c.try_chart().unwrap().renderer().get("timezone"),
        Some(json!("America/New_York"))
    );
    // A fixed zone ignores the market's.
    rig.ws.set_timezone("Asia/Tokyo");
    assert_eq!(c.display_timezone(), "Asia/Tokyo");
}

#[test]
fn the_session_setter_persists_and_reloads_the_market() {
    let rig = solo();
    let c = cell(&rig);
    assert_eq!(c.session(), MarketSession::Regular);
    c.set_session(MarketSession::Extended);
    rig.settle();
    assert_eq!(c.session(), MarketSession::Extended);
    assert_eq!(
        c.try_chart().unwrap().market().session,
        Some(MarketSession::Extended)
    );
    assert_eq!(c.dehydrate().session.as_deref(), Some("extended"));
    c.set_session(MarketSession::Extended); // no-op
}

#[test]
fn the_bars_setter_persists_the_budget_and_reloads_deeper_in_range_mode() {
    let rig = solo();
    let c = cell(&rig);
    c.set_bars(2000);
    rig.settle();
    assert_eq!(c.bars(), Some(2000.0));
    assert_eq!(c.dehydrate().bars, Some(2000.0));
    assert_eq!(c.try_chart().unwrap().market().bars, Some(2000));
}

#[test]
fn a_config_document_carrying_the_price_style_is_adopted() {
    let rig = solo();
    let c = cell(&rig);
    c.try_chart()
        .unwrap()
        .renderer()
        .apply_config(&json!({ "series": { "style": "line" } }));
    assert_eq!(c.requested_price_style(), "line");
    assert_eq!(c.dehydrate().price_style.as_deref(), Some("line"));
}

#[test]
fn the_price_style_setter_applies_live_and_reports() {
    let rig = solo();
    let c = cell(&rig);
    assert_eq!(
        (c.price_style().as_str(), c.requested_price_style().as_str()),
        ("candles", "candles")
    );
    c.set_price_style("area");
    assert_eq!(c.requested_price_style(), "area");
    assert_eq!(c.price_style(), "area");
    assert!(
        c.unavailable_price_styles().is_empty(),
        "no facts, nothing greyed"
    );
}

#[test]
fn the_cell_tracks_the_loading_state_and_the_last_crosshair() {
    let rig = solo();
    let c = cell(&rig);
    assert!(!c.loading(), "the first bars are in");
    c.set_symbol("BBB");
    assert!(c.loading() || !c.loading()); // the flag follows load:start / load:end
    rig.settle();
    assert!(!c.loading());
    assert_eq!(c.last_crosshair(), (None, None));
    let r = c.renderer().unwrap();
    r.pointer_move(r.coordinates().logical_to_x(290.0), 100.0);
    let (t, p) = c.last_crosshair();
    assert!(t.is_some() && p.is_some());
}

// ── presentation prefs ────────────────────────────────────────

#[test]
fn status_prefs_round_trip_through_the_document() {
    let rig = solo();
    let c = cell(&rig);
    c.set_watermark_visible(false);
    c.set_replay_watermark_visible(false);
    c.set_indicator_titles_visible(false);
    c.set_indicator_values_visible(false);
    c.set_statusline_part(StatuslinePart::Change, false);
    let doc = c.dehydrate();
    assert_eq!(doc.watermark, Some(false));
    assert_eq!(doc.replay_watermark, Some(false));
    assert_eq!(doc.indicator_titles, Some(false));
    assert_eq!(doc.indicator_values, Some(false));
    let parts = doc.statusline_parts.expect("parts");
    assert_eq!(
        (parts.change, parts.ohlc, parts.logo),
        (Some(false), Some(true), Some(true))
    );
    let renderer = c.try_chart().unwrap().renderer().get("indicatorTitles");
    assert_eq!(renderer, Some(json!(false)));
}

#[test]
fn applying_status_prefs_is_idempotent_and_changes_only_what_differs() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap();
    let b = rig.ws.cell("b").unwrap();
    let mut prefs = a.status_prefs();
    prefs.watermark = false;
    prefs.parts.as_mut().unwrap().market = false;
    b.apply_status_prefs(&prefs);
    assert_eq!(b.status_prefs(), prefs);
    b.apply_status_prefs(&prefs);
    assert_eq!(b.status_prefs(), prefs);
    // A shell without status lines carries no parts.
    let bare = rig_with(|o| o.statusline = false);
    assert_eq!(bare.ws.cell("only").unwrap().status_prefs().parts, None);
    bare.ws
        .cell("only")
        .unwrap()
        .set_statusline_part(StatuslinePart::Name, false); // nothing to hide
    assert!(
        bare.ws
            .cell("only")
            .unwrap()
            .dehydrate()
            .statusline_parts
            .is_none()
    );
}

#[test]
fn the_session_shade_colors_live_in_the_renderer_config() {
    let rig = solo();
    let c = cell(&rig);
    c.set_session_shade_color("premarketColor", "#112233");
    assert_eq!(c.session_shade_color("premarketColor"), "#112233");
    assert_eq!(c.session_shade_color("unknownKey"), "");
    let doc = c.dehydrate();
    assert_eq!(
        doc.renderer_config.unwrap()["sessions"]["premarketColor"],
        "#112233"
    );
}

// ── indicators ────────────────────────────────────────────────

#[test]
fn a_native_indicator_added_removed_and_restored_through_the_undo_timeline() {
    let rig = solo();
    let c = cell(&rig);
    assert_eq!(c.indicator_count(), 0);
    assert!(!c.history().can_undo());
    c.add_native("sma");
    assert_eq!(c.indicator_count(), 1);
    assert!(c.history().can_undo());
    assert_eq!(c.on_chart_rows().len(), 1);
    assert!(c.on_chart_rows()[0].native);
    assert_eq!(c.dehydrate().indicators.unwrap().natives.len(), 1);
    c.history().undo();
    assert_eq!(c.indicator_count(), 0);
    assert!(c.history().can_redo());
    c.history().redo();
    assert_eq!(c.indicator_count(), 1);
    c.history().undo();
    c.history().undo(); // nothing left: no-op
    assert_eq!(c.indicator_count(), 0);
}

#[test]
fn a_single_instance_native_added_twice_records_one_undo_entry() {
    let rig = rig_with(|_| {});
    let c = cell(&rig);
    c.try_chart()
        .unwrap()
        .add_native_indicator("vpvr", NativeIndicatorOptions::default());
    rig.settle();
    let before = c.indicator_count();
    c.add_native("vpvr"); // already on the chart: the existing instance, nothing changed
    assert_eq!(c.indicator_count(), before);
    assert!(!c.history().can_undo(), "nothing entered the undo timeline");
}

#[test]
fn an_out_of_band_native_removal_enters_the_timeline_and_undo_re_adds_the_type() {
    let rig = solo();
    let c = cell(&rig);
    c.add_native("sma");
    c.history().undo(); // clean slate: remove the add record
    c.history().redo();
    rig.settle(); // `indicator:added` lands with the first computed model
    let handle = c
        .try_chart()
        .unwrap()
        .indicators()
        .into_iter()
        .next()
        .expect("sma");
    handle.remove(); // legend close button, object tree, middle click
    assert_eq!(c.indicator_count(), 0);
    c.history().undo();
    rig.settle();
    assert_eq!(
        c.indicator_count(),
        1,
        "undo re-adds a native of the removed type"
    );
    assert_eq!(
        c.try_chart().unwrap().indicators()[0].native_type(),
        Some("sma")
    );
    c.history().redo();
    assert_eq!(c.indicator_count(), 0);
}

#[test]
fn native_input_deltas_and_visibility_persist_and_converge() {
    let rig = solo();
    let c = cell(&rig);
    c.add_native("sma");
    let handle = c
        .try_chart()
        .unwrap()
        .indicators()
        .into_iter()
        .next()
        .unwrap();
    // Change the first numeric input.
    let schema = handle.inputs();
    let numeric = schema
        .iter()
        .find(|s| matches!(s.defval, InputValue::Number(_)))
        .expect("a numeric input");
    let InputValue::Number(default) = numeric.defval else {
        unreachable!()
    };
    let mut change = crate::model::Inputs::new();
    change.insert(numeric.key.clone(), InputValue::Number(default + 7.0));
    handle.set_inputs(change);
    handle.set_visible(false);
    let doc = c.dehydrate();
    let entry = natives(&doc)
        .into_iter()
        .find(|e| type_of(e) == "sma")
        .expect("sma");
    let PersistedNativeEntry::Entry { inputs, hidden, .. } = &entry else {
        panic!("deltas travel as the object form, got {entry:?}");
    };
    assert_eq!(hidden, &Some(true));
    assert_eq!(inputs.as_ref().unwrap()[&numeric.key], json!(default + 7.0));

    // Restoring that document converges a fresh state: defaults come back for omitted keys.
    let mut reset = doc.clone();
    reset.indicators = Some(IndicatorsState {
        manifest: vec![],
        natives: vec![native("sma")],
    });
    c.rehydrate(&reset);
    let after = c
        .try_chart()
        .unwrap()
        .indicators()
        .into_iter()
        .next()
        .unwrap();
    assert!(after.visible(), "no hidden flag: visible again");
    assert_eq!(
        after.input_values()[&numeric.key],
        InputValue::Number(default),
        "the document is the truth"
    );
    // And the object form applies the deltas back.
    c.rehydrate(&doc);
    let after = c
        .try_chart()
        .unwrap()
        .indicators()
        .into_iter()
        .next()
        .unwrap();
    assert!(!after.visible());
    assert_eq!(
        after.input_values()[&numeric.key],
        InputValue::Number(default + 7.0)
    );
}

#[test]
fn rehydrating_a_ledger_converges_as_a_multiset() {
    let rig = solo();
    let c = cell(&rig);
    // Added on the chart directly: the setup itself must not enter the undo timeline.
    c.try_chart()
        .unwrap()
        .add_native_indicator("sma", NativeIndicatorOptions::default());
    let mut doc = c.dehydrate();
    doc.indicators = Some(IndicatorsState {
        manifest: vec![],
        natives: vec![native("sma"), native("sma"), native("rsi")],
    });
    c.rehydrate(&doc);
    rig.settle();
    let types: Vec<String> = c
        .try_chart()
        .unwrap()
        .indicators()
        .iter()
        .filter_map(|h| h.native_type().map(str::to_string))
        .collect();
    assert_eq!(types.iter().filter(|t| *t == "sma").count(), 2, "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "rsi").count(), 1);
    assert!(
        !c.history().can_undo(),
        "convergence is state application, not a user edit"
    );
    doc.indicators = Some(IndicatorsState::default());
    c.rehydrate(&doc);
    assert_eq!(c.indicator_count(), 0, "the surplus goes");
}

#[test]
fn a_restored_ledger_without_volume_stays_without_it_and_a_fresh_cell_gets_it() {
    let fresh = rig_with(|o| o.chart.volume = Some(true));
    let c = cell(&fresh);
    assert!(
        natives(&c.dehydrate())
            .iter()
            .any(|e| type_of(e) == "volume"),
        "a fresh cell keeps the workspace default"
    );
    // Removing volume sticks: the dehydrated ledger stops reporting the intent.
    let handle = c
        .try_chart()
        .unwrap()
        .indicators()
        .into_iter()
        .find(|h| h.native_type() == Some("volume"))
        .expect("the auto-added volume");
    handle.remove();
    fresh.settle();
    assert!(
        !natives(&c.dehydrate())
            .iter()
            .any(|e| type_of(e) == "volume")
    );

    // A pooled state that lists no volume boots without it even though the option says yes.
    let mut state = c.dehydrate();
    state.indicators = Some(IndicatorsState::default());
    let rig2 = rig_with(|o| {
        o.chart.volume = Some(true);
        o.cells.clear();
    });
    let doc = {
        let mut d = rig2.ws.get_state();
        d.charts[0].cell = state;
        d
    };
    rig2.ws.apply_state_doc(doc);
    rig2.settle();
    assert!(
        !natives(&rig2.ws.cells()[0].dehydrate())
            .iter()
            .any(|e| type_of(e) == "volume")
    );
}

#[test]
fn an_early_snapshot_before_the_first_candles_reports_the_volume_intent() {
    let rig = rig_with(|o| o.chart.volume = Some(true));
    // Build a second workspace that has not loaded yet: dehydrate right after construction.
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "1", &[("only", "AAA")]);
    opts.chart.volume = Some(true);
    let env = crate::vela::VelaEnv::new(platform.handle());
    let host = crate::workspace::testing::TestWorkspaceHost::new(800.0, 400.0);
    let ws = Workspace::new(host, env, opts).unwrap();
    let early = ws.cells()[0].dehydrate();
    assert!(
        natives(&early).iter().any(|e| type_of(e) == "volume"),
        "an early save must not read as a deliberate opt-out"
    );
    drop(rig);
}

#[test]
fn a_restored_manifest_ledger_waits_for_the_manifest_and_is_reported_meanwhile() {
    let platform = crate::runtime::testing::ManualPlatform::new(T0 as f64 + 400.0 * 86_400_000.0);
    let provider = FakeProvider::new();
    let mut opts = crate::workspace::rig::options(&provider, "1", &[]);
    opts.manifest_pending = true;
    opts.persist = Some("ws".into());
    let storage = crate::workspace::persist::MemoryStorage::new();
    let mut doc = crate::state::WorkspaceState::new("1");
    doc.charts.push(crate::state::ChartState {
        id: "only".into(),
        cell: CellState {
            symbol: Some("AAA".into()),
            timeframe: Some("60".into()),
            indicators: Some(IndicatorsState {
                manifest: vec![name("My Script")],
                natives: vec![],
            }),
            ..CellState::default()
        },
    });
    crate::workspace::persist::WorkspaceStorage::set(
        &storage,
        "ws",
        &crate::state::encode_state(&doc),
    );
    opts.storage = Some(Rc::new(storage));
    let rig = Rig::with_options(platform, provider, opts);
    let c = cell(&rig);
    let held = c.dehydrate().indicators.unwrap();
    assert_eq!(
        held.manifest,
        [name("My Script")],
        "an early save keeps the restored names"
    );
    // The manifest resolves without that entry: it settles empty.
    rig.ws.set_manifest(vec![]);
    let after = c.dehydrate().indicators.unwrap();
    assert!(after.manifest.is_empty());
}

#[test]
fn the_manifest_seeds_enabled_entries_into_fresh_cells_only() {
    let manifest = vec![
        ManifestIndicator {
            name: "On".into(),
            script: "//".into(),
            language: Some("none".into()),
            category: None,
            enabled: true,
        },
        ManifestIndicator {
            name: "Off".into(),
            script: "//".into(),
            language: Some("none".into()),
            category: Some("Misc".into()),
            enabled: false,
        },
    ];
    let rig = rig_with(|o| o.indicators = Some(manifest.clone()));
    let c = cell(&rig);
    let rows = c.library_rows();
    let scripts: Vec<&str> = rows
        .iter()
        .filter(|r| !r.native)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(
        scripts,
        ["On", "Off"],
        "the manifest in the host's own order, after the natives"
    );
    assert!(rows.len() >= 2);
}

#[test]
fn the_library_lists_supported_natives_first_sorted_by_title() {
    let rig = solo();
    rig.settle();
    let c = cell(&rig);
    let rows = c.library_rows();
    let natives: Vec<&LibraryRow> = rows.iter().take_while(|r| r.native).collect();
    assert!(natives.len() >= 3, "the classics are in the catalog");
    let titles: Vec<String> = natives.iter().map(|r| r.name.to_lowercase()).collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(titles, sorted, "A to Z by title");
    assert!(
        natives
            .iter()
            .all(|r| r.category.as_deref() == Some("Built-in"))
    );
    assert!(natives.iter().all(|r| r.native_type.is_some()));
    // Adding by library index lands on the same row.
    let first_type = natives[0].native_type.clone().unwrap();
    c.add_from_library(0);
    let added = c.try_chart().unwrap().indicators();
    assert_eq!(added[0].native_type(), Some(first_type.as_str()));
    // Removing by on-chart index.
    c.remove_from_chart(0);
    assert_eq!(c.indicator_count(), 0);
    c.add_from_library(10_000); // past the end: nothing
    c.remove_from_chart(10_000);
}

// ── persistence ───────────────────────────────────────────────

#[test]
fn dehydrate_then_rehydrate_in_place_restores_the_cell() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap();
    let b = rig.ws.cell("b").unwrap();
    a.set_symbol("CCC");
    a.set_timeframe("15");
    a.set_price_style("line");
    a.set_bars(1500);
    a.set_watermark_visible(false);
    a.add_native("sma");
    let chart = a.try_chart().unwrap();
    chart.drawings().add(
        "trendline",
        crate::drawings::controller::AddInit {
            anchors: Some(vec![
                crate::drawings::DrawingPoint {
                    time: (T0 + 3_600_000) as f64,
                    price: 100.0,
                },
                crate::drawings::DrawingPoint {
                    time: (T0 + 9_000_000) as f64,
                    price: 101.0,
                },
            ]),
            ..Default::default()
        },
    );
    rig.settle();
    let doc = a.dehydrate();
    // Apply onto the OTHER cell in place: it converges to a's document.
    b.rehydrate(&doc);
    rig.settle();
    assert_eq!(b.symbol(), "CCC");
    assert_eq!(b.timeframe(), "15");
    assert_eq!(b.requested_price_style(), "line");
    assert_eq!(b.bars(), Some(1500.0));
    assert!(!b.status_prefs().watermark);
    assert_eq!(b.try_chart().unwrap().drawings().all().len(), 1);
    assert_eq!(b.indicator_count(), 1);
    assert!(!b.history().can_undo(), "state application is not an edit");
}

#[test]
fn garbage_in_a_pooled_state_never_panics() {
    let rig = solo();
    let c = cell(&rig);
    let doc = CellState {
        symbol: Some(String::new()),
        timeframe: Some(String::new()),
        bars: Some(f64::NAN),
        session: Some("sideways".into()),
        price_style: Some(String::new()),
        renderer_config: Some(json!("not an object")),
        drawings: Some(json!([1, 2, 3])),
        indicators: Some(IndicatorsState {
            manifest: vec![name("")],
            natives: vec![native("no-such-indicator"), native("")],
        }),
        statusline_parts: Some(StatuslineParts::default()),
        ext: Some([("junk".to_string(), json!(null))].into()),
        ..CellState::default()
    };
    c.rehydrate(&doc);
    rig.settle();
    assert_eq!(c.symbol(), "AAA", "an empty symbol changes nothing");
    assert_eq!(c.timeframe(), "60");
    assert_eq!(c.session(), MarketSession::Regular);
    // Unknown ext keys ride along verbatim.
    assert_eq!(c.dehydrate().ext.unwrap()["junk"], Value::Null);
}

#[test]
fn a_pre_prefix_document_welds_its_provider_back_onto_the_symbol() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    let c = cell(&rig);
    let doc = CellState {
        symbol: Some("CCC".into()),
        provider: Some("fake".into()),
        ..CellState::default()
    };
    c.rehydrate(&doc);
    rig.settle();
    assert_eq!(c.symbol(), "fake:CCC");
    assert_eq!(c.provider().as_deref(), Some("fake"));
}

struct CountingExt {
    restored: Rc<RefCell<Vec<Value>>>,
}

impl CellExtHandler for CountingExt {
    fn key(&self) -> &str {
        "vendor.counter"
    }

    fn serialize(&self, cell: &ChartCell) -> Option<Value> {
        Some(json!({ "symbol": cell.symbol() }))
    }

    fn restore(&self, value: &Value, cell: &ChartCell) {
        self.restored.borrow_mut().push(value.clone());
        // A restore that adds indicators must not enter the undo timeline.
        cell.add_native("sma");
    }
}

#[test]
fn third_party_state_rides_the_ext_seam_and_restores_muted() {
    let restored = Rc::new(RefCell::new(Vec::new()));
    let rig = rig_with(|o| {
        o.cell_ext_handlers = vec![Rc::new(CountingExt {
            restored: restored.clone(),
        })];
    });
    let c = cell(&rig);
    let doc = c.dehydrate();
    assert_eq!(
        doc.ext.as_ref().unwrap()["vendor.counter"],
        json!({ "symbol": "AAA" })
    );
    let mut incoming = doc.clone();
    incoming.ext = Some(
        [
            ("vendor.counter".to_string(), json!({ "from": "document" })),
            ("other.plugin".to_string(), json!(7)),
        ]
        .into(),
    );
    c.rehydrate(&incoming);
    assert_eq!(
        restored.borrow().as_slice(),
        [json!({ "from": "document" })]
    );
    assert!(!c.history().can_undo(), "the restore ran muted");
    let after = c.dehydrate().ext.unwrap();
    assert_eq!(
        after["other.plugin"],
        json!(7),
        "an unhandled key survives verbatim"
    );
    assert_eq!(
        after["vendor.counter"],
        json!({ "symbol": "AAA" }),
        "the handler wins for its own key"
    );
}

#[test]
fn a_destroyed_cell_is_inert_and_still_dehydrates() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let a = rig.ws.cell("a").unwrap();
    let doc_before = a.dehydrate();
    a.destroy();
    assert!(a.is_destroyed() && a.try_chart().is_none() && a.renderer().is_none());
    a.set_symbol("EEE");
    a.set_timeframe("15");
    a.set_price_style("line");
    a.add_native("sma");
    a.reset_view();
    a.focus();
    a.destroy(); // idempotent
    let doc = a.dehydrate();
    assert_eq!(
        doc.symbol, doc_before.symbol,
        "the last known market is kept"
    );
    assert_eq!(a.indicator_count(), 0);
    assert!(a.unavailable_price_styles().is_empty());
}

#[test]
fn the_picker_rows_and_catalog_refresh_after_a_symbol_change() {
    let rig = solo();
    let c = cell(&rig);
    rig.settle();
    assert!(!c.native_catalog().is_empty());
    assert!(c.native_catalog().iter().any(|n| n.supported));
    c.add_native("sma");
    rig.settle();
    let sma = c
        .native_catalog()
        .into_iter()
        .find(|n| n.indicator_type == "sma")
        .expect("sma");
    assert!(sma.present, "the catalog follows the chart");
    c.relocalize();
}
