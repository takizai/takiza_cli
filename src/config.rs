use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliArgs {
    pub continue_session: bool,
    pub session_id: Option<String>,
    pub auto_approve: bool,
    pub show_help: bool,
    pub show_version: bool,
    pub update: bool,
    pub prompt: Option<String>,
}

impl CliArgs {
    pub fn parse() -> Self {
        Self::parse_from(std::env::args().skip(1))
    }

    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let args: Vec<String> = args.into_iter().map(|s| s.into()).collect();
        // Reserve only the subcommand itself; sentences beginning with update
        // and prompts supplied alongside session/options remain ordinary prompts.
        let update = args.first().is_some_and(|arg| arg == "update")
            && args[1..].iter().all(|arg| matches!(arg.as_str(), "--help" | "-h"));
        let mut continue_session = false;
        let mut session_id = None;
        let mut auto_approve = false;
        let mut show_help = false;
        let mut show_version = false;
        let mut prompt_parts = Vec::new();

        let mut args = args.iter().peekable();
        while let Some(arg) = args.next() {
            if arg == "--help" || arg == "-h" {
                show_help = true;
            } else if arg == "--version" || arg == "-V" || arg == "-v" {
                show_version = true;
            } else if arg == "--session" {
                session_id = Some(args.next().cloned().unwrap_or_default());
                continue_session = true;
            } else if arg == "--continue" || arg == "--resume" || arg == "-c" {
                continue_session = true;
            } else if arg == "--yes"
                || arg == "-y"
                || arg == "--auto-approve"
                || arg == "-a"
                || arg == "--skip-permissions"
                || arg == "--dangerously-skip-permissions"
            {
                auto_approve = true;
            } else if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 {
                for ch in arg[1..].chars() {
                    match ch {
                        'c' => continue_session = true,
                        'y' | 'a' => auto_approve = true,
                        'h' => show_help = true,
                        'v' | 'V' => show_version = true,
                        _ => {}
                    }
                }
            } else {
                prompt_parts.push(arg.clone());
            }
        }

        let prompt = if update || prompt_parts.is_empty() {
            None
        } else {
            Some(prompt_parts.join(" "))
        };

        Self {
            continue_session,
            session_id,
            auto_approve,
            show_help,
            show_version,
            update,
            prompt,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub context_window: Option<u64>,
    pub api_type: crate::providers::ApiType,
    pub provider_id: Option<String>,
    pub provider_headers: std::collections::BTreeMap<String, String>,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub workspace_dir: PathBuf,
    pub execution_mode: crate::execution_mode::ModeControl,
    pub continue_session: bool,
    pub proxy: Option<String>,
    pub mode: crate::theme::AppMode,
    pub effort: Option<String>,
    pub max_steps: usize,
}

impl Config {
    pub fn load_with_args(cli_args: &CliArgs) -> Self {
        dotenvy::dotenv().ok();

        if let Some(home) = crate::theme::dirs_next_or_home() {
            let harness_env = home.join("takiza-harness").join(".env");
            if harness_env.exists() {
                let _ = dotenvy::from_path(&harness_env);
            }
            let config_env = home.join(".config").join("takiza").join(".env");
            if config_env.exists() {
                let _ = dotenvy::from_path(&config_env);
            }
        }

        let prefs = crate::theme::UserPreferences::load();

        let active = prefs.active_provider.as_ref().and_then(|id| prefs.providers.get(id));
        let api_key = if let Some(provider) = active { provider.key() } else { prefs.api_key.clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("GROQ_API_KEY").ok())
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .unwrap_or_default() };
        
        let base_url = normalize_base_url(&active.map(|p| p.base_url.clone()).or_else(|| prefs.base_url.clone())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://anymodel.org/v1".to_string()));

        let model = active.map(|p| p.model.clone()).or_else(|| prefs.model.clone())
            .or_else(|| std::env::var("OPENAI_MODEL").ok())
            .unwrap_or_else(|| "llama-3.3-70b-versatile".to_string());

        let mode = prefs.mode
            .as_deref()
            .and_then(crate::theme::AppMode::from_str_loose)
            .filter(|mode| mode.is_available())
            .unwrap_or_default();

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

        // Default: auto_approve is false (must ask user before running commands)
        // Can be bypassed by -y / -a / --auto-approve or TAKIZA_AUTO_APPROVE env var
        let env_auto_approve = std::env::var("TAKIZA_AUTO_APPROVE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let auto_approve = cli_args.auto_approve || prefs.auto_approve.unwrap_or(env_auto_approve);
        let execution_mode = crate::execution_mode::ModeControl::new(if cli_args.auto_approve {
            crate::execution_mode::ExecutionMode::AllowAll
        } else { prefs.execution_mode.unwrap_or(if auto_approve {
            crate::execution_mode::ExecutionMode::AllowAll
        } else { crate::execution_mode::ExecutionMode::Auto }) });

        let effort = prefs.effort
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("REASONING_EFFORT").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some("medium".to_string()));

        Self {
            context_window: prefs.context_window,
            api_type: active.map(|p| p.api_type).unwrap_or_default(),
            provider_id: prefs.active_provider.clone(),
            provider_headers: active.map(|p| p.headers.clone()).unwrap_or_default(),
            api_key,
            base_url,
            model,
            workspace_dir,
            execution_mode,
            continue_session: cli_args.continue_session,
            proxy,
            mode,
            effort,
            max_steps: parse_max_steps(std::env::var("TAKIZA_MAX_STEPS").ok().as_deref()),
        }
    }

    #[allow(dead_code)]
    pub fn load() -> Self {
        let args = CliArgs::parse();
        Self::load_with_args(&args)
    }
}

pub fn normalize_base_url(value: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    if let Ok(mut url) = reqwest::Url::parse(value) {
        // AnyModel redirects HTTP to HTTPS. Avoid that redirect so POST and
        // Authorization reach the API together. Custom/local HTTP stays valid.
        if url.scheme() == "http" && url.host_str() == Some("anymodel.org") {
            url.set_scheme("https").expect("HTTP URL supports HTTPS");
            return url.to_string().trim_end_matches('/').to_string();
        }
    }
    value.to_string()
}

fn parse_max_steps(value: Option<&str>) -> usize {
    value.and_then(|value| value.trim().parse().ok()).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_subcommand_does_not_consume_regular_prompts() {
        assert!(super::CliArgs::parse_from(["update"]).update);
        let help = super::CliArgs::parse_from(["update", "--help"]);
        assert!(help.update && help.show_help);
        for args in [vec!["update the docs"], vec!["update", "the", "docs"], vec!["--continue", "update"]] {
            let parsed = super::CliArgs::parse_from(args);
            assert!(!parsed.update);
            assert!(parsed.prompt.unwrap().contains("update"));
        }
    }
    use super::*;

    #[test]
    fn anymodel_http_uses_https_without_changing_custom_endpoints() {
        assert_eq!(normalize_base_url(" http://anymodel.org/v1/ "), "https://anymodel.org/v1");
        assert_eq!(normalize_base_url("https://anymodel.org/v1/"), "https://anymodel.org/v1");
        assert_eq!(normalize_base_url("http://localhost:11434/v1/"), "http://localhost:11434/v1");
        assert_eq!(normalize_base_url("http://anymodel.org.example/v1"), "http://anymodel.org.example/v1");
    }

    #[test]
    fn step_limit_defaults_and_overrides() {
        assert_eq!(parse_max_steps(None), 100);
        assert_eq!(parse_max_steps(Some(" 250 ")), 250);
        assert_eq!(parse_max_steps(Some("0")), 0);
        assert_eq!(parse_max_steps(Some("invalid")), 100);
        assert_eq!(parse_max_steps(Some("-1")), 100);
    }

    #[test]
    fn test_cli_args_parsing() {
        let args = CliArgs::parse_from(["--session", "20261006_123456", "continue work"]);
        assert!(args.continue_session);
        assert_eq!(args.session_id.as_deref(), Some("20261006_123456"));
        assert_eq!(args.prompt.as_deref(), Some("continue work"));
        assert_eq!(CliArgs::parse_from(["--session"]).session_id.as_deref(), Some(""));
        let args = CliArgs::parse_from(vec!["-c", "-y"]);
        assert!(args.continue_session);
        assert!(args.auto_approve);

        let args = CliArgs::parse_from(vec!["-cy"]);
        assert!(args.continue_session);
        assert!(args.auto_approve);

        let args = CliArgs::parse_from(vec!["--continue", "--auto-approve"]);
        assert!(args.continue_session);
        assert!(args.auto_approve);

        let args = CliArgs::parse_from(vec!["--resume", "--yes"]);
        assert!(args.continue_session);
        assert!(args.auto_approve);

        let args = CliArgs::parse_from(vec!["--dangerously-skip-permissions"]);
        assert!(!args.continue_session);
        assert!(args.auto_approve);

        let default_args = CliArgs::parse_from(Vec::<String>::new());
        assert!(!default_args.continue_session);
        assert!(!default_args.auto_approve);
    }
}
