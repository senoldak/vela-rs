//! The chart's translator and `set_locale` (`i18n-chart.test.ts`, the cases
//! that need no DOM), through the public API only.

mod vela_common;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use vela_common::*;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::i18n::{
    DirectionOption, LocaleChangedEvent, LocalePack, LocalizationOptions, LocalizationPatch, Patch,
    TextDirection, msg,
};
use vela_core::ports::chart_renderer::RendererRootAttributes;

const TITLE: &str = "widget.shortcuts.title";

fn de_pack() -> LocalePack {
    LocalePack {
        locale: "de".to_string(),
        direction: None,
        messages: Arc::new(HashMap::from([
            (TITLE.to_string(), "Tastenkürzel".to_string()),
            ("widget.shortcuts.mouse".to_string(), "Maus".to_string()),
        ])),
    }
}

fn locale_events(rig: &Rig) -> Rc<RefCell<Vec<LocaleChangedEvent>>> {
    let seen: Rc<RefCell<Vec<LocaleChangedEvent>>> = Rc::default();
    let sink = Rc::clone(&seen);
    rig.chart.on(ChartEventName::LocaleChanged, move |e| {
        if let ChartEvent::LocaleChanged(event) = e {
            sink.borrow_mut().push(event.clone());
        }
    });
    seen
}

#[test]
fn a_chart_with_no_locale_reads_english_and_leaves_lang_and_dir_to_the_page() {
    let rig = Rig::builder().build();
    let i18n = rig.chart.i18n();
    assert_eq!(i18n.locale(), "en");
    assert_eq!(i18n.direction(), TextDirection::Ltr);
    let calls = rig.renderer.localizations.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].root,
        RendererRootAttributes {
            lang: None,
            dir: None
        }
    );
    assert_eq!(calls[0].price, "1,234.50");
}

#[test]
fn set_locale_switches_messages_and_fires_locale_changed() {
    let rig = Rig::builder().build();
    rig.env.locales.register(&de_pack());
    let events = locale_events(&rig);
    let title = msg(TITLE, "Keyboard shortcuts");
    assert_eq!(rig.chart.i18n().t(&title), "Keyboard shortcuts");

    rig.chart.set_locale("de");
    let i18n = rig.chart.i18n();
    assert_eq!(i18n.locale(), "de");
    assert_eq!(i18n.t(&title), "Tastenkürzel");
    {
        let calls = rig.renderer.localizations.borrow();
        let last = calls.last().expect("a localization call");
        assert_eq!(last.locale, "de");
        assert_eq!(
            last.root,
            RendererRootAttributes {
                lang: Some("de".to_string()),
                dir: None
            }
        );
        assert_eq!(calls.len(), 2, "once at construction, once per change");
    }
    assert_eq!(
        *events.borrow(),
        [LocaleChangedEvent {
            locale: "de".to_string(),
            direction: TextDirection::Ltr
        }]
    );
}

#[test]
fn update_localization_keeps_what_it_leaves_out_and_clear_restores_the_default() {
    let rig = Rig::builder().build();
    rig.env.locales.register(&de_pack());
    let events = locale_events(&rig);
    let title = msg(TITLE, "Keyboard shortcuts");

    rig.chart.set_locale("de");
    rig.chart.update_localization(LocalizationPatch {
        locale: Patch::Set("ar".to_string()),
        direction: Patch::Set(DirectionOption::Auto),
        ..LocalizationPatch::default()
    });
    assert_eq!(
        events.borrow().last().map(|e| e.direction),
        Some(TextDirection::Rtl)
    );
    {
        let calls = rig.renderer.localizations.borrow();
        assert_eq!(
            calls.last().map(|c| c.root.clone()),
            Some(RendererRootAttributes {
                lang: Some("ar".to_string()),
                dir: Some(TextDirection::Rtl)
            })
        );
    }
    // No Arabic pack: the English source reads through.
    assert_eq!(rig.chart.i18n().t(&title), "Keyboard shortcuts");

    // Fields left out are kept.
    rig.chart.update_localization(LocalizationPatch {
        numbering_system: Patch::Set("arab".to_string()),
        ..LocalizationPatch::default()
    });
    let i18n = rig.chart.i18n();
    assert_eq!(i18n.locale(), "ar");
    assert_eq!(i18n.direction(), TextDirection::Rtl);

    // An explicit clear restores the default.
    rig.chart.update_localization(LocalizationPatch {
        locale: Patch::Clear,
        direction: Patch::Clear,
        numbering_system: Patch::Clear,
        ..LocalizationPatch::default()
    });
    let i18n = rig.chart.i18n();
    assert_eq!(i18n.locale(), "en");
    assert_eq!(
        rig.renderer
            .localizations
            .borrow()
            .last()
            .map(|c| c.root.clone()),
        Some(RendererRootAttributes {
            lang: None,
            dir: None
        })
    );
}

#[test]
fn a_chart_takes_its_locale_direction_and_overrides_from_the_options() {
    let rig = Rig::builder()
        .options(|o| {
            o.localization = LocalizationOptions {
                locale: Some("he".to_string()),
                direction: Some(DirectionOption::Auto),
                messages: Some(HashMap::from([(TITLE.to_string(), "קיצורים".to_string())])),
                ..LocalizationOptions::default()
            };
        })
        .build();
    let i18n = rig.chart.i18n();
    assert_eq!(i18n.direction(), TextDirection::Rtl);
    assert_eq!(i18n.t(&msg(TITLE, "Keyboard shortcuts")), "קיצורים");
    assert_eq!(
        rig.renderer.localizations.borrow()[0].root,
        RendererRootAttributes {
            lang: Some("he".to_string()),
            dir: Some(TextDirection::Rtl)
        }
    );
}

#[test]
fn the_engine_follows_the_new_translator_for_native_indicator_titles() {
    let rig = Rig::builder().build();
    let handle = rig.chart.add_native_indicator("vpvr", Default::default());
    let before = handle.title();
    rig.env.locales.register(&LocalePack {
        locale: "de".to_string(),
        direction: None,
        messages: Arc::new(HashMap::new()),
    });
    rig.chart.set_locale("de");
    // No translation registered for the title: the English source stays.
    assert_eq!(handle.title(), before);
}

#[test]
fn charts_on_one_environment_share_the_locale_packs() {
    let a = Rig::builder().build();
    a.env.locales.register(&de_pack());
    let b = Rig::builder().build_on(a.platform.clone(), a.env.clone());
    b.chart.set_locale("de");
    assert_eq!(
        b.chart.i18n().t(&msg(TITLE, "Keyboard shortcuts")),
        "Tastenkürzel"
    );
    // The other chart did not change locale.
    assert_eq!(a.chart.i18n().locale(), "en");
}
