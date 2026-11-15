//! The rich, per-type sections of the settings panel, the parts a schema field list cannot
//! express (a position's sizing maths, a profile's level lines): a port of
//! `DrawingSettingsSections.ts`. Each appends its rows to the panel's block and edits
//! through the panel's [`Build`].

use std::cell::Cell;
use std::rc::Rc;

use serde_json::json;
use vela_core::drawings::line_style_options;
use vela_core::drawings::types::measure::direction_options;
use vela_core::render::shared::drawing_geometry::contrast_color;
use web_sys::Element;

use super::kit::Build;
use super::model::{Snap, round8};
use crate::dialogs::controls::SelOpt;
use crate::dom::attr;

/// A position's number rows: account and risk sizing.
type Clamp = (f64, f64, f64, bool);

/// The position tool's section: account and risk sizing, the three levels, and which
/// labels show.
pub fn position(b: &Build) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    let number_row = |label: String, path: &'static str, clamp: Option<Clamp>| -> Element {
        let b2 = b.clone();
        let read: Rc<dyn Fn() -> f64> = Rc::new(move || {
            b2.live(|s| round8(s.read_f64(path).unwrap_or(0.0)))
                .unwrap_or(0.0)
        });
        let b3 = b.clone();
        let (min, max, step, integer) = clamp.map_or((None, None, None, false), |(a, z, s, i)| {
            (Some(a), Some(z), Some(s), i)
        });
        let c = b.number(
            &label,
            read,
            min,
            max,
            step,
            integer,
            Rc::new(move |n| b3.edit(&[(path, json!(n))])),
        );
        b.row(&label, &[c.el])
    };

    out.push(b.block(
        Some(&b.t("drawing.section.account", "Account")),
        vec![
            number_row(
                b.t("drawing.section.riskPct", "Risk %"),
                "riskPercent",
                Some((0.0, 100.0, 0.1, false)),
            ),
            number_row(
                b.t("drawing.section.accountBalance", "Account balance"),
                "accountBalance",
                Some((0.0, 1e12, 1.0, true)),
            ),
            number_row(
                b.t("drawing.section.positionSize", "Position size"),
                "quantity",
                None,
            ),
        ],
    ));

    // Levels: the direction, the entry, then stop and target in price or points.
    let mut levels: Vec<Element> = Vec::new();
    {
        let options: Vec<SelOpt> = direction_options()
            .iter()
            .map(|o| SelOpt::new(o.value.clone(), b.m(&o.label)))
            .collect();
        let b2 = b.clone();
        let b3 = b.clone();
        let c = b.select(
            &b.t("drawing.section.direction", "Direction"),
            &options,
            Rc::new(move || {
                b2.live(|s| s.read_str("direction").unwrap_or_default())
                    .unwrap_or_default()
            }),
            Rc::new(move |v| b3.edit(&[("direction", json!(v))])),
        );
        levels.push(b.row(&b.t("drawing.section.direction", "Direction"), &[c.el]));
    }
    levels.push(number_row(
        b.t("drawing.section.entryPrice", "Entry price"),
        "entryPrice",
        None,
    ));
    for (label, path, stop) in [
        (b.t("drawing.section.stop", "Stop"), "stopPrice", true),
        (
            b.t("drawing.section.target", "Target"),
            "targetPrice",
            false,
        ),
    ] {
        // The unit the field reads in lives with the row, not with the drawing.
        let points = Rc::new(Cell::new(false));
        let display = {
            let (b2, points) = (b.clone(), points.clone());
            move || -> f64 {
                b2.live(|s| {
                    s.position
                        .as_ref()
                        .map_or(0.0, |p| match (stop, points.get()) {
                            (true, false) => p.stop_price,
                            (true, true) => p.stop_points,
                            (false, false) => p.target_price,
                            (false, true) => p.target_points,
                        })
                })
                .map_or(0.0, round8)
            }
        };
        let (b2, points2, display2) = (b.clone(), points.clone(), display.clone());
        let ni = b.number(
            &label,
            Rc::new(display.clone()),
            None,
            None,
            None,
            false,
            Rc::new(move |n| {
                let price = b2.core.position_level_price(stop, points2.get(), n);
                b2.edit(&[(path, json!(price))]);
            }),
        );
        let _ = display2;
        let options = [
            SelOpt::new("price", b.t("drawing.position.unit.price", "Price")),
            SelOpt::new("points", b.t("drawing.position.unit.points", "Points")),
        ];
        let sync_number = ni.sync.clone();
        let points3 = points.clone();
        let sel = b.select(
            &label,
            &options,
            Rc::new({
                let points = points.clone();
                move || {
                    if points.get() {
                        "points".to_string()
                    } else {
                        "price".to_string()
                    }
                }
            }),
            Rc::new(move |v| {
                points3.set(v == "points");
                sync_number();
            }),
        );
        levels.push(b.row(&label, &[ni.el, sel.el]));
    }
    out.push(b.block(Some(&b.t("drawing.section.levels", "Levels")), levels));

    // Display: which labels show.
    let mut display: Vec<Element> = Vec::new();
    for (label, path) in [
        (b.t("drawing.section.showText", "Show text"), "showText"),
        (
            b.t(
                "drawing.section.showDirectionAndRatio",
                "Show direction & ratio",
            ),
            "showHeader",
        ),
        (
            b.t("drawing.section.showLossAndSize", "Show loss & size"),
            "showLossSize",
        ),
        (
            b.t("drawing.section.showTargetLabel", "Show target label"),
            "showTargetLabel",
        ),
        (
            b.t("drawing.section.showStopLabel", "Show stop label"),
            "showStopLabel",
        ),
        (
            b.t("drawing.section.showLevelPrices", "Show level prices"),
            "showPrices",
        ),
    ] {
        let (b2, b3) = (b.clone(), b.clone());
        let sw = b.switch(
            &label,
            Rc::new(move || {
                b2.live(|s| s.read_bool(path).unwrap_or(false))
                    .unwrap_or(false)
            }),
            Rc::new(move |v| b3.edit(&[(path, json!(v))])),
        );
        display.push(b.row(&label, &[sw]));
    }
    out.push(b.block(Some(&b.t("drawing.section.display", "Display")), display));

    let summary = b.cx.el("div", "vela-dsp-note vela-dsp-summary");
    let el = summary.clone();
    let b2 = b.clone();
    b.watch(move || {
        let text = b2
            .live(|s| {
                s.position
                    .as_ref()
                    .map(|p| format!("{}  \u{2014}  {}", p.header, p.loss_size))
            })
            .flatten()
            .unwrap_or_default();
        el.set_text_content(Some(&text));
    });
    if let Some(last) = out.last() {
        let _ = last.append_child(&summary);
    }
    out
}

/// The fixed-range volume profile's section: rows, value area, colors and its level lines.
pub fn profile(b: &Build) -> Vec<Element> {
    let mut rows: Vec<Element> = Vec::new();
    let read_f = |key: &'static str, b: &Build| -> Rc<dyn Fn() -> f64> {
        let b = b.clone();
        Rc::new(move || b.live(|s| s.read_f64(key).unwrap_or(0.0)).unwrap_or(0.0))
    };
    let number_row = |label: String, key: &'static str, min: f64, max: f64| -> Element {
        let b2 = b.clone();
        let path = format!("frvp.{key}");
        let c = b.number(
            &label,
            read_f(key, b),
            Some(min),
            Some(max),
            Some(1.0),
            true,
            Rc::new(move |n| b2.edit(&[(path.as_str(), json!(n))])),
        );
        b.row(&label, &[c.el])
    };
    rows.push(number_row(
        b.t("drawing.section.rows", "Rows"),
        "rows",
        1.0,
        500.0,
    ));
    rows.push(number_row(
        b.t("drawing.section.valueArea", "Value Area"),
        "valueAreaPct",
        0.0,
        100.0,
    ));
    rows.push(number_row(
        b.t("drawing.section.widthPct", "Width %"),
        "widthPct",
        0.0,
        100.0,
    ));
    {
        let options = [
            SelOpt::new("right", b.t("drawing.fixedrangevp.anchor.right", "Right")),
            SelOpt::new("left", b.t("drawing.fixedrangevp.anchor.left", "Left")),
        ];
        let (b2, b3) = (b.clone(), b.clone());
        let c = b.select(
            &b.t("drawing.section.anchor", "Anchor"),
            &options,
            Rc::new(move || {
                b2.live(|s| s.read_str("frvp.anchor").unwrap_or_default())
                    .unwrap_or_default()
            }),
            Rc::new(move |v| b3.edit(&[("frvp.anchor", json!(v))])),
        );
        rows.push(b.row(&b.t("drawing.section.anchor", "Anchor"), &[c.el]));
    }
    let color_row = |label: String, key: &'static str| -> Element {
        let b2 = b.clone();
        let el = b.swatch(
            &label,
            Rc::new(move || {
                b2.live(|s| s.read_str(&format!("frvp.{key}")).unwrap_or_default())
                    .unwrap_or_default()
            }),
            format!("frvp.{key}"),
        );
        let row = b.row(&label, &[el]);
        attr(&row, "data-fit", "");
        row
    };
    rows.push(color_row(
        b.t("drawing.section.upVolume", "Up Volume"),
        "upColor",
    ));
    rows.push(color_row(
        b.t("drawing.section.downVolume", "Down Volume"),
        "downColor",
    ));
    rows.push(color_row(
        b.t("drawing.section.valueAreaUp", "Value Area Up"),
        "vaUpColor",
    ));
    rows.push(color_row(
        b.t("drawing.section.valueAreaDown", "Value Area Down"),
        "vaDownColor",
    ));

    let styles: Vec<SelOpt> = line_style_options()
        .iter()
        .map(|o| SelOpt::new(o.value.clone(), b.m(&o.label)))
        .collect();
    let background = b.core.theme_color("background").unwrap_or_default();
    let level_row =
        |label: String, show: &'static str, color: &'static str, style: &'static str| -> Element {
            let row = b.cx.el("div", "vela-dsp-lvl");
            let (b2, b3) = (b.clone(), b.clone());
            let sw = b.bare_switch(
                &label,
                Rc::new(move || {
                    b2.live(|s| s.read_bool(&format!("frvp.{show}")).unwrap_or(false))
                        .unwrap_or(false)
                }),
                Rc::new(move |v| b3.edit(&[(format!("frvp.{show}").as_str(), json!(v))])),
            );
            let lab = b.cx.text("span", "vela-dsp-lab vela-dsp-lvl-l", &label);
            // An unset color means the theme's contrast ink.
            let ink = contrast_color(Some(&background)).to_string();
            let b2 = b.clone();
            let col = b.swatch(
                &label,
                Rc::new(move || {
                    b2.live(|s| s.read_str(&format!("frvp.{color}")))
                        .flatten()
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| ink.clone())
                }),
                format!("frvp.{color}"),
            );
            let (b2, b3) = (b.clone(), b.clone());
            let st = b.select(
                &label,
                &styles,
                Rc::new(move || {
                    b2.live(|s| s.read_str(&format!("frvp.{style}")).unwrap_or_default())
                        .unwrap_or_default()
                }),
                Rc::new(move |v| b3.edit(&[(format!("frvp.{style}").as_str(), json!(v))])),
            );
            let _ = row.append_child(&sw.el);
            let _ = row.append_child(&lab);
            let _ = row.append_child(&col);
            let _ = row.append_child(&st.el);
            row
        };
    rows.push(level_row(
        b.t("drawing.section.vah", "VAH"),
        "showVah",
        "vahColor",
        "vahStyle",
    ));
    rows.push(level_row(
        b.t("drawing.section.val", "VAL"),
        "showVal",
        "valColor",
        "valStyle",
    ));
    rows.push(level_row(
        b.t("drawing.section.poc", "POC"),
        "showPoc",
        "pocColor",
        "pocStyle",
    ));
    rows.push(level_row(
        b.t("drawing.section.developingPOC", "Developing POC"),
        "showDevelopingPoc",
        "developingPocColor",
        "developingPocStyle",
    ));
    rows.push(level_row(
        b.t("drawing.section.developingVA", "Developing VA"),
        "showDevelopingVa",
        "developingVaColor",
        "developingVaStyle",
    ));
    vec![b.block(Some(&b.t("drawing.settings.profile", "Profile")), rows)]
}

/// Whether the drawing is a position (its section replaces the generic rows).
pub fn is_position(s: &Snap) -> bool {
    s.type_key() == "position"
}

/// Whether the drawing is a fixed-range volume profile.
pub fn is_profile(s: &Snap) -> bool {
    s.type_key() == "fixedrangevp"
}
