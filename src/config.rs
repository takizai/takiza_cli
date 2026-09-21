use std::path::PathBuf;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub workspace_dir: PathBuf,
    pub auto_approve: bool,
    pub proxy: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        dotenvy::dotenv().ok();

        let prefs = crate::theme::UserPreferences::load();

        let api_key = prefs.api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("GROQ_API_KEY").ok())
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .unwrap_or_default();
        
        let base_url = prefs.base_url
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.groq.com/openai/v1".to_string())
            .trim_end_matches('/')
            .to_string();

        let model = prefs.model
            .or_else(|| std::env::var("OPENAI_MODEL").ok())
            .unwrap_or_else(|| "llama-3.3-70b-versatile".to_string());

        let proxy = std::env::var("TAKIZA_PROXY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("HTTPS_PROXY")
                    .or_else(|_| std::env::var("https_proxy"))
                    .or_else(|_| std::env::var("ALL_PROXY"))
                    .or_else(|_| std::env::var("all_proxy"))
                    .or_else(|_| std::env::var("HTTP_PROXY"))
                    .or_else(|_| std::env::var("http_proxy"))
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });

        let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Self {
            api_key,
            base_url,
            model,
            workspace_dir,
            auto_approve: true,
            proxy,
        }
    }
}
