//! Ports of the renderer-driven parts of `native-chart-config`: `getConfig` /
//! `applyConfig`, theme re-basing, stacking restore, the factory reset. (The pure
//! merge reducer and `default_chart_style` are covered in `core::chart_config`.)

use std::cell::RefCell;

use serde_json::{Value, json};

use super::*;
use crate::chart_types::ChartTypeDefinition;
use crate::theme::light_theme;

fn bare() -> Rc<NativeRenderer> {
    NativeRenderer::new(TestHost::new(800.0, 400.0, 1.0), None)
}

fn cfg(r: &NativeRenderer) -> Value {
    r.get_config().unwrap()
}

#[test]
fn get_config_emits_a_complete_versioned_document_from_the_dark_theme() {
    let c = cfg(&bare());
    assert_eq!(c["version"], 1);
    assert_eq!(
        c["layout"],
        json!({ "background": "#151619", "textColor": "#b2b5be", "fontFamily": "sans-serif", "fontSize": 11.0 })
    );
    assert_eq!(
        c["grid"]["vertLines"],
        json!({ "visible": true, "color": "#20222c" })
    );
    assert_eq!(
        c["grid"]["horzLines"],
        json!({ "visible": true, "color": "#20222c" })
    );
    assert_eq!(c["crosshair"]["color"], "#9aa0ad");
    assert_eq!(c["crosshair"]["style"], "dashed");
    assert_eq!(c["crosshair"]["labelBackground"], "#595959");
    let ps = &c["priceScale"];
    assert_eq!(ps["mode"], "price");
    assert_eq!(ps["log"], false);
    assert_eq!(ps["freePan"], false);
    assert_eq!(ps["borderColor"], "#2a2b30");
    assert_eq!(ps["countdown"], true);
    assert_eq!(ps["animateLastPrice"], false);
    assert_eq!(c["panes"], json!({ "separatorColor": "#2a2b30" }));
    assert_eq!(c["timeScale"], json!({ "timezone": "UTC" }));
    assert_eq!(c["candles"]["upColor"], "#089981");
    assert_eq!(c["candles"]["borderUpColor"], "#089981");
    assert_eq!(c["candles"]["wickDownColor"], "#f23645");
    assert_eq!(
        c["series"],
        json!({ "style": "candles", "baseline": null, "spacing": 1.0 })
    );
}

#[test]
fn the_document_round_trips_through_json() {
    let r = bare();
    let c = cfg(&r);
    let again: Value = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(again, c);
}

#[test]
fn apply_config_applies_a_partial_config_and_reflects_it() {
    let r = bare();
    r.apply_config(&json!({
        "candles": { "upColor": "#00ff00", "borderVisible": true, "wickVisible": false },
        "grid": { "vertLines": { "visible": false }, "horzLines": { "color": "#222222" } },
        "crosshair": { "color": "#ff0000", "opacity": 0.7, "width": 2 },
    }));
    let c = cfg(&r);
    assert_eq!(c["candles"]["upColor"], "#00ff00");
    assert_eq!(c["candles"]["borderVisible"], true);
    assert_eq!(c["candles"]["wickVisible"], false);
    assert_eq!(c["grid"]["vertLines"]["visible"], false);
    assert_eq!(c["grid"]["horzLines"]["color"], "#222222");
    assert_eq!(c["crosshair"]["color"], "#ff0000");
    assert_eq!(c["crosshair"]["opacity"], 0.7);
    assert_eq!(c["crosshair"]["width"], 2.0);
}

#[test]
fn spacing_margins_and_the_candle_body_flag_apply() {
    let r = bare();
    assert_eq!(cfg(&r)["series"]["spacing"], 1.0);
    r.apply_config(&json!({ "series": { "spacing": 2 } }));
    assert_eq!(cfg(&r)["series"]["spacing"], 2.0);
    r.apply_config(&json!({ "series": { "spacing": 0 } }));
    assert_eq!(cfg(&r)["series"]["spacing"], 0.1);
    r.apply_config(&json!({ "margins": { "top": 5, "bottom": 15, "right": 20 } }));
    assert_eq!(
        cfg(&r)["margins"],
        json!({ "top": 5.0, "bottom": 15.0, "right": 20.0 })
    );
    assert_eq!(cfg(&r)["candles"]["bodyVisible"], true);
    r.apply_config(&json!({ "candles": { "bodyVisible": false } }));
    assert_eq!(cfg(&r)["candles"]["bodyVisible"], false);
}

#[test]
fn a_config_syncs_the_scene_level_features() {
    let r = bare();
    r.apply_config(&json!({
        "priceScale": { "log": true, "mode": "percent", "labelsVisible": false, "currentPriceLine": false },
        "timeScale": { "timezone": "America/New_York" },
        "series": { "style": "area" },
    }));
    assert_eq!(r.read_feature("logScale"), Some(json!(true)));
    assert_eq!(r.read_feature("scaleMode"), Some(json!("percent")));
    assert_eq!(r.read_feature("axisLabels"), Some(json!(false)));
    assert_eq!(r.read_feature("currentPriceLine"), Some(json!(false)));
    assert_eq!(r.read_feature("timezone"), Some(json!("America/New_York")));
    assert_eq!(r.read_feature("priceStyle"), Some(json!("area")));
}

#[test]
fn apply_config_of_get_config_is_idempotent() {
    let r = bare();
    r.apply_config(&json!({
        "candles": { "upColor": "#abcdef" },
        "layout": { "fontSize": 14 },
        "priceScale": { "mode": "percent" },
    }));
    let a = cfg(&r);
    r.apply_config(&a);
    assert_eq!(cfg(&r), a);
}

#[test]
fn a_candle_color_in_a_config_updates_the_color_features() {
    let r = bare();
    r.apply_config(&json!({ "candles": { "upColor": "#101010", "downColor": "#202020" } }));
    assert_eq!(r.read_feature("upColor"), Some(json!("#101010")));
    assert_eq!(r.read_feature("downColor"), Some(json!("#202020")));
}

#[test]
fn a_background_in_the_other_luminance_class_re_bases_the_derived_inks() {
    let r = bare();
    let light = light_theme();
    let ink = |k: &str| light[k].as_str().unwrap().to_string();
    r.apply_config(&json!({ "layout": { "background": "#ffffff" } }));
    let c = cfg(&r);
    assert_eq!(c["layout"]["background"], "#ffffff");
    assert_eq!(c["layout"]["textColor"], ink("textColor"));
    assert_eq!(c["grid"]["vertLines"]["color"], ink("gridColor"));
    assert_eq!(c["priceScale"]["borderColor"], ink("borderColor"));
    assert_eq!(c["panes"]["separatorColor"], ink("borderColor"));
    r.apply_config(&json!({ "layout": { "background": "#151619" } }));
    let c = cfg(&r);
    assert_eq!(c["layout"]["textColor"], "#b2b5be");
    assert_eq!(c["grid"]["vertLines"]["color"], "#20222c");
}

#[test]
fn an_explicit_text_color_wins_and_a_same_class_edit_re_bases_nothing() {
    let r = bare();
    r.apply_config(&json!({ "layout": { "background": "#ffffff", "textColor": "#fafafa" } }));
    assert_eq!(cfg(&r)["layout"]["textColor"], "#fafafa");
    let r = bare();
    r.apply_config(&json!({ "layout": { "background": "#000000" } }));
    assert_eq!(cfg(&r)["layout"]["textColor"], "#b2b5be");
}

#[test]
fn a_restored_stacking_pre_seeds_an_indicator_that_has_not_mounted() {
    let r = bare();
    r.apply_config(&json!({ "stacking": { "candles": 2, "series": { "ind-1": 3 } } }));
    let c = cfg(&r);
    assert_eq!(c["stacking"]["candles"], 2.0);
    assert_eq!(c["stacking"]["series"]["ind-1"], 3.0);
    assert_eq!(r.read_feature("candleZOrder"), Some(json!(2)));
}

#[test]
fn a_config_change_is_reported_and_the_host_chrome_follows_the_theme() {
    let rig = rig();
    let hits = Rc::new(RefCell::new(0));
    let h = hits.clone();
    let _u = rig
        .r
        .on_config_changed(Box::new(move || *h.borrow_mut() += 1))
        .unwrap();
    let before = rig.host.chrome_themes().len();
    rig.r
        .apply_config(&json!({ "layout": { "background": "#222233" } }));
    assert_eq!(*hits.borrow(), 1);
    assert!(rig.host.chrome_themes().len() > before);
    assert_eq!(rig.host.background().as_deref(), Some("#222233"));
}

#[test]
fn the_chrome_surface_follows_the_app_theme_not_the_config_background() {
    let rig = rig();
    rig.r
        .apply_config(&json!({ "layout": { "background": "#ffffff" } }));
    let chrome = rig.host.chrome_themes().pop().unwrap();
    assert_eq!(chrome["background"], dark_theme()["background"]);
    assert_eq!(rig.host.background().as_deref(), Some("#ffffff"));
}

#[test]
fn chart_type_settings_are_pushed_to_the_types_channel_and_reported() {
    let rig = rig();
    rig.r.with(|st| {
        st.chart_types
            .borrow_mut()
            .register(ChartTypeDefinition::new("sdktype"));
    });
    let pushed: Rc<RefCell<Vec<(String, Value)>>> = Rc::default();
    let p = pushed.clone();
    let _u = rig
        .r
        .on_chart_type_settings_change(Box::new(move |id, vals| {
            p.borrow_mut()
                .push((id.to_string(), Value::Object(vals.clone())));
        }))
        .unwrap();
    rig.r
        .apply_config(&json!({ "chartTypes": { "sdktype": { "rows": 25, "shade": false } } }));
    assert_eq!(pushed.borrow().len(), 1);
    assert_eq!(pushed.borrow()[0].1, json!({ "rows": 25, "shade": false }));
    assert_eq!(
        rig.r
            .with(|st| st.scene.native_data.get("sdktype-settings").cloned()),
        Some(json!({ "rows": 25, "shade": false }))
    );
    // an equal bag does not re-push
    rig.r
        .apply_config(&json!({ "chartTypes": { "sdktype": { "rows": 25 } } }));
    assert_eq!(pushed.borrow().len(), 1);
}

#[test]
fn the_factory_reset_keeps_the_live_price_style_and_resets_the_types_candle_rows() {
    let rig = rig();
    rig.r.with(|st| {
        st.chart_types
            .borrow_mut()
            .register(ChartTypeDefinition::new("sdktype"));
    });
    rig.r.apply_feature("priceStyle", &json!("sdktype"));
    rig.r.apply_config(&json!({
        "chartTypes": { "sdktype": { "candleBodyVisible": false, "candleUpColor": "#123456" } }
    }));
    let flips: Rc<RefCell<Vec<String>>> = Rc::default();
    let f = flips.clone();
    let _u = rig
        .r
        .on_price_style_change(Box::new(move |s| f.borrow_mut().push(s.clone())));
    let doc = rig.r.factory_reset_document().unwrap();
    assert_eq!(doc["series"]["style"], "sdktype");
    rig.r.apply_config(&doc);
    assert_eq!(rig.r.read_feature("priceStyle"), Some(json!("sdktype")));
    assert!(flips.borrow().is_empty());
    let c = cfg(&rig.r);
    assert!(c["chartTypes"]["sdktype"]["candleBodyVisible"].is_null());
    assert!(c["chartTypes"]["sdktype"]["candleUpColor"].is_null());
}

#[test]
fn the_factory_reset_names_timeline_mark_groups_back_to_their_declared_visibility() {
    use crate::marks::MarkGroup;
    let rig = rig();
    rig.r.set_timeline_marks(
        &[],
        &[
            MarkGroup {
                id: "news".into(),
                label: "News".into(),
                visible: Some(false),
                parent: None,
            },
            MarkGroup {
                id: "divs".into(),
                label: "Dividends".into(),
                visible: None,
                parent: None,
            },
        ],
    );
    let doc = rig.r.factory_reset_document().unwrap();
    assert_eq!(doc["marks"]["groups"]["news"], false);
    assert_eq!(doc["marks"]["groups"]["divs"], true);
}

#[test]
fn a_theme_switch_moves_series_colors_still_on_the_previous_theme_and_keeps_chosen_ones() {
    let rig = rig();
    rig.r
        .apply_config(&json!({ "line": { "color": "#abcdef" } }));
    let mut ocean = dark_theme();
    ocean["upColor"] = json!("#00aaff");
    ocean["downColor"] = json!("#ff00aa");
    rig.r.set_theme(&ocean);
    let c = cfg(&rig.r);
    assert_eq!(c["candles"]["upColor"], "#00aaff");
    assert_eq!(c["candles"]["downColor"], "#ff00aa");
    assert_eq!(c["line"]["color"], "#abcdef");
    // an inheriting color follows
    assert_eq!(c["candles"]["wickUpColor"], "#00aaff");
}
