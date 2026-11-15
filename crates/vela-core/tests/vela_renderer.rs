//! The renderer wiring of `Vela::new`: the factory option and its display
//! options, renderer defaults, themes, the settings policy and the missing
//! renderer (`orchestrator.test.ts` theme and colors, `price-axis-ticks.test.ts`,
//! `renderer-defaults.test.ts`).

mod vela_common;

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use vela_common::*;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::options::{
    PriceAxisOptions, RendererConstructor, RendererDisplayOptions, ThemeSpec, VelaOptions,
};
use vela_core::ports::chart_renderer::ChartRenderer;
use vela_core::runtime::testing::ManualPlatform;
use vela_core::theme::{RegisteredTheme, dark_theme, light_theme};
use vela_core::vela::{Vela, VelaDeps, VelaEnv, VelaError};

/// A renderer factory that records the display options it was called with and
/// hands out the one recording renderer.
fn capturing() -> (
    RendererConstructor,
    Rc<RefCell<Vec<RendererDisplayOptions>>>,
    Rc<RecRenderer>,
) {
    let seen: Rc<RefCell<Vec<RendererDisplayOptions>>> = Rc::default();
    let renderer = RecRenderer::new();
    let (sink, out) = (Rc::clone(&seen), Rc::clone(&renderer));
    let make: RendererConstructor = Rc::new(move |display| {
        sink.borrow_mut().push(display);
        Rc::clone(&out) as Rc<dyn ChartRenderer>
    });
    (make, seen, renderer)
}

fn env() -> (ManualPlatform, VelaEnv) {
    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    (platform, env)
}

fn data_options() -> VelaOptions {
    let mut options = VelaOptions::default();
    options.market.data = Some(bars(20));
    options.volume = Some(false);
    options
}

// ── the renderer option ────────────────────────────────────

#[test]
fn construction_without_a_renderer_fails_with_a_clear_error() {
    let (_platform, env) = env();
    let err = Vela::new(&env, data_options()).err().expect("an error");
    assert_eq!(err, VelaError::MissingRenderer);
    assert!(err.to_string().contains("options.renderer is required"));
}

#[test]
fn the_renderer_factory_gets_the_resolved_display_options_and_the_theme_colors() {
    let (_platform, env) = env();
    let (make, seen, _renderer) = capturing();
    let mut options = data_options();
    options.renderer = Some(make);
    let _chart = Vela::new(&env, options).expect("built");
    let seen = seen.borrow();
    let d = &seen[0];
    let dark = dark_theme();
    assert_eq!(Some(d.up_color.as_str()), dark["upColor"].as_str());
    assert_eq!(Some(d.down_color.as_str()), dark["downColor"].as_str());
    assert!(d.current_price_line);
    assert!(!d.log_scale);
    assert_eq!(d.price_style, "candles");
    assert_eq!(d.glow, 0.0);
    assert!(d.price_axis_ticks.is_none());
}

#[test]
fn candle_colors_follow_the_theme_unless_the_host_sets_them() {
    let (_platform, env) = env();
    let (make, seen, _renderer) = capturing();
    let theme = json!({
        "background": "#000000", "textColor": "#ffffff", "gridColor": "#111111",
        "borderColor": "#222222", "upColor": "#00ff00", "downColor": "#ff0000",
        "fontFamily": "sans-serif",
    });
    let mut options = data_options();
    options.renderer = Some(Rc::clone(&make));
    options.theme = Some(ThemeSpec::Theme(theme));
    let _a = Vela::new(&env, options.clone()).expect("built");
    options.up_color = Some("#111111".to_string());
    let _b = Vela::new(&env, options).expect("built");
    let seen = seen.borrow();
    assert_eq!(
        (seen[0].up_color.as_str(), seen[0].down_color.as_str()),
        ("#00ff00", "#ff0000")
    );
    assert_eq!(
        (seen[1].up_color.as_str(), seen[1].down_color.as_str()),
        ("#111111", "#ff0000")
    );
}

#[test]
fn a_registered_theme_id_resolves_through_the_environment_registry() {
    let (_platform, env) = env();
    let theme = json!({
        "background": "#010101", "textColor": "#fefefe", "gridColor": "#111111",
        "borderColor": "#222222", "upColor": "#0a0a0a", "downColor": "#0b0b0b",
        "fontFamily": "serif",
    });
    env.registries
        .themes
        .borrow_mut()
        .register(RegisteredTheme {
            id: "midnight".to_string(),
            label: "Midnight".to_string(),
            theme,
        })
        .expect("registered");
    let (make, seen, renderer) = capturing();
    let mut options = data_options();
    options.renderer = Some(make);
    options.theme = Some("midnight".into());
    let chart = Vela::new(&env, options).expect("built");
    assert_eq!(seen.borrow()[0].up_color, "#0a0a0a");
    assert_eq!(
        renderer
            .mounted_with
            .borrow()
            .as_ref()
            .and_then(|t| t["background"].as_str().map(str::to_string)),
        Some("#010101".to_string())
    );
    // An unknown id falls back to dark.
    chart.set_theme("nope");
    assert_eq!(renderer.themes.borrow().last().cloned(), Some(dark_theme()));
}

#[test]
fn price_axis_ticks_and_style_reach_the_renderer_display_options() {
    let (_platform, env) = env();
    let (make, seen, _renderer) = capturing();
    let mut options = data_options();
    options.renderer = Some(Rc::clone(&make));
    options.price_axis = Some(PriceAxisOptions {
        ticks: Some(Rc::new(|_| None)),
    });
    options.price_style = Some("line".to_string());
    options.log_scale = Some(true);
    options.current_price_line = Some(false);
    options.glow = Some(0.5);
    let _a = Vela::new(&env, options).expect("built");
    let mut plain = data_options();
    plain.renderer = Some(make);
    let _b = Vela::new(&env, plain).expect("built");
    let seen = seen.borrow();
    assert!(seen[0].price_axis_ticks.is_some());
    assert_eq!(seen[0].price_style, "line");
    assert!(seen[0].log_scale);
    assert!(!seen[0].current_price_line);
    assert_eq!(seen[0].glow, 0.5);
    assert!(seen[1].price_axis_ticks.is_none());
}

#[test]
fn an_injected_renderer_wins_over_the_option_factory() {
    let (_platform, env) = env();
    let (make, seen, _unused) = capturing();
    let injected = RecRenderer::new();
    let mut options = data_options();
    options.renderer = Some(make);
    let chart = Vela::with_deps(
        &env,
        options,
        VelaDeps {
            renderer: Some(Rc::clone(&injected) as Rc<dyn ChartRenderer>),
            ..VelaDeps::default()
        },
    )
    .expect("built");
    assert!(seen.borrow().is_empty(), "the factory was never called");
    assert!(injected.mounted_with.borrow().is_some());
    drop(chart);
}

// ── theme ──────────────────────────────────────────────────

#[test]
fn set_theme_reskins_the_renderer_and_emits_theme_changed_once() {
    let rig = Rig::builder().build();
    let seen: Rc<RefCell<Vec<Value>>> = Rc::default();
    let sink = Rc::clone(&seen);
    rig.chart.on(ChartEventName::ThemeChanged, move |e| {
        if let ChartEvent::ThemeChanged(theme) = e {
            sink.borrow_mut().push(theme.clone());
        }
    });
    rig.chart.set_theme("light");
    assert_eq!(*seen.borrow(), [light_theme()]);
    assert_eq!(
        rig.renderer.themes.borrow().last().cloned(),
        Some(light_theme())
    );
    // The same theme again is a no-op, which breaks the echo of a host that reacts.
    rig.chart.set_theme("light");
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn a_theme_selected_in_the_renderer_dialog_is_applied_and_announced() {
    let rig = Rig::builder().build();
    rig.renderer.fire_theme_select("light");
    assert_eq!(rig.log.count(ChartEventName::ThemeChanged), 1);
    assert_eq!(
        rig.renderer.themes.borrow().last().cloned(),
        Some(light_theme())
    );
}

// ── renderer defaults ──────────────────────────────────────

fn feature_renderer(r: &RecRenderer) {
    *r.feature_names.borrow_mut() = vec!["attribution".to_string(), "gridlines".to_string()];
}

fn defaults(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn a_chart_applies_every_registered_default_at_construction() {
    let rig = Rig::builder()
        .renderer(feature_renderer)
        .env(|env| {
            env.renderer_defaults.borrow_mut().register(defaults(&[
                ("attribution", json!(false)),
                ("gridlines", json!(false)),
            ]));
        })
        .build();
    assert_eq!(
        *rig.renderer.applied_features.borrow(),
        [
            ("attribution".to_string(), json!(false)),
            ("gridlines".to_string(), json!(false))
        ]
    );
}

#[test]
fn a_chart_built_before_the_registration_is_untouched_and_an_empty_registry_pushes_nothing() {
    let rig = Rig::builder().renderer(feature_renderer).build();
    assert!(rig.renderer.applied_features.borrow().is_empty());
    rig.env
        .renderer_defaults
        .borrow_mut()
        .register(defaults(&[("attribution", json!(false))]));
    assert!(
        rig.renderer.applied_features.borrow().is_empty(),
        "registering later does not reach back"
    );
}

#[test]
fn defaults_are_not_locks_an_explicit_set_afterwards_wins() {
    let rig = Rig::builder()
        .renderer(feature_renderer)
        .env(|env| {
            env.renderer_defaults
                .borrow_mut()
                .register(defaults(&[("attribution", json!(false))]));
        })
        .build();
    rig.chart
        .renderer()
        .set("attribution", json!("<b>ACME</b>"));
    assert_eq!(
        *rig.renderer.applied_features.borrow(),
        [
            ("attribution".to_string(), json!(false)),
            ("attribution".to_string(), json!("<b>ACME</b>"))
        ]
    );
}

#[test]
fn a_default_the_renderer_does_not_support_is_warned_about_and_ignored() {
    let rig = Rig::builder()
        .renderer(feature_renderer)
        .env(|env| {
            env.renderer_defaults
                .borrow_mut()
                .register(defaults(&[("glow", json!(0.6))]));
        })
        .build();
    assert!(rig.renderer.applied_features.borrow().is_empty());
    assert!(
        rig.platform
            .warnings()
            .iter()
            .any(|w| w.contains("no feature \"glow\""))
    );
}

#[test]
fn charts_on_two_environments_do_not_share_defaults() {
    let a = Rig::builder().renderer(feature_renderer).build();
    a.env
        .renderer_defaults
        .borrow_mut()
        .register(defaults(&[("attribution", json!(false))]));
    let b = Rig::builder().renderer(feature_renderer).build();
    assert!(b.renderer.applied_features.borrow().is_empty());
    let c = Rig::builder()
        .renderer(feature_renderer)
        .build_on(a.platform.clone(), a.env.clone());
    assert_eq!(c.renderer.applied_features.borrow().len(), 1);
}

// ── settings policy and controls ───────────────────────────

#[test]
fn the_renderer_control_reads_and_writes_features_through_the_chart() {
    let rig = Rig::builder().renderer(feature_renderer).build();
    assert!(rig.chart.renderer().supports("gridlines"));
    assert!(!rig.chart.renderer().supports("glow"));
    assert_eq!(rig.chart.renderer().name(), "fake");
}
