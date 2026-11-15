//! The drawing toolbar definition: inert, renderer-neutral data the core builds
//! from the type registry and a renderer paints as a vertical bar. Each
//! [`ToolGroup`] is one button with a flyout listing its [`ToolDefinition`]s;
//! one tool is armed at a time across all groups.
//!
//! The definition serializes to the original's JSON field for field (the
//! `ToolbarDefinition` the renderer port takes as plain data). Labels are read
//! through a translator when the definition is resolved; a chart keeps the
//! [`ToolbarSource`] (labels still messages) so it can hand the toolbar out
//! again in a new language ([`resolve_toolbar`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::i18n::{MessageLike, Translator, msg, text_of};

use super::registry::DrawingRegistry;

/// One armable tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_key: String,
    /// The tool's name, in the language the definition was built for.
    pub label: String,
    /// Inline SVG markup (no DOM).
    pub icon: String,
}

/// A labelled subsection inside a toolbar group's flyout (e.g. "Fibonacci"
/// within Fibonacci & Gann).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSection {
    pub label: String,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolGroup {
    pub id: String,
    pub label: String,
    pub tools: Vec<ToolDefinition>,
    /// When set, the flyout renders non-clickable headers between sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ToolSection>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarDefinition {
    pub groups: Vec<ToolGroup>,
}

/// A tool whose label is still a message.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSource {
    pub type_key: String,
    pub label: MessageLike,
    pub icon: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSectionSource {
    pub label: MessageLike,
    pub tools: Vec<ToolSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolGroupSource {
    pub id: String,
    pub label: MessageLike,
    pub tools: Vec<ToolSource>,
    pub sections: Option<Vec<ToolSectionSource>>,
}

/// A toolbar whose labels are still messages: what the chart keeps, and reads in
/// its language whenever it hands the toolbar out ([`resolve_toolbar`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolbarSource {
    pub groups: Vec<ToolGroupSource>,
}

/// A developer-supplied explicit group (just type keys; the registry fills the rest).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolbarGroupConfig {
    pub id: String,
    /// The group's name; a message (`msg()`) reads in the chart's language.
    pub label: MessageLike,
    pub tools: Vec<String>,
}

/// The object form of [`DrawingsOption`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingsConfig {
    /// Whether the toolbar is visible (default true).
    pub toolbar: Option<bool>,
    pub tools: Option<Vec<String>>,
    pub groups: Option<Vec<ToolbarGroupConfig>>,
}

/// Public `options.drawings` shape. `true` = default toolbar; an object
/// customizes it. An absent option is `None` where one is taken
/// (`Option<DrawingsOption>`).
#[derive(Clone, Debug, PartialEq)]
pub enum DrawingsOption {
    Bool(bool),
    Config(DrawingsConfig),
}

impl From<bool> for DrawingsOption {
    fn from(on: bool) -> Self {
        Self::Bool(on)
    }
}

impl From<DrawingsConfig> for DrawingsOption {
    fn from(config: DrawingsConfig) -> Self {
        Self::Config(config)
    }
}

const FIBONACCI_TYPES: &[&str] = &[
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
];

const PRICE_ACTION_TYPES: &[&str] = &["fairvaluegap", "orderblock", "liquidity"];

const GANN_TYPES: &[&str] = &["gannfan", "gannbox", "gannsquare"];

const GEOMETRY_TYPES: &[&str] = &[
    "dedekind",
    "sonic",
    "supersonic",
    "goldensonic",
    "goldensupersonic",
];

const PATTERN_TYPES: &[&str] = &["xabcd", "abcd", "headshoulders"];
const ELLIOTT_TYPES: &[&str] = &["elliottimpulse", "elliottcorrection"];
const HARMONIC_TYPES: &[&str] = &["gartley", "bat", "butterfly", "crab", "shark", "cypher"];

const MEASUREMENT_TYPES: &[&str] = &["position", "datepricerange", "magnifier"];
const VOLUME_TYPES: &[&str] = &["anchoredvwap", "fixedrangevp"];

const BRUSH_TYPES: &[&str] = &["freehand", "highlighter"];
const ARROW_TYPES: &[&str] = &["arrow", "arrowmarkup", "arrowmarkdown"];
const SHAPE_TYPES: &[&str] = &[
    "box",
    "ellipse",
    "triangle",
    "polyline",
    "circle",
    "rotatedrect",
    "path",
    "arc",
    "curve",
];

const TEXT_TYPES: &[&str] = &[
    "text",
    "callout",
    "note",
    "pricenote",
    "comment",
    "pricelabel",
    "signpost",
];
const ICON_TYPES: &[&str] = &["flagmark", "iconstamp"];

/// Where a layout section takes its tools from.
enum SectionTools {
    /// Every registered tool of one registry group, in registration order.
    RegistryGroup(&'static str),
    /// These types, in this order (unregistered ones dropped).
    Types(&'static [&'static str]),
}

struct SectionLayout {
    label: MessageLike,
    tools: SectionTools,
}

struct ToolbarGroupLayout {
    id: &'static str,
    label: MessageLike,
    sections: Vec<SectionLayout>,
}

fn section(label: MessageLike, tools: SectionTools) -> SectionLayout {
    SectionLayout { label, tools }
}

/// Canonical toolbar layout: seven top-level buttons, each with labelled flyout
/// sections.
fn toolbar_layout() -> Vec<ToolbarGroupLayout> {
    use SectionTools::{RegistryGroup, Types};
    vec![
        ToolbarGroupLayout {
            id: "lines-channels-pitchforks",
            label: msg("drawing.toolbar.group.linesChannelsPitchforks", "Lines"),
            sections: vec![
                section(
                    msg("drawing.toolbar.section.lines", "Lines"),
                    RegistryGroup("lines"),
                ),
                section(
                    msg("drawing.toolbar.section.channels", "Channels"),
                    RegistryGroup("channels"),
                ),
                section(
                    msg("drawing.toolbar.section.pitchforks", "Pitchforks"),
                    RegistryGroup("pitchforks"),
                ),
            ],
        },
        ToolbarGroupLayout {
            id: "fibonacci-gann",
            label: msg("drawing.toolbar.group.fibonacciGann", "Fibonacci"),
            sections: vec![
                section(
                    msg("drawing.toolbar.section.fibonacci", "Fibonacci"),
                    Types(FIBONACCI_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.priceAction", "Price Action"),
                    Types(PRICE_ACTION_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.gann", "Gann"),
                    Types(GANN_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.geometry", "Geometry"),
                    Types(GEOMETRY_TYPES),
                ),
            ],
        },
        ToolbarGroupLayout {
            id: "patterns-waves-harmonics",
            label: msg("drawing.toolbar.group.patternsWavesHarmonics", "Patterns"),
            sections: vec![
                section(
                    msg("drawing.toolbar.section.patterns", "Patterns"),
                    Types(PATTERN_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.elliottWaves", "Elliott Waves"),
                    Types(ELLIOTT_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.harmonics", "Harmonics"),
                    Types(HARMONIC_TYPES),
                ),
            ],
        },
        ToolbarGroupLayout {
            id: "measurements",
            label: msg("drawing.toolbar.group.measurements", "Measurements"),
            sections: vec![
                section(
                    msg("drawing.toolbar.section.measurements", "Measurements"),
                    Types(MEASUREMENT_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.volume", "Volume"),
                    Types(VOLUME_TYPES),
                ),
            ],
        },
        ToolbarGroupLayout {
            id: "brushes-arrows-shapes",
            label: msg("drawing.toolbar.group.brushesArrowsShapes", "Shapes"),
            sections: vec![
                section(
                    msg("drawing.toolbar.section.brushes", "Brushes"),
                    Types(BRUSH_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.arrows", "Arrows"),
                    Types(ARROW_TYPES),
                ),
                section(
                    msg("drawing.toolbar.section.shapes", "Shapes"),
                    Types(SHAPE_TYPES),
                ),
            ],
        },
        ToolbarGroupLayout {
            id: "text",
            label: msg("drawing.toolbar.group.text", "Text"),
            sections: vec![section(
                msg("drawing.toolbar.section.text", "Text"),
                Types(TEXT_TYPES),
            )],
        },
        ToolbarGroupLayout {
            id: "icons",
            label: msg("drawing.toolbar.group.icons", "Icons"),
            sections: vec![section(
                msg("drawing.toolbar.section.icons", "Icons"),
                Types(ICON_TYPES),
            )],
        },
    ]
}

/// Resolve a type key to a [`ToolSource`], or `None` if unregistered.
fn tool_for(registry: &DrawingRegistry, type_key: &str) -> Option<ToolSource> {
    registry.get(type_key).map(|meta| ToolSource {
        type_key: meta.type_key.to_string(),
        label: meta.label.clone(),
        icon: meta.icon.clone(),
    })
}

/// Index registered tools by their declared registry group (registration order
/// preserved within a group).
fn tools_by_registry_group(
    registry: &DrawingRegistry,
    allowed: Option<&[String]>,
) -> HashMap<&'static str, Vec<ToolSource>> {
    let mut by_group: HashMap<&'static str, Vec<ToolSource>> = HashMap::new();
    for meta in registry.entries() {
        if allowed.is_some_and(|keys| !keys.iter().any(|k| k.as_str() == meta.type_key)) {
            continue;
        }
        by_group.entry(meta.group).or_default().push(ToolSource {
            type_key: meta.type_key.to_string(),
            label: meta.label.clone(),
            icon: meta.icon.clone(),
        });
    }
    by_group
}

fn resolve_section(
    source: &SectionTools,
    by_group: &HashMap<&'static str, Vec<ToolSource>>,
    tool_map: &HashMap<String, ToolSource>,
) -> Vec<ToolSource> {
    match source {
        SectionTools::RegistryGroup(group) => by_group.get(group).cloned().unwrap_or_default(),
        SectionTools::Types(types) => types
            .iter()
            .filter_map(|type_key| tool_map.get(*type_key).cloned())
            .collect(),
    }
}

/// Build the canonical seven-group toolbar, optionally restricted to a type subset.
fn assemble_toolbar(registry: &DrawingRegistry, allowed: Option<&[String]>) -> ToolbarSource {
    let by_group = tools_by_registry_group(registry, allowed);
    let mut tool_map: HashMap<String, ToolSource> = HashMap::new();
    for tools in by_group.values() {
        for tool in tools {
            tool_map.insert(tool.type_key.clone(), tool.clone());
        }
    }

    let mut groups = Vec::new();
    for layout in toolbar_layout() {
        let mut sections = Vec::new();
        let mut flat = Vec::new();
        for source in &layout.sections {
            let tools = resolve_section(&source.tools, &by_group, &tool_map);
            if tools.is_empty() {
                continue;
            }
            flat.extend(tools.iter().cloned());
            sections.push(ToolSectionSource {
                label: source.label.clone(),
                tools,
            });
        }
        if flat.is_empty() {
            continue;
        }
        groups.push(ToolGroupSource {
            id: layout.id.to_string(),
            label: layout.label,
            tools: flat,
            sections: (!sections.is_empty()).then_some(sections),
        });
    }
    ToolbarSource { groups }
}

/// Group a flat list of type keys into the canonical seven-group layout (drops
/// unregistered types).
fn group_tools(registry: &DrawingRegistry, tools: &[String]) -> ToolbarSource {
    let allowed: Vec<String> = tools
        .iter()
        .filter(|type_key| registry.get(type_key).is_some())
        .cloned()
        .collect();
    assemble_toolbar(registry, Some(&allowed))
}

/// Build explicit groups from developer config (drops unregistered/empty entries).
fn resolve_groups(registry: &DrawingRegistry, groups: &[ToolbarGroupConfig]) -> ToolbarSource {
    let mut out = Vec::new();
    for g in groups {
        let tools: Vec<ToolSource> = g
            .tools
            .iter()
            .filter_map(|type_key| tool_for(registry, type_key))
            .collect();
        if !tools.is_empty() {
            out.push(ToolGroupSource {
                id: g.id.clone(),
                label: g.label.clone(),
                tools,
                sections: None,
            });
        }
    }
    ToolbarSource { groups: out }
}

/// The toolbar with every label read in `i18n`'s language (English without one).
pub fn resolve_toolbar(source: &ToolbarSource, i18n: Option<&dyn Translator>) -> ToolbarDefinition {
    let tool = |t: &ToolSource| ToolDefinition {
        type_key: t.type_key.clone(),
        label: text_of(i18n, &t.label, &[]),
        icon: t.icon.clone(),
    };
    ToolbarDefinition {
        groups: source
            .groups
            .iter()
            .map(|g| ToolGroup {
                id: g.id.clone(),
                label: text_of(i18n, &g.label, &[]),
                tools: g.tools.iter().map(tool).collect(),
                sections: g.sections.as_ref().map(|sections| {
                    sections
                        .iter()
                        .map(|s| ToolSection {
                            label: text_of(i18n, &s.label, &[]),
                            tools: s.tools.iter().map(tool).collect(),
                        })
                        .collect()
                }),
            })
            .collect(),
    }
}

/// The default toolbar: every registered type, grouped into the canonical
/// seven-button layout, labelled in `i18n`'s language (English without one).
pub fn default_toolbar(
    registry: &DrawingRegistry,
    i18n: Option<&dyn Translator>,
) -> ToolbarDefinition {
    resolve_toolbar(&assemble_toolbar(registry, None), i18n)
}

/// Resolve `options.drawings` into a concrete toolbar definition + initial
/// visibility. Default (`None`) means the toolbar is VISIBLE; `false` keeps the
/// subsystem available with the toolbar hidden (headless use still works via
/// `chart.drawings.add(...)`); an object means `toolbar ?? true`.
pub fn build_toolbar(
    registry: &DrawingRegistry,
    option: Option<&DrawingsOption>,
    i18n: Option<&dyn Translator>,
) -> (ToolbarDefinition, bool) {
    let (source, visible) = toolbar_source(registry, option);
    (resolve_toolbar(&source, i18n), visible)
}

/// [`build_toolbar`] with the labels still messages, for a chart to read in its
/// language.
pub fn toolbar_source(
    registry: &DrawingRegistry,
    option: Option<&DrawingsOption>,
) -> (ToolbarSource, bool) {
    match option {
        Some(DrawingsOption::Bool(false)) => (assemble_toolbar(registry, None), false),
        None | Some(DrawingsOption::Bool(true)) => (assemble_toolbar(registry, None), true),
        Some(DrawingsOption::Config(config)) => {
            let visible = config.toolbar.unwrap_or(true);
            if let Some(groups) = &config.groups {
                (resolve_groups(registry, groups), visible)
            } else if let Some(tools) = &config.tools {
                (group_tools(registry, tools), visible)
            } else {
                (assemble_toolbar(registry, None), visible)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::cloned_ref_to_slice_refs, clippy::type_complexity)]
mod tests {
    use serde_json::json;

    use crate::i18n::english_translator;

    use super::*;

    fn build(option: Option<&DrawingsOption>) -> (ToolbarDefinition, bool) {
        build_toolbar(
            &DrawingRegistry::builtin(),
            option,
            Some(english_translator()),
        )
    }

    fn types(tools: &[ToolDefinition]) -> Vec<&str> {
        tools.iter().map(|t| t.type_key.as_str()).collect()
    }

    fn group<'a>(def: &'a ToolbarDefinition, id: &str) -> &'a ToolGroup {
        def.groups.iter().find(|g| g.id == id).expect("group")
    }

    fn section<'a>(g: &'a ToolGroup, label: &str) -> &'a ToolSection {
        g.sections
            .as_ref()
            .and_then(|s| s.iter().find(|s| s.label == label))
            .expect("section")
    }

    fn section_labels(g: &ToolGroup) -> Vec<&str> {
        g.sections
            .as_ref()
            .map(|s| s.iter().map(|s| s.label.as_str()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn default_toolbar_groups_registered_types_into_seven_canonical_buttons() {
        let (definition, visible) = build(Some(&true.into()));
        assert!(visible);
        let ids: Vec<&str> = definition.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "lines-channels-pitchforks",
                "fibonacci-gann",
                "patterns-waves-harmonics",
                "measurements",
                "brushes-arrows-shapes",
                "text",
                "icons",
            ]
        );

        let lines = group(&definition, "lines-channels-pitchforks");
        assert_eq!(lines.label, "Lines");
        assert_eq!(section_labels(lines), ["Lines", "Channels", "Pitchforks"]);
        let mut line_types = types(&section(lines, "Lines").tools);
        line_types.sort();
        assert_eq!(
            line_types,
            [
                "crossline",
                "extendedline",
                "hline",
                "hray",
                "infoline",
                "ray",
                "trendangle",
                "trendline",
                "vline",
            ]
        );
        assert_eq!(
            types(&section(lines, "Channels").tools),
            [
                "parallelchannel",
                "disjointchannel",
                "flattopbottom",
                "regressionchannel"
            ]
        );
        assert_eq!(
            types(&section(lines, "Pitchforks").tools),
            [
                "pitchfork",
                "schiffpitchfork",
                "modifiedschiffpitchfork",
                "insidepitchfork"
            ]
        );

        let fib = group(&definition, "fibonacci-gann");
        assert_eq!(
            section_labels(fib),
            ["Fibonacci", "Price Action", "Gann", "Geometry"]
        );
        assert_eq!(
            types(&section(fib, "Price Action").tools),
            ["fairvaluegap", "orderblock", "liquidity"]
        );
        assert_eq!(
            types(&section(fib, "Gann").tools),
            ["gannfan", "gannbox", "gannsquare"]
        );
        assert_eq!(
            types(&section(fib, "Geometry").tools),
            [
                "dedekind",
                "sonic",
                "supersonic",
                "goldensonic",
                "goldensupersonic"
            ]
        );

        let patterns = group(&definition, "patterns-waves-harmonics");
        assert_eq!(
            section_labels(patterns),
            ["Patterns", "Elliott Waves", "Harmonics"]
        );
        assert_eq!(
            types(&section(patterns, "Harmonics").tools),
            ["gartley", "bat", "butterfly", "crab", "shark", "cypher"]
        );

        let shapes = group(&definition, "brushes-arrows-shapes");
        assert_eq!(section_labels(shapes), ["Brushes", "Arrows", "Shapes"]);
        assert_eq!(
            types(&section(shapes, "Brushes").tools),
            ["freehand", "highlighter"]
        );
        assert_eq!(
            types(&section(shapes, "Arrows").tools),
            ["arrow", "arrowmarkup", "arrowmarkdown"]
        );

        let text = group(&definition, "text");
        assert_eq!(
            types(&text.sections.as_ref().expect("sections")[0].tools),
            [
                "text",
                "callout",
                "note",
                "pricenote",
                "comment",
                "pricelabel",
                "signpost"
            ]
        );

        let icons = group(&definition, "icons");
        assert_eq!(
            types(&icons.sections.as_ref().expect("sections")[0].tools),
            ["flagmark", "iconstamp"]
        );

        let measure = group(&definition, "measurements");
        assert_eq!(measure.label, "Measurements");
        let flat = types(&measure.tools);
        assert!(flat.contains(&"datepricerange") && flat.contains(&"position"));
        // Long/Short Position is the group's default (tools[0] arms when the button is clicked directly).
        assert_eq!(measure.tools[0].type_key, "position");
        assert_eq!(
            types(&section(measure, "Measurements").tools),
            ["position", "datepricerange", "magnifier"]
        );
    }

    #[test]
    fn explicit_groups_resolve_type_keys_and_drop_unknown_ones() {
        let option = DrawingsOption::Config(DrawingsConfig {
            groups: Some(vec![
                ToolbarGroupConfig {
                    id: "lines".into(),
                    label: "Lines".into(),
                    tools: vec!["trendline".into()],
                },
                ToolbarGroupConfig {
                    id: "fib".into(),
                    label: "Fibonacci".into(),
                    tools: vec!["fib-retracement".into()],
                },
            ]),
            ..Default::default()
        });
        let (definition, visible) = build(Some(&option));
        assert!(visible);
        let ids: Vec<&str> = definition.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["lines"]);
        assert_eq!(definition.groups[0].tools[0].label, "Trend Line");
        assert!(definition.groups[0].sections.is_none());
    }

    #[test]
    fn a_tools_subset_auto_groups_by_declared_group() {
        let option = DrawingsOption::Config(DrawingsConfig {
            tools: Some(vec!["hline".into(), "not-a-tool".into()]),
            ..Default::default()
        });
        let (definition, _) = build(Some(&option));
        assert_eq!(definition.groups.len(), 1);
        assert_eq!(definition.groups[0].tools[0].type_key, "hline");
        assert_eq!(definition.groups[0].tools.len(), 1);
    }

    #[test]
    fn the_option_decides_the_initial_visibility() {
        assert!(build(None).1);
        assert!(build(Some(&true.into())).1);
        assert!(!build(Some(&false.into())).1);
        let hidden = DrawingsOption::Config(DrawingsConfig {
            toolbar: Some(false),
            ..Default::default()
        });
        assert!(!build(Some(&hidden)).1);
        assert!(build(Some(&DrawingsOption::Config(DrawingsConfig::default()))).1);
        // `false` still builds the full toolbar: the subsystem stays available.
        assert_eq!(build(Some(&false.into())).0, build(None).0);
    }

    #[test]
    fn a_translator_less_build_reads_english() {
        let registry = DrawingRegistry::builtin();
        let plain = default_toolbar(&registry, None);
        let english = default_toolbar(&registry, Some(english_translator()));
        assert_eq!(plain, english);
        assert_eq!(plain.groups[0].label, "Lines");
    }

    #[test]
    fn the_definition_serializes_to_the_original_shape() {
        let definition = ToolbarDefinition {
            groups: vec![
                ToolGroup {
                    id: "g".into(),
                    label: "G".into(),
                    tools: vec![ToolDefinition {
                        type_key: "hline".into(),
                        label: "Horizontal Line".into(),
                        icon: "<svg/>".into(),
                    }],
                    sections: Some(vec![ToolSection {
                        label: "S".into(),
                        tools: vec![],
                    }]),
                },
                ToolGroup {
                    id: "h".into(),
                    label: "H".into(),
                    tools: vec![],
                    sections: None,
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(&definition).expect("json"),
            json!({ "groups": [
                { "id": "g", "label": "G",
                  "tools": [{ "type": "hline", "label": "Horizontal Line", "icon": "<svg/>" }],
                  "sections": [{ "label": "S", "tools": [] }] },
                { "id": "h", "label": "H", "tools": [] },
            ] })
        );
    }

    #[test]
    fn a_registry_missing_types_leaves_out_empty_sections_and_groups() {
        let (def, _) = build_toolbar(&DrawingRegistry::new(), None, None);
        assert!(def.groups.is_empty());
    }
}
