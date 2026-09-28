use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Amber,      // Classic Takiza Yellow/Gold
    Cyberpunk,  // Magenta/Neon Cyan
    Emerald,    // Hacker Matrix Green
    Nord,       // Frost Arctic Blue/Cyan
    Monochrome, // Clean Silver/White
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Amber
    }
}

impl Theme {
    pub fn all() -> &'static [Theme] {
        &[
            Theme::Amber,
            Theme::Cyberpunk,
            Theme::Emerald,
            Theme::Nord,
            Theme::Monochrome,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Theme::Amber => "Amber (Takiza Gold)",
            Theme::Cyberpunk => "Cyberpunk (Neon Pink & Cyan)",
            Theme::Emerald => "Emerald (Matrix Green)",
            Theme::Nord => "Nord (Frost Arctic Blue)",
            Theme::Monochrome => "Monochrome (Minimalist White)",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Theme::Amber => "Warm golden yellow accents with deep charcoal contrast",
            Theme::Cyberpunk => "Vibrant neon magenta and electric cyan aesthetic",
            Theme::Emerald => "Classic high-contrast terminal phosphor green",
            Theme::Nord => "Calm Scandinavian winter palette with cool arctic cyan",
            Theme::Monochrome => "Clean, distraction-free greyscale minimalism",
        }
    }

    // Colors: (Primary highlight, Border / secondary, Accent)
    pub fn primary_ansi(&self) -> &'static str {
        match self {
            Theme::Amber => "\x1b[38;2;255;195;0m",
            Theme::Cyberpunk => "\x1b[38;2;255;45;149m",
            Theme::Emerald => "\x1b[38;2;0;255;128m",
            Theme::Nord => "\x1b[38;2;136;192;208m",
            Theme::Monochrome => "\x1b[38;2;240;240;245m",
        }
    }

    pub fn secondary_ansi(&self) -> &'static str {
        match self {
            Theme::Amber => "\x1b[38;2;70;200;220m",
            Theme::Cyberpunk => "\x1b[38;2;0;240;255m",
            Theme::Emerald => "\x1b[38;2;40;180;100m",
            Theme::Nord => "\x1b[38;2;94;129;172m",
            Theme::Monochrome => "\x1b[38;2;160;160;165m",
        }
    }

    pub fn primary_crossterm(&self) -> crossterm::style::Color {
        match self {
            Theme::Amber => crossterm::style::Color::Rgb { r: 255, g: 195, b: 0 },
            Theme::Cyberpunk => crossterm::style::Color::Rgb { r: 255, g: 45, b: 149 },
            Theme::Emerald => crossterm::style::Color::Rgb { r: 0, g: 255, b: 128 },
            Theme::Nord => crossterm::style::Color::Rgb { r: 136, g: 192, b: 208 },
            Theme::Monochrome => crossterm::style::Color::Rgb { r: 240, g: 240, b: 245 },
        }
    }

    #[allow(dead_code)]
    pub fn secondary_crossterm(&self) -> crossterm::style::Color {
        match self {
            Theme::Amber => crossterm::style::Color::Rgb { r: 70, g: 200, b: 220 },
            Theme::Cyberpunk => crossterm::style::Color::Rgb { r: 0, g: 240, b: 255 },
            Theme::Emerald => crossterm::style::Color::Rgb { r: 40, g: 180, b: 100 },
            Theme::Nord => crossterm::style::Color::Rgb { r: 94, g: 129, b: 172 },
            Theme::Monochrome => crossterm::style::Color::Rgb { r: 160, g: 160, b: 165 },
        }
    }

    pub fn border_crossterm(&self) -> crossterm::style::Color {
        match self {
            Theme::Amber => crossterm::style::Color::Rgb { r: 0, g: 200, b: 220 },
            Theme::Cyberpunk => crossterm::style::Color::Rgb { r: 0, g: 240, b: 255 },
            Theme::Emerald => crossterm::style::Color::Rgb { r: 40, g: 180, b: 100 },
            Theme::Nord => crossterm::style::Color::Rgb { r: 94, g: 129, b: 172 },
            Theme::Monochrome => crossterm::style::Color::Rgb { r: 140, g: 140, b: 145 },
        }
    }

    pub fn from_str_loose(s: &str) -> Option<Theme> {
        let clean = s.trim().to_lowercase();
        match clean.as_str() {
            "amber" | "takiza" | "gold" | "yellow" | "1" => Some(Theme::Amber),
            "cyberpunk" | "cyber" | "neon" | "pink" | "2" => Some(Theme::Cyberpunk),
            "emerald" | "green" | "matrix" | "3" => Some(Theme::Emerald),
            "nord" | "blue" | "arctic" | "frost" | "4" => Some(Theme::Nord),
            "monochrome" | "mono" | "white" | "silver" | "gray" | "grey" | "5" => Some(Theme::Monochrome),
            _ => None,
        }
    }
}

static ACTIVE_THEME: std::sync::RwLock<Theme> = std::sync::RwLock::new(Theme::Amber);

pub fn current() -> Theme {
    *ACTIVE_THEME.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_current(theme: Theme) {
    if let Ok(mut guard) = ACTIVE_THEME.write() {
        *guard = theme;
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppMode {
    Manual,
    MoA,
}

impl Default for AppMode {
    fn default() -> Self {
        AppMode::Manual
    }
}

impl AppMode {
    pub fn name(&self) -> &'static str {
        match self {
            AppMode::Manual => "Takiza Manual",
            AppMode::MoA => "Takiza MoA",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            AppMode::Manual => "Manual model selection from frontier LLM catalog",
            AppMode::MoA => "Mixture of Agents: smart auto-routing (~45% lower token costs)",
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            AppMode::Manual => "manual",
            AppMode::MoA => "moa",
        }
    }

    pub fn from_str_loose(s: &str) -> Option<AppMode> {
        let clean = s.trim().to_lowercase();
        match clean.as_str() {
            "manual" | "takiza manual" | "m" | "1" => Some(AppMode::Manual),
            "moa" | "takiza moa" | "auto" | "2" => Some(AppMode::MoA),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserPreferences {
    pub agreed_to_terms: bool,
    pub terms_version: String,
    pub theme: Theme,
    pub accepted_at: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            agreed_to_terms: false,
            terms_version: "1.0.0".to_string(),
            theme: Theme::Amber,
            accepted_at: None,
            provider: None,
            model: None,
            base_url: None,
            api_key: None,
            mode: None,
            effort: None,
        }
    }
}

impl UserPreferences {
    pub fn config_path() -> PathBuf {
        if let Ok(workspace) = std::env::current_dir() {
            let local = workspace.join(".takiza").join("config.json");
            if local.exists() {
                return local;
            }
        }
        if let Some(home) = dirs_next_or_home() {
            return home.join(".config").join("takiza").join("config.json");
        }
        PathBuf::from(".takiza").join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(prefs) = serde_json::from_str::<UserPreferences>(&content) {
                return prefs;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(path, json)?;
        Ok(())
    }
}

pub fn dirs_next_or_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE");
            let path = std::env::var_os("HOMEPATH");
            match (drive, path) {
                (Some(d), Some(p)) => {
                    let mut b = PathBuf::from(d);
                    b.push(p);
                    Some(b)
                }
                _ => None,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_from_str_loose() {
        assert_eq!(Theme::from_str_loose("amber"), Some(Theme::Amber));
        assert_eq!(Theme::from_str_loose("cyber"), Some(Theme::Cyberpunk));
        assert_eq!(Theme::from_str_loose("CYBERPUNK"), Some(Theme::Cyberpunk));
        assert_eq!(Theme::from_str_loose("matrix"), Some(Theme::Emerald));
        assert_eq!(Theme::from_str_loose("nord"), Some(Theme::Nord));
        assert_eq!(Theme::from_str_loose("mono"), Some(Theme::Monochrome));
        assert_eq!(Theme::from_str_loose("unknown_theme"), None);
    }

    #[test]
    fn test_theme_colors() {
        for t in Theme::all() {
            assert!(!t.name().is_empty());
            assert!(!t.description().is_empty());
            assert!(t.primary_ansi().starts_with("\x1b["));
            assert!(t.secondary_ansi().starts_with("\x1b["));
        }
    }

    #[test]
    fn test_user_preferences_serde() {
        let prefs = UserPreferences {
            agreed_to_terms: true,
            terms_version: "1.0.0".to_string(),
            theme: Theme::Cyberpunk,
            accepted_at: Some("2026-09-21 00:00:00".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let deserialized: UserPreferences = serde_json::from_str(&json).unwrap();
        assert!(deserialized.agreed_to_terms);
        assert_eq!(deserialized.theme, Theme::Cyberpunk);
        assert_eq!(deserialized.terms_version, "1.0.0");
    }

    #[test]
    fn test_active_theme_global() {
        set_current(Theme::Emerald);
        assert_eq!(current(), Theme::Emerald);
        set_current(Theme::Amber);
        assert_eq!(current(), Theme::Amber);
    }

    #[test]
    fn test_app_mode() {
        assert_eq!(AppMode::from_str_loose("manual"), Some(AppMode::Manual));
        assert_eq!(AppMode::from_str_loose("takiza manual"), Some(AppMode::Manual));
        assert_eq!(AppMode::from_str_loose("moa"), Some(AppMode::MoA));
        assert_eq!(AppMode::from_str_loose("takiza moa"), Some(AppMode::MoA));
        assert_eq!(AppMode::from_str_loose("auto"), Some(AppMode::MoA));
        assert_eq!(AppMode::from_str_loose("invalid"), None);

        assert_eq!(AppMode::Manual.name(), "Takiza Manual");
        assert_eq!(AppMode::MoA.name(), "Takiza MoA");
        assert_eq!(AppMode::Manual.as_str(), "manual");
        assert_eq!(AppMode::MoA.as_str(), "moa");
    }
}
