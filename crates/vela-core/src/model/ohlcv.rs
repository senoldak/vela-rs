/// Canonical time in Vela: Unix epoch **milliseconds**. Each renderer converts
/// to its own unit at its boundary.
pub type Millis = i64;

/// A single price bar. `time` is the bar's open time in epoch ms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ohlcv {
    pub time: Millis,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// `None` where the provider reports no volume (a gap, never a fake zero).
    pub volume: Option<f64>,
}

impl Ohlcv {
    pub fn new(
        time: Millis,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: Option<f64>,
    ) -> Self {
        Self {
            time,
            open,
            high,
            low,
            close,
            volume,
        }
    }
}
