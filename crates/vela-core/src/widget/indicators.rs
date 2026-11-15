//! The widget's indicator configuration: a JSON manifest passed at init, either inline or
//! as a URL returning the same JSON (the config seam for script libraries).
//!
//! Manifest shape (array or `{ "indicators": [...] }` wrapper):
//!
//! ```text
//! [{ "name": "EMA 20", "script": "...pine source...", "language": "pine", "enabled": true }]
//! ```
//!
//! An entry may carry `"url"` instead of `"script"`: the source is fetched lazily.
//!
//! Differences from the original: `fetch` is the [`Platform`]'s; the page's own URL (the
//! original reads `location.href` to resolve a relative `url`) is a parameter,
//! `page_url`, and a relative URL with no base to resolve against is fetched as written.
//! A manifest fetched from a URL is untrusted JSON: entries that are not objects with a
//! string `name` are dropped with a warning.

use std::rc::Rc;

use serde_json::Value;

use crate::model::inputs::{InputValue, Inputs};
use crate::runtime::{HttpRequest, LocalBoxFuture, Platform};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorManifestEntry {
    pub name: String,
    /// Inline script source (one of script/url required).
    pub script: Option<String>,
    /// Fetch the source from here instead (resolved relative to the manifest URL).
    pub url: Option<String>,
    /// Engine language (default: the chart's default engine).
    pub language: Option<String>,
    /// Add to the chart at startup (default true). Disabled entries only appear in pickers.
    pub enabled: Option<bool>,
    /// Picker grouping (default `"Indicators"`).
    pub category: Option<String>,
}

/// The manifest: the entries (the array form and the wrapper object read alike).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorManifest(pub Vec<IndicatorManifestEntry>);

impl IndicatorManifest {
    /// Read a manifest from JSON: a bare array, or an object with an `indicators` array.
    /// Anything else is an empty manifest; unusable entries are returned in `skipped`
    /// (their index) for the caller to warn about.
    pub fn from_json(value: &Value) -> (Self, Vec<usize>) {
        let list = match value {
            Value::Array(a) => Some(a),
            Value::Object(o) => o.get("indicators").and_then(Value::as_array),
            _ => None,
        };
        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        for (i, item) in list.into_iter().flatten().enumerate() {
            let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
            let Some(name) = text("name") else {
                skipped.push(i);
                continue;
            };
            entries.push(IndicatorManifestEntry {
                name,
                script: text("script"),
                url: text("url"),
                language: text("language"),
                enabled: item.get("enabled").and_then(Value::as_bool),
                category: text("category"),
            });
        }
        (Self(entries), skipped)
    }
}

/// A manifest entry with its source resolved and ready for `chart.add_indicator`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedIndicator {
    pub name: String,
    pub script: String,
    pub language: Option<String>,
    pub enabled: bool,
    pub category: Option<String>,
}

/// An async manifest source, called once at resolution time. The escape hatch for
/// manifests that a URL can't express: a filesystem read, an authenticated API. A
/// rejection (`Err`) behaves like a failing manifest URL.
pub type IndicatorLoader =
    Rc<dyn Fn() -> LocalBoxFuture<'static, Result<IndicatorManifest, String>>>;

/// Where the manifest comes from: inline, a URL returning it, or an async loader.
#[derive(Clone)]
pub enum IndicatorsSource {
    Manifest(IndicatorManifest),
    Url(String),
    Loader(IndicatorLoader),
}

/// `new URL(rel, base).href` for the common absolute bases (`scheme://authority/path`);
/// `None` when `base` is not one.
fn join_url(base: &str, rel: &str) -> Option<String> {
    let has_scheme = |s: &str| {
        s.find("://").is_some_and(|i| {
            i > 0
                && s[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        })
    };
    if has_scheme(rel) {
        return Some(rel.to_string());
    }
    if !has_scheme(base) {
        return None;
    }
    let base = base.split(['#', '?']).next().unwrap_or(base);
    let scheme_end = base.find("://")? + 3;
    let origin_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    let (origin, path) = base.split_at(origin_end);
    if let Some(rest) = rel.strip_prefix("//") {
        return Some(format!("{}//{rest}", &base[..scheme_end - 2]));
    }
    let merged = if rel.starts_with('/') {
        rel.to_string()
    } else {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        format!("{dir}/{rel}")
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in merged.split('/').skip(1) {
        match seg {
            "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    Some(format!("{origin}/{}", out.join("/")))
}

/// Load the indicator list: a manifest object, a URL returning the manifest JSON, or an
/// async loader returning the manifest. Entries with `url` sources are fetched here too
/// (relative to the manifest URL when there is one, else `page_url`). Entries that fail
/// to resolve are dropped with a warning: one broken script must not take the chart
/// down. A manifest that cannot be loaded at all is an `Err`.
pub async fn resolve_indicators(
    config: IndicatorsSource,
    platform: &dyn Platform,
    page_url: Option<&str>,
) -> Result<Vec<ResolvedIndicator>, String> {
    let (manifest, base_url): (IndicatorManifest, Option<String>) = match config {
        IndicatorsSource::Loader(load) => (load().await?, None),
        IndicatorsSource::Url(url) => {
            let res = platform
                .fetch(HttpRequest::get(url.clone()))
                .await
                .map_err(|e| e.to_string())?;
            if !res.ok() {
                return Err(format!("indicator manifest {url}: HTTP {}", res.status));
            }
            let json = res.json().map_err(|e| e.to_string())?;
            let (manifest, skipped) = IndicatorManifest::from_json(&json);
            for i in skipped {
                platform.warn(&format!(
                    "[vela] indicator manifest entry {i} skipped: no name"
                ));
            }
            (manifest, Some(url))
        }
        IndicatorsSource::Manifest(m) => (m, None),
    };

    let mut out = Vec::new();
    for entry in manifest.0 {
        let script = match (&entry.script, &entry.url) {
            (Some(script), _) => Ok(script.clone()),
            (None, Some(entry_url)) => {
                let base = base_url.as_deref().or(page_url);
                let url = base
                    .and_then(|b| join_url(b, entry_url))
                    .unwrap_or_else(|| entry_url.clone());
                match platform.fetch(HttpRequest::get(url)).await {
                    Ok(res) if res.ok() => Ok(res.body),
                    Ok(res) => Err(format!("HTTP {}", res.status)),
                    Err(e) => Err(e.to_string()),
                }
            }
            (None, None) => Err("entry has neither script nor url".to_string()),
        };
        match script {
            Ok(script) => out.push(ResolvedIndicator {
                name: entry.name,
                script,
                language: entry.language,
                enabled: entry.enabled != Some(false),
                category: entry.category,
            }),
            Err(err) => platform.warn(&format!(
                "[vela] indicator \"{}\" skipped: {err}",
                entry.name
            )),
        }
    }
    Ok(out)
}

/// One persisted manifest-instance entry: the bare NAME when every value sits on its
/// declaration default, else the name plus the DELTAS (see `input_deltas`): a default
/// that later changes in the script must never stay frozen in saved documents.
#[derive(Clone, Debug, PartialEq)]
pub enum LedgerManifestEntry {
    Name(String),
    Detailed {
        name: String,
        inputs: Option<Inputs>,
        props: Option<Inputs>,
        hidden: Option<bool>,
    },
}

impl LedgerManifestEntry {
    /// The entry's manifest NAME, whichever shape it travels as.
    pub fn name(&self) -> &str {
        match self {
            LedgerManifestEntry::Name(n) => n,
            LedgerManifestEntry::Detailed { name, .. } => name,
        }
    }
}

/// One persisted native-indicator entry: the bare TYPE when every input sits on its
/// declaration default and the indicator is visible, else the type plus the input
/// DELTAS and/or the `hidden` flag: same rule as [`LedgerManifestEntry`], so native
/// settings and visibility survive a state round-trip too.
#[derive(Clone, Debug, PartialEq)]
pub enum LedgerNativeEntry {
    Type(String),
    Detailed {
        indicator_type: String,
        inputs: Option<Inputs>,
        hidden: Option<bool>,
    },
}

impl LedgerNativeEntry {
    /// The entry's native TYPE, whichever shape it travels as.
    pub fn native_type(&self) -> &str {
        match self {
            LedgerNativeEntry::Type(t) => t,
            LedgerNativeEntry::Detailed { indicator_type, .. } => indicator_type,
        }
    }
}

/// Everything [`indicator_ledger`] needs to decide what a state snapshot reports.
#[derive(Clone, Debug, Default)]
pub struct LedgerInputs {
    /// Native entries present on the chart RIGHT NOW (live handles: type + input
    /// deltas + hidden, falling back to bare types from the sync registry).
    pub present: Vec<LedgerNativeEntry>,
    /// The live manifest instances (the shell's own synchronous list), values included.
    pub instance_entries: Vec<LedgerManifestEntry>,
    /// A restored ledger's manifest half, until it materializes (`None` once consumed).
    pub pending_manifest: Option<Vec<LedgerManifestEntry>>,
    /// The manifest can no longer change the instance set: it resolved, or the shell was
    /// built without an `indicators` option so nothing will ever resolve. Until then the
    /// pending names are reported as-is: an early save must not wipe a restored ledger
    /// the shell simply hasn't materialized yet.
    pub manifest_settled: bool,
    /// The volume auto-add is still owed: before the first `load:end` while the intent
    /// (restored ledger, else the `volume` option) says volume, or after it while the
    /// chart waits for the symbol's facts to decide the add. The registry cannot show it
    /// yet; without this, an early save would write `natives: []` and the reload would
    /// read it as a deliberate opt-out, silencing volume forever.
    pub volume_pending: bool,
}

/// The indicator ledger a state snapshot persists, PURE. The one rule: a LIVE (possibly
/// empty) set is the truth once its source settled; fallbacks cover ONLY the boot window
/// before a restored ledger materializes. "Empty because the user removed everything" must
/// never be repainted as "empty because nothing loaded yet": that resurrection was the
/// bug this helper exists to pin down.
pub fn indicator_ledger(i: &LedgerInputs) -> (Vec<LedgerManifestEntry>, Vec<LedgerNativeEntry>) {
    let mut natives = i.present.clone();
    if i.volume_pending && !natives.iter().any(|e| e.native_type() == "volume") {
        natives.push(LedgerNativeEntry::Type("volume".to_string()));
    }
    let manifest = if i.manifest_settled {
        i.instance_entries.clone()
    } else {
        i.pending_manifest
            .clone()
            .unwrap_or_else(|| i.instance_entries.clone())
    };
    (manifest, natives)
}

/// A value bag for one indicator entry (an input or prop override).
pub fn inputs_of(pairs: &[(&str, InputValue)]) -> Inputs {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::HttpResponse;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    fn entry(name: &str) -> IndicatorManifestEntry {
        IndicatorManifestEntry {
            name: name.into(),
            script: Some(format!("// {name}")),
            ..IndicatorManifestEntry::default()
        }
    }

    fn resolve(
        platform: &ManualPlatform,
        source: IndicatorsSource,
    ) -> Result<Vec<ResolvedIndicator>, String> {
        let handle = platform.handle();
        let fut = async move { resolve_indicators(source, handle.as_ref(), None).await };
        platform.block_on(fut)
    }

    #[test]
    fn an_inline_manifest_resolves_with_defaults() {
        let platform = ManualPlatform::new(0.0);
        let mut off = entry("B");
        off.enabled = Some(false);
        off.category = Some("Mine".into());
        let out = resolve(
            &platform,
            IndicatorsSource::Manifest(IndicatorManifest(vec![entry("A"), off])),
        )
        .expect("resolves");
        assert_eq!(out.len(), 2);
        assert!(out[0].enabled);
        assert!(!out[1].enabled);
        assert_eq!(out[1].category.as_deref(), Some("Mine"));
    }

    #[test]
    fn a_loader_that_fails_is_an_error() {
        let platform = ManualPlatform::new(0.0);
        let loader: IndicatorLoader =
            Rc::new(|| Box::pin(std::future::ready(Err("nope".to_string()))));
        assert_eq!(
            resolve(&platform, IndicatorsSource::Loader(loader)),
            Err("nope".to_string())
        );
    }

    #[test]
    fn an_entry_with_neither_script_nor_url_is_skipped_with_a_warning() {
        let platform = ManualPlatform::new(0.0);
        let bad = IndicatorManifestEntry {
            name: "bad".into(),
            ..IndicatorManifestEntry::default()
        };
        let out = resolve(
            &platform,
            IndicatorsSource::Manifest(IndicatorManifest(vec![bad, entry("ok")])),
        )
        .expect("resolves");
        assert_eq!(
            out.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["ok"]
        );
    }

    #[test]
    fn the_manifest_json_reads_as_array_or_wrapper_and_drops_nameless_entries() {
        let array = json!([{ "name": "A", "script": "s" }, { "script": "no name" }, 7]);
        let (m, skipped) = IndicatorManifest::from_json(&array);
        assert_eq!(m.0.len(), 1);
        assert_eq!(skipped, [1, 2]);
        let wrapped = json!({ "indicators": [{ "name": "W", "url": "x.pine", "enabled": false }] });
        let (m, _) = IndicatorManifest::from_json(&wrapped);
        assert_eq!(m.0[0].url.as_deref(), Some("x.pine"));
        assert_eq!(m.0[0].enabled, Some(false));
        assert!(
            IndicatorManifest::from_json(&json!("garbage"))
                .0
                .0
                .is_empty()
        );
        assert!(
            IndicatorManifest::from_json(&json!({ "indicators": 3 }))
                .0
                .0
                .is_empty()
        );
    }

    #[test]
    fn relative_urls_resolve_against_the_manifest_url() {
        assert_eq!(
            join_url("https://h.example/cfg/manifest.json", "a/b.pine").as_deref(),
            Some("https://h.example/cfg/a/b.pine")
        );
        assert_eq!(
            join_url("https://h.example/cfg/manifest.json", "../x.pine").as_deref(),
            Some("https://h.example/x.pine")
        );
        assert_eq!(
            join_url("https://h.example/cfg/m.json?v=1", "/root.pine").as_deref(),
            Some("https://h.example/root.pine")
        );
        assert_eq!(
            join_url("https://h.example/m.json", "https://o.example/s.pine").as_deref(),
            Some("https://o.example/s.pine")
        );
        assert_eq!(join_url("m.json", "a.pine"), None);
    }

    #[test]
    fn a_manifest_url_and_entry_urls_are_fetched_through_the_platform() {
        let platform = ManualPlatform::new(0.0);
        let manifest =
            json!([{ "name": "Remote", "url": "scripts/r.pine", "language": "pine" }]).to_string();
        platform.on_fetch(move |request| {
            let body = match request.url.as_str() {
                "https://h.example/cfg/manifest.json" => manifest.clone(),
                "https://h.example/cfg/scripts/r.pine" => "plot(close)".to_string(),
                _ => return None,
            };
            Some(Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body,
            }))
        });
        let out = resolve(
            &platform,
            IndicatorsSource::Url("https://h.example/cfg/manifest.json".into()),
        )
        .expect("resolves");
        assert_eq!(out[0].script, "plot(close)");
        assert_eq!(out[0].language.as_deref(), Some("pine"));
    }

    #[test]
    fn a_failing_manifest_url_is_an_error() {
        let platform = ManualPlatform::new(0.0);
        platform.on_fetch(|_| {
            Some(Ok(HttpResponse {
                status: 404,
                headers: vec![],
                body: String::new(),
            }))
        });
        let err = resolve(
            &platform,
            IndicatorsSource::Url("https://h.example/m.json".into()),
        )
        .unwrap_err();
        assert_eq!(err, "indicator manifest https://h.example/m.json: HTTP 404");
    }

    fn name(n: &str) -> LedgerManifestEntry {
        LedgerManifestEntry::Name(n.into())
    }

    #[test]
    fn a_settled_live_set_is_the_truth_even_when_empty() {
        let inputs = LedgerInputs {
            instance_entries: vec![],
            pending_manifest: Some(vec![name("restored")]),
            manifest_settled: true,
            ..LedgerInputs::default()
        };
        let (manifest, natives) = indicator_ledger(&inputs);
        assert!(manifest.is_empty());
        assert!(natives.is_empty());
    }

    #[test]
    fn before_settling_the_pending_ledger_is_reported_as_is() {
        let inputs = LedgerInputs {
            instance_entries: vec![],
            pending_manifest: Some(vec![name("restored")]),
            manifest_settled: false,
            ..LedgerInputs::default()
        };
        assert_eq!(indicator_ledger(&inputs).0, [name("restored")]);
        let consumed = LedgerInputs {
            instance_entries: vec![name("live")],
            pending_manifest: None,
            manifest_settled: false,
            ..LedgerInputs::default()
        };
        assert_eq!(indicator_ledger(&consumed).0, [name("live")]);
    }

    #[test]
    fn a_volume_still_owed_is_reported_once() {
        let owed = LedgerInputs {
            volume_pending: true,
            manifest_settled: true,
            ..LedgerInputs::default()
        };
        let (_, natives) = indicator_ledger(&owed);
        assert_eq!(natives, [LedgerNativeEntry::Type("volume".into())]);
        let present = LedgerInputs {
            present: vec![LedgerNativeEntry::Detailed {
                indicator_type: "volume".into(),
                inputs: None,
                hidden: Some(true),
            }],
            volume_pending: true,
            manifest_settled: true,
            ..LedgerInputs::default()
        };
        assert_eq!(indicator_ledger(&present).1.len(), 1);
    }

    #[test]
    fn entry_names_and_types_read_whichever_shape_they_travel_as() {
        let detailed = LedgerManifestEntry::Detailed {
            name: "EMA".into(),
            inputs: Some(inputs_of(&[("len", InputValue::Number(9.0))])),
            props: None,
            hidden: None,
        };
        assert_eq!(detailed.name(), "EMA");
        assert_eq!(name("RSI").name(), "RSI");
        assert_eq!(LedgerNativeEntry::Type("sma".into()).native_type(), "sma");
    }
}
