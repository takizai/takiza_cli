use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    #[serde(alias = "amber")]
    Amber,      // Classic Takiza Yellow/Gold
    #[serde(alias = "cyberpunk")]
    Cyberpunk,  // Magenta/Neon Cyan
    #[serde(alias = "emerald")]
    Emerald,    // Hacker Matrix Green
    #[serde(alias = "nord")]
    Nord,       // Frost Arctic Blue/Cyan
    #[serde(alias = "monochrome")]
    Monochrome, // Clean Silver/White
    #[serde(alias = "dracula")]
    Dracula,
    #[serde(alias = "rose")]
    Rose,
    #[serde(alias = "ocean")]
    Ocean,
    #[serde(alias = "sakura")]
    Sakura,
    #[serde(alias = "solarized")]
    Solarized,
    #[serde(alias = "evening-irkutsk", alias = "evening_irkutsk", alias = "irkutsk")]
    EveningIrkutsk,
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
            Theme::Dracula,
            Theme::Rose,
            Theme::Ocean,
            Theme::Sakura,
            Theme::Solarized,
            Theme::EveningIrkutsk,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Theme::Amber => crate::i18n::tr("Amber (Takiza Gold)"),
            Theme::Cyberpunk => crate::i18n::tr("Cyberpunk (Neon Pink & Cyan)"),
            Theme::Emerald => crate::i18n::tr("Emerald (Matrix Green)"),
            Theme::Nord => crate::i18n::tr("Nord (Frost Arctic Blue)"),
            Theme::Monochrome => crate::i18n::tr("Monochrome (Minimalist White)"),
            Theme::Dracula => crate::i18n::tr("Dracula (Violet & Mint)"),
            Theme::Rose => crate::i18n::tr("Rose (Warm Coral)"),
            Theme::Ocean => crate::i18n::tr("Ocean (Deep Sea Blue)"),
            Theme::Sakura => crate::i18n::tr("Sakura (Soft Blossom)"),
            Theme::Solarized => crate::i18n::tr("Solarized (Golden Teal)"),
            Theme::EveningIrkutsk => crate::i18n::tr("Evening Irkutsk (Angara Lights)"),
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Theme::Amber => crate::i18n::tr("Warm golden yellow accents with deep charcoal contrast"),
            Theme::Cyberpunk => crate::i18n::tr("Vibrant neon magenta and electric cyan aesthetic"),
            Theme::Emerald => crate::i18n::tr("Classic high-contrast terminal phosphor green"),
            Theme::Nord => crate::i18n::tr("Calm Scandinavian winter palette with cool arctic cyan"),
            Theme::Monochrome => crate::i18n::tr("Clean, distraction-free greyscale minimalism"),
            Theme::Dracula => crate::i18n::tr("Violet accents with soft mint borders"),
            Theme::Rose => crate::i18n::tr("Warm coral accents with muted rose borders"),
            Theme::Ocean => crate::i18n::tr("Bright ocean blue with turquoise borders"),
            Theme::Sakura => crate::i18n::tr("Soft blossom pink with lavender borders"),
            Theme::Solarized => crate::i18n::tr("Golden accents with balanced teal borders"),
            Theme::EveningIrkutsk => crate::i18n::tr("Warm amber streetlights, muted lilac activity and icy blue Angara borders"),
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
            Theme::Dracula => "\x1b[38;2;189;147;249m",
            Theme::Rose => "\x1b[38;2;255;128;145m",
            Theme::Ocean => "\x1b[38;2;80;180;255m",
            Theme::Sakura => "\x1b[38;2;245;170;215m",
            Theme::Solarized => "\x1b[38;2;220;185;75m",
            Theme::EveningIrkutsk => "\x1b[38;2;240;183;105m",
        }
    }

    pub fn secondary_ansi(&self) -> &'static str {
        match self {
            Theme::Amber => "\x1b[38;2;70;200;220m",
            Theme::Cyberpunk => "\x1b[38;2;0;240;255m",
            Theme::Emerald => "\x1b[38;2;40;180;100m",
            Theme::Nord => "\x1b[38;2;94;129;172m",
            Theme::Monochrome => "\x1b[38;2;160;160;165m",
            Theme::Dracula => "\x1b[38;2;80;200;160m",
            Theme::Rose => "\x1b[38;2;205;145;155m",
            Theme::Ocean => "\x1b[38;2;60;190;195m",
            Theme::Sakura => "\x1b[38;2;180;150;215m",
            Theme::Solarized => "\x1b[38;2;90;175;170m",
            Theme::EveningIrkutsk => "\x1b[38;2;169;151;193m",
        }
    }

    pub fn code_background_ansi(&self) -> &'static str {
        match self {
            Theme::Amber => "\x1b[48;2;38;38;44m",
            Theme::Cyberpunk => "\x1b[48;2;35;20;36m",
            Theme::Emerald => "\x1b[48;2;16;32;24m",
            Theme::Nord => "\x1b[48;2;46;52;64m",
            Theme::Monochrome => "\x1b[48;2;32;32;34m",
            Theme::Dracula => "\x1b[48;2;40;35;55m",
            Theme::Rose => "\x1b[48;2;45;28;34m",
            Theme::Ocean => "\x1b[48;2;20;32;45m",
            Theme::Sakura => "\x1b[48;2;40;30;43m",
            Theme::Solarized => "\x1b[48;2;28;38;38m",
            Theme::EveningIrkutsk => "\x1b[48;2;27;35;48m",
        }
    }

    pub fn primary_crossterm(&self) -> crossterm::style::Color {
        match self {
            Theme::Amber => crossterm::style::Color::Rgb { r: 255, g: 195, b: 0 },
            Theme::Cyberpunk => crossterm::style::Color::Rgb { r: 255, g: 45, b: 149 },
            Theme::Emerald => crossterm::style::Color::Rgb { r: 0, g: 255, b: 128 },
            Theme::Nord => crossterm::style::Color::Rgb { r: 136, g: 192, b: 208 },
            Theme::Monochrome => crossterm::style::Color::Rgb { r: 240, g: 240, b: 245 },
            Theme::Dracula => crossterm::style::Color::Rgb { r: 189, g: 147, b: 249 },
            Theme::Rose => crossterm::style::Color::Rgb { r: 255, g: 128, b: 145 },
            Theme::Ocean => crossterm::style::Color::Rgb { r: 80, g: 180, b: 255 },
            Theme::Sakura => crossterm::style::Color::Rgb { r: 245, g: 170, b: 215 },
            Theme::Solarized => crossterm::style::Color::Rgb { r: 220, g: 185, b: 75 },
            Theme::EveningIrkutsk => crossterm::style::Color::Rgb { r: 240, g: 183, b: 105 },
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
            Theme::Dracula => crossterm::style::Color::Rgb { r: 80, g: 200, b: 160 },
            Theme::Rose => crossterm::style::Color::Rgb { r: 205, g: 145, b: 155 },
            Theme::Ocean => crossterm::style::Color::Rgb { r: 60, g: 190, b: 195 },
            Theme::Sakura => crossterm::style::Color::Rgb { r: 180, g: 150, b: 215 },
            Theme::Solarized => crossterm::style::Color::Rgb { r: 90, g: 175, b: 170 },
            Theme::EveningIrkutsk => crossterm::style::Color::Rgb { r: 169, g: 151, b: 193 },
        }
    }

    pub fn border_crossterm(&self) -> crossterm::style::Color {
        match self {
            Theme::Amber => crossterm::style::Color::Rgb { r: 0, g: 200, b: 220 },
            Theme::Cyberpunk => crossterm::style::Color::Rgb { r: 0, g: 240, b: 255 },
            Theme::Emerald => crossterm::style::Color::Rgb { r: 40, g: 180, b: 100 },
            Theme::Nord => crossterm::style::Color::Rgb { r: 94, g: 129, b: 172 },
            Theme::Monochrome => crossterm::style::Color::Rgb { r: 140, g: 140, b: 145 },
            Theme::Dracula => crossterm::style::Color::Rgb { r: 80, g: 200, b: 160 },
            Theme::Rose => crossterm::style::Color::Rgb { r: 205, g: 145, b: 155 },
            Theme::Ocean => crossterm::style::Color::Rgb { r: 60, g: 190, b: 195 },
            Theme::Sakura => crossterm::style::Color::Rgb { r: 180, g: 150, b: 215 },
            Theme::Solarized => crossterm::style::Color::Rgb { r: 90, g: 175, b: 170 },
            Theme::EveningIrkutsk => crossterm::style::Color::Rgb { r: 133, g: 183, b: 205 },
        }
    }

    pub fn diff_colors_ansi(&self) -> (&'static str, &'static str) {
        match self {
            Theme::Amber => ("\x1b[38;2;140;215;120m", "\x1b[38;2;245;135;105m"),
            Theme::Cyberpunk => ("\x1b[38;2;0;240;200m", "\x1b[38;2;255;100;175m"),
            Theme::Emerald => ("\x1b[38;2;100;255;155m", "\x1b[38;2;245;130;115m"),
            Theme::Nord => ("\x1b[38;2;163;190;140m", "\x1b[38;2;191;97;106m"),
            Theme::Monochrome => ("\x1b[38;2;225;225;230m", "\x1b[38;2;155;155;165m"),
            Theme::Dracula => ("\x1b[38;2;80;250;123m", "\x1b[38;2;255;85;85m"),
            Theme::Rose => ("\x1b[38;2;150;215;170m", "\x1b[38;2;255;128;145m"),
            Theme::Ocean => ("\x1b[38;2;90;220;190m", "\x1b[38;2;250;145;120m"),
            Theme::Sakura => ("\x1b[38;2;170;220;180m", "\x1b[38;2;245;145;190m"),
            Theme::Solarized => ("\x1b[38;2;160;190;70m", "\x1b[38;2;235;115;100m"),
            Theme::EveningIrkutsk => ("\x1b[38;2;119;201;184m", "\x1b[38;2;220;139;119m"),
        }
    }

    pub fn diff_backgrounds_ansi(&self) -> (&'static str, &'static str) {
        match self {
            Theme::Amber => ("\x1b[48;2;32;52;30m", "\x1b[48;2;60;32;25m"),
            Theme::Cyberpunk => ("\x1b[48;2;14;50;45m", "\x1b[48;2;60;23;43m"),
            Theme::Emerald => ("\x1b[48;2;20;55;32m", "\x1b[48;2;55;29;25m"),
            Theme::Nord => ("\x1b[48;2;45;56;42m", "\x1b[48;2;60;36;42m"),
            Theme::Monochrome => ("\x1b[48;2;52;52;57m", "\x1b[48;2;36;36;41m"),
            Theme::Dracula => ("\x1b[48;2;23;55;36m", "\x1b[48;2;63;28;35m"),
            Theme::Rose => ("\x1b[48;2;32;51;40m", "\x1b[48;2;63;32;40m"),
            Theme::Ocean => ("\x1b[48;2;21;50;46m", "\x1b[48;2;60;34;29m"),
            Theme::Sakura => ("\x1b[48;2;38;52;42m", "\x1b[48;2;59;33;47m"),
            Theme::Solarized => ("\x1b[48;2;41;50;26m", "\x1b[48;2;58;32;26m"),
            Theme::EveningIrkutsk => ("\x1b[48;2;27;49;49m", "\x1b[48;2;57;35;38m"),
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
            "dracula" | "6" => Some(Theme::Dracula),
            "rose" | "7" => Some(Theme::Rose),
            "ocean" | "8" => Some(Theme::Ocean),
            "sakura" | "9" => Some(Theme::Sakura),
            "solarized" | "10" => Some(Theme::Solarized),
            "evening-irkutsk" | "evening_irkutsk" | "evening irkutsk" | "irkutsk" | "11" => Some(Theme::EveningIrkutsk),
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
            AppMode::Manual => crate::i18n::tr("Manual model selection from frontier LLM catalog"),
            AppMode::MoA => crate::i18n::tr("Mixture of Agents: smart auto-routing (~45% lower token costs)"),
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
    #[serde(default)]
    pub language: crate::i18n::Language,
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
    #[serde(default)]
    pub auto_approve: Option<bool>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            agreed_to_terms: false,
            terms_version: "1.0.0".to_string(),
            theme: Theme::Amber,
            language: crate::i18n::Language::English,
            accepted_at: None,
            provider: None,
            model: None,
            base_url: None,
            api_key: None,
            mode: None,
            effort: None,
            auto_approve: None,
            extra: Default::default(),
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
    fn gui_theme_names_do_not_reset_tui_preferences_and_extra_settings_survive() {
        for (name, theme) in [("amber", Theme::Amber), ("cyberpunk", Theme::Cyberpunk),
            ("emerald", Theme::Emerald), ("nord", Theme::Nord), ("monochrome", Theme::Monochrome)] {
            let mut value = serde_json::json!({
                "agreed_to_terms": true, "terms_version": "1.0.0", "theme": name,
                "model": "saved-model", "provider": "custom", "base_url": "http://localhost/v1",
                "api_key": "test-only", "auto_approve": true, "proxy": "http://localhost:8080",
                "last_workspace_dir": "/workspace", "mode": "moa", "effort": "high"
            });
            let prefs: UserPreferences = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(prefs.theme, theme);
            assert_eq!(prefs.model.as_deref(), Some("saved-model"));
            let saved = serde_json::to_value(&prefs).unwrap();
            assert_eq!(saved["auto_approve"], true);
            assert_eq!(saved["proxy"], value["proxy"]);
            assert_eq!(saved["last_workspace_dir"], value["last_workspace_dir"]);
            // Existing TUI spelling remains readable and is still written for
            // compatibility with older installed binaries.
            value["theme"] = saved["theme"].clone();
            assert_eq!(serde_json::from_value::<UserPreferences>(value).unwrap().theme, theme);
        }
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
