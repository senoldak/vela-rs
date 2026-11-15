//! Catalog-wide checks, ported from the original `test/classic-indicators.test.ts`.
//! Per-indicator cases live next to each family in `src/indicators/classics/`.

use std::collections::{BTreeMap, BTreeSet};

use vela_core::i18n::source_text;
use vela_core::indicators::classics::math::linreg;
use vela_core::indicators::classics::{
    ClassicBand, ClassicIndicatorSpec, ClassicLevel, ClassicOutput, ClassicPlot,
    classic_descriptor, classic_specs, register_classic_indicators,
};
use vela_core::indicators::{
    IndicatorStatus, NativeIndicatorContext, NativeIndicatorOutput, NativeIndicatorRegistry,
};
use vela_core::model::{Inputs, Ohlcv, SeriesPoint, SeriesSpec};

fn bar(close: f64, i: usize, volume: Option<f64>) -> Ohlcv {
    Ohlcv {
        time: i as i64 * 60_000,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume,
    }
}

fn synthetic(n: usize) -> Vec<Ohlcv> {
    (0..n)
        .map(|i| {
            bar(
                100.0 + 10.0 * (i as f64 / 15.0).sin() + (i % 7) as f64,
                i,
                Some(1000.0 + ((i % 50) * 10) as f64),
            )
        })
        .collect()
}

fn spec_of(specs: &[ClassicIndicatorSpec], indicator_type: &str) -> ClassicIndicatorSpec {
    specs
        .iter()
        .find(|s| s.indicator_type == indicator_type)
        .unwrap_or_else(|| panic!("missing spec {indicator_type}"))
        .clone()
}

struct TestContext {
    bars: Vec<Ohlcv>,
    emitted: Vec<NativeIndicatorOutput>,
    status: Option<IndicatorStatus>,
}

impl NativeIndicatorContext for TestContext {
    fn id(&self) -> &str {
        "native-test"
    }
    fn symbol(&self) -> &str {
        "TEST"
    }
    fn timeframe(&self) -> &str {
        "1m"
    }
    fn live(&self) -> bool {
        false
    }
    fn bars(&self) -> &[Ohlcv] {
        &self.bars
    }
    fn emit(&mut self, out: NativeIndicatorOutput) {
        self.emitted.push(out);
    }
    fn set_status(&mut self, status: IndicatorStatus) {
        self.status = Some(status);
    }
}

/// Drive one instance through start → stop and return what it emitted.
fn run_once(
    spec: &ClassicIndicatorSpec,
    bars: Vec<Ohlcv>,
    inputs: Option<Inputs>,
) -> NativeIndicatorOutput {
    let desc = classic_descriptor(spec);
    let mut instance = desc.create();
    let mut ctx = TestContext {
        bars,
        emitted: Vec::new(),
        status: None,
    };
    instance.start(&mut ctx, inputs.unwrap_or_else(|| desc.default_inputs()));
    instance.stop(&mut ctx);
    assert_eq!(ctx.status, Some(IndicatorStatus::Idle));
    ctx.emitted.pop().expect("the indicator emitted an output")
}

fn points_of(series: &SeriesSpec) -> &[SeriesPoint] {
    series.points().expect("a line-like series")
}

#[test]
fn registers_every_catalog_entry_with_a_unique_type() {
    let specs = classic_specs();
    let types: Vec<&str> = specs.iter().map(|s| s.indicator_type).collect();
    let unique: BTreeSet<&str> = types.iter().copied().collect();
    assert_eq!(unique.len(), types.len());
    let mut registry = NativeIndicatorRegistry::new();
    register_classic_indicators(&mut registry);
    register_classic_indicators(&mut registry); // idempotent
    assert_eq!(registry.types(), types);
    assert!(types.len() >= 70, "only {} catalog entries", types.len());
}

#[test]
fn legend_short_titles_match_the_library_declarations() {
    let library_short: BTreeMap<&str, &str> = [
        ("percent-b", "%B"),
        ("52-week-high-low", "52W H/L"),
        ("accumulation-distribution", "A/D"),
        ("aroon", "Aroon"),
        ("average-directional-index", "ADX"),
        ("average-true-range", "ATR"),
        ("awesome-oscillator", "AO"),
        ("balance-of-power", "BOP"),
        ("bandwidth", "BandWidth"),
        ("bollinger-bands", "BB"),
        ("chaikin-money-flow", "CMF"),
        ("chaikin-oscillator", "Chaikin Osc"),
        ("chaikin-volatility", "CHV"),
        ("chandelier-exit", "CE"),
        ("chande-kroll-stop", "CKS"),
        ("chande-momentum-oscillator", "CMO"),
        ("choppiness-index", "CHOP"),
        ("commodity-channel-index", "CCI"),
        ("connors-rsi", "CRSI"),
        ("coppock-curve", "Coppock"),
        ("detrended-price-oscillator", "DPO"),
        ("donchian-channels", "DC"),
        ("ema", "EMA"),
        ("ease-of-movement", "EOM"),
        ("elder-ray", "Elder Ray"),
        ("fisher-transform", "Fisher"),
        ("force-index", "FI"),
        ("gator-oscillator", "Gator Oscillator"),
        ("historical-volatility", "HV"),
        ("intraday-intensity", "Intraday Intensity"),
        ("keltner-channels", "KC"),
        ("klinger-oscillator", "KVO"),
        ("know-sure-thing", "KST"),
        ("lsma", "LSMA"),
        ("linear-regression-slope", "LRS"),
        ("macd", "MACD"),
        ("ma-envelope", "MA Env"),
        ("mass-index", "MI"),
        ("money-flow-index", "MFI"),
        ("moving-average", "MA"),
        ("negative-volume-index", "Negative Volume Index"),
        ("on-balance-volume", "OBV"),
        ("parabolic-sar", "SAR"),
        ("ppo", "PPO"),
        ("pvo", "PVO"),
        ("pivot-points", "Pivots"),
        ("positive-volume-index", "Positive Volume Index"),
        ("price-volume-trend", "PVT"),
        ("rate-of-change", "ROC"),
        ("rsi", "RSI"),
        ("relative-vigor-index", "RVGI"),
        ("relative-volatility-index", "RVI"),
        ("rma", "RMA"),
        ("schaff-trend-cycle", "Schaff Trend Cycle"),
        ("sma", "SMA"),
        ("smi-ergodic", "SMIE"),
        ("standard-deviation", "StdDev"),
        ("stochastic", "Stoch"),
        ("stochastic-rsi", "Stoch RSI"),
        ("supertrend", "SuperTrend"),
        ("trix", "TRIX"),
        ("true-strength-index", "TSI"),
        ("ttm-squeeze", "TTM Squeeze"),
        ("ulcer-index", "Ulcer Index"),
        ("ultimate-oscillator", "UO"),
        ("vidya", "VIDYA"),
        ("volume-flow-indicator", "Volume Flow Indicator"),
        ("volume-oscillator", "Vol Osc"),
        ("vortex-indicator", "VI"),
        ("vwap", "VWAP"),
        ("williams-alligator", "Alligator"),
        ("williams-fractal", "Fractals"),
        ("williams-percent-r", "%R"),
        ("zigzag", "ZigZag"),
        ("zlema", "ZLEMA"),
    ]
    .into_iter()
    .collect();
    for spec in classic_specs() {
        let expected = library_short
            .get(spec.indicator_type)
            .unwrap_or_else(|| panic!("no library short title for {}", spec.indicator_type));
        let shown = source_text(spec.short_title.as_ref().unwrap_or(&spec.title));
        assert_eq!(&shown, expected, "{}", spec.indicator_type);
    }
}

#[test]
fn every_spec_declares_defaults_for_each_of_its_inputs() {
    for spec in classic_specs() {
        let desc = classic_descriptor(&spec);
        let defaults = desc.default_inputs();
        for input in &desc.inputs_schema {
            assert_eq!(
                defaults.get(&input.key),
                Some(&input.defval),
                "{}:{}",
                spec.indicator_type,
                input.key
            );
        }
        let keys: BTreeSet<&str> = desc.inputs_schema.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            keys.len(),
            desc.inputs_schema.len(),
            "{} repeats an input key",
            spec.indicator_type
        );
    }
}

#[test]
fn every_study_allows_several_instances_per_chart() {
    for spec in classic_specs() {
        let desc = classic_descriptor(&spec);
        assert!(desc.multi_instance, "{}", spec.indicator_type);
        assert!(desc.waits_for_history, "{}", spec.indicator_type);
    }
}

#[test]
fn ships_the_regression_as_an_lsma_on_price_and_a_slope_in_its_own_pane() {
    let bars: Vec<Ohlcv> = (0..200)
        .map(|i| {
            bar(
                100.0 + 10.0 * (i as f64 / 20.0).sin() + i as f64 * 0.1,
                i,
                None,
            )
        })
        .collect();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let specs = classic_specs();
    assert!(
        !specs
            .iter()
            .any(|s| s.indicator_type == "linear-regression")
    );

    let lsma = spec_of(&specs, "lsma");
    assert_eq!(source_text(&lsma.title), "Least Squares Moving Average");
    assert!(lsma.overlay);
    let lsma_out = lsma.compute(&bars, &lsma.default_inputs());
    assert_eq!(lsma_out.plots.len(), 1);
    assert_same(&lsma_out.plots[0].values, &linreg(&closes, 25, 0.0));
    // The offset steps back along each bar's own fit.
    let mut stepped_inputs = lsma.default_inputs();
    stepped_inputs.insert("length".into(), 30.into());
    stepped_inputs.insert("offset".into(), 3.into());
    assert_same(
        &lsma.compute(&bars, &stepped_inputs).plots[0].values,
        &linreg(&closes, 30, 3.0),
    );

    let slope = spec_of(&specs, "linear-regression-slope");
    assert_eq!(source_text(&slope.title), "Linear Regression Slope");
    assert!(!slope.overlay);
    let slope_out = slope.compute(&bars, &slope.default_inputs());
    let curve = linreg(&closes, 100, 0.0);
    let prior = linreg(&closes, 100, 1.0);
    assert!((slope_out.plots[0].values[150] - (curve[150] - prior[150])).abs() < 1e-10);
    assert!(slope_out.plots[0].values[98].is_nan());
    assert_eq!(
        slope_out.levels.iter().map(|l| l.price).collect::<Vec<_>>(),
        vec![0.0]
    );
}

/// Element-wise equality that treats NaN as equal to NaN.
fn assert_same(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!(
            (x.is_nan() && y.is_nan()) || x == y,
            "index {i}: {x} != {y}"
        );
    }
}

#[test]
fn never_leaves_a_study_pane_empty() {
    let bars = synthetic(400);
    for spec in classic_specs().iter().filter(|s| !s.overlay) {
        let out = spec.compute(&bars, &spec.default_inputs());
        let in_pane = out
            .plots
            .iter()
            .filter(|p| !p.overlay && p.display.and_then(|d| d.pane) != Some(false))
            .count();
        assert!(in_pane > 0, "{}", spec.indicator_type);
    }
}

#[test]
fn every_compute_yields_bar_aligned_plot_arrays() {
    let bars = synthetic(400);
    for spec in classic_specs() {
        let out = spec.compute(&bars, &spec.default_inputs());
        for plot in &out.plots {
            assert_eq!(
                plot.values.len(),
                bars.len(),
                "{}:{}",
                spec.indicator_type,
                plot.key
            );
            if let Some(colors) = &plot.colors {
                assert_eq!(
                    colors.len(),
                    bars.len(),
                    "{}:{} colors",
                    spec.indicator_type,
                    plot.key
                );
            }
        }
        let plot_keys: BTreeSet<&str> = out.plots.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            plot_keys.len(),
            out.plots.len(),
            "{} repeats a plot key",
            spec.indicator_type
        );
        for band in &out.bands {
            assert!(
                plot_keys.contains(band.from.as_str()),
                "{}:{} from",
                spec.indicator_type,
                band.key
            );
            assert!(
                plot_keys.contains(band.to.as_str()),
                "{}:{} to",
                spec.indicator_type,
                band.key
            );
        }
    }
}

#[test]
fn every_compute_survives_degenerate_histories() {
    let short = synthetic(3);
    let no_volume: Vec<Ohlcv> = (0..120)
        .map(|i| bar(100.0 + (i % 5) as f64, i, None))
        .collect();
    for spec in classic_specs() {
        for bars in [&[][..], &short[..1], &short[..], &no_volume[..]] {
            let out = spec.compute(bars, &spec.default_inputs());
            for plot in &out.plots {
                assert_eq!(
                    plot.values.len(),
                    bars.len(),
                    "{}:{} on {} bars",
                    spec.indicator_type,
                    plot.key,
                    bars.len()
                );
            }
        }
    }
}

#[test]
fn converts_warm_up_nans_to_whitespace_points_and_wires_bands_to_plot_ids() {
    let spec = ClassicIndicatorSpec::new(
        "test-classic",
        "Test",
        false,
        Vec::new(),
        |bars, _inputs| ClassicOutput {
            plots: vec![
                ClassicPlot::new(
                    "a",
                    "A",
                    (0..bars.len())
                        .map(|i| if i == 0 { f64::NAN } else { i as f64 })
                        .collect(),
                    "#ffffff",
                ),
                ClassicPlot::new(
                    "b",
                    "B",
                    (0..bars.len()).map(|i| i as f64 + 1.0).collect(),
                    "#000000",
                ),
            ],
            bands: vec![
                ClassicBand::new("ab", "a", "b", "#12345678"),
                ClassicBand::new("dangling", "a", "missing", "#000000"),
            ],
            levels: vec![ClassicLevel::new("zero", 0.0)],
            ..Default::default()
        },
    );
    let bars = vec![bar(1.0, 0, None), bar(2.0, 1, None), bar(3.0, 2, None)];
    let out = run_once(&spec, bars, None);
    assert_eq!(out.series.len(), 2);
    let a = points_of(&out.series[0]);
    assert_eq!(a[0].value, None);
    assert_eq!(a[1].value, Some(1.0));
    assert_eq!(a[1].time, 60_000);
    // A band whose plot is missing is dropped, not emitted half-wired.
    assert_eq!(out.fills.len(), 1);
    assert_eq!(out.fills[0].from_series_id, out.series[0].id);
    assert_eq!(out.fills[0].to_series_id, out.series[1].id);
    assert_eq!(out.price_lines.len(), 1);
    assert_eq!(out.price_lines[0].price, 0.0);
    assert_eq!(out.series[0].id, "test-classic:line:a#0");
}

#[test]
fn a_pre_start_input_edit_records_without_computing() {
    let spec = spec_of(&classic_specs(), "sma");
    let desc = classic_descriptor(&spec);
    let mut instance = desc.create();
    let mut ctx = TestContext {
        bars: synthetic(60),
        emitted: Vec::new(),
        status: None,
    };
    instance.set_inputs(&mut ctx, desc.default_inputs());
    assert!(ctx.emitted.is_empty());
    instance.start(&mut ctx, desc.default_inputs());
    instance.on_bars(&mut ctx);
    assert_eq!(ctx.emitted.len(), 2);
}

#[test]
fn produces_stable_series_ids_across_recomputes() {
    let spec = spec_of(&classic_specs(), "macd");
    let bars: Vec<Ohlcv> = (0..100)
        .map(|i| bar(100.0 + (i as f64 / 5.0).sin(), i, Some(1000.0)))
        .collect();
    let first = run_once(&spec, bars.clone(), None);
    let mut longer = bars;
    longer.push(bar(101.0, 100, Some(1000.0)));
    let second = run_once(&spec, longer, None);
    let ids =
        |out: &NativeIndicatorOutput| out.series.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&first), ids(&second));
}

#[test]
fn a_plot_whose_name_reads_better_on_screen_keeps_its_identity_name() {
    let specs = classic_specs();
    let bars: Vec<Ohlcv> = (0..80)
        .map(|i| bar(100.0 + (i as f64 / 4.0).sin() * 5.0, i, Some(1000.0)))
        .collect();
    let named = |indicator_type: &str| -> Vec<(String, Option<String>)> {
        run_once(&spec_of(&specs, indicator_type), bars.clone(), None)
            .series
            .iter()
            .map(|s| (s.title.clone(), s.display_title.as_ref().map(source_text)))
            .collect()
    };
    let pair = |a: &str, b: &str| (a.to_string(), Some(b.to_string()));
    assert!(named("average-directional-index").contains(&pair("-DI", "−DI")));
    assert!(named("vortex-indicator").contains(&pair("VI-", "VI−")));
    assert_eq!(
        named("chande-kroll-stop"),
        vec![
            pair("Stop Short", "Short Stop"),
            pair("Stop Long", "Long Stop")
        ]
    );
    assert_eq!(
        named("gator-oscillator"),
        vec![
            pair(
                "Upper Histogram (Jaw - Teeth)",
                "Upper Histogram (Jaw − Teeth)"
            ),
            pair(
                "Lower Histogram (Teeth - Lips)",
                "Lower Histogram (Teeth − Lips)"
            )
        ]
    );
    assert_eq!(
        named("52-week-high-low"),
        vec![
            pair("52 Week High", "52-Week High"),
            pair("52 Week Low", "52-Week Low")
        ]
    );
    let vortex = run_once(&spec_of(&specs, "vortex-indicator"), bars.clone(), None);
    assert_eq!(
        vortex
            .series
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec!["vortex-indicator:line:vi+#0", "vortex-indicator:line:vi-#1"]
    );
}

#[test]
fn option_values_stay_english_while_their_labels_are_translatable() {
    let specs = classic_specs();
    let options_shown = |indicator_type: &str, key: &str| -> Vec<(String, Option<String>)> {
        let desc = classic_descriptor(&spec_of(&specs, indicator_type));
        let input = desc
            .inputs_schema
            .iter()
            .find(|i| i.key == key)
            .unwrap_or_else(|| panic!("{indicator_type} has no input {key}"))
            .clone();
        let labels = input.display.and_then(|d| d.options).unwrap_or_default();
        input
            .options
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                (
                    value.clone(),
                    labels
                        .get(&value)
                        .map(|m| m.is_message().then(|| source_text(m)))
                        .unwrap_or(None),
                )
            })
            .collect()
    };
    let labelled = |v: &str| (v.to_string(), Some(v.to_string()));
    assert_eq!(
        options_shown("pivot-points", "kind"),
        vec![
            labelled("Traditional"),
            labelled("Fibonacci"),
            labelled("Camarilla"),
            labelled("Woodie")
        ]
    );
    assert_eq!(
        options_shown("rate-of-change", "mode"),
        vec![labelled("ROC %"), labelled("Momentum")]
    );
}
