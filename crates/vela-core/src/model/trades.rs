use super::ohlcv::Millis;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TradeKind {
    Entry,
    Exit,
}

/// One order execution of a strategy indicator: the unit painted on the chart
/// as a trade marker. Executions anchor to their FILL bar and always render on
/// the PRICE pane, whatever pane the indicator's plots landed on.
#[derive(Clone, Debug, PartialEq)]
pub struct TradeExecution {
    /// Fill bar (bar open time, epoch ms).
    pub time: Millis,
    /// Exact fill price.
    pub price: f64,
    /// A buy paints an up arrow below the bar; a sell a down arrow above it.
    pub side: TradeSide,
    pub kind: TradeKind,
    /// Text line next to the arrow: the order id, or its comment.
    pub label: Option<String>,
    /// Filled quantity (magnitude); painted signed.
    pub qty: Option<f64>,
    /// Shared by the executions of one round-trip.
    pub trade_id: Option<String>,
}
