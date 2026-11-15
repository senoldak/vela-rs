//! A strategy's broker state and trade ledger, neutral across scripting
//! languages: an engine translates its own vocabulary into these names, so host
//! code reads the same fields whatever language the strategy was written in.

use serde::{Deserialize, Serialize};

use super::ohlcv::Millis;

/// A strategy's broker state at ONE bar: the flat summary a host reads while a
/// script runs. Distinct from `TradeExecution`, which is a single order FILL the
/// renderer paints: this is the account, not the drawing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyState {
    /// Signed contracts held (positive = long, negative = short, 0 = flat).
    pub position: f64,
    /// Average entry price of the open position (0 when flat).
    pub avg_price: f64,
    /// Account value: capital + realized + unrealized.
    pub equity: f64,
    /// Unrealized P&L of the open position.
    pub open_pnl: f64,
    /// Realized P&L, gross profit minus gross loss.
    pub net_pnl: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    /// Closed-trade outcome counts.
    pub wins: u32,
    pub losses: u32,
    pub even: u32,
    /// Largest peak-to-trough equity drop / trough-to-peak rise, in account currency.
    pub max_drawdown: f64,
    pub max_runup: f64,
    pub initial_capital: f64,
    /// Compound annual growth of the account over the backtest, in percent. Absent
    /// when the engine does not compute it, or cannot (a span too short to
    /// annualize, an account wiped out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cagr: Option<f64>,
    /// Risk-adjusted performance of the equity curve: excess return over its
    /// volatility (`sharpe`) and over its downside volatility only (`sortino`). An
    /// engine defines the sampling and the risk-free rate. Absent when the engine
    /// does not compute them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpe: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sortino: Option<f64>,
    /// The buy-and-hold benchmark: buying the instrument with the whole initial
    /// capital and holding it to the last bar, its P&L in account currency
    /// (`buy_hold_pnl`) and its gain in percent (`buy_hold_pct`), and the
    /// strategy's net profit beyond it (`outperformance`, `net_pnl - buy_hold_pnl`).
    /// An engine defines the entry it anchors on. Absent when not computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buy_hold_pnl: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buy_hold_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outperformance: Option<f64>,
}

/// The direction of a [`StrategyTrade`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategySide {
    Long,
    Short,
}

impl StrategySide {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategySide::Long => "long",
            StrategySide::Short => "short",
        }
    }
}

/// One ROUND TRIP of a strategy: an entry, and the exit that closed it when there
/// is one. Coarser than `TradeExecution` (a fill) on purpose: this is what a host
/// tabulates, exports, or reconciles against a broker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyTrade {
    /// Stable within a run; shared with the fills' `trade_id`.
    pub id: String,
    pub side: StrategySide,
    /// Contracts (magnitude; `side` carries the direction).
    pub qty: f64,
    pub entry: StrategyFill,
    /// Absent while the trade is still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<StrategyFill>,
    /// Still open at the last computed bar.
    pub open: bool,
    /// Realized P&L of this round trip in account currency, net of commission.
    /// Absent while the trade is open, and for an engine that reports no per-trade
    /// ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pnl: Option<f64>,
    /// Commission charged on this trade, in account currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commission: Option<f64>,
    /// Worst / best unrealized excursion of THIS trade from its entry, in account
    /// currency (positive magnitudes): `max_drawdown` is the adverse excursion and
    /// `max_runup` the favorable one, the per-trade counterparts of the
    /// account-level [`StrategyState::max_drawdown`] / [`StrategyState::max_runup`].
    /// An engine defines the exact convention; absent when it does not track them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drawdown: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runup: Option<f64>,
}

/// One side of a [`StrategyTrade`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategyFill {
    /// The order id the script used (Pine's `strategy.entry("Long", ...)`).
    pub id: String,
    pub time: Millis,
    pub price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fill(id: &str, time: Millis, price: f64) -> StrategyFill {
        StrategyFill {
            id: id.into(),
            time,
            price,
            comment: None,
        }
    }

    #[test]
    fn open_trade_serializes_without_exit_and_pnl() {
        let t = StrategyTrade {
            id: "1".into(),
            side: StrategySide::Long,
            qty: 2.0,
            entry: fill("Long", 1_000, 10.5),
            exit: None,
            open: true,
            pnl: None,
            commission: None,
            max_drawdown: None,
            max_runup: None,
        };
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            json!({
                "id": "1", "side": "long", "qty": 2.0, "open": true,
                "entry": { "id": "Long", "time": 1000, "price": 10.5 }
            })
        );
    }

    #[test]
    fn closed_trade_round_trips_with_camel_case_fields() {
        let raw = json!({
            "id": "7", "side": "short", "qty": 1.0, "open": false,
            "entry": { "id": "S", "time": 1, "price": 5.0, "comment": "go" },
            "exit": { "id": "S", "time": 2, "price": 4.0 },
            "pnl": 1.0, "commission": 0.1, "maxDrawdown": 0.5, "maxRunup": 1.5
        });
        let t: StrategyTrade = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(t.side.as_str(), "short");
        assert_eq!(t.max_runup, Some(1.5));
        assert_eq!(serde_json::to_value(&t).unwrap(), raw);
    }

    #[test]
    fn state_optionals_are_absent_unless_computed() {
        let s = StrategyState {
            equity: 1_000.0,
            initial_capital: 1_000.0,
            ..StrategyState::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["avgPrice"], json!(0.0));
        assert_eq!(v["initialCapital"], json!(1_000.0));
        for k in [
            "cagr",
            "sharpe",
            "sortino",
            "buyHoldPnl",
            "buyHoldPct",
            "outperformance",
        ] {
            assert!(v.get(k).is_none(), "{k}");
        }
        let back: StrategyState = serde_json::from_value(json!({
            "position": -1, "avgPrice": 2, "equity": 3, "openPnl": 0, "netPnl": 0,
            "grossProfit": 0, "grossLoss": 0, "wins": 1, "losses": 2, "even": 0,
            "maxDrawdown": 0, "maxRunup": 0, "initialCapital": 3, "cagr": 12.5
        }))
        .unwrap();
        assert_eq!(back.cagr, Some(12.5));
        assert_eq!(back.position, -1.0);
        assert_eq!(back.losses, 2);
    }

    #[test]
    fn unknown_side_is_rejected() {
        assert!(serde_json::from_value::<StrategySide>(json!("flat")).is_err());
    }
}
