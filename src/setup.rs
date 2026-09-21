//! First-run configuration persistence.
use crate::config::Config;
use anyhow::Context;
use std::{fs, io::Write, path::Path};

const INITIAL_CONFIG: &str = "# Koala configuration. Complete setup in koala on first launch.\n[llm]\nbase_url = \"https://api.openai.com/v1\"\napi_key = \"\"\nmodel = \"\"\n";

/// Publish a complete file without replacing a concurrently created config.
pub fn ensure_config(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path.parent().context("config has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        write_private(&temporary, INITIAL_CONFIG)?;
        match fs::hard_link(&temporary, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e.into()),
        }
    })();
    let _ = fs::remove_file(&temporary);
    result.with_context(|| format!("could not create {}", path.display()))
}

pub fn valid_base_url(value: &str) -> bool {
    reqwest::Url::parse(value)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

pub fn valid_model(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn needs_setup(cfg: &Config) -> bool {
    !valid_base_url(&cfg.llm.base_url)
        || !valid_model(&cfg.llm.model)
        || cfg.llm.api_key == "sk-xxx"
}

/// None leaves an environment-controlled field unchanged on disk.
pub fn save_connection(path: &Path, values: [Option<&str>; 3]) -> anyhow::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut document: toml::Table = toml::from_str(&text)?;
    let llm = document
        .entry("llm")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .context("llm must be a table")?;
    for (key, value) in ["base_url", "api_key", "model"].into_iter().zip(values) {
        if let Some(value) = value {
            llm.insert(key.into(), toml::Value::String(value.into()));
        }
    }
    let text = toml::to_string_pretty(&document)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        write_private(&temporary, &text)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn path() -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("koala-setup-{}", uuid::Uuid::new_v4()))
            .join("config.toml")
    }
    #[test]
    fn creates_parseable_incomplete_config_without_overwriting_existing_file() {
        let path = path();
        ensure_config(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let cfg: Config = toml::from_str(&text).unwrap();
        assert!(needs_setup(&cfg));
        fs::write(&path, "# custom\n[llm]\nmodel = 'existing'\n").unwrap();
        ensure_config(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# custom\n[llm]\nmodel = 'existing'\n"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn saves_connection_preserving_other_settings_and_env_owned_fields() {
        let path = path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "lang = 'zh'\n[llm]\napi_key = 'file-secret'\n[llm.headers]\nx-custom = 'keep'\n[agent]\nmax_retries = 7\n").unwrap();
        save_connection(
            &path,
            [
                Some("http://localhost:1234/v1"),
                None,
                Some("model-\"quoted\""),
            ],
        )
        .unwrap();
        let cfg: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.llm.model, "model-\"quoted\"");
        assert_eq!(cfg.llm.api_key, "file-secret");
        assert_eq!(cfg.llm.headers["x-custom"], "keep");
        assert_eq!(cfg.agent.max_retries, 7);
        assert!(!needs_setup(&cfg));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn detects_missing_and_placeholder_fields_but_accepts_keyless_endpoints() {
        for (url, key, model, expected) in [
            ("http://localhost:1234/v1", "", "local-model", false),
            ("https://api.example.com/v1", "secret", "model", false),
            ("", "secret", "model", true),
            ("not a url", "secret", "model", true),
            ("https://api.example.com/v1", "secret", "my model", true),
            ("https://api.example.com/v1", "sk-xxx", "model", true),
            ("https://api.example.com/v1", "secret", "  ", true),
        ] {
            let mut cfg = Config::default();
            cfg.llm.base_url = url.into();
            cfg.llm.api_key = key.into();
            cfg.llm.model = model.into();
            assert_eq!(needs_setup(&cfg), expected, "{url} / {model}");
        }
    }
}
