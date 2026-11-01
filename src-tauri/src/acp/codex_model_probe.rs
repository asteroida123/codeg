//! Pre-flight check that the model a Codex launch will actually use is one
//! its configured gateway serves.
//!
//! The board's 2026-10-31 R31-09 failure looked like a codeg bug and was a
//! config inconsistency: `~/.codex/config.toml` pinned `model =
//! "gpt-6-astra"` against a custom gateway that does not serve it, so the
//! first spawned run died on its opening request — `unexpected status 404
//! Not Found: Model "gpt-6-astra" is not supported by any configured account`
//! — 15 seconds into a run, with a cryptic card. Neither side can see the
//! inconsistency offline: the slug is a legitimate official entry in codex's
//! own catalog, and only the gateway knows what it serves.
//!
//! So the probe asks the gateway: `GET {base_url}/models` (the OpenAI-compat
//! listing every gateway of this shape implements), and blocks the launch
//! ONLY on a positive "list retrieved, model absent" answer. A missing
//! `/models` endpoint, a timeout, a parse failure, or an official openai
//! endpoint (no custom base URL) all read as [`ProbeOutcome::Unknown`] and
//! let the launch proceed — the run then fails the way it always did, but
//! nothing that could be a gateway quirk ever blocks a legitimate model.
//!
//! Results are cached per `(base_url, token, model)` for a few minutes so a
//! fan-out of cards does not hammer the gateway.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long a probe answer is trusted. Short, because the interesting
/// transition is a human FIXING the gateway or the default — the next card
/// should re-probe within minutes, not hours.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Budget for one `GET /models`. The probe runs inside card admission, so a
/// dead gateway must cost seconds, not a hang.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

static PROBE_HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .expect("static probe client builds")
});

type CacheKey = (String, String, String);
static CACHE: LazyLock<Mutex<HashMap<CacheKey, (Instant, ProbeOutcome)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// What the probe learned about a launch's model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Nothing contradicts the launch (model listed, nothing to check, or the
    /// gateway could not be asked). Includes the source of the "nothing to
    /// check", for logging.
    Proceed(&'static str),
    /// The gateway answered and does not serve this model. Block the launch
    /// with the actionable message; carrying the pieces lets the caller
    /// compose it.
    Unlisted {
        model: String,
        base_url: String,
    },
}

/// The model a Codex launch will actually use: the launch env's `OPENAI_MODEL`
/// (codeg writes the caller's request there; codex reads it before anything
/// else), else the root `model` of `~/.codex/config.toml`, else codex's own
/// catalog default (nothing to check). Pure; the toml text is passed in so the
/// resolution is unit-testable.
pub fn resolve_launch_model(
    runtime_env: &BTreeMap<String, String>,
    codex_config_toml: Option<&str>,
) -> Option<String> {
    if let Some(model) = runtime_env
        .get("OPENAI_MODEL")
        .map(String::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        return Some(model.to_string());
    }
    let raw = codex_config_toml?.trim();
    let table = raw.parse::<toml::Value>().ok()?;
    table
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// The endpoint + bearer the launch's model requests will go to: the launch
/// env's provider trio first (a codeg model-provider binding writes it), else
/// the `[model_providers.<root model_provider>]` table of `~/.codex/config.toml`.
/// `None` when the launch targets the official OpenAI endpoint — the codex
/// catalog governs there and the probe has nothing to add. Pure.
pub fn resolve_provider(
    runtime_env: &BTreeMap<String, String>,
    codex_config_toml: Option<&str>,
) -> Option<(String, Option<String>)> {
    let env_base = runtime_env
        .get("OPENAI_BASE_URL")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let env_key = runtime_env
        .get("OPENAI_API_KEY")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let Some(base) = env_base {
        return Some((base.to_string(), env_key.map(str::to_string)));
    }
    // No env provider: read codex's own table. An absent root `model_provider`
    // (or `model_provider = "openai"`) means the official endpoint.
    let raw = codex_config_toml?.trim();
    let table = raw.parse::<toml::Value>().ok()?;
    let provider_id = table
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "openai")?;
    let providers = table.get("model_providers")?.as_table()?;
    let entry = providers.get(provider_id)?.as_table()?;
    let base = entry
        .get("base_url")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    let token = entry
        .get("experimental_bearer_token")
        .or_else(|| entry.get("env_key"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    Some((base, token))
}

/// Pull `data[].id` out of an OpenAI-compat `/models` body. Pure.
pub fn parse_model_ids(body: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let ids = value
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    // An empty list means "endpoint exists but said nothing" — treat as no
    // answer, not as "serves nothing", so it can never block.
    if ids.is_empty() {
        return None;
    }
    Some(ids)
}

/// Ask whether `base_url` serves `model`. `None` = could not ask (network,
/// status, shape) — the caller treats that as proceed. Cached by the caller.
async fn gateway_lists_model(base_url: &str, token: Option<&str>, model: &str) -> Option<bool> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = PROBE_HTTP.get(&url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let ids = parse_model_ids(&body)?;
    Some(ids.iter().any(|id| id == model))
}

/// Pre-flight a Codex launch's model against the gateway it will use.
/// Blocking only on a positive "gateway says no"; see the module docs.
pub async fn check_codex_model_available(
    runtime_env: &BTreeMap<String, String>,
) -> ProbeOutcome {
    let codex_config = crate::commands::acp::load_codex_config_toml_raw();
    let Some(model) = resolve_launch_model(runtime_env, codex_config.as_deref()) else {
        return ProbeOutcome::Proceed("no explicit model to check");
    };
    let Some((base_url, token)) = resolve_provider(runtime_env, codex_config.as_deref()) else {
        return ProbeOutcome::Proceed("official endpoint — codex's catalog governs");
    };
    let key: CacheKey = (base_url.clone(), token.clone().unwrap_or_default(), model.clone());
    if let Ok(cache) = CACHE.lock() {
        if let Some((at, outcome)) = cache.get(&key) {
            if at.elapsed() < CACHE_TTL {
                return outcome.clone();
            }
        }
    }
    let outcome = match gateway_lists_model(&base_url, token.as_deref(), &model).await {
        Some(true) => ProbeOutcome::Proceed("model listed by the gateway"),
        Some(false) => ProbeOutcome::Unlisted { model, base_url },
        None => ProbeOutcome::Proceed("gateway could not be asked"),
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, (Instant::now(), outcome.clone()));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const CONFIG: &str = r#"
model = "gpt-6-astra"
model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = false
base_url = "https://gw.example/"
experimental_bearer_token = "sk-test"
"#;

    #[test]
    fn launch_model_resolves_env_first_then_toml_root() {
        // The env override (a card's model, after apply_requested_model_env)
        // outranks the config default — the exact retry shape of R31-09.
        assert_eq!(
            resolve_launch_model(&env(&[("OPENAI_MODEL", "gpt-6.1-sol")]), Some(CONFIG)),
            Some("gpt-6.1-sol".to_string())
        );
        // No env: the config root model — the value that 404'd.
        assert_eq!(
            resolve_launch_model(&BTreeMap::new(), Some(CONFIG)),
            Some("gpt-6-astra".to_string())
        );
        // Neither, or a config without a root model: nothing to check.
        assert_eq!(resolve_launch_model(&BTreeMap::new(), None), None);
        assert_eq!(
            resolve_launch_model(&BTreeMap::new(), Some("model_provider = \"custom\"")),
            None
        );
    }

    #[test]
    fn provider_resolves_env_trio_then_toml_table() {
        assert_eq!(
            resolve_provider(&env(&[("OPENAI_BASE_URL", "https://env.example")]), Some(CONFIG)),
            Some(("https://env.example".to_string(), None))
        );
        assert_eq!(
            resolve_provider(
                &env(&[
                    ("OPENAI_BASE_URL", "https://env.example"),
                    ("OPENAI_API_KEY", "sk-env"),
                ]),
                Some(CONFIG)
            ),
            Some(("https://env.example".to_string(), Some("sk-env".to_string())))
        );
        assert_eq!(
            resolve_provider(&BTreeMap::new(), Some(CONFIG)),
            Some(("https://gw.example/".to_string(), Some("sk-test".to_string())))
        );
        // Official endpoint (no custom provider): nothing to probe.
        assert_eq!(resolve_provider(&BTreeMap::new(), None), None);
        assert_eq!(
            resolve_provider(&BTreeMap::new(), Some("model = \"gpt-5\"\n")),
            None
        );
    }

    #[test]
    fn model_id_parsing_reads_data_ids_and_rejects_empty() {
        let body = r#"{"object":"list","data":[{"id":"gpt-6.1-sol"},{"id":"gpt-6-astra"}]}"#;
        assert_eq!(
            parse_model_ids(body),
            Some(vec!["gpt-6.1-sol".to_string(), "gpt-6-astra".to_string()])
        );
        // An endpoint that answers with no models is no answer at all — it
        // must never be read as "serves nothing".
        assert_eq!(parse_model_ids(r#"{"data":[]}"#), None);
        assert_eq!(parse_model_ids("not json"), None);
        assert_eq!(parse_model_ids(r#"{"data":{}}"#), None);
    }

    #[tokio::test]
    async fn an_unreachable_gateway_proceeds_without_blocking() {
        // A port nothing listens on inside the probe budget: Unknown, never a
        // block. (Caches the Proceed, so also exercises the cache arm.)
        let env = env(&[
            ("OPENAI_BASE_URL", "http://127.0.0.1:9"),
            ("OPENAI_API_KEY", "sk-x"),
        ]);
        match check_codex_model_available(&env).await {
            ProbeOutcome::Proceed(_) => {}
            other => panic!("unreachable gateway must proceed, got {other:?}"),
        }
    }
}
