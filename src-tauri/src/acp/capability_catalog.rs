//! Capability-catalog domain types — backing the `get_delegation_capabilities`
//! MCP tool.
//!
//! Before fanning work out with `delegate_to_agent`, the parent LLM needs to
//! know what each delegable sub-agent can actually be configured with: which
//! models, which modes, which reasoning levels. This module is the
//! layer-shared piece (mirroring [`crate::acp::session_info`]):
//!
//!   * [`AgentCapabilities`] / [`CapabilitiesReport`] — the self-describing
//!     outcome delivered over the broker socket, one entry per agent, so the
//!     companion renders it without re-querying.
//!   * [`CapabilityCatalogAccess`] — the listener-facing trait the production
//!     `ConnectionManagerCapabilityCatalog` (in `crate::acp::manager`)
//!     implements; kept here so the listener can be unit-tested with an
//!     in-memory stub.
//!   * Pure builders that project the two data source families into that one
//!     shape:
//!     - **static** — on-disk catalogs read without starting any agent
//!       process (codex's generated `model_catalog_json` chain, ZCode's
//!       `~/.zcode/v2/config.json` provider table);
//!     - **advertised** — the modes + config options a live ACP connection of
//!       that agent type has already published (the same
//!       [`SessionState`](crate::acp::SessionState) selectors the composer
//!       reads), cached opportunistically: a source of data, never a reason
//!       to launch anything.
//!
//! Honesty rules the whole module. Every list is exactly what a source said,
//! deduplicated and order-preserving; a field no source could fill is EMPTY,
//! the `source` tag names where the rest came from, and `notes` says what is
//! missing and why — never a guessed value.

use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::acp::types::{SessionConfigKindInfo, SessionConfigOptionInfo, SessionModeStateInfo};

/// The conventional config-option id of a model selector when no source named
/// the real one. This is the id `session/set_config_option` uses across the
/// agents codeg knows (and the id codeg itself synthesizes for Grok), so it is
/// the honest default for forwarding a model preference whose wire id is
/// unknown — the agent may still ignore it, and the drift is visible in the
/// delegation result.
pub const MODEL_CONFIG_OPTION_ID: &str = "model";

/// The conventional config-option id of a reasoning / thought-level selector
/// when no source named the real one — the id codeg itself synthesizes for
/// Grok's effort selector. Same preference semantics as
/// [`MODEL_CONFIG_OPTION_ID`].
pub const REASONING_EFFORT_CONFIG_OPTION_ID: &str = "reasoning_effort";

/// Upper bound on a config file this module is willing to parse. The catalogs
/// are small (tens of models); a file bigger than this is not a catalog but a
/// mistake, and refusing it keeps the tool bounded no matter what landed on
/// disk.
const MAX_CONFIG_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Where one agent's capability lists came from. Ordered by precedence:
/// static catalogs beat cached advertisements (a static read is deterministic
/// and answers without any agent process having run), advertisements beat
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Read from an on-disk catalog the agent itself consumes (codex's
    /// generated model catalog; ZCode's provider config). No agent process
    /// was started to answer.
    Static,
    /// Captured from what a live ACP connection of this agent type has
    /// advertised (`modes` + `config_options`). Reflects the runtime
    /// environment that session launched with.
    Advertised,
    /// Neither source produced data — the lists are empty because they are
    /// unknown, not because the agent has none. `notes` explains what would
    /// surface them.
    Unknown,
}

/// One delegable agent's capability matrix, as reported to the parent LLM.
/// All lists are deduplicated, order-preserving, and possibly EMPTY — an
/// empty list with `source: "unknown"` means "not known", never "none exist".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// The `agent_type` slug `delegate_to_agent` takes (e.g. `codex`,
    /// `zcode`, `custom:goose`).
    pub agent_type: String,
    /// Human-facing name (e.g. "Codex", "ZCode").
    pub display_name: String,
    /// Model ids the agent can be configured with, in source order.
    pub models: Vec<String>,
    /// Mode ids (`session/set_mode` targets) the agent advertises.
    pub modes: Vec<String>,
    /// Reasoning-effort / thought-level ids the agent accepts (per-model
    /// where the source is per-model — see `notes`).
    pub reasoning_levels: Vec<String>,
    /// The config-option id (`session/set_config_option` target) the model
    /// selector is published under, when a source identified one. Advertised
    /// entries know it — the option was on the wire; static catalog
    /// projections know the VALUES, not the agent's wire id, and leave this
    /// `None` so a caller forwarding a preference falls back to the
    /// conventional [`MODEL_CONFIG_OPTION_ID`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_option_id: Option<String>,
    /// Same as [`AgentCapabilities::model_option_id`] for the reasoning /
    /// thought-level selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_option_id: Option<String>,
    pub source: CapabilitySource,
    /// Honest caveats: what is missing, why, and how it would surface. Also
    /// carries per-model qualifications a flat list cannot (e.g. codex's
    /// reasoning levels are per model).
    pub notes: Vec<String>,
}

impl AgentCapabilities {
    /// An honest "no source could answer" entry: everything empty, `source:
    /// Unknown`, and the standing note explaining what would fill it.
    pub fn unknown(agent_type: &str, display_name: &str) -> Self {
        Self {
            agent_type: agent_type.to_string(),
            display_name: display_name.to_string(),
            models: Vec::new(),
            modes: Vec::new(),
            reasoning_levels: Vec::new(),
            model_option_id: None,
            reasoning_option_id: None,
            source: CapabilitySource::Unknown,
            notes: vec![
                "No static catalog and no cached advertisement for this agent; its \
                 models / modes / reasoning levels are unknown from here."
                    .to_string(),
                "They surface automatically once a session of this agent type runs in \
                 codeg (its advertised selectors are cached); delegating without \
                 model / mode arguments always works and uses the agent's own \
                 defaults."
                    .to_string(),
            ],
        }
    }
}

/// The whole report: one entry per delegable agent (or the one the caller
/// filtered to), plus an envelope-level note for filter misses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilitiesReport {
    pub agents: Vec<AgentCapabilities>,
    /// Set when the caller's `agent_type` filter matched nothing (unknown
    /// slug, or a custom agent that is no longer registered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Listener-facing access to resolve the catalog. The production impl
/// (`ConnectionManagerCapabilityCatalog`) combines the static on-disk
/// catalogs with the live-connection advertisement cache; tests use an
/// in-memory stub. Mirrors [`crate::acp::session_info::SessionInfoAccess`].
///
/// `agent_type` is the OPTIONAL filter slug exactly as `delegate_to_agent`
/// spells it; `None` reports every delegable agent.
#[async_trait]
pub trait CapabilityCatalogAccess: Send + Sync {
    async fn resolve(&self, agent_type: Option<&str>) -> CapabilitiesReport;
}

/// A select option's `value`, when the option is a select.
fn select_values(option: &SessionConfigOptionInfo) -> Vec<String> {
    match &option.kind {
        SessionConfigKindInfo::Select(select) => select
            .options
            .iter()
            .map(|o| o.value.clone())
            .collect::<Vec<_>>(),
        SessionConfigKindInfo::Boolean(_) => Vec::new(),
    }
}

/// Push `values` into `out`, skipping ones already present (first-seen order).
fn push_unique(out: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
}

/// Whether a config option is the MODEL selector: ACP's `category: "model"`,
/// or the conventional `model` id (Grok's synthesized selector uses both).
fn is_model_option(option: &SessionConfigOptionInfo) -> bool {
    option.category.as_deref() == Some("model") || option.id == MODEL_CONFIG_OPTION_ID
}

/// Whether a config option is the REASONING selector: ACP's
/// `category: "thought_level"`, or the id Grok's synthesized effort selector
/// carries. Deliberately narrow — an id weaselled out of a substring match
/// (`"reasoning_mode"`…) is a guess, and this module does not guess.
fn is_reasoning_option(option: &SessionConfigOptionInfo) -> bool {
    option.category.as_deref() == Some("thought_level")
        || option.id == REASONING_EFFORT_CONFIG_OPTION_ID
}

/// Project a live session's advertised selectors (the same
/// `modes` / `config_options` the composer reads) into an
/// [`AgentCapabilities`]. Nothing is invented: model ids come from
/// model-category selects, reasoning ids from thought-level selects, mode ids
/// from the advertised mode catalog, and a field no advertisement filled is
/// simply empty (with a note saying so, so an honest gap never reads as
/// "the agent supports nothing").
pub fn from_advertised(
    agent_type: &str,
    display_name: &str,
    modes: Option<&SessionModeStateInfo>,
    config_options: &[SessionConfigOptionInfo],
) -> AgentCapabilities {
    let mut models = Vec::new();
    let mut reasoning = Vec::new();
    let mut model_option_id = None;
    let mut reasoning_option_id = None;
    let mut notes = Vec::new();
    for option in config_options {
        if is_model_option(option) {
            push_unique(&mut models, select_values(option));
            if model_option_id.is_none() {
                model_option_id = Some(option.id.clone());
            }
        } else if is_reasoning_option(option) {
            push_unique(&mut reasoning, select_values(option));
            if reasoning_option_id.is_none() {
                reasoning_option_id = Some(option.id.clone());
            }
        }
    }
    let mode_list: Vec<String> = modes
        .map(|m| {
            m.available_modes
                .iter()
                .map(|mode| mode.id.clone())
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        notes.push(
            "This agent advertised no model selector; its model is fixed by its own \
             configuration, not selectable per delegation."
                .to_string(),
        );
    }
    if reasoning.is_empty() {
        notes.push(
            "No reasoning-level selector was advertised; delegating without a \
             reasoning argument uses the agent's default."
                .to_string(),
        );
    }
    if mode_list.is_empty() {
        notes.push(
            "No session modes were advertised; this agent runs in a single mode.".to_string(),
        );
    }
    AgentCapabilities {
        agent_type: agent_type.to_string(),
        display_name: display_name.to_string(),
        models,
        modes: mode_list,
        reasoning_levels: reasoning,
        model_option_id,
        reasoning_option_id,
        source: CapabilitySource::Advertised,
        notes,
    }
}

/// Project codex's catalog `models` array (opaque [`Value`] entries, the same
/// shape `codex debug models --bundled` emits and codeg's generated
/// `model_catalog_json` carries) into an [`AgentCapabilities`].
///
/// Only `visibility: "list"` entries are models the picker offers — the rest
/// are migration stubs / internal entries codex keeps for itself, and
/// offering them to the LLM would produce delegations codex silently
/// downgrades. Reasoning levels are the UNION of the per-model
/// `supported_reasoning_levels[].effort` (the catalog is per-model; the note
/// says so, because a flat union can promise a pairing the agent rejects).
pub fn from_codex_catalog(
    agent_type: &str,
    display_name: &str,
    catalog_models: &[Value],
) -> AgentCapabilities {
    let mut models = Vec::new();
    let mut reasoning: Vec<String> = Vec::new();
    let mut has_levels = false;
    for entry in catalog_models {
        let listable = entry
            .get("visibility")
            .and_then(Value::as_str)
            .is_some_and(|v| v == "list");
        if !listable {
            continue;
        }
        let Some(slug) = entry.get("slug").and_then(Value::as_str) else {
            continue;
        };
        push_unique(&mut models, [slug.to_string()]);
        if let Some(levels) = entry
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
        {
            let efforts: Vec<String> = levels
                .iter()
                .filter_map(|l| l.get("effort").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            if !efforts.is_empty() {
                has_levels = true;
                push_unique(&mut reasoning, efforts);
            }
        }
    }
    let mut notes = Vec::new();
    if has_levels {
        notes.push(
            "Reasoning levels are the union across models — codex supports them \
             PER MODEL, so a specific model may accept only a subset."
                .to_string(),
        );
    }
    if models.is_empty() {
        notes.push("The catalog source produced no listable models.".to_string());
    }
    AgentCapabilities {
        agent_type: agent_type.to_string(),
        display_name: display_name.to_string(),
        models,
        modes: Vec::new(),
        reasoning_levels: reasoning,
        // The catalog file names models, not the agent's config-option wire
        // ids — callers fall back to the conventional spellings.
        model_option_id: None,
        reasoning_option_id: None,
        source: CapabilitySource::Static,
        notes,
    }
}

/// What [`parse_zcode_provider_config`] extracted from ZCode's config:
/// the model names across enabled providers, plus the union of their
/// reasoning variants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZcodeProviderCatalog {
    pub models: Vec<String>,
    pub reasoning_variants: Vec<String>,
}

/// Parse ZCode's `~/.zcode/v2/config.json` provider table into a
/// [`ZcodeProviderCatalog`]. Pure (takes the raw document text) so it is
/// unit-testable against a fixture.
///
/// SECURITY: the config carries provider credentials (`options.apiKey`,
/// `baseURL`). This parser reads ONLY `enabled`, the `models` keys, and each
/// model's `reasoning.variants` — credentials cannot leak because they are
/// never extracted, and a test pins that the serialized output of a parsed
/// config containing an api key contains none of its bytes.
///
/// Degrades honestly: `None` on a document that is not JSON, has no
/// `provider` object, or yields no enabled providers with models — the caller
/// answers `unknown` rather than inventing a catalog.
pub fn parse_zcode_provider_config(raw: &str) -> Option<ZcodeProviderCatalog> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let providers = parsed.get("provider")?.as_object()?;
    let mut out = ZcodeProviderCatalog::default();
    for (_id, provider) in providers {
        let enabled = provider
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (name, model) in models {
            push_unique(&mut out.models, [name.clone()]);
            let reasoning = model.get("reasoning");
            let reasoning_enabled = reasoning
                .and_then(|r| r.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !reasoning_enabled {
                continue;
            }
            if let Some(variants) = reasoning
                .and_then(|r| r.get("variants"))
                .and_then(Value::as_array)
            {
                let names: Vec<String> = variants
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                push_unique(&mut out.reasoning_variants, names);
            }
        }
    }
    (!out.models.is_empty()).then_some(out)
}

/// Project a parsed [`ZcodeProviderCatalog`] into an [`AgentCapabilities`].
/// The model names are exactly as ZCode's config spells them (bare names,
/// not provider-qualified), and the note says so — the id a live session's
/// selector accepts may be qualified, and that is only knowable from an
/// advertisement.
pub fn from_zcode_provider_catalog(
    agent_type: &str,
    display_name: &str,
    catalog: &ZcodeProviderCatalog,
) -> AgentCapabilities {
    AgentCapabilities {
        agent_type: agent_type.to_string(),
        display_name: display_name.to_string(),
        models: catalog.models.clone(),
        modes: Vec::new(),
        reasoning_levels: catalog.reasoning_variants.clone(),
        // The provider config names models, not the agent's config-option
        // wire ids — callers fall back to the conventional spellings.
        model_option_id: None,
        reasoning_option_id: None,
        source: CapabilitySource::Static,
        notes: vec![
            "Model names come from ZCode's own provider config (bare names, one \
             list across enabled providers); reasoning variants are the union \
             across those models."
                .to_string(),
        ],
    }
}

/// Read a bounded config file: `None` when missing, unreadable, oversized
/// ([`MAX_CONFIG_FILE_BYTES`]), or not valid UTF-8.
fn read_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The static codex source: resolve `model_catalog_json` out of
/// `config.toml` the way codex does (so the answer is literally the file
/// codex will read at launch), else fall back to codeg's cached / bundled
/// snapshot of codex's own catalog (what codex uses when nothing replaces
/// it). Returns the `models` array plus a note naming which branch answered,
/// so a hand-written catalog the user pointed at is visible as such.
///
/// Pure over the paths it is handed; the production caller passes
/// `codex_home` (honoring `CODEX_HOME`), tests pass a fixture dir.
pub fn codex_catalog_models(codex_home: &Path) -> (Vec<Value>, Option<String>) {
    // A `model_catalog_json` reference beats the snapshot: it is a
    // whole-table replace, so when codex has one its own catalog is not in
    // play at all.
    if let Ok(toml_raw) = std::fs::read_to_string(codex_home.join("config.toml")) {
        if let Ok(doc) = toml_raw.parse::<toml::Value>() {
            if let Some(rel) = doc
                .get("model_catalog_json")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let path = resolve_codex_home_relative(rel, codex_home);
                if let Some(raw) = read_bounded(&path) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
                        if let Some(models) = parsed
                            .get("models")
                            .and_then(Value::as_array)
                            .cloned()
                            .filter(|m| !m.is_empty())
                        {
                            return (
                                models,
                                Some(format!(
                                    "Models read from the catalog file codex is \
                                     configured to launch with ({}).",
                                    rel
                                )),
                            );
                        }
                    }
                }
            }
        }
    }
    // No (or unreadable) replacement: codex's own catalog, mirrored by the
    // cache the settings editor keeps warm, else the compiled-in snapshot.
    (
        crate::acp::codex_catalog_source::cached_or_bundled_snapshot(),
        Some(
            "Models from codex's own bundled catalog (no model_catalog_json \
             replacement is configured); custom models configured in codeg's \
             Codex settings appear here only after that catalog is written."
                .to_string(),
        ),
    )
}

/// Resolve a `model_catalog_json` value the way codex does: `~/…` against the
/// home dir, absolute paths verbatim, everything else relative to
/// `CODEX_HOME`. Mirrors the settings-side helper so both readers agree on
/// which file a reference names.
fn resolve_codex_home_relative(value: &str, codex_home: &Path) -> std::path::PathBuf {
    if value == "~" {
        return home_dir_or_default();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir_or_default().join(rest);
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        codex_home.join(value)
    }
}

fn home_dir_or_default() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The static ZCode source: read `~/.zcode/v2/config.json` under a home dir.
/// `None` when there is nothing to read (no home, no file, oversized) — the
/// caller answers `unknown`.
pub fn zcode_provider_config_raw(home: Option<&Path>) -> Option<String> {
    read_bounded(&home?.join(".zcode").join("v2").join("config.json"))
}

/// Fold a modes-only advertisement into an otherwise static entry: the static
/// catalogs carry no session modes, so when a live connection of the same
/// agent type has advertised them, fill just that field and say where it came
/// from. Mutates nothing else — static lists keep precedence, per the
/// module's source ordering.
pub fn merge_advertised_modes(entry: &mut AgentCapabilities, modes: &SessionModeStateInfo) {
    let ids: Vec<String> = modes.available_modes.iter().map(|m| m.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    entry.modes = ids;
    entry.notes.push(
        "Modes captured from a live session of this agent type (not part of the \
         static catalog)."
            .to_string(),
    );
}

/// Deduplicate a whole report's agents by slug, keeping first occurrences.
/// Defensive only — the production source already yields one entry per agent.
pub fn dedupe_agents(report: &mut CapabilitiesReport) {
    let mut seen: HashSet<String> = HashSet::new();
    report.agents.retain(|a| seen.insert(a.agent_type.clone()));
}

/// Outcome of checking one requested per-call selector against a capability
/// list from this catalog. This is the whole validation contract for
/// `delegate_to_agent`'s `model` / `mode` / `reasoning_level` arguments,
/// kept next to the data it rules on so the tool that reports the lists and
/// the broker that enforces them can never drift apart:
///
///   * [`SelectorCheck::Known`] — a source filled the list and the value is
///     in it: the spelling is validated, forward it.
///   * [`SelectorCheck::HardUnknown`] — a source filled the list and the
///     value is NOT in it: reject the call as a typo, handing back the
///     accepted spellings so the caller can self-correct.
///   * [`SelectorCheck::Unknown`] — no source could fill the list: pass the
///     value through as a preference the agent may apply or ignore
///     (per 5bd912ca — selectors are preferences, and drift is reported
///     rather than blocked).
///
/// An EMPTY list is always [`SelectorCheck::Unknown`], whatever its source
/// tag: it means "nothing known", never "nothing exists" — including a
/// known-source entry that simply advertised no such selector, where an
/// unadvertised `set_config_option` id may still be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorCheck {
    Known,
    HardUnknown { accepted: Vec<String> },
    Unknown,
}

/// Check one requested selector `value` against a capability `list` under the
/// three-state rule above.
pub fn check_selector(list: &[String], value: &str) -> SelectorCheck {
    if list.is_empty() {
        return SelectorCheck::Unknown;
    }
    if list.iter().any(|known| known == value) {
        return SelectorCheck::Known;
    }
    SelectorCheck::HardUnknown {
        accepted: list.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::types::{
        SessionConfigKindInfo, SessionConfigSelectInfo, SessionConfigSelectOptionInfo,
        SessionModeInfo,
    };
    use serde_json::json;

    fn select_option(id: &str, category: Option<&str>, values: &[&str]) -> SessionConfigOptionInfo {
        SessionConfigOptionInfo {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            category: category.map(str::to_string),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: values.first().unwrap_or(&"").to_string(),
                options: values
                    .iter()
                    .map(|v| SessionConfigSelectOptionInfo {
                        value: v.to_string(),
                        name: v.to_string(),
                        description: None,
                    })
                    .collect(),
                groups: Vec::new(),
            }),
            recommended_value: None,
        }
    }

    fn modes(ids: &[&str]) -> SessionModeStateInfo {
        SessionModeStateInfo {
            current_mode_id: ids.first().unwrap_or(&"").to_string(),
            available_modes: ids
                .iter()
                .map(|id| SessionModeInfo {
                    id: id.to_string(),
                    name: id.to_string(),
                    description: None,
                })
                .collect(),
        }
    }

    #[test]
    fn advertised_projection_reads_model_thought_level_and_modes() {
        let state = modes(&["default", "plan"]);
        let options = vec![
            select_option("model", Some("model"), &["m1", "m2"]),
            select_option("reasoning_effort", Some("thought_level"), &["low", "high"]),
            select_option("unrelated", Some("other"), &["x"]),
        ];
        let caps = from_advertised("codex", "Codex", Some(&state), &options);
        assert_eq!(caps.agent_type, "codex");
        assert_eq!(caps.models, vec!["m1", "m2"]);
        assert_eq!(caps.reasoning_levels, vec!["low", "high"]);
        assert_eq!(caps.modes, vec!["default", "plan"]);
        // The advertised projection also names the WIRE IDS the selector
        // preferences must be forwarded under.
        assert_eq!(caps.model_option_id.as_deref(), Some("model"));
        assert_eq!(caps.reasoning_option_id.as_deref(), Some("reasoning_effort"));
        assert_eq!(caps.source, CapabilitySource::Advertised);
        // Every advertised field was filled, so no gap notes.
        assert!(caps.notes.is_empty(), "notes: {:?}", caps.notes);
    }

    #[test]
    fn advertised_projection_notes_honest_gaps() {
        let caps = from_advertised("grok", "Grok", None, &[]);
        assert!(caps.models.is_empty());
        assert!(caps.reasoning_levels.is_empty());
        assert!(caps.modes.is_empty());
        assert_eq!(caps.source, CapabilitySource::Advertised);
        assert!(!caps.notes.is_empty());
    }

    /// Grok's selectors are SYNTHESIZED (connection.rs) with ids
    /// `model` / `reasoning_effort` and categories `model` / `mode` — the
    /// advertised projection must recognize both halves of that pair, the
    /// id-based one included, without also swallowing Grok's effort list as
    /// "modes" (modes come from `SessionModes`, which grok never emits).
    #[test]
    fn advertised_projection_matches_grok_synthesized_ids() {
        let options = vec![
            select_option(
                "model",
                Some("model"),
                &["grok-4.5", "grok-composer-2.5-fast"],
            ),
            // Grok's effort selector carries category "mode" — NOT a
            // thought_level — so only its id can identify it.
            select_option("reasoning_effort", Some("mode"), &["low", "medium", "high"]),
        ];
        let caps = from_advertised("grok", "Grok", None, &options);
        assert_eq!(caps.models.len(), 2);
        assert_eq!(caps.reasoning_levels, vec!["low", "medium", "high"]);
        assert!(caps.modes.is_empty());
    }

    #[test]
    fn codex_projection_lists_only_visible_models_and_unions_levels() {
        let catalog = vec![
            json!({
                "slug": "gpt-6-astra",
                "visibility": "list",
                "supported_reasoning_levels": [
                    {"effort": "low"}, {"effort": "high"},
                ],
            }),
            json!({
                "slug": "gpt-6-sol",
                "visibility": "list",
                "supported_reasoning_levels": [
                    {"effort": "high"}, {"effort": "xhigh"},
                ],
            }),
            json!({"slug": "codex-auto-review", "visibility": "hide"}),
            // A listable entry with no slug at all is skipped.
            json!({"visibility": "list"}),
        ];
        let caps = from_codex_catalog("codex", "Codex", &catalog);
        assert_eq!(caps.models, vec!["gpt-6-astra", "gpt-6-sol"]);
        // Union, first-seen order, deduplicated across models.
        assert_eq!(caps.reasoning_levels, vec!["low", "high", "xhigh"]);
        assert_eq!(caps.source, CapabilitySource::Static);
        assert!(caps.modes.is_empty());
        assert!(caps.notes.iter().any(|n| n.contains("PER MODEL")));
    }

    #[test]
    fn zcode_parser_extracts_models_and_variants_without_credentials() {
        // The apiKey / baseURL must never survive the parse — pin that by
        // asserting the serialized result carries none of the key's bytes.
        let secret = "sk-definitely-secret-material";
        let raw = json!({
            "provider": {
                "builtin:bigmodel-coding-plan": {
                    "name": "BigModel - Coding Plan",
                    "kind": "anthropic",
                    "options": {"apiKey": secret, "baseURL": "https://example.invalid"},
                    "enabled": true,
                    "models": {
                        "GLM-5.3": {
                            "reasoning": {"enabled": true, "variants": ["low", "high", "max"]}
                        },
                        "GLM-5.2": {}
                    }
                },
                "builtin:disabled-provider": {
                    "enabled": false,
                    "models": {"Ghost-Model": {}}
                }
            }
        })
        .to_string();
        let catalog = parse_zcode_provider_config(&raw).expect("parses");
        assert_eq!(catalog.models, vec!["GLM-5.3", "GLM-5.2"]);
        assert_eq!(catalog.reasoning_variants, vec!["low", "high", "max"]);
        let rendered =
            serde_json::to_string(&from_zcode_provider_catalog("zcode", "ZCode", &catalog))
                .unwrap();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("apiKey"));
        assert!(!rendered.contains("baseURL"));
    }

    #[test]
    fn zcode_parser_rejects_documents_without_usable_providers() {
        assert!(parse_zcode_provider_config("not json").is_none());
        assert!(parse_zcode_provider_config(r#"{"provider": "nope"}"#).is_none());
        assert!(parse_zcode_provider_config(r#"{"provider": {}}"#).is_none());
        // Enabled provider with no models table → nothing usable.
        assert!(parse_zcode_provider_config(r#"{"provider": {"a": {"enabled": true}}}"#).is_none());
        // Only a disabled provider's models → nothing usable.
        assert!(parse_zcode_provider_config(
            r#"{"provider": {"a": {"enabled": false, "models": {"m": {}}}}}"#
        )
        .is_none());
    }

    #[test]
    fn codex_catalog_models_prefers_the_configured_replacement_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let catalog = json!({"models": [{"slug": "custom-1", "visibility": "list"}]});
        std::fs::write(
            home.join("codeg-model-catalog.json"),
            serde_json::to_string(&catalog).unwrap(),
        )
        .unwrap();
        std::fs::write(
            home.join("config.toml"),
            "model_catalog_json = \"codeg-model-catalog.json\"\n",
        )
        .unwrap();
        let (models, note) = codex_catalog_models(home);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["slug"], "custom-1");
        assert!(note
            .as_deref()
            .unwrap()
            .contains("codeg-model-catalog.json"));
    }

    #[test]
    fn codex_catalog_models_falls_back_to_the_bundled_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml at all → the snapshot chain answers, never empty.
        let (models, note) = codex_catalog_models(dir.path());
        assert!(!models.is_empty());
        assert!(note.as_deref().unwrap().contains("bundled catalog"));
    }

    #[test]
    fn unknown_entry_is_empty_with_explanatory_notes() {
        let caps = AgentCapabilities::unknown("gemini", "Gemini CLI");
        assert_eq!(caps.source, CapabilitySource::Unknown);
        assert!(
            caps.models.is_empty() && caps.modes.is_empty() && caps.reasoning_levels.is_empty()
        );
        assert!(caps.notes.iter().any(|n| n.contains("unknown")));
        let v = serde_json::to_value(&caps).unwrap();
        assert_eq!(v["agent_type"], "gemini");
        assert_eq!(v["source"], "unknown");
        // Lists always serialize, so an LLM never meets a missing field.
        assert!(v["models"].is_array());
    }

    #[test]
    fn merge_advertised_modes_fills_only_the_modes_field() {
        let mut caps = from_codex_catalog(
            "codex",
            "Codex",
            &[json!({"slug": "m1", "visibility": "list"})],
        );
        merge_advertised_modes(&mut caps, &modes(&["default", "full-auto"]));
        assert_eq!(caps.modes, vec!["default", "full-auto"]);
        assert_eq!(caps.models, vec!["m1"]);
        assert_eq!(caps.source, CapabilitySource::Static);
        assert!(caps.notes.iter().any(|n| n.contains("live session")));
        // An empty mode catalog fills nothing and notes nothing.
        let mut bare = AgentCapabilities::unknown("codex", "Codex");
        merge_advertised_modes(&mut bare, &modes(&[]));
        assert!(bare.modes.is_empty());
        assert_eq!(bare.notes.len(), 2);
    }

    #[test]
    fn report_serializes_with_envelope_note_only_when_set() {
        let report = CapabilitiesReport {
            agents: vec![AgentCapabilities::unknown("codex", "Codex")],
            note: None,
        };
        let v = serde_json::to_value(&report).unwrap();
        assert!(v.get("note").is_none());
        assert_eq!(v["agents"].as_array().unwrap().len(), 1);
    }

    /// Static projections know model VALUES, not the agent's config-option
    /// wire ids — the option-id fields stay `None` so callers fall back to
    /// the conventional spellings instead of trusting a guessed id.
    #[test]
    fn static_projections_leave_option_ids_unset() {
        let codex = from_codex_catalog(
            "codex",
            "Codex",
            &[json!({"slug": "m1", "visibility": "list"})],
        );
        assert!(codex.model_option_id.is_none());
        assert!(codex.reasoning_option_id.is_none());
        let zcode = from_zcode_provider_catalog(
            "zcode",
            "ZCode",
            &ZcodeProviderCatalog {
                models: vec!["GLM-5.3".into()],
                reasoning_variants: vec!["high".into()],
            },
        );
        assert!(zcode.model_option_id.is_none());
        assert!(zcode.reasoning_option_id.is_none());
        // ... and they deserialize back without the fields (serde default).
        let raw = serde_json::to_string(&codex).unwrap();
        let back: AgentCapabilities = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.model_option_id, None);
    }

    #[test]
    fn check_selector_is_the_three_state_rule() {
        let list = vec!["m1".to_string(), "m2".to_string()];
        assert_eq!(check_selector(&list, "m1"), SelectorCheck::Known);
        // A known list that does not contain the value is a HARD reject
        // carrying the accepted spellings.
        assert_eq!(
            check_selector(&list, "mX"),
            SelectorCheck::HardUnknown {
                accepted: list.clone()
            }
        );
        // Empty lists — unknown source, or a source that advertised no such
        // selector — always pass through as preferences.
        assert_eq!(check_selector(&[], "anything"), SelectorCheck::Unknown);
    }
}
