//! A batch entry point for the parity check against the original TypeScript
//! (`tools/parity/compare_format.mjs`): it runs a list of formatting and
//! translation requests through core on the browser's `Intl` and returns the
//! answers. Built only with the `parity` feature.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use vela_core::format::{
    ClockFormatOptions, CompactFormatOptions, DurationFormatOptions, DurationStyle,
    HourCycleSetting, NameWidth, PercentFormatOptions, PriceFormatOptions, TimeStampFormatOptions,
    TimeTickFormatOptions, TimeTickUnit, month_names, plain_integer, weekday_names,
    year_before_month, year_label, zone_offset_ms,
};
use vela_core::i18n::{
    LocaleRegistry, LocaleState, LocalizationOptions, MessageArg, MessageDescriptor, MessageLike,
    Translator, create_translator, fallback_chain, upper_case,
};
use vela_core::intl::Intl;
use vela_core::timezones::tz_offset;
use wasm_bindgen::prelude::*;

use crate::BrowserIntl;

/// A JSON number, with `null` standing for NaN (JSON cannot carry it).
fn num(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn opt_num(v: Option<&Value>) -> Option<f64> {
    v.filter(|v| !v.is_null())
        .map(|v| v.as_f64().unwrap_or(f64::NAN))
}

fn opt_bool(v: Option<&Value>) -> Option<bool> {
    v.and_then(Value::as_bool)
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

fn hour_cycle(name: Option<&str>) -> Option<HourCycleSetting> {
    Some(match name? {
        "h23" => HourCycleSetting::H23,
        "h12" => HourCycleSetting::H12,
        "locale" => HourCycleSetting::Locale,
        _ => return None,
    })
}

fn tick_unit(name: &str) -> TimeTickUnit {
    match name {
        "year" => TimeTickUnit::Year,
        "month" => TimeTickUnit::Month,
        "day" => TimeTickUnit::Day,
        "hour" => TimeTickUnit::Hour,
        "minute" => TimeTickUnit::Minute,
        _ => TimeTickUnit::Second,
    }
}

fn run(translator: &LocaleState, intl: &dyn Intl, op: &Value) -> Value {
    let get = |key: &str| op.get(key);
    let settings = translator.format_settings();
    let format = translator.format();
    match op.get("op").and_then(Value::as_str).unwrap_or("") {
        "info" => json!({
            "locale": translator.locale(),
            "direction": translator.direction().as_str(),
            "numberingSystem": translator.numbering_system(),
            "hourCycle": translator.hour_cycle().as_str(),
            "weekStart": translator.week_start(),
            "fallbackChain": fallback_chain(intl, translator.locale()),
        }),
        "t" => {
            let message = MessageLike::Message(MessageDescriptor {
                id: opt_str(get("id")).unwrap_or_default().into(),
                default_message: opt_str(get("text")).unwrap_or_default().into(),
                values: Vec::new(),
            });
            let owned: Vec<(String, MessageArg)> = get("args")
                .and_then(Value::as_object)
                .map(|args| {
                    args.iter()
                        .map(|(name, value)| {
                            let arg = match value {
                                Value::String(s) => MessageArg::from(s.clone()),
                                other => MessageArg::from(other.as_f64().unwrap_or(f64::NAN)),
                            };
                            (name.clone(), arg)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let args: Vec<(&str, MessageArg)> = owned
                .iter()
                .map(|(name, arg)| (name.as_str(), arg.clone()))
                .collect();
            json!(translator.t_with(&message, &args))
        }
        "price" => {
            let mut options = PriceFormatOptions::new(num(get("decimals")));
            options.grouping = opt_bool(get("grouping"));
            options.mintick = opt_num(get("mintick"));
            json!(format.price(num(get("value")), &options))
        }
        "percent" => json!(format.percent(
            num(get("value")),
            &PercentFormatOptions {
                surface: None,
                decimals: opt_num(get("decimals")),
                signed: opt_bool(get("signed"))
            }
        )),
        "compact" => {
            json!(format.compact(
                num(get("value")),
                &CompactFormatOptions {
                    surface: None,
                    decimals: opt_num(get("decimals")),
                    trim_zeros: opt_bool(get("trimZeros"))
                }
            ))
        }
        "timeTick" => {
            let mut options = TimeTickFormatOptions::new(
                tick_unit(&opt_str(get("unit")).unwrap_or_default()),
                opt_bool(get("major")).unwrap_or(false),
            );
            options.time_zone = opt_str(get("timeZone"));
            options.offset_ms = opt_num(get("offsetMs"));
            json!(format.time_tick(num(get("ms")), &options))
        }
        "timeStamp" => {
            json!(format.time_stamp(
                num(get("ms")),
                &TimeStampFormatOptions {
                    surface: None,
                    with_time: opt_bool(get("withTime")),
                    time_zone: opt_str(get("timeZone"))
                }
            ))
        }
        "duration" => {
            let style = match get("style").and_then(Value::as_str) {
                Some("span") => Some(DurationStyle::Span),
                Some("countdown") => Some(DurationStyle::Countdown),
                _ => None,
            };
            json!(format.duration(
                num(get("ms")),
                &DurationFormatOptions {
                    surface: None,
                    style
                }
            ))
        }
        "clock" => json!(format.clock(
            num(get("ms")),
            &ClockFormatOptions {
                surface: None,
                time_zone: opt_str(get("timeZone")),
                seconds: opt_bool(get("seconds"))
            }
        )),
        "monthNames" => {
            let width = if get("width").and_then(Value::as_str) == Some("long") {
                NameWidth::Long
            } else {
                NameWidth::Short
            };
            json!(month_names(settings, width))
        }
        "weekdayNames" => json!(weekday_names(settings)),
        "yearLabel" => json!(year_label(settings, num(get("year")))),
        "plainInteger" => json!(plain_integer(settings, num(get("n")))),
        "yearBeforeMonth" => json!(year_before_month(settings)),
        "tzOffset" => json!(tz_offset(
            intl,
            &opt_str(get("zone")).unwrap_or_default(),
            num(get("ms"))
        )),
        "zoneOffset" => json!(zone_offset_ms(
            intl,
            num(get("ms")),
            &opt_str(get("zone")).unwrap_or_default()
        )),
        "upper" => json!(upper_case(
            intl,
            &opt_str(get("text")).unwrap_or_default(),
            Some(translator.locale())
        )),
        other => json!({ "unknownOp": other }),
    }
}

/// Run a JSON array of requests and return a JSON array of answers, in order.
/// Each request names an `op`, a translator configuration (`locale`,
/// `hourCycle`, `numberingSystem`) and the op's own arguments. Every shipped
/// locale pack is registered first, as the original side does.
#[wasm_bindgen]
pub fn parity_run(requests_json: &str) -> String {
    let intl: Arc<dyn Intl> = Arc::new(BrowserIntl);
    let registry = LocaleRegistry::new(intl.clone());
    // The registrations live as long as the registry: dropping the handles does not dispose them.
    let _registrations: Vec<_> = vela_core::locales::all()
        .iter()
        .map(|pack| registry.register(pack))
        .collect();

    let requests: Vec<Value> = serde_json::from_str(requests_json).unwrap_or_default();
    let mut translators: HashMap<String, LocaleState> = HashMap::new();
    let mut answers = Vec::with_capacity(requests.len());
    for op in &requests {
        let locale = opt_str(op.get("locale"));
        let cycle = opt_str(op.get("hourCycle"));
        let numbering = opt_str(op.get("numberingSystem"));
        let key = format!("{locale:?}|{cycle:?}|{numbering:?}");
        let translator = translators.entry(key).or_insert_with(|| {
            let options = LocalizationOptions {
                locale: locale.clone(),
                hour_cycle: hour_cycle(cycle.as_deref()),
                numbering_system: numbering.clone(),
                ..Default::default()
            };
            create_translator(options, intl.clone(), registry.clone())
        });
        answers.push(run(translator, intl.as_ref(), op));
    }
    Value::Array(answers).to_string()
}

/// A smoke run of [`crate::BrowserPlatform`] against a local HTTP server:
/// timers (one fired, one cleared), a successful request, an aborted one and
/// a timed-out one. Resolves to a JSON report.
#[wasm_bindgen]
pub fn platform_smoke(base_url: String) -> js_sys::Promise {
    use std::cell::Cell;
    use std::rc::Rc;
    use vela_core::runtime::{AbortController, HttpRequest, Platform, sleep};

    wasm_bindgen_futures::future_to_promise(async move {
        let platform = crate::BrowserPlatform::new();
        let started = platform.now_ms();
        let cleared_fired = Rc::new(Cell::new(false));
        let flag = cleared_fired.clone();
        let cleared = platform.set_timeout(5.0, Box::new(move || flag.set(true)));
        platform.clear_timeout(cleared);
        sleep(&platform, 40.0).await;
        let slept = platform.now_ms() - started;

        let ok = platform
            .fetch(HttpRequest::get(format!("{base_url}/hello")))
            .await;
        let posted = platform
            .fetch(HttpRequest::post_json(
                format!("{base_url}/echo"),
                "{\"a\":1}",
            ))
            .await;
        let missing = platform
            .fetch(HttpRequest::get(format!("{base_url}/missing")))
            .await;

        let controller = AbortController::new();
        let aborting = platform
            .fetch(HttpRequest::get(format!("{base_url}/slow")).signal(controller.signal()));
        let abort = controller.clone();
        platform.set_timeout(10.0, Box::new(move || abort.abort()));
        let aborted = aborting.await;
        let timed_out = platform
            .fetch(HttpRequest::get(format!("{base_url}/slow")).timeout(30.0))
            .await;
        let refused = platform
            .fetch(HttpRequest::get("http://127.0.0.1:1/unreachable"))
            .await;

        let describe = |r: &Result<
            vela_core::runtime::HttpResponse,
            vela_core::runtime::NetError,
        >| match r {
            Ok(response) => {
                json!({ "status": response.status, "ok": response.ok(), "body": response.body, "contentType": response.header("Content-Type") })
            }
            Err(error) => json!({ "error": format!("{error:?}") }),
        };
        let report = json!({
            "sleptAtLeast40": slept >= 39.0,
            "clearedTimerFired": cleared_fired.get(),
            "ok": describe(&ok),
            "posted": describe(&posted),
            "missing": describe(&missing),
            "aborted": describe(&aborted),
            "timedOut": describe(&timed_out),
            "refusedIsFailure": matches!(refused, Err(vela_core::runtime::NetError::Failed(_))),
        });
        Ok(JsValue::from_str(&report.to_string()))
    })
}
