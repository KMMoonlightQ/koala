//! Generic extension host. No Agent or knowledge-base dependencies.
pub use koala_extension_api::*;
pub mod ui;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use tokio::io::AsyncWriteExt;

#[derive(Default)]
pub struct Extensions {
    entries: Vec<Arc<dyn Extension>>,
    reserved: HashSet<String>,
}
impl Extensions {
    pub fn new(reserved: impl IntoIterator<Item = String>) -> Self {
        Self {
            entries: Vec::new(),
            reserved: reserved.into_iter().collect(),
        }
    }
    pub fn register(&mut self, extension: Arc<dyn Extension>) -> Result<(), String> {
        if self.entries.iter().any(|e| e.name() == extension.name()) {
            return Err(format!("duplicate extension: {}", extension.name()));
        }
        let mut names: HashSet<String> = self
            .reserved
            .iter()
            .cloned()
            .chain(self.tools().into_iter().map(|t| t.function.name))
            .collect();
        for tool in extension.tools() {
            if tool.function.name.is_empty() || !names.insert(tool.function.name.clone()) {
                return Err(format!("duplicate or empty tool: {}", tool.function.name));
            }
        }
        self.entries.push(Arc::new(ui::Hosted(extension)));
        Ok(())
    }
    pub fn ui_extensions(&self) -> Vec<Arc<dyn Extension>> {
        self.entries
            .iter()
            .filter(|e| e.ui_enabled())
            .cloned()
            .collect()
    }

    pub fn tool_entries(&self) -> Vec<(Tool, Arc<dyn Extension>)> {
        self.entries
            .iter()
            .flat_map(|extension| {
                extension
                    .tools()
                    .into_iter()
                    .map(|tool| (tool, Arc::clone(extension)))
            })
            .collect()
    }

    pub fn tools(&self) -> Vec<Tool> {
        self.entries.iter().flat_map(|e| e.tools()).collect()
    }
    /// Ordered middleware: later extensions see the current arguments/result.
    pub async fn hook(&self, stage: Stage, mut payload: Value) -> Result<Response, String> {
        let mut combined = Response::default();
        for e in &self.entries {
            let r = e
                .hook(stage, &payload)
                .await
                .map_err(|err| format!("{}: {err}", e.name()))?;
            if let Some(context) = r.context {
                combined
                    .context
                    .get_or_insert_default()
                    .push_str(&format!("\n[extension {}]\n{context}\n", e.name()));
            }
            if let Some(args) = r.arguments {
                payload["arguments"] = args.clone();
                combined.arguments = Some(args);
            }
            if let Some(content) = r.content {
                payload["content"] = json!(content);
                payload["is_error"] = json!(r.is_error);
                combined.content = Some(content);
                combined.is_error = r.is_error;
            }
        }
        Ok(combined)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    #[serde(default)]
    pub ui: bool,
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<Stage>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default)]
    pub read_only: bool,
}
struct ProcessExtension {
    manifest: Manifest,
    directory: PathBuf,
    timeout_secs: u64,
}
impl ProcessExtension {
    async fn request(&self, mut request: Value) -> Result<Response, String> {
        request["api_version"] = json!(self.manifest.api_version);
        if self.manifest.api_version == 2 {
            let ctx = ui::current_context();
            request["session_id"] = json!(ctx.session_id);
            request["capabilities"] = json!({"ui": ctx.ui && self.manifest.ui});
        }
        let mut command = tokio::process::Command::new(&self.manifest.command[0]);
        command
            .args(&self.manifest.command[1..])
            .current_dir(&self.directory)
            .env(
                "KOALA_EXTENSION_CWD",
                std::env::current_dir().map_err(|e| e.to_string())?,
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (mut child, _group) = koala_process::spawn(&mut command).map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().ok_or("missing extension stdin")?;
        let data = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            async move {
                let writer = async move {
                    stdin.write_all(&data).await?;
                    stdin.shutdown().await
                };
                let (written, output) = tokio::join!(writer, child.wait_with_output());
                let output = output.map_err(|e| e.to_string())?;
                if !output.status.success() {
                    return Err(format!(
                        "{}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                written.map_err(|e| e.to_string())?;
                serde_json::from_slice(&output.stdout)
                    .map_err(|e| format!("invalid extension response: {e}"))
            },
        )
        .await
        .map_err(|_| "extension timed out".to_string())?
    }
}
impl Extension for ProcessExtension {
    fn name(&self) -> &str {
        &self.manifest.name
    }
    fn ui_enabled(&self) -> bool {
        self.manifest.api_version == 2 && self.manifest.ui
    }
    fn ui_event<'a>(&'a self, event: &'a UiInputEvent) -> ExtensionFuture<'a> {
        Box::pin(async move {
            self.request(json!({"kind": "ui_event", "event": event}))
                .await
        })
    }
    fn tools(&self) -> Vec<Tool> {
        self.manifest
            .tools
            .iter()
            .map(|t| Tool::function(&t.name, &t.description, t.parameters.clone()))
            .collect()
    }
    fn read_only(&self, name: &str) -> bool {
        self.manifest
            .tools
            .iter()
            .any(|t| t.name == name && t.read_only)
    }
    fn hook<'a>(&'a self, stage: Stage, payload: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async move {
            if !self.manifest.hooks.contains(&stage) {
                return Ok(Response::default());
            }
            self.request(
                json!({"api_version": 1, "kind": "hook", "stage": stage, "payload": payload}),
            )
            .await
        })
    }
    fn execute<'a>(&'a self, name: &'a str, args: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async move {
            self.request(json!({"api_version": 1, "kind": "tool", "name": name, "arguments": args}))
                .await
        })
    }
}

pub fn load(
    cfg: &ExtensionsConfig,
    reserved: impl IntoIterator<Item = String>,
) -> Result<Extensions, String> {
    let mut extensions = Extensions::new(reserved);
    for path in &cfg.manifests {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let manifest: Manifest =
            toml::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if !matches!(manifest.api_version, 1 | 2)
            || (manifest.ui && manifest.api_version != 2)
            || manifest.name.is_empty()
            || manifest.command.is_empty()
            || manifest.command[0].is_empty()
            || cfg.timeout_secs == 0
        {
            return Err(format!("invalid extension manifest: {}", path.display()));
        }
        extensions.register(Arc::new(ProcessExtension {
            manifest,
            directory: path.parent().unwrap().to_path_buf(),
            timeout_secs: cfg.timeout_secs,
        }))?;
    }
    Ok(extensions)
}

/// Install a self-contained extension directory. Never overwrite an installation
/// or follow source symlinks. Loading remains explicit via the printed config.
pub fn install(source: &std::path::Path, destination: &std::path::Path) -> Result<PathBuf, String> {
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(source.join("extension.toml")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if manifest.name.is_empty()
        || !manifest
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("extension name must contain only letters, digits, - or _".into());
    }
    let cfg = ExtensionsConfig {
        manifests: vec![source.join("extension.toml")],
        ..Default::default()
    };
    load(&cfg, Vec::new())?;
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let destination = destination.canonicalize().map_err(|e| e.to_string())?;
    if destination.starts_with(&source) {
        return Err("installation directory cannot be inside the source".into());
    }
    let target = destination.join(manifest.name);
    std::fs::create_dir(&target).map_err(|e| e.to_string())?;
    fn copy(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let dest = target.join(entry.file_name());
            if kind.is_dir() {
                std::fs::create_dir(&dest)?;
                copy(&entry.path(), &dest)?;
            } else if kind.is_file() {
                std::fs::copy(entry.path(), dest)?;
            } else {
                return Err(std::io::Error::other(
                    "extension source contains a symlink or special file",
                ));
            }
        }
        Ok(())
    }
    if let Err(e) = copy(&source, &target) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(e.to_string());
    }
    Ok(target.join("extension.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> PathBuf {
        let path = std::env::temp_dir().join(format!("koala-extension-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[tokio::test]
    async fn process_protocol_and_ordered_argument_rewrite() {
        let first = ProcessExtension {
            manifest: Manifest {
                ui: false,
                api_version: 1,
                name: "first".into(),
                command: vec![
                    "bash".into(),
                    "-c".into(),
                    "cat >/dev/null; printf '%s' '{\"arguments\":{\"command\":\"changed\"}}'"
                        .into(),
                ],
                hooks: vec![Stage::PreToolUse],
                tools: vec![],
            },
            directory: std::env::temp_dir(),
            timeout_secs: 2,
        };
        let second = ProcessExtension {
            manifest: Manifest {
                ui: false,
                api_version: 1,
                name: "second".into(),
                command: vec![
                    "bash".into(),
                    "-c".into(),
                    "grep -q changed || exit 2; printf '%s' '{\"block\":\"veto\"}'".into(),
                ],
                hooks: vec![Stage::PreToolUse],
                tools: vec![],
            },
            directory: std::env::temp_dir(),
            timeout_secs: 2,
        };
        let mut extensions = Extensions::default();
        extensions.register(Arc::new(first)).unwrap();
        let response = extensions
            .hook(
                Stage::PreToolUse,
                json!({"arguments": {"command": "original"}}),
            )
            .await
            .unwrap();
        assert_eq!(response.arguments.unwrap()["command"], "changed");
        extensions.register(Arc::new(second)).unwrap();
        assert!(
            extensions
                .hook(
                    Stage::PreToolUse,
                    json!({"arguments": {"command": "original"}})
                )
                .await
                .unwrap_err()
                .contains("second blocked: veto")
        );
        assert!(extensions.hook(Stage::TurnEnd, json!({})).await.is_ok());
    }
    #[tokio::test]
    async fn process_failures_and_timeout_are_errors() {
        for command in [
            "cat >/dev/null; echo invalid",
            "cat >/dev/null; exit 3",
            "sleep 5",
        ] {
            let extension = ProcessExtension {
                manifest: Manifest {
                    ui: false,
                    api_version: 1,
                    name: "test".into(),
                    command: vec!["bash".into(), "-c".into(), command.into()],
                    hooks: vec![Stage::TurnStart],
                    tools: vec![],
                },
                directory: std::env::temp_dir(),
                timeout_secs: 1,
            };
            assert!(extension.hook(Stage::TurnStart, &json!({})).await.is_err());
        }
    }
    #[test]
    fn installation_is_loadable_and_does_not_overwrite() {
        let dir = temporary();
        let source = dir.join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("extension.toml"), "api_version = 1\nname = 'test'\ncommand = ['bash', 'hook.sh']\nhooks = ['turn_start']\n").unwrap();
        std::fs::write(source.join("hook.sh"), "cat >/dev/null; echo '{}'").unwrap();
        let installed = install(&source, &dir.join("installed")).unwrap();
        assert!(installed.parent().unwrap().join("hook.sh").exists());
        assert!(install(&source, &dir.join("installed")).is_err());
        let cfg = ExtensionsConfig {
            manifests: vec![installed],
            ..Default::default()
        };
        assert!(load(&cfg, Vec::new()).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn duplicate_names_and_core_tools_are_rejected() {
        let mut extensions = Extensions::new(["bash".to_string()]);
        let conflict = ProcessExtension {
            manifest: Manifest {
                ui: false,
                api_version: 1,
                name: "conflict".into(),
                command: vec!["true".into()],
                hooks: vec![],
                tools: vec![ToolSpec {
                    name: "bash".into(),
                    description: "x".into(),
                    parameters: json!({}),
                    read_only: true,
                }],
            },
            directory: std::env::temp_dir(),
            timeout_secs: 1,
        };
        assert!(extensions.register(Arc::new(conflict)).is_err());
    }
}

/// Extensions are trusted local code. Explicit manifest paths determine order.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExtensionsConfig {
    pub manifests: Vec<PathBuf>,
    pub timeout_secs: u64,
}
impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            manifests: Vec::new(),
            timeout_secs: 30,
        }
    }
}
