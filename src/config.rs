use crate::i18n::{self, Key, Lang};
pub use koala_extensions::ExtensionsConfig;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot determine home directory for ~/.koala/config.toml")]
    HomeDirectoryUnavailable,
    #[error("failed to initialize config: {0}")]
    Initialize(#[source] anyhow::Error),
    #[error("failed to read config file {0}: {1}")]
    Read(String, #[source] std::io::Error),
    #[error("failed to parse config file {0}: {1}")]
    Parse(String, #[source] toml::de::Error),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Interface language for the TUI and system prompts; `/lang` toggles it.
    pub lang: Lang,
    /// Interface palette; auto uses the terminal's own colors.
    pub theme: Theme,
    #[serde(skip)]
    pub(crate) theme_path: Option<PathBuf>,
    /// Runtime preference location, never supplied by config.toml.
    #[serde(skip)]
    pub(crate) language_path: Option<PathBuf>,
    pub llm: LlmConfig,
    pub agent: AgentConfig,
    pub permissions: PermissionsConfig,
    pub hooks: HooksConfig,
    pub extensions: ExtensionsConfig,
    pub mcp: crate::mcp::McpConfig,
}

/// Auto uses terminal-owned colors, which follow terminal theme changes live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
    Catppuccin,
    Nord,
    Dracula,
    #[serde(rename = "catppuccin-latte")]
    CatppuccinLatte,
    #[serde(rename = "solarized-light")]
    SolarizedLight,
    #[serde(rename = "github-light")]
    GithubLight,
}

impl Theme {
    pub const ALL: [Self; 9] = [
        Self::Auto,
        Self::Light,
        Self::Dark,
        Self::Catppuccin,
        Self::Nord,
        Self::Dracula,
        Self::CatppuccinLatte,
        Self::SolarizedLight,
        Self::GithubLight,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Catppuccin => "catppuccin",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::SolarizedLight => "solarized-light",
            Self::GithubLight => "github-light",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "catppuccin" => Some(Self::Catppuccin),
            "nord" => Some(Self::Nord),
            "dracula" => Some(Self::Dracula),
            "catppuccin-latte" => Some(Self::CatppuccinLatte),
            "solarized-light" => Some(Self::SolarizedLight),
            "github-light" => Some(Self::GithubLight),
            _ => None,
        }
    }
}

/// Approval level, independent of the agent's Normal / Plan execution mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Normal,
    #[default]
    AskWhenNeed,
    AutoEdit,
    NeverAsk,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [
        Self::Normal,
        Self::AskWhenNeed,
        Self::AutoEdit,
        Self::NeverAsk,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::AskWhenNeed => "Ask When Need",
            Self::AutoEdit => "Auto Edit",
            Self::NeverAsk => "Never Ask",
        }
    }

    pub fn description(self, lang: Lang) -> &'static str {
        match self {
            Self::Normal => i18n::text(lang, Key::PermNormalDesc),
            Self::AskWhenNeed => i18n::text(lang, Key::PermAskDesc),
            Self::AutoEdit => i18n::text(lang, Key::PermAutoEditDesc),
            Self::NeverAsk => i18n::text(lang, Key::PermNeverDesc),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "ask_when_need" => Some(Self::AskWhenNeed),
            "auto_edit" => Some(Self::AutoEdit),
            "never_ask" => Some(Self::NeverAsk),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsConfig {
    pub mode: PermissionMode,
    /// Explicit trusted tools in Ask When Need and Auto Edit.
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub pre_tool_use: Vec<String>,
    pub post_tool_use: Vec<String>,
    pub turn_start: Vec<String>,
    pub turn_end: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Additional selectable models using this endpoint and credentials.
    pub models: Vec<ModelConfig>,
    /// Provider-supported values, in the order shown by /effort.
    pub reasoning_efforts: Vec<String>,
    /// Initial value; defaults to the first configured effort.
    pub reasoning_effort: Option<String>,
    /// Model capacity in tokens, used as the context usage percentage denominator.
    pub context_window: Option<std::num::NonZeroU64>,
    /// Extra HTTP headers sent with every request, for endpoints that require
    /// routing headers beyond OpenAI auth (e.g. x-opencode-session).
    pub headers: std::collections::HashMap<String, String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: String::new(),
            models: Vec::new(),
            reasoning_efforts: Vec::new(),
            reasoning_effort: None,
            context_window: None,
            headers: std::collections::HashMap::new(),
        }
    }
}

/// Model-specific capabilities; omitted metadata is never inherited from another model.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub model: String,
    pub reasoning_efforts: Vec<String>,
    pub reasoning_effort: Option<String>,
    pub context_window: Option<std::num::NonZeroU64>,
}

impl LlmConfig {
    pub fn selectable_models(&self) -> Result<Vec<ModelConfig>, String> {
        let mut models = self.models.clone();
        if !models.iter().any(|m| m.model == self.model) {
            models.insert(
                0,
                ModelConfig {
                    model: self.model.clone(),
                    reasoning_efforts: self.reasoning_efforts.clone(),
                    reasoning_effort: self.reasoning_effort.clone(),
                    context_window: self.context_window,
                },
            );
        }
        for (index, model) in models.iter().enumerate() {
            if model.model.trim().is_empty()
                || model.model.chars().any(char::is_whitespace)
                || model.model.chars().any(char::is_control)
                || models[..index].iter().any(|m| m.model == model.model)
            {
                return Err(
                    "model names must be nonempty, unique and contain no whitespace".into(),
                );
            }
            let efforts = &model.reasoning_efforts;
            if efforts.iter().enumerate().any(|(i, v)| {
                v.is_empty()
                    || v.chars().any(char::is_whitespace)
                    || v.chars().any(char::is_control)
                    || efforts[..i].contains(v)
            }) {
                return Err(format!(
                    "{}: reasoning_efforts must contain unique, nonempty values without whitespace",
                    model.model
                ));
            }
            if model
                .reasoning_effort
                .as_ref()
                .is_some_and(|v| !efforts.contains(v))
            {
                return Err(format!(
                    "{}: reasoning_effort must be listed in reasoning_efforts",
                    model.model
                ));
            }
        }
        Ok(models)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// None means unlimited; zero disables tool execution.
    #[serde(deserialize_with = "deserialize_round_limit")]
    pub max_tool_rounds: Option<usize>,
    pub max_retries: usize,
    /// Compact history when its estimated size (chars) exceeds this.
    /// Serialized request byte budget including tools and a 4096-byte response reserve.
    pub compact_threshold: usize,
    #[serde(deserialize_with = "deserialize_round_limit")]
    pub subagent_max_rounds: Option<usize>,
    /// Directory where chat transcripts (session jsonl) are appended.
    /// Extensions can use the path supplied on successful root turn_end.
    pub session_dir: PathBuf,
    /// Structured curated memory store; legacy Markdown is never imported.
    pub memory_file: PathBuf,
    pub memory_read: bool,
    pub memory_write: bool,
    /// UTF-8 bytes; clamped to 1024..=32000 at the memory interface.
    pub memory_index_bytes: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_rounds: None,
            max_retries: 5,
            compact_threshold: 40_000,
            subagent_max_rounds: None,
            session_dir: PathBuf::from(".koala/session"),
            memory_file: default_memory_file(),
            memory_read: true,
            memory_write: true,
            memory_index_bytes: 4000,
        }
    }
}

/// Accept a nonnegative round budget or the explicit TOML string "unlimited".
fn deserialize_round_limit<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Limit {
        Rounds(usize),
        Name(String),
    }
    match Limit::deserialize(deserializer)? {
        Limit::Rounds(rounds) => Ok(Some(rounds)),
        Limit::Name(name) if name == "unlimited" => Ok(None),
        Limit::Name(_) => Err(serde::de::Error::custom(
            "round limit must be a nonnegative integer or \"unlimited\"",
        )),
    }
}

/// Shared location for user configuration and global resources.
pub fn koala_dir() -> Result<PathBuf, ConfigError> {
    dirs::home_dir()
        .map(|home| home.join(".koala"))
        .ok_or(ConfigError::HomeDirectoryUnavailable)
}

fn default_memory_file() -> PathBuf {
    koala_dir()
        .expect("a home directory is required for Koala's global resources")
        .join("memory.json")
}

impl Config {
    /// Read ~/.koala/config.toml independently of the current working directory.
    /// Saved workspace appearance overrides the file; KOALA_* env vars win last.
    pub fn load() -> Result<Self, ConfigError> {
        let path = koala_dir()?.join("config.toml");
        crate::setup::ensure_config(&path).map_err(ConfigError::Initialize)?;
        let mut cfg = Self::load_files(&[path], PathBuf::from(".koala/language.toml"))?;
        cfg.apply_env();
        Ok(cfg)
    }

    pub(crate) fn load_files(
        candidates: &[PathBuf],
        language_path: PathBuf,
    ) -> Result<Self, ConfigError> {
        let mut cfg = Config::default();
        for path in candidates {
            if path.is_file() {
                let text = fs::read_to_string(path)
                    .map_err(|e| ConfigError::Read(path.display().to_string(), e))?;
                cfg = toml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(path.display().to_string(), e))?;
                break;
            }
        }
        match fs::read_to_string(&language_path) {
            Ok(text) => {
                #[derive(Deserialize)]
                struct LanguagePreference {
                    lang: Lang,
                }
                let preference: LanguagePreference = toml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(language_path.display().to_string(), e))?;
                cfg.lang = preference.lang;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ConfigError::Read(language_path.display().to_string(), e)),
        }
        let theme_path = language_path.with_file_name("theme.toml");
        match fs::read_to_string(&theme_path) {
            Ok(text) => {
                #[derive(Deserialize)]
                struct ThemePreference {
                    theme: Theme,
                }
                let preference: ThemePreference = toml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(theme_path.display().to_string(), e))?;
                cfg.theme = preference.theme;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ConfigError::Read(theme_path.display().to_string(), e)),
        }
        cfg.theme_path = Some(theme_path);
        cfg.language_path = Some(language_path);
        Ok(cfg)
    }

    pub fn apply_env(&mut self) {
        self.apply_env_with(|key| std::env::var(key).ok());
    }

    pub fn apply_env_with(&mut self, get: impl Fn(&str) -> Option<String>) {
        if let Some(v) = get("KOALA_BASE_URL") {
            self.llm.base_url = v;
        }
        if let Some(v) = get("KOALA_API_KEY") {
            self.llm.api_key = v;
        }
        if let Some(v) = get("KOALA_MODEL") {
            self.llm.model = v;
        }
        if let Some(v) = get("KOALA_THEME").and_then(|v| Theme::parse(&v)) {
            self.theme = v;
        }
        if let Some(v) = get("KOALA_LANG").and_then(|v| Lang::parse(&v)) {
            self.lang = v;
        }
    }
}

/// Replace only the language preference, leaving the user's config untouched.
/// Rename a complete temporary file so an interrupted write cannot truncate it.
pub(crate) fn save_language(path: &Path, lang: Lang) -> std::io::Result<()> {
    save_preference(path, "lang", lang.code())
}

pub(crate) fn save_theme(path: &Path, theme: Theme) -> std::io::Result<()> {
    save_preference(path, "theme", theme.code())
}

fn save_preference(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        fs::write(&temporary, format!("{key} = \"{value}\"\n"))?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_themes_parse_and_persist() {
        for name in [
            "catppuccin",
            "nord",
            "dracula",
            "catppuccin-latte",
            "solarized-light",
            "github-light",
        ] {
            let parsed = Theme::parse(name).expect("built-in theme");
            let cfg: Config = toml::from_str(&format!("theme = \"{name}\"")).unwrap();
            assert_eq!(cfg.theme, parsed);
            assert_eq!(parsed.code(), name);
            let root = std::env::temp_dir().join(format!("koala-palette-{}", uuid::Uuid::new_v4()));
            let path = root.join("theme.toml");
            save_theme(&path, parsed).unwrap();
            let restored: Config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(restored.theme, parsed);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn theme_defaults_to_auto_and_validates_config() {
        assert_eq!(toml::from_str::<Config>("").unwrap().theme, Theme::Auto);
        for theme in [Theme::Auto, Theme::Light, Theme::Dark] {
            let cfg: Config = toml::from_str(&format!("theme = \"{}\"", theme.code())).unwrap();
            assert_eq!(cfg.theme, theme);
        }
        assert!(toml::from_str::<Config>("theme = \"invalid\"").is_err());
    }

    #[test]
    fn model_profiles_override_legacy_metadata_without_leaking_to_other_models() {
        let cfg: Config = toml::from_str(
            r#"
[llm]
model = "a"
reasoning_efforts = ["legacy"]
context_window = 1000
[[llm.models]]
model = "a"
reasoning_efforts = ["high"]
[[llm.models]]
model = "b"
context_window = 2000
"#,
        )
        .unwrap();
        let models = cfg.llm.selectable_models().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].reasoning_efforts, vec!["high"]);
        assert!(models[0].context_window.is_none());
        assert!(models[1].reasoning_efforts.is_empty());
        let mut cfg = cfg;
        cfg.llm.models.push(cfg.llm.models[0].clone());
        assert!(cfg.llm.selectable_models().is_err());
        cfg.llm.models.pop();
        cfg.llm.models[1].reasoning_effort = Some("unsupported".into());
        assert!(cfg.llm.selectable_models().is_err());
    }

    #[test]
    fn model_capacity_must_be_positive_and_metadata_is_optional() {
        let cfg: Config = toml::from_str("[llm]\nmodel = 'test'").unwrap();
        assert!(cfg.llm.context_window.is_none());
        assert!(cfg.llm.reasoning_efforts.is_empty());
        assert!(cfg.llm.reasoning_effort.is_none());
        for value in ["0", "-1"] {
            assert!(toml::from_str::<Config>(&format!("[llm]\ncontext_window = {value}")).is_err());
        }
    }

    #[test]
    fn parses_full_config() {
        let text = r#"
[llm]
base_url = "https://api.openai.com/v1"
api_key = "sk-xxx"
model = "gpt-4o-mini"

[llm.headers]
x-opencode-session = "koala"

[agent]
max_tool_rounds = 2
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.llm.api_key, "sk-xxx");
        assert_eq!(cfg.llm.model, "gpt-4o-mini");
        assert_eq!(
            cfg.llm
                .headers
                .get("x-opencode-session")
                .map(String::as_str),
            Some("koala")
        );
        assert_eq!(cfg.agent.max_tool_rounds, Some(2));
        assert_eq!(cfg.agent.session_dir, PathBuf::from(".koala/session"));
    }

    #[test]
    fn permission_descriptions_follow_the_language() {
        for mode in PermissionMode::ALL {
            assert_ne!(mode.description(Lang::En), mode.description(Lang::Zh));
        }
    }

    #[test]
    fn permission_levels_parse_and_invalid_or_legacy_settings_fail() {
        for (name, mode) in [
            ("normal", PermissionMode::Normal),
            ("ask_when_need", PermissionMode::AskWhenNeed),
            ("auto_edit", PermissionMode::AutoEdit),
            ("never_ask", PermissionMode::NeverAsk),
        ] {
            let cfg: Config =
                toml::from_str(&format!("[permissions]\nmode = \"{name}\"\n")).unwrap();
            assert_eq!(cfg.permissions.mode, mode);
            assert_eq!(PermissionMode::parse(name), Some(mode));
        }
        assert!(toml::from_str::<Config>("[permissions]\nmode = \"typo\"").is_err());
        assert!(toml::from_str::<Config>("[permissions]\ndefault = \"deny\"").is_err());
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.agent.max_tool_rounds, None);
        assert_eq!(cfg.agent.subagent_max_rounds, None);
        assert_eq!(cfg.agent.max_retries, 5);
        assert_eq!(cfg.agent.compact_threshold, 40_000);
        assert_eq!(cfg.permissions.mode, PermissionMode::AskWhenNeed);
        assert!(cfg.permissions.allow.is_empty());
        // English is the default interface language.
        assert_eq!(cfg.lang, Lang::En);
    }

    #[test]
    fn lang_defaults_to_english_and_accepts_codes_or_env_override() {
        assert_eq!(toml::from_str::<Config>("").unwrap().lang, Lang::En);
        assert_eq!(
            toml::from_str::<Config>("lang = \"zh\"").unwrap().lang,
            Lang::Zh
        );
        assert_eq!(
            toml::from_str::<Config>("lang = \"en\"").unwrap().lang,
            Lang::En
        );
        assert!(toml::from_str::<Config>("lang = \"fr\"").is_err());
        let mut cfg: Config = toml::from_str("lang = \"en\"").unwrap();
        cfg.apply_env_with(|key| (key == "KOALA_LANG").then(|| "zh".to_string()));
        assert_eq!(cfg.lang, Lang::Zh);
        // An unparsable value is ignored rather than silently switching language.
        cfg.apply_env_with(|key| (key == "KOALA_LANG").then(|| "klingon".to_string()));
        assert_eq!(cfg.lang, Lang::Zh);
    }

    #[test]
    fn round_limits_accept_unlimited_and_nonnegative_integers() {
        for field in ["max_tool_rounds", "subagent_max_rounds"] {
            for (value, expected) in [("\"unlimited\"", None), ("0", Some(0)), ("50", Some(50))] {
                let cfg: Config = toml::from_str(&format!("[agent]\n{field} = {value}\n")).unwrap();
                let actual = if field == "max_tool_rounds" {
                    cfg.agent.max_tool_rounds
                } else {
                    cfg.agent.subagent_max_rounds
                };
                assert_eq!(actual, expected);
            }
            for value in ["-1", "1.5", "true", "\"typo\"", "\"8\""] {
                assert!(
                    toml::from_str::<Config>(&format!("[agent]\n{field} = {value}\n")).is_err()
                );
            }
        }
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config = toml::from_str("[llm]\nmodel = \"qwen3\"\n").unwrap();
        assert_eq!(cfg.llm.model, "qwen3");
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn env_vars_override_file_values() {
        let mut cfg: Config = toml::from_str(
            "[llm]\nbase_url = \"http://a:1/v1\"\napi_key = \"k1\"\nmodel = \"m1\"\n",
        )
        .unwrap();
        cfg.apply_env_with(|key| match key {
            "KOALA_BASE_URL" => Some("http://b:2/v2".to_string()),
            "KOALA_API_KEY" => Some("k2".to_string()),
            "KOALA_MODEL" => Some("m2".to_string()),
            _ => None,
        });
        assert_eq!(cfg.llm.base_url, "http://b:2/v2");
        assert_eq!(cfg.llm.api_key, "k2");
        assert_eq!(cfg.llm.model, "m2");
    }

    #[test]
    fn missing_env_vars_keep_file_values() {
        let mut cfg: Config = toml::from_str("[llm]\napi_key = \"from-file\"\n").unwrap();
        cfg.apply_env_with(|_| None);
        assert_eq!(cfg.llm.api_key, "from-file");
    }
}
