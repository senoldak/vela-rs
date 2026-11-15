//! The end-to-end test and the lifecycle / intro / theme / host seam checks.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use serde_json::json;

use super::*;
use crate::model::{Pane, PaneKind};
use crate::ports::chart_renderer::RendererRootAttributes;
use crate::render::canvas::Canvas2d;
use crate::render::core::scene_graph::DrawingSlice;

/// A user-drawings stand-in that paints one rectangle on the drawings canvas.
struct PaintingHook {
    ctx: Rc<dyn Canvas2d>,
    renders: Cell<usize>,
    resizes: Cell<usize>,
}

impl UserDrawingsHook for PaintingHook {
    fn prepare_slices(&self, _: &[String], _: &FrameView) -> HashMap<String, Vec<DrawingSlice>> {
        HashMap::new()
    }
    fn render(&self, view: &FrameView) {
        self.renders.set(self.renders.get() + 1);
        self.ctx.clear_rect(0.0, 0.0, 10.0, 10.0);
        self.ctx
            .fill_rect(1.0, 1.0, view.coords.width() / 10.0, 4.0);
    }
    fn on_resize(&self) {
        self.resizes.set(self.resizes.get() + 1);
    }
}

fn pile_slots() -> [CanvasSlot; 7] {
    [
        CanvasSlot::Backdrop,
        CanvasSlot::Data,
        CanvasSlot::Volume,
        CanvasSlot::Vpvr,
        CanvasSlot::Chrome,
        CanvasSlot::Drawings,
        CanvasSlot::Cursor,
    ]
}

fn painted(host: &TestHost, slot: &CanvasSlot) -> bool {
    host.canvas(slot).is_some_and(|c| !c.is_empty())
}

#[test]
fn a_chart_mounts_takes_bars_and_two_indicators_and_paints_every_canvas_then_repaints_on_pan_zoom_and_resize()
 {
    let rig = rig_with(false, 864.0, 522.0);
    // the drawings layer plugs in through the seam
    let drawings = rig.host.canvas(&CanvasSlot::Drawings).unwrap();
    let hook = Rc::new(PaintingHook {
        ctx: drawings.clone(),
        renders: Cell::new(0),
        resizes: Cell::new(0),
    });
    struct NoPort;
    impl DrawingsRendererPort for NoPort {
        fn set_toolbar(&self, _: &serde_json::Value) {}
        fn show_toolbar(&self, _: bool) {}
        fn sync_drawings(&self, _: &[crate::drawings::SerializedDrawing]) {}
        fn set_active_tool(
            &self,
            _: Option<&str>,
            _: Option<&crate::drawings::style::DrawingStyle>,
            _: Option<&crate::drawings::DrawingToolDefaults>,
        ) {
        }
        fn set_selection(&self, _: &[String]) {}
        fn open_settings(&self, _: &str) {}
        fn on_drawing_intent(
            &self,
            _: crate::ports::chart_renderer::Listener<crate::ports::chart_renderer::DrawingIntent>,
        ) -> crate::runtime::Unsubscribe {
            crate::runtime::noop_unsubscribe()
        }
    }
    assert!(rig.r.user_drawings_port().is_some()); // the default layer is attached
    rig.r
        .set_user_drawings(Some((hook.clone(), Rc::new(NoPort))));
    assert!(rig.r.user_drawings_port().is_some());

    // a volume layer and a VPVR layer, so those canvases have content too
    let bars = rig.bars(400);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    // one overlay on the price pane and one in its own pane
    let overlay: Vec<Option<f64>> = bars.iter().map(|b| Some(b.close)).collect();
    rig.mount_model(&line_model("sma", "price", true, &times, &overlay));
    rig.r.ensure_pane(&Pane {
        id: "rsi".into(),
        kind: PaneKind::Study,
        order: 1,
        height_weight: None,
        title: None,
    });
    let osc: Vec<Option<f64>> = (0..400)
        .map(|i| Some(50.0 + (i as f64 * 0.2).sin() * 20.0))
        .collect();
    rig.mount_model(&line_model("rsi", "rsi", false, &times, &osc));
    rig.host.run_until_idle();
    assert_eq!(rig.host.pending_frames(), 0);

    for slot in pile_slots() {
        assert!(painted(&rig.host, &slot), "{slot:?} stayed blank");
    }
    assert!(hook.renders.get() > 0);
    // the data canvas drew the candles AND both indicators' lines
    let data = rig.host.canvas(&CanvasSlot::Data).unwrap();
    assert!(data.calls("stroke").len() >= 2);
    assert!(!data.calls("fillRect").is_empty() || !data.calls("rect").is_empty());
    // the chrome drew the axes of both panes: price labels from the price pane's ticks
    let chrome = rig.host.canvas(&CanvasSlot::Chrome).unwrap();
    assert!(chrome.texts().len() > 6);

    // a pan repaints data and chrome
    rig.host.clear_canvases();
    rig.r.pan_by_pixels(120.0);
    rig.host.run_until_idle();
    for slot in [
        CanvasSlot::Data,
        CanvasSlot::Chrome,
        CanvasSlot::Backdrop,
        CanvasSlot::Cursor,
    ] {
        assert!(painted(&rig.host, &slot), "{slot:?} not repainted by a pan");
    }
    // a zoom glides (frames until settled) and repaints
    rig.host.clear_canvases();
    let bs0 = rig.viewport().0;
    let (anchor, w) = (
        rig.r.coordinates().right_edge_logical(),
        rig.r.coordinates().width(),
    );
    rig.r.zoom_to(bs0 * 2.0, anchor, w);
    let steps = rig.host.run_until_idle();
    assert!(steps > 2, "the zoom glided over several frames");
    assert!(rig.viewport().0 > bs0 * 1.9);
    assert!(painted(&rig.host, &CanvasSlot::Data));
    assert!(painted(&rig.host, &CanvasSlot::Chrome));
    // a resize re-sizes every canvas of the pile and repaints
    let before = rig
        .host
        .test_canvas(&CanvasSlot::Data)
        .unwrap()
        .resize_count();
    rig.host.clear_canvases();
    rig.host.set_size(700.0, 400.0);
    rig.r.resize();
    rig.host.run_until_idle();
    for slot in pile_slots() {
        let c = rig.host.test_canvas(&slot).unwrap();
        assert_eq!(c.backing_size(), (700.0, 400.0), "{slot:?}");
        assert_eq!(c.css_size(), (700.0, 400.0), "{slot:?}");
    }
    assert_eq!(
        rig.host
            .test_canvas(&CanvasSlot::Data)
            .unwrap()
            .resize_count(),
        before + 1
    );
    for slot in [
        CanvasSlot::Data,
        CanvasSlot::Chrome,
        CanvasSlot::Backdrop,
        CanvasSlot::Drawings,
    ] {
        assert!(
            painted(&rig.host, &slot),
            "{slot:?} not repainted by a resize"
        );
    }
    assert_eq!(hook.resizes.get(), 1);
    near(rig.r.coordinates().width(), 700.0 - 64.0, 1e-9);
    assert_eq!(rig.r.coordinates().height(), 400.0 - 22.0);
    // the second indicator really got its own pane under the first
    let (rt, rh) = bounds(&rig, "rsi");
    assert!(rt > 0.0 && rh > 0.0);
    // destroy tears everything down
    rig.r.destroy();
    assert!(!rig.r.is_mounted());
    assert!(rig.host.slots().is_empty());
}

fn bounds(rig: &Rig, id: &str) -> (f64, f64) {
    rig.r.with(|st| {
        let p = st.scene.panes.get(id).unwrap();
        (p.bounds.top, p.bounds.height)
    })
}

// ── mount and lifecycle ──

#[test]
fn mount_builds_the_pile_in_order_sizes_it_and_publishes_the_chrome() {
    let rig = rig_with(false, 864.0, 422.0);
    let slots = rig.host.slots();
    assert_eq!(slots.len(), 7);
    let c = rig.host.test_canvas(&CanvasSlot::Data).unwrap();
    assert_eq!(c.backing_size(), (864.0, 422.0));
    assert_eq!(rig.r.coordinates().width(), 800.0);
    assert_eq!(rig.r.coordinates().height(), 400.0);
    assert!(!rig.host.chrome_themes().is_empty());
    assert_eq!(
        rig.host.background().as_deref(),
        dark_theme()["background"].as_str()
    );
    assert_eq!(rig.host.keyboard_enabled(), Some(true));
    assert!(rig.r.is_mounted());
    // an empty chart still lays out the price pane
    assert_eq!(rig.r.with(|st| st.scene.ordered_pane_ids()), ["price"]);
    let a = rig.r.attribution_state();
    assert!(a.enabled);
    assert_eq!(a.ink, "#ffffff");
}

#[test]
fn the_device_pixel_ratio_sizes_the_backing_store_and_the_exact_device_box_wins() {
    let host = TestHost::new(400.0, 300.0, 1.5);
    let r = NativeRenderer::new(host.clone(), Some(display(false)));
    r.mount(&dark_theme());
    let c = host.test_canvas(&CanvasSlot::Data).unwrap();
    assert_eq!(c.backing_size(), (600.0, 450.0));
    near(c.css_size().0, 400.0, 1e-9);
    assert_eq!(r.coordinates().dpr(), 1.5);
    host.set_device_size(Some((601.0, 450.0)));
    r.resize();
    assert_eq!(c.backing_size().0, 601.0);
    // a stale report (more than a pixel off) is ignored
    host.set_device_size(Some((900.0, 450.0)));
    r.resize();
    assert_eq!(c.backing_size().0, 600.0);
}

#[test]
fn bars_set_before_the_first_layout_fit_once_the_plot_is_sized() {
    let host = TestHost::new(0.0, 0.0, 1.0);
    let r = NativeRenderer::new(host.clone(), Some(display(false)));
    r.mount(&dark_theme());
    r.set_bars(&flat_bars(300, 0), SetBarsOptions::default());
    host.set_size(864.0, 222.0);
    r.resize();
    let v = r.coordinates().viewport();
    near(v.bar_spacing, 800.0 / (200.0 + 10.0), 1e-3);
}

#[test]
fn the_intro_reveals_candles_then_fades_the_indicators_and_settles_to_the_real_series() {
    let rig = rig_with(true, 864.0, 422.0);
    let bars = sample_bars(100, T0, STEP);
    rig.host.clear_canvases();
    rig.r.set_bars(&bars, SetBarsOptions::default());
    assert_eq!(
        rig.host.pending_frames(),
        1,
        "the reveal owns the paint: only its frame is armed"
    );
    assert!(rig.host.canvas(&CanvasSlot::Data).unwrap().is_empty());
    assert_eq!(rig.r.with(|st| st.model_alpha), 0.0);
    rig.host.step_frame(16.0);
    let mid = rig.r.with(|st| st.scene.bars.clone());
    assert_ne!(mid, bars, "a partially revealed series is on screen");
    assert!(!rig.host.canvas(&CanvasSlot::Data).unwrap().is_empty());
    rig.host.run_until_idle();
    rig.r.with(|st| {
        assert_eq!(st.scene.bars, bars);
        assert_eq!(st.model_alpha, 1.0);
        assert!(st.intro_raf.is_none());
    });
    // it plays once: a later replacement paints at once
    rig.r.set_bars(&bars, SetBarsOptions::default());
    assert_eq!(rig.r.with(|st| st.model_alpha), 1.0);
}

#[test]
fn a_re_set_interrupts_a_running_reveal_and_the_intro_feature_replays_it() {
    let rig = rig_with(true, 864.0, 422.0);
    let bars = sample_bars(100, T0, STEP);
    rig.r.set_bars(&bars, SetBarsOptions::default());
    rig.host.step_frame(16.0);
    rig.r.set_bars(&bars, SetBarsOptions::default());
    assert_eq!(rig.r.with(|st| st.model_alpha), 1.0);
    assert!(rig.r.with(|st| st.intro_raf.is_none()));
    rig.host.run_until_idle();
    rig.r.apply_feature("intro", &json!("grow"));
    assert_eq!(rig.r.read_feature("intro"), Some(json!("grow")));
    assert!(rig.r.with(|st| st.intro_raf.is_some()));
    rig.host.run_until_idle();
    rig.r.apply_feature("intro", &json!(false));
    assert_eq!(rig.r.read_feature("intro"), Some(json!(false)));
}

#[test]
fn the_animation_features_resolve_durations_and_the_pan_umbrella_drives_the_scroll_glide() {
    let rig = rig();
    assert_eq!(rig.feature("animZoom"), json!(70));
    rig.r.apply_feature("animZoom", &json!(false));
    assert_eq!(rig.feature("animZoom"), json!(0));
    rig.r.apply_feature("animZoom", &json!(true));
    assert_eq!(rig.feature("animZoom"), json!(70));
    rig.r.apply_feature("animZoom", &json!(5000));
    assert_eq!(rig.feature("animZoom"), json!(1000));
    rig.r.apply_feature("animPan", &json!(false));
    assert_eq!(rig.feature("animPan"), json!(0));
    assert_eq!(rig.feature("animScroll"), json!(0));
    rig.r.apply_feature("animPan", &json!(220));
    assert_eq!(rig.feature("animScroll"), json!(130));
    rig.r.apply_feature("animScroll", &json!(40));
    assert_eq!(rig.feature("animScroll"), json!(40));
    rig.r.apply_feature("animAutoscale", &json!(false));
    assert_eq!(rig.feature("animAutoscale"), json!(0));
    let c = rig.r.get_config().unwrap();
    assert_eq!(
        c["animations"],
        json!({ "zoom": true, "pan": true, "autoscale": false, "intro": false })
    );
}

#[test]
fn the_countdown_chip_ticks_on_the_hosts_second_pulse_at_the_chrome_tier_only() {
    let rig = rig_with(false, 864.0, 422.0);
    let clock = rig.host.with_clock();
    rig.bars(100);
    // the feature was on from the start but there was no clock at mount: re-arm it
    rig.r.apply_feature("countdown", &json!(false));
    rig.r.apply_feature("countdown", &json!(true));
    rig.host.run_until_idle();
    assert_eq!(clock.subscribers(), 1);
    rig.host.clear_canvases();
    clock.tick(1.0);
    rig.host.run_until_idle();
    assert!(painted(&rig.host, &CanvasSlot::Chrome));
    assert!(
        !painted(&rig.host, &CanvasSlot::Data),
        "the geometry layer stays untouched"
    );
    // the host clock replaces the renderer's own; ending the feature unsubscribes
    let host_clock = super::testing::TestWallClock::new();
    rig.r.set_wall_clock(Some(host_clock.clone()));
    assert_eq!(clock.subscribers(), 0);
    assert_eq!(host_clock.subscribers(), 1);
    rig.r.apply_feature("countdown", &json!(false));
    assert_eq!(host_clock.subscribers(), 0);
    rig.r.set_wall_clock(None);
}

#[test]
fn a_theme_switch_repaints_pushes_the_chrome_surface_and_flips_the_attribution_ink() {
    let rig = rig();
    rig.bars(50);
    rig.host.clear_canvases();
    let light = crate::theme::light_theme();
    rig.r.set_theme(&light);
    rig.host.run_until_idle();
    assert!(painted(&rig.host, &CanvasSlot::Data));
    assert_eq!(rig.host.background().as_deref(), Some("#ffffff"));
    let chrome = rig.host.chrome_themes().pop().unwrap();
    assert_eq!(chrome["background"], "#ffffff");
    assert_eq!(chrome["borderColor"], light["borderColor"]);
    assert_eq!(rig.r.attribution_state().ink, "#000000");
}

#[test]
fn localization_is_adopted_pushed_to_the_host_and_repaints() {
    let rig = rig();
    rig.bars(30);
    let intl = crate::intl::default_intl();
    let registry = crate::i18n::LocaleRegistry::new(intl.clone());
    let i18n: Rc<dyn crate::i18n::Translator> = Rc::new(crate::i18n::create_translator(
        crate::i18n::LocalizationOptions::default(),
        intl,
        registry,
    ));
    rig.host.clear_canvases();
    rig.r.set_localization(
        i18n,
        RendererRootAttributes {
            lang: Some("en".into()),
            dir: None,
        },
    );
    rig.host.run_until_idle();
    assert_eq!(
        rig.host.root_attributes().last().unwrap().lang.as_deref(),
        Some("en")
    );
    assert!(rig.r.with(|st| st.scene.format.is_some()));
    assert!(painted(&rig.host, &CanvasSlot::Chrome));
}

#[test]
fn indicators_mount_update_and_remove_through_the_legend_feed_and_the_scene() {
    let rig = rig();
    let bars = rig.bars(60);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    let handle = rig.mount_model(&line_model(
        "ma",
        "price",
        true,
        &times,
        &vec![Some(100.0); 60],
    ));
    assert_eq!(rig.host.legend_rows()[0].title, "ma");
    assert_eq!(rig.host.legend_rows()[0].pane_id.as_deref(), Some("price"));
    let mut hidden = line_model("quiet", "price", true, &times, &vec![Some(100.0); 60]);
    hidden.legend = Some(false);
    rig.mount_model(&hidden);
    assert_eq!(rig.host.legend_rows().len(), 1);
    rig.r
        .set_indicator_status(&handle, crate::indicators::IndicatorStatus::Loading);
    assert_eq!(
        rig.host.legend_statuses(),
        [(
            "ma".to_string(),
            crate::indicators::IndicatorStatus::Loading
        )]
    );
    // a value patch swaps the points
    let new_points: Vec<crate::model::SeriesPoint> = times
        .iter()
        .map(|t| crate::model::SeriesPoint {
            time: *t,
            value: Some(7.0),
            color: None,
        })
        .collect();
    let mut vp = value_patch("ma", AnchorUpdate::Keep);
    vp.series = vec![crate::model::SeriesValueDelta {
        series_id: "ma:line".into(),
        values: crate::model::SeriesValues::Points(new_points),
    }];
    rig.r.update_indicator(&handle, &ScenePatch::Value(vp));
    let model = rig.r.indicator_model("ma").unwrap();
    assert_eq!(model.series[0].points().unwrap()[0].value, Some(7.0));
    // a schema patch adds and removes series
    let spec = model.series[0].clone();
    let schema = ScenePatch::Schema(crate::model::SchemaPatch {
        indicator_id: "ma".into(),
        added: vec![crate::model::SeriesSpec {
            id: "ma:two".into(),
            ..spec
        }],
        removed: vec!["ma:line".into()],
        changed: vec![],
    });
    rig.r.update_indicator(&handle, &schema);
    let ids: Vec<String> = rig
        .r
        .indicator_model("ma")
        .unwrap()
        .series
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(ids, ["ma:two"]);
    // an unknown handle is ignored
    rig.r
        .update_indicator(&IndicatorRenderHandle { id: "zzz".into() }, &schema);
    rig.r.remove_indicator(&handle);
    assert!(rig.r.indicator_model("ma").is_none());
    assert_eq!(rig.host.legend_removed(), ["ma"]);
}

#[test]
fn the_host_providers_the_picker_and_the_settings_plumbing_are_held_for_the_dialogs() {
    let rig = rig();
    assert!(rig.r.supports_set_symbol_picker());
    assert!(rig.r.symbol_picker().is_none());
    rig.r
        .set_symbol_picker(Some(Rc::new(|_, done| done("X".into()))));
    assert!(rig.r.symbol_picker().is_some());
    rig.r.set_legend_overview_action(Some(Rc::new(|| {})));
    assert!(rig.r.legend_overview_action().is_some());
    assert!(!rig.r.supports_open_indicator_settings());
    rig.r
        .set_settings_visibility(&crate::ports::chart_renderer::SettingsVisibility {
            hidden: Some(vec!["symbol".into()]),
        });
    assert_eq!(rig.r.hidden_settings(), ["symbol"]);
    let ids = rig.r.list_settings_ids();
    assert!(!ids.is_empty());
    rig.r
        .set_layout_mode(crate::ports::chart_renderer::LayoutMode::Mobile);
    assert_eq!(
        rig.host.layout_modes(),
        [crate::ports::chart_renderer::LayoutMode::Mobile]
    );
}

#[test]
fn symbol_facts_that_rule_the_style_out_draw_the_fallback_and_announce_it() {
    let rig = rig();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = log.clone();
    let _u = rig
        .r
        .on_price_style_change(Box::new(move |s| l.borrow_mut().push(s.clone())));
    rig.r
        .set_symbol_facts(Some(crate::ports::market_data::SymbolFacts {
            ohlc: Some(false),
            volume: None,
        }));
    assert_eq!(
        rig.r.read_feature("shownPriceStyle"),
        Some(json!("linemarkers"))
    );
    assert_eq!(rig.r.read_feature("priceStyle"), Some(json!("candles")));
    assert_eq!(*log.borrow(), ["linemarkers"]);
    rig.r.set_symbol_facts(None);
    assert_eq!(
        rig.r.read_feature("shownPriceStyle"),
        Some(json!("candles"))
    );
}

#[test]
fn the_registered_sdk_layers_get_their_own_canvases_and_follow_their_owners_z() {
    use crate::render::backend::layers::{
        LayerPlacement, RendererLayerArgs, RendererLayerDefinition, RendererLayerInstance,
    };
    struct Probe(Rc<Cell<usize>>, Rc<RefCell<Option<Rc<dyn Canvas2d>>>>);
    impl RendererLayerInstance for Probe {
        fn mount(&mut self, canvas: Rc<dyn Canvas2d>) {
            *self.1.borrow_mut() = Some(canvas);
        }
        fn render(&mut self, args: &RendererLayerArgs) {
            self.0.set(self.0.get() + 1);
            if let Some(c) = self.1.borrow().as_ref() {
                c.fill_rect(0.0, 0.0, args.bounds.height, 1.0);
            }
        }
    }
    let renders = Rc::new(Cell::new(0));
    let ctx: Rc<RefCell<Option<Rc<dyn Canvas2d>>>> = Rc::default();
    let (r2, c2) = (renders.clone(), ctx.clone());
    let mut layers = crate::render::backend::layers::LayerRegistry::new();
    let mut def =
        RendererLayerDefinition::new("flow", move || Box::new(Probe(r2.clone(), c2.clone())));
    def.placement = Some(LayerPlacement::BelowData);
    layers.register(def);
    let host = TestHost::new(864.0, 422.0, 1.0);
    let r = NativeRenderer::with_parts(
        host.clone(),
        Some(display(false)),
        NativeParts {
            layers,
            ..Default::default()
        },
    );
    r.mount(&dark_theme());
    assert!(host.canvas(&CanvasSlot::Layer("flow".into())).is_some());
    r.set_bars(&flat_bars(50, 0), SetBarsOptions::default());
    host.run_until_idle();
    assert!(renders.get() > 0);
    assert!(
        !host
            .canvas(&CanvasSlot::Layer("flow".into()))
            .unwrap()
            .is_empty()
    );
    let (below, above) = host.layer_order().unwrap();
    assert_eq!((below, above), (vec!["flow".to_string()], vec![]));
    // hiding the candles blanks the chart type's own layer
    r.apply_feature("priceStyle", &json!("candles"));
    // a native indicator owning the layer stacks it by z: above the candles by default
    let mut native = line_model("n", "price", true, &[], &[]);
    native.series.clear();
    native.native = Some(crate::model::NativeTag {
        indicator_type: "flow".into(),
        channel: None,
    });
    r.mount_indicator(&native);
    host.run_until_idle();
    let (below, above) = host.layer_order().unwrap();
    assert_eq!((below, above), (vec![], vec!["flow".to_string()]));
    // a dedicated instance layer for a native naming its own channel
    let mut inst = native.clone();
    inst.id = "n2".into();
    inst.native = Some(crate::model::NativeTag {
        indicator_type: "flow".into(),
        channel: Some("flow#2".into()),
    });
    r.mount_indicator(&inst);
    assert!(host.canvas(&CanvasSlot::Layer("flow#2".into())).is_some());
    r.remove_indicator(&IndicatorRenderHandle { id: "n2".into() });
    assert!(host.canvas(&CanvasSlot::Layer("flow#2".into())).is_none());
    r.destroy();
    assert!(host.slots().is_empty());
}

#[test]
fn the_constructor_option_builds_a_renderer_from_resolved_display_options() {
    let host = TestHost::new(864.0, 422.0, 1.0);
    let ctor = NativeRenderer::constructor(host.clone(), NativeParts::default());
    let renderer: Rc<dyn ChartRenderer> = ctor(display(false));
    renderer.mount(&dark_theme());
    assert_eq!(renderer.name(), "native");
    assert!(renderer.capabilities().timeline_marks);
    assert!(renderer.supports_apply_config());
    assert!(renderer.supports_pan_by());
    renderer.set_bars(&flat_bars(10, 0), SetBarsOptions::default());
    host.run_until_idle();
    assert!(renderer.get_visible_range().is_some());
}

#[test]
fn the_screenshot_pile_runs_a_fresh_paint_and_lists_the_canvases_back_to_front() {
    let rig = rig();
    rig.bars(40);
    rig.host.clear_canvases();
    let pile = rig.r.screenshot_pile().unwrap();
    assert!(!rig.host.canvas(&CanvasSlot::Data).unwrap().is_empty());
    assert_eq!(pile.images.len(), 7 - 1); // everything but the crosshair canvas
    assert_eq!(
        pile.background,
        dark_theme()["background"].as_str().unwrap()
    );
    assert_eq!((pile.width, pile.height), (800.0, 420.0));
}
