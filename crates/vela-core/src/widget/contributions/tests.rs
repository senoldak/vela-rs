//! The contribution registries: ordering, replacement, disposers, the topbar slot
//! overrides, the legend providers and the state-persistence handlers.

use std::cell::{Cell, RefCell};

use serde_json::json;

use super::*;
use crate::runtime::testing::ManualPlatform;

fn contributions() -> (ManualPlatform, Contributions) {
    let platform = ManualPlatform::new(0.0);
    let c = Contributions::new(platform.handle());
    (platform, c)
}

use crate::widget::test_support::ctx;

fn action(id: &str, target: WidgetActionTarget) -> WidgetActionDescriptor {
    WidgetActionDescriptor::new(id, target, "Label", |_| {})
}

fn ids<T, F: Fn(&T) -> &str>(list: &[Rc<T>], f: F) -> Vec<String> {
    list.iter().map(|x| f(x).to_string()).collect()
}

fn action_ids(list: &[Rc<WidgetActionDescriptor>]) -> Vec<String> {
    ids(list, |d| d.id.as_str())
}

// ── widget actions ──

#[test]
fn actions_filter_by_target_sort_by_order_and_keep_registration_order_on_ties() {
    let (_p, c) = contributions();
    let mut late = action("late", WidgetActionTarget::Topbar);
    late.order = Some(5.0);
    let mut early = action("early", WidgetActionTarget::Topbar);
    early.order = Some(-1.0);
    let _ = c.register_widget_action(late);
    let _ = c.register_widget_action(action("tie-a", WidgetActionTarget::Topbar));
    let _ = c.register_widget_action(action("other", WidgetActionTarget::Sidebar));
    let _ = c.register_widget_action(action("tie-b", WidgetActionTarget::Topbar));
    let _ = c.register_widget_action(early);
    assert_eq!(
        action_ids(&c.widget_actions(WidgetActionTarget::Topbar, None)),
        ["early", "tie-a", "tie-b", "late"]
    );
    assert_eq!(
        action_ids(&c.widget_actions(WidgetActionTarget::Sidebar, None)),
        ["other"]
    );
    assert!(
        c.widget_actions(WidgetActionTarget::ContextBody, None)
            .is_empty()
    );
}

#[test]
fn a_context_filters_actions_through_their_when_gate() {
    let (_p, c) = contributions();
    let mut hidden = action("hidden", WidgetActionTarget::Topbar);
    hidden.when = Some(Rc::new(|_| false));
    let mut gated = action("gated", WidgetActionTarget::Topbar);
    gated.when = Some(Rc::new(|ctx| ctx.symbol() == "BTCUSDT"));
    let _ = c.register_widget_action(hidden);
    let _ = c.register_widget_action(gated);
    let _ = c.register_widget_action(action("plain", WidgetActionTarget::Topbar));
    let live = ctx(1);
    assert_eq!(
        action_ids(&c.widget_actions(WidgetActionTarget::Topbar, Some(&live))),
        ["gated", "plain"]
    );
    assert_eq!(c.widget_actions(WidgetActionTarget::Topbar, None).len(), 3);
}

#[test]
fn re_registering_an_id_replaces_it_and_a_stale_disposer_does_not_remove_the_replacement() {
    let (_p, c) = contributions();
    let first = c.register_widget_action(action("x", WidgetActionTarget::Topbar));
    let mut second = action("x", WidgetActionTarget::Topbar);
    second.label = "Second".into();
    let _ = c.register_widget_action(second);
    first();
    let list = c.widget_actions(WidgetActionTarget::Topbar, None);
    assert_eq!(list.len(), 1);
    assert_eq!(text_of(None, &list[0].label, &[]), "Second");
    c.unregister_widget_action("x");
    assert!(
        c.widget_actions(WidgetActionTarget::Topbar, None)
            .is_empty()
    );
}

#[test]
fn an_action_registered_under_an_overridable_id_becomes_the_slot_override() {
    let (_p, c) = contributions();
    let off = c.register_widget_action(action("indicators", WidgetActionTarget::Topbar));
    assert_eq!(
        c.topbar_action_override("indicators")
            .map(|d| d.id.clone())
            .as_deref(),
        Some("indicators")
    );
    off();
    assert!(c.topbar_action_override("indicators").is_none());
}

#[test]
fn a_composite_built_in_id_is_refused_with_a_warning_and_never_registered() {
    let (platform, c) = contributions();
    let off = c.register_widget_action(action("symbol", WidgetActionTarget::Topbar));
    assert_eq!(platform.warnings().len(), 1);
    assert!(platform.warnings()[0].contains("\"symbol\""));
    assert!(
        !c.widget_actions(WidgetActionTarget::Topbar, None)
            .iter()
            .any(|a| a.id == "symbol")
    );
    assert!(c.topbar_action_override("symbol").is_none());
    off(); // the returned no-op disposer must be safe to call
}

#[test]
fn only_the_documented_simple_button_slots_are_overridable() {
    assert_eq!(OVERRIDABLE_TOPBAR_IDS, ["indicators", "screenshot"]);
    for id in OVERRIDABLE_TOPBAR_IDS {
        assert!(TOPBAR_BUILTIN_IDS.contains(&id));
    }
    let (_p, c) = contributions();
    // A non-built-in id is never reported as an override, even when registered.
    let _ = c.register_widget_action(action("vendor.tool", WidgetActionTarget::Topbar));
    assert!(c.topbar_action_override("vendor.tool").is_none());
}

#[test]
fn mobile_placement_defaults_follow_the_alignment() {
    assert_eq!(
        mobile_placement(Some(ActionAlign::Left), None),
        MobilePlacement::Bar
    );
    assert_eq!(
        mobile_placement(Some(ActionAlign::Right), None),
        MobilePlacement::Menu
    );
    assert_eq!(mobile_placement(None, None), MobilePlacement::Menu);
    assert_eq!(
        mobile_placement(Some(ActionAlign::Left), Some(MobilePlacement::Menu)),
        MobilePlacement::Menu
    );
}

#[test]
fn topbar_item_ids_round_trip_and_exclude_the_actions_slot() {
    for id in TOPBAR_BUILTIN_IDS {
        match TopbarItemId::from_id(id) {
            Some(item) => assert_eq!(item.as_str(), id),
            None => assert_eq!(id, "actions"),
        }
    }
    assert!(TopbarItemId::from_id("nope").is_none());
}

// ── attachments and panels ──

#[test]
fn attachments_replace_by_id_and_list_in_registration_order() {
    let (_p, c) = contributions();
    let _ = c.register_widget_attachment(WidgetAttachment::new("a", |_| Box::new(|| {})));
    let off_b = c.register_widget_attachment(WidgetAttachment::new("b", |_| Box::new(|| {})));
    let _ = c.register_widget_attachment(WidgetAttachment::new("a", |_| Box::new(|| {})));
    assert_eq!(ids(&c.widget_attachments(), |a| a.id.as_str()), ["a", "b"]);
    off_b();
    c.unregister_widget_attachment("a");
    assert!(c.widget_attachments().is_empty());
}

fn panel(id: &str, order: Option<f64>) -> SidePanelDescriptor {
    let mut p = SidePanelDescriptor::new(id, "Title", "icon", Rc::new(|_, _, _| None));
    p.order = order;
    p
}

#[test]
fn side_panels_sort_by_order_with_the_default_after_the_built_ins() {
    let (_p, c) = contributions();
    let _ = c.register_side_panel(panel("plain", None));
    let _ = c.register_side_panel(panel("first", Some(10.0)));
    let _ = c.register_side_panel(panel("tie", Some(DEFAULT_PANEL_ORDER)));
    let _ = c.register_side_panel(panel("last", Some(500.0)));
    assert_eq!(
        ids(&c.side_panels(), |p| p.id.as_str()),
        ["first", "plain", "tie", "last"]
    );
    c.unregister_side_panel("first");
    assert_eq!(c.side_panels().len(), 3);
}

// ── legend actions ──

fn lookup(infos: Vec<LegendIndicatorInfo>) -> IndicatorLookup {
    Rc::new(move |id| infos.iter().find(|i| i.id == id).cloned())
}

fn english() -> Translate {
    Rc::new(|m| text_of(None, m, &[]))
}

fn counting_contexts(log: &Rc<RefCell<Vec<u32>>>) -> ContextFactory {
    let built = Rc::new(Cell::new(0));
    let _ = log;
    Rc::new(move || {
        built.set(built.get() + 1);
        Rc::new(ctx(built.get()))
    })
}

#[test]
fn legend_actions_register_order_sort_and_gate_per_indicator() {
    let (_p, c) = contributions();
    type Seen = Rc<RefCell<Vec<(u32, String, Option<String>)>>>;
    let seen: Seen = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&seen);
    let _ = c.register_legend_action(LegendActionDescriptor {
        id: "edit".into(),
        icon: "pencil".into(),
        tooltip: "Edit".into(),
        order: Some(2.0),
        when: Some(Rc::new(|i| i.source.is_some())),
        run: Rc::new(move |ctx, info| {
            // The mock carries its build number through the symbol.
            record
                .borrow_mut()
                .push((0, info.id.clone(), info.source.clone()));
            let _ = ctx.symbol();
        }),
    });
    let _ = c.register_legend_action(LegendActionDescriptor {
        id: "any".into(),
        icon: "dot".into(),
        tooltip: "Any".into(),
        order: Some(1.0),
        when: None,
        run: Rc::new(|_, _| {}),
    });
    assert_eq!(ids(&c.legend_actions(), |a| a.id.as_str()), ["any", "edit"]);

    let infos = vec![
        LegendIndicatorInfo {
            id: "ind-1".into(),
            title: "EMA".into(),
            source: Some("plot(close)".into()),
            language: Some("pine".into()),
        },
        LegendIndicatorInfo {
            id: "native-1".into(),
            title: "Volume".into(),
            ..LegendIndicatorInfo::default()
        },
    ];
    let log = Rc::new(RefCell::new(Vec::new()));
    let provider = c.legend_actions_provider(lookup(infos), english(), counting_contexts(&log));
    assert!(provider("missing").is_empty());
    let native: Vec<String> = provider("native-1").iter().map(|v| v.id.clone()).collect();
    assert_eq!(native, ["any"]); // the source-gated action stays off a native row
    let views = provider("ind-1");
    assert_eq!(
        views.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ["any", "edit"]
    );
    assert_eq!(
        (views[1].icon.as_str(), views[1].tooltip.as_str()),
        ("pencil", "Edit")
    );
    (views[1].run)();
    assert_eq!(
        seen.borrow().as_slice(),
        [(0, "ind-1".to_string(), Some("plot(close)".to_string()))]
    );
}

#[test]
fn a_legend_action_reads_the_indicator_again_at_click_time() {
    let (_p, c) = contributions();
    let sources: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&sources);
    let _ = c.register_legend_action(LegendActionDescriptor {
        id: "edit".into(),
        icon: "pencil".into(),
        tooltip: "Edit".into(),
        order: None,
        when: None,
        run: Rc::new(move |_, info| record.borrow_mut().push(info.source.clone())),
    });
    let current = Rc::new(RefCell::new(Some("v1".to_string())));
    let live = Rc::clone(&current);
    let find: IndicatorLookup = Rc::new(move |id| {
        (id == "ind").then(|| LegendIndicatorInfo {
            id: "ind".into(),
            title: "T".into(),
            source: live.borrow().clone(),
            language: None,
        })
    });
    let log = Rc::new(RefCell::new(Vec::new()));
    let provider = c.legend_actions_provider(find, english(), counting_contexts(&log));
    let views = provider("ind");
    *current.borrow_mut() = Some("v2".to_string()); // an in-place code update after the row was built
    (views[0].run)();
    assert_eq!(sources.borrow().as_slice(), [Some("v2".to_string())]);
}

// ── legend callouts ──

fn callout(id: &str, order: Option<f64>, resolve: CalloutResolve) -> LegendCalloutDescriptor {
    LegendCalloutDescriptor {
        id: id.into(),
        order,
        callout: resolve,
    }
}

fn plain_spec(tooltip: &'static str) -> LegendCalloutSpec {
    LegendCalloutSpec {
        icon: "i".into(),
        background: "b".into(),
        color: None,
        tooltip: tooltip.into(),
        content: None,
    }
}

fn info(id: &str, title: &str) -> LegendIndicatorInfo {
    LegendIndicatorInfo {
        id: id.into(),
        title: title.into(),
        ..LegendIndicatorInfo::default()
    }
}

#[test]
fn callouts_register_order_sort_replace_by_id_and_unregister() {
    let (_p, c) = contributions();
    let d1 = c.register_legend_callout(callout("ca", Some(2.0), Rc::new(|_| None)));
    let _ = c.register_legend_callout(callout("cb", Some(1.0), Rc::new(|_| None)));
    assert_eq!(ids(&c.legend_callouts(), |d| d.id.as_str()), ["cb", "ca"]);

    let _ = c.register_legend_callout(callout("ca", None, Rc::new(|_| Some(plain_spec("A2")))));
    let replaced = c
        .legend_callouts()
        .into_iter()
        .find(|d| d.id == "ca")
        .and_then(|d| (d.callout)(&info("x", "X")))
        .map(|s| text_of(None, &s.tooltip, &[]));
    assert_eq!(replaced.as_deref(), Some("A2"));
    d1(); // stale disposer must NOT remove the replacement
    assert!(c.legend_callouts().iter().any(|d| d.id == "ca"));

    c.unregister_legend_callout("ca");
    c.unregister_legend_callout("cb");
    assert!(c.legend_callouts().is_empty());
}

#[test]
fn the_callout_provider_resolves_the_row_skips_none_and_binds_a_fresh_context_per_click() {
    let (_p, c) = contributions();
    let seen: Rc<RefCell<Vec<LegendIndicatorInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let builds: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let (seen2, builds2) = (Rc::clone(&seen), Rc::clone(&builds));
    let run: LegendRun = Rc::new(move |ctx, i| {
        // The context's build number rides on its timeframe-free surface: count calls instead.
        builds2.borrow_mut().push(ctx.symbol().len() as u32);
        seen2.borrow_mut().push(i.clone());
    });
    let _ = c.register_legend_callout(callout(
        "status",
        None,
        Rc::new(move |ind| {
            // Natives show no bubble: the per-indicator gate.
            ind.source.as_ref()?;
            Some(LegendCalloutSpec {
                icon: "market-open".into(),
                background: "var(--vela-up)".into(),
                color: Some("red".into()),
                tooltip: format!("Status of {}", ind.title).into(),
                content: Some(LegendCalloutContent {
                    title: Some("Session".into()),
                    items: vec![
                        LegendCalloutItem::Text {
                            text: "Market is open.".into(),
                        },
                        LegendCalloutItem::Button {
                            label: "Details".into(),
                            primary: None,
                            close: None,
                            run: Rc::clone(&run),
                        },
                    ],
                }),
            })
        }),
    ));
    let infos = vec![
        LegendIndicatorInfo {
            id: "ind-1".into(),
            title: "EMA".into(),
            source: Some("plot(close)".into()),
            language: Some("pine".into()),
        },
        info("native-1", "Volume"),
    ];
    let made = Rc::new(Cell::new(0));
    let counter = Rc::clone(&made);
    let context: ContextFactory = Rc::new(move || {
        counter.set(counter.get() + 1);
        Rc::new(ctx(counter.get()))
    });
    let provider = c.legend_callouts_provider(lookup(infos), english(), context);

    assert!(provider("missing").is_empty()); // an unknown row contributes nothing
    assert!(provider("native-1").is_empty()); // None spec: no bubble
    let views = provider("ind-1");
    assert_eq!(views.len(), 1);
    assert_eq!(
        (
            views[0].id.as_str(),
            views[0].icon.as_str(),
            views[0].background.as_str(),
            views[0].color.as_deref(),
            views[0].tooltip.as_str()
        ),
        (
            "status",
            "market-open",
            "var(--vela-up)",
            Some("red"),
            "Status of EMA"
        )
    );
    let panel = views[0].content.as_ref().expect("a panel");
    assert_eq!(panel.title.as_deref(), Some("Session"));
    assert!(
        matches!(&panel.items[0], LegendCalloutPanelItem::Text { text } if text == "Market is open.")
    );
    let LegendCalloutPanelItem::Button {
        run,
        label,
        primary,
        close,
        ..
    } = &panel.items[1]
    else {
        panic!("expected a button item");
    };
    assert_eq!((label.as_str(), *primary, *close), ("Details", false, None));
    made.set(0);
    run();
    run();
    assert_eq!(made.get(), 2); // never a cached context
    assert_eq!(seen.borrow()[0].source.as_deref(), Some("plot(close)"));
    assert_eq!(seen.borrow()[0].language.as_deref(), Some("pine"));
    assert_eq!(builds.borrow().len(), 2);
}

#[test]
fn a_spec_without_content_projects_a_bubble_without_a_panel() {
    let (_p, c) = contributions();
    let _ = c.register_legend_callout(callout("plain", None, Rc::new(|_| Some(plain_spec("t")))));
    let provider = c.legend_callouts_provider(
        lookup(vec![info("ind-1", "T")]),
        english(),
        Rc::new(|| Rc::new(ctx(0))),
    );
    let views = provider("ind-1");
    assert_eq!(views.len(), 1);
    assert!(views[0].content.is_none());
}

#[test]
fn a_callout_button_wears_its_primary_and_close_flags() {
    let (_p, c) = contributions();
    let _ = c.register_legend_callout(callout(
        "flags",
        None,
        Rc::new(|_| {
            Some(LegendCalloutSpec {
                content: Some(LegendCalloutContent {
                    title: None,
                    items: vec![LegendCalloutItem::Button {
                        label: "Go".into(),
                        primary: Some(true),
                        close: Some(false),
                        run: Rc::new(|_, _| {}),
                    }],
                }),
                ..plain_spec("t")
            })
        }),
    ));
    let provider = c.legend_callouts_provider(
        lookup(vec![info("i", "T")]),
        english(),
        Rc::new(|| Rc::new(ctx(0))),
    );
    let views = provider("i");
    let panel = views[0].content.as_ref().expect("a panel");
    assert!(panel.title.is_none());
    assert!(matches!(
        &panel.items[0],
        LegendCalloutPanelItem::Button {
            primary: true,
            close: Some(false),
            ..
        }
    ));
    assert!(!panel.items[0].closes_panel());
}

// ── state persistence ──

fn cell_handler(key: &str) -> StatePersistenceHandler {
    let tag = key.to_string();
    StatePersistenceHandler::Cell {
        key: key.into(),
        serialize: Rc::new(move |_| Some(json!({ "from": tag }))),
        restore: Rc::new(|_, _| {}),
    }
}

fn global_handler(key: &str) -> StatePersistenceHandler {
    let tag = key.to_string();
    StatePersistenceHandler::Global {
        key: key.into(),
        serialize: Rc::new(move |_| Some(json!({ "from": tag }))),
        restore: Rc::new(|_, _| {}),
    }
}

fn keys(list: &[Rc<StatePersistenceHandler>]) -> Vec<String> {
    list.iter().map(|h| h.key().to_string()).collect()
}

#[test]
fn state_handlers_list_by_scope_in_registration_order_and_unregister() {
    let (_p, c) = contributions();
    let _ = c.register_state_persistence(cell_handler("t.cell-a"));
    let _ = c.register_state_persistence(global_handler("t.global-a"));
    let _ = c.register_state_persistence(cell_handler("t.cell-b"));
    assert_eq!(
        keys(&c.state_persistence_handlers(StateScope::Cell)),
        ["t.cell-a", "t.cell-b"]
    );
    assert_eq!(
        keys(&c.state_persistence_handlers(StateScope::Global)),
        ["t.global-a"]
    );
    c.unregister_state_persistence("t.cell-a");
    assert_eq!(
        keys(&c.state_persistence_handlers(StateScope::Cell)),
        ["t.cell-b"]
    );
}

#[test]
fn re_registering_a_key_replaces_the_handler_across_scopes_too() {
    let (_p, c) = contributions();
    let _ = c.register_state_persistence(cell_handler("t.swap"));
    let _ = c.register_state_persistence(global_handler("t.swap")); // same key, new scope: replaces
    assert!(!keys(&c.state_persistence_handlers(StateScope::Cell)).contains(&"t.swap".to_string()));
    assert!(
        keys(&c.state_persistence_handlers(StateScope::Global)).contains(&"t.swap".to_string())
    );
}

#[test]
fn a_state_disposer_removes_its_own_registration_only() {
    let (_p, c) = contributions();
    let dispose = c.register_state_persistence(cell_handler("t.swap"));
    let _ = c.register_state_persistence(cell_handler("t.swap"));
    dispose(); // stale disposer: the key now belongs to the second handler
    assert_eq!(
        keys(&c.state_persistence_handlers(StateScope::Cell)),
        ["t.swap"]
    );
}

// ── symbol ranking and default engines ──

#[test]
fn one_symbol_ranking_at_a_time_last_wins_and_a_stale_disposer_is_inert() {
    let (_p, c) = contributions();
    assert!(c.symbol_ranking().is_none());
    let first: SymbolRankingHook = Rc::new(RankedPool::Ready);
    let dispose_first = c.register_symbol_ranking(Rc::clone(&first));
    let second: SymbolRankingHook = Rc::new(|mut pool| {
        pool.reverse();
        RankedPool::Ready(pool)
    });
    let dispose_second = c.register_symbol_ranking(Rc::clone(&second));
    dispose_first();
    assert!(c.symbol_ranking().is_some_and(|h| Rc::ptr_eq(&h, &second)));
    dispose_second();
    assert!(c.symbol_ranking().is_none());
}

fn symbol(ticker: &str) -> SymbolDescriptor {
    serde_json::from_value(json!({ "ticker": ticker })).expect("a descriptor")
}

#[test]
fn a_ranking_can_reorder_inject_and_omit_entries() {
    let (_p, c) = contributions();
    let hook: SymbolRankingHook = Rc::new(|pool| {
        let mut out = vec![symbol("INJECTED")];
        out.extend(pool.into_iter().filter(|s| s.ticker != "HIDDEN"));
        RankedPool::Ready(out)
    });
    let _ = c.register_symbol_ranking(hook);
    let ranking = c.symbol_ranking().expect("a ranking");
    let RankedPool::Ready(out) = ranking(vec![symbol("A"), symbol("HIDDEN"), symbol("B")]) else {
        panic!("expected an immediate answer");
    };
    assert_eq!(
        out.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
        ["INJECTED", "A", "B"]
    );
}

fn engine_factory() -> EngineFactory {
    Rc::new(|| unimplemented!("never built in these tests"))
}

#[test]
fn default_engines_merge_under_the_per_instance_ones() {
    let (_p, c) = contributions();
    let pine = engine_factory();
    let _ = c.register_default_engine("pine", Rc::clone(&pine));
    let _ = c.register_default_engine("js", engine_factory());
    let resolved = c.resolve_engines(None);
    assert_eq!(
        resolved.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
        ["pine", "js"]
    );

    let mine = engine_factory();
    let overrides: HashMap<String, EngineFactory> = [
        ("pine".to_string(), Rc::clone(&mine)),
        ("lua".to_string(), engine_factory()),
    ]
    .into_iter()
    .collect();
    let merged = c.resolve_engines(Some(&overrides));
    assert_eq!(
        merged.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
        ["pine", "js", "lua"]
    );
    assert!(
        Rc::ptr_eq(&merged[0].1, &mine),
        "an instance factory wins for its language"
    );
    assert!(
        Rc::ptr_eq(&c.resolve_engines(None)[0].1, &pine),
        "the registry itself is untouched"
    );
}

#[test]
fn a_default_engine_disposer_removes_only_its_own_and_unregister_drops_by_language() {
    let (_p, c) = contributions();
    let first = c.register_default_engine("pine", engine_factory());
    let second = engine_factory();
    let _ = c.register_default_engine("pine", Rc::clone(&second));
    first(); // stale
    let after = c.resolve_engines(None);
    assert_eq!(after.len(), 1);
    assert!(Rc::ptr_eq(&after[0].1, &second));
    c.unregister_default_engine("pine");
    assert!(c.resolve_engines(None).is_empty());
}

#[test]
fn clones_share_the_registries() {
    let (_p, c) = contributions();
    let other = c.clone();
    let _ = other.register_widget_action(action("shared", WidgetActionTarget::Topbar));
    assert_eq!(c.widget_actions(WidgetActionTarget::Topbar, None).len(), 1);
}
