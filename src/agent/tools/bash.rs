use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[derive(serde::Deserialize)]
struct Args {
    command: String,
    timeout: Option<u64>,
    #[serde(default)]
    background: bool,
}

pub struct Bash;

impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command and return stdout+stderr (truncated). \
         Set background=true for long-running commands; completion is reported back."
    }

    fn prompt_snippet(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolBashSnippet)
    }

    fn prompt_guidelines(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolBashRules)
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "shell command"},
                "timeout": {"type": "integer", "description": "seconds, default 30"},
                "background": {"type": "boolean", "description": "run in background, default false"}
            },
            "required": ["command"]
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let Args {
                command,
                timeout,
                background,
            } = match serde_json::from_value::<Args>(args) {
                Ok(args) => args,
                Err(e) => return ToolResult::err(format!("invalid arguments: {e}")),
            };
            if command.is_empty() {
                return ToolResult::err("command must not be empty");
            }
            if background {
                let id = ctx.background.register("bash", &command);
                let bg = ctx.background.clone();
                let mut span = match ctx
                    .graph
                    .as_ref()
                    .map(|r| {
                        r.start(
                            super::super::graph::Kind::Background,
                            format!("bash #{id}"),
                            serde_json::json!({"task_id": id, "command": command}),
                            vec![],
                        )
                    })
                    .transpose()
                {
                    Ok(span) => span,
                    Err(e) => {
                        ctx.background.finish(id, false, e.to_string());
                        return ToolResult::err(e.to_string());
                    }
                };
                let handle = tokio::spawn(async move {
                    let outcome = run_command(&command, Duration::from_secs(3600)).await;
                    let (success, output) = match outcome {
                        Ok(o) => (o.success, o.text),
                        Err(e) => (false, e),
                    };
                    if let Some(span) = &mut span
                        && let Err(e) = span.finish(
                            if success {
                                super::super::graph::Status::Succeeded
                            } else {
                                super::super::graph::Status::Failed
                            },
                            serde_json::json!({"output": output}),
                        )
                    {
                        bg.finish(id, false, format!("{output}\ngraph write failed: {e}"));
                        return;
                    }
                    bg.finish(id, success, output);
                });
                ctx.background.attach(id, handle);
                return ToolResult::ok(format!("background task #{id} started"));
            }
            let timeout = Duration::from_secs(timeout.unwrap_or(30));
            match run_command(&command, timeout).await {
                Ok(out) => ToolResult::shell_output(out.text, out.success),
                Err(e) => ToolResult::err(e),
            }
        })
    }
}

struct CommandOutput {
    success: bool,
    text: String,
}

async fn run_command(command: &str, timeout: Duration) -> Result<CommandOutput, String> {
    let (child, _group) = super::super::process::spawn(
        tokio::process::Command::new("bash")
            .arg("-lc")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| format!("spawn failed: {e}"))?;
    let future = child.wait_with_output();
    let output = tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| format!("command timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("spawn failed: {e}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stderr.trim().is_empty() {
        text.push_str(&stderr);
    }
    Ok(CommandOutput {
        success: output.status.success(),
        text,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abort_stops_shell_descendants() {
        let root = std::env::temp_dir().join(format!("koala-shell-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let ready = root.join("ready");
        let leaked = root.join("leaked");
        let cmd = format!(
            "(sleep 0.4; touch '{}') & touch '{}'; wait",
            leaked.display(),
            ready.display()
        );
        let task = tokio::spawn(async move { run_command(&cmd, Duration::from_secs(5)).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(matches!(task.await, Err(e) if e.is_cancelled()));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!leaked.exists(), "a descendant survived cancellation");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn timeout_stops_shell_descendants() {
        let path =
            std::env::temp_dir().join(format!("koala-timeout-test-{}", uuid::Uuid::new_v4()));
        let cmd = format!("(sleep 0.4; touch '{}') & wait", path.display());
        let error = run_command(&cmd, Duration::from_millis(100))
            .await
            .err()
            .unwrap();
        assert!(error.contains("timed out"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!path.exists());
    }
}
