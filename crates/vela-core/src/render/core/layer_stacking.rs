//! Stacking of the SDK renderer-layer canvases against the data canvas. Model
//! series all composite inside the ONE data canvas (the scene sorts them by
//! z), so a layer canvas can only slot below or above that whole canvas. This
//! module decides which side and in what order, from the z key of the
//! indicator that OWNS the layer (the mounted native indicator whose type
//! equals the layer id: the id doubles as the data channel, so the pairing is
//! already the SDK's contract). Unowned layers (chart-type channels) keep
//! their declared placement. The renderer applies the result to the DOM pile
//! and the screenshot compositor (browser crate).

use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerPlacement {
    BelowData,
    /// The definition default.
    #[default]
    AboveData,
}

impl LayerPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerPlacement::BelowData => "below-data",
            LayerPlacement::AboveData => "above-data",
        }
    }
}

/// One mounted layer's stacking inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerStackEntry {
    /// The layer id (= its data channel).
    pub id: String,
    /// The definition's declared placement.
    pub placement: LayerPlacement,
    /// z key of the owning indicator, or `None` when no mounted indicator owns this layer.
    pub owner_z: Option<f64>,
}

/// Layer ids on each side of the data canvas, each back to front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackedLayers {
    pub below: Vec<String>,
    pub above: Vec<String>,
}

/// Split the mounted layers around the data canvas, each side back-to-front.
/// An owned layer sits by its owner's z against `candle_z` (below the data
/// canvas when `z < candle_z`, else above: matching how a model series at that
/// z would paint against the candles). Unowned 'below-data' layers stay at the
/// very back; unowned 'above-data' layers sit directly over the data canvas,
/// under any owned layer raised above it. Ties keep registration order (the
/// sort is stable). "The very back" is still above the grid: the backdrop
/// canvas (highlights + gridlines) sits below every layer this orders.
pub fn stack_layers(entries: &[LayerStackEntry], candle_z: f64) -> StackedLayers {
    let mut keyed: Vec<(&str, f64)> = entries
        .iter()
        .map(|e| {
            let key = e.owner_z.unwrap_or(match e.placement {
                LayerPlacement::BelowData => f64::NEG_INFINITY,
                LayerPlacement::AboveData => candle_z,
            });
            (e.id.as_str(), key)
        })
        .collect();
    // A NaN difference (two -infinity keys) compares equal, like the original comparator.
    keyed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    let mut out = StackedLayers::default();
    for (id, key) in keyed {
        if key < candle_z {
            out.below.push(id.to_string());
        } else {
            out.above.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, placement: LayerPlacement, owner_z: Option<f64>) -> LayerStackEntry {
        LayerStackEntry {
            id: id.into(),
            placement,
            owner_z,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    use LayerPlacement::{AboveData, BelowData};

    #[test]
    fn unowned_layers_keep_their_declared_placement() {
        let out = stack_layers(
            &[
                entry("reveal", BelowData, None),
                entry("grid", AboveData, None),
            ],
            0.0,
        );
        assert_eq!(out.below, ids(&["reveal"]));
        assert_eq!(out.above, ids(&["grid"]));
    }

    #[test]
    fn an_owned_layer_slots_by_its_owner_z_against_the_candles() {
        let out = stack_layers(
            &[
                entry("bubbles", AboveData, Some(6.0)),
                entry("tpo", AboveData, Some(-2.0)),
            ],
            5.0,
        );
        assert_eq!(out.below, ids(&["tpo"]));
        assert_eq!(out.above, ids(&["bubbles"]));
    }

    #[test]
    fn owner_z_exactly_at_candle_z_paints_in_front() {
        let out = stack_layers(&[entry("a", AboveData, Some(5.0))], 5.0);
        assert_eq!(out.above, ids(&["a"]));
    }

    #[test]
    fn orders_each_side_by_z_with_unowned_above_data_layers_directly_over_the_data_canvas() {
        let out = stack_layers(
            &[
                entry("owned-high", AboveData, Some(9.0)),
                entry("style-channel", AboveData, None), // key = candle_z 0
                entry("owned-low", AboveData, Some(3.0)),
            ],
            0.0,
        );
        assert_eq!(
            out.above,
            ids(&["style-channel", "owned-low", "owned-high"])
        );
    }

    #[test]
    fn unowned_below_data_layers_stay_at_the_very_back() {
        let out = stack_layers(
            &[
                entry("owned-back", AboveData, Some(-3.0)),
                entry("reveal", BelowData, None),
            ],
            0.0,
        );
        assert_eq!(out.below, ids(&["reveal", "owned-back"]));
    }

    #[test]
    fn ties_keep_registration_order() {
        let out = stack_layers(
            &[
                entry("first", AboveData, Some(2.0)),
                entry("second", AboveData, Some(2.0)),
            ],
            0.0,
        );
        assert_eq!(out.above, ids(&["first", "second"]));
    }

    #[test]
    fn several_unowned_below_data_layers_keep_registration_order() {
        let out = stack_layers(
            &[
                entry("b", BelowData, None),
                entry("a", BelowData, None),
                entry("c", BelowData, None),
            ],
            0.0,
        );
        assert_eq!(out.below, ids(&["b", "a", "c"]));
    }

    #[test]
    fn no_layers_stack_to_nothing() {
        assert_eq!(stack_layers(&[], 0.0), StackedLayers::default());
    }
}
