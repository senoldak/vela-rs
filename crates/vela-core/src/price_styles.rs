//! Price-style bar transforms: the 1:1, time-preserving views a price style applies
//! to the chart's bar stream (Heikin Ashi is the first), and the extended-ticker
//! parser that asks the data plane for one.
//!
//! The original keeps each transform as a singleton so callers compare by
//! identity. Here a transform is an `Arc<dyn BarTransform>` the chart-type
//! registry hands out; compare two with [`Arc::ptr_eq`] (the built-in Heikin Ashi
//! transform is one process-wide instance, so it stays equal across registries
//! and re-registration).

use std::fmt::Debug;
use std::sync::Arc;

use crate::chart_types::ChartTypeRegistry;
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;

/// A 1:1, TIME-PRESERVING per-bar transform a price style applies to the chart's bar
/// stream. The orchestrator applies it at its single outbound bar seam, so the
/// renderer, the scripting engines, and the native indicators all see one
/// consistent VIEW while the raw series stays the untouched source of truth.
///
/// Deliberately NOT a contract for styles that change the bar count or time axis
/// (Renko, Kagi, Point & Figure): those need a different seam.
pub trait BarTransform: Debug + Send + Sync {
    /// Derive the full view series from the raw series (front-to-back; may be recursive).
    fn full(&self, raw: &[Ohlcv]) -> Vec<Ohlcv>;
    /// Derive ONE view bar from a raw bar + the PREVIOUS view bar (live tick / append; O(1)).
    fn next(&self, raw: &Ohlcv, prev_view: Option<&Ohlcv>) -> Ohlcv;
}

/// The bar transform a price style requires (`None` means raw bars). Resolved
/// through the chart-type REGISTRY: built-ins and registered types answer
/// identically, and definitions keep their transform instances so callers can
/// compare by identity.
pub fn bar_transform_for(
    registry: &ChartTypeRegistry,
    style: &str,
) -> Option<Arc<dyn BarTransform>> {
    registry.get(style)?.bar_transform.clone()
}

/// An extended ticker split into its plain symbol and what its modifier asks for
/// (see [`parse_extended_ticker`]).
#[derive(Clone, Debug)]
pub struct ExtendedTicker {
    pub symbol: String,
    pub modifier: Option<String>,
    pub transform: Option<Arc<dyn BarTransform>>,
}

/// Parse an EXTENDED TICKER, `"SYMBOL;modifier"`, into its plain symbol + the
/// transform the modifier asks for. This is the format the engine emits: on a
/// Heikin Ashi chart `syminfo.tickerid` carries `";heikinashi"` (and
/// `ticker.heikinashi()` appends it), while `ticker.standard()` strips back to the
/// plain symbol, so a plain symbol IS the standard-data request. `";standard"` is
/// also honored as an explicit raw marker. Unknown modifiers resolve to a raw
/// (`None`) transform. The recognized set is DYNAMIC: every registered chart type
/// with a ticker modifier participates; anything else stays part of the symbol (a
/// symbol containing a literal `;` is never mangled).
pub fn parse_extended_ticker(registry: &ChartTypeRegistry, symbol: &str) -> ExtendedTicker {
    let plain = || ExtendedTicker {
        symbol: symbol.to_string(),
        modifier: None,
        transform: None,
    };
    let Some(at) = symbol.rfind(';') else {
        return plain();
    };
    if at == 0 || at == symbol.len() - 1 {
        return plain();
    }
    let modifier = symbol[at + 1..].to_lowercase();
    if modifier != "standard" && !registry.ticker_modifier_ids().contains(&modifier) {
        return plain();
    }
    let transform = bar_transform_for(registry, &modifier);
    ExtendedTicker {
        symbol: symbol[..at].to_string(),
        modifier: Some(modifier),
        transform,
    }
}

/// Heikin Ashi: a 1:1, time-preserving DISPLAY transform of the raw bar stream.
///
/// ```text
/// haClose = (O + H + L + C) / 4
/// haOpen  = (prevHaOpen + prevHaClose) / 2   (seeded (O + C) / 2 on the first bar)
/// haHigh  = max(H, haOpen, haClose)
/// haLow   = min(L, haOpen, haClose)
/// ```
///
/// Time and volume carry through unchanged. The series is RECURSIVE (each bar needs
/// the previous DERIVED bar), so a full series computes front-to-back
/// ([`heikin_ashi_full`]) and a live tick derives in O(1) from the previous view
/// bar ([`heikin_ashi_next`]). Synthetic by construction: derived values are never
/// stored back into any data cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeikinAshi;

impl BarTransform for HeikinAshi {
    fn full(&self, raw: &[Ohlcv]) -> Vec<Ohlcv> {
        heikin_ashi_full(raw)
    }

    fn next(&self, raw: &Ohlcv, prev_view: Option<&Ohlcv>) -> Ohlcv {
        heikin_ashi_next(raw, prev_view)
    }
}

/// Derive one Heikin Ashi bar from a raw bar + the PREVIOUS DERIVED bar (`None` = series start).
pub fn heikin_ashi_next(raw: &Ohlcv, prev_ha: Option<&Ohlcv>) -> Ohlcv {
    let ha_close = (raw.open + raw.high + raw.low + raw.close) / 4.0;
    let ha_open = match prev_ha {
        Some(p) => (p.open + p.close) / 2.0,
        None => (raw.open + raw.close) / 2.0,
    };
    Ohlcv {
        time: raw.time,
        open: ha_open,
        high: js_max(js_max(raw.high, ha_open), ha_close),
        low: js_min(js_min(raw.low, ha_open), ha_close),
        close: ha_close,
        volume: raw.volume,
    }
}

/// Derive the full Heikin Ashi series (front-to-back; index-aligned 1:1 with `raw`).
pub fn heikin_ashi_full(raw: &[Ohlcv]) -> Vec<Ohlcv> {
    let mut out: Vec<Ohlcv> = Vec::with_capacity(raw.len());
    for bar in raw {
        let next = heikin_ashi_next(bar, out.last());
        out.push(next);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(i: i64, open: f64, high: f64, low: f64, close: f64, volume: Option<f64>) -> Ohlcv {
        Ohlcv::new(
            1_700_000_000_000 + i * 60_000,
            open,
            high,
            low,
            close,
            volume,
        )
    }

    fn raw() -> Vec<Ohlcv> {
        vec![
            bar(0, 100.0, 104.0, 98.0, 102.0, Some(500.0)),
            bar(1, 102.0, 108.0, 101.0, 107.0, Some(700.0)),
            bar(2, 107.0, 109.0, 103.0, 104.0, Some(300.0)),
            bar(3, 104.0, 105.0, 99.0, 100.0, None),
        ]
    }

    fn close_to(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-10
    }

    #[test]
    fn applies_the_recursion() {
        let ha = heikin_ashi_full(&raw());
        assert!(close_to(ha[0].close, (100.0 + 104.0 + 98.0 + 102.0) / 4.0));
        assert!(close_to(ha[0].open, (100.0 + 102.0) / 2.0));
        assert!(close_to(ha[1].open, (ha[0].open + ha[0].close) / 2.0));
        assert!(close_to(ha[2].open, (ha[1].open + ha[1].close) / 2.0));
    }

    #[test]
    fn high_low_envelope_the_extremes_and_time_volume_carry_through() {
        let src = raw();
        let ha = heikin_ashi_full(&src);
        for (r, h) in src.iter().zip(&ha) {
            assert!(close_to(h.high, r.high.max(h.open).max(h.close)));
            assert!(close_to(h.low, r.low.min(h.open).min(h.close)));
            assert!(h.high >= h.open.max(h.close));
            assert!(h.low <= h.open.min(h.close));
            assert_eq!(h.time, r.time);
            assert_eq!(h.volume, r.volume);
        }
    }

    #[test]
    fn incremental_derivation_matches_the_full_recompute() {
        let src = raw();
        let ha = heikin_ashi_full(&src);
        for i in 0..src.len() {
            let prev = i.checked_sub(1).map(|p| &ha[p]);
            assert_eq!(heikin_ashi_next(&src[i], prev), ha[i]);
        }
        let corrected = Ohlcv {
            high: 111.0,
            close: 110.0,
            ..src[3]
        };
        let replaced = heikin_ashi_next(&corrected, Some(&ha[2]));
        let mut changed = src[..3].to_vec();
        changed.push(corrected);
        assert_eq!(replaced, heikin_ashi_full(&changed)[3]);
    }

    #[test]
    fn empty_input_and_nan_bars() {
        assert!(heikin_ashi_full(&[]).is_empty());
        let nan = bar(0, f64::NAN, 2.0, 1.0, 1.5, None);
        let ha = heikin_ashi_next(&nan, None);
        assert!(ha.open.is_nan() && ha.close.is_nan() && ha.high.is_nan() && ha.low.is_nan());
        assert_eq!(ha.time, nan.time);
    }

    #[test]
    fn transform_trait_delegates() {
        let src = raw();
        let t: &dyn BarTransform = &HeikinAshi;
        assert_eq!(t.full(&src), heikin_ashi_full(&src));
        assert_eq!(t.next(&src[0], None), heikin_ashi_next(&src[0], None));
    }

    #[test]
    fn bar_transform_for_heikinashi_only_as_a_stable_singleton() {
        let reg = ChartTypeRegistry::builtin();
        let t = bar_transform_for(&reg, "heikinashi").expect("heikinashi has a transform");
        let again = bar_transform_for(&reg, "heikinashi").expect("heikinashi has a transform");
        assert!(Arc::ptr_eq(&t, &again));
        let other = ChartTypeRegistry::builtin();
        assert!(Arc::ptr_eq(
            &t,
            &bar_transform_for(&other, "heikinashi").expect("transform")
        ));
        for s in [
            "candles",
            "bars",
            "line",
            "area",
            "baseline",
            "footprint",
            "standard",
            "",
        ] {
            assert!(bar_transform_for(&reg, s).is_none(), "{s}");
        }
    }

    #[test]
    fn parse_extended_ticker_forms() {
        let reg = ChartTypeRegistry::builtin();
        let plain = parse_extended_ticker(&reg, "BINANCE:BTCUSDT");
        assert_eq!(plain.symbol, "BINANCE:BTCUSDT");
        assert!(plain.modifier.is_none() && plain.transform.is_none());

        let ha = parse_extended_ticker(&reg, "BINANCE:BTCUSDT;heikinashi");
        assert_eq!(ha.symbol, "BINANCE:BTCUSDT");
        assert_eq!(ha.modifier.as_deref(), Some("heikinashi"));
        let expected = bar_transform_for(&reg, "heikinashi").expect("transform");
        assert!(Arc::ptr_eq(
            ha.transform.as_ref().expect("transform"),
            &expected
        ));

        let std = parse_extended_ticker(&reg, "TEST;standard");
        assert_eq!(std.symbol, "TEST");
        assert_eq!(std.modifier.as_deref(), Some("standard"));
        assert!(std.transform.is_none());

        assert!(
            parse_extended_ticker(&reg, ";heikinashi")
                .modifier
                .is_none()
        );
        assert!(parse_extended_ticker(&reg, "TEST;").modifier.is_none());
        assert!(parse_extended_ticker(&reg, "").modifier.is_none());

        let unknown = parse_extended_ticker(&reg, "TEST;xyz");
        assert_eq!(unknown.symbol, "TEST;xyz");
        assert!(unknown.modifier.is_none() && unknown.transform.is_none());
    }

    #[test]
    fn parse_extended_ticker_is_case_insensitive_and_splits_at_the_last_semicolon() {
        let reg = ChartTypeRegistry::builtin();
        let t = parse_extended_ticker(&reg, "A;B;HeikinAshi");
        assert_eq!(t.symbol, "A;B");
        assert_eq!(t.modifier.as_deref(), Some("heikinashi"));
        assert!(t.transform.is_some());
        let u = parse_extended_ticker(&reg, "ÄÖ;STANDARD");
        assert_eq!(u.symbol, "ÄÖ");
    }

    #[test]
    fn an_empty_registry_recognizes_only_standard() {
        let reg = ChartTypeRegistry::new();
        assert!(
            parse_extended_ticker(&reg, "X;heikinashi")
                .modifier
                .is_none()
        );
        assert_eq!(
            parse_extended_ticker(&reg, "X;standard")
                .modifier
                .as_deref(),
            Some("standard")
        );
    }
}
