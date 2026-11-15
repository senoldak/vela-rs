//! The chart-type SDK: the registry of price styles beyond the built-in set, the
//! settings-row descriptors a type contributes to the settings dialog, the style
//! names, and which styles can draw a symbol.
//!
//! The original registry is a process-wide map; here it is a [`ChartTypeRegistry`]
//! value the caller owns and passes to [`crate::price_styles`] and
//! [`price_style_applies`].
//!
//! Left out:
//! - Translators: labels stay [`crate::i18n::MessageLike`] data; read the English
//!   text with `source_text`.
//! - A `supports` predicate that throws counts as supporting in the original; a
//!   panicking Rust predicate is not caught.

mod applicability;
mod builtins;
mod labels;
mod registry;
mod settings;

pub use applicability::{PRICE_STYLE_FALLBACKS, price_style_applies, shown_price_style};
pub use builtins::{heikin_ashi_transform, register_builtin_chart_types};
pub use labels::{price_style_labels, price_style_message, price_style_unavailable};
pub use registry::{
    BasePainting, ChartTypeDefinition, ChartTypeRegistry, DataEngineFactory, SeriesDataEngine,
    SeriesDataEngineHost, SupportsFn,
};
pub use settings::{
    ChartTypeSettingsInstance, ChartTypeSettingsSection, ChartTypeSettingsSubsection,
    NormalizedSettingsRow, SettingsInlineControl, SettingsLayout, SettingsPlacement,
    SettingsRowCondition, SettingsRowDescriptor, SettingsRowInlineNumber, SettingsRowSwatch,
    SettingsRowToggle, SettingsRowValueKey, SettingsRowWhen, SettingsRowWidth,
    SettingsSelectOption, SettingsValue, SettingsValueType, SettingsVisibility,
    normalize_settings_row, settings_row_value_keys, settings_row_visible,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::model::Ohlcv;
    use crate::ports::market_data::SymbolFacts;
    use crate::price_styles::{
        BarTransform, bar_transform_for, heikin_ashi_full, parse_extended_ticker,
    };

    #[derive(Debug)]
    struct Identity;

    impl BarTransform for Identity {
        fn full(&self, raw: &[Ohlcv]) -> Vec<Ohlcv> {
            raw.to_vec()
        }
        fn next(&self, raw: &Ohlcv, _prev: Option<&Ohlcv>) -> Ohlcv {
            *raw
        }
    }

    fn with_identity(id: &str) -> ChartTypeDefinition {
        let mut def = ChartTypeDefinition::new(id);
        def.bar_transform = Some(Arc::new(Identity));
        def
    }

    struct NoopEngine;

    impl SeriesDataEngine for NoopEngine {
        fn start(&mut self, _host: Arc<dyn SeriesDataEngineHost>) {}
        fn suspend(&mut self) {}
        fn resume(&mut self) {}
        fn stop(&mut self) {}
    }

    fn bag(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn eq(key: &str, v: SettingsValue) -> SettingsRowWhen {
        SettingsRowWhen::One(SettingsRowCondition::equals(key, v))
    }

    fn text(s: &str) -> SettingsValue {
        SettingsValue::Text(s.to_string())
    }

    #[test]
    fn heikinashi_is_registered_and_resolves_its_transform() {
        let reg = ChartTypeRegistry::builtin();
        let label = reg.get("heikinashi").and_then(|d| d.label.as_ref());
        assert_eq!(
            label.map(|l| l.source_text()).as_deref(),
            Some("Heikin Ashi")
        );
        let t = bar_transform_for(&reg, "heikinashi").expect("transform");
        let raw = [
            Ohlcv::new(0, 10.0, 12.0, 9.0, 11.0, None),
            Ohlcv::new(1, 11.0, 13.0, 10.0, 12.0, None),
        ];
        assert_eq!(t.full(&raw), heikin_ashi_full(&raw));
        assert!(Arc::ptr_eq(
            &t,
            &bar_transform_for(&reg, "heikinashi").expect("transform")
        ));
    }

    #[test]
    fn registration_is_id_keyed_last_wins_and_unregister_removes_it() {
        let mut reg = ChartTypeRegistry::builtin();
        reg.register(with_identity("renko-like"));
        assert!(reg.get("renko-like").is_some());
        assert!(reg.all().iter().any(|d| d.id == "renko-like"));
        reg.unregister("renko-like");
        assert!(reg.get("renko-like").is_none());
        assert!(bar_transform_for(&reg, "renko-like").is_none());

        // Replacing keeps the original position and the latest definition.
        reg.register(with_identity("a"));
        reg.register(with_identity("b"));
        let mut replaced = ChartTypeDefinition::new("a");
        replaced.icon = Some("<svg/>".into());
        reg.register(replaced);
        let ids: Vec<&str> = reg.all().iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["heikinashi", "a", "b"]);
        assert_eq!(reg.get("a").and_then(|d| d.icon.as_deref()), Some("<svg/>"));
        reg.unregister("missing");
        assert_eq!(reg.all().len(), 3);
    }

    #[test]
    fn builtins_can_be_removed_and_restored_idempotently() {
        let mut reg = ChartTypeRegistry::builtin();
        let before = bar_transform_for(&reg, "heikinashi").expect("transform");
        reg.unregister("heikinashi");
        assert!(reg.all().is_empty());
        register_builtin_chart_types(&mut reg);
        register_builtin_chart_types(&mut reg);
        assert_eq!(reg.all().len(), 1);
        assert!(Arc::ptr_eq(
            &before,
            &bar_transform_for(&reg, "heikinashi").expect("transform")
        ));
    }

    #[test]
    fn ticker_modifiers_are_dynamic() {
        let mut reg = ChartTypeRegistry::builtin();
        assert!(
            reg.ticker_modifier_ids()
                .contains(&"heikinashi".to_string())
        );

        let mut engine_only = ChartTypeDefinition::new("renko-like");
        engine_only.data_engine = Some(Arc::new(|| Box::new(NoopEngine)));
        reg.register(engine_only);
        assert!(
            !reg.ticker_modifier_ids()
                .contains(&"renko-like".to_string())
        );

        reg.register(with_identity("renko-like"));
        assert!(
            reg.ticker_modifier_ids()
                .contains(&"renko-like".to_string())
        );
        let parsed = parse_extended_ticker(&reg, "BTCUSDT;renko-like");
        assert_eq!(parsed.modifier.as_deref(), Some("renko-like"));

        reg.unregister("renko-like");
        let parsed = parse_extended_ticker(&reg, "BTCUSDT;renko-like");
        assert_eq!(parsed.symbol, "BTCUSDT;renko-like");
        assert!(parsed.modifier.is_none() && parsed.transform.is_none());
        assert_eq!(
            parse_extended_ticker(&reg, "BTCUSDT;heikinashi")
                .modifier
                .as_deref(),
            Some("heikinashi")
        );
        assert_eq!(
            parse_extended_ticker(&reg, "SYM;standard")
                .modifier
                .as_deref(),
            Some("standard")
        );
    }

    #[test]
    fn explicit_ticker_modifier_flag_overrides_the_default() {
        let mut reg = ChartTypeRegistry::new();
        let mut off = with_identity("off");
        off.ticker_modifier = Some(false);
        let mut on = ChartTypeDefinition::new("on");
        on.ticker_modifier = Some(true);
        reg.register(off);
        reg.register(on);
        assert_eq!(reg.ticker_modifier_ids(), ["on"]);
    }

    #[test]
    fn base_painting_defaults_to_candles() {
        let mut reg = ChartTypeRegistry::new();
        reg.register(ChartTypeDefinition::new("bp-default"));
        let mut none = ChartTypeDefinition::new("bp-none");
        none.base_painting = Some(BasePainting::None);
        reg.register(none);
        assert_eq!(
            reg.get("bp-default").map(|d| d.base_painting()),
            Some(BasePainting::Candles)
        );
        assert_eq!(
            reg.get("bp-none").map(|d| d.base_painting().as_str()),
            Some("none")
        );
        reg.unregister("bp-none");
        assert!(reg.get("bp-none").is_none());
    }

    #[test]
    fn structured_sections_keep_instances_subsections_and_placement() {
        let section = ChartTypeSettingsSection {
            title: "Renko-like".into(),
            placement: SettingsPlacement::AfterSymbol,
            instances: vec![
                ChartTypeSettingsInstance {
                    label: "Block 1".into(),
                    enable_key: None,
                    rows: vec![
                        SettingsRowDescriptor::Heading {
                            label: "Display".into(),
                            when: None,
                        },
                        SettingsRowDescriptor::Number {
                            key: "size".into(),
                            label: "Size".into(),
                            defval: 4.0,
                            min: None,
                            max: None,
                            step: None,
                            when: None,
                        },
                    ],
                },
                ChartTypeSettingsInstance {
                    label: "Block 2".into(),
                    enable_key: Some("b2Enabled".into()),
                    rows: vec![],
                },
            ],
            subsections: vec![ChartTypeSettingsSubsection {
                title: "Overlay".into(),
                rows: vec![],
                enable_key: None,
            }],
            ..ChartTypeSettingsSection::default()
        };
        let mut def = ChartTypeDefinition::new("renko-like");
        def.settings = Some(section);
        let mut reg = ChartTypeRegistry::new();
        reg.register(def);
        let s = reg
            .get("renko-like")
            .and_then(|d| d.settings.as_ref())
            .expect("settings");
        assert_eq!(s.placement.as_str(), "after-symbol");
        assert_eq!(s.instances[1].enable_key.as_deref(), Some("b2Enabled"));
        assert_eq!(s.subsections[0].title.source_text(), "Overlay");
        assert_eq!(s.layout, SettingsLayout::Flat);
        assert_eq!(s.visibility.as_str(), "active");
    }

    #[test]
    fn sugar_kinds_reduce_to_the_canonical_composite_shape() {
        let n = normalize_settings_row(&SettingsRowDescriptor::Number {
            key: "n".into(),
            label: "N".into(),
            defval: 3.0,
            min: Some(1.0),
            max: Some(9.0),
            step: None,
            when: None,
        })
        .expect("value row");
        assert_eq!(n.label, "N".into());
        assert!(n.toggle.is_none() && n.when.is_none());
        assert_eq!(
            n.controls,
            [SettingsInlineControl::Number {
                key: "n".into(),
                label: "N".into(),
                defval: 3.0,
                min: Some(1.0),
                max: Some(9.0),
                step: None,
                placeholder: None,
                when: None,
            }]
        );
        let c = normalize_settings_row(&SettingsRowDescriptor::Color {
            key: "c".into(),
            label: "C".into(),
            defval: "#123456".into(),
            when: None,
        })
        .expect("value row");
        assert_eq!(
            c.controls,
            [SettingsInlineControl::Color {
                key: "c".into(),
                label: "C".into(),
                defval: "#123456".into(),
                when: None,
            }]
        );
        let s = normalize_settings_row(&SettingsRowDescriptor::Select {
            key: "s".into(),
            label: "S".into(),
            options: vec![
                SettingsSelectOption::Plain("a".into()),
                SettingsSelectOption::Plain("b".into()),
            ],
            defval: "a".into(),
            when: None,
        })
        .expect("value row");
        assert_eq!(
            s.controls,
            [SettingsInlineControl::Select {
                key: "s".into(),
                label: "S".into(),
                options: vec![
                    SettingsSelectOption::Plain("a".into()),
                    SettingsSelectOption::Plain("b".into())
                ],
                defval: "a".into(),
                when: None,
            }]
        );
    }

    fn toggle_with_attachments() -> SettingsRowDescriptor {
        SettingsRowDescriptor::Toggle {
            key: "on".into(),
            label: "Line".into(),
            defval: true,
            number: Some(SettingsRowInlineNumber {
                key: "pct".into(),
                label: "Percent".into(),
                defval: 70.0,
                min: None,
                max: None,
                step: None,
            }),
            colors: vec![SettingsRowSwatch {
                key: "ink".into(),
                label: "Ink".into(),
                defval: "#fff".into(),
                when: None,
            }],
            width: Some(SettingsRowWidth {
                key: "w".into(),
                label: "Width".into(),
                defval: 2.0,
            }),
            when: None,
        }
    }

    #[test]
    fn a_toggle_keeps_the_number_colors_width_order() {
        let n = normalize_settings_row(&toggle_with_attachments()).expect("value row");
        assert_eq!(
            n.toggle,
            Some(SettingsRowToggle {
                key: "on".into(),
                defval: true
            })
        );
        let kinds: Vec<&str> = n
            .controls
            .iter()
            .map(|c| match c {
                SettingsInlineControl::Number { .. } => "number",
                SettingsInlineControl::Color { .. } => "color",
                SettingsInlineControl::Width { .. } => "width",
                SettingsInlineControl::Select { .. } => "select",
                SettingsInlineControl::Hint { .. } => "hint",
            })
            .collect();
        assert_eq!(kinds, ["number", "color", "width"]);
    }

    #[test]
    fn a_range_becomes_two_placeholder_preserving_numbers_around_a_hint() {
        let n = normalize_settings_row(&SettingsRowDescriptor::Range {
            label: "Volume".into(),
            min_key: "lo".into(),
            max_key: "hi".into(),
            defval: 0.0,
            min: None,
            max: None,
            step: None,
            placeholder: Some("Off".into()),
            when: None,
        })
        .expect("value row");
        assert_eq!(n.controls.len(), 3);
        match &n.controls[0] {
            SettingsInlineControl::Number {
                key,
                label,
                placeholder,
                ..
            } => {
                assert_eq!(key, "lo");
                assert_eq!(
                    placeholder.as_ref().map(|p| p.source_text()).as_deref(),
                    Some("Off")
                );
                assert_eq!(label.source_text(), "Volume — min");
            }
            other => panic!("expected a number control, got {other:?}"),
        }
        assert_eq!(
            n.controls[1],
            SettingsInlineControl::Hint { text: "–".into() }
        );
        match &n.controls[2] {
            SettingsInlineControl::Number { key, label, .. } => {
                assert_eq!(key, "hi");
                assert_eq!(label.source_text(), "Volume — max");
            }
            other => panic!("expected a number control, got {other:?}"),
        }
    }

    #[test]
    fn the_composite_row_kind_passes_through_unchanged() {
        let row = SettingsRowDescriptor::Row {
            label: "Mixed".into(),
            toggle: Some(SettingsRowToggle {
                key: "on".into(),
                defval: false,
            }),
            controls: vec![
                SettingsInlineControl::Select {
                    key: "mode".into(),
                    label: "Mode".into(),
                    options: vec![SettingsSelectOption::Pair {
                        value: "a".into(),
                        label: "A".into(),
                    }],
                    defval: "a".into(),
                    when: None,
                },
                SettingsInlineControl::Number {
                    key: "n".into(),
                    label: "N".into(),
                    defval: 1.0,
                    min: None,
                    max: None,
                    step: None,
                    placeholder: None,
                    when: None,
                },
            ],
            when: Some(eq("mode", text("x"))),
        };
        let n = normalize_settings_row(&row).expect("value row");
        assert_eq!(n.toggle.as_ref().map(|t| t.key.as_str()), Some("on"));
        assert_eq!(n.controls.len(), 2);
        assert!(n.when.is_some());
    }

    #[test]
    fn headings_do_not_normalize_and_store_nothing() {
        let heading = SettingsRowDescriptor::Heading {
            label: "G".into(),
            when: None,
        };
        let header = SettingsRowDescriptor::Header {
            label: "H".into(),
            when: None,
        };
        assert!(normalize_settings_row(&heading).is_none());
        assert!(!heading.is_value_row() && !header.is_value_row());
        assert!(settings_row_value_keys(&heading).is_empty());
        assert!(settings_row_value_keys(&header).is_empty());
    }

    #[test]
    fn value_keys_enumerate_every_stored_key_with_type_and_default() {
        let key = |k: &str, t: SettingsValueType, d: SettingsValue| SettingsRowValueKey {
            key: k.into(),
            value_type: t,
            defval: d,
        };
        assert_eq!(
            settings_row_value_keys(&toggle_with_attachments()),
            [
                key("on", SettingsValueType::Boolean, SettingsValue::Bool(true)),
                key(
                    "pct",
                    SettingsValueType::Number,
                    SettingsValue::Number(70.0)
                ),
                key("ink", SettingsValueType::String, text("#fff")),
                key("w", SettingsValueType::Number, SettingsValue::Number(2.0)),
            ]
        );
        let range = SettingsRowDescriptor::Range {
            label: "V".into(),
            min_key: "lo".into(),
            max_key: "hi".into(),
            defval: 0.0,
            min: None,
            max: None,
            step: None,
            placeholder: None,
            when: None,
        };
        let names: Vec<String> = settings_row_value_keys(&range)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert_eq!(names, ["lo", "hi"]);

        let mixed = SettingsRowDescriptor::Row {
            label: "Mixed".into(),
            toggle: Some(SettingsRowToggle {
                key: "on".into(),
                defval: false,
            }),
            controls: vec![
                SettingsInlineControl::Select {
                    key: "mode".into(),
                    label: "Mode".into(),
                    options: vec![SettingsSelectOption::Plain("a".into())],
                    defval: "a".into(),
                    when: None,
                },
                SettingsInlineControl::Hint { text: "–".into() },
                SettingsInlineControl::Width {
                    key: "w".into(),
                    label: "W".into(),
                    defval: 1.0,
                    when: None,
                },
            ],
            when: None,
        };
        let keys = settings_row_value_keys(&mixed);
        assert_eq!(
            keys,
            [
                key("on", SettingsValueType::Boolean, SettingsValue::Bool(false)),
                key("mode", SettingsValueType::String, text("a")),
                key("w", SettingsValueType::Number, SettingsValue::Number(1.0)),
            ]
        );
        assert_eq!(keys[1].value_type.as_str(), "string");
    }

    #[test]
    fn when_without_a_gate_is_always_visible() {
        assert!(settings_row_visible(None, &Map::new()));
    }

    #[test]
    fn when_equals_matches_the_bag_value_strictly() {
        let g = eq("mode", text("volume"));
        assert!(settings_row_visible(
            Some(&g),
            &bag(json!({ "mode": "volume" }))
        ));
        assert!(!settings_row_visible(
            Some(&g),
            &bag(json!({ "mode": "delta" }))
        ));
        let on = eq("on", SettingsValue::Bool(true));
        assert!(!settings_row_visible(
            Some(&on),
            &bag(json!({ "on": false }))
        ));
        assert!(!settings_row_visible(Some(&on), &Map::new()));
        let n = eq("n", SettingsValue::Number(1.0));
        assert!(settings_row_visible(Some(&n), &bag(json!({ "n": 1 }))));
        assert!(!settings_row_visible(Some(&n), &bag(json!({ "n": "1" }))));
        assert!(!settings_row_visible(Some(&n), &bag(json!({ "n": null }))));
    }

    #[test]
    fn when_any_of_wins_over_equals_and_matches_membership() {
        let when = SettingsRowWhen::One(SettingsRowCondition {
            key: "mode".into(),
            equals: Some(text("x")),
            any_of: Some(vec![text("delta"), text("deltaAbs")]),
        });
        assert!(settings_row_visible(
            Some(&when),
            &bag(json!({ "mode": "delta" }))
        ));
        assert!(!settings_row_visible(
            Some(&when),
            &bag(json!({ "mode": "x" }))
        ));
        let empty = SettingsRowWhen::One(SettingsRowCondition::any_of("mode", vec![]));
        assert!(!settings_row_visible(
            Some(&empty),
            &bag(json!({ "mode": "x" }))
        ));
    }

    #[test]
    fn when_with_neither_equals_nor_any_of_matches_an_unset_key() {
        let when = SettingsRowWhen::One(SettingsRowCondition {
            key: "k".into(),
            equals: None,
            any_of: None,
        });
        assert!(settings_row_visible(Some(&when), &Map::new()));
        assert!(!settings_row_visible(Some(&when), &bag(json!({ "k": 1 }))));
    }

    #[test]
    fn when_array_ands_its_conditions() {
        let when = SettingsRowWhen::All(vec![
            SettingsRowCondition::equals("mode", text("volume")),
            SettingsRowCondition::equals("dual", SettingsValue::Bool(false)),
        ]);
        assert!(settings_row_visible(
            Some(&when),
            &bag(json!({ "mode": "volume", "dual": false }))
        ));
        assert!(!settings_row_visible(
            Some(&when),
            &bag(json!({ "mode": "volume", "dual": true }))
        ));
        assert!(!settings_row_visible(
            Some(&when),
            &bag(json!({ "mode": "delta", "dual": false }))
        ));
        assert!(settings_row_visible(
            Some(&SettingsRowWhen::All(vec![])),
            &Map::new()
        ));
    }

    #[test]
    fn price_style_labels_resolve_registry_then_builtin_then_id() {
        let reg = ChartTypeRegistry::builtin();
        assert_eq!(
            price_style_message(&reg, "heikinashi").source_text(),
            "Heikin Ashi"
        );
        assert_eq!(
            price_style_message(&reg, "linemarkers").source_text(),
            "Line with markers"
        );
        assert_eq!(
            price_style_message(&reg, "columns").source_text(),
            "Columns"
        );
        assert_eq!(price_style_message(&reg, "footprint"), "footprint".into());
        assert_eq!(price_style_labels().len(), 8);
        assert_eq!(
            price_style_unavailable().source_text(),
            "Not available for this symbol's data"
        );
        let mut reg = reg;
        let mut custom = ChartTypeDefinition::new("candles");
        custom.label = Some("Mine".into());
        reg.register(custom);
        assert_eq!(price_style_message(&reg, "candles").source_text(), "Mine");
    }

    #[test]
    fn applicability_follows_the_symbol_facts() {
        let reg = ChartTypeRegistry::builtin();
        let single = SymbolFacts {
            ohlc: Some(false),
            volume: None,
        };
        let ids = ["candles", "bars", "heikinashi", "line", "area", "columns"];
        for id in ids {
            assert!(price_style_applies(&reg, id, None), "{id}");
            assert!(
                price_style_applies(&reg, id, Some(&SymbolFacts::default())),
                "{id}"
            );
        }
        let blocked: Vec<&str> = ids
            .into_iter()
            .filter(|id| !price_style_applies(&reg, id, Some(&single)))
            .collect();
        assert_eq!(blocked, ["candles", "bars", "heikinashi"]);
        // Volume-less data blocks nothing built in.
        let no_volume = SymbolFacts {
            ohlc: None,
            volume: Some(false),
        };
        assert!(price_style_applies(&reg, "heikinashi", Some(&no_volume)));
    }

    #[test]
    fn a_registered_supports_predicate_decides_for_its_type() {
        let mut reg = ChartTypeRegistry::new();
        let mut def = ChartTypeDefinition::new("candles");
        def.supports = Some(Arc::new(|_| true));
        let mut needs_volume = ChartTypeDefinition::new("flow");
        needs_volume.supports = Some(Arc::new(|f| f.volume != Some(false)));
        reg.register(def);
        reg.register(needs_volume);
        let single = SymbolFacts {
            ohlc: Some(false),
            volume: None,
        };
        assert!(price_style_applies(&reg, "candles", Some(&single)));
        let none = SymbolFacts {
            ohlc: None,
            volume: Some(false),
        };
        assert!(!price_style_applies(&reg, "flow", Some(&none)));
        assert!(price_style_applies(&reg, "flow", None));
    }

    #[test]
    fn shown_price_style_falls_back_to_the_first_applicable_known_style() {
        let reg = ChartTypeRegistry::builtin();
        let single = SymbolFacts {
            ohlc: Some(false),
            volume: None,
        };
        let known = ["candles", "line", "linemarkers", "heikinashi"];
        assert_eq!(
            shown_price_style(&reg, "candles", Some(&single), &known),
            "linemarkers"
        );
        assert_eq!(
            shown_price_style(&reg, "area", Some(&single), &known),
            "area"
        );
        assert_eq!(shown_price_style(&reg, "candles", None, &known), "candles");
        let only_line = ["candles", "line"];
        assert_eq!(
            shown_price_style(&reg, "bars", Some(&single), &only_line),
            "line"
        );
        let neither: [String; 1] = ["candles".to_string()];
        assert_eq!(
            shown_price_style(&reg, "candles", Some(&single), &neither),
            "candles"
        );
    }
}
