//! Pure data helpers: timeframe arithmetic, the in-memory bar cache and the
//! symbol vocabulary shared by pickers and registries.
//!
//! Left out: `CachingDataFeed`, `MultiProviderFeed`, `ProviderRegistry` and the
//! `providers/` folder (asynchronous, network-bound), the module-wide
//! `sharedBarStore` singleton (the caller owns the [`BarStore`] it shares), and
//! `foldGroups` (it lives in the widget's symbol picker, not in the data layer).

pub mod caching_feed;
pub mod multi_provider_feed;
pub mod provider_registry;
pub mod providers;

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{Millis, Ohlcv};
use crate::ports::data_provider::SymbolDescriptor;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const WEEK_MS: i64 = 604_800_000;
/// `M` is treated as a 30-day month (bucketing only).
const MONTH_MS: i64 = 2_592_000_000;

/// JavaScript's `String.prototype.trim` whitespace set (it differs from Rust's
/// `char::is_whitespace`: U+FEFF counts, U+0085 does not).
fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c.is_whitespace() && c != '\u{85}')
}

/// Unit letter to ms. Lower-cased except `M`: a month, which the `m` (minute)
/// alias must not shadow.
fn unit_ms(letter: char) -> Option<i64> {
    if letter == 'M' {
        return Some(MONTH_MS);
    }
    match letter.to_ascii_lowercase() {
        's' => Some(1_000),
        'm' => Some(MINUTE_MS),
        'h' => Some(HOUR_MS),
        'd' => Some(DAY_MS),
        'w' => Some(WEEK_MS),
        _ => None,
    }
}

/// Bar duration in ms for a Vela timeframe: a bare number is **minutes** (Pine
/// resolution: `60` = 1h, `240` = 4h), `D`/`W`/`M` are the named periods, also
/// with a count (`1D`, `2W`, `3M`, `12M`), `S` counts seconds (`30S`), and the
/// `15m`/`4h`/`1d`/`1w` aliases are also accepted. Letters are case-insensitive
/// except `M` (month) vs `m` (minute). Falls back to 1h for anything unparsed.
///
/// A count too large for `i64` saturates instead of becoming a float.
pub fn timeframe_to_ms(timeframe: &str) -> Millis {
    let tf = timeframe.trim_matches(is_js_space);
    match tf {
        "D" => return DAY_MS,
        "W" => return WEEK_MS,
        "M" => return MONTH_MS,
        _ => {}
    }
    let digits_end = tf.bytes().take_while(u8::is_ascii_digit).count();
    if digits_end > 0 {
        let rest = tf[digits_end..].trim_start_matches(is_js_space);
        let mut chars = rest.chars();
        // A bare number means minutes.
        let letter = match chars.next() {
            None => Some('m'),
            Some(c) if c.is_ascii_alphabetic() && chars.next().is_none() => Some(c),
            Some(_) => None,
        };
        if let Some(mult) = letter.and_then(unit_ms) {
            let count = tf[..digits_end].parse::<i64>().unwrap_or(i64::MAX);
            return count.saturating_mul(mult);
        }
    }
    HOUR_MS
}

/// Cache key for one series. Symbols and timeframes never contain `|`. The
/// session is a KEY dimension when non-default: regular and extended bars of
/// one symbol genuinely differ (extended has more bars, and session dailies
/// differ in OHLC), so they must never share a series. Absent or `regular`
/// stays keyless, so existing keys do not move.
pub fn series_key(provider: &str, symbol: &str, timeframe: &str, session: Option<&str>) -> String {
    match session {
        Some(s) if !s.is_empty() && s != "regular" => {
            format!("{provider}|{symbol}|{timeframe}|{s}")
        }
        _ => format!("{provider}|{symbol}|{timeframe}"),
    }
}

fn symbol_of(key: &str) -> &str {
    key.split('|').nth(1).unwrap_or("")
}

/// The owner a [`BarStore::retain`] declaration belongs to when the caller has
/// no shell instance to name.
pub const DEFAULT_RETAIN_OWNER: &str = "default";

/// In-memory cache of CLOSED bars, keyed by `(provider, symbol, timeframe)`.
/// Each series holds a single time-sorted, de-duplicated vector. Scoped to the
/// current symbol: [`BarStore::retain_symbol`] evicts other symbols' series
/// (the agreed purge policy; smarter eviction comes later), except symbols
/// protected via [`BarStore::retain`], the multi-chart seam: a workspace
/// declares every cell's symbol so one cell's load never evicts the others'
/// history.
#[derive(Debug, Default)]
pub struct BarStore {
    series: HashMap<String, Vec<Ohlcv>>,
    /// Earliest bar-open time fetched for a series: what the cache covers.
    covered_from: HashMap<String, Millis>,
    current_symbol: Option<String>,
    /// Symbols protected from the current-symbol purge (multi-chart cells): the
    /// UNION of every owner's declaration. Empty means legacy single-chart
    /// behavior.
    retained: HashSet<String>,
    /// Per-owner declarations behind `retained`: several shells on one page
    /// must not clobber (or, on destroy, evict) each other's protected symbols.
    retained_by_owner: HashMap<String, HashSet<String>>,
}

impl BarStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&[Ohlcv]> {
        self.series.get(key).map(Vec::as_slice)
    }

    /// Record that the series is covered back to `from`. Tracks the EARLIEST
    /// such time: what has been fetched, not what bar boundary happens to
    /// align. The coverage check uses this instead of the first cached bar's
    /// time (which is always at or after the requested `from`, so it can never
    /// prove coverage).
    pub fn mark_covered(&mut self, key: &str, from: Millis) {
        self.covered_from
            .entry(key.to_owned())
            .and_modify(|earliest| *earliest = (*earliest).min(from))
            .or_insert(from);
    }

    /// The earliest time the series is covered back to, or `None` if never
    /// fetched.
    pub fn covered_from_of(&self, key: &str) -> Option<Millis> {
        self.covered_from.get(key).copied()
    }

    /// Merge `bars` into the series (dedup by time, incoming wins); keeps it
    /// sorted. Returns the merged set.
    pub fn merge(&mut self, key: &str, bars: &[Ohlcv]) -> &[Ohlcv] {
        if !bars.is_empty() {
            let mut by_time: BTreeMap<Millis, Ohlcv> = BTreeMap::new();
            for b in self.series.get(key).into_iter().flatten().chain(bars) {
                by_time.insert(b.time, *b);
            }
            self.series
                .insert(key.to_owned(), by_time.into_values().collect());
        }
        self.series.get(key).map_or(&[], Vec::as_slice)
    }

    /// Scope the cache to the current chart symbol. Idempotent per symbol: only
    /// purges other-symbol series when the symbol actually CHANGES, so
    /// secondary series (cross-symbol, higher or lower timeframe) fetched
    /// during a run survive re-runs of the same chart and are dropped only when
    /// the chart symbol flips. Symbols declared via [`BarStore::retain`] are
    /// never purged.
    pub fn retain_symbol(&mut self, symbol: &str) {
        if self.current_symbol.as_deref() == Some(symbol) {
            return;
        }
        self.current_symbol = Some(symbol.to_owned());
        self.purge_outside();
    }

    /// Declare the set of symbols a multi-chart workspace is displaying
    /// (CANONICAL tickers, post-registry resolution). These survive every
    /// [`BarStore::retain_symbol`] purge, so cells loading different symbols
    /// stop evicting each other's history. Replaces the previous set FOR THAT
    /// OWNER (several shells on one page keep separate declarations, the
    /// effective set is their union) and purges anything now outside the union
    /// plus the current symbol immediately. An empty set releases the owner's
    /// declaration; with no owners left, the legacy single-chart policy is
    /// back. Secondary symbols a script fetches are not in this set and still
    /// drop on cross-cell loads: correctness is unaffected (they re-fetch on
    /// demand).
    pub fn retain<I, S>(&mut self, symbols: I, owner: &str)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let declared: HashSet<String> = symbols.into_iter().map(Into::into).collect();
        if declared.is_empty() {
            self.retained_by_owner.remove(owner);
        } else {
            self.retained_by_owner.insert(owner.to_owned(), declared);
        }
        self.retained = self.retained_by_owner.values().flatten().cloned().collect();
        self.purge_outside();
    }

    /// Drop every series whose symbol is neither the current one nor retained.
    fn purge_outside(&mut self) {
        let current = self.current_symbol.as_deref();
        let retained = &self.retained;
        let doomed: Vec<String> = self
            .series
            .keys()
            .filter(|key| {
                let sym = symbol_of(key);
                Some(sym) != current && !retained.contains(sym)
            })
            .cloned()
            .collect();
        for key in doomed {
            self.series.remove(&key);
            self.covered_from.remove(&key);
        }
    }

    pub fn clear(&mut self) {
        self.series.clear();
        self.covered_from.clear();
    }
}

// --- symbol-base ----------------------------------------------------------

/// Quote suffixes `base_of` strips from a bare ticker.
const QUOTE_SUFFIXES: [&str; 8] = ["USDT", "USDC", "USD1", "USDS", "BUSD", "USD", "EUR", "PERP"];

/// The BASE asset of a descriptor: the description's first segment
/// (`BTC / USDT`), else the ticker with common quote suffixes stripped.
pub fn base_of(d: &SymbolDescriptor) -> String {
    let from_desc = d
        .description
        .as_deref()
        .and_then(|desc| desc.split('/').next())
        .map(|first| first.trim_matches(is_js_space))
        .filter(|first| !first.is_empty());
    if let Some(first) = from_desc {
        return strip_perpetual(first).to_owned();
    }
    let stripped = strip_quote_suffix(&d.ticker);
    if stripped.is_empty() {
        d.ticker.clone()
    } else {
        stripped.to_owned()
    }
}

/// `s.replace(/\s+Perpetual$/i, '')`.
fn strip_perpetual(s: &str) -> &str {
    const WORD: &str = "perpetual";
    let Some(cut) = s.len().checked_sub(WORD.len()) else {
        return s;
    };
    if !s.is_char_boundary(cut) || !s[cut..].eq_ignore_ascii_case(WORD) {
        return s;
    }
    let before = &s[..cut];
    let trimmed = before.trim_end_matches(is_js_space);
    if trimmed.len() < before.len() {
        trimmed
    } else {
        s
    }
}

/// `t.replace(/[-_/]?(USDT|USDC|USD1|USDS|BUSD|USD|EUR|PERP)$/i, '')`: the
/// leftmost match wins, so the earliest-starting suffix (with its optional
/// separator) is the one removed.
fn strip_quote_suffix(ticker: &str) -> &str {
    let bytes = ticker.as_bytes();
    let start = QUOTE_SUFFIXES
        .iter()
        .filter_map(|suffix| {
            let at = bytes.len().checked_sub(suffix.len())?;
            if !bytes[at..].eq_ignore_ascii_case(suffix.as_bytes()) {
                return None;
            }
            let has_separator = at > 0 && matches!(bytes[at - 1], b'-' | b'_' | b'/');
            Some(if has_separator { at - 1 } else { at })
        })
        .min();
    start.map_or(ticker, |at| &ticker[..at])
}

/// `encodeURIComponent` over the UTF-8 bytes of `s`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The Ledger crypto-icon CDN URL for a BASE asset (CORS-clean, PNG): what the
/// bundled crypto providers predefine their icon resolvers with. An empty base
/// gives `None` (the shells' initials badge takes over).
pub fn ledger_crypto_icon_url(base: &str) -> Option<String> {
    let key = base.trim_matches(is_js_space).to_uppercase();
    if key.is_empty() {
        None
    } else {
        Some(format!(
            "https://crypto-icons.ledger.com/{}.png",
            encode_uri_component(&key)
        ))
    }
}

// --- symbol-groups --------------------------------------------------------
// Symbol GROUPS (futures roots): the pure vocabulary shared by every consumer
// that folds or resolves grouped listings: the picker (fold members under the
// group row, expand on demand) and the registry (translate a picked or typed
// group to the member it loads).

/// A GROUP's own row repeats the group in `ticker`: the port's documented
/// invariant (`SymbolDescriptor::group`); members carry the same `group` with
/// their own ticker.
pub fn is_group_row(d: &SymbolDescriptor) -> bool {
    d.group.as_deref() == Some(d.ticker.as_str())
}

/// The venue-scoped identity of a row's group: two venues may both list an
/// `ES` root.
pub fn group_key_of(d: &SymbolDescriptor) -> String {
    let venue = d.prefix.as_deref().or(d.provider.as_deref()).unwrap_or("");
    format!(
        "{}:{}",
        venue.to_lowercase(),
        d.group.as_deref().unwrap_or("").to_uppercase()
    )
}

/// The group's MEMBER rows (its loadable symbols), in the pool's own order:
/// providers emit members deliberately ordered, and the zero-or-many-defaults
/// fallback leans on it.
pub fn group_members<'a>(
    pool: &'a [SymbolDescriptor],
    group_row: &SymbolDescriptor,
) -> Vec<&'a SymbolDescriptor> {
    let key = group_key_of(group_row);
    pool.iter()
        .filter(|s| s.group.is_some() && !is_group_row(s) && group_key_of(s) == key)
        .collect()
}

/// The member a GROUP pick loads: the single `default`, else (none or several
/// marked) the FIRST listed member (the agreed client-side rule). `None` only
/// for a memberless group.
pub fn default_member_of<'a>(
    pool: &'a [SymbolDescriptor],
    group_row: &SymbolDescriptor,
) -> Option<&'a SymbolDescriptor> {
    let members = group_members(pool, group_row);
    let mut defaults = members.iter().filter(|m| m.default == Some(true));
    match (defaults.next(), defaults.next()) {
        (Some(only), None) => Some(only),
        _ => members.first().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const DAY: i64 = 86_400_000;
    const MONTH: i64 = 30 * DAY;

    // ---- timeframe_to_ms ----

    #[test]
    fn bare_numbers_are_minutes_and_named_periods_themselves() {
        assert_eq!(timeframe_to_ms("1"), MIN);
        assert_eq!(timeframe_to_ms("240"), 240 * MIN);
        assert_eq!(timeframe_to_ms("D"), DAY);
        assert_eq!(timeframe_to_ms("W"), 7 * DAY);
        assert_eq!(timeframe_to_ms("M"), MONTH);
    }

    #[test]
    fn a_counted_month_is_months_not_minutes() {
        assert_eq!(timeframe_to_ms("1M"), MONTH);
        assert_eq!(timeframe_to_ms("3M"), 3 * MONTH);
        assert_eq!(timeframe_to_ms("12M"), 12 * MONTH);
    }

    #[test]
    fn lowercase_m_stays_minutes() {
        assert_eq!(timeframe_to_ms("1m"), MIN);
        assert_eq!(timeframe_to_ms("15m"), 15 * MIN);
    }

    #[test]
    fn counted_days_weeks_and_aliases_in_either_case() {
        assert_eq!(timeframe_to_ms("1D"), DAY);
        assert_eq!(timeframe_to_ms("2W"), 14 * DAY);
        assert_eq!(timeframe_to_ms("4h"), 4 * 60 * MIN);
        assert_eq!(timeframe_to_ms("4H"), 4 * 60 * MIN);
        assert_eq!(timeframe_to_ms("1d"), DAY);
        assert_eq!(timeframe_to_ms("1w"), 7 * DAY);
    }

    #[test]
    fn seconds_timeframes() {
        assert_eq!(timeframe_to_ms("30S"), 30_000);
        assert_eq!(timeframe_to_ms("1s"), 1_000);
    }

    #[test]
    fn unreadable_timeframes_fall_back_to_one_hour() {
        assert_eq!(timeframe_to_ms(""), 60 * MIN);
        assert_eq!(timeframe_to_ms("5Y"), 60 * MIN);
        assert_eq!(timeframe_to_ms("abc"), 60 * MIN);
    }

    #[test]
    fn whitespace_and_oversized_counts() {
        assert_eq!(timeframe_to_ms("  15 \n"), 15 * MIN);
        assert_eq!(timeframe_to_ms("15 m"), 15 * MIN); // `\s*` between count and unit
        assert_eq!(timeframe_to_ms("15mm"), 60 * MIN);
        assert_eq!(timeframe_to_ms("1.5"), 60 * MIN);
        assert_eq!(timeframe_to_ms("-5"), 60 * MIN);
        assert_eq!(timeframe_to_ms("0"), 0);
        assert_eq!(timeframe_to_ms("99999999999999999999999"), i64::MAX);
        assert_eq!(timeframe_to_ms("\u{feff}D\u{feff}"), DAY);
        assert_eq!(timeframe_to_ms("\u{85}D"), 60 * MIN); // NEL is not JS whitespace
    }

    // ---- BarStore ----

    const START: i64 = 1_700_000_000_000;
    const STEP: i64 = 3_600_000;

    fn bars(count: i64) -> Vec<Ohlcv> {
        (0..count)
            .map(|i| {
                let price = (100 + i % 7) as f64;
                Ohlcv::new(
                    START + i * STEP,
                    price,
                    price + 1.0,
                    price - 1.0,
                    price,
                    Some(1.0),
                )
            })
            .collect()
    }

    fn key(symbol: &str) -> String {
        series_key("binance", symbol, "60", None)
    }

    fn len(store: &BarStore, symbol: &str) -> Option<usize> {
        store.get(&key(symbol)).map(<[Ohlcv]>::len)
    }

    #[test]
    fn series_key_adds_only_a_non_default_session() {
        assert_eq!(
            series_key("binance", "BTCUSDT", "60", None),
            "binance|BTCUSDT|60"
        );
        assert_eq!(
            series_key("binance", "BTCUSDT", "60", Some("")),
            "binance|BTCUSDT|60"
        );
        assert_eq!(
            series_key("binance", "BTCUSDT", "60", Some("regular")),
            "binance|BTCUSDT|60"
        );
        assert_eq!(
            series_key("p", "AAPL", "D", Some("extended")),
            "p|AAPL|D|extended"
        );
    }

    #[test]
    fn legacy_default_retain_symbol_purges_other_symbols_on_change() {
        let mut store = BarStore::new();
        store.merge(&key("BTCUSDT"), &bars(10));
        store.retain_symbol("BTCUSDT");
        store.merge(&key("ETHUSDT"), &bars(10));
        store.retain_symbol("ETHUSDT");
        assert_eq!(len(&store, "BTCUSDT"), None);
        assert_eq!(len(&store, "ETHUSDT"), Some(10));
    }

    #[test]
    fn retained_symbols_survive_alternating_purges() {
        let mut store = BarStore::new();
        store.retain(["BTCUSDT", "ETHUSDT"], DEFAULT_RETAIN_OWNER);
        store.merge(&key("BTCUSDT"), &bars(10));
        store.retain_symbol("BTCUSDT");
        store.merge(&key("ETHUSDT"), &bars(10));
        store.retain_symbol("ETHUSDT");
        store.retain_symbol("BTCUSDT"); // flip back: ETH must survive too
        assert_eq!(len(&store, "BTCUSDT"), Some(10));
        assert_eq!(len(&store, "ETHUSDT"), Some(10));
    }

    #[test]
    fn non_retained_secondary_symbols_still_drop_on_a_cross_symbol_purge() {
        let mut store = BarStore::new();
        store.retain(["BTCUSDT", "ETHUSDT"], DEFAULT_RETAIN_OWNER);
        store.retain_symbol("BTCUSDT");
        store.merge(&key("BTCUSDT"), &bars(10));
        store.merge(&key("SOLUSDT"), &bars(10));
        store.retain_symbol("BTCUSDT");
        assert_eq!(len(&store, "SOLUSDT"), Some(10)); // same current symbol: idempotent
        store.retain_symbol("ETHUSDT");
        assert_eq!(len(&store, "SOLUSDT"), None);
        assert_eq!(len(&store, "BTCUSDT"), Some(10));
    }

    #[test]
    fn shrinking_the_retained_set_purges_dropped_symbols_and_their_coverage() {
        let mut store = BarStore::new();
        store.retain(["BTCUSDT", "ETHUSDT"], DEFAULT_RETAIN_OWNER);
        store.merge(&key("BTCUSDT"), &bars(10));
        store.merge(&key("ETHUSDT"), &bars(10));
        store.mark_covered(&key("ETHUSDT"), START);
        store.retain_symbol("BTCUSDT");
        store.retain(["BTCUSDT"], DEFAULT_RETAIN_OWNER);
        assert_eq!(len(&store, "ETHUSDT"), None);
        assert_eq!(store.covered_from_of(&key("ETHUSDT")), None);
        assert_eq!(len(&store, "BTCUSDT"), Some(10));
    }

    #[test]
    fn an_empty_set_restores_the_legacy_policy() {
        let mut store = BarStore::new();
        store.retain(["ETHUSDT"], DEFAULT_RETAIN_OWNER);
        store.merge(&key("ETHUSDT"), &bars(10));
        store.retain_symbol("BTCUSDT");
        assert_eq!(len(&store, "ETHUSDT"), Some(10)); // protected
        store.retain(Vec::<String>::new(), DEFAULT_RETAIN_OWNER);
        assert_eq!(len(&store, "ETHUSDT"), None); // protection dropped, BTC is current
    }

    #[test]
    fn owners_keep_separate_declarations_and_the_effective_set_is_their_union() {
        let mut store = BarStore::new();
        store.retain_symbol("BTCUSDT");
        store.retain(["ETHUSDT"], "shell-a");
        store.retain(["SOLUSDT"], "shell-b");
        store.merge(&key("ETHUSDT"), &bars(3));
        store.merge(&key("SOLUSDT"), &bars(3));
        store.retain(Vec::<String>::new(), "shell-a"); // shell-a is destroyed
        assert_eq!(len(&store, "ETHUSDT"), None);
        assert_eq!(len(&store, "SOLUSDT"), Some(3)); // shell-b's survives
    }

    #[test]
    fn merge_dedups_by_time_incoming_wins_and_keeps_order() {
        let mut store = BarStore::new();
        let k = key("BTCUSDT");
        let mut first = bars(3);
        first.reverse(); // unsorted input
        store.merge(&k, &first);
        let replacement = Ohlcv::new(START + STEP, 1.0, 2.0, 0.5, 9.0, None);
        let extra = Ohlcv::new(START + 5 * STEP, 1.0, 1.0, 1.0, 1.0, None);
        let merged = store.merge(&k, &[extra, replacement]).to_vec();
        let times: Vec<i64> = merged.iter().map(|b| b.time).collect();
        assert_eq!(
            times,
            vec![START, START + STEP, START + 2 * STEP, START + 5 * STEP]
        );
        assert_eq!(merged[1], replacement);
    }

    #[test]
    fn merging_nothing_returns_the_current_series_or_empty() {
        let mut store = BarStore::new();
        assert!(store.merge("absent", &[]).is_empty());
        assert_eq!(store.get("absent"), None);
        store.merge("k", &bars(2));
        assert_eq!(store.merge("k", &[]).len(), 2);
    }

    #[test]
    fn mark_covered_keeps_the_earliest_time() {
        let mut store = BarStore::new();
        assert_eq!(store.covered_from_of("k"), None);
        store.mark_covered("k", 500);
        store.mark_covered("k", 900);
        assert_eq!(store.covered_from_of("k"), Some(500));
        store.mark_covered("k", 100);
        assert_eq!(store.covered_from_of("k"), Some(100));
    }

    #[test]
    fn a_key_without_a_symbol_segment_is_purged() {
        let mut store = BarStore::new();
        store.merge("odd", &bars(1));
        store.retain_symbol("BTCUSDT");
        assert_eq!(store.get("odd"), None);
    }

    #[test]
    fn clear_drops_series_and_coverage() {
        let mut store = BarStore::new();
        store.merge("k", &bars(2));
        store.mark_covered("k", 1);
        store.clear();
        assert_eq!(store.get("k"), None);
        assert_eq!(store.covered_from_of("k"), None);
    }

    // ---- symbol-base ----

    fn desc(ticker: &str, description: Option<&str>) -> SymbolDescriptor {
        SymbolDescriptor {
            description: description.map(str::to_owned),
            ..SymbolDescriptor::new(ticker)
        }
    }

    #[test]
    fn base_of_prefers_the_description_strips_perpetual_and_falls_back_to_the_ticker() {
        assert_eq!(base_of(&desc("BTCUSDT", Some("BTC / USDT"))), "BTC");
        assert_eq!(
            base_of(&desc("BTCUSDT.P", Some("BTC / USDT Perpetual"))),
            "BTC"
        );
        assert_eq!(base_of(&desc("ETHUSDT", None)), "ETH");
        assert_eq!(base_of(&desc("SOL-USD", None)), "SOL");
        assert_eq!(base_of(&desc("USDT", None)), "USDT"); // never emptied
    }

    #[test]
    fn base_of_edge_cases() {
        assert_eq!(base_of(&desc("X", Some("ETH Perpetual"))), "ETH");
        assert_eq!(base_of(&desc("X", Some("ETH perpetual"))), "ETH");
        assert_eq!(base_of(&desc("X", Some("Perpetual"))), "Perpetual"); // needs a space before it
        assert_eq!(base_of(&desc("X", Some("  Bitcoin  / USD"))), "Bitcoin");
        assert_eq!(base_of(&desc("ETHUSDT", Some("   "))), "ETH"); // blank description falls back
        assert_eq!(base_of(&desc("ETHUSDT", Some(""))), "ETH");
        assert_eq!(base_of(&desc("BTC_PERP", None)), "BTC");
        assert_eq!(base_of(&desc("BTC/EUR", None)), "BTC");
        assert_eq!(base_of(&desc("btcbusd", None)), "btc");
        assert_eq!(base_of(&desc("BTCUSD1", None)), "BTC");
        assert_eq!(base_of(&desc("-USD", None)), "-USD"); // stripping would empty it
        assert_eq!(base_of(&desc("AAPL", None)), "AAPL");
        assert_eq!(base_of(&desc("日本USDT", None)), "日本");
    }

    #[test]
    fn ledger_icon_url_uppercases_and_encodes_and_empty_is_none() {
        assert_eq!(
            ledger_crypto_icon_url("btc").as_deref(),
            Some("https://crypto-icons.ledger.com/BTC.png")
        );
        assert_eq!(ledger_crypto_icon_url(""), None);
        assert_eq!(ledger_crypto_icon_url("  "), None);
        assert_eq!(
            ledger_crypto_icon_url("a b/c").as_deref(),
            Some("https://crypto-icons.ledger.com/A%20B%2FC.png")
        );
        assert_eq!(
            ledger_crypto_icon_url("1inch-x_y").as_deref(),
            Some("https://crypto-icons.ledger.com/1INCH-X_Y.png")
        );
    }

    // ---- symbol-groups ----

    fn row(ticker: &str, kind: &str, group: &str, prefix: &str, default: bool) -> SymbolDescriptor {
        SymbolDescriptor {
            kind: Some(kind.into()),
            group: Some(group.into()),
            prefix: Some(prefix.into()),
            default: default.then_some(true),
            ..SymbolDescriptor::new(ticker)
        }
    }

    fn cme() -> Vec<SymbolDescriptor> {
        vec![
            SymbolDescriptor {
                description: Some("E-mini S&P 500".into()),
                ..row("ES", "root", "ES", "CME", false)
            },
            row("ES1!", "futures", "ES", "CME", true),
            row("ES2!", "futures", "ES", "CME", false),
            SymbolDescriptor {
                description: Some("E-mini Nasdaq-100".into()),
                ..row("NQ", "root", "NQ", "CME", false)
            },
            row("NQ1!", "futures", "NQ", "CME", false), // deliberately NO default
            row("NQ2!", "futures", "NQ", "CME", false),
        ]
    }

    fn tickers(rows: &[&SymbolDescriptor]) -> Vec<String> {
        rows.iter().map(|r| r.ticker.clone()).collect()
    }

    #[test]
    fn a_group_row_repeats_its_group_in_ticker() {
        let pool = cme();
        assert!(is_group_row(&pool[0]));
        assert!(!is_group_row(&pool[1]));
        assert!(!is_group_row(&SymbolDescriptor::new("AAPL")));
    }

    #[test]
    fn members_come_back_in_pool_order_scoped_to_the_group() {
        let pool = cme();
        assert_eq!(tickers(&group_members(&pool, &pool[0])), ["ES1!", "ES2!"]);
    }

    #[test]
    fn the_single_default_member_is_what_a_group_pick_loads() {
        let pool = cme();
        assert_eq!(
            default_member_of(&pool, &pool[0]).map(|m| m.ticker.as_str()),
            Some("ES1!")
        );
    }

    #[test]
    fn zero_defaults_pick_the_first_listed_member() {
        let pool = cme();
        assert_eq!(
            default_member_of(&pool, &pool[3]).map(|m| m.ticker.as_str()),
            Some("NQ1!")
        );
    }

    #[test]
    fn many_defaults_still_pick_the_first_listed_member() {
        let pool = vec![
            row("GC", "root", "GC", "COMEX", false),
            row("GC2!", "futures", "GC", "COMEX", true),
            row("GC1!", "futures", "GC", "COMEX", true),
        ];
        assert_eq!(
            default_member_of(&pool, &pool[0]).map(|m| m.ticker.as_str()),
            Some("GC2!")
        );
    }

    #[test]
    fn group_identity_is_venue_scoped() {
        let mut pool = cme();
        pool.push(row("ES9!", "futures", "ES", "OTHER", true));
        assert_eq!(tickers(&group_members(&pool, &pool[0])), ["ES1!", "ES2!"]);
    }

    #[test]
    fn group_key_falls_back_to_provider_and_normalizes_case() {
        let d = SymbolDescriptor {
            group: Some("es".into()),
            provider: Some("CME".into()),
            ..SymbolDescriptor::new("ES")
        };
        assert_eq!(group_key_of(&d), "cme:ES");
        assert_eq!(group_key_of(&SymbolDescriptor::new("X")), ":");
        // An empty prefix does not fall through to the provider (`??`, not `||`).
        let empty_prefix = SymbolDescriptor {
            prefix: Some(String::new()),
            ..d
        };
        assert_eq!(group_key_of(&empty_prefix), ":ES");
    }

    #[test]
    fn a_memberless_group_resolves_to_nothing() {
        let lone = row("XX", "root", "XX", "CME", false);
        assert_eq!(default_member_of(std::slice::from_ref(&lone), &lone), None);
    }
}
