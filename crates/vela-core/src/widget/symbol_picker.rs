//! Symbol search: the pure half of the original `SymbolPicker` dialog.
//!
//! Here: the search and ranking ([`filter_symbols`]: ticker prefix, ticker substring,
//! description, venue; venue scopes), the group fold ([`fold_groups`]), the type tabs, the
//! avatar hue, and [`SymbolPickerModel`], the picker's state: tab, query, paging, expanded
//! groups, highlight, the ranked pool cache and the choice a pick commits.
//!
//! Left for the browser crate: the dialog, search input (upper-casing what is typed), tab
//! strip and list elements and their CSS, focus handling, the infinite-scroll listener
//! (call [`SymbolPickerModel::grow`] when the list nears its bottom), row elements with the
//! ticker icon ([`ticker_icon`](super::symbol_icon::ticker_icon)), and rendering the empty
//! states ([`SymbolPickerModel::empty_message`]).
//!
//! One approximation: the original sorts a venue's listing with `String.localeCompare`;
//! [`locale_compare`] orders punctuation before digits before letters, letters
//! case-insensitively (lower case first on a tie), which is what ICU does for tickers.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use super::contributions::{RankedPool, SymbolRankingHook};
use crate::data::{default_member_of, group_key_of, group_members, is_group_row};
use crate::i18n::{MessageLike, Translator, msg, text_of};
use crate::ports::data_provider::SymbolDescriptor;
use crate::runtime::Platform;

/// The identity of a row for de-duplication: venue + ticker.
fn row_key(s: &SymbolDescriptor) -> String {
    let venue = s.prefix.as_deref().or(s.provider.as_deref()).unwrap_or("");
    format!("{}:{}", venue.to_lowercase(), s.ticker.to_uppercase())
}

/// First occurrence wins, keyed by venue+ticker: a ranking hook may inject an entry that
/// also exists later in the pool; the injected one keeps position AND data.
pub fn dedupe_symbols(list: &[SymbolDescriptor]) -> Vec<SymbolDescriptor> {
    let mut seen = HashSet::new();
    list.iter()
        .filter(|s| seen.insert(row_key(s)))
        .cloned()
        .collect()
}

/// Fold GROUPED listings (futures roots): walking top-down, drop a MEMBER row whose group's
/// own row already appeared ABOVE it: the group stands for its members, which stay
/// reachable through its expander. A member that matched WITHOUT its group (the query hit
/// `ES2` but not `ES`) keeps its own row: it matched more specifically than the fold.
/// Order-respecting on purpose: decisions for a row never depend on rows BELOW it, so
/// growing the list (infinite scroll) appends without reshuffling.
pub fn fold_groups(list: &[SymbolDescriptor]) -> Vec<SymbolDescriptor> {
    let mut groups_above: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for s in list {
        if is_group_row(s) {
            groups_above.insert(group_key_of(s));
            out.push(s.clone());
        } else if s.group.is_none() || !groups_above.contains(&group_key_of(s)) {
            out.push(s.clone());
        }
    }
    out
}

/// The pinned head of the empty-query list (majors first, like the reference picker).
pub const TOP_TICKERS: [&str; 8] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "DOGEUSDT", "ADAUSDT", "LINKUSDT",
];

/// Deterministic avatar hue from a ticker (self-contained: no external icon service).
pub fn avatar_color(ticker: &str) -> String {
    let mut h: u32 = 0;
    for unit in ticker.encode_utf16() {
        h = (h * 31 + u32::from(unit)) % 360;
    }
    format!("hsl({h}, 42%, 38%)")
}

/// A query split into an optional venue SCOPE and a search TERM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedQuery {
    pub scope: Option<String>,
    pub term: String,
}

fn only_one<T: Clone>(matches: &[T]) -> Option<T> {
    if matches.len() == 1 {
        Some(matches[0].clone())
    } else {
        None
    }
}

/// Split a query into an optional venue SCOPE and a search TERM. A leading token (before a
/// `:` or a space) naming a venue present in the list (exactly, or as a unique prefix)
/// scopes the search to it: `binance:BTC`, `binance BTC` and `bina BTC` all read as
/// Binance's BTC; a token that is no venue (`BTC USD`) leaves the whole query as the term.
/// (The original's `/^\s*([^\s:]+)\s*[:\s]\s*(.*)$/`.)
pub fn parse_query(raw: &str, venues: &[String]) -> ParsedQuery {
    let whole = || ParsedQuery {
        scope: None,
        term: raw.trim().to_string(),
    };
    let s = raw.trim_start();
    let token_end = s
        .find(|c: char| c.is_whitespace() || c == ':')
        .unwrap_or(s.len());
    let (token, rest) = s.split_at(token_end);
    if token.is_empty() || rest.is_empty() {
        return whole();
    }
    // After the token: optional whitespace, then one `:` or whitespace separator, then the
    // term. (Whitespace alone separates when no colon follows.)
    let rest_trim = rest.trim_start_matches(char::is_whitespace);
    let term = if let Some(after) = rest_trim.strip_prefix(':') {
        after
    } else if rest.starts_with(char::is_whitespace) {
        // A whitespace separator: the first whitespace char is the separator itself, the
        // regex backtracks the preceding `\s*` to give it back.
        rest.strip_prefix(|c: char| c.is_whitespace())
            .unwrap_or(rest)
    } else {
        return whole();
    };
    let t = token.to_lowercase();
    let scope = if venues.contains(&t) {
        Some(t)
    } else {
        let candidates: Vec<String> = venues
            .iter()
            .filter(|v| v.starts_with(&t))
            .cloned()
            .collect();
        only_one(&candidates)
    };
    match scope {
        Some(scope) => ParsedQuery {
            scope: Some(scope),
            term: term.trim().to_string(),
        },
        None => whole(),
    }
}

/// The row's venue label: the LISTING prefix the data declares, else the provider name.
fn venue_of(s: &SymbolDescriptor) -> Option<&str> {
    s.prefix.as_deref().or(s.provider.as_deref())
}

/// `String.prototype.localeCompare` for tickers (see the module note).
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    fn class(c: char) -> u8 {
        if c.is_alphabetic() {
            2
        } else if c.is_numeric() {
            1
        } else {
            0
        }
    }
    let key = |c: char| (class(c), c.to_lowercase().collect::<String>());
    let (mut ia, mut ib) = (a.chars(), b.chars());
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = key(x).cmp(&key(y));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
    // Same letters: lower case sorts before upper case.
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            return if x.is_lowercase() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
    }
    Ordering::Equal
}

/// What an empty query pins: the built-in majors, a custom list, or nothing (a registered
/// symbol ranking already shaped the pool).
#[derive(Clone, Copy, Debug)]
pub enum TopPolicy<'a> {
    Pin(&'a [&'a str]),
    Off,
}

/// Rank: ticker prefix > ticker substring > description substring > venue-name substring
/// (typing `binance` surfaces that venue's symbols after any literal matches). An optional
/// venue prefix (see [`parse_query`]) scopes the pool first: venue alone browses it whole,
/// alphabetically. The `limit` caps the result; growing it yields a stable superset, so
/// the picker appends pages without reshuffling.
pub fn filter_symbols(
    list: &[SymbolDescriptor],
    query: &str,
    limit: usize,
    top: TopPolicy<'_>,
) -> Vec<SymbolDescriptor> {
    // The venues are the list's own: the picker's pool is already tab-filtered, and a scope
    // token must never resolve to a venue that has nothing to show. Both spellings scope:
    // the displayed listing venue (`nasdaq AAP`) and the provider id (`edgx AAP`).
    let mut venues: Vec<String> = Vec::new();
    for s in list {
        for v in [venue_of(s), s.provider.as_deref()].into_iter().flatten() {
            let v = v.to_lowercase();
            if !v.is_empty() && !venues.contains(&v) {
                venues.push(v);
            }
        }
    }
    let ParsedQuery { scope, term } = parse_query(query, &venues);
    let in_scope = |s: &&SymbolDescriptor| match &scope {
        Some(scope) => {
            venue_of(s).map(str::to_lowercase).as_deref() == Some(scope.as_str())
                || s.provider.as_deref().map(str::to_lowercase).as_deref() == Some(scope.as_str())
        }
        None => true,
    };
    let pool: Vec<&SymbolDescriptor> = list.iter().filter(in_scope).collect();
    let q = term.to_uppercase();
    if q.is_empty() {
        // Venue alone: browse the whole venue, alphabetically. Empty query: pin the `top`
        // tickers that exist in the index, then fill with the head.
        if scope.is_some() {
            let mut sorted = pool;
            sorted.sort_by(|a, b| locale_compare(&a.ticker, &b.ticker));
            return sorted.into_iter().take(limit).cloned().collect();
        }
        let TopPolicy::Pin(top) = top else {
            return pool.into_iter().take(limit).cloned().collect();
        };
        let pinned: Vec<&SymbolDescriptor> = top
            .iter()
            .filter_map(|t| {
                // `new Map(...)`: a later duplicate ticker overwrites an earlier one.
                pool.iter()
                    .rev()
                    .find(|s| s.ticker.to_uppercase() == *t)
                    .copied()
            })
            .collect();
        let rest = pool
            .iter()
            .filter(|s| !top.contains(&s.ticker.to_uppercase().as_str()));
        return pinned
            .into_iter()
            .chain(rest.copied())
            .take(limit)
            .cloned()
            .collect();
    }
    let q_lower = term.to_lowercase();
    let (mut prefix, mut substr, mut desc, mut venue) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for s in &pool {
        let t = s.ticker.to_uppercase();
        if t.starts_with(&q) {
            prefix.push(*s);
        } else if t.contains(&q) {
            substr.push(*s);
        } else if s
            .description
            .as_deref()
            .unwrap_or("")
            .to_uppercase()
            .contains(&q)
        {
            desc.push(*s);
        } else if scope.is_none()
            // Inside a scope the venue is fixed: the tier would swallow the whole pool.
            && (venue_of(s).is_some_and(|v| v.to_lowercase().contains(&q_lower))
                || s.provider.as_deref().is_some_and(|p| p.to_lowercase().contains(&q_lower)))
        {
            venue.push(*s);
        }
        if prefix.len() >= limit {
            break;
        }
    }
    prefix
        .into_iter()
        .chain(substr)
        .chain(desc)
        .chain(venue)
        .take(limit)
        .cloned()
        .collect()
}

/// The asset-class tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SymbolTab {
    #[default]
    All,
    Stocks,
    Etfs,
    Crypto,
    Futures,
    Forex,
    Commodities,
}

impl SymbolTab {
    pub const ALL: [SymbolTab; 7] = [
        SymbolTab::All,
        SymbolTab::Stocks,
        SymbolTab::Etfs,
        SymbolTab::Crypto,
        SymbolTab::Futures,
        SymbolTab::Forex,
        SymbolTab::Commodities,
    ];

    /// The tab's id; the label is display text only.
    pub fn id(self) -> &'static str {
        match self {
            SymbolTab::All => "all",
            SymbolTab::Stocks => "stocks",
            SymbolTab::Etfs => "etfs",
            SymbolTab::Crypto => "crypto",
            SymbolTab::Futures => "futures",
            SymbolTab::Forex => "forex",
            SymbolTab::Commodities => "commodities",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    pub fn label(self) -> MessageLike {
        match self {
            SymbolTab::All => msg("widget.symbolPicker.tab.all", "All"),
            SymbolTab::Stocks => msg("widget.symbolPicker.tab.stocks", "Stocks"),
            SymbolTab::Etfs => msg("widget.symbolPicker.tab.etfs", "ETFs"),
            SymbolTab::Crypto => msg("widget.symbolPicker.tab.crypto", "Crypto"),
            SymbolTab::Futures => msg("widget.symbolPicker.tab.futures", "Futures"),
            SymbolTab::Forex => msg("widget.symbolPicker.tab.forex", "Forex"),
            SymbolTab::Commodities => msg("widget.symbolPicker.tab.commodities", "Commodities"),
        }
    }

    /// The descriptor types each tab lists (crypto adds perpetual futures, see
    /// [`tab_lists`]). `All` lists every type.
    pub fn types(self) -> &'static [&'static str] {
        match self {
            SymbolTab::All => &[],
            SymbolTab::Crypto => &["crypto"],
            SymbolTab::Stocks => &["stock"],
            SymbolTab::Etfs => &["etf"],
            SymbolTab::Futures => &["futures", "root"],
            SymbolTab::Forex => &["forex"],
            SymbolTab::Commodities => &["commodity"],
        }
    }
}

/// Whether `tab` lists a descriptor: its type is one of the tab's, and the crypto tab also
/// lists perpetual futures.
pub fn tab_lists(tab: SymbolTab, s: &SymbolDescriptor) -> bool {
    if tab == SymbolTab::All {
        return true;
    }
    let kind = s.kind.as_deref().unwrap_or("").to_lowercase();
    tab.types().contains(&kind.as_str()) || (tab == SymbolTab::Crypto && kind == "futures")
}

/// Rows rendered per page: the list GROWS by this much every time the scroll nears the
/// bottom, so a 13k-symbol venue browses whole without ever rendering 13k rows up front.
pub const PAGE: usize = 100;

/// The picker's title, search placeholder and empty states.
pub fn title_message() -> MessageLike {
    msg("widget.symbolPicker.title", "Symbol Search")
}
pub fn search_placeholder() -> MessageLike {
    msg("widget.symbolPicker.search", "Search symbol\u{2026}")
}
pub fn no_match_message() -> MessageLike {
    msg("widget.symbolPicker.noMatch", "No symbols match.")
}
pub fn empty_message() -> MessageLike {
    msg("widget.symbolPicker.empty", "No symbols available yet.")
}
pub fn show_contracts_label() -> MessageLike {
    msg("widget.symbolPicker.showContracts", "Show contracts")
}

/// Where the picker's symbols come from.
pub type SymbolSource = Rc<dyn Fn() -> Rc<Vec<SymbolDescriptor>>>;
/// The registered symbol ranking, read live.
pub type RankingLookup = Rc<dyn Fn() -> Option<SymbolRankingHook>>;

/// The ranked pool cache: `key` fingerprints the raw pool the ranking ran on.
struct Ranked {
    key: String,
    result: Rc<Vec<SymbolDescriptor>>,
}

#[derive(Default)]
struct State {
    rows: Vec<SymbolDescriptor>,
    highlighted: usize,
    seed: String,
    query: String,
    tab: SymbolTab,
    visible: usize,
    /// The last filter pass returned fewer raw rows than asked: the pool is drained
    /// (checked BEFORE folding: folding shortens pages without meaning exhaustion).
    exhausted: bool,
    /// Group rows currently expanded (venue-scoped keys): members shown inline.
    expanded: HashSet<String>,
    ranked: Option<Ranked>,
    ranking: bool,
}

/// The picker's state and behavior without a dialog. The view calls the mutators and paints
/// [`rows`](Self::rows) with the [`highlighted`](Self::highlighted) one marked.
pub struct SymbolPickerModel {
    platform: Rc<dyn Platform>,
    source: RefCell<SymbolSource>,
    ranking: RankingLookup,
    /// Repaint the (possibly open) list: called when an async ranking lands.
    repaint: Rc<dyn Fn()>,
    state: Rc<RefCell<State>>,
}

impl SymbolPickerModel {
    pub fn new(
        platform: Rc<dyn Platform>,
        ranking: RankingLookup,
        repaint: impl Fn() + 'static,
    ) -> Self {
        let state = State {
            visible: PAGE,
            ..State::default()
        };
        Self {
            platform,
            source: RefCell::new(Rc::new(|| Rc::new(Vec::new()))),
            ranking,
            repaint: Rc::new(repaint),
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Wire where symbols come from (re-called on every widget rebuild).
    pub fn set_source(&self, source: SymbolSource) {
        *self.source.borrow_mut() = source;
    }

    /// Text to seed the field with when the dialog opens.
    pub fn set_seed(&self, text: &str) {
        self.state.borrow_mut().seed = text.to_string();
    }

    /// The dialog opened: the query becomes the (upper-cased) seed, which is then
    /// consumed, and the list refreshes. Returns the query to put in the field.
    pub fn on_open(&self) -> String {
        {
            let mut st = self.state.borrow_mut();
            st.query = std::mem::take(&mut st.seed).to_uppercase();
        }
        self.refresh();
        self.query()
    }

    pub fn query(&self) -> String {
        self.state.borrow().query.clone()
    }

    /// The field's text changed (the view upper-cases it first).
    pub fn set_query(&self, query: &str) {
        self.state.borrow_mut().query = query.to_string();
        self.refresh();
    }

    /// Text typed at the picker that never reached its field while it is open: it extends
    /// the query (the field takes focus a tick after the dialog does, and a fast typist's
    /// next key lands in that tick). Returns the new query.
    pub fn append_typed(&self, text: &str) -> String {
        let next = format!("{}{}", self.query(), text).to_uppercase();
        self.set_query(&next);
        next
    }

    pub fn tab(&self) -> SymbolTab {
        self.state.borrow().tab
    }

    pub fn set_tab(&self, tab: SymbolTab) {
        self.state.borrow_mut().tab = tab;
        self.refresh();
    }

    pub fn rows(&self) -> Vec<SymbolDescriptor> {
        self.state.borrow().rows.clone()
    }

    pub fn highlighted(&self) -> usize {
        self.state.borrow().highlighted
    }

    /// Whether the pool is drained: scrolling has nothing more to load.
    pub fn exhausted(&self) -> bool {
        self.state.borrow().exhausted
    }

    pub fn is_expanded(&self, s: &SymbolDescriptor) -> bool {
        self.state.borrow().expanded.contains(&group_key_of(s))
    }

    /// The text of the list's empty state: nothing matched the query, or the pool is empty.
    pub fn empty_message(&self, i18n: Option<&dyn Translator>) -> String {
        let has_query = !self.state.borrow().query.is_empty();
        text_of(
            i18n,
            &if has_query {
                no_match_message()
            } else {
                empty_message()
            },
            &[],
        )
    }

    /// The picker's pool: the source, shaped by the registered symbol ranking. Cached: the
    /// hook runs when the pool CHANGES (an index lands or refreshes), never per keystroke;
    /// an async hook resolves onto the next repaint (stale-while-revalidate in between, the
    /// raw pool before the first resolve).
    fn pool(&self) -> Rc<Vec<SymbolDescriptor>> {
        let source = Rc::clone(&self.source.borrow());
        let raw = source();
        let Some(hook) = (self.ranking)() else {
            return raw;
        };
        let key = format!(
            "{}:{}:{}",
            raw.len(),
            raw.first().map_or("", |s| s.ticker.as_str()),
            raw.last().map_or("", |s| s.ticker.as_str())
        );
        let start = {
            let mut st = self.state.borrow_mut();
            let stale = st.ranked.as_ref().is_none_or(|r| r.key != key);
            if stale && !st.ranking {
                st.ranking = true;
                true
            } else {
                false
            }
        };
        if start {
            let copy = raw.as_ref().clone();
            let state = Rc::clone(&self.state);
            let repaint = Rc::clone(&self.repaint);
            let ranked_key = key;
            self.platform.spawn(Box::pin(async move {
                let out = match hook(copy) {
                    RankedPool::Ready(v) => v,
                    RankedPool::Pending(f) => f.await,
                };
                {
                    let mut st = state.borrow_mut();
                    st.ranked = Some(Ranked {
                        key: ranked_key,
                        result: Rc::new(dedupe_symbols(&out)),
                    });
                    st.ranking = false;
                }
                repaint(); // repaint the (possibly open) list with the ranked pool
            }));
        }
        match &self.state.borrow().ranked {
            Some(r) => Rc::clone(&r.result),
            None => raw,
        }
    }

    fn compute_rows(&self) -> Vec<SymbolDescriptor> {
        let all = self.pool();
        let (tab, query, visible, expanded) = {
            let st = self.state.borrow();
            (st.tab, st.query.clone(), st.visible, st.expanded.clone())
        };
        let pool: Vec<SymbolDescriptor> = if tab == SymbolTab::All {
            all.as_ref().clone()
        } else {
            all.iter().filter(|s| tab_lists(tab, s)).cloned().collect()
        };
        // A registered ranking OWNS the empty-query head: the built-in majors pin stands
        // down and the pool's own order shows.
        let top = if (self.ranking)().is_some() {
            TopPolicy::Off
        } else {
            TopPolicy::Pin(&TOP_TICKERS)
        };
        let filtered = filter_symbols(&pool, &query, visible, top);
        self.state.borrow_mut().exhausted = filtered.len() < visible;
        let folded = fold_groups(&filtered);
        if expanded.is_empty() {
            return folded;
        }
        // Splice each EXPANDED group's members right under its row: every member of the
        // group (the full pool knows them all, whatever the query matched), minus any
        // already standing on its own row elsewhere in the list.
        let mut present: HashSet<String> = folded.iter().map(row_key).collect();
        let mut out = Vec::new();
        for s in &folded {
            out.push(s.clone());
            if !is_group_row(s) || !expanded.contains(&group_key_of(s)) {
                continue;
            }
            for m in group_members(&all, s) {
                if present.insert(row_key(m)) {
                    out.push(m.clone());
                }
            }
        }
        out
    }

    /// A new query/tab: back to one page, the first row highlighted.
    pub fn refresh(&self) {
        self.state.borrow_mut().visible = PAGE;
        let rows = self.compute_rows();
        let mut st = self.state.borrow_mut();
        st.rows = rows;
        st.highlighted = 0;
    }

    /// The list nears its bottom: uncover the next page. Returns the rows to APPEND
    /// (rows already on screen stay put), empty when the pool is drained.
    pub fn grow(&self) -> Vec<SymbolDescriptor> {
        {
            let mut st = self.state.borrow_mut();
            if st.exhausted {
                return Vec::new();
            }
            st.visible += PAGE;
        }
        let already = self.state.borrow().rows.len();
        let rows = self.compute_rows();
        let appended = rows.get(already..).unwrap_or_default().to_vec();
        self.state.borrow_mut().rows = rows;
        appended
    }

    /// Expand/collapse a group row IN PLACE: same query, same page; only the member rows
    /// under the group appear or go. The highlight follows the row it was on.
    pub fn toggle_expand(&self, s: &SymbolDescriptor) {
        let key = group_key_of(s);
        let focus = {
            let mut st = self.state.borrow_mut();
            if !st.expanded.remove(&key) {
                st.expanded.insert(key);
            }
            st.rows.get(st.highlighted).cloned()
        };
        let rows = self.compute_rows();
        let mut st = self.state.borrow_mut();
        let at = focus.and_then(|f| rows.iter().position(|r| *r == f));
        st.highlighted = match at {
            Some(i) => i,
            None => st.highlighted.min(rows.len().saturating_sub(1)),
        };
        st.rows = rows;
    }

    /// ArrowDown (`1`) / ArrowUp (`-1`): clamped to the rows.
    pub fn move_highlight(&self, delta: i64) {
        let mut st = self.state.borrow_mut();
        if st.rows.is_empty() {
            return;
        }
        let next = (st.highlighted as i64 + delta).clamp(0, st.rows.len() as i64 - 1);
        st.highlighted = next as usize;
    }

    /// Route a row activation: a GROUP row loads its default member (the root itself is
    /// listed, never loadable), any other row loads itself. Returns the committed symbol:
    /// `EXCHANGE:`-prefixed when the row named a venue, so the selection pins the venue the
    /// user actually pointed at (several venues may list this ticker, and a bare ticker
    /// would re-resolve by declaration order). The prefix is the CANONICAL one: the listing
    /// prefix when the data declares it (`NASDAQ:AAPL`), the provider name otherwise.
    pub fn pick(&self, s: &SymbolDescriptor) -> String {
        let pool = self.pool();
        let target = if is_group_row(s) {
            default_member_of(&pool, s).unwrap_or(s)
        } else {
            s
        };
        selection_symbol(&target.ticker, venue_of(target))
    }

    /// Enter: pick the highlighted row, if any.
    pub fn pick_highlighted(&self) -> Option<String> {
        let row = {
            let st = self.state.borrow();
            st.rows.get(st.highlighted).cloned()
        };
        row.map(|r| self.pick(&r))
    }
}

/// `VENUE:TICKER`, or the bare ticker for a row without a venue.
pub fn selection_symbol(ticker: &str, venue: Option<&str>) -> String {
    match venue {
        Some(v) if !v.is_empty() => format!("{v}:{ticker}"),
        _ => ticker.to_string(),
    }
}

#[cfg(test)]
mod tests;
