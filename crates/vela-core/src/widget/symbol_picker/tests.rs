use std::cell::Cell;

use super::*;
use crate::runtime::testing::ManualPlatform;

fn sym(ticker: &str, description: Option<&str>, provider: Option<&str>) -> SymbolDescriptor {
    SymbolDescriptor {
        description: description.map(str::to_string),
        provider: provider.map(str::to_string),
        ..SymbolDescriptor::new(ticker)
    }
}

fn tickers(list: &[SymbolDescriptor]) -> Vec<&str> {
    list.iter().map(|s| s.ticker.as_str()).collect()
}

const PIN: TopPolicy<'static> = TopPolicy::Pin(&TOP_TICKERS);

fn filter(list: &[SymbolDescriptor], q: &str) -> Vec<SymbolDescriptor> {
    filter_symbols(list, q, 100, PIN)
}

fn base_list() -> Vec<SymbolDescriptor> {
    vec![
        sym("BTCUSDT", Some("Bitcoin / TetherUS"), None),
        sym("ETHUSDT", Some("Ethereum / TetherUS"), None),
        sym("WBTCUSDT", Some("Wrapped Bitcoin"), None),
        sym("SOLUSDT", Some("Solana"), None),
    ]
}

#[test]
fn ranks_ticker_prefix_above_substring_above_description_matches() {
    let list = base_list();
    assert_eq!(tickers(&filter(&list, "BTC")), ["BTCUSDT", "WBTCUSDT"]);
    assert_eq!(tickers(&filter(&list, "bitcoin")), ["BTCUSDT", "WBTCUSDT"]);
    // BTCUSDT matches via its description ("TetherUS" contains "eth"): ranked after the prefix hit.
    assert_eq!(tickers(&filter(&list, "ETH")), ["ETHUSDT", "BTCUSDT"]);
    // The picker stores typed queries in uppercase; matching stays case-insensitive.
    assert_eq!(filter(&list, "btcusdt"), filter(&list, "BTCUSDT"));
    assert_eq!(filter(&list, "BtC"), filter(&list, "BTC"));
}

#[test]
fn an_empty_query_returns_the_head_of_the_list_and_the_limit_caps_results() {
    let list = base_list();
    assert_eq!(
        tickers(&filter_symbols(&list, "", 2, PIN)),
        ["BTCUSDT", "ETHUSDT"]
    );
    assert_eq!(filter_symbols(&list, "USDT", 2, PIN).len(), 2);
    assert!(filter_symbols(&list, "", 0, PIN).is_empty());
    assert!(filter_symbols(&[], "BTC", 10, PIN).is_empty());
}

#[test]
fn the_empty_query_pin_is_parameterized() {
    let list = base_list();
    let shuffled = vec![
        list[3].clone(),
        list[2].clone(),
        list[0].clone(),
        list[1].clone(),
    ]; // SOL, WBTC, BTC, ETH
    // default: the built-in majors pin (BTCUSDT sits in it: pulled ahead)
    assert_eq!(
        tickers(&filter_symbols(&shuffled, "", 100, PIN)),
        ["BTCUSDT", "ETHUSDT", "SOLUSDT", "WBTCUSDT"]
    );
    // custom top list: pinned in ITS order, rest keeps pool order
    assert_eq!(
        tickers(&filter_symbols(
            &shuffled,
            "",
            100,
            TopPolicy::Pin(&["WBTCUSDT"])
        )),
        ["WBTCUSDT", "SOLUSDT", "BTCUSDT", "ETHUSDT"]
    );
    // off: a registered symbol ranking already shaped the pool: trust its order
    assert_eq!(
        tickers(&filter_symbols(&shuffled, "", 100, TopPolicy::Off)),
        ["SOLUSDT", "WBTCUSDT", "BTCUSDT", "ETHUSDT"]
    );
    // a typed query is untouched by the pin policy
    assert_eq!(
        tickers(&filter_symbols(&shuffled, "BTC", 100, TopPolicy::Off)),
        ["BTCUSDT", "WBTCUSDT"]
    );
}

#[test]
fn a_grown_limit_yields_a_stable_superset() {
    // The infinite-scroll contract: re-running with a larger limit must keep the smaller
    // result as its exact prefix, for every query shape (empty, ranked search, scoped).
    let wide: Vec<SymbolDescriptor> = (0..250)
        .map(|i| {
            sym(
                &format!("AA{i:03}"),
                Some(&format!("Issue {i}")),
                Some("edgx"),
            )
        })
        .collect();
    for q in ["", "AA", "Issue", "edgx:"] {
        let small = filter_symbols(&wide, q, 100, PIN);
        let big = filter_symbols(&wide, q, 200, PIN);
        assert_eq!(big[..small.len()], small[..], "{q:?}");
        assert!(big.len() > small.len(), "{q:?}");
    }
}

fn venues() -> Vec<SymbolDescriptor> {
    vec![
        sym("BTCUSDT", Some("Bitcoin / TetherUS"), Some("binance")),
        sym("ETHUSDT", Some("Ethereum / TetherUS"), Some("binance")),
        sym("BTCUSD", Some("Bitcoin / USD"), Some("binance.us")),
        sym("BTC-USD", Some("Bitcoin"), Some("coinbase")),
        sym("ADA-USD", Some("Cardano"), Some("coinbase")),
        sym("BTC", Some("BTC / USD Perpetual"), Some("hyperliquid")),
    ]
}

fn labelled(list: &[SymbolDescriptor], q: &str) -> Vec<String> {
    filter(list, q)
        .iter()
        .map(|s| format!("{}:{}", s.provider.as_deref().unwrap_or(""), s.ticker))
        .collect()
}

#[test]
fn a_venue_name_alone_surfaces_that_venue_after_literal_matches_case_insensitively() {
    let v = venues();
    assert_eq!(
        labelled(&v, "coinbase"),
        ["coinbase:BTC-USD", "coinbase:ADA-USD"]
    );
    assert_eq!(
        labelled(&v, "COINBASE"),
        ["coinbase:BTC-USD", "coinbase:ADA-USD"]
    );
    // "binance" is a substring of both binance and binance.us provider names.
    assert_eq!(
        labelled(&v, "binance"),
        ["binance:BTCUSDT", "binance:ETHUSDT", "binance.us:BTCUSD"]
    );
}

#[test]
fn venue_plus_a_separator_browses_the_venue_whole_alphabetically() {
    assert_eq!(
        labelled(&venues(), "coinbase:"),
        ["coinbase:ADA-USD", "coinbase:BTC-USD"]
    );
}

#[test]
fn venue_term_and_venue_space_term_scope_the_ranked_search_to_the_venue() {
    let v = venues();
    assert_eq!(labelled(&v, "binance:btc"), ["binance:BTCUSDT"]);
    assert_eq!(labelled(&v, "binance btc"), ["binance:BTCUSDT"]);
    assert_eq!(labelled(&v, "coinbase BTC"), ["coinbase:BTC-USD"]);
    // Inside a scope the venue tier is off: a term matching only the venue name adds nothing.
    assert!(labelled(&v, "coinbase base").is_empty());
}

#[test]
fn a_unique_venue_prefix_scopes_and_an_ambiguous_one_leaves_the_query_as_a_term() {
    let v = venues();
    assert_eq!(labelled(&v, "coin BTC"), ["coinbase:BTC-USD"]);
    assert_eq!(labelled(&v, "hyper btc"), ["hyperliquid:BTC"]);
    // "binan" prefixes binance AND binance.us: no scope, and no literal match either.
    assert!(labelled(&v, "binan btc").is_empty());
    // Exact name wins over its own extensions: "binance" scopes to binance, not binance.us.
    assert_eq!(
        labelled(&v, "binance:usd"),
        ["binance:BTCUSDT", "binance:ETHUSDT"]
    );
}

#[test]
fn a_leading_token_that_is_no_venue_stays_part_of_the_term() {
    let v = venues();
    assert!(labelled(&v, "BTC USD").is_empty()); // no normalization across the space: not a scope
    assert_eq!(
        tickers(&filter(&v, "btc")),
        ["BTCUSDT", "BTCUSD", "BTC-USD", "BTC"]
    );
}

#[test]
fn symbols_without_a_provider_never_resolve_or_match_a_venue_token() {
    let mut mixed = vec![sym("OFFLINE", Some("Sample data"), None)];
    mixed.extend(venues());
    assert_eq!(
        tickers(&filter(&mixed, "binance")),
        ["BTCUSDT", "ETHUSDT", "BTCUSD"]
    );
    assert_eq!(tickers(&filter(&mixed, "off")), ["OFFLINE"]);
}

#[test]
fn a_declared_listing_prefix_is_the_venue_label() {
    let listing = |ticker: &str, prefix: &str| SymbolDescriptor {
        kind: Some("stock".into()),
        prefix: Some(prefix.into()),
        provider: Some("edgx".into()),
        ..SymbolDescriptor::new(ticker)
    };
    let mut equities = vec![
        listing("AAPL", "NASDAQ"),
        listing("IBM", "NYSE"),
        listing("SPY", "AMEX"),
    ];
    equities.extend(venues());
    let label = |q: &str| -> Vec<String> {
        filter(&equities, q)
            .iter()
            .map(|s| {
                format!(
                    "{}:{}",
                    s.prefix.as_deref().or(s.provider.as_deref()).unwrap_or(""),
                    s.ticker
                )
            })
            .collect()
    };
    assert_eq!(label("nasdaq AAP"), ["NASDAQ:AAPL"]); // the listing venue scopes
    assert!(label("NYSE:AAPL").is_empty()); // wrong venue matches nothing
    assert_eq!(label("edgx:"), ["NASDAQ:AAPL", "NYSE:IBM", "AMEX:SPY"]); // provider id still browses its pool
    assert_eq!(label("nasdaq"), ["NASDAQ:AAPL"]); // venue-substring tier reads the label
}

#[test]
fn parse_query_handles_the_edge_shapes() {
    let v: Vec<String> = ["binance", "coinbase"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let term_only = |t: &str| ParsedQuery {
        scope: None,
        term: t.into(),
    };
    assert_eq!(parse_query("", &v), term_only(""));
    assert_eq!(parse_query("   ", &v), term_only(""));
    assert_eq!(parse_query("binance", &v), term_only("binance"));
    assert_eq!(parse_query(":abc", &v), term_only(":abc"));
    assert_eq!(
        parse_query("  binance  : btc  ", &v),
        ParsedQuery {
            scope: Some("binance".into()),
            term: "btc".into()
        }
    );
    assert_eq!(
        parse_query("BINANCE ", &v),
        ParsedQuery {
            scope: Some("binance".into()),
            term: String::new()
        }
    );
}

#[test]
fn locale_compare_orders_like_a_ticker_list_expects() {
    let mut list = vec!["btc", "BTC-USD", "ADA", "7UP", "ada", "BTC"];
    list.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(list, ["7UP", "ada", "ADA", "btc", "BTC", "BTC-USD"]);
    assert_eq!(locale_compare("A", "A"), Ordering::Equal);
    assert_eq!(locale_compare("A-B", "AB"), Ordering::Less);
}

// ── groups ──

fn cme() -> Vec<SymbolDescriptor> {
    let row = |ticker: &str,
               kind: &str,
               group: &str,
               description: Option<&str>,
               default: Option<bool>| SymbolDescriptor {
        kind: Some(kind.into()),
        group: Some(group.into()),
        prefix: Some("CME".into()),
        description: description.map(str::to_string),
        default,
        ..SymbolDescriptor::new(ticker)
    };
    vec![
        row("ES", "root", "ES", Some("E-mini S&P 500"), None),
        row("ES1!", "futures", "ES", None, Some(true)),
        row("ES2!", "futures", "ES", None, None),
        row("NQ", "root", "NQ", Some("E-mini Nasdaq-100"), None),
        row("NQ1!", "futures", "NQ", None, None), // deliberately NO default
        row("NQ2!", "futures", "NQ", None, None),
    ]
}

#[test]
fn folding_drops_members_standing_below_their_group_row() {
    assert_eq!(tickers(&fold_groups(&cme())), ["ES", "NQ"]);
}

#[test]
fn folding_keeps_a_member_that_matched_without_its_group_row() {
    let c = cme();
    assert_eq!(
        tickers(&fold_groups(&[c[2].clone(), c[4].clone()])),
        ["ES2!", "NQ1!"]
    );
}

#[test]
fn folding_is_order_respecting_a_member_above_its_group_row_keeps_its_own_row() {
    let c = cme();
    assert_eq!(
        tickers(&fold_groups(&[c[1].clone(), c[0].clone(), c[2].clone()])),
        ["ES1!", "ES"]
    );
}

#[test]
fn folding_is_per_venue() {
    let c = cme();
    let other = SymbolDescriptor {
        kind: Some("futures".into()),
        group: Some("ES".into()),
        prefix: Some("OTHER".into()),
        ..SymbolDescriptor::new("ES1!")
    };
    let folded = fold_groups(&[c[0].clone(), other]);
    let labels: Vec<String> = folded
        .iter()
        .map(|s| format!("{}:{}", s.prefix.as_deref().unwrap_or(""), s.ticker))
        .collect();
    assert_eq!(labels, ["CME:ES", "OTHER:ES1!"]);
}

#[test]
fn folding_leaves_ungrouped_rows_alone() {
    let plain = vec![
        SymbolDescriptor {
            kind: Some("crypto".into()),
            ..SymbolDescriptor::new("BTCUSDT")
        },
        SymbolDescriptor {
            kind: Some("stock".into()),
            prefix: Some("NASDAQ".into()),
            ..SymbolDescriptor::new("AAPL")
        },
    ];
    assert_eq!(fold_groups(&plain), plain);
}

#[test]
fn dedupe_keeps_the_first_row_per_venue_and_ticker() {
    let a = sym("BTCUSDT", Some("injected"), Some("Binance"));
    let b = sym("btcusdt", Some("later"), Some("binance"));
    let c = sym("BTCUSDT", None, Some("coinbase"));
    let out = dedupe_symbols(&[a.clone(), b, c.clone()]);
    assert_eq!(out, [a, c]);
}

#[test]
fn the_avatar_hue_is_deterministic_per_ticker_and_a_valid_hsl() {
    assert_eq!(avatar_color("BTCUSDT"), avatar_color("BTCUSDT"));
    let eth = avatar_color("ETHUSDT");
    assert!(eth.starts_with("hsl(") && eth.ends_with(", 42%, 38%)"));
    assert_ne!(avatar_color("BTCUSDT"), eth);
    assert_eq!(avatar_color(""), "hsl(0, 42%, 38%)");
}

#[test]
fn tabs_list_their_types_and_crypto_adds_perpetual_futures() {
    let of = |kind: &str| SymbolDescriptor {
        kind: Some(kind.into()),
        ..SymbolDescriptor::new("X")
    };
    assert!(tab_lists(SymbolTab::All, &SymbolDescriptor::new("X")));
    assert!(tab_lists(SymbolTab::Stocks, &of("Stock")));
    assert!(!tab_lists(SymbolTab::Stocks, &of("etf")));
    assert!(tab_lists(SymbolTab::Futures, &of("root")));
    assert!(tab_lists(SymbolTab::Crypto, &of("futures")));
    assert!(!tab_lists(SymbolTab::Forex, &of("futures")));
    assert!(!tab_lists(SymbolTab::Forex, &SymbolDescriptor::new("X")));
    for tab in SymbolTab::ALL {
        assert_eq!(SymbolTab::from_id(tab.id()), Some(tab));
    }
    assert_eq!(SymbolTab::from_id("nope"), None);
}

// ── the model ──

fn model_over(
    platform: &ManualPlatform,
    pool: Vec<SymbolDescriptor>,
    ranking: Option<SymbolRankingHook>,
) -> (SymbolPickerModel, Rc<Cell<u32>>) {
    let repaints = Rc::new(Cell::new(0));
    let count = Rc::clone(&repaints);
    let model = SymbolPickerModel::new(
        platform.handle(),
        Rc::new(move || ranking.clone()),
        move || count.set(count.get() + 1),
    );
    let shared = Rc::new(pool);
    model.set_source(Rc::new(move || Rc::clone(&shared)));
    (model, repaints)
}

fn model_rows(m: &SymbolPickerModel) -> Vec<String> {
    m.rows().iter().map(|s| s.ticker.clone()).collect()
}

#[test]
fn opening_seeds_the_query_in_upper_case_and_consumes_the_seed() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, base_list(), None);
    m.set_seed("btc");
    assert_eq!(m.on_open(), "BTC");
    assert_eq!(model_rows(&m), ["BTCUSDT", "WBTCUSDT"]);
    assert_eq!(m.on_open(), "", "the seed is consumed");
    assert_eq!(model_rows(&m).len(), 4);
}

#[test]
fn typing_at_an_open_picker_extends_the_query() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, base_list(), None);
    m.on_open();
    assert_eq!(m.append_typed("e"), "E");
    assert_eq!(m.append_typed("t"), "ET");
    assert_eq!(model_rows(&m)[0], "ETHUSDT");
}

#[test]
fn the_highlight_moves_clamped_and_resets_on_a_new_query() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, base_list(), None);
    m.on_open();
    m.move_highlight(-1);
    assert_eq!(m.highlighted(), 0);
    m.move_highlight(1);
    m.move_highlight(1);
    assert_eq!(m.highlighted(), 2);
    m.move_highlight(99);
    assert_eq!(m.highlighted(), 3);
    m.set_query("S");
    assert_eq!(m.highlighted(), 0);
    let empty = {
        let (e, _) = model_over(&platform, Vec::new(), None);
        e.move_highlight(1);
        e.highlighted()
    };
    assert_eq!(empty, 0);
}

#[test]
fn picking_commits_the_venue_pinned_symbol() {
    let platform = ManualPlatform::new(0.0);
    let pool = vec![
        sym("BTCUSDT", None, Some("binance")),
        SymbolDescriptor {
            prefix: Some("NASDAQ".into()),
            provider: Some("edgx".into()),
            ..SymbolDescriptor::new("AAPL")
        },
        sym("OFFLINE", None, None),
    ];
    let (m, _) = model_over(&platform, pool, None);
    m.on_open();
    let rows = m.rows();
    assert_eq!(m.pick(&rows[0]), "binance:BTCUSDT");
    assert_eq!(m.pick(&rows[1]), "NASDAQ:AAPL"); // the listing prefix wins over the provider id
    assert_eq!(m.pick(&rows[2]), "OFFLINE");
    assert_eq!(m.pick_highlighted().as_deref(), Some("binance:BTCUSDT"));
    assert_eq!(selection_symbol("X", Some("")), "X");
}

#[test]
fn picking_a_group_loads_its_default_member_and_never_the_dead_root() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, cme(), None);
    m.on_open();
    let rows = m.rows();
    assert_eq!(model_rows(&m), ["ES", "NQ"]); // the members stand under their groups
    assert_eq!(m.pick(&rows[0]), "CME:ES1!"); // the single default
    assert_eq!(m.pick(&rows[1]), "CME:NQ1!"); // no default: the first listed member
}

#[test]
fn expanding_a_group_splices_its_members_in_place_and_keeps_the_highlight_on_its_row() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, cme(), None);
    m.on_open();
    m.move_highlight(1); // on NQ
    let es = m.rows()[0].clone();
    assert!(!m.is_expanded(&es));
    m.toggle_expand(&es);
    assert!(m.is_expanded(&es));
    assert_eq!(model_rows(&m), ["ES", "ES1!", "ES2!", "NQ"]);
    assert_eq!(m.highlighted(), 3, "the highlight follows NQ");
    m.toggle_expand(&es);
    assert_eq!(model_rows(&m), ["ES", "NQ"]);
    assert_eq!(m.highlighted(), 1);
}

#[test]
fn the_type_tab_filters_the_one_result_list() {
    let platform = ManualPlatform::new(0.0);
    let pool = vec![
        SymbolDescriptor {
            kind: Some("stock".into()),
            ..SymbolDescriptor::new("AAPL")
        },
        SymbolDescriptor {
            kind: Some("crypto".into()),
            ..SymbolDescriptor::new("BTCUSDT")
        },
    ];
    let (m, _) = model_over(&platform, pool, None);
    m.on_open();
    assert_eq!(model_rows(&m).len(), 2);
    m.set_tab(SymbolTab::Stocks);
    assert_eq!(model_rows(&m), ["AAPL"]);
    assert_eq!(m.tab(), SymbolTab::Stocks);
}

#[test]
fn growing_appends_the_next_page_until_the_pool_is_drained() {
    let platform = ManualPlatform::new(0.0);
    let wide: Vec<SymbolDescriptor> = (0..250)
        .map(|i| sym(&format!("AA{i:03}"), None, None))
        .collect();
    let (m, _) = model_over(&platform, wide, None);
    m.on_open();
    assert_eq!(m.rows().len(), PAGE);
    assert!(!m.exhausted());
    let first = m.rows();
    let appended = m.grow();
    assert_eq!(appended.len(), 100);
    assert_eq!(
        m.rows()[..PAGE],
        first[..],
        "rows already on screen stay put"
    );
    assert_eq!(m.grow().len(), 50);
    assert!(m.exhausted());
    assert!(m.grow().is_empty());
    // A new query starts back at one page.
    m.set_query("AA");
    assert_eq!(m.rows().len(), PAGE);
}

#[test]
fn the_empty_states_read_after_whether_a_query_was_typed() {
    let platform = ManualPlatform::new(0.0);
    let (m, _) = model_over(&platform, Vec::new(), None);
    m.on_open();
    assert_eq!(m.empty_message(None), "No symbols available yet.");
    m.set_query("ZZ");
    assert_eq!(m.empty_message(None), "No symbols match.");
}

#[test]
fn a_registered_ranking_owns_the_head_and_repaints_when_it_lands() {
    let platform = ManualPlatform::new(0.0);
    let hook: SymbolRankingHook = Rc::new(|mut pool| {
        pool.reverse();
        RankedPool::Ready(pool)
    });
    let (m, repaints) = model_over(&platform, base_list(), Some(hook));
    // The first open still shows the raw pool: the ranking resolves onto the next repaint.
    m.on_open();
    assert_eq!(
        model_rows(&m),
        ["BTCUSDT", "ETHUSDT", "WBTCUSDT", "SOLUSDT"]
    );
    platform.run();
    assert_eq!(repaints.get(), 1);
    m.refresh();
    // The majors pin stands down: the ranked (reversed) order shows.
    assert_eq!(
        model_rows(&m),
        ["SOLUSDT", "WBTCUSDT", "ETHUSDT", "BTCUSDT"]
    );
}

#[test]
fn an_async_ranking_resolves_onto_a_later_repaint() {
    let platform = ManualPlatform::new(0.0);
    let hook: SymbolRankingHook = Rc::new(|pool| {
        RankedPool::Pending(Box::pin(async move {
            let mut p = pool;
            p.truncate(2);
            p
        }))
    });
    let (m, repaints) = model_over(&platform, base_list(), Some(hook));
    m.on_open();
    assert_eq!(
        m.rows().len(),
        4,
        "the raw pool shows until the future resolves"
    );
    platform.run();
    assert_eq!(repaints.get(), 1);
    m.refresh();
    assert_eq!(m.rows().len(), 2);
}
