//! Dumps the classic catalog as JSON: every spec's metadata and inputs, plus
//! its compute output over deterministic bars under a spread of input
//! scenarios. The output is the Rust half of a parity check against the
//! original TypeScript catalog (see the README).
//!
//! Usage: `cargo run --release --example dump_catalog > catalog.json`

use std::fmt::Write as _;
use std::io::Write as _;

use vela_core::i18n::{MessageLike, source_text};
use vela_core::indicators::classics::{
    ClassicIndicatorSpec, ClassicInput, ClassicOutput, classic_specs,
};
use vela_core::model::{
    DrawingXLoc, InputType, InputValue, Inputs, LineStyle, MarkerPosition, MarkerSize, Ohlcv,
    SeriesDisplay,
};

fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn number(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".to_string()
    }
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map(f).unwrap_or_else(|| "null".to_string())
}

fn array<T>(items: impl IntoIterator<Item = T>, f: impl Fn(T) -> String) -> String {
    format!(
        "[{}]",
        items.into_iter().map(f).collect::<Vec<_>>().join(",")
    )
}

fn object(fields: &[(&str, String)]) -> String {
    format!(
        "{{{}}}",
        fields
            .iter()
            .map(|(k, v)| format!("{}:{}", string(k), v))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn message(m: &MessageLike) -> String {
    string(&source_text(m))
}

fn line_style(s: LineStyle) -> String {
    string(match s {
        LineStyle::Solid => "solid",
        LineStyle::Dashed => "dashed",
        LineStyle::Dotted => "dotted",
    })
}

fn input_value(v: &InputValue) -> String {
    match v {
        InputValue::Number(n) => number(*n),
        InputValue::Text(s) => string(s),
        InputValue::Bool(b) => b.to_string(),
    }
}

fn input_type(t: InputType) -> &'static str {
    match t {
        InputType::Int => "int",
        InputType::Float => "float",
        InputType::Bool => "bool",
        InputType::String => "string",
        InputType::Source => "source",
        InputType::Color => "color",
        InputType::Price => "price",
        InputType::Time => "time",
        InputType::Session => "session",
        InputType::Timeframe => "timeframe",
        InputType::Symbol => "symbol",
        InputType::TextArea => "text_area",
    }
}

fn input(i: &ClassicInput) -> String {
    object(&[
        ("key", string(&i.key)),
        ("title", message(&i.title)),
        ("type", string(input_type(i.input_type))),
        ("defval", input_value(&i.defval)),
        ("min", opt(i.min, number)),
        ("max", opt(i.max, number)),
        ("step", opt(i.step, number)),
        (
            "options",
            opt(i.options.as_ref(), |o| array(o, |s| string(s))),
        ),
        ("group", opt(i.group.as_ref(), |s| string(s))),
        ("inline", opt(i.inline.as_ref(), |s| string(s))),
        ("tooltip", opt(i.tooltip.as_ref(), message)),
        (
            "when",
            array(&i.when, |c| {
                object(&[
                    ("key", string(&c.key)),
                    ("equals", opt(c.equals.as_ref(), input_value)),
                    (
                        "anyOf",
                        opt(c.any_of.as_ref(), |set| array(set, input_value)),
                    ),
                ])
            }),
        ),
    ])
}

fn display(d: SeriesDisplay) -> String {
    let flag = |f: Option<bool>| opt(f, |b| b.to_string());
    object(&[
        ("pane", flag(d.pane)),
        ("priceScale", flag(d.price_scale)),
        ("legend", flag(d.legend)),
        ("dataWindow", flag(d.data_window)),
    ])
}

fn colors(c: &[Option<String>]) -> String {
    array(c, |c| opt(c.as_ref(), |s| string(s)))
}

fn output(out: &ClassicOutput) -> String {
    let plots = array(&out.plots, |p| {
        object(&[
            ("key", string(&p.key)),
            ("title", message(&p.title)),
            ("displayTitle", opt(p.display_title.as_ref(), message)),
            ("kind", string(p.kind.as_str())),
            ("color", string(&p.color)),
            ("width", opt(p.width, number)),
            ("lineStyle", opt(p.line_style, line_style)),
            ("base", opt(p.base, number)),
            ("overlay", p.overlay.to_string()),
            ("display", opt(p.display, display)),
            ("values", array(&p.values, |v| number(*v))),
            ("colors", opt(p.colors.as_ref(), |c| colors(c))),
        ])
    });
    let bands = array(&out.bands, |b| {
        object(&[
            ("key", string(&b.key)),
            ("from", string(&b.from)),
            ("to", string(&b.to)),
            ("color", string(&b.color)),
            ("colors", opt(b.colors.as_ref(), |c| colors(c))),
            (
                "gradient",
                opt(b.gradient.as_ref(), |g| {
                    array(g, |stop| {
                        opt(stop.as_ref(), |s| {
                            object(&[
                                ("topValue", number(s.top_value)),
                                ("bottomValue", number(s.bottom_value)),
                                ("topColor", string(&s.top_color)),
                                ("bottomColor", string(&s.bottom_color)),
                            ])
                        })
                    })
                }),
            ),
        ])
    });
    let levels = array(&out.levels, |l| {
        object(&[
            ("key", string(&l.key)),
            ("price", number(l.price)),
            ("color", opt(l.color.as_ref(), |s| string(s))),
            ("lineStyle", opt(l.line_style, line_style)),
            ("title", opt(l.title.as_ref(), message)),
        ])
    });
    let markers = array(&out.markers, |m| {
        object(&[
            ("time", m.time.to_string()),
            (
                "position",
                string(match m.position {
                    MarkerPosition::AboveBar => "aboveBar",
                    MarkerPosition::BelowBar => "belowBar",
                    MarkerPosition::InBar => "inBar",
                }),
            ),
            ("shape", string(&m.shape)),
            ("color", string(&m.color)),
            ("text", opt(m.text.as_ref(), |s| string(s))),
            (
                "size",
                opt(m.size, |s| {
                    string(match s {
                        MarkerSize::Tiny => "tiny",
                        MarkerSize::Small => "small",
                        MarkerSize::Normal => "normal",
                        MarkerSize::Large => "large",
                        MarkerSize::Huge => "huge",
                    })
                }),
            ),
        ])
    });
    let polylines = array(&out.polylines, |p| {
        object(&[
            ("key", string(&p.key)),
            (
                "points",
                array(&p.points, |pt| {
                    object(&[
                        (
                            "xloc",
                            string(match pt.xloc {
                                DrawingXLoc::BarIndex => "bar_index",
                                DrawingXLoc::BarTime => "bar_time",
                            }),
                        ),
                        ("x", number(pt.x)),
                        ("price", number(pt.price)),
                    ])
                }),
            ),
            ("curved", p.curved.to_string()),
            ("closed", p.closed.to_string()),
            ("lineColor", opt(p.line_color.as_ref(), |s| string(s))),
            ("fillColor", opt(p.fill_color.as_ref(), |s| string(s))),
            ("lineWidth", number(p.line_width)),
            ("lineStyle", line_style(p.line_style)),
            ("arrowLeft", p.arrow_left.to_string()),
            ("arrowRight", p.arrow_right.to_string()),
            ("overlay", p.overlay.to_string()),
        ])
    });
    object(&[
        ("plots", plots),
        ("bands", bands),
        ("levels", levels),
        ("markers", markers),
        ("polylines", polylines),
    ])
}

/// Lehmer generator: every product stays under 2^53, so a JavaScript port
/// would reproduce it exactly. Prices are built from integers so both sides
/// read identical doubles from the JSON.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = (self.0 * 48271) % 2_147_483_647;
        self.0
    }
}

fn walk(count: usize, start: i64, step_ms: i64, with_volume: bool, seed: u64) -> Vec<Ohlcv> {
    let mut rng = Rng(seed);
    let mut close_cents: i64 = 10_000;
    let mut bars = Vec::with_capacity(count);
    for i in 0..count {
        let open_cents = close_cents;
        close_cents = (close_cents + (rng.next() % 201) as i64 - 100).max(1_000);
        let high_cents = open_cents.max(close_cents) + (rng.next() % 80) as i64;
        let low_cents = open_cents.min(close_cents) - (rng.next() % 80) as i64;
        let volume = 1_000 + rng.next() % 5_000;
        bars.push(Ohlcv {
            time: start + i as i64 * step_ms,
            open: open_cents as f64 / 100.0,
            high: high_cents as f64 / 100.0,
            low: low_cents as f64 / 100.0,
            close: close_cents as f64 / 100.0,
            volume: with_volume.then_some(volume as f64),
        });
    }
    bars
}

fn bars_json(bars: &[Ohlcv]) -> String {
    array(bars, |b| {
        object(&[
            ("time", b.time.to_string()),
            ("open", number(b.open)),
            ("high", number(b.high)),
            ("low", number(b.low)),
            ("close", number(b.close)),
            ("volume", opt(b.volume, number)),
        ])
    })
}

/// Input scenarios for one spec: `(name, bar set, inputs)`.
fn scenarios(spec: &ClassicIndicatorSpec) -> Vec<(String, &'static str, Inputs)> {
    let defaults = spec.default_inputs();
    let mut out = vec![
        ("defaults".to_string(), "hourly", defaults.clone()),
        ("defaults".to_string(), "minuteNoVolume", defaults.clone()),
        ("defaults".to_string(), "daily", defaults.clone()),
        ("defaults".to_string(), "tiny", defaults.clone()),
        ("no inputs".to_string(), "hourly", Inputs::new()),
    ];
    let mut scaled = defaults.clone();
    for i in &spec.inputs {
        match (&i.defval, i.input_type) {
            (InputValue::Text(def), _) => {
                for option in i.options.iter().flatten().filter(|o| *o != def) {
                    let mut inputs = defaults.clone();
                    inputs.insert(i.key.clone(), option.as_str().into());
                    out.push((format!("{}={}", i.key, option), "hourly", inputs));
                }
            }
            (InputValue::Bool(def), _) => {
                let mut inputs = defaults.clone();
                inputs.insert(i.key.clone(), (!def).into());
                out.push((format!("{}={}", i.key, !def), "hourly", inputs));
            }
            (InputValue::Number(def), InputType::Int) => {
                let v = (def * 1.5).floor() + 1.0;
                scaled.insert(i.key.clone(), i.max.map_or(v, |max| v.min(max)).into());
            }
            (InputValue::Number(def), _) => {
                let v = def * 1.5;
                scaled.insert(i.key.clone(), i.max.map_or(v, |max| v.min(max)).into());
            }
        }
    }
    out.push(("numbers scaled".to_string(), "hourly", scaled.clone()));
    out.push(("numbers scaled".to_string(), "daily", scaled));
    out
}

fn main() {
    // 2024-03-01T00:00Z hourly: crosses weeks, a month end and a quarter end.
    let hourly = walk(1200, 1_709_251_200_000, 3_600_000, true, 12_345);
    let minute_no_volume = walk(400, 0, 60_000, false, 777);
    // 2022-01-01T00:00Z daily: more than a year, so yearly windows fill.
    let daily = walk(500, 1_640_995_200_000, 86_400_000, true, 4_242);
    let tiny = walk(3, 1_709_251_200_000, 3_600_000, true, 99);
    let sets: [(&str, &[Ohlcv]); 4] = [
        ("hourly", &hourly),
        ("minuteNoVolume", &minute_no_volume),
        ("daily", &daily),
        ("tiny", &tiny),
    ];

    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    let bar_sets = object(
        &sets
            .iter()
            .map(|(name, bars)| (*name, bars_json(bars)))
            .collect::<Vec<_>>(),
    );
    write!(w, "{{\"bars\":{bar_sets},\"specs\":[").expect("write to stdout");
    for (n, spec) in classic_specs().iter().enumerate() {
        if n > 0 {
            write!(w, ",").expect("write to stdout");
        }
        let runs = array(scenarios(spec), |(name, set, inputs)| {
            let bars = sets
                .iter()
                .find(|(s, _)| *s == set)
                .map(|(_, b)| *b)
                .unwrap_or(&[]);
            object(&[
                ("name", string(&name)),
                ("bars", string(set)),
                (
                    "inputs",
                    object(
                        &inputs
                            .iter()
                            .map(|(k, v)| (k.as_str(), input_value(v)))
                            .collect::<Vec<_>>(),
                    ),
                ),
                ("output", output(&spec.compute(bars, &inputs))),
            ])
        });
        let json = object(&[
            ("type", string(spec.indicator_type)),
            ("title", message(&spec.title)),
            ("shortTitle", opt(spec.short_title.as_ref(), message)),
            ("overlay", spec.overlay.to_string()),
            ("inputs", array(&spec.inputs, input)),
            ("runs", runs),
        ]);
        write!(w, "{json}").expect("write to stdout");
    }
    writeln!(w, "]}}").expect("write to stdout");
}
