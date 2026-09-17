use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {0}: {1}")]
    Read(String, #[source] std::io::Error),
    #[error("failed to parse config file {0}: {1}")]
    Parse(String, #[source] toml::de::Error),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub llm: LlmConfig,
    pub memory: MemoryConfig,
    pub agent: AgentConfig,
    pub permissions: PermissionsConfig,
    pub hooks: HooksConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    /// Fallback when a tool matches neither allow nor deny: "allow" | "ask" | "deny".
    pub default: String,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default: "ask".into(),
            allow: vec!["todo_write".into(), "remember".into(), "skill".into()],
            deny: Vec::new(),
        }
    }
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
            headers: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub workspace: PathBuf,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from(".kb"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_tool_rounds: usize,
    pub max_retries: usize,
    /// Compact history when its estimated size (chars) exceeds this.
    pub compact_threshold: usize,
    pub subagent_max_rounds: usize,
    /// Directory where chat transcripts (session jsonl) are appended.
    /// This directory is the only interface between the agent and the knowledge base.
    pub session_dir: PathBuf,
    /// Agent-private memory file, injected into the system prompt every turn.
    pub memory_file: PathBuf,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_rounds: 8,
            max_retries: 3,
            compact_threshold: 40_000,
            subagent_max_rounds: 8,
            session_dir: PathBuf::from(".kb/session"),
            memory_file: default_memory_file(),
        }
    }
}

fn default_memory_file() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("kb-agent").join("memory.md"))
        .unwrap_or_else(|| PathBuf::from("memory.md"))
}

impl Config {
    /// First existing file wins: ./config.toml, then ~/.config/kb-agent/config.toml.
    /// Missing files fall back to defaults; KBA_* env vars override everything.
    pub fn load() -> Result<Self, ConfigError> {
        let mut candidates = vec![PathBuf::from("config.toml")];
        if let Some(dir) = dirs::config_dir() {
            candidates.push(dir.join("kb-agent").join("config.toml"));
        }
        let mut cfg = Config::default();
        for path in candidates {
            if path.is_file() {
                let text = fs::read_to_string(&path)
                    .map_err(|e| ConfigError::Read(path.display().to_string(), e))?;
                cfg = toml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(path.display().to_string(), e))?;
                break;
            }
        }
        cfg.apply_env();
        Ok(cfg)
    }

    pub fn apply_env(&mut self) {
        self.apply_env_with(|key| std::env::var(key).ok());
    }

    pub fn apply_env_with(&mut self, get: impl Fn(&str) -> Option<String>) {
        if let Some(v) = get("KBA_BASE_URL") {
            self.llm.base_url = v;
        }
        if let Some(v) = get("KBA_API_KEY") {
            self.llm.api_key = v;
        }
        if let Some(v) = get("KBA_MODEL") {
            self.llm.model = v;
        }
        if let Some(v) = get("KBA_WORKSPACE") {
            self.memory.workspace = PathBuf::from(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let text = r#"
[llm]
base_url = "https://api.openai.com/v1"
api_key = "sk-xxx"
model = "gpt-4o-mini"

[llm.headers]
x-opencode-session = "kb-agent"

[memory]
workspace = ".kb"

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
            Some("kb-agent")
        );
        assert_eq!(cfg.memory.workspace, PathBuf::from(".kb"));
        assert_eq!(cfg.agent.max_tool_rounds, 2);
        assert_eq!(cfg.agent.session_dir, PathBuf::from(".kb/session"));
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.memory.workspace, PathBuf::from(".kb"));
        assert_eq!(cfg.agent.max_tool_rounds, 8);
        assert_eq!(cfg.agent.max_retries, 3);
        assert_eq!(cfg.agent.compact_threshold, 40_000);
        assert_eq!(cfg.permissions.default, "ask");
        assert!(cfg.permissions.allow.contains(&"todo_write".to_string()));
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config = toml::from_str("[llm]\nmodel = \"qwen3\"\n").unwrap();
        assert_eq!(cfg.llm.model, "qwen3");
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.memory.workspace, PathBuf::from(".kb"));
    }

    #[test]
    fn env_vars_override_file_values() {
        let mut cfg: Config = toml::from_str(
            "[llm]\nbase_url = \"http://a:1/v1\"\napi_key = \"k1\"\nmodel = \"m1\"\n",
        )
        .unwrap();
        cfg.apply_env_with(|key| match key {
            "KBA_BASE_URL" => Some("http://b:2/v2".to_string()),
            "KBA_API_KEY" => Some("k2".to_string()),
            "KBA_MODEL" => Some("m2".to_string()),
            "KBA_WORKSPACE" => Some("/tmp/ws".to_string()),
            _ => None,
        });
        assert_eq!(cfg.llm.base_url, "http://b:2/v2");
        assert_eq!(cfg.llm.api_key, "k2");
        assert_eq!(cfg.llm.model, "m2");
        assert_eq!(cfg.memory.workspace, PathBuf::from("/tmp/ws"));
    }

    #[test]
    fn missing_env_vars_keep_file_values() {
        let mut cfg: Config = toml::from_str("[llm]\napi_key = \"from-file\"\n").unwrap();
        cfg.apply_env_with(|_| None);
        assert_eq!(cfg.llm.api_key, "from-file");
    }
}
