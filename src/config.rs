use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliArgs {
    pub continue_session: bool,
    pub auto_approve: bool,
    pub show_help: bool,
    pub show_version: bool,
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
        let mut continue_session = false;
        let mut auto_approve = false;
        let mut show_help = false;
        let mut show_version = false;
        let mut prompt_parts = Vec::new();

        for arg in &args {
            if arg == "--help" || arg == "-h" {
                show_help = true;
            } else if arg == "--version" || arg == "-V" || arg == "-v" {
                show_version = true;
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

        let prompt = if prompt_parts.is_empty() {
            None
        } else {
            Some(prompt_parts.join(" "))
        };

        Self {
            continue_session,
            auto_approve,
            show_help,
            show_version,
            prompt,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub workspace_dir: PathBuf,
    pub auto_approve: bool,
    pub continue_session: bool,
    pub proxy: Option<String>,
    pub mode: crate::theme::AppMode,
    pub effort: Option<String>,
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

        let api_key = prefs.api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("GROQ_API_KEY").ok())
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .unwrap_or_default();
        
        let base_url = prefs.base_url
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://anymodel.org/v1".to_string())
            .trim_end_matches('/')
            .to_string();

        let model = prefs.model
            .or_else(|| std::env::var("OPENAI_MODEL").ok())
            .unwrap_or_else(|| "llama-3.3-70b-versatile".to_string());

        let mode = prefs.mode
            .as_deref()
            .and_then(crate::theme::AppMode::from_str_loose)
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

        let auto_approve = cli_args.auto_approve || env_auto_approve;

        let effort = prefs.effort
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("REASONING_EFFORT").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some("medium".to_string()));

        Self {
            api_key,
            base_url,
            model,
            workspace_dir,
            auto_approve,
            continue_session: cli_args.continue_session,
            proxy,
            mode,
            effort,
        }
    }

    #[allow(dead_code)]
    pub fn load() -> Self {
        let args = CliArgs::parse();
        Self::load_with_args(&args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_args_parsing() {
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
