//! MCP clients expose discovered tools through the existing extension boundary.
use crate::extensions::{Extension, ExtensionFuture, Extensions, Response, Stage};
use crate::llm::Tool;
use rmcp::{
    RoleClient, ServiceExt,
    model::*,
    service::{Peer, PeerRequestOptions, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: BTreeMap<String, ServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub enabled: bool,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub bearer_token_env: Option<String>,
    pub startup_timeout_secs: u64,
    pub timeout_secs: u64,
    /// Explicit local trust decision; server readOnlyHint alone is insufficient.
    pub read_only_tools: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            bearer_token_env: None,
            startup_timeout_secs: 30,
            timeout_secs: 60,
            read_only_tools: vec![],
        }
    }
}

impl ServerConfig {
    fn validate(&self, name: &str) -> Result<(), String> {
        if !valid_name(name) {
            return Err("server name must contain only ASCII letters, digits, _ or -".into());
        }
        if self.startup_timeout_secs == 0 || self.timeout_secs == 0 {
            return Err("timeouts must be positive".into());
        }
        match (&self.command, &self.url) {
            (Some(command), None) if !command.trim().is_empty() => {
                if !self.headers.is_empty() || self.bearer_token_env.is_some() {
                    return Err("HTTP headers/token cannot be used with command".into());
                }
            }
            (None, Some(url)) => {
                let url = reqwest::Url::parse(url).map_err(|_| "invalid MCP URL")?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    return Err("MCP URL must use http or https".into());
                }
                if !self.args.is_empty() || !self.env.is_empty() || self.cwd.is_some() {
                    return Err("args/env/cwd require command".into());
                }
            }
            _ => return Err("configure exactly one of command or url".into()),
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

struct RemoteTool {
    definition: Tool,
    remote_name: String,
    read_only: bool,
}
struct McpExtension {
    name: String,
    service: RunningService<RoleClient, ()>,
    tools: Vec<RemoteTool>,
    timeout: Duration,
}

pub async fn load(config: &McpConfig, extensions: &mut Extensions) -> Result<(), String> {
    // Validate all entries before launching any programs.
    for (name, cfg) in &config.servers {
        if cfg.enabled {
            cfg.validate(name).map_err(|e| format!("MCP {name}: {e}"))?;
        }
    }
    for (name, cfg) in &config.servers {
        if !cfg.enabled {
            continue;
        }
        let extension = tokio::time::timeout(
            Duration::from_secs(cfg.startup_timeout_secs),
            McpExtension::connect(name, cfg),
        )
        .await
        .map_err(|_| format!("MCP {name}: startup timed out"))?
        .map_err(|e| format!("MCP {name}: {e}"))?;
        extensions.register(Arc::new(extension))?;
    }
    Ok(())
}

impl McpExtension {
    async fn connect(name: &str, cfg: &ServerConfig) -> Result<Self, String> {
        let service = if let Some(command) = &cfg.command {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(&cfg.args).envs(&cfg.env).kill_on_drop(true);
            if let Some(cwd) = &cfg.cwd {
                cmd.current_dir(cwd);
            }
            // Server logs must never write over the terminal UI.
            let (transport, _) = TokioChildProcess::builder(cmd)
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            ().serve(transport).await.map_err(|e| e.to_string())?
        } else {
            let mut headers = std::collections::HashMap::new();
            for (key, value) in &cfg.headers {
                let key = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|_| "invalid HTTP header name")?;
                let value = reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| "invalid HTTP header value")?;
                headers.insert(key, value);
            }
            let mut transport_config =
                StreamableHttpClientTransportConfig::with_uri(cfg.url.as_ref().unwrap().clone())
                    .custom_headers(headers);
            if let Some(key) = &cfg.bearer_token_env {
                let token = std::env::var(key)
                    .map_err(|_| format!("missing bearer token environment variable: {key}"))?;
                if token.trim().is_empty() {
                    return Err(format!("empty bearer token environment variable: {key}"));
                }
                transport_config = transport_config.auth_header(token);
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())?;
            ().serve(StreamableHttpClientTransport::with_client(
                client,
                transport_config,
            ))
            .await
            .map_err(|e| e.to_string())?
        };
        let discovered = service.list_all_tools().await.map_err(|e| e.to_string())?;
        let mut tools = Vec::new();
        let mut names = HashSet::new();
        for tool in discovered {
            let local_name = format!("mcp__{name}__{}", tool.name);
            if !valid_name(&local_name) || local_name.len() > 64 {
                return Err(format!(
                    "tool name cannot be exposed to the model (ASCII letters/digits/_/-, max 64 bytes including prefix): {local_name}"
                ));
            }
            if !names.insert(tool.name.to_string()) {
                return Err(format!("duplicate tool: {}", tool.name));
            }
            tools.push(RemoteTool {
                definition: Tool::function(
                    &local_name,
                    tool.description.as_deref().unwrap_or("MCP tool"),
                    Value::Object((*tool.input_schema).clone()),
                ),
                read_only: cfg.read_only_tools.iter().any(|n| n == tool.name.as_ref()),
                remote_name: tool.name.into_owned(),
            });
        }
        for tool in &cfg.read_only_tools {
            if !names.contains(tool) {
                return Err(format!("read_only_tools contains unknown tool: {tool}"));
            }
        }
        Ok(Self {
            name: format!("mcp:{name}"),
            service,
            tools,
            timeout: Duration::from_secs(cfg.timeout_secs),
        })
    }
}

// Dropping a foreground turn sends cancellation without tearing down a shared server.
struct CancelOnDrop(Option<(Peer<RoleClient>, RequestId)>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some((peer, id)) = self.0.take() {
            tokio::spawn(async move {
                let notification = CancelledNotificationParam::new(
                    Some(id),
                    Some("koala tool call interrupted".into()),
                );
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    peer.notify_cancelled(notification),
                )
                .await;
            });
        }
    }
}

impl Extension for McpExtension {
    fn name(&self) -> &str {
        &self.name
    }
    fn tools(&self) -> Vec<Tool> {
        self.tools.iter().map(|t| t.definition.clone()).collect()
    }
    fn read_only(&self, name: &str) -> bool {
        self.tools
            .iter()
            .any(|t| t.definition.function.name == name && t.read_only)
    }
    fn hook<'a>(&'a self, _: Stage, _: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async { Ok(Response::default()) })
    }
    fn execute<'a>(&'a self, name: &'a str, args: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async move {
            let tool = self
                .tools
                .iter()
                .find(|t| t.definition.function.name == name)
                .ok_or("unknown MCP tool")?;
            let args = args
                .as_object()
                .ok_or("MCP arguments must be an object")?
                .clone();
            let params = CallToolRequestParams::new(tool.remote_name.clone()).with_arguments(args);
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let handle = self
                .service
                .send_cancellable_request(request, PeerRequestOptions::with_timeout(self.timeout))
                .await
                .map_err(|e| e.to_string())?;
            let mut guard = CancelOnDrop(Some((self.service.peer().clone(), handle.id.clone())));
            let result = handle.await_response().await;
            guard.0 = None;
            let ServerResult::CallToolResult(result) = result.map_err(|e| e.to_string())? else {
                return Err("unsupported MCP tool response".into());
            };
            // Preserve every content block and structuredContent, including isError.
            Ok(Response {
                content: Some(serde_json::to_string(&result).map_err(|e| e.to_string())?),
                is_error: result.is_error.unwrap_or(false),
                ..Response::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{
            permissions::{Permissions, Policy},
            tools::catalog::ToolCatalog,
        },
        config::{Config, PermissionMode, PermissionsConfig},
    };
    use serde_json::json;

    fn fixture() -> ServerConfig {
        ServerConfig {
            command: Some("python3".into()),
            args: vec![format!(
                "{}/tests/fixtures/mcp_server.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            env: BTreeMap::from([("MCP_TEST_VALUE".into(), "configured".into())]),
            cwd: Some(std::env::temp_dir()),
            timeout_secs: 1,
            ..ServerConfig::default()
        }
    }

    #[test]
    fn config_defaults_and_invalid_combinations() {
        let cfg: Config =
            toml::from_str("[mcp.servers.local]\ncommand = 'python3'\nargs = ['server.py']")
                .unwrap();
        let server = &cfg.mcp.servers["local"];
        assert!(server.enabled);
        assert_eq!(server.timeout_secs, 60);
        assert!(server.validate("local").is_ok());
        assert!(toml::from_str::<Config>("[mcp.servers.x]\ncommmand = 'typo'").is_err());
        for server in [
            ServerConfig::default(),
            ServerConfig {
                url: Some("https://example.com/mcp".into()),
                ..fixture()
            },
            ServerConfig {
                timeout_secs: 0,
                ..fixture()
            },
            ServerConfig {
                url: Some("file:///tmp/x".into()),
                ..ServerConfig::default()
            },
        ] {
            assert!(server.validate("test").is_err());
        }
        assert!(fixture().validate("bad/name").is_err());
    }

    #[tokio::test]
    async fn stdio_pagination_shared_session_results_and_permissions() {
        let cfg = ServerConfig {
            read_only_tools: vec!["echo".into()],
            ..fixture()
        };
        let extension = Arc::new(McpExtension::connect("local", &cfg).await.unwrap());
        assert_eq!(extension.tools().len(), 2);
        let mut extensions = Extensions::default();
        extensions.register(extension.clone()).unwrap();
        let permissions = Permissions::new(&PermissionsConfig {
            mode: PermissionMode::AskWhenNeed,
            ..Default::default()
        });
        for depth in [0, 1] {
            let catalog = ToolCatalog::build(depth, &extensions);
            assert!(catalog.plan_allowed("mcp__local__echo"));
            assert!(!catalog.plan_allowed("mcp__local__write"));
            assert_eq!(
                catalog.policy(&permissions, "mcp__local__echo", &json!({})),
                Policy::Allow
            );
            assert_eq!(
                catalog.policy(&permissions, "mcp__local__write", &json!({})),
                Policy::Ask
            );
            assert_eq!(
                catalog.policy(
                    &Permissions::new(&PermissionsConfig::default()),
                    "mcp__local__echo",
                    &json!({})
                ),
                Policy::Ask
            );
        }
        for (index, fail) in [false, true].into_iter().enumerate() {
            let response = extension
                .execute("mcp__local__echo", &json!({"text":"你好", "fail":fail}))
                .await
                .unwrap();
            assert_eq!(response.is_error, fail);
            let value: Value = serde_json::from_str(&response.content.unwrap()).unwrap();
            assert_eq!(value["content"][0]["text"], "你好");
            assert_eq!(value["structuredContent"]["name"], "echo");
            assert_eq!(value["structuredContent"]["calls"], index + 1);
            assert_eq!(value["structuredContent"]["env"], "configured");
            assert_eq!(
                PathBuf::from(value["structuredContent"]["cwd"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                std::env::temp_dir().canonicalize().unwrap()
            );
        }
        assert!(
            extension
                .execute("mcp__local__echo", &json!({"protocol_error":true}))
                .await
                .unwrap_err()
                .contains("bad arguments")
        );
        assert!(
            extension
                .execute("mcp__local__echo", &json!([]))
                .await
                .is_err()
        );
        assert!(extension.execute("missing", &json!({})).await.is_err());
    }

    #[tokio::test]
    async fn timeout_and_interrupt_cancel_requests_without_restarting_server() {
        let extension = McpExtension::connect("local", &fixture()).await.unwrap();
        assert!(
            extension
                .execute("mcp__local__echo", &json!({"hang":true}))
                .await
                .unwrap_err()
                .contains("timeout")
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                extension.execute("mcp__local__echo", &json!({"hang":true}))
            )
            .await
            .is_err()
        );
        // Give the cancellation notification a chance to enter the transport queue.
        let mut observed = false;
        for _ in 0..20 {
            let response = extension
                .execute("mcp__local__echo", &json!({}))
                .await
                .unwrap();
            let value: Value = serde_json::from_str(&response.content.unwrap()).unwrap();
            if value["structuredContent"]["cancelled"]
                .as_array()
                .unwrap()
                .len()
                == 2
            {
                observed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(observed);
        assert!(
            extension
                .execute("mcp__local__echo", &json!({"crash":true}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn load_skips_disabled_servers_and_reports_startup_errors() {
        let mut extensions = Extensions::default();
        let disabled = ServerConfig {
            enabled: false,
            ..Default::default()
        };
        let mut config = McpConfig {
            servers: BTreeMap::from([("disabled".into(), disabled)]),
        };
        load(&config, &mut extensions).await.unwrap();
        assert!(extensions.tools().is_empty());
        let mut hanging = fixture();
        hanging.startup_timeout_secs = 1;
        hanging.env.insert("MCP_TEST_HANG".into(), "1".into());
        config.servers.insert("hanging".into(), hanging);
        assert!(
            load(&config, &mut extensions)
                .await
                .unwrap_err()
                .contains("MCP hanging: startup timed out")
        );
        let cfg = ServerConfig {
            read_only_tools: vec!["typo".into()],
            ..fixture()
        };
        assert!(
            McpExtension::connect("local", &cfg)
                .await
                .err()
                .unwrap()
                .contains("unknown tool")
        );
    }

    #[tokio::test]
    async fn streamable_http_discovers_and_calls_tools_with_headers() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let cfg = fixture();
        let mut process = tokio::process::Command::new("python3")
            .args(&cfg.args)
            .arg("--http")
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(process.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let cfg = ServerConfig {
            url: Some(format!("http://127.0.0.1:{}/mcp", line.trim())),
            headers: BTreeMap::from([("X-MCP-Test".into(), "header-value".into())]),
            ..Default::default()
        };
        let extension = tokio::time::timeout(
            Duration::from_secs(5),
            McpExtension::connect("remote", &cfg),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(extension.tools().len(), 2);
        let response = extension
            .execute("mcp__remote__echo", &json!({"text":"over HTTP"}))
            .await
            .unwrap();
        assert!(response.content.unwrap().contains("over HTTP"));
        drop(extension);
        process.kill().await.unwrap();
        process.wait().await.unwrap();
    }
}
