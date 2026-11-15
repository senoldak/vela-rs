//! Every built-in drawing tool, grouped by family. Each family module ports
//! the matching files of the original `core/drawings/types/` and exposes a
//! `metas()` function with its registry entries.

pub mod annotations;
pub mod channels;
pub mod fib;
pub mod gann;
pub mod lines;
pub mod mach;
pub mod measure;
pub mod patterns;
pub mod price_action;
pub mod segment;
pub mod shapes;
pub mod stats;

use super::registry::DrawingTypeMeta;

/// The original's registration order: the order a toolbar lists the tools in.
pub const REGISTRATION_ORDER: [&str; 79] = [
    "trendline",
    "hline",
    "ray",
    "extendedline",
    "vline",
    "hray",
    "crossline",
    "infoline",
    "trendangle",
    "box",
    "text",
    "callout",
    "note",
    "pricenote",
    "comment",
    "pricelabel",
    "signpost",
    "parallelchannel",
    "disjointchannel",
    "flattopbottom",
    "regressionchannel",
    "pitchfork",
    "schiffpitchfork",
    "modifiedschiffpitchfork",
    "insidepitchfork",
    "arrow",
    "ellipse",
    "triangle",
    "polyline",
    "freehand",
    "highlighter",
    "circle",
    "rotatedrect",
    "path",
    "arc",
    "curve",
    "arrowmarkup",
    "arrowmarkdown",
    "flagmark",
    "iconstamp",
    "fibretracement",
    "fibextension",
    "fibextensiontrend",
    "fibfan",
    "fibtimezones",
    "fibchannel",
    "fibspeedfan",
    "trendfibtime",
    "fibcircles",
    "fibarcs",
    "fibwedge",
    "fibspiral",
    "gannfan",
    "gannbox",
    "gannsquare",
    "dedekind",
    "sonic",
    "supersonic",
    "goldensonic",
    "goldensupersonic",
    "xabcd",
    "abcd",
    "elliottimpulse",
    "elliottcorrection",
    "headshoulders",
    "gartley",
    "bat",
    "butterfly",
    "crab",
    "shark",
    "cypher",
    "datepricerange",
    "position",
    "magnifier",
    "anchoredvwap",
    "fixedrangevp",
    "fairvaluegap",
    "orderblock",
    "liquidity",
];

/// The registry entries of every built-in tool, in registration order. A type
/// missing from [`REGISTRATION_ORDER`] keeps its family's position at the end.
pub fn builtin_metas() -> Vec<DrawingTypeMeta> {
    let mut metas: Vec<DrawingTypeMeta> = [
        lines::metas(),
        shapes::metas(),
        annotations::metas(),
        channels::metas(),
        fib::metas(),
        gann::metas(),
        mach::metas(),
        patterns::metas(),
        price_action::metas(),
        measure::metas(),
        stats::metas(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let rank = |meta: &DrawingTypeMeta| {
        REGISTRATION_ORDER
            .iter()
            .position(|key| *key == meta.type_key)
            .unwrap_or(REGISTRATION_ORDER.len())
    };
    metas.sort_by_key(rank);
    metas
}
