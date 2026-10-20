//! Model capabilities, shared with the GUI and CLI. Never infer effort from thinking text.
use serde_json::{json, Value};

/// An explicitly empty preference means provider default, even when env is set.
pub fn preference(stored: Option<String>, environment: Option<String>) -> Option<String> {
    stored
        .or(environment)
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty() && value != "auto")
}

pub fn levels(model: &str, _api: &str, metadata: Option<&Value>) -> Vec<String> {
    const VALID: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    if let Some(reported) = metadata
        .and_then(|m| {
            m.get("reasoning_effort")
                .or_else(|| m.pointer("/capabilities/effort"))
        })
        .and_then(Value::as_object)
    {
        if reported.get("supported").and_then(Value::as_bool) == Some(false) {
            return vec![];
        }
        if let Some(values) = reported.get("levels").and_then(Value::as_array) {
            return VALID
                .iter()
                .filter(|level| values.iter().any(|v| v.as_str() == Some(**level)))
                .map(|v| v.to_string())
                .collect();
        }
        if VALID.iter().any(|level| reported.contains_key(*level)) {
            return VALID
                .iter()
                .filter(|level| {
                    reported.get(**level).is_some_and(|v| {
                        v.as_bool() == Some(true) || v["supported"].as_bool() == Some(true)
                    })
                })
                .map(|v| v.to_string())
                .collect();
        }
    }
    let capability = metadata.and_then(|m| {
        m.get("reasoning_efforts")
            .or_else(|| m.pointer("/capabilities/reasoning_effort"))
    });
    if let Some(values) = capability.and_then(Value::as_array) {
        return VALID
            .iter()
            .filter(|level| values.iter().any(|v| v.as_str() == Some(**level)))
            .map(|v| v.to_string())
            .collect();
    }
    if capability == Some(&Value::Bool(false)) {
        return vec![];
    }
    let parameters = metadata
        .and_then(|m| m.get("supported_parameters"))
        .and_then(Value::as_array);
    let explicit = parameters.map(|params| {
        params.iter().any(|p| {
            matches!(
                p.as_str(),
                Some("reasoning_effort" | "reasoning.effort" | "output_config.effort")
            )
        })
    });
    if explicit == Some(false) {
        return vec![];
    }
    let explicitly_supported = capability == Some(&Value::Bool(true)) || explicit == Some(true);
    let name = model.rsplit('/').next().unwrap_or(model).to_lowercase();
    let name = if name.starts_with("claude-") {
        name.replace('.', "-")
    } else {
        name
    };
    static RULES: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    let rules = RULES.get_or_init(|| {
        serde_json::from_str(include_str!("reasoning-models.json")).unwrap()
    });
    rules
        .iter()
        .filter(|rule| {
            let id = rule["id"].as_str().unwrap();
            let suffix = name.strip_prefix(id).unwrap_or("?");
            let date = suffix.strip_prefix('-').unwrap_or("").replace('-', "");
            name == id || (date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
        })
        .max_by_key(|rule| rule["id"].as_str().unwrap().len())
        .map(|rule| {
            rule["levels"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            if explicitly_supported {
                ["low", "medium", "high"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            } else {
                vec![]
            }
        })
}

pub fn apply(body: &mut Value, api: &str, allowed: &[String], effort: Option<&str>) {
    let Some(effort) = effort.filter(|e| allowed.iter().any(|level| level == e)) else {
        return;
    };
    match api {
        "anthropic_messages" => body["output_config"] = json!({"effort":effort}),
        "openai_responses" => body["reasoning"] = json!({"effort":effort}),
        "chat_completions" => body["reasoning_effort"] = json!(effort),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_preference_survives_reload_and_overrides_env() {
        assert_eq!(preference(Some("".into()), Some("high".into())), None);
        assert_eq!(preference(Some("auto".into()), Some("high".into())), None);
        assert_eq!(preference(None, None), None);
        assert_eq!(preference(None, Some(" LOW ".into())), Some("low".into()));
    }
    #[test]
    fn capabilities_belong_to_models() {
        for name in [
            "foo1",
            "my-reasoner",
            "deepseek-r1",
            "claude-3-7-sonnet",
            "gpt-5-chat-latest",
            "gpt-5-future",
        ] {
            assert!(levels(name, "chat_completions", None).is_empty(), "{name}");
        }
        assert_eq!(
            levels("openai/o3-mini-2025-01-31", "chat_completions", None),
            ["low", "medium", "high"]
        );
        assert_eq!(levels("gpt-5-pro", "openai_responses", None), ["high"]);
        assert_eq!(
            levels("claude-opus-4-6", "chat_completions", None),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(
            levels("claude-opus-4-6", "anthropic_messages", None),
            ["low", "medium", "high", "max"]
        );
        assert!(levels(
            "o3",
            "chat_completions",
            Some(&json!({"capabilities":{"reasoning_effort":false}}))
        )
        .is_empty());
        assert_eq!(
            levels(
                "custom",
                "chat_completions",
                Some(&json!({"reasoning_efforts":["high","low","bogus"]}))
            ),
            ["low", "high"]
        );
    }
    #[test]
    fn claude_aliases_work_across_all_transports_and_metadata_wins() {
        for api in ["chat_completions", "openai_responses", "anthropic_messages"] {
            for model in ["cc/claude-opus-5-5", "anthropic/claude-opus-5.5"] {
                assert_eq!(
                    levels(model, api, None),
                    ["low", "medium", "high", "xhigh", "max"]
                );
                assert!(levels(
                    model,
                    api,
                    Some(&json!({"reasoning_effort":{"supported":false}}))
                )
                .is_empty());
                assert_eq!(
                    levels(
                        model,
                        api,
                        Some(&json!({"supported_parameters":["reasoning_effort"]}))
                    ),
                    ["low", "medium", "high", "xhigh", "max"]
                );
                let mut body = json!({});
                apply(&mut body, api, &levels(model, api, None), Some("max"));
                let path = match api {
                    "chat_completions" => "/reasoning_effort",
                    "openai_responses" => "/reasoning/effort",
                    _ => "/output_config/effort",
                };
                assert_eq!(body.pointer(path), Some(&json!("max")));
            }
            assert_eq!(
                levels(
                    "custom",
                    api,
                    Some(
                        &json!({"capabilities":{"effort":{"supported":true,"low":{"supported":true},"high":{"supported":false},"max":{"supported":true}}}})
                    )
                ),
                ["low", "max"]
            );
            assert_eq!(
                levels(
                    "custom",
                    api,
                    Some(&json!({"reasoning_effort":{"supported":true,"levels":["high","max"]}}))
                ),
                ["high", "max"]
            );
        }
    }
    #[test]
    fn wire_omits_unsupported_and_stale_values() {
        for api in ["chat_completions", "openai_responses", "anthropic_messages"] {
            let mut body = json!({});
            apply(&mut body, api, &[], Some("high"));
            assert_eq!(body, json!({}));
            apply(&mut body, api, &["low".into()], Some("high"));
            assert_eq!(body, json!({}));
            apply(&mut body, api, &["low".into()], Some("low"));
            let path = match api {
                "chat_completions" => "/reasoning_effort",
                "openai_responses" => "/reasoning/effort",
                _ => "/output_config/effort",
            };
            assert_eq!(body.pointer(path), Some(&json!("low")));
        }
    }
}
