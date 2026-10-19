//! Saved connections and provider-scoped model catalogs.
use crate::{config::Config, theme::UserPreferences};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct ModelCatalogAuthRequired(pub u16);
impl std::fmt::Display for ModelCatalogAuthRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Model catalog requires authorization (HTTP {})", self.0)
    }
}
impl std::error::Error for ModelCatalogAuthRequired {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    #[default]
    ChatCompletions,
    OpenaiResponses,
    AnthropicMessages,
}
impl ApiType {
    pub const ALL: [Self; 3] = [
        Self::ChatCompletions,
        Self::OpenaiResponses,
        Self::AnthropicMessages,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::ChatCompletions => "OpenAI Chat Completions",
            Self::OpenaiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }
    pub fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::OpenaiResponses => "responses",
            Self::AnthropicMessages => "messages",
        }
    }
    pub fn default_url(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "https://api.anthropic.com/v1",
            _ => "https://api.openai.com/v1",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}
impl Model {
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

// Credentials deliberately excluded from Debug output.
#[derive(Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    #[serde(default)]
    pub api_type: ApiType,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub models: Vec<Model>,
}
impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.name)
            .field("api_type", &self.api_type)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}
impl Provider {
    pub fn key(&self) -> String {
        match &self.api_key_env {
            Some(name) => std::env::var(name).unwrap_or_default(),
            None => self.api_key.clone(),
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.name.trim().is_empty(), "Provider name is required");
        let url = reqwest::Url::parse(&self.base_url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Use an HTTP(S) base URL without credentials, query or fragment"
        );
        for (name, value) in &self.headers {
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
            reqwest::header::HeaderValue::from_str(value)?;
        }
        Ok(())
    }
}
impl UserPreferences {
    /// Migration is explicit and uses the resolved connection (including env settings).
    pub fn migrate_provider(&mut self, config: &Config) {
        if !self.providers.is_empty() {
            return;
        }
        let id = "default".to_string();
        let key_env = if self.api_key.is_none() && !config.api_key.is_empty() {
            [
                "OPENAI_API_KEY",
                "GROQ_API_KEY",
                "OPENROUTER_API_KEY",
                "DEEPSEEK_API_KEY",
            ]
            .into_iter()
            .find(|name| std::env::var(name).ok().as_deref() == Some(&config.api_key))
            .map(str::to_string)
        } else {
            None
        };
        self.providers.insert(
            id.clone(),
            Provider {
                name: self.provider.clone().unwrap_or_else(|| "Default".into()),
                api_type: config.api_type,
                base_url: config.base_url.clone(),
                api_key: if key_env.is_some() {
                    String::new()
                } else {
                    config.api_key.clone()
                },
                api_key_env: key_env,
                headers: config.provider_headers.clone(),
                model: config.model.clone(),
                models: Vec::new(),
            },
        );
        self.active_provider = Some(id);
    }
    pub fn activate_provider(&mut self, id: &str, config: &mut Config) -> anyhow::Result<()> {
        let provider = self
            .providers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown saved provider: {id}"))?;
        provider.validate()?;
        config.provider_id = Some(id.into());
        config.api_type = provider.api_type;
        config.base_url = crate::config::normalize_base_url(&provider.base_url);
        config.api_key = provider.key();
        config.provider_headers = provider.headers.clone();
        config.model = provider.model.clone();
        self.active_provider = Some(id.into());
        // Preserve compatibility with older versions and scripts reading these fields.
        self.provider = Some(provider.name.clone());
        self.base_url = Some(config.base_url.clone());
        self.model = Some(config.model.clone());
        self.api_key = None;
        Ok(())
    }
    pub fn remember_model(&mut self, id: String) {
        self.model = Some(id.clone());
        if let Some(provider) = self
            .active_provider
            .as_ref()
            .and_then(|id| self.providers.get_mut(id))
        {
            provider.model = id.clone();
            if !provider.models.iter().any(|model| model.id == id) {
                provider.models.push(Model {
                    name: id.clone(),
                    id,
                    context_window: None,
                });
            }
        }
    }
    pub fn next_provider_id(&self) -> String {
        (1..)
            .map(|n| format!("provider-{n}"))
            .find(|id| !self.providers.contains_key(id))
            .unwrap()
    }
}

pub fn parse_models(value: &serde_json::Value) -> Vec<Model> {
    let Some(data) = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let mut models = BTreeMap::new();
    for value in data {
        let Some(id) = value["id"]
            .as_str()
            .or_else(|| value["name"].as_str())
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let name = value["display_name"]
            .as_str()
            .or_else(|| value["name"].as_str())
            .unwrap_or(id);
        models.entry(id.to_string()).or_insert_with(|| Model {
            id: id.into(),
            name: name.into(),
            context_window: crate::model_metadata::parse_context_limit(value, id),
        });
    }
    models.into_values().collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn config() -> Config {
        Config {
            context_window: None,
            api_type: ApiType::ChatCompletions,
            provider_id: None,
            provider_headers: Default::default(),
            api_key: "legacy-key".into(),
            base_url: "http://localhost/v1".into(),
            model: "legacy-model".into(),
            workspace_dir: std::env::temp_dir(),
            execution_mode: Default::default(),
            continue_session: false,
            proxy: None,
            mode: crate::theme::AppMode::Manual,
            effort: None,
            max_steps: 10,
        }
    }
    fn provider(name: &str, api_type: ApiType, key: &str, model: &str) -> Provider {
        Provider {
            name: name.into(),
            api_type,
            base_url: "http://localhost/v1".into(),
            api_key: key.into(),
            api_key_env: None,
            headers: Default::default(),
            model: model.into(),
            models: vec![],
        }
    }
    #[test]
    fn malformed_context_limits_preserve_provider_preferences_for_validation() {
        let mut prefs=UserPreferences::default();prefs.api_key=Some("test-key".into());
        let mut value=serde_json::to_value(&prefs).unwrap();
        for bad in [serde_json::json!(-1),serde_json::json!("1M"),serde_json::json!(1.5)] {
            value["context_window"]=bad;
            let prefs:UserPreferences=serde_json::from_value(value.clone()).unwrap();
            assert_eq!(prefs.context_window,Some(0));assert_eq!(prefs.api_key.as_deref(),Some("test-key"));
        }
    }
    #[test]
    fn profiles_keep_credentials_protocol_and_model_isolated_across_restart() {
        let mut prefs = UserPreferences::default();
        let mut config = config();
        prefs.migrate_provider(&config);
        for n in 0..100 {
            prefs.providers.insert(
                format!("other-{n}"),
                provider(
                    &format!("Other {n}"),
                    ApiType::AnthropicMessages,
                    "",
                    "claude-deployment",
                ),
            );
        }
        prefs.activate_provider("other-99", &mut config).unwrap();
        assert!(config.api_key.is_empty());
        assert_eq!(config.api_type, ApiType::AnthropicMessages);
        prefs.remember_model("my-second-deployment".into());
        let mut restored: UserPreferences =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        restored.activate_provider("default", &mut config).unwrap();
        assert_eq!(config.api_key, "legacy-key");
        assert_eq!(config.model, "legacy-model");
        restored.activate_provider("other-99", &mut config).unwrap();
        assert_eq!(config.model, "my-second-deployment");
        assert!(config.api_key.is_empty());
        assert_eq!(restored.providers.len(), 101);
        restored.migrate_provider(&config);
        assert_eq!(restored.providers.len(), 101);
        assert!(!format!("{:?}", restored.providers["default"]).contains("legacy-key"));
    }
    #[test]
    fn model_catalog_uses_real_ids_names_and_reported_windows() {
        let models = parse_models(&serde_json::json!({"data":[
            {"id":"deployment-v2","name":"Readable name","context_length":64000},
            {"id":"claude-deploy","display_name":"Claude deployment"},{"id":"deployment-v2","name":"Duplicate"},{}
        ]}));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].label(), "Claude deployment");
        assert_eq!(models[1].id, "deployment-v2");
        assert_eq!(models[1].context_window, Some(64000));
        assert!(parse_models(&serde_json::json!({"error":"not a catalog"})).is_empty());
    }
    #[test]
    fn provider_rejects_invalid_urls_and_header_injection() {
        let mut provider = provider("Local", ApiType::OpenaiResponses, "", "m");
        provider.validate().unwrap();
        for url in [
            "file:///tmp",
            "https://user:secret@example.com/v1",
            "https://example.com/v1?api_key=secret",
        ] {
            provider.base_url = url.into();
            assert!(provider.validate().is_err());
        }
        provider.base_url = "http://localhost/v1".into();
        provider
            .headers
            .insert("x-routing".into(), "bad\r\nheader: value".into());
        assert!(provider.validate().is_err());
    }
}
