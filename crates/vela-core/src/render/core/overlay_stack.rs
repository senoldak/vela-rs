//! The price pane's draw stack as the legend restacks it, one step forward or
//! back: the pane's indicators ordered as they paint, with the candles as one
//! more entry at `candle_z`. The renderer feeds it the scene's keys and
//! applies the z it returns.

/// One layer of the stack: an indicator by id, or the candles (`id: None`).
#[derive(Clone, Debug, PartialEq)]
pub struct StackEntry {
    pub id: Option<String>,
    pub z: f64,
}

impl StackEntry {
    pub fn indicator(id: impl Into<String>, z: f64) -> Self {
        Self {
            id: Some(id.into()),
            z,
        }
    }

    pub fn candles(z: f64) -> Self {
        Self { id: None, z }
    }
}

/// Order the price pane's layers back to front. `indicators` comes in the
/// scene's own paint order (z, then mount order); the candles paint before an
/// indicator sharing their key, as the backends draw them.
pub fn price_stack(indicators: &[StackEntry], candle_z: f64) -> Vec<StackEntry> {
    let mut out = indicators.to_vec();
    let at = out
        .iter()
        .position(|e| e.z >= candle_z)
        .unwrap_or(out.len());
    out.insert(at, StackEntry::candles(candle_z));
    out
}

/// A new key for one indicator (see [`untie`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ZFix {
    pub id: String,
    pub z: f64,
}

/// New keys that make every indicator's key distinct while keeping the paint
/// order: the entries of a run sharing one key spread out between it and the
/// next key up (or one past it at the top). The first entry of a run keeps its
/// key, so the candles, which paint first on a shared key, never move. Empty
/// when no two layers share a key.
pub fn untie(stack: &[StackEntry]) -> Vec<ZFix> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < stack.len() {
        let mut j = i + 1;
        while j < stack.len() && stack[j].z == stack[i].z {
            j += 1;
        }
        let run = j - i;
        if run > 1 {
            let base = stack[i].z;
            let next = stack.get(j).map_or(base + 1.0, |e| e.z);
            let step = (next - base) / run as f64;
            for k in 1..run {
                if let Some(id) = &stack[i + k].id {
                    out.push(ZFix {
                        id: id.clone(),
                        z: base + k as f64 * step,
                    });
                }
            }
        }
        i = j;
    }
    out
}

/// What paints next to a layer: the candles, another indicator, or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Neighbor {
    Price,
    Indicator(String),
}

fn index_of(stack: &[StackEntry], id: &str) -> Option<usize> {
    stack.iter().position(|e| e.id.as_deref() == Some(id))
}

fn neighbor(entry: &StackEntry) -> Neighbor {
    match &entry.id {
        None => Neighbor::Price,
        Some(id) => Neighbor::Indicator(id.clone()),
    }
}

/// What paints directly in front of `id`: the candles, another indicator, or nothing.
pub fn layer_above(stack: &[StackEntry], id: &str) -> Option<Neighbor> {
    let i = index_of(stack, id)?;
    stack.get(i + 1).map(neighbor)
}

/// What paints directly behind `id`: the candles, another indicator, or nothing.
pub fn layer_below(stack: &[StackEntry], id: &str) -> Option<Neighbor> {
    let i = index_of(stack, id)?;
    i.checked_sub(1).map(|b| neighbor(&stack[b]))
}

/// The z that puts `id` directly in front of the layer above it, behind
/// whatever comes next: halfway to the next key, or one past the layer above
/// at the top of the stack. `None` when nothing is above. Every other layer
/// keeps its key. Expects distinct keys (see [`untie`]).
pub fn raised_z(stack: &[StackEntry], id: &str) -> Option<f64> {
    let i = index_of(stack, id)?;
    let above = stack.get(i + 1)?;
    Some(match stack.get(i + 2) {
        Some(next) => (above.z + next.z) / 2.0,
        None => above.z + 1.0,
    })
}

/// The z that puts `id` directly behind the layer below it, in front of
/// whatever comes before: halfway to the previous key, or one under the layer
/// below at the bottom of the stack. `None` when nothing is below. Every other
/// layer keeps its key. Expects distinct keys (see [`untie`]).
pub fn lowered_z(stack: &[StackEntry], id: &str) -> Option<f64> {
    let i = index_of(stack, id)?;
    let below = stack.get(i.checked_sub(1)?)?;
    Some(match i.checked_sub(2).map(|p| &stack[p]) {
        Some(prev) => (prev.z + below.z) / 2.0,
        None => below.z - 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Order = Vec<Option<String>>;

    fn order(ids: &[Option<&str>]) -> Order {
        ids.iter().map(|i| i.map(str::to_string)).collect()
    }

    fn ids_of(stack: &[StackEntry]) -> Order {
        stack.iter().map(|e| e.id.clone()).collect()
    }

    fn e(id: &str, z: f64) -> StackEntry {
        StackEntry::indicator(id, z)
    }

    /// The default mount order: each new indicator goes to the bottom, under the candles at 0.
    fn default_stack() -> Vec<StackEntry> {
        vec![e("gaps", -3.0), e("ema", -2.0), e("sma", -1.0)]
    }

    /// Apply a step's key and read the new back-to-front order.
    fn apply(indicators: &[StackEntry], id: &str, z: Option<f64>) -> Order {
        let z = z.expect("a step key");
        let mut next: Vec<StackEntry> = indicators
            .iter()
            .map(|x| {
                if x.id.as_deref() == Some(id) {
                    StackEntry { z, ..x.clone() }
                } else {
                    x.clone()
                }
            })
            .collect();
        next.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap());
        ids_of(&price_stack(&next, 0.0))
    }

    #[test]
    fn price_stack_slots_the_candles_at_their_key_before_an_indicator_sharing_it() {
        assert_eq!(
            ids_of(&price_stack(&default_stack(), 0.0)),
            order(&[Some("gaps"), Some("ema"), Some("sma"), None])
        );
        assert_eq!(
            ids_of(&price_stack(&[e("a", -1.0), e("b", 0.0), e("c", 2.0)], 0.0)),
            order(&[Some("a"), None, Some("b"), Some("c")])
        );
        assert_eq!(price_stack(&[], 0.0), vec![StackEntry::candles(0.0)]);
    }

    #[test]
    fn untie_is_empty_when_every_key_is_distinct() {
        assert!(untie(&price_stack(&default_stack(), 0.0)).is_empty());
    }

    #[test]
    fn untie_spreads_a_shared_key_up_to_the_next_one() {
        let stack = price_stack(&[e("volume", -1.0), e("bb", -1.0), e("ce", -0.5)], 0.0);
        assert_eq!(
            untie(&stack),
            vec![ZFix {
                id: "bb".into(),
                z: -0.75
            }]
        );
    }

    #[test]
    fn untie_never_moves_the_candles_which_lead_a_run_on_their_key() {
        let stack = price_stack(&[e("a", 0.0), e("b", 0.0)], 0.0);
        assert_eq!(ids_of(&stack), order(&[None, Some("a"), Some("b")]));
        assert_eq!(
            untie(&stack),
            vec![
                ZFix {
                    id: "a".into(),
                    z: 1.0 / 3.0
                },
                ZFix {
                    id: "b".into(),
                    z: 2.0 / 3.0
                },
            ]
        );
    }

    #[test]
    fn a_run_at_the_top_spreads_up_to_one_past_its_key() {
        let stack = price_stack(&[e("a", 5.0), e("b", 5.0)], 0.0);
        assert_eq!(
            untie(&stack),
            vec![ZFix {
                id: "b".into(),
                z: 5.5
            }]
        );
    }

    /// Untie, then apply one step, the way the renderer does.
    fn step_untied(indicators: &[StackEntry], id: &str, raise: bool) -> Order {
        let fixes = untie(&price_stack(indicators, 0.0));
        let untied: Vec<StackEntry> = indicators
            .iter()
            .map(|x| {
                let z = fixes
                    .iter()
                    .find(|f| Some(&f.id) == x.id.as_ref())
                    .map_or(x.z, |f| f.z);
                StackEntry { z, ..x.clone() }
            })
            .collect();
        let stack = price_stack(&untied, 0.0);
        let z = if raise {
            raised_z(&stack, id)
        } else {
            lowered_z(&stack, id)
        };
        apply(&untied, id, z)
    }

    fn shared() -> Vec<StackEntry> {
        // Volume and BB share -1 (Volume painting first), CE sits in front of both.
        vec![e("volume", -1.0), e("bb", -1.0), e("ce", -0.5)]
    }

    #[test]
    fn a_step_over_a_shared_key_sends_an_indicator_behind_exactly_the_one_behind_it() {
        assert_eq!(
            layer_below(&price_stack(&shared(), 0.0), "ce"),
            Some(Neighbor::Indicator("bb".into()))
        );
        assert_eq!(
            step_untied(&shared(), "ce", false),
            order(&[Some("volume"), Some("ce"), Some("bb"), None])
        );
    }

    #[test]
    fn a_step_over_a_shared_key_brings_the_back_one_in_front_of_exactly_the_other() {
        assert_eq!(
            step_untied(&shared(), "volume", true),
            order(&[Some("bb"), Some("volume"), Some("ce"), None])
        );
    }

    fn with_front() -> Vec<StackEntry> {
        let mut v = default_stack();
        v.push(e("front", 1.0));
        v
    }

    #[test]
    fn layer_above_names_the_indicator_directly_in_front() {
        let stack = price_stack(&with_front(), 0.0);
        assert_eq!(
            layer_above(&stack, "gaps"),
            Some(Neighbor::Indicator("ema".into()))
        );
        assert_eq!(
            layer_above(&stack, "ema"),
            Some(Neighbor::Indicator("sma".into()))
        );
    }

    #[test]
    fn layer_above_names_the_price_for_the_top_indicator_behind_the_candles() {
        assert_eq!(
            layer_above(&price_stack(&with_front(), 0.0), "sma"),
            Some(Neighbor::Price)
        );
    }

    #[test]
    fn layer_above_has_nothing_above_the_top_nor_for_an_indicator_outside_the_stack() {
        let stack = price_stack(&with_front(), 0.0);
        assert_eq!(layer_above(&stack, "front"), None);
        assert_eq!(layer_above(&stack, "volume"), None);
    }

    #[test]
    fn layer_below_names_the_price_for_the_bottom_indicator_in_front_of_the_candles() {
        assert_eq!(
            layer_below(&price_stack(&with_front(), 0.0), "front"),
            Some(Neighbor::Price)
        );
    }

    #[test]
    fn layer_below_names_the_indicator_directly_behind() {
        let stack = price_stack(&with_front(), 0.0);
        assert_eq!(
            layer_below(&stack, "sma"),
            Some(Neighbor::Indicator("ema".into()))
        );
        assert_eq!(
            layer_below(&stack, "ema"),
            Some(Neighbor::Indicator("gaps".into()))
        );
    }

    #[test]
    fn layer_below_has_nothing_behind_the_bottom_nor_for_an_indicator_outside_the_stack() {
        let stack = price_stack(&with_front(), 0.0);
        assert_eq!(layer_below(&stack, "gaps"), None);
        assert_eq!(layer_below(&stack, "volume"), None);
    }

    fn lower(indicators: &[StackEntry], id: &str) -> Order {
        apply(indicators, id, lowered_z(&price_stack(indicators, 0.0), id))
    }

    #[test]
    fn lowered_z_sends_an_indicator_behind_the_one_directly_behind_it_and_no_further() {
        assert_eq!(
            lower(&default_stack(), "sma"),
            order(&[Some("gaps"), Some("sma"), Some("ema"), None])
        );
    }

    #[test]
    fn lowered_z_sends_an_indicator_in_front_of_the_candles_back_behind_the_price() {
        assert_eq!(
            lower(&with_front(), "front"),
            order(&[Some("gaps"), Some("ema"), Some("sma"), Some("front"), None])
        );
    }

    #[test]
    fn lowered_z_sends_the_bottom_indicator_in_front_of_the_price_behind_it_at_the_bottom() {
        assert_eq!(
            lower(&[e("a", 1.0), e("b", 2.0)], "a"),
            order(&[Some("a"), None, Some("b")])
        );
    }

    #[test]
    fn lowered_z_is_the_inverse_of_a_step_forward() {
        let stack = price_stack(&default_stack(), 0.0);
        let up = apply(&default_stack(), "gaps", raised_z(&stack, "gaps"));
        assert_eq!(up, order(&[Some("ema"), Some("gaps"), Some("sma"), None]));
        let mut moved: Vec<StackEntry> = default_stack()
            .into_iter()
            .map(|x| {
                if x.id.as_deref() == Some("gaps") {
                    StackEntry {
                        z: raised_z(&stack, "gaps").unwrap(),
                        ..x
                    }
                } else {
                    x
                }
            })
            .collect();
        moved.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap());
        assert_eq!(
            apply(&moved, "gaps", lowered_z(&price_stack(&moved, 0.0), "gaps")),
            order(&[Some("gaps"), Some("ema"), Some("sma"), None])
        );
    }

    #[test]
    fn lowered_z_is_none_with_nothing_below() {
        assert_eq!(lowered_z(&price_stack(&default_stack(), 0.0), "gaps"), None);
        assert_eq!(
            lowered_z(&price_stack(&default_stack(), 0.0), "missing"),
            None
        );
    }

    fn step(indicators: &[StackEntry], id: &str) -> Order {
        apply(indicators, id, raised_z(&price_stack(indicators, 0.0), id))
    }

    #[test]
    fn raised_z_steps_an_indicator_in_front_of_the_one_above_it_and_no_further() {
        assert_eq!(
            step(&default_stack(), "gaps"),
            order(&[Some("ema"), Some("gaps"), Some("sma"), None])
        );
    }

    #[test]
    fn raised_z_brings_the_top_indicator_in_front_of_the_price() {
        assert_eq!(
            step(&default_stack(), "sma"),
            order(&[Some("gaps"), Some("ema"), None, Some("sma")])
        );
    }

    #[test]
    fn raised_z_lands_between_the_price_and_an_indicator_already_in_front_of_it() {
        assert_eq!(
            step(&with_front(), "sma"),
            order(&[Some("gaps"), Some("ema"), None, Some("sma"), Some("front")])
        );
    }

    #[test]
    fn raised_z_steps_in_front_of_the_top_indicator_past_the_candles() {
        assert_eq!(
            step(&[e("a", 1.0), e("b", 2.0)], "a"),
            order(&[None, Some("b"), Some("a")])
        );
    }

    #[test]
    fn raised_z_leaves_every_other_key_as_it_was() {
        let stack = price_stack(&default_stack(), 0.0);
        let z = raised_z(&stack, "gaps").unwrap();
        assert!(z > -2.0 && z < -1.0);
    }

    #[test]
    fn raised_z_is_none_with_nothing_above() {
        assert_eq!(
            raised_z(&price_stack(&default_stack(), 0.0), "missing"),
            None
        );
        assert_eq!(raised_z(&price_stack(&[e("top", 1.0)], 0.0), "top"), None);
    }
}
