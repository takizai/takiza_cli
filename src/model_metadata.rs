//! Context limits reported by the selected provider and deployment only.
use crate::{config::Config, llm::LlmClient};
use serde_json::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::{collections::HashMap, sync::{Mutex, OnceLock}};

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key { endpoint: String, model: String, api_key: String, proxy: Option<String> }
impl Key {
    fn from_config(config: &Config) -> Self {
        Self { endpoint: crate::config::normalize_base_url(&config.base_url), model: config.model.clone(),
            api_key: config.api_key.clone(), proxy: config.proxy.clone() }
    }
    fn cache_id(&self) -> String {
        // Include credentials and routing in the scope without writing either to disk.
        let scope = serde_json::to_vec(&(&self.endpoint, &self.model, &self.api_key, &self.proxy)).unwrap();
        format!("{:x}", Sha256::digest(scope))
    }
}
const LEARNED_TTL: u64 = 7 * 24 * 60 * 60;
#[derive(Serialize, Deserialize)]
struct LearnedLimit { tokens: u64, observed_at: u64 }
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn cache_path(key: &Key) -> PathBuf {
    crate::theme::UserPreferences::config_path().with_file_name("context-limits").join(format!("{}.json", key.cache_id()))
}
fn read_learned(path: &Path, time: u64) -> Option<u64> {
    let record: LearnedLimit = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (record.tokens > 0 && record.observed_at <= time && time - record.observed_at < LEARNED_TTL)
        .then_some(record.tokens)
}
fn write_learned(path: &Path, tokens: u64, time: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let bytes = serde_json::to_vec(&LearnedLimit { tokens, observed_at: time })?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, path)
}
#[derive(Default)]
struct State {
    current: Option<Key>,
    // Missing entry = not requested; None = loading; Some(None) = unavailable.
    entries: HashMap<Key, Option<Option<u64>>>,
    learned: HashMap<Key, u64>,
    changed: bool,
}
impl State {
    fn limit(&self, key: &Key) -> Option<u64> {
        self.entries.get(key).copied().flatten().flatten().into_iter()
            .chain(self.learned.get(key).copied()).min()
    }
    fn observe(&mut self, key: Key, limit: u64) {
        self.learned.insert(key.clone(), limit);
        if self.current.as_ref() == Some(&key) { self.changed = true; }
    }
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Start one background lookup per model/provider/credential combination.
pub fn ensure(config: &Config) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return; };
    let key = Key::from_config(config);
    {
        let mut state = state().lock().unwrap();
        state.current = Some(key.clone());
        if state.entries.contains_key(&key) { return; }
        if !state.learned.contains_key(&key) {
            if let Some(limit) = read_learned(&cache_path(&key), now()) { state.observe(key.clone(), limit); }
        }
        state.entries.insert(key.clone(), None);
    }
    let config = config.clone();
    runtime.spawn(async move {
        // Includes connection, body reading and the optional detail lookup.
        let limit = tokio::time::timeout(std::time::Duration::from_secs(8),
            LlmClient::new(config).model_context_limit()).await.ok().flatten();
        let mut state = state().lock().unwrap();
        state.entries.insert(key.clone(), Some(limit));
        if state.current.as_ref() == Some(&key) { state.changed = true; }
    });
}

pub fn take_changed() -> bool { std::mem::take(&mut state().lock().unwrap().changed) }

pub fn current_limit() -> Option<u64> {
    let state = state().lock().unwrap();
    state.current.as_ref().and_then(|key| state.limit(key))
}
pub fn limit_for(config: &Config) -> Option<u64> {
    state().lock().unwrap().limit(&Key::from_config(config))
}

/// Only called for an actual provider overflow response, never for compaction itself.
pub fn remember_overflow(config: &Config, body: &str) {
    let Some(limit) = parse_overflow_limit(body) else { return; };
    let key = Key::from_config(config);
    let mut state = state().lock().unwrap();
    state.observe(key.clone(), limit);
    // A read-only config directory must not prevent compaction and retry.
    let _ = write_learned(&cache_path(&key), limit, now());
}

/// Extract only an explicit total context cap. Input/output counts, requested
/// totals and generic "too long" messages are not evidence of a window size.
pub fn parse_overflow_limit(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok();
    let error = value.as_ref().and_then(|value| value.get("error")).or(value.as_ref());
    let structured = error.and_then(object_limit);
    let message = error.and_then(|error| error.get("message")).and_then(Value::as_str)
        .or_else(|| error.and_then(Value::as_str)).unwrap_or(body).to_lowercase();
    let mut limits = Vec::new();
    for prefix in ["maximum context length is", "maximum context length of", "maximum context length:",
        "maximum context window is", "maximum context window of", "maximum context window:",
        "context window limit is", "context length limit is"] {
        for (offset, _) in message.match_indices(prefix) {
            let rest = message[offset + prefix.len()..].trim_start();
            let end = rest.find(|c: char| !c.is_ascii_digit() && c != ',' && c != '_').unwrap_or(rest.len());
            if end == 0 || !rest[end..].trim_start().starts_with("tokens") { continue; }
            if let Ok(limit) = rest[..end].replace([',', '_'], "").parse::<u64>() {
                if limit > 0 { limits.push(limit); }
            }
        }
    }
    structured.into_iter().chain(limits).min()
}

fn positive_integer(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok()).filter(|n| *n > 0)
}

fn object_limit(value: &Value) -> Option<u64> {
    let direct = ["context_length", "context_window", "max_context_length", "max_context_window", "max_context_tokens", "max_model_len",
        "contextLength", "contextWindow", "maxContextLength"]
        .iter().filter_map(|field| value.get(field).and_then(positive_integer)).min();
    let nested = value.get("limits").and_then(|limits| ["context", "context_length", "context_window"]
        .iter().filter_map(|field| limits.get(field).and_then(positive_integer)).min());
    direct.into_iter().chain(nested).min()
}

/// Deployment metadata is global on some gateways. Only use a record explicitly
/// identifying the requested model; never take another model's or a trained cap.
pub fn parse_deployment_context_limit(response: &Value, model: &str) -> Option<u64> {
    fn record_limit(record: &Value, model: &str) -> Option<u64> {
        if !["id", "name", "model", "modelName", "model_name", "served_model_name"].iter()
            .any(|field| record.get(field).and_then(Value::as_str) == Some(model)) { return None; }
        object_limit(record).into_iter().chain(
            ["runtime", "engine"].iter().filter_map(|field| record.get(field).and_then(object_limit)))
            .min()
    }
    let mut limits = Vec::new();
    if let Some(limit) = record_limit(response, model) { limits.push(limit); }
    for field in ["modelInfo", "model_info", "models", "data"] {
        if let Some(records) = response.get(field) {
            if let Some(records) = records.as_array() {
                limits.extend(records.iter().filter_map(|record| record_limit(record, model)));
            } else if let Some(limit) = record_limit(records, model) { limits.push(limit); }
        }
    }
    limits.into_iter().min()
}

pub fn parse_context_limit(response: &Value, model: &str) -> Option<u64> {
    let models = response.get("data").and_then(Value::as_array)
        .or_else(|| response.get("models").and_then(Value::as_array))
        .or_else(|| response.as_array());
    let record = if let Some(models) = models {
        models.iter().find(|record| ["id", "name", "model"].iter()
            .any(|field| record.get(field).and_then(Value::as_str) == Some(model)))?
    } else {
        let record = response.get("data").filter(|data| data.is_object()).unwrap_or(response);
        // A detail endpoint can omit the id, but never accept a different model.
        if ["id", "name", "model"].iter().filter_map(|field| record.get(field).and_then(Value::as_str))
            .next().is_some_and(|id| id != model) { return None; }
        record
    };
    let limit = object_limit(record);
    let provider_limit = record.get("top_provider").and_then(object_limit);
    match (limit, provider_limit) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn format_limit(tokens: u64) -> String {
    // Providers commonly report either binary-sized or decimal-sized windows.
    let base = if !tokens.is_multiple_of(1000) && tokens.is_multiple_of(1024) { 1024u64 } else { 1000u64 };
    let (unit, suffix) = if tokens >= base * base { (base * base, "M") }
        else if tokens >= base { (base, "K") } else { return tokens.to_string(); };
    if tokens.is_multiple_of(unit) { format!("{}{suffix}", tokens / unit) }
    else { format!("{:.1}{suffix}", tokens as f64 / unit as f64) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn overflow_requires_an_explicit_context_cap() {
        for (body, limit) in [
            ("This model's maximum context length is 131072 tokens. However, you requested 140000 tokens.", 131072),
            ("Maximum context length is 131,072 tokens", 131072),
            ("maximum context length of 1_000_000 tokens", 1000000),
            ("Maximum context window: 2000000 tokens", 2000000),
            (r#"{"error":{"code":"context_length_exceeded","message":"too long","max_model_len":64000}}"#, 64000),
            (r#"{"error":{"message":"maximum context length is 128000 tokens; requested 145000","context_length":64000}}"#, 64000),
        ] { assert_eq!(parse_overflow_limit(body), Some(limit), "{body}"); }
        for body in ["context_length_exceeded", "input is too long: 140000 > 128000",
            "maximum context length is unknown; requested 140000 tokens", "maximum context length is 0 tokens",
            "maximum context length is 128K tokens", "maximum context length is -1 tokens",
            "requested 140000 tokens: 131808 input + 8192 output",
            r#"{"error":{"message":"prompt too long","max_tokens":8192,"input_tokens":140000}}"#] {
            assert_eq!(parse_overflow_limit(body), None, "{body}");
        }
    }

    #[test]
    fn learned_caps_survive_metadata_races_and_are_scoped() {
        let key = Key { endpoint: "https://provider/v1".into(), model: "test".into(),
            api_key: "secret".into(), proxy: None };
        let mut state = State { current: Some(key.clone()), ..State::default() };
        state.entries.insert(key.clone(), None);
        state.observe(key.clone(), 64000);
        assert!(state.changed);
        assert_eq!(state.limit(&key), Some(64000));
        state.entries.insert(key.clone(), Some(Some(1000000)));
        assert_eq!(state.limit(&key), Some(64000));
        state.entries.insert(key.clone(), Some(None));
        assert_eq!(state.limit(&key), Some(64000));
        state.entries.insert(key.clone(), Some(Some(32000)));
        assert_eq!(state.limit(&key), Some(32000));
        for other in [Key { model: "other".into(), ..key.clone() },
            Key { endpoint: "https://other/v1".into(), ..key.clone() },
            Key { api_key: "other".into(), ..key.clone() },
            Key { proxy: Some("http://proxy".into()), ..key.clone() }] {
            assert_ne!(key.cache_id(), other.cache_id());
            assert_eq!(state.limit(&other), None);
        }
    }

    #[test]
    fn learned_cache_expires_and_ignores_corrupt_records() {
        let path = std::env::temp_dir().join(format!("takiza-learned-limit-{}.json", std::process::id()));
        write_learned(&path, 131072, 100).unwrap();
        assert_eq!(read_learned(&path, 100), Some(131072));
        assert_eq!(read_learned(&path, 100 + LEARNED_TTL - 1), Some(131072));
        assert_eq!(read_learned(&path, 100 + LEARNED_TTL), None);
        assert_eq!(read_learned(&path, 99), None);
        write_learned(&path, 64000, 200).unwrap();
        assert_eq!(read_learned(&path, 200), Some(64000));
        write_learned(&path, 0, 200).unwrap();
        assert_eq!(read_learned(&path, 200), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_learned(&path, 200), None);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn uses_runtime_cap_for_the_selected_deployment_only() {
        let metadata = json!({"modelInfo":[
            {"modelName":"other","max_model_len":2000000},
            {"modelName":"selected","context_length":1000000,"runtime":{"max_model_len":64000}}
        ]});
        assert_eq!(parse_deployment_context_limit(&metadata,"selected"),Some(64000));
        assert_eq!(parse_deployment_context_limit(&metadata,"missing"),None);
        assert_eq!(parse_deployment_context_limit(&json!({"max_model_len":1000000}),"selected"),None);
        assert_eq!(parse_context_limit(&json!({"id":"selected","context_length":1000000,"max_model_len":64000}),"selected"),Some(64000));
        assert_eq!(parse_context_limit(&json!({"id":"selected","context_length":1000000,"limits":{"context":32000}}),"selected"),Some(32000));
        assert_eq!(parse_deployment_context_limit(&json!({"modelName":"selected","config":{"max_position_embeddings":1000000}}),"selected"),None);
    }
    #[test]
    fn reads_only_the_selected_models_context_not_output_limits() {
        assert_eq!(parse_context_limit(&json!({"data":[
            {"id":"other","context_length":2000000},
            {"id":"selected","context_window":131072,"max_completion_tokens":8192}
        ]}), "selected"), Some(131072));
        assert_eq!(parse_context_limit(&json!({"data":[{"id":"selected","context_length":"1000000"}]}), "selected"), Some(1000000));
        assert_eq!(parse_context_limit(&json!({"data":{"id":"selected","context_length":2000000,"top_provider":{"context_length":128000}}}), "selected"), Some(128000));
        assert_eq!(parse_context_limit(&json!({"id":"selected","limits":{"context":1000000,"output":8192}}), "selected"), Some(1000000));
        assert_eq!(parse_context_limit(&json!({"id":"selected","maxContextLength":128000}), "selected"), Some(128000));
        for response in [json!({"data":[{"id":"other","context_length":1000000}]}),
            json!({"id":"selected","max_tokens":8192}), json!({"context_length":0}),
            json!({"context_length":-1}), json!({"context_length":"invalid"}),
            json!({"id":"other","context_length":1000000})] {
            assert_eq!(parse_context_limit(&response,"selected"), None);
        }
    }
    #[test]
    fn formats_decimal_and_binary_windows() {
        for (value, label) in [(128000,"128K"),(131072,"128K"),(1000000,"1M"),
            (1048576,"1M"),(2000000,"2M"),(2097152,"2M"),(200000,"200K"),
            (1500000,"1.5M"),(512,"512")] { assert_eq!(format_limit(value), label); }
    }
}
